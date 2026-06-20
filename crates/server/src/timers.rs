//! Timers as **application-level configuration** — the `TimerRegistry` and `Timer` (issue #73).
//!
//! A timer is the thing that *produces lap-gate passes* — the built-in synthetic
//! **Simulator**, or (reserved for #65/2b) a real **RotorHazard** server. The model parallels
//! the event model ([`EventRegistry`](crate::events::EventRegistry)): a Race Director configures
//! their timers **once** at the application level (a persisted registry) and each event simply
//! **selects** which of them to use (see [`EventMeta::timers`](crate::events::EventMeta::timers)).
//! Set up the RotorHazard once, and every new event just picks it.
//!
//! # Two pieces, mirroring events
//!
//! - **App-level registry (this module).** The [`TimerRegistry`] holds every configured
//!   [`Timer`] behind a lock and **persists** them to `<GRIDFPV_DATA_DIR>/timers.json`
//!   (restored on boot; in-memory only when no data dir is configured). A built-in
//!   **Simulator** ([`SIM_TIMER_ID`]) is always present — so an unconfigured Director can run a
//!   sim race out of the box — and cannot be deleted.
//! - **Per-event selection (`crate::events`).** Each [`EventMeta`](crate::events::EventMeta)
//!   carries a `timers: Vec<TimerId>` of the timers that event uses; new events (and Practice)
//!   default to `["sim"]`.
//!
//! # The kinds
//!
//! [`TimerKind::Sim`] is the synthetic source wired end-to-end here (its `laps`/`lap_ms` drive
//! the per-event sim bridge). [`TimerKind::Rotorhazard`] is **config-only / reserved** — it
//! holds the RH server `url` so the surface and persistence are forward-compatible, but nothing
//! connects to it in this slice; that is 2b (#65). A selected RotorHazard timer is a no-op stub.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The reserved id of the always-present built-in **Simulator** timer.
///
/// The Simulator is seeded into every registry, draws its `laps`/`lap_ms` from the Director's
/// env defaults (`GRIDFPV_SIM_LAPS` / `GRIDFPV_SIM_LAP_MS`), and cannot be deleted — so a
/// Director with nothing configured can still run a sim race. New events default to selecting it.
pub const SIM_TIMER_ID: &str = "sim";

/// The display name of the built-in Simulator timer.
pub const SIM_TIMER_NAME: &str = "Simulator";

/// The file name (under the data dir) the timer registry is persisted to (issue #73).
pub const TIMERS_FILE: &str = "timers.json";

/// Identifies a **timer** in the application-level registry (issue #73).
///
/// A transparent string newtype like [`EventId`](crate::scope::EventId): the built-in Simulator
/// has the reserved id [`SIM_TIMER_ID`]; created timers get an auto-generated slug + suffix id,
/// never user-entered (names are display-only).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/")]
pub struct TimerId(pub String);

/// The kind of a timer — *how* it produces passes (issue #73).
///
/// Externally tagged so it maps to a TS discriminated union. [`Sim`](TimerKind::Sim) is the
/// synthetic source wired end-to-end in this slice; [`Rotorhazard`](TimerKind::Rotorhazard) is
/// **reserved / config-only** — its `url` is stored and round-trips on the wire and on disk, but
/// nothing connects to it here (that is 2b / #65). A selected RotorHazard timer is a no-op stub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum TimerKind {
    /// The built-in synthetic source: emit a holeshot + `laps` laps per pilot at `lap_ms`
    /// real-time pace when a heat goes Running (mirrors the existing sim knobs).
    Sim {
        /// Laps each sim pilot flies beyond the holeshot.
        laps: u32,
        /// The nominal real-time pace of one sim lap, in milliseconds.
        #[ts(type = "number")]
        lap_ms: u64,
    },
    /// A **RotorHazard** server — config-only / reserved for 2b (#65). Holds the RH base URL the
    /// connector will dial; not connected in this slice.
    Rotorhazard {
        /// The RotorHazard server base URL (e.g. `http://rotorhazard.local:5000`).
        url: String,
    },
}

/// Whether a timer is currently usable (issue #73).
///
/// The Simulator is always [`Ready`](TimerStatus::Ready) (it needs nothing external). A reserved
/// RotorHazard timer reports [`Configured`](TimerStatus::Configured) — it has a URL on file but is
/// not yet connected (2b wires the live connection and the `Connected`/`Unreachable` states).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum TimerStatus {
    /// Usable right now — the built-in Simulator.
    Ready,
    /// Configured but not connected (a reserved RotorHazard timer; 2b connects it).
    Configured,
}

/// One configured timer in the application-level registry (issue #73).
///
/// The wire shape `GET /timers` returns and the on-disk shape `timers.json` persists: a stable
/// [`TimerId`], a human display `name`, the [`TimerKind`] (its config), and a derived
/// [`TimerStatus`]. Derives serde (its JSON *is* both the wire and the persisted form) and
/// `ts_rs::TS` so the frontend reads a generated `Timer` type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct Timer {
    /// The stable handle an event selects by and the API addresses (`PUT /timers/{id}`).
    pub id: TimerId,
    /// The human-readable display name (display-only; the id is authoritative).
    pub name: String,
    /// The kind + config: a [`TimerKind::Sim`] or a reserved [`TimerKind::Rotorhazard`].
    pub kind: TimerKind,
    /// The derived usability of the timer (see [`TimerStatus`]).
    pub status: TimerStatus,
}

impl Timer {
    /// Derive the [`TimerStatus`] from a [`TimerKind`]: the Simulator is [`Ready`](TimerStatus::Ready);
    /// a reserved RotorHazard timer is [`Configured`](TimerStatus::Configured) (not yet connected).
    fn status_for(kind: &TimerKind) -> TimerStatus {
        match kind {
            TimerKind::Sim { .. } => TimerStatus::Ready,
            TimerKind::Rotorhazard { .. } => TimerStatus::Configured,
        }
    }
}

/// The body of `POST /timers` — the config a caller supplies to create a timer (issue #73).
///
/// A display `name` plus the [`TimerKind`]; the **id is auto-generated** server-side (a slug of
/// the name + a short random suffix), never user-entered, mirroring `POST /events`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CreateTimerRequest {
    /// The display name for the new timer.
    pub name: String,
    /// The kind + config of the new timer.
    pub kind: TimerKind,
}

/// The body of `PUT /timers/{id}` — the editable fields of a timer (issue #73).
///
/// Edits the display `name` and/or the [`TimerKind`] config (e.g. retune the sim's `lap_ms`, or
/// point a RotorHazard timer at a new URL). Both optional so a partial edit is a one-field body;
/// the id is fixed (it is in the path) and the built-in Simulator may be retuned but not removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct UpdateTimerRequest {
    /// A new display name, or `None` to leave it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<String>,
    /// A new kind + config, or `None` to leave it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub kind: Option<TimerKind>,
}

/// The body of `PUT /events/{id}/timers` — the timer ids an event selects (issue #73).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SetEventTimersRequest {
    /// The timers this event uses, in selection order. Each must name a known timer.
    pub ids: Vec<TimerId>,
}

/// The application-level registry of all configured timers (issue #73).
///
/// Maps each [`TimerId`] to its [`Timer`]. A built-in **Simulator** ([`SIM_TIMER_ID`]) is always
/// present. The set is **persisted** to `<data_dir>/timers.json` (restored on boot) so the RD's
/// timers survive a Director restart; with no data dir configured it is in-memory only. Cloning
/// shares the one registry (`Arc<RwLock<…>>`), so it is the axum router state cloned into every
/// handler, exactly like the [`EventRegistry`](crate::events::EventRegistry).
#[derive(Clone)]
pub struct TimerRegistry {
    inner: Arc<RwLock<Registry>>,
}

/// The guarded interior: the timer map and where `timers.json` lives.
struct Registry {
    /// `TimerId → Timer`. A `BTreeMap` so listing is deterministic (the Simulator is listed
    /// first explicitly regardless).
    timers: BTreeMap<TimerId, Timer>,
    /// Directory `timers.json` is persisted under; `None` ⇒ in-memory only (no data dir).
    data_dir: Option<PathBuf>,
}

impl TimerRegistry {
    /// Build a registry seeded with the built-in Simulator, persisting to `data_dir` when given.
    ///
    /// The Simulator's `laps`/`lap_ms` default from `sim_laps`/`sim_lap_ms` (the Director passes
    /// the env defaults). When `data_dir` is `Some` and a `timers.json` already exists, the saved
    /// timers are restored over the top (an unreadable/corrupt file degrades to just the
    /// Simulator rather than failing to boot); a restored Simulator's config wins so a retune
    /// survives a restart. When `data_dir` is `None` the registry is in-memory only.
    pub fn new(
        data_dir: Option<PathBuf>,
        sim_laps: u32,
        sim_lap_ms: u64,
    ) -> Result<Self, TimerError> {
        let mut timers = BTreeMap::new();

        // Always seed the built-in Simulator first.
        let sim = Timer {
            id: TimerId(SIM_TIMER_ID.to_string()),
            name: SIM_TIMER_NAME.to_string(),
            kind: TimerKind::Sim {
                laps: sim_laps,
                lap_ms: sim_lap_ms,
            },
            status: TimerStatus::Ready,
        };
        timers.insert(sim.id.clone(), sim);

        if let Some(dir) = &data_dir {
            std::fs::create_dir_all(dir).map_err(|e| {
                TimerError(format!("could not create data dir {}: {e}", dir.display()))
            })?;
            // Restore persisted timers over the seed (a missing/corrupt file is ignored — the
            // Director still boots with at least the Simulator).
            if let Some(restored) = read_persisted_timers(dir) {
                for mut timer in restored {
                    // Keep the derived status authoritative (never trust a persisted status).
                    timer.status = Timer::status_for(&timer.kind);
                    timers.insert(timer.id.clone(), timer);
                }
            }
        }

        Ok(Self {
            inner: Arc::new(RwLock::new(Registry { timers, data_dir })),
        })
    }

    /// Every timer, **Simulator first**, then the rest in id order — the `GET /timers` body.
    pub fn list(&self) -> Vec<Timer> {
        let reg = self.read();
        let mut out = Vec::with_capacity(reg.timers.len());
        let sim = TimerId(SIM_TIMER_ID.to_string());
        if let Some(s) = reg.timers.get(&sim) {
            out.push(s.clone());
        }
        for (id, timer) in &reg.timers {
            if *id != sim {
                out.push(timer.clone());
            }
        }
        out
    }

    /// Whether a timer with `id` exists — the per-event selection validates each id through this.
    pub fn exists(&self, id: &TimerId) -> bool {
        self.read().timers.contains_key(id)
    }

    /// The [`Timer`] for `id`, or `None` — the source bridge resolves a selected id's config here.
    pub fn get(&self, id: &TimerId) -> Option<Timer> {
        self.read().timers.get(id).cloned()
    }

    /// Create a timer from a [`CreateTimerRequest`], returning it (issue #73).
    ///
    /// The **id is auto-generated** — a slug of the `name` + a short random suffix — so it is
    /// unique and never the reserved `sim`. The derived [`TimerStatus`] is set from the kind, and
    /// the registry is **persisted** on success.
    pub fn create(&self, request: &CreateTimerRequest) -> Result<Timer, TimerError> {
        let mut reg = self.write();
        let id = loop {
            let candidate = TimerId(format!("{}-{}", slugify(&request.name), short_suffix()));
            if candidate.0 != SIM_TIMER_ID && !reg.timers.contains_key(&candidate) {
                break candidate;
            }
        };
        let timer = Timer {
            id: id.clone(),
            name: request.name.trim().to_string(),
            status: Timer::status_for(&request.kind),
            kind: request.kind.clone(),
        };
        reg.timers.insert(id, timer.clone());
        reg.persist()?;
        Ok(timer)
    }

    /// Edit a timer's name and/or kind (issue #73), returning the updated [`Timer`].
    ///
    /// The built-in Simulator may be retuned (e.g. a new `lap_ms`) but not renamed away — any
    /// timer's name/kind is editable. An unknown id is a [`TimerError`]. The registry is
    /// **persisted** on success.
    pub fn update(&self, id: &TimerId, request: &UpdateTimerRequest) -> Result<Timer, TimerError> {
        let mut reg = self.write();
        let timer = reg
            .timers
            .get_mut(id)
            .ok_or_else(|| TimerError(format!("no timer with id {:?}", id.0)))?;
        if let Some(name) = &request.name {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                timer.name = trimmed.to_string();
            }
        }
        if let Some(kind) = &request.kind {
            timer.kind = kind.clone();
            timer.status = Timer::status_for(kind);
        }
        let updated = timer.clone();
        reg.persist()?;
        Ok(updated)
    }

    /// Delete a timer (issue #73). The built-in **Simulator cannot be deleted** (it is always
    /// present); attempting to is a [`TimerError`]. An unknown id is also an error. The registry
    /// is **persisted** on success.
    pub fn delete(&self, id: &TimerId) -> Result<(), TimerError> {
        if id.0 == SIM_TIMER_ID {
            return Err(TimerError(
                "the built-in Simulator timer cannot be deleted".to_string(),
            ));
        }
        let mut reg = self.write();
        if reg.timers.remove(id).is_none() {
            return Err(TimerError(format!("no timer with id {:?}", id.0)));
        }
        reg.persist()?;
        Ok(())
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Registry> {
        self.inner.read().expect("timer registry lock poisoned")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Registry> {
        self.inner.write().expect("timer registry lock poisoned")
    }
}

impl Registry {
    /// Persist the timer set to `<data_dir>/timers.json` (issue #73), a no-op with no data dir.
    /// The Simulator is persisted too so a retune survives a restart.
    fn persist(&self) -> Result<(), TimerError> {
        let Some(dir) = &self.data_dir else {
            return Ok(());
        };
        let timers: Vec<&Timer> = self.timers.values().collect();
        let json = serde_json::to_string_pretty(&timers)
            .map_err(|e| TimerError(format!("could not serialize timers: {e}")))?;
        std::fs::write(timers_path(dir), json)
            .map_err(|e| TimerError(format!("could not persist timers: {e}")))
    }
}

/// The file the timer set is persisted to under `dir`: `<dir>/timers.json`.
fn timers_path(dir: &Path) -> PathBuf {
    dir.join(TIMERS_FILE)
}

/// Read the persisted timers from `<dir>/timers.json`, or `None` if absent/unreadable/corrupt.
/// A bad file degrades to "no persisted timers" so the Director still boots with the Simulator.
fn read_persisted_timers(dir: &Path) -> Option<Vec<Timer>> {
    let raw = std::fs::read_to_string(timers_path(dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// An error mutating the timer registry (a persistence failure, an unknown id, a protected delete).
#[derive(Debug, Clone)]
pub struct TimerError(pub String);

impl std::fmt::Display for TimerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "timer registry error: {}", self.0)
    }
}

impl std::error::Error for TimerError {}

/// Slugify a display name into an id-friendly stem (same rule as the event registry): lowercase
/// ASCII alphanumerics kept, every other run collapsed to a single `-`, trimmed of dashes; an
/// empty/symbol-only name yields `timer`.
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
        "timer".to_string()
    } else {
        trimmed.to_string()
    }
}

/// A short random lowercase-alphanumeric suffix making an auto-generated id unique (same source
/// as the event registry — the OS CSPRNG).
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

    fn sim_req(name: &str) -> CreateTimerRequest {
        CreateTimerRequest {
            name: name.to_string(),
            kind: TimerKind::Sim {
                laps: 3,
                lap_ms: 2000,
            },
        }
    }

    fn rh_req(name: &str, url: &str) -> CreateTimerRequest {
        CreateTimerRequest {
            name: name.to_string(),
            kind: TimerKind::Rotorhazard {
                url: url.to_string(),
            },
        }
    }

    #[test]
    fn simulator_is_always_present_and_first() {
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let list = reg.list();
        let first = list.first().unwrap();
        assert_eq!(first.id.0, SIM_TIMER_ID);
        assert_eq!(first.name, SIM_TIMER_NAME);
        assert_eq!(first.status, TimerStatus::Ready);
        // The Simulator draws its config from the Director's env defaults.
        assert_eq!(
            first.kind,
            TimerKind::Sim {
                laps: 5,
                lap_ms: 2500
            }
        );
    }

    #[test]
    fn create_auto_generates_a_unique_slug_id_and_lists_after_sim() {
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let a = reg
            .create(&rh_req("Track RH!", "http://rh.local:5000"))
            .unwrap();
        let b = reg
            .create(&rh_req("Track RH!", "http://rh.local:5000"))
            .unwrap();
        assert!(a.id.0.starts_with("track-rh-"));
        assert_ne!(a.id, b.id);
        assert_eq!(a.status, TimerStatus::Configured);
        let ids: Vec<_> = reg.list().into_iter().map(|t| t.id).collect();
        assert_eq!(ids[0].0, SIM_TIMER_ID);
        assert!(ids.contains(&a.id) && ids.contains(&b.id));
    }

    #[test]
    fn update_edits_name_and_kind() {
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let created = reg.create(&sim_req("My Sim")).unwrap();
        let updated = reg
            .update(
                &created.id,
                &UpdateTimerRequest {
                    name: Some("Renamed".into()),
                    kind: Some(TimerKind::Sim {
                        laps: 9,
                        lap_ms: 1000,
                    }),
                },
            )
            .unwrap();
        assert_eq!(updated.name, "Renamed");
        assert_eq!(
            updated.kind,
            TimerKind::Sim {
                laps: 9,
                lap_ms: 1000
            }
        );
    }

    #[test]
    fn retuning_the_simulator_is_allowed_but_deleting_it_is_not() {
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let sim = TimerId(SIM_TIMER_ID.into());
        // Retune is fine.
        reg.update(
            &sim,
            &UpdateTimerRequest {
                name: None,
                kind: Some(TimerKind::Sim {
                    laps: 1,
                    lap_ms: 50,
                }),
            },
        )
        .unwrap();
        assert_eq!(
            reg.get(&sim).unwrap().kind,
            TimerKind::Sim {
                laps: 1,
                lap_ms: 50
            }
        );
        // Delete is rejected.
        assert!(reg.delete(&sim).is_err());
        assert!(reg.exists(&sim));
    }

    #[test]
    fn delete_removes_a_created_timer() {
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let created = reg.create(&sim_req("Temp")).unwrap();
        assert!(reg.exists(&created.id));
        reg.delete(&created.id).unwrap();
        assert!(!reg.exists(&created.id));
        assert!(reg.delete(&created.id).is_err());
    }

    #[test]
    fn timers_persist_across_a_restart_with_a_data_dir() {
        let dir = std::env::temp_dir().join(format!("gridfpv-timers-test-{}", short_suffix()));
        {
            let reg = TimerRegistry::new(Some(dir.clone()), 5, 2500).unwrap();
            let created = reg
                .create(&rh_req("Field RH", "http://rh.local:5000"))
                .unwrap();
            // Retune the Simulator too, to prove its config also survives.
            reg.update(
                &TimerId(SIM_TIMER_ID.into()),
                &UpdateTimerRequest {
                    name: None,
                    kind: Some(TimerKind::Sim {
                        laps: 7,
                        lap_ms: 1234,
                    }),
                },
            )
            .unwrap();

            let reopened = TimerRegistry::new(Some(dir.clone()), 5, 2500).unwrap();
            // The created RH timer survived…
            let got = reopened.get(&created.id).unwrap();
            assert_eq!(
                got.kind,
                TimerKind::Rotorhazard {
                    url: "http://rh.local:5000".into()
                }
            );
            assert_eq!(got.status, TimerStatus::Configured);
            // …and so did the retuned Simulator config.
            assert_eq!(
                reopened.get(&TimerId(SIM_TIMER_ID.into())).unwrap().kind,
                TimerKind::Sim {
                    laps: 7,
                    lap_ms: 1234
                }
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_corrupt_timers_file_degrades_to_just_the_simulator() {
        let dir = std::env::temp_dir().join(format!("gridfpv-timers-bad-{}", short_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(timers_path(&dir), b"not json at all").unwrap();
        let reg = TimerRegistry::new(Some(dir.clone()), 5, 2500).unwrap();
        let list = reg.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id.0, SIM_TIMER_ID);
        std::fs::remove_dir_all(&dir).ok();
    }
}
