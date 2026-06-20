//! The axum snapshot HTTP transport (protocol.html §2, §4) — issue #42.
//!
//! This is the first real transport over the wire types: a [`Router`] of `GET` snapshot
//! endpoints, one addressing scheme per [`Scope`], each returning a [`Snapshot`] of the
//! scoped projection plus the [`Cursor`] the change stream resumes from. "Snapshot first,
//! then subscribe" (§2): the client renders the body immediately and opens the WS stream
//! (#43) *from* the returned cursor.
//!
//! # [`AppState`] — the shared event source
//!
//! Every path the server serves reads (and the control path #45 writes) the **one**
//! append-only [`EventLog`]. [`AppState`] wraps it in an `Arc<Mutex<…>>` so it can be
//! cloned into every axum handler and, later, shared with the WS stream task and the
//! control handler:
//!
//! - **Reads** (this issue): a handler locks, reads the log into a `Vec<Event>`, folds
//!   the requested projection, and unlocks. The cursor is the log length at read time
//!   (see below).
//! - **Writes** (#45): the control handler will lock and `append` through the same
//!   handle, and the change stream (#43) will observe the new tail. Holding the log
//!   behind a single mutex keeps reads and the eventual writes serialized against one
//!   another with no torn state.
//!
//! The log is stored as `Arc<Mutex<dyn EventLog + Send>>` so any backend
//! ([`InMemoryLog`](gridfpv_storage::InMemoryLog),
//! [`SqliteLog`](gridfpv_storage::SqliteLog)) drops in unchanged.
//!
//! # Cursor semantics
//!
//! The [`Cursor`] a snapshot returns is **the log length at read time** — i.e. the offset
//! the *next* appended event will receive (`EventLog::len`). That is exactly the resume
//! point: a subscription opened `from` this cursor begins at the first event appended
//! after the snapshot, so nothing is missed or double-applied (§2, §3). The public
//! projection-sequence cursor the WS stream advances (#43) is seeded from this value.
//! (Mapping log offsets to the per-projection sequence number §9.5 is a #43 concern; for
//! the snapshot the log length *is* the resume cursor.)
//!
//! # Scope addressing (protocol.html §4, §9.6)
//!
//! §4 fixes the four addressable resources (event / class / heat / pilot); §9.6 defers the
//! exact URL grammar to implementation. This module pins a concrete REST addressing over
//! those four, which the doc-reconciliation pass refines:
//!
//! | scope | route | body |
//! |-------|-------|------|
//! | event | `GET /snapshot/event/{event}` | [`LiveRaceState`] over the whole log |
//! | class | `GET /snapshot/class/{event}/{class}` | [`LiveRaceState`] (class filtering deferred — see below) |
//! | heat  | `GET /snapshot/heat/{heat}` | [`LiveRaceState`] for that heat, or — with `?projection=laps` / `?projection=result` — its [`LapList`] / [`HeatResult`] |
//! | pilot | `GET /snapshot/pilot/{event}/{pilot}` | the pilot's [`LapList`] (their laps across the event) |
//!
//! A single connection may hold several scopes at once (§4); the multi-scope *subscribe*
//! is a stream concern (#43). The heat scope is the tightest, lowest-latency one and the
//! one with a precise log filter; the broader event/class scopes fold the whole log.
//!
//! ## Deferred filtering (model gaps, not protocol gaps)
//!
//! The raw event log (`gridfpv-events`) has **no event-id or class-id** on its events —
//! one Director serves one event, and the class→heat and pilot→competitor mappings are
//! scheduler / registration concerns (#36, Architecture §9) not yet in the log. So:
//!
//! - **Event scope** serves the whole log's live state (correct: the log *is* one event).
//! - **Class scope** is addressable and serves live state, but cannot yet *filter* the log
//!   to one class — there is no class tag to filter on. It returns the same whole-event
//!   live state; precise class filtering lands when the schedule model carries class tags.
//! - **Pilot scope** filters the lap projection to the competitor whose ref equals the
//!   `pilot` id (the registration binding that maps a `PilotId` to source competitors is
//!   out of scope here, Architecture §9; until it lands the pilot id is matched against the
//!   competitor ref directly).
//!
//! These are noted so #43/#44/#45 build on a stable addressing surface while the log-level
//! filters are tightened later.

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use gridfpv_engine::scoring::{WinCondition, score_events};
use gridfpv_events::{CompetitorRef, Event, HeatId, SourceTime};
use gridfpv_projection::{LapList, lap_list_marshaled, registrations};
use gridfpv_storage::{EventLog, Offset, Result as StorageResult, StoredEvent};
use serde::Deserialize;
use tokio::sync::Notify;

use crate::auth::{JoinTokenResponse, TokenStore};
use crate::control_handler::ControlAuth;
use crate::error::{ErrorCode, ProtocolError};
use crate::events::{CreateEventRequest, EventMeta, EventRegistry};
use crate::live_state::live_state;
use crate::scope::{ClassId, EventId, PilotId};
use crate::snapshot::{ProjectionBody, Snapshot};
use crate::stream::Cursor;

/// The object-safe slice of [`EventLog`] the protocol transport needs: read the whole
/// log, read its tail, append, and report its length.
///
/// [`EventLog`] itself is **not** dyn-compatible (its `append_batch` is generic over the
/// iterator type), so it cannot be stored behind a trait object directly. This facade
/// exposes exactly the operations the snapshot reads (and the future control writes, #45,
/// and stream tail, #43) need, and is blanket-implemented for every `EventLog`, so any
/// backend ([`InMemoryLog`](gridfpv_storage::InMemoryLog),
/// [`SqliteLog`](gridfpv_storage::SqliteLog)) drops in behind one `Arc<Mutex<dyn …>>`.
pub trait EventSource {
    /// Read every entry in append order (the snapshot folds these into a projection).
    fn read_all(&self) -> StorageResult<Vec<StoredEvent>>;
    /// Read every entry from `start` (inclusive) to the end — the WS stream tail (#43).
    fn read_from(&self, start: Offset) -> StorageResult<Vec<StoredEvent>>;
    /// Append a single event, returning its assigned offset — the control path (#45).
    fn append(&mut self, event: Event, recorded_at: Option<i64>) -> StorageResult<Offset>;
    /// The number of entries — equivalently the offset the next append receives, which is
    /// the snapshot resume [`Cursor`].
    fn len(&self) -> StorageResult<u64>;
    /// Whether the log has no entries.
    fn is_empty(&self) -> StorageResult<bool> {
        Ok(self.len()? == 0)
    }
}

impl<L: EventLog> EventSource for L {
    fn read_all(&self) -> StorageResult<Vec<StoredEvent>> {
        EventLog::read_all(self)
    }
    fn read_from(&self, start: Offset) -> StorageResult<Vec<StoredEvent>> {
        EventLog::read_from(self, start)
    }
    fn append(&mut self, event: Event, recorded_at: Option<i64>) -> StorageResult<Offset> {
        EventLog::append(self, event, recorded_at)
    }
    fn len(&self) -> StorageResult<u64> {
        EventLog::len(self)
    }
}

/// A thread-safe handle to the one append-only event log every protocol path shares.
///
/// `Send` (not `Send + Sync`) is required on the trait object because it lives behind a
/// [`Mutex`]; the `Arc<Mutex<…>>` makes the whole handle `Send + Sync` so axum can store
/// it in [`AppState`] and clone it into every handler. The object-safe [`EventSource`]
/// facade lets any [`EventLog`] backend sit behind it.
pub type SharedLog = Arc<Mutex<dyn EventSource + Send>>;

/// The shared application state every axum handler is given (protocol.html §2).
///
/// Holds the [`SharedLog`] — the single source of truth the snapshot reads fold, the WS
/// stream (#43) tails, and the control path (#45) appends through. Cloning an `AppState`
/// clones the `Arc`s, so all handlers and tasks share one log and one append signal.
///
/// # Append notification (the stream wakeup, #43)
///
/// The change stream is a long-lived task that, after replaying the log tail, must wake
/// the *instant* a new event is appended so it can fold and push the next envelope. A
/// [`tokio::sync::Notify`] is the wakeup: every [`append`](AppState::append) appends to
/// the log and then `notify_waiters()`. A stream that has caught up to the log tail waits
/// on `notified()`; the next append wakes it and it reads the new tail. `Notify` (rather
/// than a `broadcast` channel) carries no payload — the event itself is read back from
/// the log, the one source of truth — so a slow stream can never lag a bounded channel
/// and miss an event; it always re-reads from where it left off. The control path (#45)
/// drives the very same [`append`](AppState::append) so its writes wake every stream.
#[derive(Clone)]
pub struct AppState {
    log: SharedLog,
    /// Woken on every append so caught-up change streams re-read the log tail.
    appended: Arc<Notify>,
    /// The auth authority (#44): opaque bearer/join tokens → role-bearing sessions. Shared
    /// (it is internally `Arc`'d) so every handler — and the [`ControlAuth`] extractor —
    /// consults the same sessions; control reads it through [`AppState::tokens`].
    ///
    /// [`ControlAuth`]: crate::control_handler::ControlAuth
    tokens: TokenStore,
}

impl AppState {
    /// Build the state from a concrete log backend (e.g. an
    /// [`InMemoryLog`](gridfpv_storage::InMemoryLog) or
    /// [`SqliteLog`](gridfpv_storage::SqliteLog)).
    pub fn new(log: impl EventLog + Send + 'static) -> Self {
        Self {
            log: Arc::new(Mutex::new(log)),
            appended: Arc::new(Notify::new()),
            tokens: TokenStore::new(),
        }
    }

    /// Build the state from an already-shared log handle — for when the WS stream (#43)
    /// or control path (#45) needs to share the *same* `Arc<Mutex<…>>` with the router.
    pub fn from_shared(log: SharedLog) -> Self {
        Self {
            log,
            appended: Arc::new(Notify::new()),
            tokens: TokenStore::new(),
        }
    }

    /// Build the state from a concrete log backend while **sharing** an existing
    /// [`TokenStore`] — used by the [`EventRegistry`](crate::events::EventRegistry) so every
    /// per-event [`AppState`] consults the one auth authority (an RD token authenticates
    /// control on any event; a join token reads any event). Each event keeps its **own** log
    /// and its own append-notify, but the token store is one Director-wide gate.
    pub fn with_tokens(log: impl EventLog + Send + 'static, tokens: TokenStore) -> Self {
        Self {
            log: Arc::new(Mutex::new(log)),
            appended: Arc::new(Notify::new()),
            tokens,
        }
    }

    /// The shared auth token store (#44), for minting/revoking tokens out of band (the RD
    /// console issues itself an RD token; an operator issues a join-token QR) and for the
    /// [`ControlAuth`] extractor to authenticate a control caller.
    ///
    /// [`ControlAuth`]: crate::control_handler::ControlAuth
    pub fn tokens(&self) -> &TokenStore {
        &self.tokens
    }

    /// The shared log handle, for tasks that tail or append outside the router.
    pub fn log(&self) -> SharedLog {
        Arc::clone(&self.log)
    }

    /// Append an event to the log **and wake every subscribed change stream**
    /// (protocol.html §3) — the one write path the control endpoint (#45) reuses.
    ///
    /// Locks the log, appends through [`EventSource::append`] (assigning the next dense
    /// [`Offset`]), unlocks, then `notify_waiters()` so any stream parked on the log tail
    /// wakes and folds the new event into its scope. Returns the assigned offset.
    ///
    /// The notify happens *after* the lock is released and the append has committed, so a
    /// woken stream is guaranteed to see the new event when it re-reads the tail (no woken
    /// stream can observe a torn or not-yet-committed write).
    pub fn append(&self, event: Event, recorded_at: Option<i64>) -> Result<Offset, ProtocolError> {
        let offset = {
            let mut log = self.log.lock().map_err(|_| {
                ProtocolError::new(ErrorCode::Internal, "the event log lock was poisoned")
            })?;
            log.append(event, recorded_at)
                .map_err(|e| ProtocolError::new(ErrorCode::Internal, e.to_string()))?
        };
        self.appended.notify_waiters();
        Ok(offset)
    }

    /// A handle to the append-notification primitive, for the change-stream task to park
    /// on between log reads (see the type docs).
    pub(crate) fn appended(&self) -> Arc<Notify> {
        Arc::clone(&self.appended)
    }

    /// Read the whole log into a `Vec<Event>` plus the resume [`Cursor`] (the log length
    /// at read time). A single lock spans the read so the events and the cursor are
    /// consistent with one another.
    pub(crate) fn read(&self) -> Result<(Vec<Event>, Cursor), ProtocolError> {
        let log = self.log.lock().map_err(|_| {
            ProtocolError::new(ErrorCode::Internal, "the event log lock was poisoned")
        })?;
        let stored = log
            .read_all()
            .map_err(|e| ProtocolError::new(ErrorCode::Internal, e.to_string()))?;
        let cursor = Cursor::new(
            log.len()
                .map_err(|e| ProtocolError::new(ErrorCode::Internal, e.to_string()))?,
        );
        let events = stored.into_iter().map(|s| s.event).collect();
        Ok((events, cursor))
    }
}

/// Build the **event-rooted** protocol [`Router`] over the [`EventRegistry`] (issue #72).
///
/// Every read/realtime/control surface is rooted under its event — `/events/{eventId}/…` —
/// and the handler resolves `eventId` to that event's [`AppState`] (its own log) via the
/// registry before serving (mirroring the within-event scope filtering: heat window, pilot,
/// etc.). The events lifecycle API (`GET /events`, `POST /events`) and a liveness
/// `GET /health` sit at the root. An unknown `eventId` is a typed [`ProtocolError`] 404
/// (`UnknownScope`), the same shape a wrong route gets (#64).
pub fn router(registry: EventRegistry) -> Router {
    // The per-event surface, rooted under `/events/{event_id}`. Each handler resolves the
    // event id to its own `AppState`/log through the registry.
    let read = Router::new()
        .route("/health", get(|| async { "ok" }))
        // Events lifecycle (issue #72): list (Practice first) and RD-gated create.
        .route("/events", get(list_events).post(create_event))
        // Per-event read/realtime surface — `{event_id}` resolves to that event's log.
        .route(
            "/events/{event_id}/snapshot/event/{event}",
            get(snapshot_event),
        )
        .route(
            "/events/{event_id}/snapshot/class/{event}/{class}",
            get(snapshot_class),
        )
        .route(
            "/events/{event_id}/snapshot/heat/{heat}",
            get(snapshot_heat),
        )
        .route(
            "/events/{event_id}/snapshot/pilot/{event}/{pilot}",
            get(snapshot_pilot),
        )
        .route("/events/{event_id}/stream", get(crate::ws::stream_handler))
        // RD-gated mint of a read-only join token (#63), now per-event.
        .route("/events/{event_id}/auth/join-token", post(mint_join_token));
    // The privileged RD control surface (§5) is composed on separately so its auth layer
    // wraps just `/events/{event_id}/control`.
    crate::control_handler::control_routes(read)
        // Any path under a known API tree that matched no route above is a typed 404
        // (#64), not the SPA shell — see [`api_fallback`] / [`smart_fallback`].
        .fallback(api_fallback)
        .with_state(registry)
}

/// Resolve an [`EventId`] to its [`AppState`] through the registry, or a typed 404.
///
/// The single resolution point every per-event handler funnels through: an unknown event id
/// is an [`ErrorCode::UnknownScope`] [`ProtocolError`] (HTTP 404), mirroring an unknown heat
/// / pilot, so a client reads one uniform shape whether the *event* or a *scope within it*
/// is missing.
pub fn resolve_event(registry: &EventRegistry, id: &EventId) -> Result<AppState, ProtocolError> {
    registry.resolve(id).ok_or_else(|| {
        ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no event with id {:?}", id.0),
        )
    })
}

/// `GET /events` — list every event's [`EventMeta`], Practice first (issue #72).
///
/// Reads are open on the LAN (§5), so listing events needs no token — a spectator can see
/// which events exist before scoping into one.
async fn list_events(State(registry): State<EventRegistry>) -> Json<Vec<EventMeta>> {
    Json(registry.list())
}

/// `POST /events` — create a new event from a [`CreateEventRequest`], RD-gated (issue #72).
///
/// [`ControlAuth`] runs first: only an authenticated **RD** may create an event (and with
/// full-trust by default the control gate is open until a token is configured — #72 Slice 1b).
/// The id is **auto-generated** (a slug of the name + a short random suffix) — names are
/// display-only, ids are never user-entered. The request's optional descriptive fields
/// (`date`/`location`/`description`/`organizer`) are stored on the new event's meta. The event
/// gets its own SQLite-backed log under the configured data dir (or an in-memory log when none
/// is configured) and the freshly-created [`EventMeta`] is returned.
async fn create_event(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Json(body): Json<CreateEventRequest>,
) -> Result<Json<EventMeta>, ProtocolError> {
    let meta = registry
        .create(&body)
        .map_err(|e| ProtocolError::new(ErrorCode::Internal, e.to_string()))?;
    Ok(Json(meta))
}

/// `POST /events/{event_id}/auth/join-token` — mint a fresh **read-only** join token
/// (protocol.html §5, §9.4) — issue #63, now event-rooted.
///
/// [`ControlAuth`] runs first: only an authenticated **RD** may mint one. The token store is
/// Director-wide (shared across events), so the minted token reads any event; the path is
/// rooted under an event for a uniform surface and a typed 404 on an unknown event id. On
/// success a brand-new read-only token is returned as a [`JoinTokenResponse`].
async fn mint_join_token(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
) -> Result<Json<JoinTokenResponse>, ProtocolError> {
    let state = resolve_event(&registry, &event_id)?;
    let token = state.tokens().issue_join_token();
    Ok(Json(JoinTokenResponse { token }))
}

/// Whether a request path addresses a known protocol **API tree** (#64).
///
/// The blanket SPA fallback (in the Director wiring) serves `index.html` for any unmatched
/// path so client-side routes resolve. That is wrong for a *mistyped API* path — a bad
/// `/snapshot/...`, `/control/...`, `/stream/...`, `/health/...`, or `/auth/...` — which
/// should surface a typed [`ProtocolError`], not an HTML 200 the client cannot parse as a
/// `Snapshot`. This predicate names the API prefixes so the [`smart_fallback`] can split
/// "mistyped API → 404 JSON" from "genuine client-side route → SPA shell".
///
/// A path matches when it equals a prefix exactly or continues with `/` (so `/snapshotxyz`
/// is *not* an API path, but `/snapshot`, `/snapshot/`, and `/snapshot/zzz` all are).
pub fn is_api_path(path: &str) -> bool {
    // The event-rooted surface (#72) puts snapshot/stream/control/auth *under* `/events`, so
    // `/events` is the one API tree that matters now; the bare `/snapshot|/stream|/control|/auth`
    // prefixes are kept so a *legacy* (pre-#72) mistyped call still 404s as a typed API error
    // rather than falling through to the SPA shell.
    const API_PREFIXES: [&str; 6] = [
        "/health",
        "/events",
        "/snapshot",
        "/stream",
        "/control",
        "/auth",
    ];
    API_PREFIXES.iter().any(|prefix| {
        path == *prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// The typed-404 fallback for **unmatched API-tree** paths (#64).
///
/// Mounted as [`router`]'s own fallback so a request under a known API prefix that matched
/// no concrete route (a wrong `/snapshot/zzz/...`, a `/control/bogus`, a `/auth/nope`)
/// returns a [`ProtocolError`] of [`ErrorCode::UnknownScope`] as JSON (HTTP 404) — the one
/// uniform error shape — rather than falling through to the SPA shell. Non-API paths never
/// reach this fallback when the SPA service is composed via [`smart_fallback`]; reached
/// directly (a bare `router` with no SPA) every unmatched path is a typed 404, which is the
/// correct API-only behaviour.
async fn api_fallback(req: Request<Body>) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::UnknownScope,
        format!(
            "no protocol route matches {} {}",
            req.method(),
            req.uri().path()
        ),
    )
}

/// Compose the Director's outer fallback (#64): a **mistyped API path → typed 404 JSON**,
/// **any other path → the SPA `spa` service** (the client-side router shell).
///
/// The Director mounts the protocol [`router`] first, then needs *one* fallback for
/// everything it does not own. A naive `fallback_service(spa)` serves `index.html` for a
/// mistyped API path too — so a wrong `/snapshot/...` arrives as an HTML 200 the client
/// cannot parse (v0.4 bug #64). This wraps the SPA service so a path under a known API tree
/// ([`is_api_path`]) instead gets the typed [`ProtocolError`] 404 from [`api_fallback`],
/// while genuine client-side routes still resolve to the SPA shell exactly as before.
///
/// Generic over the inner SPA service so the Director's `ServeDir(+ index.html fallback)`
/// drops straight in (its `Response<ServeFileSystemResponseBody>` is mapped into the axum
/// [`Response`]); the returned service is itself usable with [`Router::fallback_service`].
pub fn smart_fallback<S, B>(spa: S) -> Router
where
    S: tower::Service<Request<Body>, Response = Response<B>, Error = std::convert::Infallible>
        + Clone
        + Send
        + Sync
        + 'static,
    S::Future: Send + 'static,
    B: axum::body::HttpBody<Data = axum::body::Bytes> + Send + 'static,
    B::Error: Into<axum::BoxError>,
{
    Router::new()
        // Unmatched API-tree paths → typed 404 (#64). A `Router` with no routes matches
        // nothing, so *every* request hits this fallback; we branch on the path there.
        .fallback(move |req: Request<Body>| {
            let mut spa = spa.clone();
            async move {
                if is_api_path(req.uri().path()) {
                    api_fallback(req).await.into_response()
                } else {
                    // Drive the SPA service for a genuine client-side route, mapping its
                    // body into the axum `Body` the fallback returns.
                    use tower::ServiceExt;
                    match spa.ready().await {
                        Ok(svc) => match svc.call(req).await {
                            Ok(response) => response.map(Body::new),
                            Err(infallible) => match infallible {},
                        },
                        Err(infallible) => match infallible {},
                    }
                }
            }
        })
}

/// The projection a heat-scope snapshot returns. Selected by `?projection=…`; defaults to
/// the live race-state.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum HeatProjection {
    /// The live race-state for the heat (default).
    #[default]
    Live,
    /// The heat's per-pilot [`LapList`].
    Laps,
    /// The heat's scored [`HeatResult`].
    Result,
}

/// Query parameters for the heat-scope endpoint.
#[derive(Debug, Default, Deserialize)]
struct HeatQuery {
    #[serde(default)]
    projection: HeatProjection,
}

/// `GET /events/{event_id}/snapshot/event/{event}` — the whole event's live race-state
/// (§4 event scope), served against the resolved event's own log (issue #72).
///
/// `event_id` resolves to that event's [`AppState`] via the registry (an unknown id → a
/// typed 404). The trailing `{event}` is the §4 scope address within the event; with the
/// log now genuinely per-event, the event-scope body folds **that event's** log — no more
/// whole-log passthrough.
async fn snapshot_event(
    State(registry): State<EventRegistry>,
    Path((event_id, _event)): Path<(EventId, EventId)>,
) -> Result<Json<Snapshot>, ProtocolError> {
    let state = resolve_event(&registry, &event_id)?;
    let (events, cursor) = state.read()?;
    Ok(Json(Snapshot {
        cursor,
        body: ProjectionBody::LiveRaceState(live_state(&events)),
    }))
}

/// `GET /snapshot/class/{event}/{class}` — a class's live race-state (§4 class scope).
///
/// Class-level *filtering* of the log is deferred (the log carries no class tag yet — see
/// the module docs); this serves the whole-event live state under the class address so the
/// scope is reachable now and tightens later without an addressing change.
async fn snapshot_class(
    State(registry): State<EventRegistry>,
    Path((event_id, _event, _class)): Path<(EventId, EventId, ClassId)>,
) -> Result<Json<Snapshot>, ProtocolError> {
    let state = resolve_event(&registry, &event_id)?;
    let (events, cursor) = state.read()?;
    Ok(Json(Snapshot {
        cursor,
        body: ProjectionBody::LiveRaceState(live_state(&events)),
    }))
}

/// `GET /snapshot/heat/{heat}` — the tightest scope (§4 heat scope).
///
/// `?projection=live` (default) returns the heat's [`LiveRaceState`]; `?projection=laps`
/// its [`LapList`]; `?projection=result` its scored [`HeatResult`]. The log is filtered to
/// the heat's window so the body is heat-local.
async fn snapshot_heat(
    State(registry): State<EventRegistry>,
    Path((event_id, heat)): Path<(EventId, HeatId)>,
    Query(query): Query<HeatQuery>,
) -> Result<Json<Snapshot>, ProtocolError> {
    let state = resolve_event(&registry, &event_id)?;
    let (events, cursor) = state.read()?;

    // The heat must exist in the log (a `HeatScheduled` for this id), else UnknownScope.
    let scheduled = events
        .iter()
        .any(|e| matches!(e, Event::HeatScheduled { heat: h, .. } if *h == heat));
    if !scheduled {
        return Err(ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no heat scheduled with id {:?}", heat.0),
        ));
    }

    let heat_events = heat_window(&events, &heat);

    let body = match query.projection {
        HeatProjection::Live => {
            // Fold the heat's window into live state (it is the only heat present, so it
            // is the current one).
            ProjectionBody::LiveRaceState(live_state(&heat_events))
        }
        HeatProjection::Laps => ProjectionBody::LapList(lap_list_marshaled(
            heat_events.iter().enumerate().map(|(i, e)| (i as u64, e)),
        )),
        HeatProjection::Result => {
            // The win condition is heat / format config not carried in the raw log; the
            // snapshot scores the heat's passes under a neutral best-lap qualifying rule
            // so the result body is populated. The authoritative per-heat win condition is
            // applied by the engine when scoring is driven (#45); refining the served
            // result to the configured condition is part of that work.
            let race_start = heat_events
                .iter()
                .filter_map(first_pass_at)
                .min()
                .unwrap_or(SourceTime::from_micros(0));
            ProjectionBody::HeatResult(score_events(
                &heat_events,
                WinCondition::BestLap,
                race_start,
            ))
        }
    };

    Ok(Json(Snapshot { cursor, body }))
}

/// `GET /snapshot/pilot/{event}/{pilot}` — a pilot's laps across the event (§4 pilot scope).
///
/// Resolves the `PilotId` to the source competitor(s) it is **bound** to by the registration
/// projection ([`registrations`], #60), then filters the lap projection to those
/// competitors — so a pilot following their own laps sees every channel they were
/// registered on (e.g. across re-seats / multiple timers). For an event with no registration
/// bindings yet, it falls back to the legacy behaviour of treating the `pilot` id as a bare
/// [`CompetitorRef`], so an un-registered setup still resolves a pilot by their source ref.
async fn snapshot_pilot(
    State(registry): State<EventRegistry>,
    Path((event_id, _event, pilot)): Path<(EventId, EventId, PilotId)>,
) -> Result<Json<Snapshot>, ProtocolError> {
    let state = resolve_event(&registry, &event_id)?;
    let (events, cursor) = state.read()?;

    // The source competitors bound to this pilot (by the registration fold). When the log
    // carries no binding for the pilot, fall back to matching the pilot id against a bare
    // competitor ref (the pre-#60 behaviour) so unregistered events still resolve.
    let bindings = registrations(&events);
    let bound: Vec<CompetitorRef> = bindings
        .iter()
        .filter(|(_, bound_pilot)| **bound_pilot == pilot)
        .map(|(key, _)| key.competitor.clone())
        .collect();
    let fallback_ref = CompetitorRef(pilot.0.clone());

    let full = lap_list_marshaled(events.iter().enumerate().map(|(i, e)| (i as u64, e)));
    let competitors: Vec<_> = full
        .competitors
        .into_iter()
        .filter(|c| {
            if bound.is_empty() {
                c.competitor.competitor == fallback_ref
            } else {
                bound.contains(&c.competitor.competitor)
            }
        })
        .collect();

    if competitors.is_empty() {
        return Err(ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no laps for pilot {:?} in this event", pilot.0),
        ));
    }

    Ok(Json(Snapshot {
        cursor,
        body: ProjectionBody::LapList(LapList { competitors }),
    }))
}

/// The `at` of an event if it is a lap-gate pass, for deriving a heat's race start.
pub(crate) fn first_pass_at(event: &Event) -> Option<SourceTime> {
    match event {
        Event::Pass(p) if p.gate.is_lap_gate() => Some(p.at),
        _ => None,
    }
}

/// Filter the log to a single heat's window: that heat's scheduling / state-change events,
/// plus all passes and marshaling adjudications that fall *while the heat is the active
/// one*.
///
/// With one heat per log in the common case this is the whole log; with several heats it
/// scopes passes to the span between this heat's first scheduling/transition and the next
/// heat's. Passes carry no heat id (they are raw observations), so attribution is by
/// position in the log relative to heat-loop events — the same ordering the engine uses to
/// decide which heat consumes a pass (race-engine.html §2).
pub(crate) fn heat_window(events: &[Event], heat: &HeatId) -> Vec<Event> {
    let mut window = Vec::new();
    // `active` tracks whether the cursor is currently inside this heat's span: it opens on
    // a heat-loop event for `heat` and closes on a heat-loop event for a *different* heat.
    let mut active = false;
    for event in events {
        match event {
            Event::HeatScheduled { heat: h, .. } | Event::HeatStateChanged { heat: h, .. } => {
                active = h == heat;
                if active {
                    window.push(event.clone());
                }
            }
            // Passes and adjudications belong to whichever heat is currently active.
            _ if active => window.push(event.clone()),
            _ => {}
        }
    }
    window
}

/// Render a [`ProtocolError`] as an HTTP error response (protocol.html §9.8): the JSON
/// error body under the status its [`ErrorCode`] maps to. The single shared error shape is
/// returned uniformly across every snapshot path.
impl IntoResponse for ProtocolError {
    fn into_response(self) -> Response {
        let status = match self.code {
            ErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
            ErrorCode::UnknownScope => StatusCode::NOT_FOUND,
            ErrorCode::StaleCursor => StatusCode::GONE,
            ErrorCode::VersionMismatch => StatusCode::UPGRADE_REQUIRED,
            ErrorCode::BadRequest => StatusCode::BAD_REQUEST,
            ErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(self)).into_response()
    }
}

// `EventId` / `ClassId` / `PilotId` / `HeatId` are transparent string newtypes, so axum's
// `Path` extractor deserializes a single path segment straight into them via serde.

#[cfg(test)]
mod tests {
    // `Body` and `Request` come in via `use super::*` (they are `use`d at module level for
    // the smart fallback); the tests reach them through the glob.
    use super::*;
    use gridfpv_events::{AdapterId, GateIndex, HeatTransition, Pass};
    use gridfpv_projection::CompetitorKey;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::snapshot::HeatPhase;

    fn pass(competitor: &str, at: i64, seq: u64) -> Event {
        Event::Pass(Pass {
            adapter: AdapterId("vd".into()),
            competitor: CompetitorRef(competitor.into()),
            at: SourceTime::from_micros(at),
            sequence: Some(seq),
            gate: GateIndex::LAP,
            signal: None,
        })
    }

    /// A recorded heat log: q-1 scheduled, run through to Scored, with laps for A and B.
    fn recorded_heat() -> Vec<Event> {
        vec![
            Event::HeatScheduled {
                heat: HeatId("q-1".into()),
                lineup: vec![CompetitorRef("A".into()), CompetitorRef("B".into())],
            },
            Event::HeatStateChanged {
                heat: HeatId("q-1".into()),
                transition: HeatTransition::Staged,
            },
            Event::HeatStateChanged {
                heat: HeatId("q-1".into()),
                transition: HeatTransition::Armed,
            },
            Event::HeatStateChanged {
                heat: HeatId("q-1".into()),
                transition: HeatTransition::Running,
            },
            pass("A", 1_000_000, 1),
            pass("B", 1_500_000, 1),
            pass("A", 4_000_000, 2), // A lap 1 = 3.0s
            pass("B", 5_500_000, 2), // B lap 1 = 4.0s
            pass("A", 6_500_000, 3), // A lap 2 = 2.5s
            Event::HeatStateChanged {
                heat: HeatId("q-1".into()),
                transition: HeatTransition::Finished,
            },
            Event::HeatStateChanged {
                heat: HeatId("q-1".into()),
                transition: HeatTransition::Scored,
            },
        ]
    }

    use crate::events::{EventRegistry, PRACTICE_EVENT_ID};

    // The per-event route prefix the tests drive is `/events/practice` — the always-present
    // in-memory Practice event (#72); every snapshot/control/auth path is rooted under it.

    /// Build a registry whose **Practice** event log already holds `events`, returning the
    /// registry (the router state), the Practice [`AppState`] (for token minting in tests),
    /// and the log length. Practice is in-memory, so the seed is just appends to its log.
    fn state_with(events: Vec<Event>) -> (EventRegistry, AppState, u64) {
        let registry = EventRegistry::new(None).unwrap();
        let state = registry
            .resolve(&EventId(PRACTICE_EVENT_ID.into()))
            .expect("Practice is always present");
        for e in &events {
            state.append(e.clone(), None).unwrap();
        }
        let len = events.len() as u64;
        (registry, state, len)
    }

    async fn get_snapshot(registry: EventRegistry, uri: &str) -> (StatusCode, Option<Snapshot>) {
        let response = router(registry)
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let snap = serde_json::from_slice::<Snapshot>(&bytes).ok();
        (status, snap)
    }

    #[tokio::test]
    async fn event_scope_returns_live_state_and_cursor() {
        let (registry, _state, len) = state_with(recorded_heat());
        let (status, snap) =
            get_snapshot(registry, "/events/practice/snapshot/event/spring-cup").await;
        assert_eq!(status, StatusCode::OK);
        let snap = snap.unwrap();
        // The cursor is the log length at read time — the resume point.
        assert_eq!(snap.cursor, Cursor::new(len));
        match snap.body {
            ProjectionBody::LiveRaceState(ls) => {
                assert_eq!(ls.current_heat, Some(HeatId("q-1".into())));
                assert_eq!(ls.phase, HeatPhase::Scored);
                assert_eq!(
                    ls.active_pilots,
                    vec![CompetitorRef("A".into()), CompetitorRef("B".into())]
                );
                // A leads (2 laps vs 1).
                assert_eq!(ls.running_order.first(), Some(&CompetitorRef("A".into())));
            }
            other => panic!("expected live state, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn heat_scope_default_is_live_state() {
        let (registry, _state, len) = state_with(recorded_heat());
        let (status, snap) = get_snapshot(registry, "/events/practice/snapshot/heat/q-1").await;
        assert_eq!(status, StatusCode::OK);
        let snap = snap.unwrap();
        assert_eq!(snap.cursor, Cursor::new(len));
        assert!(matches!(snap.body, ProjectionBody::LiveRaceState(_)));
    }

    #[tokio::test]
    async fn heat_scope_laps_projection_returns_lap_list() {
        let (registry, _state, _) = state_with(recorded_heat());
        let (status, snap) = get_snapshot(
            registry,
            "/events/practice/snapshot/heat/q-1?projection=laps",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        match snap.unwrap().body {
            ProjectionBody::LapList(laps) => {
                let a = laps
                    .competitor(&CompetitorKey {
                        adapter: AdapterId("vd".into()),
                        competitor: CompetitorRef("A".into()),
                    })
                    .unwrap();
                assert_eq!(a.lap_count(), 2);
            }
            other => panic!("expected lap list, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn heat_scope_result_projection_returns_heat_result() {
        let (registry, _state, _) = state_with(recorded_heat());
        let (status, snap) = get_snapshot(
            registry,
            "/events/practice/snapshot/heat/q-1?projection=result",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        match snap.unwrap().body {
            ProjectionBody::HeatResult(result) => {
                // Both A and B placed.
                assert_eq!(result.places.len(), 2);
            }
            other => panic!("expected heat result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_heat_is_not_found() {
        let (registry, _state, _) = state_with(recorded_heat());
        let response = router(registry)
            .oneshot(
                Request::builder()
                    .uri("/events/practice/snapshot/heat/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let err: ProtocolError = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(err.code, ErrorCode::UnknownScope);
    }

    #[tokio::test]
    async fn pilot_scope_filters_to_the_pilot_laps() {
        let (registry, _state, len) = state_with(recorded_heat());
        let (status, snap) =
            get_snapshot(registry, "/events/practice/snapshot/pilot/spring-cup/A").await;
        assert_eq!(status, StatusCode::OK);
        let snap = snap.unwrap();
        assert_eq!(snap.cursor, Cursor::new(len));
        match snap.body {
            ProjectionBody::LapList(laps) => {
                // Only pilot A's laps, not B's.
                assert_eq!(laps.competitors.len(), 1);
                assert_eq!(
                    laps.competitors[0].competitor.competitor,
                    CompetitorRef("A".into())
                );
                assert_eq!(laps.competitors[0].lap_count(), 2);
            }
            other => panic!("expected lap list, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pilot_scope_resolves_a_registered_pilot_to_its_bound_competitor() {
        // Bind pilot "acroace" to (vd, A), then query the pilot by their PilotId — the
        // snapshot resolves the binding and returns A's laps (#60).
        let mut events = recorded_heat();
        events.push(Event::CompetitorRegistered {
            adapter: AdapterId("vd".into()),
            competitor: CompetitorRef("A".into()),
            pilot: gridfpv_events::PilotId("acroace".into()),
        });
        let (registry, _state, _) = state_with(events);
        let (status, snap) = get_snapshot(
            registry,
            "/events/practice/snapshot/pilot/spring-cup/acroace",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        match snap.unwrap().body {
            ProjectionBody::LapList(laps) => {
                assert_eq!(laps.competitors.len(), 1);
                assert_eq!(
                    laps.competitors[0].competitor.competitor,
                    CompetitorRef("A".into())
                );
                assert_eq!(laps.competitors[0].lap_count(), 2);
            }
            other => panic!("expected lap list, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_pilot_is_not_found() {
        let (registry, _state, _) = state_with(recorded_heat());
        let response = router(registry)
            .oneshot(
                Request::builder()
                    .uri("/events/practice/snapshot/pilot/spring-cup/nobody")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn class_scope_is_reachable() {
        let (registry, _state, len) = state_with(recorded_heat());
        let (status, snap) =
            get_snapshot(registry, "/events/practice/snapshot/class/spring-cup/open").await;
        assert_eq!(status, StatusCode::OK);
        let snap = snap.unwrap();
        assert_eq!(snap.cursor, Cursor::new(len));
        assert!(matches!(snap.body, ProjectionBody::LiveRaceState(_)));
    }

    #[tokio::test]
    async fn empty_log_event_scope_is_idle_with_zero_cursor() {
        let (registry, _state, _) = state_with(vec![]);
        let (status, snap) =
            get_snapshot(registry, "/events/practice/snapshot/event/spring-cup").await;
        assert_eq!(status, StatusCode::OK);
        let snap = snap.unwrap();
        assert_eq!(snap.cursor, Cursor::new(0));
        match snap.body {
            ProjectionBody::LiveRaceState(ls) => assert_eq!(ls.current_heat, None),
            other => panic!("expected idle live state, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn two_heats_scope_to_their_own_windows() {
        // Two heats in one log; the heat scope must filter to its own passes.
        let events = vec![
            Event::HeatScheduled {
                heat: HeatId("q-1".into()),
                lineup: vec![CompetitorRef("A".into())],
            },
            Event::HeatStateChanged {
                heat: HeatId("q-1".into()),
                transition: HeatTransition::Running,
            },
            pass("A", 1_000_000, 1),
            pass("A", 4_000_000, 2), // q-1: A one lap
            Event::HeatScheduled {
                heat: HeatId("q-2".into()),
                lineup: vec![CompetitorRef("B".into())],
            },
            Event::HeatStateChanged {
                heat: HeatId("q-2".into()),
                transition: HeatTransition::Running,
            },
            pass("B", 10_000_000, 1),
            pass("B", 13_000_000, 2),
            pass("B", 15_000_000, 3), // q-2: B two laps
        ];
        let (registry, _state, _) = state_with(events);

        let (_, snap) = get_snapshot(
            registry.clone(),
            "/events/practice/snapshot/heat/q-1?projection=laps",
        )
        .await;
        match snap.unwrap().body {
            ProjectionBody::LapList(laps) => {
                // Only A appears in q-1's window.
                assert_eq!(laps.competitors.len(), 1);
                assert_eq!(
                    laps.competitors[0].competitor.competitor,
                    CompetitorRef("A".into())
                );
            }
            other => panic!("expected lap list, got {other:?}"),
        }

        let (_, snap) = get_snapshot(
            registry,
            "/events/practice/snapshot/heat/q-2?projection=laps",
        )
        .await;
        match snap.unwrap().body {
            ProjectionBody::LapList(laps) => {
                assert_eq!(laps.competitors.len(), 1);
                assert_eq!(
                    laps.competitors[0].competitor.competitor,
                    CompetitorRef("B".into())
                );
                assert_eq!(laps.competitors[0].lap_count(), 2);
            }
            other => panic!("expected lap list, got {other:?}"),
        }
    }

    // --- #64: unknown API-tree paths are a typed 404, not the SPA shell -----------------

    /// Drive a request against the bare protocol `router` (no SPA composed) and return the
    /// status plus the parsed [`ProtocolError`] body, if the body is one.
    async fn get_raw(registry: EventRegistry, uri: &str) -> (StatusCode, Option<ProtocolError>) {
        let response = router(registry)
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let err = serde_json::from_slice::<ProtocolError>(&bytes).ok();
        (status, err)
    }

    #[test]
    fn is_api_path_matches_only_the_known_trees() {
        // Exact prefix and any `/`-continuation are API paths.
        for p in [
            "/health",
            "/snapshot",
            "/snapshot/zzz/q-1",
            "/stream",
            "/control",
            "/control/bogus",
            "/auth",
            "/auth/join-token",
        ] {
            assert!(is_api_path(p), "{p} should be an API path");
        }
        // A bare client-side route — and a prefix that is only a substring — are NOT.
        for p in [
            "/",
            "/heats/q-1/live",
            "/snapshotxyz",
            "/streaming",
            "/index.html",
        ] {
            assert!(!is_api_path(p), "{p} should NOT be an API path");
        }
    }

    #[tokio::test]
    async fn unknown_snapshot_route_is_typed_404_not_spa() {
        // A wrong /snapshot/... shape (an extra/garbage segment) matched no route → typed 404.
        let (registry, _state, _) = state_with(recorded_heat());
        let (status, err) = get_raw(registry, "/snapshot/zzz/nope/extra").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            err.expect("a ProtocolError body").code,
            ErrorCode::UnknownScope
        );
    }

    #[tokio::test]
    async fn bogus_control_path_is_typed_404() {
        let (registry, _state, _) = state_with(recorded_heat());
        let (status, err) = get_raw(registry, "/control/bogus").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            err.expect("a ProtocolError body").code,
            ErrorCode::UnknownScope
        );
    }

    #[tokio::test]
    async fn a_real_route_still_works_alongside_the_api_fallback() {
        let (registry, _state, _) = state_with(recorded_heat());
        let (status, snap) = get_snapshot(registry, "/events/practice/snapshot/heat/q-1").await;
        assert_eq!(status, StatusCode::OK);
        assert!(matches!(
            snap.unwrap().body,
            ProjectionBody::LiveRaceState(_)
        ));
    }

    #[tokio::test]
    async fn smart_fallback_serves_spa_for_non_api_routes_and_404s_api_ones() {
        use axum::response::Html;

        // An inner "SPA" service that returns a recognisable shell for any path.
        let spa = tower::service_fn(|_req: Request<Body>| async {
            Ok::<_, std::convert::Infallible>(Html("<title>RD Console</title>").into_response())
        });
        let app = smart_fallback(spa);

        // A genuine client-side route → the SPA shell (200), not a typed 404.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/heats/q-1/live")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&bytes).contains("RD Console"));

        // A mistyped API path → typed 404 ProtocolError, NOT the SPA shell.
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/snapshot/zzz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let err: ProtocolError = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(err.code, ErrorCode::UnknownScope);
    }

    // --- #63: minting a read-only join token over HTTP ----------------------------------

    /// `POST /auth/join-token` with an optional bearer token; returns status + parsed body.
    async fn post_join_token(
        registry: EventRegistry,
        token: Option<&str>,
    ) -> (StatusCode, Option<JoinTokenResponse>) {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/events/practice/auth/join-token");
        if let Some(token) = token {
            builder = builder.header("Authorization", format!("Bearer {token}"));
        }
        let response = router(registry)
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = serde_json::from_slice::<JoinTokenResponse>(&bytes).ok();
        (status, body)
    }

    #[tokio::test]
    async fn rd_token_mints_a_join_token_that_reads_but_cannot_control() {
        use crate::auth::Role;

        let (registry, state, _) = state_with(recorded_heat());
        let rd = state.tokens().issue_rd_token();

        // An RD mints a fresh read-only join token over HTTP.
        let (status, body) = post_join_token(registry.clone(), Some(&rd)).await;
        assert_eq!(status, StatusCode::OK);
        let join = body.expect("a JoinTokenResponse body").token;
        assert!(!join.is_empty());

        // The minted token authenticates a READ as a read-only session…
        let read = state
            .tokens()
            .authenticate_read(Some(&join))
            .unwrap()
            .expect("the minted token resolves a session");
        assert_eq!(read.role, Role::ReadOnly);
        // …but is rejected on CONTROL.
        assert_eq!(
            state
                .tokens()
                .authenticate_control(Some(&join))
                .unwrap_err()
                .code,
            ErrorCode::Unauthorized
        );
    }

    #[tokio::test]
    async fn minting_a_join_token_requires_an_rd_token_once_one_is_configured() {
        let (registry, state, _) = state_with(recorded_heat());
        // Configure a control credential so the full-trust default closes (#72, Slice 1b):
        // without this, control is open and a no-token mint would succeed.
        let _rd = state.tokens().issue_rd_token();

        // No token → 401 (control is now gated).
        let (status, _) = post_join_token(registry.clone(), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // A read-only/join token → 401 (it may not mint another).
        let join = state.tokens().issue_join_token();
        let (status, _) = post_join_token(registry.clone(), Some(&join)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // An unknown token → 401.
        let (status, _) = post_join_token(registry, Some("not-a-real-token")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn minting_a_join_token_is_open_when_no_rd_token_is_configured() {
        // Full-trust default (#72, Slice 1b): an unconfigured Director has no control
        // credential, so a no-token caller may mint a join token (control is open).
        let (registry, _state, _) = state_with(recorded_heat());
        let (status, body) = post_join_token(registry, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_some_and(|b| !b.token.is_empty()));
    }
}
