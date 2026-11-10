//! The timer directory: the timers this Director knows (`docs/primitives.md` §3.1).
//!
//! A timer is an instrument. It outlives any event, like a pilot or a class, so the list
//! lives beside those directories and is saved to `<data_dir>/timers.json`. The directory
//! holds two things per timer:
//!
//! - the **record** a person made: its name, its kind and its address. Saved.
//! - its **status** right now: connected or not, its seats, how well its clock is known.
//!   Reported by whatever holds the connection, and never saved.
//!
//! This module opens no connection. The Director app watches the directory (see
//! [`TimerDirectory::watch`]) and keeps one connection per timer.
//!
//! | route | what |
//! |-------|------|
//! | `GET /timers` | every timer with its status |
//! | `POST /timers` | add a timer |
//! | `DELETE /timers/{id}` | remove a timer |

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use gridfpv_events::{AdapterId, SeatRef};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use ts_rs::TS;

use crate::auth::ControlAuth;
use crate::error::{ErrorCode, ProtocolError};
use crate::events::EventRegistry;

/// The file the timer records are saved to, under the data dir.
pub const TIMERS_FILE: &str = "timers.json";

/// What kind of timer this is: which adapter speaks to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum TimerKind {
    /// An Open FPV Timer, over its network protocol.
    Oft,
}

/// A timer as a person set it up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct Timer {
    /// The handle passes and seats carry. Made by the Director; never shown.
    pub id: AdapterId,
    /// The name people see ("Gate").
    pub name: String,
    pub kind: TimerKind,
    /// Where it is on the network: `host:port`.
    pub address: String,
}

/// What `POST /timers` takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CreateTimerRequest {
    pub name: String,
    pub kind: TimerKind,
    /// `host:port`.
    pub address: String,
}

/// A timer's state right now.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct TimerStatus {
    pub connected: bool,
    /// Why it is not connected, in words for the director. Absent when connected, or when
    /// nothing has been tried yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub problem: Option<String>,
    /// The seats the timer reported, in its own order.
    pub seats: Vec<SeatRef>,
    /// The most the timer's clock can be off from the Director's, in microseconds. Absent
    /// until it has been measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub clock_error_micros: Option<i64>,
}

/// A timer with its status (`GET /timers`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct TimerView {
    pub timer: Timer,
    pub status: TimerStatus,
}

/// Why a directory call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimerError {
    #[error("a timer needs a name")]
    NoName,
    #[error("a timer needs an address, like 192.168.1.50:7270")]
    BadAddress,
    #[error("there is no such timer")]
    NotFound,
    #[error("could not save the timers: {0}")]
    Io(String),
}

impl From<TimerError> for ProtocolError {
    fn from(error: TimerError) -> Self {
        let code = match error {
            TimerError::NoName | TimerError::BadAddress => ErrorCode::BadRequest,
            TimerError::NotFound => ErrorCode::UnknownScope,
            TimerError::Io(_) => ErrorCode::Internal,
        };
        ProtocolError::new(code, error.to_string())
    }
}

struct Inner {
    timers: BTreeMap<AdapterId, Timer>,
    status: BTreeMap<AdapterId, TimerStatus>,
    data_dir: Option<PathBuf>,
}

/// The shared timer directory. Cloning shares the one directory.
#[derive(Clone)]
pub struct TimerDirectory {
    inner: Arc<RwLock<Inner>>,
    /// Counts up each time the set of records changes, so a watcher knows to look again.
    generation: watch::Sender<u64>,
}

impl TimerDirectory {
    /// Build the directory, loading `<data_dir>/timers.json` when there is one. A file that
    /// cannot be read is reported and treated as empty: a bad file must not stop the
    /// Director from starting.
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        let mut timers = BTreeMap::new();
        if let Some(dir) = &data_dir {
            let path = dir.join(TIMERS_FILE);
            match std::fs::read(&path) {
                Ok(bytes) => match serde_json::from_slice::<Vec<Timer>>(&bytes) {
                    Ok(saved) => {
                        timers.extend(saved.into_iter().map(|t| (t.id.clone(), t)));
                    }
                    Err(e) => eprintln!(
                        "WARNING: ignoring {}: it could not be read as timers: {e}",
                        path.display()
                    ),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => eprintln!("WARNING: could not read {}: {e}", path.display()),
            }
        }
        Self {
            inner: Arc::new(RwLock::new(Inner {
                timers,
                status: BTreeMap::new(),
                data_dir,
            })),
            generation: watch::Sender::new(0),
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Every timer record, in id order.
    pub fn records(&self) -> Vec<Timer> {
        self.read().timers.values().cloned().collect()
    }

    /// Every timer with its status, in id order.
    pub fn list(&self) -> Vec<TimerView> {
        let inner = self.read();
        inner
            .timers
            .values()
            .map(|timer| TimerView {
                timer: timer.clone(),
                status: inner.status.get(&timer.id).cloned().unwrap_or_default(),
            })
            .collect()
    }

    /// A receiver that changes each time a timer is added or removed.
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.generation.subscribe()
    }

    /// Add a timer. Its id is made here: a slug of the name and a short random tail.
    pub fn create(&self, request: &CreateTimerRequest) -> Result<Timer, TimerError> {
        let name = request.name.trim();
        if name.is_empty() {
            return Err(TimerError::NoName);
        }
        let address = request.address.trim();
        // `host:port`, with a port that is a number. The host is not resolved here.
        let well_formed = address
            .rsplit_once(':')
            .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok());
        if !well_formed {
            return Err(TimerError::BadAddress);
        }
        let mut inner = self.write();
        let id = loop {
            let candidate = AdapterId(format!("{}-{}", slug(name), tail()));
            if !inner.timers.contains_key(&candidate) {
                break candidate;
            }
        };
        let timer = Timer {
            id: id.clone(),
            name: name.to_owned(),
            kind: request.kind,
            address: address.to_owned(),
        };
        inner.timers.insert(id, timer.clone());
        if let Err(e) = save(&inner) {
            inner.timers.remove(&timer.id);
            return Err(e);
        }
        drop(inner);
        self.generation.send_modify(|g| *g += 1);
        Ok(timer)
    }

    /// Remove a timer. Its passes stay in the logs of the events it timed.
    pub fn delete(&self, id: &AdapterId) -> Result<(), TimerError> {
        let mut inner = self.write();
        let removed = inner.timers.remove(id).ok_or(TimerError::NotFound)?;
        if let Err(e) = save(&inner) {
            inner.timers.insert(id.clone(), removed);
            return Err(e);
        }
        inner.status.remove(id);
        drop(inner);
        self.generation.send_modify(|g| *g += 1);
        Ok(())
    }

    /// Change a timer's status. Called by whatever holds its connection. A timer that has
    /// been removed is ignored.
    pub fn report(&self, id: &AdapterId, change: impl FnOnce(&mut TimerStatus)) {
        let mut inner = self.write();
        if inner.timers.contains_key(id) {
            change(inner.status.entry(id.clone()).or_default());
        }
    }
}

fn save(inner: &Inner) -> Result<(), TimerError> {
    let Some(dir) = &inner.data_dir else {
        return Ok(());
    };
    let io = |e: std::io::Error| TimerError::Io(e.to_string());
    std::fs::create_dir_all(dir).map_err(io)?;
    let records: Vec<&Timer> = inner.timers.values().collect();
    let bytes = serde_json::to_vec_pretty(&records).map_err(|e| TimerError::Io(e.to_string()))?;
    // Write beside the file, then swap it in, so a crash never leaves half a file.
    let path = dir.join(TIMERS_FILE);
    let scratch = dir.join(format!("{TIMERS_FILE}.tmp"));
    std::fs::write(&scratch, bytes).map_err(io)?;
    std::fs::rename(&scratch, &path).map_err(io)
}

fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() {
        "timer".to_owned()
    } else {
        out.to_owned()
    }
}

fn tail() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut bytes = [0u8; 4];
    getrandom::fill(&mut bytes).expect("OS CSPRNG available");
    bytes
        .iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

/// `GET /timers` — every timer with its status. An open read.
pub async fn list_timers(State(registry): State<EventRegistry>) -> Json<Vec<TimerView>> {
    Json(registry.timers().list())
}

/// `POST /timers` — add a timer, RD-gated.
pub async fn create_timer(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Json(body): Json<CreateTimerRequest>,
) -> Result<Json<Timer>, ProtocolError> {
    Ok(Json(registry.timers().create(&body)?))
}

/// `DELETE /timers/{timer_id}` — remove a timer, RD-gated.
pub async fn delete_timer(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(timer_id): Path<AdapterId>,
) -> Result<StatusCode, ProtocolError> {
    registry.timers().delete(&timer_id)?;
    Ok(StatusCode::OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(name: &str, address: &str) -> CreateTimerRequest {
        CreateTimerRequest {
            name: name.into(),
            kind: TimerKind::Oft,
            address: address.into(),
        }
    }

    #[test]
    fn a_timer_is_added_listed_and_removed() {
        let directory = TimerDirectory::new(None);
        let mut watch = directory.watch();
        let timer = directory
            .create(&request(" Gate ", "10.0.0.5:7270"))
            .unwrap();
        assert_eq!(timer.name, "Gate");
        assert!(timer.id.0.starts_with("gate-"));
        assert!(watch.has_changed().unwrap());
        watch.mark_unchanged();
        let listed = directory.list();
        assert_eq!(listed.len(), 1);
        assert!(!listed[0].status.connected);

        directory.delete(&timer.id).unwrap();
        assert!(watch.has_changed().unwrap());
        assert!(directory.list().is_empty());
        assert_eq!(directory.delete(&timer.id), Err(TimerError::NotFound));
    }

    #[test]
    fn a_timer_needs_a_name_and_a_host_and_port() {
        let directory = TimerDirectory::new(None);
        assert_eq!(
            directory.create(&request("  ", "10.0.0.5:7270")),
            Err(TimerError::NoName)
        );
        for bad in ["", "10.0.0.5", "10.0.0.5:port", ":7270"] {
            assert_eq!(
                directory.create(&request("Gate", bad)),
                Err(TimerError::BadAddress),
                "{bad:?}"
            );
        }
        assert!(
            directory
                .create(&request("Gate", "gate.local:7270"))
                .is_ok()
        );
    }

    #[test]
    fn status_is_reported_live_and_dropped_with_the_timer() {
        let directory = TimerDirectory::new(None);
        let timer = directory.create(&request("Gate", "10.0.0.5:7270")).unwrap();
        directory.report(&timer.id, |status| {
            status.connected = true;
            status.seats = vec![SeatRef("0".into())];
        });
        assert!(directory.list()[0].status.connected);
        // A report for a timer that is gone changes nothing and brings nothing back.
        directory.delete(&timer.id).unwrap();
        directory.report(&timer.id, |status| status.connected = true);
        assert!(directory.list().is_empty());
    }

    #[test]
    fn records_survive_a_restart_and_status_does_not() {
        let dir =
            std::env::temp_dir().join(format!("gridfpv-timers-{}-{}", std::process::id(), tail()));
        let id = {
            let directory = TimerDirectory::new(Some(dir.clone()));
            let timer = directory.create(&request("Gate", "10.0.0.5:7270")).unwrap();
            directory.report(&timer.id, |status| status.connected = true);
            timer.id
        };
        let directory = TimerDirectory::new(Some(dir.clone()));
        let listed = directory.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].timer.id, id);
        assert!(!listed[0].status.connected);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_file_does_not_stop_the_directory_loading() {
        let dir = std::env::temp_dir().join(format!(
            "gridfpv-timers-bad-{}-{}",
            std::process::id(),
            tail()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(TIMERS_FILE), b"{ not json").unwrap();
        assert!(TimerDirectory::new(Some(dir.clone())).list().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
