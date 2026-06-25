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
//! | heat  | `GET /snapshot/heat/{heat}` | [`LiveRaceState`] for that heat, or — with `?projection=laps` / `?projection=audit` / `?projection=result` — its [`LapList`] / marshaling audit trail / [`HeatResult`] |
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
use axum::routing::{get, post, put};
use gridfpv_engine::format::{FormatRegistry, FormatSchema};
use gridfpv_engine::scoring::{WinCondition, score_with_global_offsets};
use gridfpv_events::{CompetitorRef, Event, HeatId, SourceTime};
use gridfpv_projection::{
    LapList, lap_list_marshaled, marshaling_log, registrations, signal_trace,
};
use gridfpv_storage::{EventLog, Offset, Result as StorageResult, StoredEvent};
use serde::Deserialize;
use tokio::sync::Notify;

use crate::auth::{JoinTokenResponse, TokenStore};
use crate::channels::ChannelCatalogEntry;
use crate::classes::{
    Class, ClassError, ClassErrorKind, CreateClassRequest, SetClassHiddenRequest,
    UpdateClassRequest,
};
use crate::control_handler::ControlAuth;
use crate::error::{ErrorCode, ProtocolError};
use crate::events::{
    ActiveEvent, CreateEventRequest, EventMeta, EventRegistry, NewRoundReq, RoundDef, RoundError,
    SetActiveEventRequest, SetClassMembershipRequest, SetEventClassesRequest,
    SetEventRosterRequest, UpdateRoundReq,
};
use crate::live_state::{HeatSummary, heat_summaries, live_state, with_heat_timing};
use crate::pilots::{CreatePilotRequest, Pilot, PilotError, PilotErrorKind, UpdatePilotRequest};
use crate::round_engine;
use crate::scope::{ClassId, EventId, PilotId};
use crate::snapshot::{ProjectionBody, Snapshot};
use crate::stream::Cursor;
use crate::timers::{
    CreateTimerRequest, SetEventTimersRequest, SetPrimaryTimerRequest, Timer, TimerId,
    UpdateTimerRequest,
};
use gridfpv_events::RoundId;

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
    /// The event's **open-practice live accumulator** (open-practice format, Slice 1): the
    /// per-channel, in-memory (NOT logged) laps for an active open-practice heat. The source bridge
    /// writes the heat's passes here instead of the log; the `/stream` live-state fold overlays its
    /// computed [`LiveRaceState`](crate::live_state::LiveRaceState) so the non-logged laps still
    /// drive the live view. `None` when no open-practice heat is active. Shared per the `AppState`'s
    /// `Arc`s, so the bridge and the stream see the one cell.
    open_practice: crate::open_practice::OpenPracticeLive,
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
            open_practice: crate::open_practice::OpenPracticeLive::new(),
        }
    }

    /// Build the state from an already-shared log handle — for when the WS stream (#43)
    /// or control path (#45) needs to share the *same* `Arc<Mutex<…>>` with the router.
    pub fn from_shared(log: SharedLog) -> Self {
        Self {
            log,
            appended: Arc::new(Notify::new()),
            tokens: TokenStore::new(),
            open_practice: crate::open_practice::OpenPracticeLive::new(),
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
            open_practice: crate::open_practice::OpenPracticeLive::new(),
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
        // Server-authoritative race clock (#62 follow-up): a heat's `Armed → Running` and
        // `Running → Unofficial` transitions are the race-start / race-end instants the live
        // clock anchors to. The runtime/control paths append them with no caller timestamp, so
        // stamp the server wall clock here (the single append choke point) when one is absent —
        // making the transition's `recorded_at` the authoritative timing every client reads.
        // A caller-supplied timestamp (a replay, a test pinning an instant) still wins.
        let recorded_at = recorded_at
            .or_else(|| matches!(event, Event::HeatStateChanged { .. }).then(now_micros));
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

    /// The event's **open-practice live accumulator** (open-practice format, Slice 1) — the shared
    /// per-channel, in-memory (NOT logged) lap store. The source bridge writes an open-practice
    /// heat's passes here (via [`OpenPracticeLive::record`](crate::open_practice::OpenPracticeLive::record));
    /// the `/stream` live-state fold overlays its computed live state. Cloning shares the one cell.
    pub fn open_practice(&self) -> crate::open_practice::OpenPracticeLive {
        self.open_practice.clone()
    }

    /// **Wake every subscribed change stream** without appending to the log — the non-log push the
    /// open-practice live delivery uses (open-practice format, Slice 1).
    ///
    /// An open-practice heat's laps are accumulated in memory (not logged), so they never reach the
    /// log's append-notify; after mutating the [`open_practice`](Self::open_practice) accumulator the
    /// bridge calls this so a parked stream re-folds and pushes a fresh-value
    /// [`LiveRaceState`](crate::live_state::LiveRaceState) envelope reflecting the new per-channel
    /// laps — reusing the exact same wakeup `append` uses, just without a log write.
    pub fn wake_streams(&self) {
        self.appended.notify_waiters();
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

    /// Read the whole log as [`StoredEvent`]s (carrying each entry's `recorded_at`) plus the
    /// resume [`Cursor`]. Like [`read`](Self::read) but keeps the server timestamps the
    /// live-state clock is anchored to (the `Running` / `Unofficial` transition instants —
    /// see [`live_state::with_heat_timing`](crate::live_state::with_heat_timing)).
    pub(crate) fn read_stored(&self) -> Result<(Vec<StoredEvent>, Cursor), ProtocolError> {
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
        Ok((stored, cursor))
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
        // RD-gated **permanent** delete of an event + ALL its data (the papercut fix): the
        // registry entry, the persisted `<id>.sqlite`(+wal/shm), and the active pointer if it
        // pointed here. The built-in Practice event cannot be deleted (a `BadRequest`); an
        // unknown id is a typed 404.
        .route("/events/{event_id}", axum::routing::delete(delete_event))
        // The Director's active event (issue #90): an open read so any client resumes into the
        // selected event on connect/reload, and an RD-gated write to set it.
        .route("/active-event", get(get_active_event).put(set_active_event))
        // Application-level timers (issue #73): the persisted registry the RD configures once and
        // each event selects from. `GET /timers` is an open read (Mock first); create/edit/
        // delete are RD-gated. `DELETE` rejects the built-in Mock.
        .route("/timers", get(list_timers).post(create_timer))
        .route("/timers/{timer_id}", put(update_timer).delete(delete_timer))
        // Application-level pilots (issue #74): the persisted directory the RD maintains once and
        // each event rosters from. `GET /pilots` is an open read; create/edit/delete are RD-gated.
        .route("/pilots", get(list_pilots).post(create_pilot))
        .route("/pilots/{pilot_id}", put(update_pilot).delete(delete_pilot))
        // Application-level classes (issue #84): the persisted directory the RD maintains once and
        // each event selects from. `GET /classes` is an open read; create/edit/delete are RD-gated.
        .route("/classes", get(list_classes).post(create_class))
        .route(
            "/classes/{class_id}",
            put(update_class).delete(delete_class),
        )
        // Hide/archive a class (hide/archive classes): a control-gated visibility toggle, valid for
        // built-in + custom classes. The id stays in the directory; hiding only filters it from the
        // per-event class picker. Persisted to a sidecar so a hidden built-in survives the re-seed.
        .route("/classes/{class_id}/hidden", put(set_class_hidden))
        // The valid **format names** (race redesign Slice 2b): the single source of truth the
        // Rounds UI's format dropdown reads, straight from [`FormatRegistry::standard`]. An open
        // read (no token) — it is static configuration, not event state.
        .route("/formats", get(list_formats))
        // The standard **FPV channel catalog** (race redesign Slice 4b): the shared band/channel ↔
        // raw-MHz vocabulary the Channels UI offers (a timer's available-channels picker) and reads
        // back to label a heat's assigned frequencies. An open read (no token) — static, compiled-in
        // configuration like `/formats`, not per-event state.
        .route("/channels", get(list_channels))
        // Per-event class **selection** (issue #84): RD-gated; each id must name a known directory
        // class. Set the whole selection wholesale (mirrors the timer selection).
        .route("/events/{event_id}/classes", put(set_event_classes))
        // Per-class **membership** (race redesign Slice 1a): RD-gated; the class must be selected by
        // the event and each pilot id must name a known directory pilot. Replaces that class's pilot
        // list wholesale (an empty list clears it).
        .route(
            "/events/{event_id}/classes/{class_id}/membership",
            put(set_class_membership),
        )
        // Per-event **rounds** (race redesign Slice 2a): RD-gated. Add a round (POST, id
        // generated), or update/remove an existing one by its generated round id. Each class must be
        // selected by the event, the format must be known, and a `FromRanking` seeding source must
        // name an existing round.
        .route("/events/{event_id}/rounds", post(add_round))
        .route(
            "/events/{event_id}/rounds/{round_id}",
            put(update_round).delete(remove_round),
        )
        // Per-event scheduled **heats** (race redesign Slice 3b): the round-tagged heats list the
        // Heats UI reads — open, no token (a read), like the snapshot routes.
        .route("/events/{event_id}/heats", get(list_heats))
        // A round's **ranking** (race redesign Slice 5/6a): the ordered per-pilot ranking the
        // engine seeds `FromRanking` from — what the bracket-carry UI displays. Open, no token.
        .route(
            "/events/{event_id}/rounds/{round_id}/ranking",
            get(round_ranking),
        )
        // A class's **standings** (race redesign Slice 5/6a): the per-pilot rows aggregated across
        // the class's rounds (the season-join shape the Results UI reads). Open, no token.
        .route(
            "/events/{event_id}/classes/{class_id}/standings",
            get(class_standings),
        )
        // Per-event **roster** (issue #74): RD-gated; each id must name a known directory pilot.
        // Set the whole roster, or add/remove a single pilot.
        .route("/events/{event_id}/roster", put(set_event_roster))
        .route(
            "/events/{event_id}/roster/{pilot_id}",
            post(add_to_roster).delete(remove_from_roster),
        )
        // Per-event timer **selection** (issue #73): RD-gated; each id must name a known timer.
        .route("/events/{event_id}/timers", put(set_event_timers))
        // Per-event **primary** timer (issue #112): RD-gated; the id must be in the selection.
        .route(
            "/events/{event_id}/primary-timer",
            put(set_event_primary_timer),
        )
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

/// `DELETE /events/{event_id}` — **permanently** delete an event and ALL of its data, RD-gated
/// (the papercut fix).
///
/// [`ControlAuth`] runs first (the same gate as create; open in full-trust by default). The
/// delete is total and irreversible: the registry entry, the event's persisted state (its
/// `<id>.sqlite` log plus the WAL/SHM sidecars under the data dir), and the active-event pointer
/// if it pointed at this event are all removed — so nothing of it survives a restart. The
/// built-in **Practice** event cannot be deleted (a `BadRequest`); an unknown id is a typed 404
/// (`UnknownScope`). On success an empty 200 is returned (no body — like the other deletes).
async fn delete_event(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
) -> Result<StatusCode, ProtocolError> {
    registry.delete(&event_id).map_err(|e| {
        // A protected-Practice delete is a client error (BadRequest); a genuinely unknown id is a
        // 404. Distinguish by whether the event still resolves (it does not after a real delete).
        let code = if registry.resolve(&event_id).is_some() {
            ErrorCode::BadRequest
        } else {
            ErrorCode::UnknownScope
        };
        ProtocolError::new(code, e.to_string())
    })?;
    Ok(StatusCode::OK)
}

/// `GET /active-event` — the Director's currently-active event, or `null` (issue #90).
///
/// An **open read** (no token): every client — RD console, pilot view, read-only spectator —
/// reads this on connect/reload to resume into the selected event (or fall to the picker when
/// `null`). The active event is Director state, not per-client browser state, so a reload /
/// reconnect / app-restart resumes into the same event all clients are on.
async fn get_active_event(State(registry): State<EventRegistry>) -> Json<ActiveEvent> {
    Json(ActiveEvent {
        event: registry.active(),
    })
}

/// `PUT /active-event` — set the Director's active event, RD-gated (issue #90).
///
/// [`ControlAuth`] runs first (the same gate as every other control write): only an
/// authenticated RD may change which event the Director is on (open in full-trust by default).
/// The body's `id` must name a known event, else a typed 404 (`UnknownScope`). On success the
/// active event is persisted server-side (surviving a Director restart) and its [`EventMeta`]
/// is returned.
async fn set_active_event(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Json(body): Json<SetActiveEventRequest>,
) -> Result<Json<EventMeta>, ProtocolError> {
    let meta = registry
        .set_active(&body.id)
        .map_err(|e| ProtocolError::new(ErrorCode::UnknownScope, e.to_string()))?;
    Ok(Json(meta))
}

/// `GET /timers` — list every configured timer, **Mock first** (issue #73).
///
/// An **open read** (no token): a client renders the timer picker / per-event selection without a
/// credential, mirroring `GET /events`.
async fn list_timers(State(registry): State<EventRegistry>) -> Json<Vec<Timer>> {
    Json(registry.timers().list())
}

/// `POST /timers` — create a timer from a [`CreateTimerRequest`], RD-gated (issue #73).
///
/// [`ControlAuth`] runs first (open in full-trust by default). The id is auto-generated
/// server-side (a name slug + suffix); the new [`Timer`] is returned and the registry persisted.
async fn create_timer(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Json(body): Json<CreateTimerRequest>,
) -> Result<Json<Timer>, ProtocolError> {
    let timer = registry
        .timers()
        .create(&body)
        .map_err(|e| ProtocolError::new(ErrorCode::Internal, e.to_string()))?;
    Ok(Json(timer))
}

/// `PUT /timers/{timer_id}` — edit a timer's name/config, RD-gated (issue #73).
///
/// The built-in Mock may be retuned but not removed (that is `DELETE`'s concern). An unknown
/// id is a typed 404 (`UnknownScope`). On success the updated [`Timer`] is returned and the
/// registry persisted.
async fn update_timer(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(timer_id): Path<TimerId>,
    Json(body): Json<UpdateTimerRequest>,
) -> Result<Json<Timer>, ProtocolError> {
    let timer = registry
        .timers()
        .update(&timer_id, &body)
        .map_err(|e| ProtocolError::new(ErrorCode::UnknownScope, e.to_string()))?;
    Ok(Json(timer))
}

/// `DELETE /timers/{timer_id}` — remove a timer, RD-gated (issue #73).
///
/// The built-in **Mock cannot be deleted** (it is always present) — attempting to is a
/// `BadRequest`; an unknown id is a 404 (`UnknownScope`). On success an empty 200 is returned and
/// the registry persisted.
async fn delete_timer(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(timer_id): Path<TimerId>,
) -> Result<StatusCode, ProtocolError> {
    // The protected-Mock delete is a client error (BadRequest); a genuinely unknown id is a
    // 404. Distinguish by whether the timer exists at all.
    registry.timers().delete(&timer_id).map_err(|e| {
        let code = if registry.timers().exists(&timer_id) {
            ErrorCode::BadRequest
        } else {
            ErrorCode::UnknownScope
        };
        ProtocolError::new(code, e.to_string())
    })?;
    Ok(StatusCode::OK)
}

/// `PUT /events/{event_id}/timers` — set an event's **selected timers** (issue #73) and optionally
/// the **primary** among them (issue #112), RD-gated.
///
/// [`ControlAuth`] runs first. The event must exist (else a typed 404) and **each** id in the body
/// must name a known timer in the registry (else a 404 naming the bad id) — so an event can never
/// reference a deleted/unknown timer. When a `primary` is given it must be one of `ids` (else a
/// 400). On success the updated [`EventMeta`] is returned.
async fn set_event_timers(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
    Json(body): Json<SetEventTimersRequest>,
) -> Result<Json<EventMeta>, ProtocolError> {
    // Validate every selected timer exists before recording the selection.
    let timers = registry.timers();
    for id in &body.ids {
        if !timers.exists(id) {
            return Err(ProtocolError::new(
                ErrorCode::UnknownScope,
                format!("no timer with id {:?}", id.0),
            ));
        }
    }
    // A primary, if given, must be one of the timers being selected (issue #112).
    if let Some(primary) = &body.primary {
        if !body.ids.contains(primary) {
            return Err(ProtocolError::new(
                ErrorCode::BadRequest,
                format!(
                    "primary timer {:?} is not in the selected timers",
                    primary.0
                ),
            ));
        }
    }
    registry
        .set_timers(&event_id, body.ids)
        .map_err(|e| ProtocolError::new(ErrorCode::UnknownScope, e.to_string()))?;
    // Record the primary in the same request (it is now guaranteed in the just-set selection).
    let meta = registry
        .set_primary_timer(&event_id, body.primary)
        .map_err(|e| ProtocolError::new(ErrorCode::UnknownScope, e.to_string()))?;
    Ok(Json(meta))
}

/// `PUT /events/{event_id}/primary-timer` — designate an event's **primary** timer, RD-gated
/// (issue #112).
///
/// [`ControlAuth`] runs first. The event must exist (else a typed 404). When `id` is given it must
/// be one of the event's **currently-selected** timers (else a 400); `null` clears the override so
/// the first selected timer becomes the effective primary. On success the updated [`EventMeta`] is
/// returned. The per-event source bridge reads the primary live, so a change fails over the active
/// source on the next poll.
async fn set_event_primary_timer(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
    Json(body): Json<SetPrimaryTimerRequest>,
) -> Result<Json<EventMeta>, ProtocolError> {
    let meta = registry
        .set_primary_timer(&event_id, body.id)
        .map_err(|e| {
            // "not in the selection" is a bad request; an unknown event is a 404.
            let code = if e.0.contains("not in the event's selected timers") {
                ErrorCode::BadRequest
            } else {
                ErrorCode::UnknownScope
            };
            ProtocolError::new(code, e.to_string())
        })?;
    Ok(Json(meta))
}

/// `GET /pilots` — list every pilot in the directory, in id order (issue #74).
///
/// An **open read** (no token): a client renders the pilot directory / per-event roster picker
/// without a credential, mirroring `GET /timers`.
async fn list_pilots(State(registry): State<EventRegistry>) -> Json<Vec<Pilot>> {
    Json(registry.pilots().list())
}

/// `POST /pilots` — create a pilot from a [`CreatePilotRequest`], RD-gated (issue #74).
///
/// [`ControlAuth`] runs first (open in full-trust by default). The `callsign` is required; the id
/// is auto-generated server-side (a callsign slug + suffix). The new [`Pilot`] is returned and the
/// directory persisted. A missing/blank callsign is a `BadRequest`.
async fn create_pilot(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Json(body): Json<CreatePilotRequest>,
) -> Result<Json<Pilot>, ProtocolError> {
    let pilot = registry
        .pilots()
        .create(&body)
        .map_err(pilot_error_to_protocol)?;
    Ok(Json(pilot))
}

/// `PUT /pilots/{pilot_id}` — edit a pilot's callsign/metadata, RD-gated (issue #74).
///
/// An unknown id is a typed 404 (`UnknownScope`). On success the updated [`Pilot`] is returned and
/// the directory persisted.
async fn update_pilot(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(pilot_id): Path<PilotId>,
    Json(body): Json<UpdatePilotRequest>,
) -> Result<Json<Pilot>, ProtocolError> {
    let pilot = registry
        .pilots()
        .update(&pilot_id, &body)
        .map_err(pilot_error_to_protocol)?;
    Ok(Json(pilot))
}

/// `DELETE /pilots/{pilot_id}` — remove a pilot from the directory, RD-gated (issue #74).
///
/// An unknown id is a 404 (`UnknownScope`). On success an empty 200 is returned and the directory
/// persisted. A stale roster id on some event is harmless (rosters tolerate an unknown id).
async fn delete_pilot(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(pilot_id): Path<PilotId>,
) -> Result<StatusCode, ProtocolError> {
    registry
        .pilots()
        .delete(&pilot_id)
        .map_err(pilot_error_to_protocol)?;
    Ok(StatusCode::OK)
}

/// Map a [`PilotError`] to a typed [`ProtocolError`] (issue #74): a validation failure is a
/// `BadRequest` (400), an unknown id is an `UnknownScope` (404), and a persistence failure is
/// `Internal` (500).
fn pilot_error_to_protocol(error: PilotError) -> ProtocolError {
    let code = match error.kind {
        PilotErrorKind::Invalid => ErrorCode::BadRequest,
        PilotErrorKind::NotFound => ErrorCode::UnknownScope,
        PilotErrorKind::Internal => ErrorCode::Internal,
    };
    ProtocolError::new(code, error.to_string())
}

/// `PUT /events/{event_id}/roster` — set an event's **roster** (issue #74), RD-gated.
///
/// [`ControlAuth`] runs first. The event must exist (else a typed 404) and **each** id in the body
/// must name a known directory pilot (else a 404 naming the bad id) — so a roster can never
/// reference a deleted/unknown pilot. On success the updated [`EventMeta`] is returned.
async fn set_event_roster(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
    Json(body): Json<SetEventRosterRequest>,
) -> Result<Json<EventMeta>, ProtocolError> {
    let pilots = registry.pilots();
    for id in &body.pilot_ids {
        if !pilots.exists(id) {
            return Err(ProtocolError::new(
                ErrorCode::UnknownScope,
                format!("no pilot with id {:?}", id.0),
            ));
        }
    }
    let meta = registry
        .set_roster(&event_id, body.pilot_ids)
        .map_err(|e| ProtocolError::new(ErrorCode::UnknownScope, e.to_string()))?;
    Ok(Json(meta))
}

/// `POST /events/{event_id}/roster/{pilot_id}` — add one pilot to an event's roster (issue #74),
/// RD-gated. The event must exist and the pilot must name a known directory pilot (else a 404).
/// Idempotent. On success the updated [`EventMeta`] is returned.
async fn add_to_roster(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path((event_id, pilot_id)): Path<(EventId, PilotId)>,
) -> Result<Json<EventMeta>, ProtocolError> {
    if !registry.pilots().exists(&pilot_id) {
        return Err(ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no pilot with id {:?}", pilot_id.0),
        ));
    }
    let meta = registry
        .add_to_roster(&event_id, pilot_id)
        .map_err(|e| ProtocolError::new(ErrorCode::UnknownScope, e.to_string()))?;
    Ok(Json(meta))
}

/// `DELETE /events/{event_id}/roster/{pilot_id}` — remove one pilot from an event's roster
/// (issue #74), RD-gated. The event must exist (else a 404); removing a pilot not on the roster is
/// a no-op (the pilot need not still exist in the directory). On success the updated [`EventMeta`]
/// is returned.
async fn remove_from_roster(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path((event_id, pilot_id)): Path<(EventId, PilotId)>,
) -> Result<Json<EventMeta>, ProtocolError> {
    let meta = registry
        .remove_from_roster(&event_id, &pilot_id)
        .map_err(|e| ProtocolError::new(ErrorCode::UnknownScope, e.to_string()))?;
    Ok(Json(meta))
}

/// `GET /classes` — list every class in the directory, in id order (issue #84).
///
/// An **open read** (no token): a client renders the class directory / per-event selection picker
/// without a credential, mirroring `GET /pilots`.
async fn list_classes(State(registry): State<EventRegistry>) -> Json<Vec<Class>> {
    Json(registry.classes().list())
}

/// `GET /formats` — the valid **formats + their param schemas** (race redesign Slice 2b / 7a).
///
/// The single source of truth the Rounds UI reads: each production format
/// ([`FormatRegistry::standard`]) with the **param schema** its generator consumes
/// ([`FormatRegistry::standard_schemas`]) — `{ name, params: [{ key, label, kind, options?,
/// default? }] }` — so the UI renders both the format dropdown and a per-format params editor. An
/// open read (no token) — static, compiled-in configuration, not per-event state.
async fn list_formats() -> Json<Vec<FormatSchema>> {
    Json(FormatRegistry::standard_schemas())
}

/// `GET /channels` — the standard **FPV channel catalog** (race redesign Slice 4b).
///
/// The shared band/channel ↔ raw-MHz vocabulary the Channels UI reads: it offers these
/// human-readable labels when a Race Director picks a Flexible timer's available channels, and
/// resolves a heat's assigned raw frequency back to a band+channel label. An open read (no token) —
/// static, compiled-in configuration straight from [`crate::channels::catalog`], not event state.
async fn list_channels() -> Json<Vec<ChannelCatalogEntry>> {
    Json(crate::channels::catalog())
}

/// `POST /classes` — create a class from a [`CreateClassRequest`], RD-gated (issue #84).
///
/// [`ControlAuth`] runs first (open in full-trust by default). The `name` is required; the id is
/// auto-generated server-side (a name slug + suffix). The new [`Class`] is returned and the
/// directory persisted. A missing/blank name is a `BadRequest`.
async fn create_class(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Json(body): Json<CreateClassRequest>,
) -> Result<Json<Class>, ProtocolError> {
    let class = registry
        .classes()
        .create(&body)
        .map_err(class_error_to_protocol)?;
    Ok(Json(class))
}

/// `PUT /classes/{class_id}` — edit a class's name/source/metadata, RD-gated (issue #84).
///
/// An unknown id is a typed 404 (`UnknownScope`). On success the updated [`Class`] is returned and
/// the directory persisted.
async fn update_class(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(class_id): Path<ClassId>,
    Json(body): Json<UpdateClassRequest>,
) -> Result<Json<Class>, ProtocolError> {
    let class = registry
        .classes()
        .update(&class_id, &body)
        .map_err(class_error_to_protocol)?;
    Ok(Json(class))
}

/// `DELETE /classes/{class_id}` — remove a class from the directory, RD-gated (issue #84).
///
/// An unknown id is a 404 (`UnknownScope`). On success an empty 200 is returned and the directory
/// persisted. A stale selection id on some event is harmless (selections tolerate an unknown id).
async fn delete_class(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(class_id): Path<ClassId>,
) -> Result<StatusCode, ProtocolError> {
    registry
        .classes()
        .delete(&class_id)
        .map_err(class_error_to_protocol)?;
    Ok(StatusCode::OK)
}

/// `PUT /classes/{class_id}/hidden` — hide or un-hide a class (hide/archive classes), RD-gated.
///
/// [`ControlAuth`] runs first. The body is `{ hidden: bool }`. Hiding is a **visibility
/// preference**, not an edit, so it is valid for **built-in** classes too (never a read-only
/// rejection): the class stays in the directory and the main Classes view; it is just filtered out
/// of the per-event class picker. The choice is persisted to a sidecar so a hidden built-in survives
/// the boot re-seed. An unknown id is a typed 404 (`UnknownScope`). On success the updated [`Class`]
/// (with its fresh `hidden` flag) is returned.
async fn set_class_hidden(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(class_id): Path<ClassId>,
    Json(body): Json<SetClassHiddenRequest>,
) -> Result<Json<Class>, ProtocolError> {
    let class = registry
        .classes()
        .set_hidden(&class_id, body.hidden)
        .map_err(class_error_to_protocol)?;
    Ok(Json(class))
}

/// Map a [`ClassError`] to a typed [`ProtocolError`] (issue #84): a validation failure is a
/// `BadRequest` (400), an unknown id is an `UnknownScope` (404), and a persistence failure is
/// `Internal` (500) — mirroring [`pilot_error_to_protocol`].
fn class_error_to_protocol(error: ClassError) -> ProtocolError {
    let code = match error.kind {
        ClassErrorKind::Invalid => ErrorCode::BadRequest,
        // A read-only built-in edit/delete is a rejected bad request (the built-in is canonical).
        ClassErrorKind::ReadOnly => ErrorCode::BadRequest,
        ClassErrorKind::NotFound => ErrorCode::UnknownScope,
        ClassErrorKind::Internal => ErrorCode::Internal,
    };
    ProtocolError::new(code, error.to_string())
}

/// `PUT /events/{event_id}/classes` — set an event's **class selection** (issue #84), RD-gated.
///
/// [`ControlAuth`] runs first. The event must exist (else a typed 404) and **each** id in the body
/// must name a known directory class (else a 404 naming the bad id) — so a selection can never
/// reference a deleted/unknown class. On success the updated [`EventMeta`] is returned. Mirrors
/// `set_event_timers` (a wholesale set with per-id validation).
async fn set_event_classes(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
    Json(body): Json<SetEventClassesRequest>,
) -> Result<Json<EventMeta>, ProtocolError> {
    let classes = registry.classes();
    for id in &body.ids {
        if !classes.exists(id) {
            return Err(ProtocolError::new(
                ErrorCode::UnknownScope,
                format!("no class with id {:?}", id.0),
            ));
        }
    }
    let meta = registry
        .set_classes(&event_id, body.ids)
        .map_err(|e| ProtocolError::new(ErrorCode::UnknownScope, e.to_string()))?;
    Ok(Json(meta))
}

/// `PUT /events/{event_id}/classes/{class_id}/membership` — set which roster pilots race one
/// class (race redesign Slice 1a), RD-gated.
///
/// [`ControlAuth`] runs first. The class must name a known directory class and **each** pilot id in
/// the body must name a known directory pilot (else a typed 404 naming the bad id) — so a class's
/// membership can never reference a deleted/unknown class or pilot. The event must exist (else a
/// 404). On success the updated [`EventMeta`] is returned. Mirrors `set_event_classes` /
/// `set_event_roster` (per-id validation against the relevant directory).
async fn set_class_membership(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path((event_id, class_id)): Path<(EventId, ClassId)>,
    Json(body): Json<SetClassMembershipRequest>,
) -> Result<Json<EventMeta>, ProtocolError> {
    if !registry.classes().exists(&class_id) {
        return Err(ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no class with id {:?}", class_id.0),
        ));
    }
    let pilots = registry.pilots();
    for slot in &body.pilots {
        if !pilots.exists(&slot.pilot) {
            return Err(ProtocolError::new(
                ErrorCode::UnknownScope,
                format!("no pilot with id {:?}", slot.pilot.0),
            ));
        }
    }
    // Validate each assigned channel (race redesign Slice 7a) against the event's **primary**
    // timer's available-channels pool — the GQ-style fixed channel must be one the timer offers.
    // The pool may exceed the timer's `node_count` (node_count caps only pilots-per-heat), so any
    // channel in the pool is valid; we never cap the number of distinct channels at node_count.
    let assigned: Vec<u16> = body.pilots.iter().filter_map(|s| s.channel).collect();
    if !assigned.is_empty() {
        let meta = registry.meta_of(&event_id).ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::UnknownScope,
                format!("no event with id {:?}", event_id.0),
            )
        })?;
        let timer = meta
            .effective_primary()
            .and_then(|id| registry.timers().get(&id));
        let Some(timer) = timer else {
            return Err(ProtocolError::new(
                ErrorCode::BadRequest,
                "cannot assign per-pilot channels: the event has no resolvable primary timer"
                    .to_string(),
            ));
        };
        for channel in &assigned {
            if !timer.available_channels.contains(channel) {
                return Err(ProtocolError::new(
                    ErrorCode::BadRequest,
                    format!(
                        "channel {channel} is not in the primary timer {:?}'s available channels",
                        timer.id.0
                    ),
                ));
            }
        }
    }
    let meta = registry
        .set_class_membership(&event_id, class_id, body.pilots)
        .map_err(|e| ProtocolError::new(ErrorCode::UnknownScope, e.to_string()))?;
    Ok(Json(meta))
}

/// Map a [`RoundError`] to a [`ProtocolError`]: a missing event/round is a typed **404**
/// ([`ErrorCode::UnknownScope`]); an invalid round definition (bad class, unknown format, dangling
/// seeding source) is a **400** ([`ErrorCode::BadRequest`]).
fn round_error(e: RoundError) -> ProtocolError {
    let code = match e {
        RoundError::EventNotFound(_) | RoundError::RoundNotFound(_) => ErrorCode::UnknownScope,
        RoundError::Invalid(_) => ErrorCode::BadRequest,
    };
    ProtocolError::new(code, e.to_string())
}

/// `POST /events/{event_id}/rounds` — add a **round** to an event (race redesign Slice 2a),
/// RD-gated.
///
/// [`ControlAuth`] runs first. The round id is **auto-generated** server-side (never in the body).
/// Each class in the body must be selected by the event, the `format` must be a known
/// [`FormatRegistry`](gridfpv_engine::format::FormatRegistry) name, and a `FromRanking` seeding
/// source must name an existing round — else a typed **400**. An unknown event is a **404**. On
/// success the created [`RoundDef`] (with its generated id) is returned and the event's meta is
/// written through to disk (issue #115).
async fn add_round(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
    Json(body): Json<NewRoundReq>,
) -> Result<Json<RoundDef>, ProtocolError> {
    let round = registry.add_round(&event_id, body).map_err(round_error)?;
    // Open-practice refinement: an open-practice round has no class/pilots, so the normal manual
    // `FillRound` flow can't run — but its **channels are the lineup**, so the single open heat can
    // be built immediately. Auto-run the equivalent of `FillRound` here so the RD can Stage/Start the
    // practice with no manual fill. Idempotent for free: `fill_round` dedups against already-scheduled
    // heats, so re-creating/editing never double-schedules — one open heat per round. A non-open
    // round is left to the RD's manual `FillRound` as before.
    if crate::round_engine::is_open_practice(&round) {
        let state = resolve_event(&registry, &event_id)?;
        let ack = crate::control_handler::apply_fill_round(
            &registry,
            &event_id,
            &state,
            round.id.clone(),
        );
        if let Some(err) = ack.error {
            return Err(err);
        }
    }
    Ok(Json(round))
}

/// `PUT /events/{event_id}/rounds/{round_id}` — replace an existing **round**'s fields (race
/// redesign Slice 2a), RD-gated.
///
/// [`ControlAuth`] runs first. The round id is the path segment (not editable); every other field is
/// replaced wholesale. Same validation as `add_round` (bad class / format / seeding → **400**); an
/// unknown event or round id is a **404**. On success the updated [`RoundDef`] is returned and the
/// meta is written through to disk.
async fn update_round(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path((event_id, round_id)): Path<(EventId, RoundId)>,
    Json(body): Json<UpdateRoundReq>,
) -> Result<Json<RoundDef>, ProtocolError> {
    let round = registry
        .update_round(&event_id, &round_id, body)
        .map_err(round_error)?;
    Ok(Json(round))
}

/// `DELETE /events/{event_id}/rounds/{round_id}` — remove a **round** from an event (race redesign
/// Slice 2a), RD-gated.
///
/// [`ControlAuth`] runs first. An unknown event or round id is a typed **404**. On success the
/// event's updated [`EventMeta`] is returned and the meta is written through to disk.
async fn remove_round(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path((event_id, round_id)): Path<(EventId, RoundId)>,
) -> Result<Json<EventMeta>, ProtocolError> {
    let meta = registry
        .remove_round(&event_id, &round_id)
        .map_err(round_error)?;
    Ok(Json(meta))
}

/// `GET /events/{event_id}/heats` — the event's **scheduled heats** (race redesign Slice 3b).
///
/// A read (open, no token, like the snapshot routes): folds the event's log into one
/// [`HeatSummary`] per scheduled heat — id, lineup, the round/class it was tagged with, its
/// derived phase, and whether it is the current heat — in first-scheduled order. The Heats UI
/// groups this by round to render each round's heats list. An unknown event is a typed **404**.
async fn list_heats(
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
) -> Result<Json<Vec<HeatSummary>>, ProtocolError> {
    let state = resolve_event(&registry, &event_id)?;
    let (events, _cursor) = state.read()?;
    Ok(Json(heat_summaries(&events)))
}

/// Map a [`round_engine::FillError`] to a typed [`ProtocolError`]: an unknown round (or seeding
/// source round) is a **404** ([`ErrorCode::UnknownScope`]); an unscorable round (empty field,
/// unknown format) is a **400** ([`ErrorCode::BadRequest`]) — mirroring the control handler's
/// `FillRound` mapping so the read routes answer the same shape.
fn fill_error(err: round_engine::FillError) -> ProtocolError {
    use round_engine::FillError;
    let code = match err {
        FillError::UnknownRound(_) | FillError::UnknownSourceRound(_) => ErrorCode::UnknownScope,
        FillError::EmptyField(_) | FillError::UnknownFormat(_) | FillError::MissingChannel(_) => {
            ErrorCode::BadRequest
        }
    };
    ProtocolError::new(code, err.to_string())
}

/// `GET /events/{event_id}/rounds/{round_id}/ranking` — a round's **ranking** (race redesign
/// Slice 5/6a).
///
/// The ordered per-pilot ranking the engine seeds `FromRanking` from ([`round_engine::round_ranking`])
/// — the same provisional-or-final ordering a bracket carries — exposed as a read for the
/// bracket-carry display. A read (open, no token, like the snapshot routes). An unknown event or
/// round is a typed **404**; a round that cannot be ranked (unknown format, dangling seeding
/// source) is a **400**.
async fn round_ranking(
    State(registry): State<EventRegistry>,
    Path((event_id, round_id)): Path<(EventId, RoundId)>,
) -> Result<Json<Vec<gridfpv_engine::format::RankEntry>>, ProtocolError> {
    let state = resolve_event(&registry, &event_id)?;
    let meta = registry.meta_of(&event_id).ok_or_else(|| {
        ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no event with id {:?}", event_id.0),
        )
    })?;
    let round = meta
        .rounds
        .iter()
        .find(|r| r.id == round_id)
        .ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::UnknownScope,
                format!("no round with id {:?} in this event", round_id.0),
            )
        })?;
    let (events, _cursor) = state.read()?;
    let ranking = round_engine::round_ranking(&meta, round, &events).map_err(fill_error)?;
    Ok(Json(ranking))
}

/// `GET /events/{event_id}/classes/{class_id}/standings` — a class's **standings** (race redesign
/// Slice 5/6a).
///
/// The season-join projection the Results UI reads: [`round_engine::class_standings`] folds the
/// event log + meta into one per-pilot row per competitor that raced the class, aggregated across
/// the class's rounds (points, best lap, total laps), best standing first. A read (open, no token).
/// An unknown event is a typed **404**; an unscorable class round is a **400**. A class with no
/// rounds yields empty standings (a 200), not an error.
async fn class_standings(
    State(registry): State<EventRegistry>,
    Path((event_id, class_id)): Path<(EventId, ClassId)>,
) -> Result<Json<round_engine::ClassStandings>, ProtocolError> {
    let state = resolve_event(&registry, &event_id)?;
    let meta = registry.meta_of(&event_id).ok_or_else(|| {
        ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no event with id {:?}", event_id.0),
        )
    })?;
    let (events, _cursor) = state.read()?;
    let standings = round_engine::class_standings(&meta, &class_id, &events).map_err(fill_error)?;
    Ok(Json(standings))
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
    const API_PREFIXES: [&str; 10] = [
        "/health",
        "/events",
        "/active-event",
        "/timers",
        "/pilots",
        "/classes",
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
    /// The heat's marshaling [`AuditEntry`](gridfpv_projection::AuditEntry) trail (#55).
    Audit,
    /// The heat's scored [`HeatResult`].
    Result,
    /// The heat's captured RSSI signal trace
    /// ([`SignalTraceView`](gridfpv_projection::SignalTraceView), marshaling Slice 1).
    Signal,
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
    let (stored, cursor) = state.read_stored()?;
    let events: Vec<Event> = stored.iter().map(|s| s.event.clone()).collect();
    // Open-practice overlay (open-practice format, Slice 1): the live state's phase/clock are always
    // the **real log's** (folded here as for any heat); while an open-practice heat is active its
    // per-channel laps are in memory (NOT logged), so the accumulator splices those laps onto the log
    // base — "snapshot first, then subscribe" stays correct (the `/stream` fold applies the same
    // merge), so a client renders the live per-channel laps immediately atop a truthful phase/clock.
    // `with_heat_timing` folds the current heat's server-authoritative race-start/end instants
    // (#62 follow-up) from the stored log's `recorded_at` so the clock is consistent everywhere.
    let body = state
        .open_practice()
        .merge_into(with_heat_timing(live_state(&events), &stored));
    Ok(Json(Snapshot {
        cursor,
        body: ProjectionBody::LiveRaceState(body),
    }))
}

/// `GET /snapshot/class/{event}/{class}` — a class's live race-state (§4 class scope).
///
/// Now that [`Event::HeatScheduled`] carries the `class` it runs in (race redesign Slice 5/6a),
/// the class scope is a **real filter**: [`class_window`] scopes the log to the heats tagged with
/// `class` (their scheduling / state-change events plus the passes that fall while one of them is
/// the active heat), and the live state is folded over that filtered view — so the body reflects
/// only this class's racing, not the whole event. A class with no tagged heats folds an idle state.
async fn snapshot_class(
    State(registry): State<EventRegistry>,
    Path((event_id, _event, class)): Path<(EventId, EventId, ClassId)>,
) -> Result<Json<Snapshot>, ProtocolError> {
    let state = resolve_event(&registry, &event_id)?;
    let (stored, cursor) = state.read_stored()?;
    let events: Vec<Event> = stored.iter().map(|s| s.event.clone()).collect();
    let class_events = class_window(&events, &class);
    // The window's `current_heat` resolves which heat is on the timer; its timing is folded
    // from the *full* stored log (the heat's transition instants live there with `recorded_at`).
    Ok(Json(Snapshot {
        cursor,
        body: ProjectionBody::LiveRaceState(with_heat_timing(live_state(&class_events), &stored)),
    }))
}

/// Filter the log to a single **class's** heats (race redesign Slice 5/6a): every event that
/// belongs to a heat tagged `HeatScheduled { class: Some(class), .. }`, plus the passes and
/// marshaling adjudications that fall *while one of the class's heats is the active one*.
///
/// The class's heats are first collected from the `HeatScheduled` tags; then the log is replayed
/// once, opening the window on any heat-loop event for one of those heats and closing it on a
/// heat-loop event for a heat *not* in the class — the same position-based pass attribution
/// [`heat_window`] uses to scope a single heat, generalized to a set of heats. So a class's live
/// state folds only its own heats and passes, with no other class's racing bleeding in.
pub(crate) fn class_window(events: &[Event], class: &ClassId) -> Vec<Event> {
    // The heat ids tagged with this class (a `HeatScheduled` whose `class` equals `class`).
    let class_heats: std::collections::HashSet<&HeatId> = events
        .iter()
        .filter_map(|e| match e {
            Event::HeatScheduled {
                heat,
                class: Some(c),
                ..
            } if c == class => Some(heat),
            _ => None,
        })
        .collect();

    let mut window = Vec::new();
    // `active` tracks whether the cursor is currently inside one of the class's heats: it opens on
    // a heat-loop event for a class heat and closes on a heat-loop event for any non-class heat.
    let mut active = false;
    for event in events {
        match event {
            Event::HeatScheduled { heat, .. } | Event::HeatStateChanged { heat, .. } => {
                active = class_heats.contains(heat);
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

/// `GET /snapshot/heat/{heat}` — the tightest scope (§4 heat scope).
///
/// `?projection=live` (default) returns the heat's [`LiveRaceState`]; `?projection=laps`
/// its [`LapList`]; `?projection=audit` its marshaling audit trail
/// ([`AuditEntry`](gridfpv_projection::AuditEntry) list, #55); `?projection=result` its scored
/// [`HeatResult`]; `?projection=signal` its captured RSSI signal trace
/// ([`SignalTraceView`](gridfpv_projection::SignalTraceView), marshaling Slice 1). The log is
/// filtered to the heat's window so the body is heat-local.
async fn snapshot_heat(
    State(registry): State<EventRegistry>,
    Path((event_id, heat)): Path<(EventId, HeatId)>,
    Query(query): Query<HeatQuery>,
) -> Result<Json<Snapshot>, ProtocolError> {
    let state = resolve_event(&registry, &event_id)?;
    let (stored, cursor) = state.read_stored()?;
    let events: Vec<Event> = stored.iter().map(|s| s.event.clone()).collect();

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

    // The heat's window, carrying each event's GLOBAL append offset — load-bearing for the
    // marshaling lap/audit folds (their `LogRef`s must be global, not window-relative, #55).
    let heat_offsets = heat_window_offsets(&events, &heat);
    let heat_events: Vec<Event> = heat_offsets.iter().map(|(_, e)| e.clone()).collect();

    let body = match query.projection {
        HeatProjection::Live => {
            // Open-practice overlay (open-practice format, Slice 1): fold the heat's real log window
            // for a truthful phase/clock, then — when this *is* the active open-practice heat — splice
            // its in-memory (NOT logged) per-channel laps on top. `merge_into` guards on the heat
            // matching the accumulator's, so a non-op heat folds its log window unchanged.
            ProjectionBody::LiveRaceState(
                state
                    .open_practice()
                    .merge_into(with_heat_timing(live_state(&heat_events), &stored)),
            )
        }
        HeatProjection::Laps => ProjectionBody::LapList(lap_list_marshaled(
            heat_offsets.iter().map(|(o, e)| (*o, e)),
        )),
        HeatProjection::Audit => {
            // The defensible-results audit panel: fold the heat's rulings into a reverse-chrono
            // trail, keyed on global offsets with each fact's `recorded_at` as "when" (#55).
            ProjectionBody::MarshalingAudit(marshaling_log(
                heat_offsets
                    .iter()
                    .map(|(o, e)| (stored.get(*o as usize).and_then(|s| s.recorded_at), *o, e)),
                &heat,
            ))
        }
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
            // Score with the window's PRESERVED global offsets so a `RulingReversed` (a UI
            // reverse-DQ) un-applies the penalty at its true global `LogRef` target (#55) — a
            // re-enumerated window would match the wrong offset.
            ProjectionBody::HeatResult(score_with_global_offsets(
                heat_offsets.iter().map(|(o, e)| (*o, e)),
                WinCondition::BestLap,
                race_start,
            ))
        }
        HeatProjection::Signal => {
            // The signal-as-evidence trace (marshaling Slice 1): fold the heat window's
            // SignalChunk/SignalThresholds facts into a per-competitor trace. Pure and clock-free.
            ProjectionBody::SignalTrace(signal_trace(&heat_events))
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

/// Current server wall-clock time in **microseconds** since the Unix epoch — the basis the
/// race clock is anchored to. Used to stamp a heat transition's `recorded_at` at append time
/// (the `Running` / `Unofficial` instants the live `race_started_at` / `race_ended_at` fold
/// from), so header and HUD count from the one authoritative race-go.
fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// The `at` of an event if it is a lap-gate pass, for deriving a heat's race start.
pub(crate) fn first_pass_at(event: &Event) -> Option<SourceTime> {
    match event {
        Event::Pass(p) if p.gate.is_lap_gate() => Some(p.at),
        _ => None,
    }
}

/// Filter the log to a single heat's window, **preserving each event's global append offset**:
/// that heat's scheduling / state-change events, plus all passes and marshaling adjudications
/// that fall *while the heat is the active one*, each paired with its position in the full log.
///
/// With one heat per log in the common case this is the whole log; with several heats it
/// scopes passes to the span between this heat's first scheduling/transition and the next
/// heat's. Passes carry no heat id (they are raw observations), so attribution is by
/// position in the log relative to heat-loop events — the same ordering the engine uses to
/// decide which heat consumes a pass (race-engine.html §2).
///
/// The retained **global offset** is load-bearing for marshaling (#55): the lap projection and
/// audit fold are keyed on it, and a `LogRef` correction command targets that global offset. An
/// earlier bug re-enumerated the window `0,1,2,…`, so a UI-selected lap in a later heat targeted
/// the *wrong* pass; folding with the real offsets fixes that.
pub(crate) fn heat_window_offsets(events: &[Event], heat: &HeatId) -> Vec<(u64, Event)> {
    let mut window = Vec::new();
    // `active` tracks whether the cursor is currently inside this heat's span: it opens on
    // a heat-loop event for `heat` and closes on a heat-loop event for a *different* heat.
    let mut active = false;
    for (offset, event) in events.iter().enumerate() {
        match event {
            Event::HeatScheduled { heat: h, .. } | Event::HeatStateChanged { heat: h, .. } => {
                active = h == heat;
                if active {
                    window.push((offset as u64, event.clone()));
                }
            }
            // Passes and adjudications belong to whichever heat is currently active.
            _ if active => window.push((offset as u64, event.clone())),
            _ => {}
        }
    }
    window
}

/// The heat window as a bare `Vec<Event>` — the offset-agnostic view used where global offsets
/// are not needed (live state, results scoring). The marshaling lap/audit folds use
/// [`heat_window_offsets`] instead, so they target the correct global `LogRef`.
pub(crate) fn heat_window(events: &[Event], heat: &HeatId) -> Vec<Event> {
    heat_window_offsets(events, heat)
        .into_iter()
        .map(|(_, e)| e)
        .collect()
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
    use gridfpv_events::{AdapterId, GateIndex, HeatTransition, LogRef, Pass};
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

    /// A recorded heat log: q-1 scheduled, run through to Final, with laps for A and B.
    fn recorded_heat() -> Vec<Event> {
        vec![
            Event::HeatScheduled {
                heat: HeatId("q-1".into()),
                lineup: vec![CompetitorRef("A".into()), CompetitorRef("B".into())],
                class: None,
                round: None,
                frequencies: vec![],
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
                transition: HeatTransition::Finalized,
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
                assert_eq!(ls.phase, HeatPhase::Final);
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
    async fn heat_scope_audit_projection_returns_marshaling_trail() {
        // Seed a heat plus two rulings: void B's first pass, then DQ A. The audit returns both,
        // newest first, with NO automatic passes.
        let mut events = recorded_heat();
        events.push(Event::DetectionVoided {
            target: LogRef(5), // global offset of B's first pass in `recorded_heat`
        });
        events.push(Event::PenaltyApplied {
            heat: HeatId("q-1".into()),
            competitor: CompetitorRef("A".into()),
            penalty: gridfpv_events::Penalty::Disqualify { reason: None },
        });
        let (registry, _state, _) = state_with(events);
        let (status, snap) = get_snapshot(
            registry,
            "/events/practice/snapshot/heat/q-1?projection=audit",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        match snap.unwrap().body {
            ProjectionBody::MarshalingAudit(trail) => {
                assert_eq!(trail.len(), 2, "two rulings, no passes");
                // Newest first: the DQ.
                assert_eq!(trail[0].kind, gridfpv_projection::AuditKind::PenaltyApplied);
                assert_eq!(trail[1].kind, gridfpv_projection::AuditKind::Voided);
            }
            other => panic!("expected marshaling audit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn heat_scope_signal_projection_returns_captured_trace() {
        // Seed a heat with two signal chunks for node A plus thresholds; the signal projection
        // folds them into a per-competitor trace scoped to the heat window.
        let mut events = recorded_heat();
        events.push(Event::SignalThresholds(gridfpv_events::SignalThresholds {
            adapter: AdapterId("rotorhazard".into()),
            competitor: CompetitorRef("A".into()),
            enter: 90,
            exit: 80,
        }));
        events.push(Event::SignalChunk(gridfpv_events::SignalChunk {
            adapter: AdapterId("rotorhazard".into()),
            competitor: CompetitorRef("A".into()),
            from: SourceTime::from_micros(0),
            period_micros: 100_000,
            rssi: vec![70, 150],
        }));
        events.push(Event::SignalChunk(gridfpv_events::SignalChunk {
            adapter: AdapterId("rotorhazard".into()),
            competitor: CompetitorRef("A".into()),
            from: SourceTime::from_micros(200_000),
            period_micros: 100_000,
            rssi: vec![148, 70],
        }));
        let (registry, _state, _) = state_with(events);
        let (status, snap) = get_snapshot(
            registry,
            "/events/practice/snapshot/heat/q-1?projection=signal",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        match snap.unwrap().body {
            ProjectionBody::SignalTrace(view) => {
                assert_eq!(view.competitors.len(), 1);
                let trace = &view.competitors[0];
                assert_eq!(trace.competitor.competitor, CompetitorRef("A".into()));
                assert_eq!(trace.samples, vec![70, 150, 148, 70]);
                assert_eq!(trace.enter, Some(90));
                assert_eq!(trace.exit, Some(80));
            }
            other => panic!("expected signal trace, got {other:?}"),
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
    async fn class_scope_filters_to_the_class_heats() {
        // Two heats in different classes; the class scope folds only its own class's heat.
        // `open`'s heat ran A; `sport`'s heat ran B. The open class scope sees only A's racing.
        let events = vec![
            Event::HeatScheduled {
                heat: HeatId("o-1".into()),
                lineup: vec![CompetitorRef("A".into())],
                class: Some(ClassId("open".into())),
                round: Some(RoundId("q1".into())),
                frequencies: vec![],
            },
            Event::HeatStateChanged {
                heat: HeatId("o-1".into()),
                transition: HeatTransition::Running,
            },
            pass("A", 1_000_000, 1),
            pass("A", 4_000_000, 2), // open/A: one lap
            Event::HeatScheduled {
                heat: HeatId("s-1".into()),
                lineup: vec![CompetitorRef("B".into())],
                class: Some(ClassId("sport".into())),
                round: Some(RoundId("q2".into())),
                frequencies: vec![],
            },
            Event::HeatStateChanged {
                heat: HeatId("s-1".into()),
                transition: HeatTransition::Running,
            },
            pass("B", 10_000_000, 1),
            pass("B", 13_000_000, 2),
            pass("B", 15_000_000, 3), // sport/B: two laps
        ];
        let (registry, _state, _) = state_with(events);

        // The open class scope: current heat is open's, B (sport) never appears.
        let (status, snap) = get_snapshot(
            registry.clone(),
            "/events/practice/snapshot/class/spring-cup/open",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        match snap.unwrap().body {
            ProjectionBody::LiveRaceState(ls) => {
                assert_eq!(ls.current_heat, Some(HeatId("o-1".into())));
                assert_eq!(ls.active_pilots, vec![CompetitorRef("A".into())]);
                assert!(
                    !ls.running_order.contains(&CompetitorRef("B".into())),
                    "sport's pilot does not appear in the open class scope"
                );
            }
            other => panic!("expected live state, got {other:?}"),
        }

        // And the sport scope sees only B.
        let (_, snap) =
            get_snapshot(registry, "/events/practice/snapshot/class/spring-cup/sport").await;
        match snap.unwrap().body {
            ProjectionBody::LiveRaceState(ls) => {
                assert_eq!(ls.current_heat, Some(HeatId("s-1".into())));
                assert_eq!(ls.active_pilots, vec![CompetitorRef("B".into())]);
            }
            other => panic!("expected live state, got {other:?}"),
        }
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
                class: None,
                round: None,
                frequencies: vec![],
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
                class: None,
                round: None,
                frequencies: vec![],
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

    // --- #90: the Director's active event over HTTP -------------------------------------

    /// `GET /active-event` → status + parsed `ActiveEvent`.
    async fn get_active(registry: EventRegistry) -> (StatusCode, Option<ActiveEvent>) {
        let response = router(registry)
            .oneshot(
                Request::builder()
                    .uri("/active-event")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = serde_json::from_slice::<ActiveEvent>(&bytes).ok();
        (status, body)
    }

    /// `PUT /active-event` with `{ id }` and an optional bearer token → status + parsed body.
    async fn put_active(
        registry: EventRegistry,
        id: &str,
        token: Option<&str>,
    ) -> (StatusCode, Vec<u8>) {
        let mut builder = Request::builder()
            .method("PUT")
            .uri("/active-event")
            .header("Content-Type", "application/json");
        if let Some(token) = token {
            builder = builder.header("Authorization", format!("Bearer {token}"));
        }
        let json = serde_json::to_string(&SetActiveEventRequest {
            id: EventId(id.to_string()),
        })
        .unwrap();
        let response = router(registry)
            .oneshot(builder.body(Body::from(json)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, bytes.to_vec())
    }

    #[tokio::test]
    async fn active_event_is_none_until_set_then_resumes() {
        let (registry, _state, _) = state_with(recorded_heat());

        // A fresh Director has no active event → the picker.
        let (status, body) = get_active(registry.clone()).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.expect("an ActiveEvent body").event.is_none());

        // Setting it (open Director — no token needed) returns Practice's meta…
        let (status, raw) = put_active(registry.clone(), PRACTICE_EVENT_ID, None).await;
        assert_eq!(status, StatusCode::OK);
        let meta: EventMeta = serde_json::from_slice(&raw).unwrap();
        assert_eq!(meta.id.0, PRACTICE_EVENT_ID);

        // …and now the open read resumes into it.
        let (_, body) = get_active(registry).await;
        assert_eq!(
            body.unwrap().event.map(|m| m.id.0),
            Some(PRACTICE_EVENT_ID.to_string())
        );
    }

    #[tokio::test]
    async fn setting_an_unknown_active_event_is_404() {
        let (registry, _state, _) = state_with(recorded_heat());
        let (status, raw) = put_active(registry, "no-such-event", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let err: ProtocolError = serde_json::from_slice(&raw).unwrap();
        assert_eq!(err.code, ErrorCode::UnknownScope);
    }

    #[tokio::test]
    async fn setting_the_active_event_requires_an_rd_token_once_configured() {
        let (registry, state, _) = state_with(recorded_heat());
        // Configure a control credential so the full-trust default closes.
        let _rd = state.tokens().issue_rd_token();
        let (status, _) = put_active(registry, PRACTICE_EVENT_ID, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // --- DELETE /events/{id}: permanent delete of an event + all its data (papercut) ----

    /// `DELETE /events/{id}` with an optional bearer token → status + raw body bytes.
    async fn delete_event_req(
        registry: EventRegistry,
        id: &str,
        token: Option<&str>,
    ) -> (StatusCode, Vec<u8>) {
        let mut builder = Request::builder()
            .method("DELETE")
            .uri(format!("/events/{id}"));
        if let Some(token) = token {
            builder = builder.header("Authorization", format!("Bearer {token}"));
        }
        let response = router(registry)
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, bytes.to_vec())
    }

    #[tokio::test]
    async fn delete_event_removes_a_created_event() {
        let (registry, _state, _) = state_with(recorded_heat());
        // Create a real event, then delete it through the route.
        let created = registry
            .create(&crate::events::CreateEventRequest {
                name: "Doomed".into(),
                date: None,
                location: None,
                description: None,
                organizer: None,
            })
            .unwrap();
        assert!(registry.resolve(&created.id).is_some());

        let (status, _) = delete_event_req(registry.clone(), &created.id.0, None).await;
        assert_eq!(status, StatusCode::OK);
        // Gone from the registry and the listing.
        assert!(registry.resolve(&created.id).is_none());
        assert!(!registry.list().iter().any(|m| m.id == created.id));
    }

    #[tokio::test]
    async fn delete_event_rejects_practice_and_unknown() {
        let (registry, _state, _) = state_with(recorded_heat());
        // Practice cannot be deleted → BadRequest (400), and it still resolves.
        let (status, raw) = delete_event_req(registry.clone(), PRACTICE_EVENT_ID, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err: ProtocolError = serde_json::from_slice(&raw).unwrap();
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert!(
            registry
                .resolve(&EventId(PRACTICE_EVENT_ID.into()))
                .is_some()
        );

        // An unknown id → a typed 404 (UnknownScope).
        let (status, raw) = delete_event_req(registry, "no-such-event", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let err: ProtocolError = serde_json::from_slice(&raw).unwrap();
        assert_eq!(err.code, ErrorCode::UnknownScope);
    }

    #[tokio::test]
    async fn delete_event_requires_an_rd_token_once_configured() {
        let (registry, state, _) = state_with(recorded_heat());
        let _rd = state.tokens().issue_rd_token();
        let created = registry
            .create(&crate::events::CreateEventRequest {
                name: "Gated".into(),
                date: None,
                location: None,
                description: None,
                organizer: None,
            })
            .unwrap();
        let (status, _) = delete_event_req(registry.clone(), &created.id.0, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // The event is untouched after the rejected delete.
        assert!(registry.resolve(&created.id).is_some());
    }

    // --- POST /events/{id}/control: a missing/wrong Content-Type → JSON ProtocolError ----

    #[tokio::test]
    async fn control_missing_content_type_is_a_json_protocol_error() {
        // A control POST whose body is valid JSON but lacks the `Content-Type: application/json`
        // header used to return a bare-text 4xx; it must now be the uniform `ProtocolError` JSON.
        let (registry, _state, _) = state_with(recorded_heat());
        let command = serde_json::to_string(&crate::control::Command::Stage {
            heat: HeatId("q-1".into()),
        })
        .unwrap();
        let response = router(registry)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/events/practice/control")
                    // No Content-Type header on purpose.
                    .body(Body::from(command))
                    .unwrap(),
            )
            .await
            .unwrap();
        // A 400 with a parseable ProtocolError(BadRequest) body — not a bare-text 4xx.
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let err: ProtocolError =
            serde_json::from_slice(&bytes).expect("the body is a JSON ProtocolError");
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert!(!err.message.is_empty());
    }

    #[tokio::test]
    async fn control_malformed_json_body_is_a_json_protocol_error() {
        // A correct Content-Type but an unparseable body is likewise a typed JSON error.
        let (registry, _state, _) = state_with(recorded_heat());
        let response = router(registry)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/events/practice/control")
                    .header("Content-Type", "application/json")
                    .body(Body::from("{ not valid json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let err: ProtocolError =
            serde_json::from_slice(&bytes).expect("the body is a JSON ProtocolError");
        assert_eq!(err.code, ErrorCode::BadRequest);
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

    // --- #73: the application-level timer registry + per-event selection ----------------

    use crate::timers::{
        CreateTimerRequest, SetEventTimersRequest, Timer, TimerKind, UpdateTimerRequest,
    };

    /// `GET /timers` → status + parsed `Timer[]`.
    async fn get_timers(registry: EventRegistry) -> (StatusCode, Vec<Timer>) {
        let response = router(registry)
            .oneshot(
                Request::builder()
                    .uri("/timers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = serde_json::from_slice::<Vec<Timer>>(&bytes).unwrap_or_default();
        (status, body)
    }

    /// `POST /timers` with a JSON body + optional token → status + raw bytes.
    async fn post_timer(
        registry: EventRegistry,
        body: &CreateTimerRequest,
        token: Option<&str>,
    ) -> (StatusCode, Vec<u8>) {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/timers")
            .header("Content-Type", "application/json");
        if let Some(t) = token {
            builder = builder.header("Authorization", format!("Bearer {t}"));
        }
        let json = serde_json::to_string(body).unwrap();
        let response = router(registry)
            .oneshot(builder.body(Body::from(json)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, bytes.to_vec())
    }

    #[tokio::test]
    async fn timers_list_has_the_mock_first_and_is_open() {
        let (registry, _state, _) = state_with(vec![]);
        let (status, timers) = get_timers(registry).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(timers.first().unwrap().id.0, "mock");
        assert!(matches!(timers[0].kind, TimerKind::Mock { .. }));
    }

    #[tokio::test]
    async fn post_timer_creates_and_lists_it() {
        let (registry, _state, _) = state_with(vec![]);
        let body = CreateTimerRequest {
            name: "Field RH".into(),
            kind: TimerKind::Rotorhazard {
                url: "http://rh.local:5000".into(),
            },
            channel_capability: None,
            node_count: None,
            available_channels: None,
        };
        let (status, raw) = post_timer(registry.clone(), &body, None).await;
        assert_eq!(status, StatusCode::OK);
        let created: Timer = serde_json::from_slice(&raw).unwrap();
        assert!(created.id.0.starts_with("field-rh-"));

        let (_, timers) = get_timers(registry).await;
        assert!(timers.iter().any(|t| t.id == created.id));
    }

    #[tokio::test]
    async fn post_timer_requires_an_rd_token_once_configured() {
        let (registry, state, _) = state_with(vec![]);
        let _rd = state.tokens().issue_rd_token();
        let body = CreateTimerRequest {
            name: "Gated".into(),
            kind: TimerKind::Mock {
                laps: 1,
                lap_ms: 50,
            },
            channel_capability: None,
            node_count: None,
            available_channels: None,
        };
        let (status, _) = post_timer(registry, &body, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn delete_rejects_the_builtin_mock() {
        let (registry, _state, _) = state_with(vec![]);
        let response = router(registry)
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/timers/mock")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // Protected delete is a client error, not a 404.
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn put_event_timers_validates_and_sets_the_selection() {
        let (registry, _state, _) = state_with(vec![]);
        // Create a real timer to select.
        let body = CreateTimerRequest {
            name: "Extra Sim".into(),
            kind: TimerKind::Mock {
                laps: 1,
                lap_ms: 50,
            },
            channel_capability: None,
            node_count: None,
            available_channels: None,
        };
        let (_, raw) = post_timer(registry.clone(), &body, None).await;
        let extra: Timer = serde_json::from_slice(&raw).unwrap();

        // Selecting a known timer succeeds and is reflected on the event meta.
        let req = SetEventTimersRequest {
            ids: vec![extra.id.clone()],
            primary: None,
        };
        let response = router(registry.clone())
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/events/practice/timers")
                    .header("Content-Type", "application/json")
                    .body(Body::from(serde_json::to_string(&req).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let meta: EventMeta = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(meta.timers, vec![extra.id]);

        // Selecting an UNKNOWN timer → 404 UnknownScope.
        let bad = SetEventTimersRequest {
            ids: vec![crate::timers::TimerId("no-such-timer".into())],
            primary: None,
        };
        let response = router(registry)
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/events/practice/timers")
                    .header("Content-Type", "application/json")
                    .body(Body::from(serde_json::to_string(&bad).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn update_timer_retunes_the_mock() {
        let (registry, _state, _) = state_with(vec![]);
        let body = UpdateTimerRequest {
            name: None,
            kind: Some(TimerKind::Mock {
                laps: 9,
                lap_ms: 100,
            }),
            ..Default::default()
        };
        let response = router(registry.clone())
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/timers/mock")
                    .header("Content-Type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let (_, timers) = get_timers(registry).await;
        let sim = timers.iter().find(|t| t.id.0 == "mock").unwrap();
        assert_eq!(
            sim.kind,
            TimerKind::Mock {
                laps: 9,
                lap_ms: 100
            }
        );
    }
}
