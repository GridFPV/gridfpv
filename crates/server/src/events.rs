//! Events as **first-class containers** — the [`EventRegistry`] and [`EventMeta`].
//!
//! An **event is the container** for a whole fact log: everything that happens at one event
//! lives in *that* event's append-only log, distinct from every other event's. The registry
//! maps each [`EventId`] to its metadata and its own log handle ([`SharedLog`]).
//!
//! This is the skeleton: the registry creates, lists and deletes events, tracks which one is
//! active, and opens each event's log. Nothing reads or writes those logs over HTTP yet — the
//! competition model of `docs/primitives.md` is built on top of this.
//!
//! # Where a log lives
//!
//! Each persistent event is its **own SQLite file** under the data dir (one `log` table per
//! file, dense offsets from 0 — the [`SqliteLog`] schema). A registry with no configured data
//! dir falls back to an **in-memory** log ([`InMemoryLog`]) per created event, so an
//! unconfigured Director (and the tests) still work — non-durably. Any backend that honours
//! [`EventLog`] can sit behind the same handle.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use gridfpv_events::Event;
use gridfpv_storage::{
    EventLog, InMemoryLog, Offset, Result as StorageResult, SqliteLog, StoredEvent,
};

use crate::auth::TokenStore;
use crate::classes::ClassDirectory;
use crate::pilots::PilotDirectory;

/// Identifies an **event** — one whole competition, and the container for its log.
///
/// A transparent string newtype like the event-model ids. Always auto-generated on create
/// (a slug of the name plus a short random suffix), never user-entered.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/")]
pub struct EventId(pub String);

/// The object-safe slice of [`EventLog`] an event's log handle exposes: read the whole log,
/// read its tail, append, and report its length.
///
/// [`EventLog`] itself is **not** dyn-compatible (its `append_batch` is generic over the
/// iterator type), so it cannot be stored behind a trait object directly. This facade is
/// blanket-implemented for every `EventLog`, so any backend ([`InMemoryLog`], [`SqliteLog`])
/// drops in behind one `Arc<Mutex<dyn …>>`.
pub trait EventSource {
    /// Read every entry in append order.
    fn read_all(&self) -> StorageResult<Vec<StoredEvent>>;
    /// Read every entry from `start` (inclusive) to the end.
    fn read_from(&self, start: Offset) -> StorageResult<Vec<StoredEvent>>;
    /// Read the entries in `[start, end)`.
    fn read_range(&self, start: Offset, end: Offset) -> StorageResult<Vec<StoredEvent>>;
    /// Append a single event, returning its assigned offset.
    fn append(&mut self, event: Event, recorded_at: Option<i64>) -> StorageResult<Offset>;
    /// The number of entries — equivalently the offset the next append receives.
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
    fn read_range(&self, start: Offset, end: Offset) -> StorageResult<Vec<StoredEvent>> {
        EventLog::read_range(self, start, end)
    }
    fn append(&mut self, event: Event, recorded_at: Option<i64>) -> StorageResult<Offset> {
        EventLog::append(self, event, recorded_at)
    }
    fn len(&self) -> StorageResult<u64> {
        EventLog::len(self)
    }
}

/// A thread-safe handle to one event's append-only log.
///
/// `Send` (not `Send + Sync`) is required on the trait object because it lives behind a
/// [`Mutex`]; the `Arc<Mutex<…>>` makes the whole handle `Send + Sync`. The mutex serializes
/// every read and append against one another, so there is no torn state.
pub type SharedLog = Arc<Mutex<dyn EventSource + Send>>;

/// The file name (under the data dir) the Director's active-event id is persisted to, so
/// the selected event survives a Director restart.
pub const ACTIVE_EVENT_FILE: &str = "active-event";

/// The key, in an event's sidecar `meta` table, under which its [`EventMeta`] is persisted
/// as JSON. Stored on create so a Director restart restores it.
pub const EVENT_META_KEY: &str = "event_meta";

/// The file-name suffix (and the only files the boot-scan opens) of a persistent event's
/// SQLite log under the data dir: `<id>.sqlite`.
const EVENT_DB_SUFFIX: &str = ".sqlite";

/// The metadata describing one event in the registry.
///
/// The wire shape `GET /events` returns: a stable [`EventId`], a human display `name`, the
/// creation time, and whether the event is **persistent** (file-backed) or ephemeral (an
/// in-memory log, when the Director has no data dir configured). Derives serde (its JSON *is*
/// the wire form) and `ts_rs::TS` so the frontend reads a generated `EventMeta` type.
///
/// Unknown keys are ignored on read, so a record stored by an older build (one that carried
/// rounds, timers, a roster…) still loads; those fields are simply dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct EventMeta {
    /// The stable handle the event is addressed by (`/events/{id}`).
    pub id: EventId,
    /// The human-readable display name (names are display-only; the id is authoritative).
    pub name: String,
    /// Creation time in **milliseconds since the Unix epoch** (a plain JSON number — bounded
    /// far below 2^53, rendered as a TS `number` not a `bigint`, matching every other integer
    /// on the wire).
    #[ts(type = "number")]
    pub created_at: i64,
    /// Whether the event's log is durable (a SQLite file) or ephemeral (an in-memory log —
    /// `false`, which happens only when no data dir is configured).
    pub persistent: bool,
    /// Optional **display date** of the event, as a free-form string (e.g. `"2026-06-20"` or
    /// `"Sat 20 Jun"`). A string, not an epoch — it is a *human label the RD types*, shown
    /// verbatim on the picker and (later) the context header; the authoritative machine
    /// timestamp is [`created_at`](Self::created_at). Omitted from the wire when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub date: Option<String>,
    /// Optional venue / location label (e.g. `"Main field"`). Omitted from the wire when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub location: Option<String>,
    /// Optional free-text description / notes for the event. Omitted from the wire when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    /// Optional organizer name (the running club / person). Omitted from the wire when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub organizer: Option<String>,
}

/// The wire shape of `GET /active-event` — the **Director's currently-active event**, or
/// `null` when none is set.
///
/// The active event is **Director (server-side) state**: there is exactly one Race Director
/// running one event, so the selected event lives on the Director, not in each browser. Every
/// client reads this on connect/reload to resume into the workspace (or fall to the picker when
/// `null`). The `event` field is the full [`EventMeta`] so a client renders the context header
/// without a second round-trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ActiveEvent {
    /// The active event's metadata, or `null` when no event is active (→ the picker).
    pub event: Option<EventMeta>,
}

/// The body of `PUT /active-event` — the id of the event to make the Director's active one.
/// The id must name a known event, else a typed 404 (`UnknownScope`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SetActiveEventRequest {
    /// The event to make active.
    pub id: EventId,
}

/// The body of `POST /events` — the only thing a caller supplies when creating an event.
///
/// Just a display `name`; the **id is always auto-generated** (a slug of the name plus a
/// short random suffix), never user-entered, per the maintainer's rule. Keeping the id off
/// the wire means two events can share a name without colliding and a client cannot pick its
/// own id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CreateEventRequest {
    /// The display name for the new event.
    pub name: String,
    /// Optional **display date** stored on the new event's [`EventMeta::date`] (see there).
    /// Omitted from the wire when unset — a name-only create stays a one-field body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub date: Option<String>,
    /// Optional venue / location, stored on [`EventMeta::location`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub location: Option<String>,
    /// Optional free-text description, stored on [`EventMeta::description`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    /// Optional organizer name, stored on [`EventMeta::organizer`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub organizer: Option<String>,
}

impl CreateEventRequest {
    /// A **name-only** create request — the one-click path the console's "create your first
    /// event" affordance and the setup wizard both take, and the shape every test uses to build
    /// its event now that there is no built-in one.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            date: None,
            location: None,
            description: None,
            organizer: None,
        }
    }
}

/// One registered event: its metadata plus its own log.
struct RegisteredEvent {
    meta: EventMeta,
    log: SharedLog,
    live: Live,
}

/// An event's live state beside its log: the engine's fold of the log, kept caught up, and
/// the channel that tells listeners the log grew. See [`crate::control`].
#[derive(Clone)]
pub struct Live {
    /// The fold of the log so far. Always lock the log **before** this.
    pub state: Arc<Mutex<gridfpv_engine::state::EventState>>,
    /// The log cursor (how many entries), sent whenever something a console shows moves.
    pub changed: tokio::sync::watch::Sender<u64>,
    /// Whether a race is counting down or running right now. Kept here so a timer
    /// connection can ask many times a second without locking the event.
    pub race_on: Arc<std::sync::atomic::AtomicBool>,
    /// The race that is counting down or running right now, for a timer that has to be
    /// told when to listen.
    pub race_now: Arc<Mutex<Option<RaceNow>>>,
}

/// The race that is counting down or running right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RaceNow {
    pub race: gridfpv_events::RaceId,
    /// Its start on the Director's clock, in microseconds: the tone for a race still
    /// counting down, the logged start for one that is running.
    pub starts_at: i64,
    /// The timers its lineup uses.
    pub timers: Vec<gridfpv_events::AdapterId>,
    /// The minimum lap of the format in force, in milliseconds. 0 when it has none.
    pub min_lap_ms: u32,
}

impl Live {
    fn new() -> Self {
        Self {
            state: Arc::default(),
            changed: tokio::sync::watch::Sender::new(0),
            race_on: Arc::default(),
            race_now: Arc::default(),
        }
    }
}

/// The registry of all events on this Director.
///
/// Maps each [`EventId`] to its [`EventMeta`] and its own log. A fresh registry holds **no
/// events at all** — the RD creates the first one. Created events get a file-backed
/// [`SqliteLog`] under the configured data dir (one file per event). Cloning shares the one
/// registry (it is `Arc<RwLock<…>>`), so it can be the axum router state cloned into every
/// handler.
#[derive(Clone)]
pub struct EventRegistry {
    inner: Arc<RwLock<Registry>>,
}

/// The guarded interior: the event map, the shared token store, the app-level directories,
/// and where persistent event DBs live.
struct Registry {
    /// `EventId → RegisteredEvent`. A `BTreeMap` so listing is deterministic (id order).
    events: BTreeMap<EventId, RegisteredEvent>,
    /// The one Director-wide auth authority.
    tokens: TokenStore,
    /// The Director-wide application-level **pilot directory**. It lives here so the single
    /// router state ([`EventRegistry`]) exposes it to the Pilots API handlers without a second
    /// axum state type. Cloning shares the one directory.
    pilots: PilotDirectory,
    /// The Director-wide application-level **class directory**, held here for the same reason
    /// as the pilot directory.
    classes: ClassDirectory,
    /// The Director-wide timer directory, held here for the same reason.
    timers: crate::timers::TimerDirectory,
    /// Directory persistent event SQLite files are created under; `None` ⇒ created events
    /// fall back to an in-memory log (no data dir configured — non-durable).
    data_dir: Option<PathBuf>,
    /// The Director's **currently-active event** — the one all clients resume into on
    /// connect/reload. `None` ⇒ the picker. Persisted to `<data_dir>/active-event` (when a
    /// data dir is configured) so it survives a Director restart; in-memory only with no
    /// data dir.
    active_event: Option<EventId>,
}

impl EventRegistry {
    /// Build a registry over `data_dir`, restoring every event previously created there.
    ///
    /// A registry over a fresh data dir holds **no events**: there is no built-in event, so
    /// the RD's first act on a new Director is to create one. When `data_dir` is `Some`,
    /// [`create`](EventRegistry::create) writes a SQLite file per event there; when `None`,
    /// created events fall back to an in-memory log (so the registry is still usable with no
    /// configured storage — useful in tests and an unconfigured Director).
    pub fn new(data_dir: Option<PathBuf>) -> Result<Self, RegistryError> {
        let tokens = TokenStore::new();
        let mut events = BTreeMap::new();

        // The pilots persisted to `<data_dir>/pilots.json` (empty on first boot).
        let pilots = PilotDirectory::new(data_dir.clone())
            .map_err(|e| RegistryError::io(format!("could not build pilot directory: {e}")))?;

        // The classes persisted to `<data_dir>/classes.json`, plus the built-ins.
        let classes = ClassDirectory::new(data_dir.clone())
            .map_err(|e| RegistryError::io(format!("could not build class directory: {e}")))?;

        if let Some(dir) = &data_dir {
            std::fs::create_dir_all(dir).map_err(|e| {
                RegistryError::io(format!("could not create data dir {}: {e}", dir.display()))
            })?;
            // Reload every previously-created event: scan the data dir for the per-event
            // `<id>.sqlite` files and restore each event's `EventMeta` + its log.
            restore_persisted_events(dir, &mut events);
        }

        // Restore the persisted active event on boot: read `<data_dir>/active-event` if
        // present and it still names a known event. A missing file, an unreadable one, or a
        // stale id (the event no longer exists) all degrade to `None` — the picker — rather
        // than failing to boot.
        let active_event = data_dir
            .as_deref()
            .and_then(read_persisted_active_event)
            .filter(|id| events.contains_key(id));

        Ok(Self {
            inner: Arc::new(RwLock::new(Registry {
                events,
                tokens,
                pilots,
                classes,
                timers: crate::timers::TimerDirectory::new(data_dir.clone()),
                data_dir,
                active_event,
            })),
        })
    }

    /// The Director-wide application-level **pilot directory** — the authority the Pilots API
    /// mutates. Cloning shares the one directory.
    pub fn pilots(&self) -> PilotDirectory {
        self.read().pilots.clone()
    }

    /// The Director-wide application-level **class directory** — the authority the Classes
    /// API mutates. Cloning shares the one directory.
    pub fn classes(&self) -> ClassDirectory {
        self.read().classes.clone()
    }

    /// The Director's currently-active event's [`EventMeta`], or `None` when no
    /// event is active (the picker). The single read every client makes on connect/reload to
    /// resume into the selected event.
    pub fn active(&self) -> Option<EventMeta> {
        let reg = self.read();
        reg.active_event
            .as_ref()
            .and_then(|id| reg.events.get(id))
            .map(|e| e.meta.clone())
    }

    /// Set the Director's active event, returning its [`EventMeta`]. Validates the
    /// id names a known event, else [`RegistryError`] (the caller maps it to a typed 404). The
    /// new active id is **persisted** to `<data_dir>/active-event` (when a data dir is
    /// configured) so it survives a Director restart; with no data dir it is held in memory.
    pub fn set_active(&self, id: &EventId) -> Result<EventMeta, RegistryError> {
        let mut reg = self.write();
        let meta = reg
            .events
            .get(id)
            .map(|e| e.meta.clone())
            .ok_or_else(|| RegistryError::not_found(format!("no event with id {:?}", id.0)))?;
        reg.active_event = Some(id.clone());
        // Persist best-effort; a write failure is logged-shaped (returned) but the in-memory
        // state is already updated so the live session is correct regardless.
        if let Some(dir) = reg.data_dir.clone() {
            write_persisted_active_event(&dir, id)
                .map_err(|e| RegistryError::io(format!("could not persist active event: {e}")))?;
        }
        Ok(meta)
    }

    /// An event's [`EventMeta`], or `None` if no such event.
    pub fn meta_of(&self, id: &EventId) -> Option<EventMeta> {
        self.read().events.get(id).map(|e| e.meta.clone())
    }

    /// The shared [`TokenStore`] — the Director mints/pins its RD token through this, and the
    /// one credential gates every RD write.
    pub fn tokens(&self) -> TokenStore {
        self.read().tokens.clone()
    }

    /// An event's own log, or `None` if no such event.
    pub fn log(&self, id: &EventId) -> Option<SharedLog> {
        self.read().events.get(id).map(|e| e.log.clone())
    }

    /// Whether a race is counting down or running in the active event right now.
    pub fn race_on(&self) -> bool {
        let reg = self.read();
        reg.active_event
            .as_ref()
            .and_then(|id| reg.events.get(id))
            .is_some_and(|event| {
                event
                    .live
                    .race_on
                    .load(std::sync::atomic::Ordering::Relaxed)
            })
    }

    /// The race counting down or running in the active event right now, if any.
    pub fn race_now(&self) -> Option<RaceNow> {
        let reg = self.read();
        let live = &reg.events.get(reg.active_event.as_ref()?)?.live;
        live.race_now.lock().ok()?.clone()
    }

    /// The Director-wide timer directory.
    pub fn timers(&self) -> crate::timers::TimerDirectory {
        self.read().timers.clone()
    }

    /// An event's live state, or `None` if no such event.
    pub fn live(&self, id: &EventId) -> Option<Live> {
        self.read().events.get(id).map(|e| e.live.clone())
    }

    /// The metadata for every event, in id order.
    ///
    /// The order is stable (the map is a `BTreeMap`) so `GET /events` is deterministic. The list
    /// is **empty on a fresh Director** — that is the first-run state the picker must handle,
    /// not an error.
    pub fn list(&self) -> Vec<EventMeta> {
        self.read()
            .events
            .values()
            .map(|e| e.meta.clone())
            .collect()
    }

    /// Create a new event from a [`CreateEventRequest`], returning its [`EventMeta`].
    ///
    /// The **id is auto-generated** — a slug of the request's `name` plus a short random
    /// suffix — so it is unique and never user-entered (names are display-only). The optional
    /// descriptive fields (`date`/`location`/`description`/`organizer`) are stored verbatim on
    /// the new event's meta. A file-backed [`SqliteLog`] is opened under the configured data dir
    /// (`<data_dir>/<id>.sqlite`); with no data dir configured the event falls back to an
    /// in-memory log so creation still succeeds.
    pub fn create(&self, request: &CreateEventRequest) -> Result<EventMeta, RegistryError> {
        let name = request.name.as_str();
        let mut reg = self.write();

        // Auto-generate a unique id: slug + short random suffix, retried on the (astronomically
        // unlikely) collision so the id is always fresh.
        let id = loop {
            let candidate = EventId(format!("{}-{}", slugify(name), short_suffix()));
            if !reg.events.contains_key(&candidate) {
                break candidate;
            }
        };

        // Open the event's own log: a SQLite file per event under the data dir, else
        // in-memory (no configured storage).
        let (log, persistent): (SharedLog, bool) = match &reg.data_dir {
            Some(dir) => {
                let path = event_db_path(dir, &id);
                let log = SqliteLog::open(&path).map_err(|e| {
                    RegistryError::io(format!("could not open event log {}: {e}", path.display()))
                })?;
                (Arc::new(Mutex::new(log)), true)
            }
            None => (Arc::new(Mutex::new(InMemoryLog::new())), false),
        };

        // Optional descriptive fields are stored verbatim, normalized so a blank string is
        // treated as "unset" (so an empty "Add details" field never persists an empty label).
        let meta = EventMeta {
            id: id.clone(),
            name: name.to_string(),
            created_at: now_millis(),
            persistent,
            date: normalize_optional(&request.date),
            location: normalize_optional(&request.location),
            description: normalize_optional(&request.description),
            organizer: normalize_optional(&request.organizer),
        };
        // Persist the freshly-built meta into the event's own SQLite `meta` table so a
        // Director restart can restore it. Only for a persistent (file-backed) event — an
        // in-memory event has nothing to persist to.
        if persistent {
            if let Some(dir) = reg.data_dir.clone() {
                persist_event_meta(&dir, &meta)?;
            }
        }

        reg.events.insert(
            id,
            RegisteredEvent {
                meta: meta.clone(),
                log,
                live: Live::new(),
            },
        );
        Ok(meta)
    }

    ///
    /// Removes the registry entry, deletes the event's on-disk state (its `<id>.sqlite` log plus
    /// the WAL/SHM sidecars under the data dir), and — if it was the Director's active event —
    /// clears the active pointer (persisting the cleared pointer so the picker is shown after a
    /// restart). The deletion is complete: nothing of the event survives a restart (the boot scan
    /// finds no `<id>.sqlite` to restore).
    ///
    /// Every event is deletable — there is no reserved built-in event any more. An unknown
    /// id is a [`RegistryError`] the caller maps to a typed 404.
    ///
    /// The on-disk file removal is best-effort *after* the in-memory drop: dropping the
    /// [`RegisteredEvent`] closes the live SQLite connection (unless a caller still holds a
    /// handle from [`log`](EventRegistry::log)), so the files are then free to unlink. A missing file is not an error (idempotent cleanup);
    /// a genuine unlink failure is surfaced as a [`RegistryError`] so the caller can report it.
    pub fn delete(&self, id: &EventId) -> Result<(), RegistryError> {
        let mut reg = self.write();

        // Drop the in-memory entry first; this closes the event's own SQLite connection so
        // the on-disk files are unlocked for removal below.
        let removed = reg.events.remove(id);
        if removed.is_none() {
            return Err(RegistryError::not_found(format!(
                "no event with id {:?}",
                id.0
            )));
        }
        drop(removed);

        // If it was the active event, clear the pointer (and persist the cleared state) so a
        // reload/restart lands on the picker rather than dangling at a now-gone event.
        if reg.active_event.as_ref() == Some(id) {
            reg.active_event = None;
            if let Some(dir) = reg.data_dir.clone() {
                // Best-effort: removing the pointer file degrades a stale read to `None` anyway.
                let _ = std::fs::remove_file(active_event_path(&dir));
            }
        }

        // Permanently remove the event's persisted state: the `<id>.sqlite` log plus its WAL/SHM
        // sidecars. A missing file is fine (idempotent); a real unlink error is reported.
        if let Some(dir) = reg.data_dir.clone() {
            remove_event_files(&dir, id)?;
        }
        Ok(())
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Registry> {
        self.inner.read().expect("event registry lock poisoned")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Registry> {
        self.inner.write().expect("event registry lock poisoned")
    }
}

/// The SQLite file an event's log lives in under `dir`: `<dir>/<id>.sqlite`.
fn event_db_path(dir: &Path, id: &EventId) -> PathBuf {
    dir.join(format!("{}{}", id.0, EVENT_DB_SUFFIX))
}

/// Permanently remove an event's on-disk state under `dir`: its `<id>.sqlite` log and the
/// WAL/SHM sidecars SQLite leaves alongside it in WAL journal mode (`<id>.sqlite-wal`,
/// `<id>.sqlite-shm`). Each removal is best-effort against a *missing* file (already gone is
/// success — the cleanup is idempotent), but a genuine unlink failure (e.g. a permission error)
/// on the main log file is surfaced as a [`RegistryError`] so a partial delete is not silent.
fn remove_event_files(dir: &Path, id: &EventId) -> Result<(), RegistryError> {
    let main = event_db_path(dir, id);
    // The WAL/SHM sidecars share the main path with a suffix appended to the full file name.
    let wal = dir.join(format!("{}{}-wal", id.0, EVENT_DB_SUFFIX));
    let shm = dir.join(format!("{}{}-shm", id.0, EVENT_DB_SUFFIX));
    // Sidecars are pure cache/journal — a removal failure there is not fatal (SQLite recreates
    // them), so they are unlinked best-effort. The main log file is the durable state; a real
    // failure to remove it (not just "already absent") is reported.
    let _ = std::fs::remove_file(&wal);
    let _ = std::fs::remove_file(&shm);
    match std::fs::remove_file(&main) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(RegistryError::io(format!(
            "could not delete event log {}: {e}",
            main.display()
        ))),
    }
}

/// Persist an event's [`EventMeta`] into its own SQLite file's sidecar `meta` table, so a Director restart can restore it. Opens the event's `<dir>/<id>.sqlite` (WAL
/// allows this alongside the live log connection), serialises the meta to JSON, and
/// upserts it under [`EVENT_META_KEY`].
fn persist_event_meta(dir: &Path, meta: &EventMeta) -> Result<(), RegistryError> {
    let path = event_db_path(dir, &meta.id);
    let log = SqliteLog::open(&path).map_err(|e| {
        RegistryError::io(format!(
            "could not open event log {} to persist meta: {e}",
            path.display()
        ))
    })?;
    let json = serde_json::to_string(meta)
        .map_err(|e| RegistryError::io(format!("could not serialise event meta: {e}")))?;
    log.set_meta(EVENT_META_KEY, &json)
        .map_err(|e| RegistryError::io(format!("could not persist event meta: {e}")))?;
    Ok(())
}

/// Restore every persisted event in `dir` into `events` on boot.
///
/// Scans `<dir>` for `*.sqlite` files (each a created event's own log), and for each one opens
/// the log, reads its [`EventMeta`] back from the sidecar `meta` table, and rebuilds a
/// [`RegisteredEvent`] over that same on-disk log — so created events (and their metadata)
/// survive a Director restart. An entry that cannot be opened, has no persisted meta, or whose
/// meta cannot be parsed is **skipped** (logged-shaped, not fatal) so one bad file never blocks
/// boot.
fn restore_persisted_events(dir: &Path, events: &mut BTreeMap<EventId, RegisteredEvent>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // No data dir yet (first boot) — nothing to restore.
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Only event log files: a `<id>.sqlite`. Derive the id from the stem and skip
        // anything else (the `active-event` pointer, `pilots.json`, WAL/SHM sidecars).
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(EVENT_DB_SUFFIX) else {
            continue;
        };
        if stem.is_empty() {
            continue;
        }
        let id = EventId(stem.to_string());

        // Open the event's own log and read its persisted meta back.
        let log = match SqliteLog::open(&path) {
            Ok(log) => log,
            Err(e) => {
                // An unreadable file — skip, don't fail boot, but make it LOUD: a silent skip here
                // would vanish the event with no trace.
                eprintln!(
                    "WARNING: skipping event log {} on boot: could not open it: {e}",
                    path.display()
                );
                continue;
            }
        };
        let meta = match log.get_meta(EVENT_META_KEY) {
            Ok(Some(json)) => match serde_json::from_str::<EventMeta>(&json) {
                Ok(meta) => meta,
                Err(e) => {
                    // Unparseable meta — skip, but LOUDLY. A non-additive `EventMeta` change would
                    // make EVERY prior event fail to parse and silently disappear on upgrade; this
                    // log line is the only signal that happened.
                    eprintln!(
                        "ERROR: skipping event {:?} (log {}): its persisted meta could not be \
                         parsed — a non-additive EventMeta change can do this: {e}",
                        stem,
                        path.display()
                    );
                    continue;
                }
            },
            // No persisted meta (a half-written create) — skip rather
            // than fabricate a name; without meta the event isn't safely reconstructable.
            Ok(None) => continue,
            Err(e) => {
                eprintln!(
                    "WARNING: skipping event {:?} (log {}): could not read its persisted meta: {e}",
                    stem,
                    path.display()
                );
                continue;
            }
        };

        events.insert(
            id,
            RegisteredEvent {
                meta,
                log: Arc::new(Mutex::new(log)),
                live: Live::new(),
            },
        );
    }
}

/// The file the active-event id is persisted to under `dir`.
fn active_event_path(dir: &Path) -> PathBuf {
    dir.join(ACTIVE_EVENT_FILE)
}

/// Read the persisted active-event id from `<dir>/active-event`, or `None` if the file is
/// absent/unreadable/blank. The id is validated against the live event set by the caller, so a
/// stale id here is harmless. The file holds just the id (trimmed).
fn read_persisted_active_event(dir: &Path) -> Option<EventId> {
    let raw = std::fs::read_to_string(active_event_path(dir)).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(EventId(trimmed.to_string()))
    }
}

/// Persist the active-event id to `<dir>/active-event`, overwriting any prior value.
fn write_persisted_active_event(dir: &Path, id: &EventId) -> std::io::Result<()> {
    std::fs::write(active_event_path(dir), &id.0)
}

/// An error mutating the event registry (a missing event or a storage failure). Carries a
/// [`RegistryErrorKind`] so the HTTP layer can map an *unknown id* to `404` and an *I/O /
/// persistence* failure to `500`.
///
/// This matters because the in-memory state is mutated **before** the write-through: a persistence
/// failure must surface as a `500`, not a `404`/`400`, so the caller knows the change did not durably
/// land.
#[derive(Debug, Clone)]
pub struct RegistryError {
    /// What kind of failure this is (drives the HTTP status the handler picks).
    pub kind: RegistryErrorKind,
    /// A human-readable message.
    pub message: String,
}

/// The class of a [`RegistryError`], so a handler can pick the right status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryErrorKind {
    /// The addressed event/entity does not exist → 404.
    NotFound,
    /// A server-side storage / persistence failure → 500.
    Io,
}

impl RegistryError {
    /// An unknown-id error (HTTP 404).
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            kind: RegistryErrorKind::NotFound,
            message: message.into(),
        }
    }

    /// An I/O / persistence error (HTTP 500).
    pub fn io(message: impl Into<String>) -> Self {
        Self {
            kind: RegistryErrorKind::Io,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "event registry error: {}", self.message)
    }
}

impl std::error::Error for RegistryError {}

/// Current wall-clock time in milliseconds since the Unix epoch (creation timestamps).
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Trim an optional descriptive field, treating a blank/whitespace-only value as **unset**
/// (`None`) so an empty "Add details" input never persists an empty label.
fn normalize_optional(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Slugify a display name into the id-friendly stem: lowercase, ASCII alphanumerics kept,
/// every run of other characters collapsed to a single `-`, trimmed of leading/trailing
/// dashes. An empty/symbol-only name yields `event` so the id stem is never blank.
fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    let trimmed = slug.trim_matches('-');
    if trimmed.is_empty() {
        "event".to_string()
    } else {
        trimmed.to_string()
    }
}

/// A short random lowercase-alphanumeric suffix that makes an auto-generated id unique even
/// when two events share a name. Drawn from the OS CSPRNG (the same source the auth tokens
/// use) so it is unguessable and collision-resistant.
fn short_suffix() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut bytes = [0u8; 6];
    getrandom::fill(&mut bytes).expect("OS CSPRNG available");
    bytes
        .iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_events::AdapterId;

    /// A name-only create request (the common one-click path) for the tests.
    fn req(name: &str) -> CreateEventRequest {
        CreateEventRequest::named(name)
    }

    /// Any one log fact — the registry tests only care that it lands in the right log.
    fn fact() -> Event {
        Event::AdapterConnected {
            adapter: AdapterId("test".into()),
        }
    }

    /// How many facts an event's log holds.
    fn log_len(reg: &EventRegistry, id: &EventId) -> u64 {
        reg.log(id).unwrap().lock().unwrap().len().unwrap()
    }

    #[test]
    fn a_fresh_registry_has_no_events() {
        // There is no built-in event. A brand-new Director lists nothing, and nothing
        // resolves — that empty state is the console's "create your first event" screen,
        // not an error.
        let reg = EventRegistry::new(None).unwrap();
        assert!(reg.list().is_empty());
        assert!(reg.active().is_none());
        assert!(reg.log(&EventId("practice".into())).is_none());
    }

    #[test]
    fn creating_the_first_event_makes_it_resolvable_and_listed() {
        let reg = EventRegistry::new(None).unwrap();
        let meta = reg.create(&req("Practice")).unwrap();
        assert_eq!(meta.name, "Practice");
        assert!(reg.log(&meta.id).is_some());
        assert_eq!(reg.meta_of(&meta.id), Some(meta.clone()));
        assert_eq!(reg.list().len(), 1);
        assert_eq!(reg.list()[0].id, meta.id);
    }

    #[test]
    fn unknown_event_does_not_resolve() {
        let reg = EventRegistry::new(None).unwrap();
        assert!(reg.log(&EventId("nope".into())).is_none());
        assert!(reg.meta_of(&EventId("nope".into())).is_none());
    }

    #[test]
    fn create_auto_generates_a_unique_slug_id() {
        let reg = EventRegistry::new(None).unwrap();
        let a = reg.create(&req("Spring Cup 2026!")).unwrap();
        let b = reg.create(&req("Spring Cup 2026!")).unwrap();
        // Same name, distinct ids (the random suffix disambiguates); slug is name-derived.
        assert!(a.id.0.starts_with("spring-cup-2026-"));
        assert!(b.id.0.starts_with("spring-cup-2026-"));
        assert_ne!(a.id, b.id);
        // Both resolve, and both are listed.
        assert!(reg.log(&a.id).is_some());
        let ids: Vec<_> = reg.list().into_iter().map(|m| m.id).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&a.id) && ids.contains(&b.id));
    }

    #[test]
    fn each_created_event_gets_its_own_log() {
        let reg = EventRegistry::new(None).unwrap();
        let other = reg.create(&req("Practice")).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();

        // Append a fact into the created event only.
        reg.log(&created.id)
            .unwrap()
            .lock()
            .unwrap()
            .append(fact(), None)
            .unwrap();

        // That event's log has the fact; the other event's log is untouched (per-event dense).
        assert_eq!(log_len(&reg, &created.id), 1);
        assert_eq!(log_len(&reg, &other.id), 0);
    }

    #[test]
    fn slugify_collapses_and_trims() {
        assert_eq!(slugify("Spring Cup 2026!"), "spring-cup-2026");
        assert_eq!(slugify("  weird___name  "), "weird-name");
        assert_eq!(slugify("!!!"), "event");
        assert_eq!(slugify(""), "event");
    }

    #[test]
    fn active_event_defaults_to_none_then_set_and_read_back() {
        let reg = EventRegistry::new(None).unwrap();
        assert!(reg.active().is_none());

        // Setting to a known event returns its meta and reads back.
        let created = reg.create(&req("Race Night")).unwrap().id;
        let meta = reg.set_active(&created).unwrap();
        assert_eq!(meta.id, created);
        assert_eq!(reg.active().map(|m| m.id), Some(created));
    }

    #[test]
    fn a_stale_active_event_pointer_degrades_to_the_picker() {
        // A data dir whose `active-event` file names an id that no longer exists. Booting over
        // it must land on the picker, not fail to boot and not dangle at a missing event.
        let dir = std::env::temp_dir().join(format!("gridfpv-stale-active-{}", short_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(ACTIVE_EVENT_FILE), "practice").unwrap();

        let reg =
            EventRegistry::new(Some(dir.clone())).expect("a stale pointer must not fail boot");
        assert!(
            reg.active().is_none(),
            "the stale id degrades to the picker"
        );
        assert!(reg.log(&EventId("practice".into())).is_none());
        assert!(reg.list().is_empty());

        // And the RD can still create an event and make it active over the same data dir.
        let created = reg.create(&req("Race Night")).unwrap();
        reg.set_active(&created.id).unwrap();
        let reopened = EventRegistry::new(Some(dir.clone())).unwrap();
        assert_eq!(reopened.active().map(|m| m.id), Some(created.id));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn set_active_rejects_an_unknown_event() {
        let reg = EventRegistry::new(None).unwrap();
        assert!(reg.set_active(&EventId("nope".into())).is_err());
        assert!(reg.active().is_none());
    }

    #[test]
    fn active_event_persists_across_a_restart_with_a_data_dir() {
        let dir = std::env::temp_dir().join(format!("gridfpv-active-test-{}", short_suffix()));
        {
            let reg = EventRegistry::new(Some(dir.clone())).unwrap();
            let created = reg.create(&req("Persisted")).unwrap();
            reg.set_active(&created.id).unwrap();
            // A fresh registry over the SAME data dir restores the active event — the created
            // event's SQLite file (and its persisted meta) is reloaded on boot, so a
            // created-id active pointer resolves rather than degrading to the picker.
            let reopened = EventRegistry::new(Some(dir.clone())).unwrap();
            assert_eq!(reopened.active().map(|m| m.id), Some(created.id.clone()));

            // A second created event takes over the pointer, and that survives too.
            let second = reg.create(&req("Second")).unwrap();
            reg.set_active(&second.id).unwrap();
            let reopened2 = EventRegistry::new(Some(dir.clone())).unwrap();
            assert_eq!(reopened2.active().map(|m| m.id), Some(second.id));
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn created_events_and_their_metadata_survive_a_restart() {
        // A created event (with a name and descriptive fields) must be re-listed with its
        // metadata intact, and its log, after the Director restarts over the same data dir.
        let dir = std::env::temp_dir().join(format!("gridfpv-reload-test-{}", short_suffix()));
        let created_id;
        {
            let reg = EventRegistry::new(Some(dir.clone())).unwrap();
            let created = reg
                .create(&CreateEventRequest {
                    name: "Spring Cup".to_string(),
                    date: Some("2026-06-20".to_string()),
                    location: Some("Main field".to_string()),
                    description: None,
                    organizer: Some("GridFPV Club".to_string()),
                })
                .unwrap();
            created_id = created.id.clone();
            reg.set_active(&created.id).unwrap();
            reg.log(&created.id)
                .unwrap()
                .lock()
                .unwrap()
                .append(fact(), None)
                .unwrap();
        }

        // Simulate a Director restart: a brand-new registry over the SAME data dir.
        let reopened = EventRegistry::new(Some(dir.clone())).unwrap();

        // The event is listed again with its metadata.
        let restored = reopened
            .meta_of(&created_id)
            .expect("the created event should be reloaded on restart");
        assert_eq!(restored.name, "Spring Cup");
        assert!(restored.persistent);
        assert_eq!(restored.date.as_deref(), Some("2026-06-20"));
        assert_eq!(restored.location.as_deref(), Some("Main field"));
        assert_eq!(restored.organizer.as_deref(), Some("GridFPV Club"));

        // It is in the public list.
        let ids: Vec<_> = reopened.list().into_iter().map(|m| m.id).collect();
        assert!(ids.contains(&created_id));

        // Its log facts survived too.
        assert_eq!(log_len(&reopened, &created_id), 1);

        // And the active-event pointer restores to it.
        assert_eq!(reopened.active().map(|m| m.id), Some(created_id));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_event_stored_in_an_older_shape_still_loads() {
        // `CLAUDE.md`: a stored record in an old shape must still LOAD. An event persisted by
        // a build that kept rounds, timers, a roster and channel layouts on the meta opens
        // here with those keys ignored — the event is listed, not skipped.
        let dir = std::env::temp_dir().join(format!("gridfpv-old-shape-{}", short_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        let id = EventId("old-cup-abc123".into());
        let log = SqliteLog::open(event_db_path(&dir, &id)).unwrap();
        log.set_meta(
            EVENT_META_KEY,
            r#"{"id":"old-cup-abc123","name":"Old Cup","created_at":1750000000000,
                "persistent":true,"location":"Main field",
                "timers":["mock","rh-1"],"primary_timer":"rh-1","roster":["ace"],
                "classes":["open"],"classes_membership":[{"class":"open","members":[]}],
                "rounds":[{"id":"r1","format":"time_trial"}],
                "channel_layouts":[{"id":"l1","name":"A","nodes":[]}]}"#,
        )
        .unwrap();
        drop(log);

        let reg = EventRegistry::new(Some(dir.clone())).unwrap();
        let meta = reg.meta_of(&id).expect("the old-shape event still loads");
        assert_eq!(meta.name, "Old Cup");
        assert_eq!(meta.location.as_deref(), Some("Main field"));
        assert!(reg.log(&id).is_some());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn delete_removes_the_event_and_all_its_data_and_survives_restart() {
        // Deleting an event must remove the registry entry, its persisted SQLite log
        // (+ wal/shm), clear it as the active event, and stay gone after a restart.
        let dir = std::env::temp_dir().join(format!("gridfpv-delete-test-{}", short_suffix()));
        let created_id;
        {
            let reg = EventRegistry::new(Some(dir.clone())).unwrap();
            let created = reg.create(&req("Doomed Event")).unwrap();
            created_id = created.id.clone();
            // Give it a log fact and make it the active event so deletion must clear that too.
            reg.set_active(&created.id).unwrap();
            reg.log(&created.id)
                .unwrap()
                .lock()
                .unwrap()
                .append(fact(), None)
                .unwrap();

            // The SQLite file exists on disk before the delete.
            let db = event_db_path(&dir, &created_id);
            assert!(
                db.exists(),
                "the event's SQLite file should exist pre-delete"
            );

            // Delete it.
            reg.delete(&created_id).unwrap();

            // Gone from the registry, from the list, and as the active event.
            assert!(reg.log(&created_id).is_none());
            assert!(!reg.list().iter().any(|m| m.id == created_id));
            assert!(
                reg.active().is_none(),
                "deleting the active event clears it"
            );

            // And the on-disk state is gone (no orphan log / wal / shm files).
            assert!(!db.exists(), "the event's SQLite file is removed");
            assert!(!dir.join(format!("{}.sqlite-wal", created_id.0)).exists());
            assert!(!dir.join(format!("{}.sqlite-shm", created_id.0)).exists());
        }

        // Survives a restart: a fresh registry over the same data dir does not re-list it.
        let reopened = EventRegistry::new(Some(dir.clone())).unwrap();
        assert!(
            reopened.log(&created_id).is_none(),
            "a deleted event must not reappear after a restart"
        );
        assert!(reopened.active().is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn delete_rejects_an_unknown_event_and_no_event_is_reserved() {
        let reg = EventRegistry::new(None).unwrap();
        // An unknown id is an error and removes nothing.
        assert!(reg.delete(&EventId("no-such-event".into())).is_err());
        // Nothing is reserved: an event an RD names "Practice" deletes like any other.
        let mine = reg.create(&req("Practice")).unwrap();
        assert!(reg.delete(&mine.id).is_ok());
        assert!(reg.log(&mine.id).is_none());
        assert!(reg.list().is_empty());
    }
}
