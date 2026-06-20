//! Events as **first-class containers** — the `EventRegistry` and `EventMeta` (issue #72).
//!
//! An **event is the container** for a whole fact log: its heats, registrations, passes,
//! and marshaling adjudications all live in *one* event's append-only log, distinct from
//! every other event's. The flat single-log model (one Director = one event) is replaced
//! by a registry mapping each [`EventId`] to that event's own [`AppState`](crate::app::AppState)
//! — and therefore its own [`EventLog`]. Every read/realtime/control surface is rooted under
//! the event (`/events/{eventId}/…`); the registry resolves the id to the log that surface
//! serves (see [`crate::app::events_router`]).
//!
//! # Two physical realizations of one logical model
//!
//! The model is **backend-agnostic** — the registry stores an `AppState` over *any*
//! [`EventLog`] backend, so the same logical "an event owns a dense, per-event log" holds
//! across both realizations:
//!
//! - **Local (now):** each persistent event is its **own SQLite file** (one `log` table per
//!   file, dense offsets from 0 — exactly the existing [`SqliteLog`](gridfpv_storage::SqliteLog)
//!   schema). The built-in **Practice** event is an **in-memory** log
//!   ([`InMemoryLog`](gridfpv_storage::InMemoryLog)), non-persistent by design.
//! - **Cloud (v0.7 — NOT built here, kept compatible):** one Postgres DB with an `events`
//!   table and a shared `event_log` table keyed by `event_id` with a **composite primary key
//!   `(event_id, offset)`**, so each event's offset sequence stays **per-event dense** —
//!   identical to "one SQLite file per event" today. Reads are always event-scoped (the
//!   registry never serves across events), so the Postgres backend slots behind the same
//!   `EventLog` trait: an event-scoped log handle is a `WHERE event_id = ?` view whose
//!   `offset` column is dense within that event. No surface here reads the whole DB; every
//!   path resolves an event first, so the per-event-dense-offset invariant is the only thing
//!   the cloud mapping must preserve — and the composite PK gives it for free.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use gridfpv_storage::{InMemoryLog, SqliteLog};

use crate::app::AppState;
use crate::auth::TokenStore;
use crate::scope::EventId;

/// The reserved id of the always-present built-in **Practice** event.
///
/// Practice is seeded into every registry, backed by an in-memory (non-persistent) log:
/// the RD can run a sim race with nothing configured. Its id is reserved — [`EventRegistry::create`]
/// auto-generates ids and never collides with it.
pub const PRACTICE_EVENT_ID: &str = "practice";

/// The display name of the built-in Practice event.
pub const PRACTICE_EVENT_NAME: &str = "Practice";

/// The metadata describing one event in the registry (issue #72).
///
/// The wire shape `GET /events` returns: a stable [`EventId`], a human display `name`, the
/// creation time, and whether the event is **persistent** (file-backed) or ephemeral (the
/// in-memory Practice event). Derives serde (its JSON *is* the wire form) and `ts_rs::TS`
/// so the frontend reads a generated `EventMeta` type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct EventMeta {
    /// The stable handle every per-event route is rooted under (`/events/{id}/…`).
    pub id: EventId,
    /// The human-readable display name (names are display-only; the id is authoritative).
    pub name: String,
    /// Creation time in **milliseconds since the Unix epoch** (a plain JSON number — bounded
    /// far below 2^53, rendered as a TS `number` not a `bigint`, matching every other integer
    /// on the wire). Practice is seeded at registry construction.
    #[ts(type = "number")]
    pub created_at: i64,
    /// Whether the event's log is durable (a SQLite file) or ephemeral (the in-memory
    /// Practice log, `false`).
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

/// The body of `POST /events` — the only thing a caller supplies when creating an event.
///
/// Just a display `name`; the **id is always auto-generated** (a slug of the name plus a
/// short random suffix), never user-entered, per the maintainer's rule. Keeping the id off
/// the wire means two events can share a name without colliding and a client can't squat a
/// reserved id (e.g. `practice`).
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

/// One registered event: its metadata plus the [`AppState`] (its own log + append-notify,
/// the shared token store) every per-event surface serves against.
struct RegisteredEvent {
    meta: EventMeta,
    state: AppState,
}

/// The registry of all events on this Director (issue #72) — the backend-agnostic
/// `EventRegistry` the routing layer resolves an [`EventId`] through.
///
/// Maps each [`EventId`] to its [`AppState`] (and so its own [`EventLog`]). A built-in
/// **Practice** event ([`PRACTICE_EVENT_ID`], in-memory, non-persistent) is always present.
/// Created events get a file-backed [`SqliteLog`](gridfpv_storage::SqliteLog) under the
/// configured data dir (one file per event — the local realization of the per-event-dense
/// log; see the module docs for the Postgres mapping). Cloning shares the one registry (it
/// is `Arc<RwLock<…>>`), so it can be the axum router state cloned into every handler.
#[derive(Clone)]
pub struct EventRegistry {
    inner: Arc<RwLock<Registry>>,
}

/// The guarded interior: the event map, the shared token store, and where persistent event
/// DBs live.
struct Registry {
    /// `EventId → RegisteredEvent`. A `BTreeMap` so listing is deterministic (Practice is
    /// listed first explicitly regardless).
    events: BTreeMap<EventId, RegisteredEvent>,
    /// The one Director-wide auth authority, shared into every per-event [`AppState`].
    tokens: TokenStore,
    /// Directory persistent event SQLite files are created under; `None` ⇒ created events
    /// fall back to an in-memory log (no data dir configured — non-durable).
    data_dir: Option<PathBuf>,
}

impl EventRegistry {
    /// Build a registry seeded with the built-in Practice event, persisting created events
    /// under `data_dir` when given.
    ///
    /// The Practice event is an in-memory, non-persistent log. When `data_dir` is `Some`,
    /// [`create`](EventRegistry::create) writes a SQLite file per event there; when `None`,
    /// created events fall back to an in-memory log (so the registry is still usable with no
    /// configured storage — useful in tests and an unconfigured Director).
    pub fn new(data_dir: Option<PathBuf>) -> Result<Self, RegistryError> {
        let tokens = TokenStore::new();
        let mut events = BTreeMap::new();

        // Seed Practice: an in-memory (non-persistent) log, sharing the one token store.
        let practice_id = EventId(PRACTICE_EVENT_ID.to_string());
        let practice_state = AppState::with_tokens(InMemoryLog::new(), tokens.clone());
        events.insert(
            practice_id.clone(),
            RegisteredEvent {
                meta: EventMeta {
                    id: practice_id,
                    name: PRACTICE_EVENT_NAME.to_string(),
                    created_at: now_millis(),
                    persistent: false,
                    date: None,
                    location: None,
                    description: None,
                    organizer: None,
                },
                state: practice_state,
            },
        );

        if let Some(dir) = &data_dir {
            std::fs::create_dir_all(dir).map_err(|e| {
                RegistryError(format!("could not create data dir {}: {e}", dir.display()))
            })?;
        }

        Ok(Self {
            inner: Arc::new(RwLock::new(Registry {
                events,
                tokens,
                data_dir,
            })),
        })
    }

    /// The shared [`TokenStore`] — the Director mints/pins its RD token through this so the
    /// one credential authenticates control on *every* event.
    pub fn tokens(&self) -> TokenStore {
        self.read().tokens.clone()
    }

    /// Resolve an [`EventId`] to that event's [`AppState`], or `None` if no such event.
    ///
    /// This is the registry's core operation: every per-event route resolves the id here and
    /// serves against the returned state's own log. An unknown id is the caller's cue to
    /// return a typed 404 (mirroring the `UnknownScope` pattern).
    pub fn resolve(&self, id: &EventId) -> Option<AppState> {
        self.read().events.get(id).map(|e| e.state.clone())
    }

    /// The metadata for every event, **Practice first**, then the rest in id order.
    ///
    /// The order is stable so `GET /events` is deterministic and the console can default to
    /// the first (Practice).
    pub fn list(&self) -> Vec<EventMeta> {
        let reg = self.read();
        let mut out = Vec::with_capacity(reg.events.len());
        let practice = EventId(PRACTICE_EVENT_ID.to_string());
        if let Some(p) = reg.events.get(&practice) {
            out.push(p.meta.clone());
        }
        for (id, ev) in &reg.events {
            if *id != practice {
                out.push(ev.meta.clone());
            }
        }
        out
    }

    /// Create a new persistent event from a [`CreateEventRequest`], returning its [`EventMeta`].
    ///
    /// The **id is auto-generated** — a slug of the request's `name` plus a short random
    /// suffix — so it is unique and never user-entered (names are display-only). The optional
    /// descriptive fields (`date`/`location`/`description`/`organizer`) are stored verbatim on
    /// the new event's meta. A file-backed [`SqliteLog`](gridfpv_storage::SqliteLog) is opened
    /// under the configured data dir (`<data_dir>/<id>.sqlite`); with no data dir configured the
    /// event falls back to an in-memory log so creation still succeeds. The new event shares the
    /// registry's token store, so the RD's token controls it immediately.
    pub fn create(&self, request: &CreateEventRequest) -> Result<EventMeta, RegistryError> {
        let name = request.name.as_str();
        let mut reg = self.write();

        // Auto-generate a unique id: slug + short random suffix, retried on the (astronomically
        // unlikely) collision so the id is always fresh and never the reserved `practice`.
        let id = loop {
            let candidate = EventId(format!("{}-{}", slugify(name), short_suffix()));
            if candidate.0 != PRACTICE_EVENT_ID && !reg.events.contains_key(&candidate) {
                break candidate;
            }
        };

        // Open the event's own log: a SQLite file per event under the data dir, else
        // in-memory (no configured storage).
        let (state, persistent) = match &reg.data_dir {
            Some(dir) => {
                let path = event_db_path(dir, &id);
                let log = SqliteLog::open(&path).map_err(|e| {
                    RegistryError(format!("could not open event log {}: {e}", path.display()))
                })?;
                (AppState::with_tokens(log, reg.tokens.clone()), true)
            }
            None => (
                AppState::with_tokens(InMemoryLog::new(), reg.tokens.clone()),
                false,
            ),
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
        reg.events.insert(
            id,
            RegisteredEvent {
                meta: meta.clone(),
                state,
            },
        );
        Ok(meta)
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
    dir.join(format!("{}.sqlite", id.0))
}

/// An error creating an event or its registry (a storage failure, a bad data dir).
#[derive(Debug, Clone)]
pub struct RegistryError(pub String);

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "event registry error: {}", self.0)
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
    use gridfpv_events::{CompetitorRef, Event, HeatId};

    /// A name-only create request (the common one-click path) for the tests.
    fn req(name: &str) -> CreateEventRequest {
        CreateEventRequest {
            name: name.to_string(),
            date: None,
            location: None,
            description: None,
            organizer: None,
        }
    }

    #[test]
    fn practice_is_always_present_and_first() {
        let reg = EventRegistry::new(None).unwrap();
        let list = reg.list();
        assert_eq!(list.first().unwrap().id.0, PRACTICE_EVENT_ID);
        assert_eq!(list.first().unwrap().name, PRACTICE_EVENT_NAME);
        assert!(!list.first().unwrap().persistent);
        // Practice resolves to a usable AppState.
        assert!(reg.resolve(&EventId(PRACTICE_EVENT_ID.into())).is_some());
    }

    #[test]
    fn unknown_event_does_not_resolve() {
        let reg = EventRegistry::new(None).unwrap();
        assert!(reg.resolve(&EventId("nope".into())).is_none());
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
        // Both resolve, and they are listed after Practice.
        assert!(reg.resolve(&a.id).is_some());
        let ids: Vec<_> = reg.list().into_iter().map(|m| m.id).collect();
        assert_eq!(ids[0].0, PRACTICE_EVENT_ID);
        assert!(ids.contains(&a.id) && ids.contains(&b.id));
    }

    #[test]
    fn created_event_log_is_independent_of_practice() {
        let reg = EventRegistry::new(None).unwrap();
        let practice = reg.resolve(&EventId(PRACTICE_EVENT_ID.into())).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();
        let created_state = reg.resolve(&created.id).unwrap();

        // Append a heat into the created event only.
        created_state
            .append(
                Event::HeatScheduled {
                    heat: HeatId("q-1".into()),
                    lineup: vec![CompetitorRef("A".into())],
                },
                None,
            )
            .unwrap();

        // The created event's log has the heat; Practice's log is untouched (per-event dense).
        let (created_events, _) = created_state.read().unwrap();
        assert_eq!(created_events.len(), 1);
        let (practice_events, _) = practice.read().unwrap();
        assert_eq!(practice_events.len(), 0);
    }

    #[test]
    fn one_rd_token_controls_every_event() {
        let reg = EventRegistry::new(None).unwrap();
        let rd = reg.tokens().issue_rd_token();
        let created = reg.create(&req("Race Night")).unwrap();
        // The shared token store is the same instance behind every event's AppState.
        let practice = reg.resolve(&EventId(PRACTICE_EVENT_ID.into())).unwrap();
        let created_state = reg.resolve(&created.id).unwrap();
        assert!(practice.tokens().authenticate_control(Some(&rd)).is_ok());
        assert!(
            created_state
                .tokens()
                .authenticate_control(Some(&rd))
                .is_ok()
        );
    }

    #[test]
    fn slugify_collapses_and_trims() {
        assert_eq!(slugify("Spring Cup 2026!"), "spring-cup-2026");
        assert_eq!(slugify("  weird___name  "), "weird-name");
        assert_eq!(slugify("!!!"), "event");
        assert_eq!(slugify(""), "event");
    }

    #[test]
    fn create_persists_a_file_per_event_when_a_data_dir_is_set() {
        let dir = std::env::temp_dir().join(format!("gridfpv-reg-test-{}", short_suffix()));
        let reg = EventRegistry::new(Some(dir.clone())).unwrap();
        let created = reg.create(&req("Persisted")).unwrap();
        assert!(created.persistent);
        let path = event_db_path(&dir, &created.id);
        assert!(path.exists(), "an event DB file should be created");
        std::fs::remove_dir_all(&dir).ok();
    }
}
