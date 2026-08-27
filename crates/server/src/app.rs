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

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use gridfpv_engine::format::{FormatRegistry, FormatSchema};
use gridfpv_engine::scoring::{HeatResult, WinCondition, score_corrected_with_global_offsets};
use gridfpv_events::{CompetitorRef, Event, HeatId, SourceTime};
use gridfpv_projection::{
    AuditEntry, LapList, lap_list_marshaled, lap_list_marshaled_with_floor, marshaling_log,
    registrations, signal_trace,
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
    ActiveEvent, ChannelLayouts, CreateEventRequest, EventMeta, EventRegistry, LayoutError,
    LayoutId, NewChannelLayoutRequest, NewRoundReq, RegistryError, RegistryErrorKind, RoundDef,
    RoundError, RoundIssue, SetActiveEventRequest, SetChannelLayoutRequest,
    SetClassMembershipRequest, SetEventClassesRequest, SetEventRosterRequest, UpdateRoundReq,
};
use crate::live_state::{
    HeatSummary, heat_summaries, heats_of_defined_rounds, live_state_over_with_floor,
    live_state_with_floor, with_heat_timing,
};
use crate::pilots::{CreatePilotRequest, Pilot, PilotError, PilotErrorKind, UpdatePilotRequest};
use crate::round_engine;
use crate::scope::{ClassId, EventId, PilotId};
use crate::snapshot::{ProjectionBody, Snapshot};
use crate::stream::Cursor;
use crate::timers::{
    CreateTimerRequest, SetEventTimersRequest, SetPrimaryTimerRequest, SetTimerNodesRequest, Timer,
    TimerId, TimerNodes, TimerSignal, UpdateTimerRequest,
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
    /// The **command serialization lock** (release-hardening): every validated write — a control
    /// command's validate→append, and each runtime driver's checked auto-append — holds this for
    /// the whole read-check-append sequence, so a ruling can never land on a heat that went Final
    /// between its validation and its append (the auto-official racing the RD), Finalize can't
    /// slip past a concurrent FileProtest, and two ScheduleHeats can't both pass the duplicate-id
    /// check. Ordering: this lock is ALWAYS taken before the log mutex, never while holding it.
    /// Raw pass appends (the source bridge) bypass it — they validate nothing.
    commands: Arc<Mutex<()>>,
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
            commands: Arc::new(Mutex::new(())),
        }
    }

    /// Hold the command serialization lock for a validate→append sequence (see the field doc).
    /// Callers MUST NOT already hold the log mutex. A poisoned lock is recovered — the guard
    /// protects ordering, not data.
    pub fn command_guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.commands
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Append `event` iff `check` still passes over the current log, all under the command
    /// serialization lock — the runtime drivers' fire-time recheck (a cancelled-but-in-flight
    /// driver's stale transition must not land after a manual command changed the heat's state).
    /// Returns `Ok(None)` when the check rejected (nothing appended).
    pub fn append_checked(
        &self,
        event: Event,
        recorded_at: Option<i64>,
        check: impl FnOnce(&[Event]) -> bool,
    ) -> Result<Option<Offset>, ProtocolError> {
        let _guard = self.command_guard();
        let (events, _cursor) = self.read()?;
        if !check(&events) {
            return Ok(None);
        }
        self.append(event, recorded_at).map(Some)
    }

    /// Build the state from an already-shared log handle — for when the WS stream (#43)
    /// or control path (#45) needs to share the *same* `Arc<Mutex<…>>` with the router.
    pub fn from_shared(log: SharedLog) -> Self {
        Self {
            log,
            appended: Arc::new(Notify::new()),
            tokens: TokenStore::new(),
            commands: Arc::new(Mutex::new(())),
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
            commands: Arc::new(Mutex::new(())),
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
        // clock anchors to, and a heat's `HeatStarting` is the **arm instant** the start-tone
        // countdown anchors to (`tone_at` = its `recorded_at` + the logged `delay_ms`, #249). The
        // runtime/control paths append all of these with no caller timestamp, so stamp the server
        // wall clock here (the single append choke point) when one is absent — making the event's
        // `recorded_at` the authoritative timing every client reads. Without this the `HeatStarting`
        // entry is untimed and `heat_tone_at` yields `None`, so the Armed-phase countdown never shows.
        // A caller-supplied timestamp (a replay, a test pinning an instant) still wins.
        let recorded_at = recorded_at.or_else(|| {
            matches!(
                event,
                Event::HeatStateChanged { .. } | Event::HeatStarting { .. }
            )
            .then(now_micros)
        });
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
        // The product identity (alpha field-support): WHICH build is this rig running? The
        // console footer reads it, and a bug report from the field should quote it. The
        // version is the ONE workspace version (v0.4.0-alpha.1 scheme, `cargo xtask version`);
        // the contract version is the independent wire-compat integer.
        .route(
            "/about",
            get(|| async {
                Json(serde_json::json!({
                    "name": "GridFPV",
                    "version": env!("CARGO_PKG_VERSION"),
                    "contract_version": crate::CONTRACT_VERSION,
                }))
            }),
        )
        // The Director's wall clock, in epoch microseconds (open, no auth). The console measures its
        // offset from this (round-trip-corrected) so the start countdown + race clock read off
        // *server* time, not the RD device's clock — which can differ by ~1s on a separate laptop and
        // otherwise makes the Armed countdown bottom out early. See `serverNowMs` in session.svelte.ts.
        .route(
            "/time",
            get(|| async { Json(serde_json::json!({ "now_micros": now_micros() })) }),
        )
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
        // Manual **connect / disconnect** of a RotorHazard timer, independent of any event
        // (issue #383): the Timers menu's "is this thing reachable?" control. Connections used to
        // open only for the *active event's selected* timers, so verifying a URL (or the GridFPV
        // plugin) meant creating and activating an event first. RD-gated, like every other timer
        // write; the hold is explicit — it lasts until `disconnect`.
        .route("/timers/{timer_id}/connect", post(connect_timer))
        .route("/timers/{timer_id}/disconnect", post(disconnect_timer))
        // **Restart** the RotorHazard server behind a timer (#386) — the guided plugin install's
        // last step, so installing the plugin never requires opening RotorHazard's own web UI.
        // RD-gated, and REFUSED outright while a race is in progress on the timer.
        .route("/timers/{timer_id}/restart", post(restart_timer))
        // **Node discovery + the per-node enable set** (#412): what the timer said it has, what
        // GridFPV is configured for, and which nodes a heat may actually be seated on. The read is
        // open (it is the same information `GET /timers` already carries, resolved); the write is
        // RD-gated like every other timer write.
        .route(
            "/timers/{timer_id}/nodes",
            get(timer_nodes).put(set_timer_nodes),
        )
        // **Tune telemetry** (#355 S2a): live per-node signal for one timer, on demand.
        //
        // A polled read rather than a scoped subscription on the event change-stream, because it
        // is not the same kind of thing: `ws.rs` is log-offset cursors, sequences and re-snapshot
        // machinery over an event's *log*, and tune telemetry is timer-scoped, log-free, and must
        // work **before an event exists** — which is the state an untuned timer is in. The `GET`
        // both reads the snapshot and renews the subscription lease (the first call starts it);
        // the `stop` is for promptness on view close, not for correctness.
        .route("/timers/{timer_id}/signal", get(timer_signal))
        .route("/timers/{timer_id}/calibration", post(calibrate_timer))
        // **Capture** one node's threshold from a pass (#355). The same write path as the
        // calibration route above and gated identically — the difference is that RotorHazard
        // supplies the number instead of the RD, which is the only way to bootstrap a timer nobody
        // has ever tuned (#411).
        .route("/timers/{timer_id}/capture", post(capture_timer_level))
        // **Set one node's channel** while tuning it (#413). The other half of the Tune page's
        // write: a gate cannot be tuned meaningfully until its node is listening on the channel it
        // will race. Gated exactly like the calibration write above — RD-gated, RotorHazard-only,
        // refused under a *scored* heat and allowed in open practice.
        .route("/timers/{timer_id}/channel", post(set_timer_channel))
        .route("/timers/{timer_id}/signal/stop", post(stop_timer_signal))
        // The downloadable GridFPV RotorHazard plugin bundle (D16, S1) the guided-install UX
        // offers when a timer's plugin is missing/incompatible. Open read: it's static, embedded
        // at build, and carries no event data — just the plugin folder to drop into RH's plugins/.
        .route("/plugin/gridfpv.zip", get(download_plugin_bundle))
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
        // Per-event **channel layouts** (#117 S2): the event-scoped answer to *what goes on which
        // node?*. A layout is one complete tuning of the event's timer — one channel per enabled
        // node, drawn from the timer's **allowed** set (S1). The `GET` is open (a read, like the
        // heats list); add / replace / remove are RD-gated. Layouts are event state: editing one
        // never touches the global timer record, which is the bug this slice exists to close.
        .route(
            "/events/{event_id}/layouts",
            get(list_channel_layouts).post(add_channel_layout),
        )
        .route(
            "/events/{event_id}/layouts/{layout_id}",
            put(update_channel_layout).delete(remove_channel_layout),
        )
        // Per-event scheduled **heats** (race redesign Slice 3b): the round-tagged heats list the
        // Heats UI reads — open, no token (a read), like the snapshot routes.
        .route("/events/{event_id}/heats", get(list_heats))
        // Per-event **round issues** (#416): the stored rounds whose `node-{i}` seats cannot record
        // a lap — a read, open like the heats list. #412 refuses an impossible seat when a round is
        // written; this is the same rule applied to what is ALREADY stored, so a round authored
        // before that fix (or one a later node/timer change broke) is visible where the RD can
        // repair it instead of racing a heat that silently records nothing.
        .route("/events/{event_id}/round-issues", get(list_round_issues))
        // The event-wide **audit trail** (the "defensible results" review surface): every heat's
        // marshaling audit fold, heat-tagged and merged newest-first — what the console's Audit
        // page reads. Open, no token (a read, like the heats list and the snapshot routes).
        .route("/events/{event_id}/audit", get(event_audit))
        // A round's **ranking** (race redesign Slice 5/6a): the ordered per-pilot ranking the
        // engine seeds `FromRanking` from — what the bracket-carry UI displays. Open, no token.
        .route(
            "/events/{event_id}/rounds/{round_id}/ranking",
            get(round_ranking),
        )
        // A round's **standings** (time-trial / qual display): the per-pilot rows for a round — each
        // pilot's best lap plus the win-condition metric they're ranked on, in ranking order. Open,
        // no token (a read, like the ranking route).
        .route(
            "/events/{event_id}/rounds/{round_id}/standings",
            get(round_standings),
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
    // The registry types the failure: a protected-Practice delete is a client error (BadRequest),
    // a genuinely unknown id is a 404, and a file-removal (I/O) failure is a 500 — note the
    // in-memory drop already happened, so an I/O failure must NOT read as a 404.
    registry
        .delete(&event_id)
        .map_err(registry_error_to_protocol)?;
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
        .map_err(registry_error_to_protocol)?;
    Ok(Json(meta))
}

/// `GET /timers` — list every configured timer, **Mock first** (issue #73).
///
/// An **open read** (no token): a client renders the timer picker / per-event selection without a
/// credential, mirroring `GET /events`.
async fn list_timers(State(registry): State<EventRegistry>) -> Json<Vec<Timer>> {
    Json(registry.timers().list())
}

/// `GET /plugin/gridfpv.zip` — the downloadable GridFPV RotorHazard plugin bundle (D16, S1).
///
/// An **open read**: the guided-install UX offers it when a timer's plugin is missing/incompatible.
/// The bytes are a STORE-only ZIP built from the plugin source embedded at compile time
/// ([`plugin_bundle`](crate::plugin_bundle)), so the download always matches this Director's
/// protocol version. Served as an attachment so the browser saves it.
async fn download_plugin_bundle() -> Response {
    let zip = crate::plugin_bundle::plugin_zip();
    (
        [
            (axum::http::header::CONTENT_TYPE, "application/zip"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                concat!("attachment; filename=\"", "gridfpv-plugin.zip", "\""),
            ),
        ],
        zip,
    )
        .into_response()
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
    // Reject a bad config up front as a 400 (release-hardening P2): a 0 node count, an empty RH URL,
    // or a runaway Mock laps count.
    crate::timers::validate_timer_config(&body.kind, body.node_count)
        .map_err(|msg| ProtocolError::new(ErrorCode::BadRequest, msg))?;
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
    // Validate the *effective* post-edit config as a 400 (release-hardening P2): merge the partial
    // request onto the existing timer so a partial edit (e.g. just `node_count: 0`) is still caught.
    // A nonexistent id is left to `update` to report as a 404.
    if let Some(existing) = registry.timers().get(&timer_id) {
        let kind = body.kind.clone().unwrap_or(existing.kind);
        let node_count = body.node_count.or(existing.node_count);
        crate::timers::validate_timer_config(&kind, node_count)
            .map_err(|msg| ProtocolError::new(ErrorCode::BadRequest, msg))?;
    }
    let timer = registry
        .timers()
        .update(&timer_id, &body)
        .map_err(|e| ProtocolError::new(ErrorCode::UnknownScope, e.to_string()))?;
    Ok(Json(timer))
}

/// `POST /timers/{timer_id}/connect` — hold a live connection to an RH timer, RD-gated (#383).
///
/// The Timers menu's **Connect**: it sets the timer's manual connection hold, and the connection
/// reconciler dials it on its next tick **independent of any event** — so the RD can answer "is
/// this URL right? does it have the plugin?" from the page where timers are configured, with no
/// event created, activated, or selected. The connection publishes the usual `TimerStatus`
/// (`Connecting` → `Connected` / `Error`) and `PluginPresence`, so the same badges tell the story.
///
/// The hold is **explicit** — it lasts until `disconnect`, which is what a diagnostic control
/// should do; if an active event later selects the timer, the event's connection supersedes the
/// manual one (no double connection) and the hold takes it back when the event lets go.
///
/// A Mock timer is a `400` (nothing to dial); an unknown id is a 404 (`UnknownScope`). Returns the
/// updated [`Timer`].
async fn connect_timer(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(timer_id): Path<TimerId>,
) -> Result<Json<Timer>, ProtocolError> {
    set_manual_connect(&registry, &timer_id, true)
}

/// `POST /timers/{timer_id}/disconnect` — release a manually-held RH connection, RD-gated (#383).
///
/// The Timers menu's **Disconnect**: it clears the manual hold, and the reconciler drops the link
/// on its next tick (leaving the timer `Disconnected`) — unless the active event also selects the
/// timer, in which case the event-driven connection stays up, which is the point of holding the two
/// inputs separately. An unknown id is a 404 (`UnknownScope`). Returns the updated [`Timer`].
async fn disconnect_timer(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(timer_id): Path<TimerId>,
) -> Result<Json<Timer>, ProtocolError> {
    set_manual_connect(&registry, &timer_id, false)
}

/// The shared body of [`connect_timer`] / [`disconnect_timer`]: flip the hold and map the registry
/// error onto a typed protocol error — a genuinely unknown id is a 404, anything else (a Mock with
/// nothing to dial) is a client `400`.
fn set_manual_connect(
    registry: &EventRegistry,
    timer_id: &TimerId,
    held: bool,
) -> Result<Json<Timer>, ProtocolError> {
    let timers = registry.timers();
    timers
        .set_manual_connect(timer_id, held)
        .map(Json)
        .map_err(|e| {
            let code = if timers.exists(timer_id) {
                ErrorCode::BadRequest
            } else {
                ErrorCode::UnknownScope
            };
            ProtocolError::new(code, e.to_string())
        })
}

/// `POST /timers/{timer_id}/restart` — restart a RotorHazard timer's server, RD-gated (#386).
///
/// The guided plugin install's last step. RotorHazard imports plugins **once at startup**, so the
/// `plugins/gridfpv/` folder the RD just dropped in is inert until RH re-executes; RH exposes that
/// restart, unauthenticated, on the socket the Director is already holding
/// (`restart_server`). Emitting it here means the whole install is three clicks inside GridFPV
/// rather than a trip to RotorHazard's own web UI. **Only `restart_server` is wired** — its
/// `shutdown_pi` / `reboot_pi` neighbours take the timing hardware down rather than bringing it
/// back, and stay out of reach.
///
/// # The refusals
///
/// * **A race in progress on this timer → `400`.** Restarting RotorHazard mid-heat takes the RD's
///   timing hardware down with the race on it, so this is gated on **heat phase**
///   ([`EventRegistry::heat_in_progress_on_timer`]: `Staged`/`Armed`/`Running`/`Unofficial` in any
///   event that selects the timer), not merely confirmed in the console. The refusal names the
///   heat and the timer by their **friendly names** (repo display rule).
/// * A **Mock**, or a timer that is **not connected**, is a `400` (nothing to restart, or no
///   socket to emit on); an unknown id is a 404 (`UnknownScope`).
///
/// On success the request is parked on the timer registry and the connection reconciler emits it on
/// its next tick; the updated [`Timer`] is returned, matching `connect`/`disconnect`. What follows
/// is an **expected** drop → reconnect: the socket closes, the timer passes through
/// `Disconnected`/`Error` for a few seconds, and the reconnect re-probes the plugin — which is what
/// flips its `PluginPresence` from `Missing` to `Present`. The console presents that window as a
/// restart in progress, not as a fault.
async fn restart_timer(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(timer_id): Path<TimerId>,
) -> Result<Json<Timer>, ProtocolError> {
    let timers = registry.timers();
    // Resolve the timer first so the refusals below can name it, and so an unknown id is a clean
    // 404 rather than a message about a timer that does not exist.
    let timer = timers.get(&timer_id).ok_or_else(|| {
        ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no timer with id {:?}", timer_id.0),
        )
    })?;
    // Kind first: the built-in Mock is in every event's DEFAULT timer selection, so gating on the
    // heat before the kind answered a Mock with "… is running Heat 1 — finish or reset that heat",
    // which is both wrong and actionable-looking. A Mock has no timing server at all.
    if !matches!(timer.kind, crate::timers::TimerKind::Rotorhazard { .. }) {
        return Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!(
                "{} is not a RotorHazard timer — there is no timing server to restart",
                timer.name
            ),
        ));
    }
    // The hard gate: never restart the timing hardware out from under a live race.
    if let Some(heat) = registry.heat_in_progress_on_timer(&timer_id) {
        return Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!(
                "{} is running {} — finish or reset that heat before restarting the timer",
                timer.name, heat
            ),
        ));
    }
    timers
        .request_restart(&timer_id)
        .map(Json)
        .map_err(|e| ProtocolError::new(ErrorCode::BadRequest, e.to_string()))
}

/// `GET /timers/{timer_id}/nodes` — a timer's **node set**: reported, configured, enabled (#412).
///
/// Returns a [`TimerNodes`]: every node index the timer has, each with its 1-based display label
/// and its enabled state, plus the enabled indices in seat order and any [`NodeDrift`] between what
/// the hardware reported and what GridFPV is configured for.
///
/// This is the shared resolver for "how many pilots fit in a heat on this timer, and which gates do
/// they sit on?" — the console, the seat mapping and the calibration guard all read the same answer
/// from [`Timer::node_view`] rather than each re-deriving it. An unknown id is a 404.
///
/// [`NodeDrift`]: crate::timers::NodeDrift
/// [`Timer::node_view`]: crate::timers::Timer::node_view
async fn timer_nodes(
    State(registry): State<EventRegistry>,
    Path(timer_id): Path<TimerId>,
) -> Result<Json<TimerNodes>, ProtocolError> {
    registry.timers().nodes(&timer_id).map(Json).ok_or_else(|| {
        ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no timer with id {:?}", timer_id.0),
        )
    })
}

/// `PUT /timers/{timer_id}/nodes` — set a timer's node config, RD-gated (#412).
///
/// Body is a [`SetTimerNodesRequest`]: the width override (a number to pin it, `null` to go back to
/// following the timer, absent to leave it) and/or the set of node indices to keep **enabled**.
///
/// This is the RD's answer to *"reported is 4 but node 3 is busted, I need to use nodes 1, 2 and
/// 4"* — a **set**, not a count, because a dead node is rarely the last one. It is a decision, so it
/// is persisted and survives a reconnect: a timer that keeps reporting four working nodes does not
/// get to switch one back on.
///
/// Refused as a `400` for a `node_count` of `0` and for an edit that would leave **no** node
/// enabled (both cap every heat to no pilots); an unknown id is a 404.
async fn set_timer_nodes(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(timer_id): Path<TimerId>,
    Json(body): Json<SetTimerNodesRequest>,
) -> Result<Json<TimerNodes>, ProtocolError> {
    let timers = registry.timers();
    if !timers.exists(&timer_id) {
        return Err(ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no timer with id {:?}", timer_id.0),
        ));
    }
    timers
        .set_nodes(&timer_id, &body)
        .map(Json)
        .map_err(|e| ProtocolError::new(ErrorCode::BadRequest, e.to_string()))
}

/// `GET /timers/{timer_id}/signal` — the timer's **live tuning signal**, RD-gated (#355 S2a).
///
/// Returns a [`TimerSignal`]: every node the timer reports (**including unseated ones** — "is this
/// node even alive?" is half the diagnostic), each with its latest RSSI / peak / nadir / pass count
/// / thresholds and a bounded rolling RSSI window for the graph.
///
/// **The call is the subscription.** The first `GET` starts the stream — the connection driver
/// sees the new lease on its next tick and opens the transport's pre-parse gate — and every `GET`
/// renews it. Stop polling and the stream stops itself within [`SIGNAL_LEASE`], which is what
/// makes a closed tab, a crashed browser or a dropped network safe: none of them get to leave a
/// timer streaming forever, and none of them have to say goodbye.
///
/// Nothing this touches is an `Event`, a `SignalChunk`, or a log. The data exists only in memory,
/// only while an RD is looking at it.
///
/// A **Mock** is a `400` (it has no signal to read); an unknown id is a 404 (`UnknownScope`).
async fn timer_signal(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(timer_id): Path<TimerId>,
) -> Result<Json<TimerSignal>, ProtocolError> {
    let timers = registry.timers();
    let timer = signal_capable_timer(&timers, &timer_id)?;
    let _ = timer;
    Ok(Json(timers.signal(&timer_id)))
}

/// `POST /timers/{timer_id}/calibration` — set one node's enter/exit thresholds, RD-gated (#355).
///
/// The **write** half of the Tune page, and the thing that turns it from a diagnostic into a
/// repair. Body is a [`CalibrationRequest`]: a node, and whichever of `enter_at` / `exit_at`
/// actually moved. There is no Apply button on the page, so this is called per adjustment (on
/// pointer-up, on blur) rather than once per session — which is also why every refusal below is
/// re-checked on **every** write and not once at page load.
///
/// # This acknowledges a dispatch. It is not a readback.
///
/// RotorHazard does not echo a level set: `on_set_enter_at_level` / `on_set_exit_at_level` call
/// straight into `calibration.py`, which writes the profile, pushes to the hardware and fires an
/// internal `Evt` — and emits nothing back (identical on v4.3.0 and v4.4.0). So a `200` here means
/// the write was accepted and queued onto the live socket, and **nothing more**. Answering with a
/// synthesised readback would report success for a write that may never have reached the detector,
/// which is precisely the failure this page exists to diagnose.
///
/// **The console confirms by poll.** The Director asks RotorHazard to re-broadcast
/// `enter_and_exit_at_levels` immediately after each write; that arrives on the same socket that
/// feeds [`timer_signal`], so the value comes back as [`NodeSignal::enter_at`] /
/// [`NodeSignal::exit_at`] on the next `GET /timers/{id}/signal`. A threshold that never comes back
/// holding the value the RD sent is a write that did not land, and the page must say so.
///
/// # The refusals
///
/// * A **Mock** → `400`: it has no radio, so there is nothing to calibrate.
/// * A **scored race in progress on this timer** → `400`, gated on heat phase
///   ([`EventRegistry::scored_heat_in_progress_on_timer`]:
///   `Staged`/`Armed`/`Running`/`Unofficial` in the active event). Moving a detection threshold
///   under a competition heat changes what counts as a lap while it is being counted.
///
///   **Open practice is exempt, and deliberately so.** Practice is excluded from scoring (#398),
///   so there is no result for a moved threshold to corrupt — and a pilot in the air on a practice
///   heat is exactly when an RD wants to tune (*"I want to slide the slider and then test right
///   away"*). Refusing there would leave the RD tuning an idle gate and walking a quad through by
///   hand: the RotorHazard-UI loop this page exists to replace. This is a **narrower** gate than
///   [`restart_timer`]'s on purpose — a restart takes the timing hardware down and destroys the
///   practice session with it, while a threshold nudge does not.
/// * A timer that is **not connected**, a `node` beyond the timer's width, or a body carrying
///   **neither** threshold → `400` from the registry.
/// * An unknown id → 404 (`UnknownScope`).
///
/// Every refusal names the timer by its **friendly name** (repo display rule).
///
/// Levels are **clamped** server-side to `RSSI_MIN..=RSSI_MAX`, never trusted from the client: a
/// `0` is falsy to RotorHazard and would silently no-op while looking accepted.
async fn calibrate_timer(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(timer_id): Path<TimerId>,
    Json(request): Json<crate::timers::CalibrationRequest>,
) -> Result<Json<crate::timers::CalibrationDispatch>, ProtocolError> {
    let timers = registry.timers();
    // Resolve first so the refusals can name the timer, and so an unknown id is a clean 404 rather
    // than a message about a timer that does not exist.
    let timer = timers.get(&timer_id).ok_or_else(|| {
        ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no timer with id {:?}", timer_id.0),
        )
    })?;
    // Kind before phase, for the same reason [`restart_timer`] does it: the built-in Mock is in
    // every event's default selection, so gating on the heat first would answer a Mock with "… is
    // running Heat 1", which is both wrong and actionable-looking.
    if !matches!(timer.kind, crate::timers::TimerKind::Rotorhazard { .. }) {
        return Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!(
                "{} is not a RotorHazard timer — there is no detector to calibrate",
                timer.name
            ),
        ));
    }
    // The hard gate: never move a detection threshold under a SCORED race. Open practice is
    // exempt (#398 excludes it from scoring), which is what lets an RD tune with pilots in the air.
    let scored_heat = registry.scored_heat_in_progress_on_timer(&timer_id);
    if let Some(heat) = scored_heat {
        return Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!(
                "{} is running {}, a scored heat — finish or reset it before changing its \
                 thresholds (open practice can be tuned while it runs)",
                timer.name, heat
            ),
        ));
    }
    // Whether a heat — necessarily an open-practice one, the scored case having just refused — is
    // racing on this timer right now. Carried onto the write so the driver's own armed-heat
    // backstop knows this one was cleared: without it the route would accept a practice write the
    // driver then silently dropped, and a write that reports dispatched but never lands is the
    // exact failure this page exists to catch.
    let during_open_practice = registry.heat_in_progress_on_timer(&timer_id).is_some();
    timers
        .request_calibration(&timer_id, &request, during_open_practice)
        .map(Json)
        .map_err(|e| ProtocolError::new(ErrorCode::BadRequest, e.to_string()))
}

/// `POST /timers/{timer_id}/capture` — have the timer **measure** one node's threshold, RD-gated
/// (#355).
///
/// The Tune page's third write, and the answer to a gap #411 names in as many words: a fresh RD
/// with no saved profile and a badly-tuned timer has **no starting point**. GridFPV deliberately
/// refuses to ship a fabricated default — the right level depends on craft, VTX power, antenna and
/// gate geometry, none of which GridFPV knows, and a default would also change the hardware on
/// first connect, which is the surprise D27's drift rule exists to prevent. A capture measures the
/// RD's actual craft on their actual gate. It is the only non-guessing bootstrap there is.
///
/// # What the RD is agreeing to when this is called
///
/// RotorHazard opens a **three-second sampling window the instant the emit lands**
/// (`CAP_ENTER_EXIT_AT_MILLIS`, identical on v4.3.0 and v4.4.0) and averages the node's RSSI across
/// it — it does not look back at a lap already flown, and it does not take the peak. The pass has
/// to happen inside the window. That is why [`CaptureDispatch`] carries `window_ms`: the console
/// counts it down rather than hardcoding a number that could drift from RotorHazard's, and it is
/// why the control is labelled with what it will do rather than with a bare verb.
///
/// # This acknowledges a dispatch. It cannot be a readback.
///
/// Same rule as [`calibrate_timer`], one step stronger: a `200` here means the capture was
/// *started*, and the level it will produce does not exist yet. RotorHazard's handler returns
/// nothing on any path — including the paths where it silently refuses, which are a node that is
/// not answering (`api_valid_flag`) and a capture already running on that node/threshold.
///
/// **Confirmation is by poll.** The captured level reaches the console as
/// [`NodeSignal::enter_at`] / [`NodeSignal::exit_at`] on a later `GET /timers/{id}/signal`, fed by
/// RotorHazard's own end-of-capture `node_enter_at_level` broadcast *and* by the readback the
/// driver fires once the window closes. A capture whose level never comes back is reported as not
/// landed — never as a success.
///
/// # The refusals
///
/// Everything [`calibrate_timer`] refuses, for the same reasons and in the same order:
///
/// * a **Mock** → `400` (no detector to capture from);
/// * a **scored race in progress on this timer** → `400`. A capture *ends by setting a threshold*,
///   so it moves a detector mid-race exactly as a typed level does. **Open practice is exempt**
///   (#398 excludes it from scoring), and a practice heat is the natural moment to capture: the
///   pass the capture needs is one a pilot is already flying.
/// * a timer that is **not connected**, a `node` beyond the timer's width **or one the RD has
///   disabled** (#412) → `400` from the registry;
/// * a capture of that threshold **already running on that node** → `400`. RotorHazard refuses that
///   one in silence, so accepting it here would show a capture as started that never was.
/// * an unknown id → `404`.
///
/// Every refusal names the timer and the node by their **friendly names** (repo display rule).
async fn capture_timer_level(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(timer_id): Path<TimerId>,
    Json(request): Json<crate::timers::CaptureRequest>,
) -> Result<Json<crate::timers::CaptureDispatch>, ProtocolError> {
    let timers = registry.timers();
    let timer = timers.get(&timer_id).ok_or_else(|| {
        ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no timer with id {:?}", timer_id.0),
        )
    })?;
    // Kind before phase, exactly as `calibrate_timer` does it: the Mock is in every event's default
    // selection, so gating on the heat first would answer a Mock with "… is running Heat 1".
    if !matches!(timer.kind, crate::timers::TimerKind::Rotorhazard { .. }) {
        return Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!(
                "{} is not a RotorHazard timer — there is no detector to capture from",
                timer.name
            ),
        ));
    }
    if let Some(heat) = registry.scored_heat_in_progress_on_timer(&timer_id) {
        return Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!(
                "{} is running {}, a scored heat — a capture sets the threshold when it finishes, \
                 so it would change which laps that heat counts (open practice can be captured \
                 while it runs)",
                timer.name, heat
            ),
        ));
    }
    let during_open_practice = registry.heat_in_progress_on_timer(&timer_id).is_some();
    timers
        .request_capture(&timer_id, &request, during_open_practice)
        .map(Json)
        .map_err(|e| ProtocolError::new(ErrorCode::BadRequest, e.to_string()))
}

/// `POST /timers/{timer_id}/channel` — set one node's **channel**, RD-gated (#413).
///
/// The Tune page already *shows* each node's frequency; this makes it settable, so an RD standing
/// at the gate never has to leave for heat setup (or RotorHazard's own UI) to put the node on the
/// channel it will race and then walk back. Body is a [`ChannelRequest`]: a node, a raw centre
/// frequency, and the catalog band/channel the RD picked.
///
/// # Band and channel travel with the frequency
///
/// RotorHazard's `on_set_frequency` accepts `{ node, frequency, band?, channel? }` and stores the
/// label on the active profile when it is given. Sending the frequency alone leaves RotorHazard's
/// own UI showing a bare number with no `R7`-style label — and the RD validates this work *by
/// refreshing that page*, where an unlabelled channel reads as "it half worked". The label is
/// resolved server-side against GridFPV's own catalog (D27 owns the vocabulary), so a hand-rolled
/// client cannot put an invented band name on the timer.
///
/// # This acknowledges a dispatch. It is not a readback.
///
/// Same rule as [`calibrate_timer`]: a `200` means accepted and queued onto the live socket. The
/// console confirms by poll — every RotorHazard heartbeat carries each node's current frequency, so
/// the change comes back as [`NodeSignal::frequency_mhz`] on a later `GET /timers/{id}/signal`.
///
/// # The refusals
///
/// * A **Mock** → `400`: it has no receiver to tune.
/// * A **scored race in progress on this timer** → `400`
///   ([`EventRegistry::scored_heat_in_progress_on_timer`]). Retuning a node's receiver mid-race
///   takes the gate off the channel the pilot is flying — at least as disruptive as moving a
///   threshold. **Open practice is exempt** for exactly #398's reason, and because tuning with
///   pilots in the air is the workflow this page exists for.
/// * A timer that is **not connected**, a `node` beyond the timer's width **or one the RD has
///   disabled** (#412), a frequency outside the 5.8 GHz band, or one a **Fixed** timer does not
///   support → `400` from the registry.
/// * An unknown id → 404 (`UnknownScope`).
///
/// **Two nodes on one channel is not refused** — the console flags it, because it is a real
/// mistake, but it is also what a swap looks like halfway through.
///
/// Every refusal names the timer, the node and the channel by their **friendly names** (repo
/// display rule): `Node 3`, `Raceband R7`, never `2` or `5880`.
///
/// [`NodeSignal::frequency_mhz`]: crate::timers::NodeSignal::frequency_mhz
/// [`ChannelRequest`]: crate::timers::ChannelRequest
async fn set_timer_channel(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(timer_id): Path<TimerId>,
    Json(request): Json<crate::timers::ChannelRequest>,
) -> Result<Json<crate::timers::ChannelDispatch>, ProtocolError> {
    let timers = registry.timers();
    // Resolve first so every refusal can name the timer, and so an unknown id is a clean 404.
    let timer = timers.get(&timer_id).ok_or_else(|| {
        ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no timer with id {:?}", timer_id.0),
        )
    })?;
    // Kind before phase, exactly as [`calibrate_timer`] does it: the built-in Mock is in every
    // event's default selection, so gating on the heat first would answer a Mock with "… is running
    // Heat 1", which is both wrong and actionable-looking.
    if !matches!(timer.kind, crate::timers::TimerKind::Rotorhazard { .. }) {
        return Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!(
                "{} is not a RotorHazard timer — there is no receiver to tune",
                timer.name
            ),
        ));
    }
    // The hard gate: never retune a node's receiver under a SCORED race. Open practice is exempt.
    if let Some(heat) = registry.scored_heat_in_progress_on_timer(&timer_id) {
        return Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!(
                "{} is running {}, a scored heat — finish or reset it before changing a node's \
                 channel (open practice can be retuned while it runs)",
                timer.name, heat
            ),
        ));
    }
    // Whether a heat — necessarily an open-practice one — is racing right now, carried onto the
    // write so the driver's own armed-heat backstop knows this one was already cleared.
    let during_open_practice = registry.heat_in_progress_on_timer(&timer_id).is_some();
    timers
        .request_channel(&timer_id, &request, during_open_practice)
        .map(Json)
        .map_err(|e| ProtocolError::new(ErrorCode::BadRequest, e.to_string()))
}

/// `POST /timers/{timer_id}/signal/stop` — end the timer's tuning stream now, RD-gated (#355 S2a).
///
/// The lease already guarantees the stream stops; this makes it stop *promptly* when the RD closes
/// the Tune view, instead of a few seconds later. Idempotent, and harmless on a timer that was
/// never streaming. An unknown id is a 404 (`UnknownScope`).
async fn stop_timer_signal(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(timer_id): Path<TimerId>,
) -> Result<StatusCode, ProtocolError> {
    let timers = registry.timers();
    if !timers.exists(&timer_id) {
        return Err(ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no timer with id {:?}", timer_id.0),
        ));
    }
    timers.stop_signal(&timer_id);
    Ok(StatusCode::NO_CONTENT)
}

/// Resolve a timer that can carry tune telemetry, or say why it cannot.
///
/// Kind-checked the same way [`restart_timer`] is, and for the same reason: the built-in Mock is in
/// every event's default selection, so answering "no signal yet" for it would look like a timer
/// that is merely quiet rather than one that has no detector at all. The refusal names the timer by
/// its **friendly name** (repo display rule).
fn signal_capable_timer(
    timers: &crate::timers::TimerRegistry,
    timer_id: &TimerId,
) -> Result<Timer, ProtocolError> {
    let timer = timers.get(timer_id).ok_or_else(|| {
        ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no timer with id {:?}", timer_id.0),
        )
    })?;
    if !matches!(timer.kind, crate::timers::TimerKind::Rotorhazard { .. }) {
        return Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!(
                "{} is not a RotorHazard timer — it has no detector signal to tune against",
                timer.name
            ),
        ));
    }
    Ok(timer)
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
///
/// # The GridFPV-plugin gate (#405)
///
/// A RotorHazard timer without a loaded, compatible GridFPV plugin **cannot be newly selected**:
/// the refusal is a typed `400` carrying
/// [`SelectionRefusal::selection_message`](crate::timers::SelectionRefusal::selection_message),
/// which names the timer by its friendly name and says what to do next. This lives here, in the
/// API, and not only in the console's picker, because this route is reachable directly — a rule
/// enforced only in the UI is not enforced. Mock timers are never gated.
///
/// **Already-selected timers are grandfathered.** Only ids that are *not already* in the event's
/// selection are gated. Two reasons: (1) an event persisted before this rule may already select a
/// plugin-less RH timer, and re-affirming that selection — which the console's wholesale
/// auto-save does on *every* toggle — must not fail, or the RD could never edit that event's
/// timers again; (2) "select" is the act being gated, and re-sending an existing selection is not
/// selecting. What stops such an event from actually *racing* a plugin-less timer is the
/// **arm-time backstop** in `control_handler`, plus the warning the console renders on the row.
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
    // The plugin gate (#405), applied only to *newly* selected ids. An unknown event has no
    // selection to compare against and no reason to be gated — `set_timers` below reports it as
    // the typed 404 it already is, and reporting a plugin problem on a non-existent event would
    // bury that.
    if let Some(meta) = registry.meta_of(&event_id) {
        let already: std::collections::BTreeSet<_> = meta.timers.iter().collect();
        for id in &body.ids {
            if already.contains(id) {
                continue;
            }
            let Some(timer) = timers.get(id) else {
                continue;
            };
            if let Some(refusal) = timer.selection_refusal() {
                return Err(ProtocolError::new(
                    ErrorCode::BadRequest,
                    refusal.selection_message(&timer.name),
                ));
            }
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
        .map_err(registry_error_to_protocol)?;
    // Record the primary in the same request (it is now guaranteed in the just-set selection).
    let meta = registry
        .set_primary_timer(&event_id, body.primary)
        .map_err(registry_error_to_protocol)?;
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
        .map_err(registry_error_to_protocol)?;
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

/// Map a [`RegistryError`] to a typed [`ProtocolError`] (release-hardening P1-7): an unknown id is
/// an `UnknownScope` (404), a bad request is a `BadRequest` (400), and an **I/O / persistence**
/// failure is `Internal` (500). The last is the load-bearing case: the in-memory state is mutated
/// before the write-through, so a failed write must surface as a 500 (not a 404/400) — the change
/// did not durably land. Mirrors [`pilot_error_to_protocol`] / [`class_error_to_protocol`].
fn registry_error_to_protocol(error: RegistryError) -> ProtocolError {
    let code = match error.kind {
        RegistryErrorKind::NotFound => ErrorCode::UnknownScope,
        RegistryErrorKind::Invalid => ErrorCode::BadRequest,
        RegistryErrorKind::Io => ErrorCode::Internal,
    };
    ProtocolError::new(code, error.to_string())
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
        .map_err(registry_error_to_protocol)?;
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
        .map_err(registry_error_to_protocol)?;
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
        .map_err(registry_error_to_protocol)?;
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
        .map_err(registry_error_to_protocol)?;
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
    // The event must exist (else a 404). Beyond directory existence, membership is scoped to **this
    // event** (release-hardening P1-5): the class must be one the event *selected* and every pilot
    // must be on the event's *roster* — otherwise a raw API call could seat a non-roster pilot or a
    // class the event never picked, and the raced field would diverge from the seeding/standings
    // (which resolve against the roster). Mirrors `validate_round_fields`'s class-selection guard.
    let meta = registry.meta_of(&event_id).ok_or_else(|| {
        ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no event with id {:?}", event_id.0),
        )
    })?;
    if !meta.classes.contains(&class_id) {
        return Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!("class {:?} is not selected by this event", class_id.0),
        ));
    }
    for slot in &body.pilots {
        if !meta.roster.contains(&slot.pilot) {
            return Err(ProtocolError::new(
                ErrorCode::BadRequest,
                format!("pilot {:?} is not on this event's roster", slot.pilot.0),
            ));
        }
    }
    // Validate each assigned channel (race redesign Slice 7a) against the event's **primary**
    // timer's **allowed** channel set (#117 S1) — the GQ-style fixed channel must be one the RD has
    // said this timer may use. The allowed set may exceed the timer's `node_count` (node_count caps
    // only pilots-per-heat), so any channel in it is valid; we never cap the number of distinct
    // channels at node_count.
    let assigned: Vec<u16> = body.pilots.iter().filter_map(|s| s.channel).collect();
    if !assigned.is_empty() {
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
                    // CLAUDE.md: the RD reads a band+channel label and a timer NAME — never a raw
                    // MHz number and never the timer's id.
                    format!(
                        "{} is not one of the channels {:?} is allowed to use",
                        crate::timers::channel_label(*channel),
                        timer.name
                    ),
                ));
            }
        }
    }
    let meta = registry
        .set_class_membership(&event_id, class_id, body.pilots)
        .map_err(registry_error_to_protocol)?;
    Ok(Json(meta))
}

// ── Event channel layouts (#117 S2) ──────────────────────────────────────────────────────────────
//
// Four routes, shaped exactly like the rounds ones, and every write answers with the **whole**
// [`ChannelLayouts`] view rather than the one layout it touched. The overlap warnings are a property
// of the layout *set*, so returning only the changed layout would leave the console to re-derive
// them — a second implementation of a rule the Director already owns.

/// Map a [`LayoutError`] to a [`ProtocolError`]: a missing event/layout is a typed **404**
/// ([`ErrorCode::UnknownScope`]); an invalid tuning (duplicate channel, a channel outside the
/// timer's allowed set, a disabled/out-of-range node, an incomplete mapping, a blank/duplicate
/// name) is a **400** ([`ErrorCode::BadRequest`]) whose message is already phrased for the RD.
fn layout_error(e: LayoutError) -> ProtocolError {
    let code = match e {
        LayoutError::EventNotFound(_) | LayoutError::LayoutNotFound(_) => ErrorCode::UnknownScope,
        LayoutError::Invalid(_) => ErrorCode::BadRequest,
    };
    ProtocolError::new(code, e.to_string())
}

/// `GET /events/{event_id}/layouts` — an event's **channel layouts** plus their cross-layout overlap
/// warnings (#117 S2). Open, no token (a read, like the heats list).
///
/// The `overlaps` are advisory and always have been: reusing a channel between layouts only matters
/// for the keep-pilots-on-one-channel strategy, so it is reported and never enforced. An unknown
/// event is a typed **404**.
async fn list_channel_layouts(
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
) -> Result<Json<ChannelLayouts>, ProtocolError> {
    registry
        .channel_layouts(&event_id)
        .map(Json)
        .ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::UnknownScope,
                format!("no event with id {:?}", event_id.0),
            )
        })
}

/// `POST /events/{event_id}/layouts` — define a **channel layout** on an event (#117 S2), RD-gated.
///
/// [`ControlAuth`] runs first. The layout id is **auto-generated** server-side (never in the body).
/// Omitting `nodes` **seeds** the layout from the event timer's allowed set — the global→event seam:
/// what the RD ticked globally is the default an event starts from, and every edit from here on is
/// event-local. A tuning that duplicates a channel, names a channel the timer is not allowed to
/// use, names a disabled/out-of-range node, or leaves an enabled node untuned is a typed **400**;
/// an unknown event is a **404**. On success the event's meta is written through to disk (issue
/// #115) and the whole resulting [`ChannelLayouts`] view is returned.
async fn add_channel_layout(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
    Json(body): Json<NewChannelLayoutRequest>,
) -> Result<Json<ChannelLayouts>, ProtocolError> {
    registry
        .add_channel_layout(&event_id, body)
        .map(Json)
        .map_err(layout_error)
}

/// `PUT /events/{event_id}/layouts/{layout_id}` — replace a **channel layout**'s name and mapping
/// (#117 S2), RD-gated.
///
/// The id is fixed (the path segment); the name and the whole node → channel mapping are replaced
/// wholesale and re-validated exactly as on create. Unknown event/layout → **404**; an invalid
/// tuning → **400**. Written through to disk (issue #115); returns the whole updated
/// [`ChannelLayouts`] view.
async fn update_channel_layout(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path((event_id, layout_id)): Path<(EventId, LayoutId)>,
    Json(body): Json<SetChannelLayoutRequest>,
) -> Result<Json<ChannelLayouts>, ProtocolError> {
    registry
        .update_channel_layout(&event_id, &layout_id, body)
        .map(Json)
        .map_err(layout_error)
}

/// `DELETE /events/{event_id}/layouts/{layout_id}` — remove a **channel layout** (#117 S2), RD-gated.
///
/// Unknown event/layout → **404** (not a silent no-op: a console deleting a layout someone else
/// already deleted is told, rather than left believing it removed something). Written through to
/// disk (issue #115); returns the whole updated [`ChannelLayouts`] view.
async fn remove_channel_layout(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path((event_id, layout_id)): Path<(EventId, LayoutId)>,
) -> Result<Json<ChannelLayouts>, ProtocolError> {
    registry
        .remove_channel_layout(&event_id, &layout_id)
        .map(Json)
        .map_err(layout_error)
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
            // Open practice's single channel heat is one draw — single-step (#216).
            crate::control::FillMode::Next,
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
    // Heats whose round the event no longer defines went with that round (#418). The log is
    // append-only so the `HeatScheduled` entries remain, but a removed round takes its heats with
    // it: they have no name, no win condition and no scoring left to resolve through. Only
    // unstarted heats can be in this position — `remove_round` refuses a round with a heat in
    // progress or past `Scheduled` — so nothing with results is ever hidden here.
    let defined: Vec<gridfpv_events::RoundId> = registry
        .rounds_of(&event_id)
        .unwrap_or_default()
        .into_iter()
        .map(|round| round.id)
        .collect();
    Ok(Json(heats_of_defined_rounds(
        heat_summaries(&events),
        &defined,
    )))
}

/// `GET /events/{event_id}/round-issues` — the event's **impossible seats** (#416).
///
/// A read (open, no token, like the heats list): every stored round whose open-practice seating
/// names a node that cannot record a lap — one beyond the primary timer's width, one the RD has
/// disabled, or one beyond what the timer reported ([`SeatProblem`]). Each entry carries the
/// round's label, the timer's name, the 1-based node label and the RD-facing sentence that says
/// what to do about it; the console renders them on the round they belong to, next to the edit
/// control that repairs them.
///
/// Empty means **nothing wrong** — an event with no resolvable primary timer has no node set to
/// check against and answers with an empty list. An unknown event is a typed **404**.
///
/// [`SeatProblem`]: crate::events::SeatProblem
async fn list_round_issues(
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
) -> Result<Json<Vec<RoundIssue>>, ProtocolError> {
    registry.round_issues(&event_id).map(Json).ok_or_else(|| {
        ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no event with id {:?}", event_id.0),
        )
    })
}

/// Map a [`round_engine::FillError`] to a typed [`ProtocolError`]: an unknown round (or seeding
/// source round) is a **404** ([`ErrorCode::UnknownScope`]); an unscorable round (empty field,
/// unknown format) is a **400** ([`ErrorCode::BadRequest`]) — mirroring the control handler's
/// `FillRound` mapping so the read routes answer the same shape.
fn fill_error(err: round_engine::FillError) -> ProtocolError {
    use round_engine::FillError;
    let code = match err {
        FillError::UnknownRound(_) | FillError::UnknownSourceRound(_) => ErrorCode::UnknownScope,
        FillError::EmptyField(_)
        | FillError::UnknownFormat(_)
        | FillError::MissingChannel(_)
        | FillError::Assign(_)
        | FillError::SeedingTooDeep => ErrorCode::BadRequest,
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
    // Open practice is EXCLUDED from ranking — the one and only way a practice round differs from
    // any other (`crate::open_practice`). Its laps are on the log like everyone else's; they simply
    // never place anybody.
    if crate::open_practice::excluded_from_scoring(round) {
        return Ok(Json(Vec::new()));
    }
    let (events, _cursor) = state.read()?;
    let ranking = round_engine::round_ranking(&meta, round, &events).map_err(fill_error)?;
    Ok(Json(ranking))
}

/// `GET /events/{event_id}/rounds/{round_id}/standings` — a round's **standings** (time-trial / qual
/// display).
///
/// The per-pilot rows for a single round ([`round_engine::round_standings`]): each pilot's best
/// single lap plus the win-condition metric they're ranked on (best-N-consecutive time, lap count,
/// or best lap), in [`round_engine::round_ranking`] order so the standings + ranking never disagree.
/// A read (open, no token, like the ranking route). An unknown event or round is a typed **404**; a
/// round that cannot be scored (unknown format, dangling seeding source) is a **400**.
async fn round_standings(
    State(registry): State<EventRegistry>,
    Path((event_id, round_id)): Path<(EventId, RoundId)>,
) -> Result<Json<Vec<round_engine::RoundStanding>>, ProtocolError> {
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
    // Open practice is EXCLUDED from standings (`crate::open_practice`) — see `round_ranking`.
    if crate::open_practice::excluded_from_scoring(round) {
        return Ok(Json(Vec::new()));
    }
    let (events, _cursor) = state.read()?;
    let standings = round_engine::round_standings(&meta, round, &events).map_err(fill_error)?;
    Ok(Json(standings))
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
    // Open practice is EXCLUDED from standings (`crate::open_practice`): the class join folds a
    // meta with every excluded round already removed, so a practice round can never contribute
    // points, laps or a best lap — nor a points adjustment ruled on one of its heats.
    let meta = crate::open_practice::scoring_meta(&meta);
    let standings = round_engine::class_standings(&meta, &class_id, &events).map_err(fill_error)?;
    Ok(Json(standings))
}

/// One row of the **event-wide** audit trail (`GET /events/{event_id}/audit`): a per-heat
/// marshaling [`AuditEntry`] plus the heat it belongs to.
///
/// The per-heat [`AuditEntry`] deliberately carries no heat id — it is served from a heat-scoped
/// route where the heat is implicit. The event-wide read merges every heat's trail into one list,
/// so each entry must say *which* heat it rules on; the console's Audit page renders (and filters
/// by) that tag. The entry's own fields are flattened onto the row, so on the wire this is "an
/// `AuditEntry` plus `heat`" — additive, no re-modelling.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "bindings/")]
pub struct EventAuditEntry {
    /// The heat this ruling belongs to — attributed by the same heat-window rules Marshaling's
    /// per-heat audit uses ([`heat_window_offsets`]), so the two views can never disagree.
    pub heat: HeatId,
    /// The per-heat audit fact itself (kind / when / offset / competitor / summary), flattened.
    #[serde(flatten)]
    pub entry: AuditEntry,
}

/// Fold the whole event log into the **event-wide audit trail**, newest first.
///
/// For each heat the log ever scheduled (the distinct `HeatScheduled` ids, in order), this runs
/// the *existing* per-heat audit fold — [`marshaling_log`] over that heat's
/// [`heat_window_offsets`], exactly the pipeline the heat-scoped `?projection=audit` snapshot
/// uses — tags each entry with the heat id, merges all heats, and sorts by the entry's global
/// append offset **descending** (append order is time order, so newest first). Reusing the
/// shipped window attribution is the point: a ruling filed about a *finished* heat while a later
/// heat is live lands under the marshaled heat here too, and a **Restarted** heat's pre-restart
/// rulings are absent (the window folds from the heat's current run by design — an abandoned
/// run's rulings are not part of the heat's result, so they are not part of its audit either).
pub(crate) fn event_audit_log(stored: &[StoredEvent]) -> Vec<EventAuditEntry> {
    let events: Vec<Event> = stored.iter().map(|s| s.event.clone()).collect();
    // The distinct heats ever scheduled, in first-scheduled order (a re-schedule of the same id
    // must not fold — and double-report — the same window twice).
    let mut heats: Vec<HeatId> = Vec::new();
    for event in &events {
        if let Event::HeatScheduled { heat, .. } = event {
            if !heats.contains(heat) {
                heats.push(heat.clone());
            }
        }
    }
    let mut entries: Vec<EventAuditEntry> = Vec::new();
    for heat in &heats {
        let offsets = heat_window_offsets(&events, heat);
        let trail = marshaling_log(
            offsets
                .iter()
                .map(|(o, e)| (stored.get(*o as usize).and_then(|s| s.recorded_at), *o, e)),
            heat,
        );
        entries.extend(trail.into_iter().map(|entry| EventAuditEntry {
            heat: heat.clone(),
            entry,
        }));
    }
    // Newest first across the whole event: the global append offset is the one total order every
    // heat's entries share (recorded_at can be absent), so descending offset is descending time.
    entries.sort_by_key(|e| std::cmp::Reverse(e.entry.at_ref));
    entries
}

/// `GET /events/{event_id}/audit` — the **event-wide** audit trail (the "defensible results"
/// review surface).
///
/// Serves every heat's marshaling audit fold, heat-tagged and merged newest-first (see
/// [`event_audit_log`]). This is what the console's Audit page reads: the full searchable ruling
/// history for the event, while Marshaling keeps only the marshaled heat's recent entries. An
/// open read (no token), like the heats list and the snapshot routes; an unknown event is a
/// typed **404**.
async fn event_audit(
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
) -> Result<Json<Vec<EventAuditEntry>>, ProtocolError> {
    let state = resolve_event(&registry, &event_id)?;
    let (stored, _cursor) = state.read_stored()?;
    Ok(Json(event_audit_log(&stored)))
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
    // A pure fold of the log — every format, open practice included (D5, reversed 2026-08-24):
    // practice laps are ordinary `Pass` events, so there is no overlay to splice.
    // `with_heat_timing` folds the current heat's server-authoritative race-start/end instants
    // (#62 follow-up) from the stored log's `recorded_at` so the clock is consistent everywhere.
    //
    // The D26 min-lap floor is resolved from registry meta for the heat this fold reports as
    // current (#409). It is NOT in the log, so a pure-log fold cannot see it — and without it the
    // event scope counted an echo pass the heat scope's lap list suppressed.
    let rounds = registry.rounds_of(&event_id).unwrap_or_default();
    let floor = live_fold_floor(&events, &rounds);
    let body = with_heat_timing(live_state_with_floor(&events, floor), &stored);
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
    let class_offsets = class_window_offsets(&events, &class);
    // The window's `current_heat` resolves which heat is on the timer; its timing is folded
    // from the *full* stored log (the heat's transition instants live there with `recorded_at`).
    //
    // The D26 floor is resolved over the WINDOW, not the whole log (#409): the class fold picks
    // its current heat from the filtered slice, so that is the heat whose round owns the floor.
    let window_events: Vec<Event> = class_offsets.iter().map(|(_, e)| e.clone()).collect();
    let rounds = registry.rounds_of(&event_id).unwrap_or_default();
    let floor = live_fold_floor(&window_events, &rounds);
    Ok(Json(Snapshot {
        cursor,
        body: ProjectionBody::LiveRaceState(with_heat_timing(
            live_state_over_with_floor(&class_offsets, floor),
            &stored,
        )),
    }))
}

/// Filter the log to a single **class's** heats (race redesign Slice 5/6a): every event that
/// belongs to a heat tagged `HeatScheduled { class: Some(class), .. }`, plus the passes and
/// marshaling adjudications that fall *while one of the class's heats is the active one*.
///
/// The class's heats are first collected from the `HeatScheduled` tags; then the log is replayed
/// once, opening the window on any heat-loop event for one of those heats and closing it on a
/// heat-loop event for a heat *not* in the class — the same position-based pass attribution
/// [`heat_window_offsets`] uses to scope a single heat, generalized to a set of heats. So a
/// class's live state folds only its own heats and passes, with no other class's racing bleeding
/// in. Carries each event's GLOBAL append offset — the class-scope live fold
/// feeds these to [`live_state_over_with_floor`] so marshaling adjudications (global `LogRef` targets)
/// resolve inside the filtered view (the same #55 rule as `heat_window_offsets`).
pub(crate) fn class_window_offsets(events: &[Event], class: &ClassId) -> Vec<(u64, Event)> {
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
    for (offset, event) in events.iter().enumerate() {
        let offset = offset as u64;
        match event {
            Event::HeatScheduled { heat, .. } | Event::HeatStateChanged { heat, .. } => {
                active = class_heats.contains(heat);
                if active {
                    window.push((offset, event.clone()));
                }
            }
            // A tagged pass belongs to its stamped heat — in or out by class membership,
            // independent of the positional cursor (same rule as `heat_window_offsets`),
            // and frozen out once that heat's run is official (the Final freeze).
            Event::Pass(p) if p.heat.is_some() => {
                if p.heat.as_ref().is_some_and(|h| {
                    class_heats.contains(h)
                        && offset < crate::live_state::current_run_pass_ceiling(events, h) as u64
                }) {
                    window.push((offset, event.clone()));
                }
            }
            // Untagged passes and adjudications belong to whichever heat is currently active.
            _ if active => window.push((offset, event.clone())),
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

    // The heat's ROUND config, resolved once for every projection that scores or folds laps:
    // the win condition (#45) and the min-lap floor (D26 — the floor must reach the laps,
    // live, and result folds identically, or the lap list and the score disagree about a
    // suppressed pass). A heat with no round (ad-hoc) keeps the neutral defaults.
    // Through the SHARED resolver every live surface uses (`round_def_of_heat` /
    // `live_fold_floor`, #409), so the heat scope and the event/class scopes can never resolve
    // a different round — or a different floor — for the same heat.
    let rounds = registry.rounds_of(&event_id).unwrap_or_default();
    let round_def = round_def_of_heat(&events, &heat, &rounds);
    let min_lap_micros = min_lap_micros_of(round_def.as_ref());

    let body = match query.projection {
        HeatProjection::Live => {
            // A pure fold of the heat's log window — every format, open practice included (D5,
            // reversed 2026-08-24): practice passes are logged like anyone else's, no overlay.
            ProjectionBody::LiveRaceState(with_heat_timing(
                live_state_over_with_floor(&heat_offsets, min_lap_micros),
                &stored,
            ))
        }
        HeatProjection::Laps => ProjectionBody::LapList(lap_list_marshaled_with_floor(
            heat_offsets.iter().map(|(o, e)| (*o, e)),
            min_lap_micros,
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
            // Open practice is EXCLUDED from results (`crate::open_practice`) — the one and only
            // way a practice heat differs from any other. Its laps ARE on the log (and its lap
            // list, live state and audit trail all read them); they just never score a placement,
            // so this projection is empty rather than a ranked board nobody should read.
            if crate::open_practice::heat_excluded_from_scoring(round_def.as_ref()) {
                return Ok(Json(Snapshot {
                    cursor,
                    body: ProjectionBody::HeatResult(Default::default()),
                }));
            }
            // Score under the heat's ROUND win condition (#45), mirroring
            // `round_engine::completed_heats`: resolve the heat's round from its
            // `HeatScheduled` tag, then look its `RoundDef::win_condition` up in the event
            // meta. A heat with no associated round (an ad-hoc / sim heat) falls back to a
            // neutral best-lap qualifying rule, so an un-tagged heat is unchanged.
            let win_condition = round_def
                .as_ref()
                .map(|r| r.win_condition)
                .unwrap_or(WinCondition::BestLap);
            // Score over the heat's FULL adjudicated window via the one shared helper the
            // round/class standings also use ([`round_engine::completed_heats`] →
            // [`score_heat_window`]), so the per-heat result and the standings can never
            // disagree on an adjudicated heat (#226). The helper preserves the window's global
            // offsets so a `RulingReversed` / `LapThrownOut` resolves to its true `LogRef` (#55).
            ProjectionBody::HeatResult(score_heat_window(
                &events,
                &heat,
                win_condition,
                min_lap_micros,
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

    // The pilot view folds the WHOLE log (every heat, every round) — rounds carry different
    // min-lap floors, so no single floor applies here; the per-heat views are the floored,
    // authoritative surfaces (D26). A pilot may therefore see a raw echo here that the RD's
    // heat view suppresses — read-only, never scored.
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

/// Filter the log to a single heat's window, **preserving each event's global append offset**.
///
/// Three routing rules, by what the event itself can say about where it belongs:
///
/// - **Heat-tagged marshaling events** (`PenaltyApplied` / `HeatVoided` / `ProtestFiled` /
///   a tagged `LapInserted`) route **by their tag** — a ruling about a finished heat filed
///   while a later heat is live must land in the marshaled heat's window and never leak into
///   the live one. Positional attribution here was the mis-windowing bug: the ruling was
///   silently a no-op on its target heat AND disqualified/penalized pilots in whichever heat
///   happened to be active.
/// - **Target-carrying rulings** (`DetectionVoided` / `LapAdjusted` / `LapSplit` /
///   `LapThrownOut` / `ProtestResolved` / `RulingReversed`) belong to whichever heat their
///   **target offset** is in. Targets always reference an earlier append offset, so one
///   forward scan with an incrementally-built membership set resolves chains ("reverse the
///   ruling that voided the pass…") without a fixpoint pass.
/// - **Untagged events** (raw `Pass`es — wire observations that carry no heat id — plus a
///   legacy untagged `LapInserted`, registrations, …) attribute **positionally**: they belong
///   to whichever heat is active at that point in the log, the same ordering the engine uses
///   to decide which heat consumes a pass (race-engine.html §2).
///
/// The retained **global offset** is load-bearing for marshaling (#55): the lap projection and
/// audit fold are keyed on it, and a `LogRef` correction command targets that global offset. An
/// earlier bug re-enumerated the window `0,1,2,…`, so a UI-selected lap in a later heat targeted
/// the *wrong* pass; folding with the real offsets fixes that.
/// The window additionally folds from the heat's **current run**
/// ([`current_run_start`](crate::live_state::current_run_start) — the latest `Running`, or one
/// past the latest `Aborted`/`Restarted`): everything except the heat-loop events themselves
/// must sit at/after that boundary. A reset abandons the prior run, so its passes — and any
/// rulings made about them — are not part of this heat's result, the same rule the live
/// standings already applied. Without it, a Restarted-and-re-raced heat scored BOTH runs'
/// passes (the ghost run even out-ranked the real one). Heat-loop events stay un-filtered:
/// they carry the lineup and the FSM lineage the folds need.
pub(crate) fn heat_window_offsets(events: &[Event], heat: &HeatId) -> Vec<(u64, Event)> {
    let run_start = crate::live_state::current_run_start(events, heat) as u64;
    let pass_ceiling = crate::live_state::current_run_pass_ceiling(events, heat) as u64;
    let mut window = Vec::new();
    // The offsets already claimed by this window — a target-carrying ruling joins iff its
    // target is one of them (targets always point backwards, so one forward scan suffices).
    let mut claimed: BTreeSet<u64> = BTreeSet::new();
    // `active` tracks whether the cursor is currently inside this heat's span: it opens on
    // a heat-loop event for `heat` and closes on a heat-loop event for a *different* heat.
    let mut active = false;
    for (offset, event) in events.iter().enumerate() {
        let offset = offset as u64;
        let include = match event {
            Event::HeatScheduled { heat: h, .. } | Event::HeatStateChanged { heat: h, .. } => {
                active = h == heat;
                active
            }
            // Heat-tagged marshaling events: by tag, never by position.
            Event::HeatVoided { heat: h }
            | Event::PenaltyApplied { heat: h, .. }
            | Event::ProtestFiled { heat: h, .. } => h == heat && offset >= run_start,
            Event::LapInserted { heat: Some(h), .. } => h == heat && offset >= run_start,
            // Target-carrying rulings: by their target's membership (a ruling targeting an
            // abandoned run's pass drops out with its target).
            Event::DetectionVoided { target }
            | Event::LapAdjusted { target, .. }
            | Event::LapSplit { target, .. }
            | Event::LapThrownOut { target }
            | Event::ProtestResolved { target, .. }
            | Event::RulingReversed { target } => {
                claimed.contains(&target.0) && offset >= run_start
            }
            // Passes: by their bridge-stamped heat TAG when present (robust against a
            // heat-span event closing the positional span mid-race — the scheduling-eats-laps
            // bug); an untagged (legacy) pass keeps the positional rule. Either way, a pass
            // landing at/after the run's `Finalized` is FROZEN OUT: once a result is official
            // it cannot shift under a delayed catch-up pass with no command and no audit
            // entry (rulings are unaffected — a Revert re-opens marshaling, not the record).
            Event::Pass(p) => {
                let before_official = offset < pass_ceiling;
                match &p.heat {
                    Some(h) => h == heat && offset >= run_start && before_official,
                    None => active && offset >= run_start && before_official,
                }
            }
            // Untagged (legacy insertions, registrations, …): positional.
            _ => active && offset >= run_start,
        };
        if include {
            claimed.insert(offset);
            window.push((offset, event.clone()));
        }
    }
    window
}

/// Score a single heat over its **full adjudicated event window** under `win_condition` — the
/// one scoring path shared by the per-heat result projection ([`HeatProjection::Result`]) and
/// the round / class standings ([`round_engine::completed_heats`]), so the heat page and the
/// standings can never disagree on an adjudicated heat (#226).
///
/// Before this existed, the standings path scored each heat over a **pass-only** list (every
/// marshaling adjudication — DQ / time / throw-out / void / lap-edit — discarded), so a heat's
/// standings showed the raw on-track result while its heat page showed the adjudicated one. This
/// closes that split-brain by giving both call sites the *same* window + scorer.
///
/// Windows the log to the heat via [`heat_window_offsets`] (the heat's passes **and** every
/// adjudication that falls while the heat is active), **preserving each event's global append
/// offset** so a [`RulingReversed`](gridfpv_events::Event::RulingReversed) /
/// [`LapThrownOut`](gridfpv_events::Event::LapThrownOut) resolves to its true `LogRef` target —
/// a re-enumerated window would match the wrong offset (#55). The race clock is the window's
/// earliest lap-gate pass. Scores via [`score_corrected_with_global_offsets`] under the round's win
/// condition, so penalties / throw-outs / voids / lap-edits all land.
pub(crate) fn score_heat_window(
    events: &[Event],
    heat: &HeatId,
    win_condition: WinCondition,
    min_lap_micros: Option<i64>,
) -> HeatResult {
    let heat_offsets = heat_window_offsets(events, heat);
    // Fold the marshaling lap corrections (void / insert / adjust / split) into the pass
    // stream FIRST — scoring raw passes here was the residual #226 split-brain: the marshaling
    // lap list showed the corrected laps while the result, rankings, standings, and seeding
    // scored the uncorrected ones. The corrected stream keeps each surviving pass's global
    // offset, so a throw-out targeting a lap's end pass still excludes the right lap. The
    // round's min-lap floor (D26) applies here too — the score and the lap list must agree.
    let corrected = gridfpv_projection::corrected_passes_with_floor(
        heat_offsets.iter().map(|(o, e)| (*o, e)),
        min_lap_micros,
    );
    let race_start = corrected
        .iter()
        .filter(|(_, p)| p.gate.is_lap_gate())
        .map(|(_, p)| p.at)
        .min()
        .unwrap_or(SourceTime::from_micros(0));
    score_corrected_with_global_offsets(
        &corrected,
        win_condition,
        race_start,
        heat_offsets.iter().map(|(o, e)| (*o, e)),
    )
}

/// A round's min-lap floor in MICROSECONDS (D26), `None` when unset/zero — the single
/// conversion every fold call site shares.
pub(crate) fn min_lap_micros_of(round: Option<&crate::events::RoundDef>) -> Option<i64> {
    round
        .and_then(|r| r.min_lap_secs)
        .filter(|s| *s > 0)
        .map(|s| s as i64 * 1_000_000)
}

/// The [`RoundDef`](crate::events::RoundDef) a heat was scheduled under, resolved against
/// `rounds` (the event's CURRENT registry meta) — `None` for an untagged / ad-hoc heat, or a
/// round that has since been removed.
///
/// The round tag is read off the log with [`round_of_heat`] (the heat's *latest*
/// `HeatScheduled`, so a re-materialized heat resolves against the schedule that stands), and
/// the config is read from registry meta, which is where `min_lap_secs` and the win condition
/// live. One helper so every scope resolves "which round is this heat's" identically.
pub(crate) fn round_def_of_heat(
    events: &[Event],
    heat: &HeatId,
    rounds: &[crate::events::RoundDef],
) -> Option<crate::events::RoundDef> {
    let round_id = crate::live_state::round_of_heat(events, heat)?;
    rounds.iter().find(|r| r.id == round_id).cloned()
}

/// The **min-lap floor** (D26) that applies to a live fold over `events` — the floor of the
/// round owning the heat that fold will report as current.
///
/// This is the one resolver every live surface goes through: the event- and class-scope
/// snapshots, and the change stream's per-prefix fold (#409). It deliberately takes the *same*
/// event slice the fold consumes — for the class scope that is the class's filtered window, not
/// the whole log — because [`live_state_core`](crate::live_state) picks its current heat from
/// that slice, and a floor resolved against a different heat is worse than no floor at all.
///
/// A heat with no round, or a round with no `min_lap_secs`, yields `None` — D26's "0/absent =
/// off, so pre-existing rounds keep bit-identical results".
pub(crate) fn live_fold_floor(events: &[Event], rounds: &[crate::events::RoundDef]) -> Option<i64> {
    let heat = crate::live_state::current_heat(events)?;
    min_lap_micros_of(round_def_of_heat(events, &heat, rounds).as_ref())
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
    use crate::events::{MemberSlot, NewRoundReq, SeedingRule};
    use gridfpv_engine::scoring::Metric;
    use gridfpv_events::{AdapterId, GateIndex, HeatTransition, LogRef, Pass};
    use gridfpv_projection::CompetitorKey;
    use http_body_util::BodyExt;
    use serde_json::json;
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
            heat: None,
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
                label: None,
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

    /// D26: the min-lap floor reaches SCORING through `score_heat_window` — an echo pass
    /// that closes an under-floor lap is suppressed from the scored chain, so the result and
    /// the (floored) lap list agree: the 0.004s phantom can never be anyone's best lap.
    #[test]
    fn score_heat_window_applies_the_min_lap_floor() {
        let mut events = recorded_heat(); // A: passes at 1.0s / 4.0s / 6.5s (laps 3.0s, 2.5s)
        // A double-detection echo 4ms after A's second pass — inserted DURING the run
        // (appending it after `Finalized` would hit the Final freeze instead, which this
        // test's own control run would then be measuring).
        let finished_at = events
            .iter()
            .position(|e| {
                matches!(
                    e,
                    Event::HeatStateChanged {
                        transition: HeatTransition::Finished,
                        ..
                    }
                )
            })
            .unwrap();
        events.insert(finished_at, pass("A", 4_004_000, 9));
        let heat = HeatId("q-1".into());

        let unfloored = score_heat_window(&events, &heat, WinCondition::BestLap, None);
        let a = unfloored
            .places
            .iter()
            .find(|p| p.competitor.competitor.0 == "A")
            .unwrap();
        assert_eq!(
            a.metric,
            gridfpv_engine::scoring::Metric::BestLapMicros(Some(4_000)),
            "without the floor the echo IS the (phantom) best lap"
        );

        let floored = score_heat_window(&events, &heat, WinCondition::BestLap, Some(1_000_000));
        let a = floored
            .places
            .iter()
            .find(|p| p.competitor.competitor.0 == "A")
            .unwrap();
        assert_eq!(
            a.metric,
            gridfpv_engine::scoring::Metric::BestLapMicros(Some(2_500_000)),
            "the floor suppresses the echo; the real 2.5s lap wins"
        );
    }

    /// The FINAL FREEZE: a pass landing AFTER a heat's run went official never joins its
    /// window — a delayed RotorHazard catch-up pass (tagged or untagged) used to silently
    /// change a Final result with no command, no ruling, and no audit entry.
    #[test]
    fn a_pass_landing_after_finalized_never_joins_the_official_record() {
        let heat = HeatId("q-1".into());
        let mut events = recorded_heat();
        let window_before = heat_window_offsets(&events, &heat);
        let passes_before = window_before
            .iter()
            .filter(|(_, e)| matches!(e, Event::Pass(_)))
            .count();
        assert_eq!(passes_before, 5, "the run's real passes all count");

        // A late catch-up pass TAGGED with the (now Final) heat…
        let mut late = Pass {
            adapter: AdapterId("vd".into()),
            competitor: CompetitorRef("A".into()),
            at: SourceTime::from_micros(9_000_000),
            sequence: Some(9),
            gate: GateIndex::LAP,
            signal: None,
            heat: Some(heat.clone()),
        };
        events.push(Event::Pass(late.clone()));
        // …and an UNTAGGED one (legacy positional attribution would claim it too).
        late.heat = None;
        late.sequence = Some(10);
        events.push(Event::Pass(late));

        let window_after = heat_window_offsets(&events, &heat);
        let passes_after = window_after
            .iter()
            .filter(|(_, e)| matches!(e, Event::Pass(_)))
            .count();
        assert_eq!(
            passes_after, passes_before,
            "the official record is frozen — late passes stay out"
        );

        // Rulings are NOT frozen (Revert re-opens marshaling): a void targeting a real run
        // pass still joins the window.
        events.push(Event::DetectionVoided { target: LogRef(6) });
        let with_ruling = heat_window_offsets(&events, &heat);
        assert!(
            with_ruling
                .iter()
                .any(|(_, e)| matches!(e, Event::DetectionVoided { .. })),
            "rulings keep flowing into the window"
        );
    }

    /// A registry whose single created event carries a round with `win_condition` and a heat
    /// `q-1` tagged with that round, driven Scheduled → Final over the given lap-gate `passes`.
    /// Used to prove the result projection scores under the heat's round win condition (#45).
    fn registry_with_round_heat(
        win_condition: WinCondition,
        time_limit_secs: Option<u32>,
        passes: Vec<Event>,
    ) -> EventRegistry {
        let registry = new_registry();
        let event_id = sole_event(&registry);
        let round = registry
            .add_round(
                &event_id,
                NewRoundReq {
                    layouts: Vec::new(),
                    label: "Race".into(),
                    classes: vec![],
                    format: "timed_qual".into(),
                    params: std::collections::BTreeMap::new(),
                    win_condition: Some(win_condition),
                    seeding: SeedingRule::FromRoster,
                    time_limit_secs,
                    channel_mode: None,
                    staging_timer_secs: None,
                    start_procedure: None,
                    grace_window: None,
                    protest_window: None,
                    min_lap_secs: None,
                },
            )
            .expect("round adds (empty classes validate)");

        let heat = || HeatId("q-1".into());
        let changed = |t| Event::HeatStateChanged {
            heat: heat(),
            transition: t,
        };
        let mut events = vec![
            Event::HeatScheduled {
                heat: heat(),
                lineup: vec![CompetitorRef("A".into()), CompetitorRef("B".into())],
                class: None,
                round: Some(round.id.clone()),
                frequencies: vec![],
                label: None,
            },
            changed(HeatTransition::Staged),
            changed(HeatTransition::Armed),
            changed(HeatTransition::Running),
        ];
        events.extend(passes);
        events.push(changed(HeatTransition::Finished));
        events.push(changed(HeatTransition::Finalized));

        let state = registry.resolve(&event_id).expect("the created event");
        for e in &events {
            state.append(e.clone(), None).unwrap();
        }
        registry
    }

    use crate::events::{CreateEventRequest, EventRegistry};

    // There is no built-in event any more (#414), so every test that drives a per-event route
    // **creates** one through the real creation path and roots its URIs under that event's id.
    // `event_uri` builds those paths, so a test writes only the part after `/events/{id}`.

    /// A fresh registry holding exactly one created event — the fixture that replaced the
    /// built-in Practice event. Going through `create` means the tests exercise the same path
    /// the RD's first-run "create your first event" does.
    fn new_registry() -> EventRegistry {
        let registry = EventRegistry::new(None).unwrap();
        registry
            .create(&CreateEventRequest::named("Test Event"))
            .expect("create the test event");
        registry
    }

    /// The id of a test registry's single event.
    fn sole_event(registry: &EventRegistry) -> EventId {
        let mut list = registry.list();
        assert_eq!(
            list.len(),
            1,
            "this helper is for a registry holding exactly one created event"
        );
        list.remove(0).id
    }

    /// `/events/{id}` + `path` for the registry's single event — the per-event route prefix the
    /// tests drive.
    fn event_uri(registry: &EventRegistry, path: &str) -> String {
        format!("/events/{}{}", sole_event(registry).0, path)
    }

    /// Build a registry whose single created event's log already holds `events`, returning the
    /// registry (the router state), that event's [`AppState`] (for token minting in tests), and
    /// the log length.
    fn state_with(events: Vec<Event>) -> (EventRegistry, AppState, u64) {
        let registry = new_registry();
        let state = registry
            .resolve(&sole_event(&registry))
            .expect("the created event");
        for e in &events {
            state.append(e.clone(), None).unwrap();
        }
        let len = events.len() as u64;
        (registry, state, len)
    }

    /// `GET` the snapshot route at `path` (relative to the registry's single event root).
    async fn get_snapshot(registry: EventRegistry, path: &str) -> (StatusCode, Option<Snapshot>) {
        let uri = event_uri(&registry, path);
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
        let (status, snap) = get_snapshot(registry, "/snapshot/event/spring-cup").await;
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
        let (status, snap) = get_snapshot(registry, "/snapshot/heat/q-1").await;
        assert_eq!(status, StatusCode::OK);
        let snap = snap.unwrap();
        assert_eq!(snap.cursor, Cursor::new(len));
        assert!(matches!(snap.body, ProjectionBody::LiveRaceState(_)));
    }

    /// Regression for #249: a `randomized-delay` round arms a heat and the start driver appends
    /// `HeatStarting { delay_ms }` with **no caller timestamp**. The append choke point must stamp
    /// its server `recorded_at` (like a heat transition) so `heat_tone_at` can anchor the Armed-phase
    /// tone countdown to it. Before the fix `HeatStarting` was untimed, `tone_at` was always `None`,
    /// and the RD's "Tone in S.s" countdown never showed. Here we drive a heat Scheduled → Staged →
    /// Armed, append an *untimed* `HeatStarting`, and assert the heat-scope live state surfaces a
    /// `tone_at` in the future (now + the logged delay) while `Armed`.
    #[tokio::test]
    async fn armed_heat_surfaces_tone_at_from_an_untimed_heat_starting() {
        let heat = || HeatId("q-1".into());
        let changed = |t| Event::HeatStateChanged {
            heat: heat(),
            transition: t,
        };
        let (registry, state, _) = state_with(vec![
            Event::HeatScheduled {
                heat: heat(),
                lineup: vec![CompetitorRef("A".into()), CompetitorRef("B".into())],
                class: None,
                round: None,
                frequencies: vec![],
                label: None,
            },
            changed(HeatTransition::Staged),
            changed(HeatTransition::Armed),
        ]);

        // The start driver's append: an untimed `HeatStarting`. The choke point must stamp it.
        let before = now_micros();
        let delay_ms: u32 = 3000;
        state
            .append(
                Event::HeatStarting {
                    heat: heat(),
                    delay_ms,
                },
                None,
            )
            .unwrap();
        let after = now_micros();

        // The stored entry carries a server `recorded_at` (no longer an untimed entry).
        let (stored, _) = state.read_stored().unwrap();
        let starting = stored
            .iter()
            .find(|s| matches!(&s.event, Event::HeatStarting { .. }))
            .expect("the HeatStarting was appended");
        let armed_at = starting
            .recorded_at
            .expect("the append choke point stamped HeatStarting's recorded_at (#249)");
        assert!(before <= armed_at && armed_at <= after);

        // The heat-scope live state surfaces the tone instant while Armed: armed_at + delay.
        let (status, snap) = get_snapshot(registry, "/snapshot/heat/q-1").await;
        assert_eq!(status, StatusCode::OK);
        match snap.unwrap().body {
            ProjectionBody::LiveRaceState(live) => {
                assert_eq!(live.phase, HeatPhase::Armed);
                assert_eq!(live.tone_at, Some(armed_at + i64::from(delay_ms) * 1_000));
            }
            other => panic!("expected live race state, got {other:?}"),
        }
    }

    /// Once the heat is `Running`, the tone has fired: `tone_at` clears (the countdown ends and
    /// `race_started_at` takes over). Guards the Armed-only gating end-to-end through the snapshot.
    #[tokio::test]
    async fn running_heat_clears_tone_at() {
        let heat = || HeatId("q-1".into());
        let changed = |t| Event::HeatStateChanged {
            heat: heat(),
            transition: t,
        };
        let (registry, state, _) = state_with(vec![
            Event::HeatScheduled {
                heat: heat(),
                lineup: vec![CompetitorRef("A".into()), CompetitorRef("B".into())],
                class: None,
                round: None,
                frequencies: vec![],
                label: None,
            },
            changed(HeatTransition::Staged),
            changed(HeatTransition::Armed),
        ]);
        state
            .append(
                Event::HeatStarting {
                    heat: heat(),
                    delay_ms: 3000,
                },
                None,
            )
            .unwrap();
        // The runtime's auto Armed → Running (the tone fired).
        state
            .append(changed(HeatTransition::Running), None)
            .unwrap();

        let (status, snap) = get_snapshot(registry, "/snapshot/heat/q-1").await;
        assert_eq!(status, StatusCode::OK);
        match snap.unwrap().body {
            ProjectionBody::LiveRaceState(live) => {
                assert_eq!(live.phase, HeatPhase::Running);
                assert_eq!(live.tone_at, None);
                assert!(live.race_started_at.is_some());
            }
            other => panic!("expected live race state, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn heat_scope_laps_projection_returns_lap_list() {
        let (registry, _state, _) = state_with(recorded_heat());
        let (status, snap) = get_snapshot(registry, "/snapshot/heat/q-1?projection=laps").await;
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
        let (status, snap) = get_snapshot(registry, "/snapshot/heat/q-1?projection=result").await;
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
    async fn heat_scope_result_uses_round_first_to_laps_win_condition() {
        // A completes three laps earlier than B, but B holds the single fastest lap (0.5s). Under
        // the round's First-to-3 win condition the order is by who reached lap 3 first (A), and the
        // placement metric is `ReachedAt` — NOT the hardcoded best-lap placeholder (#45) that would
        // have ordered B first by best lap.
        let passes = vec![
            pass("A", 0, 1),
            pass("B", 0, 1),
            pass("A", 1_000_000, 2), // A lap 1 = 1.0s
            pass("B", 500_000, 2),   // B lap 1 = 0.5s (the single fastest lap)
            pass("A", 2_000_000, 3), // A lap 2 = 1.0s
            pass("A", 3_000_000, 4), // A reaches lap 3 at t = 3.0s
            pass("B", 3_500_000, 3), // B lap 2 = 3.0s
            pass("B", 4_000_000, 4), // B reaches lap 3 at t = 4.0s
        ];
        let registry = registry_with_round_heat(WinCondition::FirstToLaps { n: 3 }, None, passes);
        let (status, snap) = get_snapshot(registry, "/snapshot/heat/q-1?projection=result").await;
        assert_eq!(status, StatusCode::OK);
        match snap.unwrap().body {
            ProjectionBody::HeatResult(result) => {
                assert_eq!(result.places.len(), 2);
                // A first: reached lap 3 at 3.0s (before B's 4.0s) — by the race, not best lap.
                let first = &result.places[0];
                assert_eq!(first.competitor.competitor, CompetitorRef("A".into()));
                assert_eq!(first.position, 1);
                assert_eq!(
                    first.metric,
                    Metric::ReachedAt(Some(SourceTime::from_micros(3_000_000)))
                );
                let second = &result.places[1];
                assert_eq!(second.competitor.competitor, CompetitorRef("B".into()));
                assert_eq!(
                    second.metric,
                    Metric::ReachedAt(Some(SourceTime::from_micros(4_000_000)))
                );
            }
            other => panic!("expected heat result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn heat_scope_result_uses_round_best_consecutive_win_condition() {
        // A best-2-consecutive round: the placement metric is `BestConsecutiveMicros`, ranking by
        // the smallest 2-lap window (A's 2.0s beats B's 6.0s).
        let passes = vec![
            pass("A", 0, 1),
            pass("A", 1_000_000, 2), // 1.0s
            pass("A", 2_000_000, 3), // 1.0s → best 2-consec = 2.0s
            pass("B", 0, 1),
            pass("B", 3_000_000, 2), // 3.0s
            pass("B", 6_000_000, 3), // 3.0s → best 2-consec = 6.0s
        ];
        let registry =
            registry_with_round_heat(WinCondition::BestConsecutive { n: 2 }, Some(60), passes);
        let (status, snap) = get_snapshot(registry, "/snapshot/heat/q-1?projection=result").await;
        assert_eq!(status, StatusCode::OK);
        match snap.unwrap().body {
            ProjectionBody::HeatResult(result) => {
                let first = &result.places[0];
                assert_eq!(first.competitor.competitor, CompetitorRef("A".into()));
                assert_eq!(first.position, 1);
                assert_eq!(first.metric, Metric::BestConsecutiveMicros(Some(2_000_000)));
            }
            other => panic!("expected heat result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn heat_scope_result_no_round_falls_back_to_best_lap() {
        // `recorded_heat`'s q-1 carries `round: None` (an ad-hoc heat with no round), so the result
        // still scores under the best-lap fallback — the placement metric is `BestLapMicros`, so the
        // un-tagged heat's behaviour is unchanged. A's fastest lap (2.5s) beats B's (4.0s).
        let (registry, _state, _) = state_with(recorded_heat());
        let (status, snap) = get_snapshot(registry, "/snapshot/heat/q-1?projection=result").await;
        assert_eq!(status, StatusCode::OK);
        match snap.unwrap().body {
            ProjectionBody::HeatResult(result) => {
                let first = &result.places[0];
                assert_eq!(first.competitor.competitor, CompetitorRef("A".into()));
                assert_eq!(first.metric, Metric::BestLapMicros(Some(2_500_000)));
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
        let (status, snap) = get_snapshot(registry, "/snapshot/heat/q-1?projection=audit").await;
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

    // --- The event-wide audit read (`GET /events/{event_id}/audit`) -----------------------------

    /// `GET /events/{id}/audit` for the registry's single event, deserialized. The route serves
    /// plain `Vec<EventAuditEntry>` (no snapshot envelope — a directory-style read like `/heats`).
    async fn get_event_audit(registry: EventRegistry) -> (StatusCode, Vec<EventAuditEntry>) {
        let uri = event_uri(&registry, "/audit");
        let response = router(registry)
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let entries = serde_json::from_slice::<Vec<EventAuditEntry>>(&bytes).unwrap_or_default();
        (status, entries)
    }

    /// The heat-loop events for a second heat `q-2` (C and D), appended after `recorded_heat`'s
    /// `q-1`. With `recorded_heat` first (offsets 0..=10), these land at offsets 11..=18 — the
    /// two passes at 15 and 16.
    fn second_heat() -> Vec<Event> {
        let changed = |t| Event::HeatStateChanged {
            heat: HeatId("q-2".into()),
            transition: t,
        };
        vec![
            Event::HeatScheduled {
                heat: HeatId("q-2".into()),
                lineup: vec![CompetitorRef("C".into()), CompetitorRef("D".into())],
                class: None,
                round: None,
                frequencies: vec![],
                label: None,
            },
            changed(HeatTransition::Staged),
            changed(HeatTransition::Armed),
            changed(HeatTransition::Running),
            pass("C", 1_000_000, 1),
            pass("C", 4_000_000, 2),
            changed(HeatTransition::Finished),
            changed(HeatTransition::Finalized),
        ]
    }

    #[tokio::test]
    async fn event_audit_merges_heats_newest_first_with_correct_heat_tags() {
        // Two heats run back-to-back, then two rulings appended AFTER both have run: first a void
        // targeting q-2's second pass (offset 16 → window-attributed to q-2), then a penalty
        // heat-tagged to q-1 — the FIRST heat, filed while it is long finished. The tag (not the
        // position in the log) must decide the heat it lands under.
        let mut events = recorded_heat(); // q-1 at offsets 0..=10
        events.extend(second_heat()); // q-2 at offsets 11..=18
        events.push(Event::DetectionVoided {
            target: LogRef(16), // q-2's second pass → belongs to q-2
        });
        events.push(Event::PenaltyApplied {
            heat: HeatId("q-1".into()),
            competitor: CompetitorRef("A".into()),
            penalty: gridfpv_events::Penalty::Disqualify { reason: None },
        });
        let (registry, _state, _) = state_with(events);

        let (status, entries) = get_event_audit(registry).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(entries.len(), 2, "two rulings, no passes");
        // Newest first: the penalty (offset 20) precedes the void (offset 19)…
        assert_eq!(entries[0].entry.at_ref, LogRef(20));
        assert_eq!(
            entries[0].entry.kind,
            gridfpv_projection::AuditKind::PenaltyApplied
        );
        // …and it is tagged to q-1 (the heat it names), NOT q-2 (the heat last active in the log).
        assert_eq!(entries[0].heat, HeatId("q-1".into()));
        assert_eq!(entries[1].entry.at_ref, LogRef(19));
        assert_eq!(entries[1].entry.kind, gridfpv_projection::AuditKind::Voided);
        assert_eq!(entries[1].heat, HeatId("q-2".into()));
    }

    #[tokio::test]
    async fn event_audit_omits_a_restarted_heats_pre_restart_rulings() {
        // A ruling made during q-1's FIRST run, then the heat is Restarted and re-raced. The heat
        // window folds from the current run only (a reset abandons the prior run and everything
        // ruled about it), so the pre-restart penalty must NOT appear in the event audit — while a
        // post-restart ruling does.
        let heat = || HeatId("q-1".into());
        let changed = |t| Event::HeatStateChanged {
            heat: heat(),
            transition: t,
        };
        let events = vec![
            Event::HeatScheduled {
                heat: heat(),
                lineup: vec![CompetitorRef("A".into()), CompetitorRef("B".into())],
                class: None,
                round: None,
                frequencies: vec![],
                label: None,
            },
            changed(HeatTransition::Staged),
            changed(HeatTransition::Armed),
            changed(HeatTransition::Running),
            pass("A", 1_000_000, 1),
            // The abandoned run's ruling (offset 5): a penalty filed mid-first-run.
            Event::PenaltyApplied {
                heat: heat(),
                competitor: CompetitorRef("A".into()),
                penalty: gridfpv_events::Penalty::Disqualify { reason: None },
            },
            changed(HeatTransition::Restarted), // offset 6 — abandons the run above
            changed(HeatTransition::Staged),
            changed(HeatTransition::Armed),
            changed(HeatTransition::Running),
            pass("A", 1_000_000, 2),
            pass("A", 4_000_000, 3),
            changed(HeatTransition::Finished),
            changed(HeatTransition::Finalized),
            // The current run's ruling (offset 14): survives.
            Event::PenaltyApplied {
                heat: heat(),
                competitor: CompetitorRef("B".into()),
                penalty: gridfpv_events::Penalty::TimeAdded { micros: 2_000_000 },
            },
        ];
        let (registry, _state, _) = state_with(events);

        let (status, entries) = get_event_audit(registry).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            entries.len(),
            1,
            "only the current run's ruling — the pre-restart DQ is abandoned with its run"
        );
        assert_eq!(entries[0].entry.at_ref, LogRef(14));
        assert_eq!(entries[0].heat, heat());
        assert_eq!(entries[0].entry.competitor, Some(CompetitorRef("B".into())));
    }

    #[tokio::test]
    async fn event_audit_on_unknown_event_is_not_found() {
        let (registry, _state, _) = state_with(recorded_heat());
        let response = router(registry)
            .oneshot(
                Request::builder()
                    .uri("/events/no-such-event/audit")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
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
        let (status, snap) = get_snapshot(registry, "/snapshot/heat/q-1?projection=signal").await;
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
        let uri = event_uri(&registry, "/snapshot/heat/does-not-exist");
        let response = router(registry)
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
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
        let (status, snap) = get_snapshot(registry, "/snapshot/pilot/spring-cup/A").await;
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
        let (status, snap) = get_snapshot(registry, "/snapshot/pilot/spring-cup/acroace").await;
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
        let uri = event_uri(&registry, "/snapshot/pilot/spring-cup/nobody");
        let response = router(registry)
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn class_scope_is_reachable() {
        let (registry, _state, len) = state_with(recorded_heat());
        let (status, snap) = get_snapshot(registry, "/snapshot/class/spring-cup/open").await;
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
                label: None,
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
                label: None,
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
        let (status, snap) =
            get_snapshot(registry.clone(), "/snapshot/class/spring-cup/open").await;
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
        let (_, snap) = get_snapshot(registry, "/snapshot/class/spring-cup/sport").await;
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
        let (status, snap) = get_snapshot(registry, "/snapshot/event/spring-cup").await;
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
                label: None,
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
                label: None,
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

        let (_, snap) = get_snapshot(registry.clone(), "/snapshot/heat/q-1?projection=laps").await;
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

        let (_, snap) = get_snapshot(registry, "/snapshot/heat/q-2?projection=laps").await;
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
        let (status, snap) = get_snapshot(registry, "/snapshot/heat/q-1").await;
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

        // Setting it (open Director — no token needed) returns the created event's meta…
        let event = sole_event(&registry);
        let (status, raw) = put_active(registry.clone(), &event.0, None).await;
        assert_eq!(status, StatusCode::OK);
        let meta: EventMeta = serde_json::from_slice(&raw).unwrap();
        assert_eq!(meta.id, event);

        // …and now the open read resumes into it.
        let (_, body) = get_active(registry).await;
        assert_eq!(body.unwrap().event.map(|m| m.id), Some(event));
    }

    #[tokio::test]
    async fn a_stale_active_event_id_reads_as_no_active_event_not_a_500() {
        // #414: an upgraded Director's persisted `active-event` may still name the removed
        // built-in `practice` event. The registry drops the stale pointer on boot, so the open
        // read is a plain 200 with `event: null` — the picker — never a 500 or a dangling meta.
        let (registry, _state, _) = state_with(recorded_heat());
        let (status, body) = get_active(registry.clone()).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.expect("an ActiveEvent body").event.is_none());

        // Pointing it at the removed id over HTTP is a typed 404, not a 500.
        let (status, raw) = put_active(registry, "practice", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let err: ProtocolError = serde_json::from_slice(&raw).unwrap();
        assert_eq!(err.code, ErrorCode::UnknownScope);
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
        let event = sole_event(&registry);
        let (status, _) = put_active(registry, &event.0, None).await;
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
    async fn delete_event_rejects_unknown() {
        let (registry, _state, _) = state_with(recorded_heat());
        // No event is reserved any more (#414) — the old built-in `practice` id is simply
        // unknown, so it 404s like any other unknown id rather than 400-ing as undeletable.
        let (status, raw) = delete_event_req(registry.clone(), "practice", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let err: ProtocolError = serde_json::from_slice(&raw).unwrap();
        assert_eq!(err.code, ErrorCode::UnknownScope);

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
        let uri = event_uri(&registry, "/control");
        let response = router(registry)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
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
        let uri = event_uri(&registry, "/control");
        let response = router(registry)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
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

    /// `POST /events/{id}/auth/join-token` with an optional bearer token; status + parsed body.
    async fn post_join_token(
        registry: EventRegistry,
        token: Option<&str>,
    ) -> (StatusCode, Option<JoinTokenResponse>) {
        let mut builder = Request::builder()
            .method("POST")
            .uri(event_uri(&registry, "/auth/join-token"));
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
        CreateTimerRequest, NodeReading, SIGNAL_SAMPLE_INTERVAL, SetEventTimersRequest, Timer,
        TimerKind, UpdateTimerRequest,
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

    /// `POST /timers/{id}/{connect|disconnect}` → status + parsed `Timer` (when the call succeeded).
    async fn post_timer_connection(
        registry: EventRegistry,
        timer_id: &str,
        action: &str,
    ) -> (StatusCode, Option<Timer>) {
        let response = router(registry)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/timers/{timer_id}/{action}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice::<Timer>(&bytes).ok())
    }

    #[tokio::test]
    async fn connect_and_disconnect_hold_a_timers_connection_without_an_event() {
        // #383: the Timers menu's diagnostic control. No event is created, activated, or selects
        // the timer — the hold alone is what the connection reconciler dials on.
        let (registry, _state, _) = state_with(vec![]);
        let rh = registry
            .timers()
            .create(&CreateTimerRequest {
                name: "Field RH".into(),
                kind: TimerKind::Rotorhazard {
                    url: "http://rh.local:5000".into(),
                },
                channel_capability: None,
                node_count: None,
                available_channels: None,
            })
            .unwrap();

        let (status, timer) = post_timer_connection(registry.clone(), &rh.id.0, "connect").await;
        assert_eq!(status, StatusCode::OK);
        assert!(timer.unwrap().manual_connect);
        assert_eq!(registry.timers().manual_connections(), vec![rh.id.clone()]);
        // The hold is visible in the open `GET /timers` read, so a console can render Disconnect.
        let (_, listed) = get_timers(registry.clone()).await;
        assert!(listed.iter().any(|t| t.id == rh.id && t.manual_connect));

        // Explicit lifetime: it stands until disconnect, which releases it.
        let (status, timer) = post_timer_connection(registry.clone(), &rh.id.0, "disconnect").await;
        assert_eq!(status, StatusCode::OK);
        assert!(!timer.unwrap().manual_connect);
        assert!(registry.timers().manual_connections().is_empty());
    }

    #[tokio::test]
    async fn connecting_a_mock_or_an_unknown_timer_is_rejected() {
        // A Mock has nothing to dial (a client `400`); an unknown id is a 404.
        let (registry, _state, _) = state_with(vec![]);
        let (status, _) = post_timer_connection(registry.clone(), "mock", "connect").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = post_timer_connection(registry, "no-such-timer", "connect").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// `GET /timers/{id}/signal` → status + parsed [`TimerSignal`] (when the call succeeded).
    async fn get_timer_signal(
        registry: EventRegistry,
        timer_id: &str,
    ) -> (StatusCode, Option<TimerSignal>) {
        let response = router(registry)
            .oneshot(
                Request::builder()
                    .uri(format!("/timers/{timer_id}/signal"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice::<TimerSignal>(&bytes).ok())
    }

    /// `POST /timers/{id}/signal/stop` → status.
    async fn post_stop_timer_signal(registry: EventRegistry, timer_id: &str) -> StatusCode {
        router(registry)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/timers/{timer_id}/signal/stop"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    /// A RotorHazard timer to read tune telemetry from — no event, and none needed (#355 S2a): the
    /// tune path is timer-scoped precisely so it works before an event exists, which is the state
    /// an untuned timer is in.
    fn rh_timer_for_signal(registry: &EventRegistry) -> Timer {
        registry
            .timers()
            .create(&CreateTimerRequest {
                name: "Field RH".into(),
                kind: TimerKind::Rotorhazard {
                    url: "http://rh.local:5000".into(),
                },
                channel_capability: None,
                node_count: None,
                available_channels: None,
            })
            .unwrap()
    }

    #[tokio::test]
    async fn reading_a_timers_signal_starts_and_renews_the_lease() {
        // #355 S2a: the GET *is* the subscription. Nothing else opens it, and nothing but a
        // continuing poll keeps it open.
        let (registry, _state, _) = state_with(vec![]);
        let rh = rh_timer_for_signal(&registry);
        assert!(
            !registry.timers().signal_wanted(&rh.id),
            "nothing streams until someone asks"
        );

        let (status, signal) = get_timer_signal(registry.clone(), &rh.id.0).await;
        assert_eq!(status, StatusCode::OK);
        let signal = signal.expect("a snapshot");
        assert_eq!(signal.timer, rh.id);
        // No live connection in this test, so nothing has pushed — which the snapshot says plainly
        // rather than pretending. "No signal" and "no link" are different problems.
        assert!(!signal.streaming);
        assert!(signal.lease_ms_remaining > 0);
        assert_eq!(
            signal.period_micros,
            SIGNAL_SAMPLE_INTERVAL.as_micros() as u32
        );
        assert!(registry.timers().signal_wanted(&rh.id));

        // A push from the (simulated) connection driver shows up on the next read, all nodes.
        registry.timers().push_signal(
            &rh.id,
            &(0..8)
                .map(|index| NodeReading {
                    seen: true,
                    rssi: Some(40.0 + index as f32),
                    enter_at: Some(90.0),
                    exit_at: Some(80.0),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        );
        let (_, signal) = get_timer_signal(registry.clone(), &rh.id.0).await;
        let signal = signal.expect("a snapshot");
        assert!(signal.streaming);
        assert_eq!(signal.nodes.len(), 8);
        assert_eq!(signal.sample_micros.len(), 1);
        assert_eq!(signal.nodes[7].samples, vec![47.0]);
        assert_eq!(signal.nodes[7].enter_at, Some(90.0));
    }

    #[tokio::test]
    async fn stopping_a_timers_signal_ends_it_promptly() {
        // The lease alone would stop it; the explicit stop is for closing the Tune view without
        // leaving the socket parsing heartbeats for another five seconds.
        let (registry, _state, _) = state_with(vec![]);
        let rh = rh_timer_for_signal(&registry);
        get_timer_signal(registry.clone(), &rh.id.0).await;
        assert!(registry.timers().signal_wanted(&rh.id));

        assert_eq!(
            post_stop_timer_signal(registry.clone(), &rh.id.0).await,
            StatusCode::NO_CONTENT
        );
        assert!(!registry.timers().signal_wanted(&rh.id));
        // Idempotent — a second close, or a view that was never opened, is not an error.
        assert_eq!(
            post_stop_timer_signal(registry.clone(), &rh.id.0).await,
            StatusCode::NO_CONTENT
        );
    }

    #[tokio::test]
    async fn a_mock_or_unknown_timer_has_no_signal_to_read() {
        // A Mock is in every event's default selection and has no detector at all, so answering
        // "no nodes yet" would read as a quiet timer rather than one that cannot have signal.
        let (registry, _state, _) = state_with(vec![]);
        let (status, _) = get_timer_signal(registry.clone(), "mock").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = get_timer_signal(registry.clone(), "no-such-timer").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            post_stop_timer_signal(registry, "no-such-timer").await,
            StatusCode::NOT_FOUND
        );
    }

    /// A connected RotorHazard timer selected by Practice — the precondition every restart test
    /// shares (#386): the Director only accepts a restart on a live connection, and the race-phase
    /// refusal only looks at events that *select* the timer.
    fn connected_rh_timer_selected_by_the_event(registry: &EventRegistry) -> Timer {
        let rh = registry
            .timers()
            .create(&CreateTimerRequest {
                name: "Field RH".into(),
                kind: TimerKind::Rotorhazard {
                    url: "http://rh.local:5000".into(),
                },
                channel_capability: None,
                node_count: None,
                available_channels: None,
            })
            .unwrap();
        registry
            .timers()
            .set_status(&rh.id, crate::timers::TimerStatus::Connected);
        let event = sole_event(registry);
        registry
            .set_timers(&event, vec![rh.id.clone()])
            .expect("select the RH timer for the event");
        // The event must be ACTIVE, not merely selecting the timer: only the active event's
        // selection opens a connection, so only its heats can be in progress on the timer. The
        // in-progress scan is scoped to the active event for that reason, and a fixture that
        // stages a heat in a non-active event models a state the Director cannot reach.
        registry
            .set_active(&event)
            .expect("make it the active event");
        registry.timers().get(&rh.id).unwrap()
    }

    #[tokio::test]
    async fn restarting_a_connected_rh_timer_queues_the_restart_for_the_reconciler() {
        // #386: the guided plugin install's last step. The route parks the request on the timer
        // registry — the connection layer lives above this crate — and the reconciler drains it.
        let (registry, _state, _) = state_with(vec![]);
        let rh = connected_rh_timer_selected_by_the_event(&registry);

        let (status, timer) = post_timer_connection(registry.clone(), &rh.id.0, "restart").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(timer.unwrap().id, rh.id);
        // Asking twice before the drain coalesces into ONE restart, not two.
        let (status, _) = post_timer_connection(registry.clone(), &rh.id.0, "restart").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            registry.timers().take_restart_requests(),
            vec![rh.id.clone()]
        );
        // Drained exactly once: a second drain is empty (nothing is re-queued).
        assert!(registry.timers().take_restart_requests().is_empty());
    }

    #[tokio::test]
    async fn restarting_a_timer_is_refused_while_a_race_is_in_progress_on_it() {
        // The hard gate (#386): restarting RotorHazard takes the RD's timing hardware down, so it is
        // refused on HEAT PHASE — not merely confirmed in the console. Each of the four in-progress
        // phases must refuse, and the refusal must name the heat by its FRIENDLY name (CLAUDE.md).
        for transition in [
            HeatTransition::Staged,
            HeatTransition::Armed,
            HeatTransition::Running,
            HeatTransition::Finished, // → Unofficial
        ] {
            let (registry, state, _) = state_with(vec![]);
            let rh = connected_rh_timer_selected_by_the_event(&registry);
            state
                .append(
                    Event::HeatScheduled {
                        heat: HeatId("q-1".into()),
                        lineup: vec![CompetitorRef("A".into())],
                        class: None,
                        round: None,
                        frequencies: vec![],
                        label: Some("Qualifier Heat 1".into()),
                    },
                    None,
                )
                .unwrap();
            for t in [
                HeatTransition::Staged,
                HeatTransition::Armed,
                HeatTransition::Running,
                HeatTransition::Finished,
            ] {
                state
                    .append(
                        Event::HeatStateChanged {
                            heat: HeatId("q-1".into()),
                            transition: t,
                        },
                        None,
                    )
                    .unwrap();
                if t == transition {
                    break;
                }
            }

            let response = router(registry.clone())
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/timers/{}/restart", rh.id.0))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "a {transition:?} heat must refuse the restart"
            );
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let err: ProtocolError = serde_json::from_slice(&bytes).unwrap();
            // Names the heat AND the timer — never their raw ids.
            assert!(
                err.message.contains("Qualifier Heat 1"),
                "the refusal must name the heat: {}",
                err.message
            );
            assert!(
                err.message.contains("Field RH"),
                "the refusal must name the timer: {}",
                err.message
            );
            assert!(
                !err.message.contains(&rh.id.0),
                "the refusal must not leak the raw timer id: {}",
                err.message
            );
            // Nothing was queued: the refusal is a real refusal, not a confirm-and-fire.
            assert!(registry.timers().take_restart_requests().is_empty());
        }
    }

    #[tokio::test]
    async fn restarting_is_allowed_once_the_heat_is_final_and_before_it_is_staged() {
        // The bookends of the in-progress window: a `Scheduled` heat has not begun and a `Final` one
        // is done, so neither blocks the plugin install's restart.
        let (registry, state, _) = state_with(vec![]);
        let rh = connected_rh_timer_selected_by_the_event(&registry);
        state
            .append(
                Event::HeatScheduled {
                    heat: HeatId("q-1".into()),
                    lineup: vec![CompetitorRef("A".into())],
                    class: None,
                    round: None,
                    frequencies: vec![],
                    label: None,
                },
                None,
            )
            .unwrap();
        let (status, _) = post_timer_connection(registry.clone(), &rh.id.0, "restart").await;
        assert_eq!(status, StatusCode::OK, "a Scheduled heat has not begun");
        let _ = registry.timers().take_restart_requests();

        for t in [
            HeatTransition::Staged,
            HeatTransition::Armed,
            HeatTransition::Running,
            HeatTransition::Finished,
            HeatTransition::Finalized,
        ] {
            state
                .append(
                    Event::HeatStateChanged {
                        heat: HeatId("q-1".into()),
                        transition: t,
                    },
                    None,
                )
                .unwrap();
        }
        let (status, _) = post_timer_connection(registry.clone(), &rh.id.0, "restart").await;
        assert_eq!(status, StatusCode::OK, "a Final heat is done racing");
    }

    #[tokio::test]
    async fn restarting_a_mock_an_unknown_or_a_disconnected_timer_is_rejected() {
        // A Mock has no timing server to restart and an unknown id is a 404 — mirroring
        // connect/disconnect. A configured-but-not-connected RH timer is also a 400: there is no
        // socket to emit `restart_server` on, and a request is never held over for a future connect.
        let (registry, _state, _) = state_with(vec![]);
        let (status, _) = post_timer_connection(registry.clone(), "mock", "restart").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = post_timer_connection(registry.clone(), "no-such-timer", "restart").await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let rh = registry
            .timers()
            .create(&CreateTimerRequest {
                name: "Field RH".into(),
                kind: TimerKind::Rotorhazard {
                    url: "http://rh.local:5000".into(),
                },
                channel_capability: None,
                node_count: None,
                available_channels: None,
            })
            .unwrap();
        let (status, _) = post_timer_connection(registry.clone(), &rh.id.0, "restart").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(registry.timers().take_restart_requests().is_empty());
    }

    /// `GET`/`PUT` `/timers/{id}/nodes` with an optional JSON body → status + raw bytes.
    async fn timer_nodes_call(
        registry: EventRegistry,
        timer_id: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, Vec<u8>) {
        let request = Request::builder().uri(format!("/timers/{timer_id}/nodes"));
        let request = match &body {
            Some(json) => request
                .method("PUT")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(json).unwrap()))
                .unwrap(),
            None => request.method("GET").body(Body::empty()).unwrap(),
        };
        let response = router(registry).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, bytes.to_vec())
    }

    #[tokio::test]
    async fn the_nodes_route_reports_the_enabled_set_and_the_drift() {
        // #412 end to end over the wire: discover, disable, read back.
        use crate::timers::TimerNodes;
        let (registry, _state, _) = state_with(vec![]);
        let rh = create_rh_timer(&registry, "Field RH");

        // Nothing observed yet: the width is the default, every node enabled, no drift.
        let (status, bytes) = timer_nodes_call(registry.clone(), &rh.id.0, None).await;
        assert_eq!(status, StatusCode::OK);
        let view: TimerNodes = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(view.reported, None);
        assert_eq!(view.configured, None);
        assert_eq!(view.width, crate::timers::DEFAULT_NODE_COUNT);
        assert!(view.drift.is_none());

        // The timer connects and says it has four nodes.
        registry.timers().set_reported_nodes(&rh.id, 4);
        let (_, bytes) = timer_nodes_call(registry.clone(), &rh.id.0, None).await;
        let view: TimerNodes = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(view.reported, Some(4));
        assert_eq!(view.width, 4);
        assert_eq!(view.enabled, vec![0, 1, 2, 3]);
        assert!(view.nodes.iter().all(|n| n.reported && n.enabled));

        // The RD switches off "Node 3" — wire index 2.
        let (status, bytes) = timer_nodes_call(
            registry.clone(),
            &rh.id.0,
            Some(json!({ "enabled": [0, 1, 3] })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let view: TimerNodes = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            view.enabled,
            vec![0, 1, 3],
            "a set with a hole, not a prefix"
        );
        assert_eq!(view.nodes[2].label, "Node 3");
        assert!(!view.nodes[2].enabled);
        assert_eq!(
            view.nodes[2].seat,
            gridfpv_events::CompetitorRef("node-2".into())
        );

        // Pinning the width above what the hardware has surfaces the drift — the bench bug, made
        // visible instead of silently building an oversized heat.
        let (_, bytes) =
            timer_nodes_call(registry.clone(), &rh.id.0, Some(json!({ "node_count": 8 }))).await;
        let view: TimerNodes = serde_json::from_slice(&bytes).unwrap();
        let drift = view.drift.expect("reported 4 vs configured 8 is drift");
        assert_eq!((drift.reported, drift.configured), (4, 8));
        assert_eq!(drift.enabled_beyond_reported, vec![4, 5, 6, 7]);

        // …and `null` puts it back on the hardware's word.
        let (_, bytes) = timer_nodes_call(
            registry.clone(),
            &rh.id.0,
            Some(json!({ "node_count": null })),
        )
        .await;
        let view: TimerNodes = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(view.width, 4);
        assert!(view.drift.is_none());
        assert_eq!(view.enabled, vec![0, 1, 3], "the disable is untouched");

        // An unknown timer is a 404; an edit that disables everything is a 400.
        let (status, _) = timer_nodes_call(registry.clone(), "no-such-timer", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) =
            timer_nodes_call(registry.clone(), &rh.id.0, Some(json!({ "enabled": [] }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// `POST /timers/{id}/calibration` with a raw JSON body → status + raw bytes.
    async fn post_calibration(
        registry: EventRegistry,
        timer_id: &str,
        body: serde_json::Value,
    ) -> (StatusCode, Vec<u8>) {
        let response = router(registry)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/timers/{timer_id}/calibration"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, bytes.to_vec())
    }

    /// The refusal message from a failed calibration call.
    fn refusal(bytes: &[u8]) -> String {
        serde_json::from_slice::<ProtocolError>(bytes)
            .expect("a ProtocolError body")
            .message
    }

    #[tokio::test]
    async fn calibrating_a_connected_rh_timer_queues_the_write_and_records_it_as_grid_config() {
        // #355: the write half of the Tune page. The route parks the write on the timer registry —
        // the connection layer lives above this crate — and the reconciler drains it onto the live
        // socket. D27: the accepted value is ALSO recorded on the timer, because a threshold the RD
        // set is GridFPV's config; the timer is only where it is applied.
        let (registry, _state, _) = state_with(vec![]);
        let rh = connected_rh_timer_selected_by_the_event(&registry);

        let (status, bytes) = post_calibration(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 2, "enter_at": 96 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let dispatch: crate::timers::CalibrationDispatch = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(dispatch.timer, rh.id);
        assert_eq!(dispatch.node, 2);
        assert_eq!(dispatch.enter_at, Some(96));
        // The threshold that was NOT sent stays absent — the route never invents the other half.
        assert_eq!(dispatch.exit_at, None);

        // D27: GridFPV holds the value itself, not merely the timer.
        assert_eq!(
            registry.timers().calibration(&rh.id),
            vec![crate::timers::NodeCalibration {
                node: 2,
                enter_at: Some(96),
                exit_at: None,
            }]
        );

        // The queue drains EXACTLY ONCE.
        let drained = registry.timers().take_calibration_requests();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].timer, rh.id);
        assert_eq!(drained[0].node, 2);
        assert_eq!(drained[0].enter_at, Some(96));
        assert!(
            registry.timers().take_calibration_requests().is_empty(),
            "a second drain is empty — nothing is re-queued"
        );
    }

    #[tokio::test]
    async fn repeated_writes_to_one_node_coalesce_to_the_latest_value_per_threshold() {
        // The page writes on interaction end, so a drag that lands twice before the reconciler's
        // next tick must apply the LATEST value once — never replay a stale one after it. Enter and
        // exit are independent: writing one must not clear the other.
        let (registry, _state, _) = state_with(vec![]);
        let rh = connected_rh_timer_selected_by_the_event(&registry);

        for enter in [80, 90, 101] {
            let (status, _) = post_calibration(
                registry.clone(),
                &rh.id.0,
                json!({ "node": 0, "enter_at": enter }),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
        }
        let (status, _) = post_calibration(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 0, "exit_at": 77 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        // A different node is its own entry, not a coalesce target.
        let (status, _) = post_calibration(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 3, "enter_at": 55 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let drained = registry.timers().take_calibration_requests();
        assert_eq!(drained.len(), 2, "one entry per node, not one per write");
        assert_eq!(drained[0].node, 0);
        assert_eq!(drained[0].enter_at, Some(101), "the latest enter wins");
        assert_eq!(
            drained[0].exit_at,
            Some(77),
            "the exit is carried alongside"
        );
        assert_eq!(drained[1].node, 3);
        assert!(registry.timers().take_calibration_requests().is_empty());
    }

    #[tokio::test]
    async fn calibration_levels_are_clamped_server_side() {
        // Never trust the client for a value that reaches timing hardware. `0` is the dangerous one:
        // RotorHazard's `calibration.py` tests the level for truthiness, so a `0` is read as "re-read
        // it off the node" and the old threshold silently survives while the write looks accepted.
        let (registry, _state, _) = state_with(vec![]);
        let rh = connected_rh_timer_selected_by_the_event(&registry);

        let (status, bytes) = post_calibration(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 1, "enter_at": 0, "exit_at": 9_999 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let dispatch: crate::timers::CalibrationDispatch = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(dispatch.enter_at, Some(crate::timers::RSSI_MIN));
        assert_eq!(dispatch.exit_at, Some(crate::timers::RSSI_MAX));

        // The clamp happens ONCE, before both the record and the queue — so neither can hold a
        // value the other does not.
        let drained = registry.timers().take_calibration_requests();
        assert_eq!(drained[0].enter_at, Some(crate::timers::RSSI_MIN));
        assert_eq!(drained[0].exit_at, Some(crate::timers::RSSI_MAX));
        assert_eq!(
            registry.timers().calibration(&rh.id),
            vec![crate::timers::NodeCalibration {
                node: 1,
                enter_at: Some(crate::timers::RSSI_MIN),
                exit_at: Some(crate::timers::RSSI_MAX),
            }]
        );
    }

    #[tokio::test]
    async fn calibrating_is_refused_while_a_scored_race_is_in_progress_on_the_timer() {
        // The hard gate (#355): moving a detection threshold under a SCORED race changes what counts
        // as a lap while it is being counted. Gated on HEAT PHASE, and the refusal names the heat and
        // the timer by their FRIENDLY names (CLAUDE.md) — and says the heat is *scored*, so an RD
        // refused mid-heat learns why rather than just "a heat is running".
        for transition in [
            HeatTransition::Staged,
            HeatTransition::Armed,
            HeatTransition::Running,
            HeatTransition::Finished, // → Unofficial
        ] {
            let (registry, state, _) = state_with(vec![]);
            let rh = connected_rh_timer_selected_by_the_event(&registry);
            state
                .append(
                    Event::HeatScheduled {
                        heat: HeatId("q-1".into()),
                        lineup: vec![CompetitorRef("A".into())],
                        class: None,
                        round: None,
                        frequencies: vec![],
                        label: Some("Qualifier Heat 1".into()),
                    },
                    None,
                )
                .unwrap();
            for t in [
                HeatTransition::Staged,
                HeatTransition::Armed,
                HeatTransition::Running,
                HeatTransition::Finished,
            ] {
                state
                    .append(
                        Event::HeatStateChanged {
                            heat: HeatId("q-1".into()),
                            transition: t,
                        },
                        None,
                    )
                    .unwrap();
                if t == transition {
                    break;
                }
            }

            let (status, bytes) = post_calibration(
                registry.clone(),
                &rh.id.0,
                json!({ "node": 0, "enter_at": 90 }),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "a {transition:?} heat must refuse the calibration write"
            );
            let message = refusal(&bytes);
            assert!(
                message.contains("Qualifier Heat 1"),
                "the refusal must name the heat: {message}"
            );
            assert!(
                message.contains("Field RH"),
                "the refusal must name the timer: {message}"
            );
            assert!(
                message.contains("scored heat"),
                "the refusal must say the heat is SCORED — that is what makes it different from \
                 open practice, which is tunable while it runs: {message}"
            );
            assert!(
                !message.contains(&rh.id.0),
                "the refusal must not leak the raw timer id: {message}"
            );
            // A real refusal, not a confirm-and-fire — and nothing was recorded as config either.
            assert!(registry.timers().take_calibration_requests().is_empty());
            assert!(registry.timers().calibration(&rh.id).is_empty());
        }
    }

    #[tokio::test]
    async fn calibrating_is_accepted_while_an_open_practice_heat_is_running() {
        // #355 + #398, and the companion to the refusal above: an OPEN PRACTICE heat does NOT block
        // a threshold change. Practice is excluded from scoring, so there is no result for a moved
        // threshold to corrupt — and a pilot in the air on a practice heat is exactly when an RD
        // wants to tune ("I want to slide the slider and then test right away"). Refuse here and the
        // RD can only tune an idle gate and wave a quad through by hand, which is the RotorHazard-UI
        // loop this page was built to replace.
        //
        // Easy to regress back to the stricter `heat_in_progress_on_timer` gate, which is why this
        // exercises every racing phase rather than just Running.
        for transition in [
            HeatTransition::Staged,
            HeatTransition::Armed,
            HeatTransition::Running,
            HeatTransition::Finished, // → Unofficial
        ] {
            let (registry, state, _) = state_with(vec![]);
            let rh = connected_rh_timer_selected_by_the_event(&registry);
            // A round in the OPEN PRACTICE format — `open_practice::excluded_from_scoring` keys on
            // the format alone, and the gate consults that same predicate so the two cannot drift.
            let round = registry
                .add_round(
                    &sole_event(&registry),
                    NewRoundReq {
                        layouts: Vec::new(),
                        label: "Practice".into(),
                        classes: vec![],
                        // The OPEN PRACTICE format is the whole of the exemption:
                        // `open_practice::excluded_from_scoring` keys on the format name alone, and
                        // the calibration gate consults that same predicate, so the two cannot drift.
                        format: gridfpv_engine::format::OpenPractice::NAME.to_string(),
                        params: std::collections::BTreeMap::new(),
                        win_condition: None,
                        seeding: SeedingRule::ActiveNodes { nodes: vec![0] },
                        time_limit_secs: None,
                        channel_mode: None,
                        staging_timer_secs: None,
                        start_procedure: None,
                        grace_window: None,
                        protest_window: None,
                        min_lap_secs: None,
                    },
                )
                .expect("an open-practice round");
            state
                .append(
                    Event::HeatScheduled {
                        heat: HeatId("p-1".into()),
                        lineup: vec![CompetitorRef("node-0".into())],
                        class: None,
                        round: Some(round.id.clone()),
                        frequencies: vec![],
                        label: Some("Practice Heat 1".into()),
                    },
                    None,
                )
                .unwrap();
            for t in [
                HeatTransition::Staged,
                HeatTransition::Armed,
                HeatTransition::Running,
                HeatTransition::Finished,
            ] {
                state
                    .append(
                        Event::HeatStateChanged {
                            heat: HeatId("p-1".into()),
                            transition: t,
                        },
                        None,
                    )
                    .unwrap();
                if t == transition {
                    break;
                }
            }

            let (status, bytes) = post_calibration(
                registry.clone(),
                &rh.id.0,
                json!({ "node": 0, "enter_at": 90 }),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::OK,
                "an open-practice heat in {transition:?} must NOT block a threshold change"
            );
            let dispatch: crate::timers::CalibrationDispatch =
                serde_json::from_slice(&bytes).unwrap();
            assert_eq!(dispatch.enter_at, Some(90));

            // …and it must actually reach the wire. The write carries the route's finding that a
            // practice heat is racing, so the driver's own armed-heat backstop lets it through —
            // without that flag the route would accept a write the driver silently dropped, which is
            // "dispatched but never landed", the failure this page exists to catch.
            let drained = registry.timers().take_calibration_requests();
            assert_eq!(drained.len(), 1);
            assert!(
                drained[0].during_open_practice,
                "the write must be stamped as cleared against an open-practice heat, or the \
                 driver's armed-heat backstop will drop it"
            );
        }
    }

    #[tokio::test]
    async fn calibrating_a_mock_an_unknown_or_a_disconnected_timer_is_rejected() {
        // A Mock has no radio to calibrate; an unknown id is a 404 (never a message about a timer
        // that does not exist); a configured-but-not-connected RH timer is a 400 — there is no socket
        // to emit on, and a threshold is never held over for a future connection.
        let (registry, _state, _) = state_with(vec![]);

        let (status, bytes) = post_calibration(
            registry.clone(),
            "mock",
            json!({ "node": 0, "enter_at": 90 }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let message = refusal(&bytes);
        assert!(
            message.contains("Mock") && message.contains("not a RotorHazard timer"),
            "the Mock refusal must name the timer and say why: {message}"
        );

        let (status, _) = post_calibration(
            registry.clone(),
            "no-such-timer",
            json!({ "node": 0, "enter_at": 90 }),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let rh = registry
            .timers()
            .create(&CreateTimerRequest {
                name: "Field RH".into(),
                kind: TimerKind::Rotorhazard {
                    url: "http://rh.local:5000".into(),
                },
                channel_capability: None,
                node_count: None,
                available_channels: None,
            })
            .unwrap();
        let (status, bytes) = post_calibration(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 0, "enter_at": 90 }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let message = refusal(&bytes);
        assert!(
            message.contains("Field RH") && message.contains("not connected"),
            "the disconnected refusal must name the timer: {message}"
        );
        assert!(registry.timers().take_calibration_requests().is_empty());
        assert!(registry.timers().calibration(&rh.id).is_empty());
    }

    #[tokio::test]
    async fn a_calibration_write_with_no_threshold_or_an_unknown_node_is_refused() {
        // "I asked for nothing and it worked" is the shape of every silent calibration failure, so an
        // empty write is a refusal rather than a no-op success. A node beyond the timer's width is
        // refused too: RotorHazard's `calibration.py` drops an out-of-range seat index with nothing
        // but a log line, which would look exactly like a successful write that did nothing.
        let (registry, _state, _) = state_with(vec![]);
        let rh = connected_rh_timer_selected_by_the_event(&registry);

        let (status, bytes) =
            post_calibration(registry.clone(), &rh.id.0, json!({ "node": 0 })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            refusal(&bytes).contains("no threshold given"),
            "the empty-write refusal must say what is missing"
        );

        let width = registry.timers().get(&rh.id).unwrap().node_width();
        let (status, bytes) = post_calibration(
            registry.clone(),
            &rh.id.0,
            json!({ "node": width, "enter_at": 90 }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let message = refusal(&bytes);
        assert!(
            message.contains("Field RH"),
            "the refusal must name the timer: {message}"
        );
        assert!(
            // The 1-based display name, per the repo display rule: index `width` is "Node width+1".
            message.contains(&format!("Node {}", width + 1)),
            "the refusal must name the node the way the page labels it (1-based): {message}"
        );

        // #412: a node that EXISTS but the RD has disabled is refused too — tuning a gate no heat
        // is ever seated on would confirm a write on hardware nobody flies.
        registry
            .timers()
            .set_nodes(
                &rh.id,
                &crate::timers::SetTimerNodesRequest {
                    node_count: None,
                    enabled: Some((0..width).filter(|n| *n != 2).collect()),
                },
            )
            .unwrap();
        let (status, bytes) = post_calibration(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 2, "enter_at": 90 }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let message = refusal(&bytes);
        assert!(
            message.contains("Node 3") && message.contains("disabled"),
            "the disabled-node refusal must name the node 1-based and say why: {message}"
        );
        assert!(registry.timers().take_calibration_requests().is_empty());
    }

    /// `POST /timers/{id}/capture` with a raw JSON body → status + raw bytes (#355).
    async fn post_capture(
        registry: EventRegistry,
        timer_id: &str,
        body: serde_json::Value,
    ) -> (StatusCode, Vec<u8>) {
        let response = router(registry)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/timers/{timer_id}/capture"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, bytes.to_vec())
    }

    /// Put a level on the timer's live signal feed, the way the connection driver does.
    ///
    /// A capture is settled against **what the timer is reporting**, so a test about a capture has
    /// to feed one — this is the same `push_signal` the driver calls, through the same lease.
    fn report_levels(
        registry: &EventRegistry,
        timer: &crate::timers::TimerId,
        levels: &[(f32, f32)],
    ) {
        let timers = registry.timers();
        let _ = timers.signal(timer); // open/renew the lease — no lease, no ring, no readings
        let readings: Vec<crate::timers::NodeReading> = levels
            .iter()
            .map(|(enter, exit)| crate::timers::NodeReading {
                seen: true,
                enter_at: Some(*enter),
                exit_at: Some(*exit),
                ..Default::default()
            })
            .collect();
        timers.push_signal(timer, &readings);
    }

    #[tokio::test]
    async fn a_capture_is_queued_with_rotorhazards_own_sampling_window() {
        // #355: the third write. The route parks the capture on the registry — the connection layer
        // lives above this crate — and the reconciler drains it onto the live socket as
        // `cap_enter_at_btn`.
        //
        // The dispatch is NOT a readback and could not be: RotorHazard opens a three-second sampling
        // window at the emit and only then has a level. What it carries instead is that window, so
        // the console counts down RotorHazard's own number rather than one the console invented.
        let (registry, _state, _) = state_with(vec![]);
        let rh = connected_rh_timer_selected_by_the_event(&registry);
        report_levels(&registry, &rh.id, &[(90.0, 80.0), (95.0, 85.0)]);

        let (status, bytes) = post_capture(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 1, "threshold": "enter" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let dispatch: crate::timers::CaptureDispatch = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(dispatch.timer, rh.id);
        assert_eq!(dispatch.node, 1);
        assert_eq!(dispatch.threshold, crate::timers::CaptureThreshold::Enter);
        // `BaseHardwareInterface::CAP_ENTER_EXIT_AT_MILLIS`, verified identical on v4.3.0 and v4.4.0.
        assert_eq!(dispatch.window_ms, crate::timers::CAPTURE_WINDOW_MS);
        assert_eq!(dispatch.settle_ms, crate::timers::CAPTURE_SETTLE_MS);
        // What the capture is replacing — evidence about the timer, and what "a new level arrived"
        // will be measured against.
        assert_eq!(dispatch.previous, Some(95));

        // Nothing is recorded as GridFPV's config YET: the level does not exist. Recording one here
        // would be a fabricated success, which is exactly what this control exists to avoid.
        assert!(registry.timers().calibration(&rh.id).is_empty());

        let drained = registry.timers().take_capture_requests();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].node, 1);
        assert_eq!(drained[0].threshold, crate::timers::CaptureThreshold::Enter);
        assert!(
            registry.timers().take_capture_requests().is_empty(),
            "a second drain is empty — nothing is re-queued"
        );
    }

    #[tokio::test]
    async fn a_second_capture_of_a_threshold_already_capturing_is_refused_not_silently_dropped() {
        // RotorHazard's `start_capture_enter_at_level` returns False when a capture of that
        // threshold is already running on that node — and emits NOTHING. Accepting the second press
        // here would show a capture as started that never was: the fourth silently-ignored write
        // (#423). The OTHER threshold on the same node is a different capture and must be allowed.
        let (registry, _state, _) = state_with(vec![]);
        let rh = connected_rh_timer_selected_by_the_event(&registry);
        report_levels(&registry, &rh.id, &[(90.0, 80.0)]);

        let (status, _) = post_capture(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 0, "threshold": "enter" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, bytes) = post_capture(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 0, "threshold": "enter" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let message = refusal(&bytes);
        assert!(
            message.contains("Node 1") && message.contains("already capturing"),
            "the refusal must name the node 1-based and say why: {message}"
        );

        // The exit threshold is its own capture — RotorHazard arms them independently.
        let (status, _) = post_capture(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 0, "threshold": "exit" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(registry.timers().take_capture_requests().len(), 2);
    }

    #[tokio::test]
    async fn a_captured_level_is_confirmed_by_poll_and_recorded_as_grid_config() {
        // The whole point, and the D27 half of it. A capture is confirmed the same way a typed level
        // is — by the timer reporting it on the signal feed — and once it is, the level becomes
        // GridFPV's own value on `Timer::calibration`, exactly as a typed one is. It is NOT left as
        // something read back off the timer.
        let (registry, _state, _) = state_with(vec![]);
        let rh = connected_rh_timer_selected_by_the_event(&registry);
        report_levels(&registry, &rh.id, &[(90.0, 80.0)]);

        let (status, _) = post_capture(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 0, "threshold": "enter" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Nothing is credited before RotorHazard's window closes: it has not computed a level yet,
        // so a threshold that moved in those three seconds moved for some other reason.
        report_levels(&registry, &rh.id, &[(118.0, 80.0)]);
        assert!(
            registry.timers().resolve_captures().is_empty(),
            "a capture must not settle before its sampling window has closed"
        );
        assert!(registry.timers().calibration(&rh.id).is_empty());

        // …and once it has.
        std::thread::sleep(std::time::Duration::from_millis(
            u64::from(crate::timers::CAPTURE_WINDOW_MS) + 20,
        ));
        report_levels(&registry, &rh.id, &[(118.0, 80.0)]);
        let settled = registry.timers().resolve_captures();
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].node, 0);
        assert_eq!(settled[0].level, Some(118));

        // D27: GridFPV holds the value, not merely the timer.
        assert_eq!(
            registry.timers().calibration(&rh.id),
            vec![crate::timers::NodeCalibration {
                node: 0,
                enter_at: Some(118),
                exit_at: None,
            }]
        );
        // Settled exactly once — a resolved capture is retired, not re-reported every tick.
        assert!(registry.timers().resolve_captures().is_empty());
    }

    #[tokio::test]
    async fn a_capture_that_does_not_land_is_reported_and_records_nothing() {
        // RotorHazard refuses a capture — a node that is not answering, or one already capturing —
        // by returning False and emitting nothing at all. So an unchanged level is the ONLY evidence
        // of that refusal there is, and inventing a recorded level to fill the gap would be the
        // fabricated success this whole control exists to avoid.
        let (registry, _state, _) = state_with(vec![]);
        let rh = connected_rh_timer_selected_by_the_event(&registry);
        report_levels(&registry, &rh.id, &[(90.0, 80.0)]);

        let (status, _) = post_capture(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 0, "threshold": "enter" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        std::thread::sleep(std::time::Duration::from_millis(
            u64::from(crate::timers::CAPTURE_WINDOW_MS)
                + u64::from(crate::timers::CAPTURE_SETTLE_MS)
                + 20,
        ));
        report_levels(&registry, &rh.id, &[(90.0, 80.0)]); // unchanged, as RotorHazard left it
        let settled = registry.timers().resolve_captures();
        assert_eq!(settled.len(), 1);
        assert_eq!(
            settled[0].level, None,
            "a capture that produced no new level must be reported as such, never as a success"
        );
        assert!(
            registry.timers().calibration(&rh.id).is_empty(),
            "nothing may be recorded for a capture that did not land"
        );
        // And it is retired, so a later capture on the same threshold is not refused by a ghost.
        assert!(registry.timers().resolve_captures().is_empty());
        let (status, _) = post_capture(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 0, "threshold": "enter" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn capturing_is_refused_while_a_scored_race_is_in_progress_on_the_timer() {
        // A capture ends by SETTING the threshold, so it moves a detector mid-race exactly as a
        // typed level does. Same gate as the calibration write, and the refusal names the heat, the
        // timer, and the fact that the heat is *scored* — the thing that distinguishes it from open
        // practice, which is capturable while it runs.
        for transition in [
            HeatTransition::Staged,
            HeatTransition::Armed,
            HeatTransition::Running,
            HeatTransition::Finished, // → Unofficial
        ] {
            let (registry, state, _) = state_with(vec![]);
            let rh = connected_rh_timer_selected_by_the_event(&registry);
            state
                .append(
                    Event::HeatScheduled {
                        heat: HeatId("q-1".into()),
                        lineup: vec![CompetitorRef("A".into())],
                        class: None,
                        round: None,
                        frequencies: vec![],
                        label: Some("Qualifier Heat 1".into()),
                    },
                    None,
                )
                .unwrap();
            for t in [
                HeatTransition::Staged,
                HeatTransition::Armed,
                HeatTransition::Running,
                HeatTransition::Finished,
            ] {
                state
                    .append(
                        Event::HeatStateChanged {
                            heat: HeatId("q-1".into()),
                            transition: t,
                        },
                        None,
                    )
                    .unwrap();
                if t == transition {
                    break;
                }
            }

            let (status, bytes) = post_capture(
                registry.clone(),
                &rh.id.0,
                json!({ "node": 0, "threshold": "enter" }),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "a {transition:?} heat must refuse the capture"
            );
            let message = refusal(&bytes);
            assert!(
                message.contains("Qualifier Heat 1") && message.contains("Field RH"),
                "the refusal must name the heat and the timer: {message}"
            );
            assert!(
                message.contains("scored heat"),
                "the refusal must say the heat is SCORED — open practice is capturable: {message}"
            );
            assert!(
                !message.contains(&rh.id.0),
                "the refusal must not leak the raw timer id: {message}"
            );
            // A real refusal: nothing queued, and no capture left outstanding to block the next one.
            assert!(registry.timers().take_capture_requests().is_empty());
            assert!(!registry.timers().capture_in_flight(&rh.id));
        }
    }

    #[tokio::test]
    async fn capturing_is_accepted_while_an_open_practice_heat_is_running() {
        // #398, and sharper here than for a typed level: the pass a capture NEEDS is one a pilot is
        // already flying. Refusing during practice would leave the RD waving a quad through an idle
        // gate by hand — the RotorHazard-UI loop this page exists to replace.
        for transition in [
            HeatTransition::Staged,
            HeatTransition::Armed,
            HeatTransition::Running,
            HeatTransition::Finished, // → Unofficial
        ] {
            let (registry, state, _) = state_with(vec![]);
            let rh = connected_rh_timer_selected_by_the_event(&registry);
            let round = registry
                .add_round(
                    &sole_event(&registry),
                    NewRoundReq {
                        layouts: Vec::new(),
                        label: "Practice".into(),
                        classes: vec![],
                        format: gridfpv_engine::format::OpenPractice::NAME.to_string(),
                        params: std::collections::BTreeMap::new(),
                        win_condition: None,
                        seeding: SeedingRule::ActiveNodes { nodes: vec![0] },
                        time_limit_secs: None,
                        channel_mode: None,
                        staging_timer_secs: None,
                        start_procedure: None,
                        grace_window: None,
                        protest_window: None,
                        min_lap_secs: None,
                    },
                )
                .expect("an open-practice round");
            state
                .append(
                    Event::HeatScheduled {
                        heat: HeatId("p-1".into()),
                        lineup: vec![CompetitorRef("node-0".into())],
                        class: None,
                        round: Some(round.id.clone()),
                        frequencies: vec![],
                        label: Some("Practice Heat 1".into()),
                    },
                    None,
                )
                .unwrap();
            for t in [
                HeatTransition::Staged,
                HeatTransition::Armed,
                HeatTransition::Running,
                HeatTransition::Finished,
            ] {
                state
                    .append(
                        Event::HeatStateChanged {
                            heat: HeatId("p-1".into()),
                            transition: t,
                        },
                        None,
                    )
                    .unwrap();
                if t == transition {
                    break;
                }
            }

            let (status, _) = post_capture(
                registry.clone(),
                &rh.id.0,
                json!({ "node": 0, "threshold": "enter" }),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::OK,
                "an open-practice heat in {transition:?} must NOT block a capture"
            );
            // …and it must actually reach the wire: without the stamp the driver's armed-heat
            // backstop would drop a capture the route deliberately allowed.
            let drained = registry.timers().take_capture_requests();
            assert_eq!(drained.len(), 1);
            assert!(
                drained[0].during_open_practice,
                "the capture must be stamped as cleared against an open-practice heat"
            );
        }
    }

    #[tokio::test]
    async fn capturing_a_mock_an_unknown_a_disconnected_timer_or_a_disabled_node_is_refused() {
        // Every refusal the calibration write has, for the same reasons — and #412 in particular:
        // RotorHazard drops an out-of-range seat index with nothing but a log line, so offering a
        // capture on a node that is not there (or one the RD switched off) would produce a control
        // that looks like it worked and measured nothing.
        let (registry, _state, _) = state_with(vec![]);

        let (status, bytes) = post_capture(
            registry.clone(),
            "mock",
            json!({ "node": 0, "threshold": "enter" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let message = refusal(&bytes);
        assert!(
            message.contains("Mock") && message.contains("not a RotorHazard timer"),
            "the Mock refusal must name the timer and say why: {message}"
        );

        let (status, _) = post_capture(
            registry.clone(),
            "no-such-timer",
            json!({ "node": 0, "threshold": "enter" }),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let disconnected = registry
            .timers()
            .create(&CreateTimerRequest {
                name: "Bench RH".into(),
                kind: TimerKind::Rotorhazard {
                    url: "http://rh.local:5000".into(),
                },
                channel_capability: None,
                node_count: None,
                available_channels: None,
            })
            .unwrap();
        let (status, bytes) = post_capture(
            registry.clone(),
            &disconnected.id.0,
            json!({ "node": 0, "threshold": "enter" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(refusal(&bytes).contains("Bench RH"));

        let rh = connected_rh_timer_selected_by_the_event(&registry);
        let width = registry.timers().get(&rh.id).unwrap().node_width();
        let (status, bytes) = post_capture(
            registry.clone(),
            &rh.id.0,
            json!({ "node": width, "threshold": "enter" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            refusal(&bytes).contains(&format!("Node {}", width + 1)),
            "the refusal must name the node the way the page labels it (1-based)"
        );

        registry
            .timers()
            .set_nodes(
                &rh.id,
                &crate::timers::SetTimerNodesRequest {
                    node_count: None,
                    enabled: Some((0..width).filter(|n| *n != 2).collect()),
                },
            )
            .unwrap();
        let (status, bytes) = post_capture(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 2, "threshold": "enter" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let message = refusal(&bytes);
        assert!(
            message.contains("Node 3") && message.contains("disabled"),
            "the disabled-node refusal must name the node 1-based and say why: {message}"
        );
        assert!(registry.timers().take_capture_requests().is_empty());
    }

    /// `POST /timers/{id}/channel` with a raw JSON body → status + raw bytes (#413).
    async fn post_channel(
        registry: EventRegistry,
        timer_id: &str,
        body: serde_json::Value,
    ) -> (StatusCode, Vec<u8>) {
        let response = router(registry)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/timers/{timer_id}/channel"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, bytes.to_vec())
    }

    #[tokio::test]
    async fn setting_a_node_channel_queues_it_with_band_and_channel_and_records_it_as_grid_config()
    {
        // #413: the Tune page's other write. Two things matter here and they are separate.
        //
        // 1. The BAND AND CHANNEL travel with the frequency. RotorHazard's `on_set_frequency` stores
        //    them on the active profile, and the RD validates this work by refreshing RotorHazard's
        //    own page — where a bare `5880` with no `R7` beside it reads as "it half worked". The
        //    label is resolved from GridFPV's OWN catalog, never trusted from the wire.
        // 2. D27: the accepted channel is recorded on the timer, because a channel the RD picked is
        //    GridFPV's config; the timer is only where it takes effect.
        let (registry, _state, _) = state_with(vec![]);
        let rh = connected_rh_timer_selected_by_the_event(&registry);

        let (status, bytes) = post_channel(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 1, "mhz": 5880, "band": "Raceband", "channel": "R7" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let dispatch: crate::timers::ChannelDispatch = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(dispatch.timer, rh.id);
        assert_eq!(dispatch.node, 1);
        assert_eq!(dispatch.mhz, 5880);
        assert_eq!(dispatch.band.as_deref(), Some("Raceband"));
        assert_eq!(dispatch.channel.as_deref(), Some("R7"));
        // Nothing was on this node before, and no thresholds are held for it — so there is nothing
        // stale to announce yet.
        assert_eq!(dispatch.previous_mhz, None);
        assert!(!dispatch.thresholds_tuned_on_another_channel);

        assert_eq!(
            registry.timers().node_channels(&rh.id),
            vec![crate::timers::NodeChannel {
                node: 1,
                mhz: 5880,
                band: Some("Raceband".into()),
                channel: Some("R7".into()),
            }]
        );

        // The queue drains EXACTLY ONCE, carrying the label onto the wire.
        let drained = registry.timers().take_channel_requests();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].timer, rh.id);
        assert_eq!(drained[0].node, 1);
        assert_eq!(drained[0].mhz, 5880);
        assert_eq!(drained[0].band.as_deref(), Some("Raceband"));
        assert_eq!(drained[0].channel.as_deref(), Some("R7"));
        assert!(
            registry.timers().take_channel_requests().is_empty(),
            "a second drain is empty — nothing is re-queued"
        );
    }

    #[tokio::test]
    async fn a_channel_label_is_resolved_from_gridfpvs_own_catalog_not_taken_from_the_client() {
        // D27: GridFPV owns the vocabulary. A client-supplied `(band, channel, mhz)` triple is
        // honoured only when the catalog actually holds it — which is what lets a caller name
        // `Fatshark F8` for the frequency the console leads as `Raceband R7` — and an invented one
        // falls back to the catalog's own answer rather than reaching the timer. A custom MHz travels with NO label at all,
        // because it has none: a made-up name on RotorHazard's screen is worse than the number.
        let (registry, _state, _) = state_with(vec![]);
        let rh = connected_rh_timer_selected_by_the_event(&registry);

        // A real, deliberately-chosen alternative band for a coincident frequency. 5880 is both
        // Raceband R7 and Fatshark F8; the console's picker leads with Raceband and carries `(F8)`
        // in the label, but the API still honours the alternative name when a caller sends it.
        let (status, bytes) = post_channel(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 0, "mhz": 5880, "band": "Fatshark", "channel": "F8" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let dispatch: crate::timers::ChannelDispatch = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(dispatch.band.as_deref(), Some("Fatshark"));

        // An invented label is replaced by the catalog's, not forwarded.
        let (status, bytes) = post_channel(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 0, "mhz": 5880, "band": "Nonsense", "channel": "ZZ9" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let dispatch: crate::timers::ChannelDispatch = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(dispatch.band.as_deref(), Some("Raceband"));
        assert_eq!(dispatch.channel.as_deref(), Some("R7"));

        // A custom raw MHz the catalog does not know: the frequency alone, and no invented label.
        let (status, bytes) = post_channel(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 0, "mhz": 5891 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let dispatch: crate::timers::ChannelDispatch = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(dispatch.mhz, 5891);
        assert_eq!(dispatch.band, None);
        assert_eq!(dispatch.channel, None);
    }

    #[tokio::test]
    async fn a_channel_change_reports_that_the_thresholds_were_tuned_on_the_previous_channel() {
        // The thing nothing else announces (#413). RotorHazard's `on_set_frequency` writes the
        // frequency into the CURRENT PROFILE — the same row that holds `enter_ats`/`exit_ats`. So
        // changing a node's channel leaves its thresholds exactly where they were, tuned for the
        // channel it just left: the levels read unchanged and therefore fine, while the gate now
        // detects on numbers never calibrated for the frequency it is on.
        //
        // The Director is the only party that can say so — it holds the record of what GridFPV set.
        // Reported, never acted on: the levels are deliberately untouched (recalling saved
        // per-channel levels is #411).
        let (registry, _state, _) = state_with(vec![]);
        let rh = connected_rh_timer_selected_by_the_event(&registry);

        // A node on R7, then tuned.
        let (status, _) = post_channel(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 0, "mhz": 5880, "band": "Raceband", "channel": "R7" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = post_calibration(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 0, "enter_at": 92, "exit_at": 84 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Now move it to a different channel.
        let (status, bytes) = post_channel(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 0, "mhz": 5800, "band": "Fatshark", "channel": "F4" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let dispatch: crate::timers::ChannelDispatch = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(dispatch.previous_mhz, Some(5880));
        assert!(
            dispatch.thresholds_tuned_on_another_channel,
            "the levels were tuned on R7 and this node is now on F4 — the RD has to be told"
        );

        // The thresholds themselves are UNTOUCHED: GridFPV changed one thing, so one thing changed.
        assert_eq!(
            registry.timers().calibration(&rh.id),
            vec![crate::timers::NodeCalibration {
                node: 0,
                enter_at: Some(92),
                exit_at: Some(84),
            }]
        );

        // Re-picking the channel it is already on is not "stale" — nothing moved.
        let (status, bytes) = post_channel(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 0, "mhz": 5800, "band": "Fatshark", "channel": "F4" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let dispatch: crate::timers::ChannelDispatch = serde_json::from_slice(&bytes).unwrap();
        assert!(!dispatch.thresholds_tuned_on_another_channel);
    }

    #[tokio::test]
    async fn a_flexible_timer_with_an_empty_channel_pool_still_accepts_any_catalog_channel() {
        // The trap this feature is built around (#413). Both real RotorHazard timers on the bench
        // report `channel_capability: "Flexible"` with an EMPTY `available_channels`, which means
        // "no restriction" — it is the per-heat allocation POOL, not a capability. A Director that
        // read it as a restriction would refuse every channel on precisely the timers this is for.
        let (registry, _state, _) = state_with(vec![]);
        let rh = connected_rh_timer_selected_by_the_event(&registry);
        assert!(
            registry
                .timers()
                .get(&rh.id)
                .unwrap()
                .available_channels
                .is_empty(),
            "the fixture models the bench: a Flexible RH with nothing configured in its pool"
        );

        let (status, _) = post_channel(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 0, "mhz": 5658, "band": "Raceband", "channel": "R1" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "an empty pool is not a restriction");
    }

    #[tokio::test]
    async fn a_fixed_timer_refuses_a_channel_outside_its_declared_set() {
        // The other half of the capability: a Fixed timer supports what it supports, and the refusal
        // names the channel the way the RD reads it (CLAUDE.md), never as a bare number.
        let (registry, _state, _) = state_with(vec![]);
        let rh = registry
            .timers()
            .create(&CreateTimerRequest {
                name: "Fixed RH".into(),
                kind: TimerKind::Rotorhazard {
                    url: "http://rh.local:5000".into(),
                },
                channel_capability: Some(crate::timers::ChannelCapability::Fixed {
                    channels: vec![5658, 5695],
                }),
                node_count: None,
                available_channels: None,
            })
            .unwrap();
        registry
            .timers()
            .set_status(&rh.id, crate::timers::TimerStatus::Connected);

        let (status, bytes) = post_channel(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 0, "mhz": 5880, "band": "Raceband", "channel": "R7" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let message = refusal(&bytes);
        assert!(
            message.contains("Raceband R7") && message.contains("Fixed RH"),
            "the refusal must name the channel and the timer by their friendly names: {message}"
        );
        assert!(
            !message.contains("5880"),
            "a bare frequency must never reach an RD (CLAUDE.md): {message}"
        );

        // A channel it DOES support goes through.
        let (status, _) = post_channel(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 0, "mhz": 5658, "band": "Raceband", "channel": "R1" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn setting_a_channel_is_refused_while_a_scored_race_is_in_progress_on_the_timer() {
        // Retuning a node's receiver under a SCORED race takes the gate off the channel the pilot is
        // flying — at least as disruptive as moving a threshold, so the same hard gate applies.
        for transition in [
            HeatTransition::Staged,
            HeatTransition::Armed,
            HeatTransition::Running,
            HeatTransition::Finished, // → Unofficial
        ] {
            let (registry, state, _) = state_with(vec![]);
            let rh = connected_rh_timer_selected_by_the_event(&registry);
            state
                .append(
                    Event::HeatScheduled {
                        heat: HeatId("q-1".into()),
                        lineup: vec![CompetitorRef("A".into())],
                        class: None,
                        round: None,
                        frequencies: vec![],
                        label: Some("Qualifier Heat 1".into()),
                    },
                    None,
                )
                .unwrap();
            for t in [
                HeatTransition::Staged,
                HeatTransition::Armed,
                HeatTransition::Running,
                HeatTransition::Finished,
            ] {
                state
                    .append(
                        Event::HeatStateChanged {
                            heat: HeatId("q-1".into()),
                            transition: t,
                        },
                        None,
                    )
                    .unwrap();
                if t == transition {
                    break;
                }
            }

            let (status, bytes) = post_channel(
                registry.clone(),
                &rh.id.0,
                json!({ "node": 0, "mhz": 5880, "band": "Raceband", "channel": "R7" }),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "a {transition:?} scored heat must refuse the channel change"
            );
            let message = refusal(&bytes);
            assert!(
                message.contains("Field RH")
                    && message.contains("Qualifier Heat 1")
                    && message.contains("scored"),
                "the refusal must name the timer and the heat, and say the heat is scored: \
                 {message}"
            );
            // Nothing queued and nothing recorded: a refusal is a refusal on both halves.
            assert!(registry.timers().take_channel_requests().is_empty());
            assert!(registry.timers().node_channels(&rh.id).is_empty());
        }
    }

    #[tokio::test]
    async fn setting_a_channel_is_accepted_while_an_open_practice_heat_is_running() {
        // #398's exemption, applied to #413: practice is excluded from scoring, so there is no
        // result a retune can corrupt — and pilots in the air is exactly when an RD is checking
        // whether the gate is on the right channel at all.
        for transition in [
            HeatTransition::Staged,
            HeatTransition::Armed,
            HeatTransition::Running,
            HeatTransition::Finished, // → Unofficial
        ] {
            let (registry, state, _) = state_with(vec![]);
            let rh = connected_rh_timer_selected_by_the_event(&registry);
            let round = registry
                .add_round(
                    &sole_event(&registry),
                    NewRoundReq {
                        layouts: Vec::new(),
                        label: "Practice".into(),
                        classes: vec![],
                        format: gridfpv_engine::format::OpenPractice::NAME.to_string(),
                        params: std::collections::BTreeMap::new(),
                        win_condition: None,
                        seeding: SeedingRule::ActiveNodes { nodes: vec![0] },
                        time_limit_secs: None,
                        channel_mode: None,
                        staging_timer_secs: None,
                        start_procedure: None,
                        grace_window: None,
                        protest_window: None,
                        min_lap_secs: None,
                    },
                )
                .expect("an open-practice round");
            state
                .append(
                    Event::HeatScheduled {
                        heat: HeatId("p-1".into()),
                        lineup: vec![CompetitorRef("node-0".into())],
                        class: None,
                        round: Some(round.id.clone()),
                        frequencies: vec![],
                        label: Some("Practice Heat 1".into()),
                    },
                    None,
                )
                .unwrap();
            for t in [
                HeatTransition::Staged,
                HeatTransition::Armed,
                HeatTransition::Running,
                HeatTransition::Finished,
            ] {
                state
                    .append(
                        Event::HeatStateChanged {
                            heat: HeatId("p-1".into()),
                            transition: t,
                        },
                        None,
                    )
                    .unwrap();
                if t == transition {
                    break;
                }
            }

            let (status, _) = post_channel(
                registry.clone(),
                &rh.id.0,
                json!({ "node": 0, "mhz": 5880, "band": "Raceband", "channel": "R7" }),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::OK,
                "an open-practice heat in {transition:?} must NOT block a channel change"
            );
            // …and it must reach the wire: the write carries the route's finding, so the driver's
            // own armed-heat backstop lets it through rather than silently dropping it.
            let drained = registry.timers().take_channel_requests();
            assert_eq!(drained.len(), 1);
            assert!(
                drained[0].during_open_practice,
                "the write must be stamped as cleared against an open-practice heat"
            );
        }
    }

    #[tokio::test]
    async fn a_channel_write_to_a_mock_a_disabled_node_or_an_impossible_frequency_is_refused() {
        // RotorHazard validates `0 <= node_index < num_nodes` and otherwise writes nothing but a log
        // line — so an out-of-range write would look accepted and land nowhere, which is exactly the
        // failure the Tune page exists to remove. A DISABLED node (#412) is refused for the same
        // reason it refuses a threshold: no heat is ever seated there.
        let (registry, _state, _) = state_with(vec![]);

        // A Mock has no receiver to tune.
        let (status, bytes) =
            post_channel(registry.clone(), "mock", json!({ "node": 0, "mhz": 5880 })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(refusal(&bytes).contains("not a RotorHazard timer"));

        // An unknown id is a 404, never a message about a timer that does not exist.
        let (status, _) = post_channel(
            registry.clone(),
            "no-such-timer",
            json!({ "node": 0, "mhz": 5880 }),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let rh = connected_rh_timer_selected_by_the_event(&registry);
        let width = registry.timers().get(&rh.id).unwrap().node_width();

        // Beyond the width.
        let (status, bytes) = post_channel(
            registry.clone(),
            &rh.id.0,
            json!({ "node": width, "mhz": 5880 }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let message = refusal(&bytes);
        assert!(
            // 1-based on screen, per the repo display rule.
            message.contains(&format!("Node {}", width + 1)),
            "the refusal must name the node the way the page labels it: {message}"
        );

        // Disabled by the RD.
        registry
            .timers()
            .set_nodes(
                &rh.id,
                &crate::timers::SetTimerNodesRequest {
                    node_count: None,
                    enabled: Some((0..width).filter(|n| *n != 2).collect()),
                },
            )
            .unwrap();
        let (status, bytes) = post_channel(
            registry.clone(),
            &rh.id.0,
            json!({ "node": 2, "mhz": 5880 }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let message = refusal(&bytes);
        assert!(
            message.contains("Node 3") && message.contains("disabled"),
            "the disabled-node refusal must name the node 1-based and say why: {message}"
        );

        // `frequency: 0` is a real RotorHazard command — it tunes the node to NOTHING, silently
        // switching a gate off. No dropdown should be able to send it by accident.
        let (status, bytes) =
            post_channel(registry.clone(), &rh.id.0, json!({ "node": 0, "mhz": 0 })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(refusal(&bytes).contains("5.8 GHz"));

        assert!(registry.timers().take_channel_requests().is_empty());
        assert!(registry.timers().node_channels(&rh.id).is_empty());
    }

    #[tokio::test]
    async fn two_nodes_on_the_same_channel_is_allowed_because_a_swap_looks_exactly_like_that() {
        // Flagged by the console, never blocked by the Director (#413). Two gates on one frequency
        // both see the same craft, which is wrong for a race — but it is also precisely what a bench
        // swap looks like halfway through, and refusing it would block the legitimate case to
        // prevent a recoverable one.
        let (registry, _state, _) = state_with(vec![]);
        let rh = connected_rh_timer_selected_by_the_event(&registry);

        for node in [0, 1] {
            let (status, _) = post_channel(
                registry.clone(),
                &rh.id.0,
                json!({ "node": node, "mhz": 5880, "band": "Raceband", "channel": "R7" }),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
        }
        assert_eq!(registry.timers().take_channel_requests().len(), 2);
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

    /// `PUT /events/{id}/timers` → status + body bytes (the shared driver for the #405 gate tests).
    async fn put_event_timers(
        registry: EventRegistry,
        event_id: &str,
        ids: Vec<crate::timers::TimerId>,
    ) -> (StatusCode, Vec<u8>) {
        let req = SetEventTimersRequest { ids, primary: None };
        let response = router(registry)
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/events/{event_id}/timers"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(serde_json::to_string(&req).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, bytes.to_vec())
    }

    /// Create a RotorHazard timer in `registry` named `name` (unprobed — `plugin: None`).
    fn create_rh_timer(registry: &EventRegistry, name: &str) -> Timer {
        registry
            .timers()
            .create(&CreateTimerRequest {
                name: name.into(),
                kind: TimerKind::Rotorhazard {
                    url: "http://rh.local:5000".into(),
                },
                channel_capability: None,
                node_count: None,
                available_channels: None,
            })
            .unwrap()
    }

    /// The `PluginPresence::Present` a healthy `gridfpv_hello` probe records.
    fn present_plugin() -> crate::timers::PluginPresence {
        crate::timers::PluginPresence::Present {
            plugin_version: "0.1.0".into(),
            rhapi_version: "1.4".into(),
            capabilities: vec!["hello".into()],
        }
    }

    #[tokio::test]
    async fn selecting_an_rh_timer_without_the_plugin_is_refused_with_the_reason() {
        // #405: the gate is at **event timer selection**, and it lives in the API — this route is
        // reachable directly, so a rule enforced only in the console's picker is not enforced.
        // Each presence gets its own message: three problems, three fixes.
        let (registry, _state, _) = state_with(vec![]);
        let rh = create_rh_timer(&registry, "Field RH");

        // `plugin: None` — never probed. "Connect this timer first", NOT "plugin missing":
        // presence is only knowable over a live socket, so this is the normal state of a freshly
        // added timer, and installing a plugin is not the fix.
        let (status, body) = put_event_timers(
            registry.clone(),
            &sole_event(&registry).0,
            vec![rh.id.clone()],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err: ProtocolError = serde_json::from_slice(&body).unwrap();
        assert!(err.message.contains("Field RH"), "{}", err.message);
        assert!(err.message.contains("Connect it"), "{}", err.message);
        assert!(
            !err.message.contains(&rh.id.0),
            "no raw id: {}",
            err.message
        );
        // Nothing was recorded.
        assert!(
            !registry
                .timers_of(&sole_event(&registry))
                .unwrap()
                .contains(&rh.id)
        );

        // Probed, no plugin → the guided install.
        registry
            .timers()
            .set_plugin(&rh.id, crate::timers::PluginPresence::Missing);
        let (status, body) = put_event_timers(
            registry.clone(),
            &sole_event(&registry).0,
            vec![rh.id.clone()],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err: ProtocolError = serde_json::from_slice(&body).unwrap();
        assert!(
            err.message.contains("not running the GridFPV plugin"),
            "{}",
            err.message
        );
        assert!(err.message.contains("Install it"), "{}", err.message);

        // Probed, wrong protocol → update it.
        registry.timers().set_plugin(
            &rh.id,
            crate::timers::PluginPresence::Incompatible {
                plugin_version: "0.0.1".into(),
                protocol_version: 99,
                reason: "protocol 99".into(),
            },
        );
        let (status, body) = put_event_timers(
            registry.clone(),
            &sole_event(&registry).0,
            vec![rh.id.clone()],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err: ProtocolError = serde_json::from_slice(&body).unwrap();
        assert!(err.message.contains("Update it"), "{}", err.message);

        // Present → selectable. This is what makes #383's Connect load-bearing rather than a
        // diagnostic convenience: a timer becomes selectable only after it has been connected.
        registry.timers().set_plugin(&rh.id, present_plugin());
        let (status, body) = put_event_timers(
            registry.clone(),
            &sole_event(&registry).0,
            vec![rh.id.clone()],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let meta: EventMeta = serde_json::from_slice(&body).unwrap();
        assert_eq!(meta.timers, vec![rh.id]);
    }

    #[tokio::test]
    async fn the_plugin_gate_never_touches_mock_timers() {
        // Mock timers are unaffected (#405) — they have no plugin to require, and gating them
        // would break the out-of-the-box sim race.
        let (registry, _state, _) = state_with(vec![]);
        let (status, _) = put_event_timers(
            registry.clone(),
            &sole_event(&registry).0,
            vec![crate::timers::TimerId(crate::timers::MOCK_TIMER_ID.into())],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn an_already_selected_plugin_less_timer_stays_saveable() {
        // "Do not break existing events" (#405). An event persisted before this rule may already
        // select a plugin-less RH timer. The gate applies to *newly* selected ids only, so the RD
        // can still edit that event's selection — including the console's wholesale auto-save,
        // which resends the whole selection on every toggle. What stops it from actually racing is
        // the arm-time backstop, not a refusal to save.
        let (registry, _state, _) = state_with(vec![]);
        let rh = create_rh_timer(&registry, "Field RH");
        let event = sole_event(&registry);
        // Simulate the persisted-before-the-rule state by writing the selection past the route.
        registry.set_timers(&event, vec![rh.id.clone()]).unwrap();
        registry
            .timers()
            .set_plugin(&rh.id, crate::timers::PluginPresence::Missing);

        // Re-sending the existing selection, and adding a Mock alongside it, both succeed.
        let mock = crate::timers::TimerId(crate::timers::MOCK_TIMER_ID.into());
        let (status, _) = put_event_timers(
            registry.clone(),
            &sole_event(&registry).0,
            vec![rh.id.clone()],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, body) = put_event_timers(
            registry.clone(),
            &sole_event(&registry).0,
            vec![rh.id.clone(), mock.clone()],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let meta: EventMeta = serde_json::from_slice(&body).unwrap();
        assert_eq!(meta.timers, vec![rh.id.clone(), mock.clone()]);

        // But once the RD drops it, re-selecting it is a fresh selection — and refused.
        let (status, _) = put_event_timers(
            registry.clone(),
            &sole_event(&registry).0,
            vec![mock.clone()],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = put_event_timers(
            registry.clone(),
            &sole_event(&registry).0,
            vec![mock, rh.id],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
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
        let uri = event_uri(&registry, "/timers");
        let response = router(registry.clone())
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(uri)
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
        let uri = event_uri(&registry, "/timers");
        let response = router(registry)
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(uri)
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

    // --- P2: timer config validation ----------------------------------------

    #[tokio::test]
    async fn post_timer_rejects_zero_node_count() {
        let (registry, _state, _) = state_with(vec![]);
        let body = CreateTimerRequest {
            name: "Zero".into(),
            kind: TimerKind::Mock {
                laps: 1,
                lap_ms: 50,
            },
            channel_capability: None,
            node_count: Some(0),
            available_channels: None,
        };
        let (status, _) = post_timer(registry, &body, None).await;
        // A 0-node timer caps every heat to no pilots — rejected as a 400, not silently created.
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    // --- P1-5: membership is scoped to the event's roster + class selection --

    /// Seed a class C (directory + **selected**) and pilot P (directory + **roster**) on the
    /// registry's single event, returning their ids alongside an extra pilot Q and class D that
    /// are in the directory but *not* on the roster / selection.
    fn membership_fixture(
        registry: &EventRegistry,
    ) -> (ClassId, ClassId, PilotId, PilotId, EventId) {
        let event = sole_event(registry);
        let class_c = registry
            .classes()
            .create(&CreateClassRequest {
                name: "Open".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        let class_d = registry
            .classes()
            .create(&CreateClassRequest {
                name: "Unselected".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        let pilot_p = registry
            .pilots()
            .create(&CreatePilotRequest {
                callsign: "Rostered".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        let pilot_q = registry
            .pilots()
            .create(&CreatePilotRequest {
                callsign: "Outsider".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        registry.set_classes(&event, vec![class_c.clone()]).unwrap();
        registry.set_roster(&event, vec![pilot_p.clone()]).unwrap();
        (class_c, class_d, pilot_p, pilot_q, event)
    }

    async fn put_membership(
        registry: EventRegistry,
        event: &EventId,
        class: &ClassId,
        pilots: Vec<PilotId>,
    ) -> StatusCode {
        let body = SetClassMembershipRequest {
            pilots: pilots.into_iter().map(MemberSlot::new).collect(),
        };
        let response = router(registry)
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!(
                        "/events/{}/classes/{}/membership",
                        event.0, class.0
                    ))
                    .header("Content-Type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        response.status()
    }

    #[tokio::test]
    async fn put_membership_rejects_non_roster_pilot_and_non_selected_class() {
        let (registry, _state, _) = state_with(vec![]);
        let (class_c, class_d, pilot_p, pilot_q, event) = membership_fixture(&registry);

        // Happy path: a selected class + a rostered pilot is accepted.
        assert_eq!(
            put_membership(registry.clone(), &event, &class_c, vec![pilot_p.clone()]).await,
            StatusCode::OK
        );

        // A pilot in the directory but NOT on the event roster → 400.
        assert_eq!(
            put_membership(registry.clone(), &event, &class_c, vec![pilot_q]).await,
            StatusCode::BAD_REQUEST
        );

        // A class in the directory but NOT selected by the event → 400.
        assert_eq!(
            put_membership(registry, &event, &class_d, vec![pilot_p]).await,
            StatusCode::BAD_REQUEST
        );
    }

    // --- #117 S2: event channel layouts over the wire -------------------------

    /// `POST /events/{id}/layouts` with `body`, returning the status and the parsed JSON body.
    async fn post_layout(
        registry: EventRegistry,
        event: &EventId,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let response = router(registry)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/events/{}/layouts", event.0))
                    .header("Content-Type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    #[tokio::test]
    async fn defining_a_layout_seeds_it_from_the_timers_allowed_set() {
        // The global→event seam over the wire: no `nodes` in the body, and the Director seeds the
        // layout from what the RD ticked for this timer on the Timers page.
        let (registry, _state, _) = state_with(vec![]);
        let event = sole_event(&registry);
        let (status, body) =
            post_layout(registry.clone(), &event, json!({ "name": "Bracket A" })).await;
        assert_eq!(status, StatusCode::OK);
        let layouts = body["layouts"].as_array().unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0]["name"], "Bracket A");
        // The Mock's eight Raceband channels, one per node, node index ascending.
        let nodes = layouts[0]["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 8);
        assert_eq!(nodes[0]["node"], 0);
        assert_eq!(nodes[0]["channel"], 5658);
        assert_eq!(nodes[7]["node"], 7);
        assert_eq!(nodes[7]["channel"], 5917);
        // The `GET` sees the same thing (it is the same view type).
        let response = router(registry)
            .oneshot(
                Request::builder()
                    .uri(format!("/events/{}/layouts", event.0))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let read: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(read["layouts"], body["layouts"]);
    }

    #[tokio::test]
    async fn a_layout_with_two_nodes_on_one_channel_is_a_400_naming_both_nodes() {
        let (registry, _state, _) = state_with(vec![]);
        let event = sole_event(&registry);
        let (status, body) = post_layout(
            registry,
            &event,
            json!({
                "name": "Bracket A",
                "nodes": [
                    { "node": 0, "channel": 5658 },
                    { "node": 1, "channel": 5658 },
                    { "node": 2, "channel": 5732 },
                    { "node": 3, "channel": 5769 },
                    { "node": 4, "channel": 5806 },
                    { "node": 5, "channel": 5843 },
                    { "node": 6, "channel": 5880 },
                    { "node": 7, "channel": 5917 }
                ]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let message = body["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("Node 1") && message.contains("Node 2"),
            "{message}"
        );
        assert!(message.contains("Raceband R1"), "{message}");
    }

    #[tokio::test]
    async fn cross_layout_overlap_comes_back_as_a_warning_on_a_200() {
        // The RD's call: reuse is flagged, never refused. Two identically-seeded layouts both land.
        let (registry, _state, _) = state_with(vec![]);
        let event = sole_event(&registry);
        let (first, _) =
            post_layout(registry.clone(), &event, json!({ "name": "Bracket A" })).await;
        assert_eq!(first, StatusCode::OK);
        let (status, body) = post_layout(registry, &event, json!({ "name": "Bracket B" })).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "an overlap must not block the write"
        );
        assert_eq!(body["layouts"].as_array().unwrap().len(), 2);
        let overlaps = body["overlaps"].as_array().unwrap();
        assert_eq!(overlaps.len(), 1);
        assert_eq!(overlaps[0]["channels"].as_array().unwrap().len(), 8);
    }

    #[tokio::test]
    async fn layout_routes_404_an_unknown_event_and_an_unknown_layout() {
        let (registry, _state, _) = state_with(vec![]);
        let missing = EventId("nope".into());
        let (status, _) = post_layout(registry.clone(), &missing, json!({ "name": "X" })).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let event = sole_event(&registry);
        let response = router(registry)
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/events/{}/layouts/never-existed", event.0))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    // --- P1-7: a registry I/O failure maps to a 500, not a 404/400 ----------

    #[test]
    fn registry_error_kinds_map_to_the_right_status() {
        use crate::events::RegistryError;
        // An I/O / persistence failure is a 500 — the load-bearing case: in-memory state was already
        // mutated, so it must NOT read as a 404/400.
        assert_eq!(
            registry_error_to_protocol(RegistryError::io("disk full")).code,
            ErrorCode::Internal
        );
        assert_eq!(
            registry_error_to_protocol(RegistryError::not_found("nope")).code,
            ErrorCode::UnknownScope
        );
        assert_eq!(
            registry_error_to_protocol(RegistryError::invalid("bad")).code,
            ErrorCode::BadRequest
        );
    }
}
