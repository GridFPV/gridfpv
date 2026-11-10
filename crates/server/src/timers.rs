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
//! | `GET /timers/{id}/config` | the timer's settings, read from the timer |
//! | `PUT /timers/{id}/config` | change them, and read them back to prove they took |
//! | `GET /timers/{id}/live` | server-sent events: live signal and levels, for tuning |
//!
//! **Settings live on the timer.** Grid keeps no copy: a read asks the timer, and a write
//! is followed by a read, so what a screen shows is always what the timer is doing.
//!
//! **Live signal is not logged.** It goes from the connection straight to whoever is
//! listening and is then gone. (A race's signal is logged; see `gridfpv_adapters::oft`.)

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use std::convert::Infallible;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use futures_util::stream::{self, Stream};
use gridfpv_adapters::oft::{OftConfig, OftSeatConfig, OftThreshold};
use gridfpv_events::{AdapterId, SeatRef, SignalChunk};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
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

/// One seat in a timer's settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct TimerSeatConfig {
    pub seat: SeatRef,
    /// The video frequency the seat watches, in MHz.
    pub freq_mhz: u32,
    /// Whether the timer watches this seat at all.
    pub enabled: bool,
}

/// How a timer sets the levels it detects crossings against. Values are tenths of a dB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum TimerThreshold {
    /// Follow the noise floor: enter `snr_enter` above it, exit `hysteresis` below that.
    Auto { snr_enter: i32, hysteresis: i32 },
    /// Fixed enter and exit levels.
    Manual { enter: i32, exit: i32 },
}

/// A timer's settings. Today the threshold is one setting for the whole timer; per-seat
/// thresholds come with the next version of the OFT protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct TimerConfig {
    pub seats: Vec<TimerSeatConfig>,
    /// The most seats one receiver may share. More seats on a receiver means a lower
    /// sample rate for each.
    pub seats_per_node: u32,
    pub threshold: TimerThreshold,
    /// The timer ignores a second crossing on a seat within this many milliseconds.
    pub min_lap_ms: u32,
}

impl From<OftConfig> for TimerConfig {
    fn from(config: OftConfig) -> Self {
        Self {
            seats: config
                .seats
                .into_iter()
                .map(|seat| TimerSeatConfig {
                    seat: gridfpv_adapters::oft::seat_ref(seat.seat),
                    freq_mhz: seat.freq_mhz,
                    enabled: seat.enabled,
                })
                .collect(),
            seats_per_node: config.seats_per_node,
            threshold: match config.threshold {
                OftThreshold::Auto {
                    snr_enter,
                    hysteresis,
                } => TimerThreshold::Auto {
                    snr_enter,
                    hysteresis,
                },
                OftThreshold::Manual { enter, exit } => TimerThreshold::Manual { enter, exit },
            },
            min_lap_ms: config.min_lap_ms,
        }
    }
}

impl TimerConfig {
    /// The settings in an OFT Timer's own shape. A seat handle that is not an OFT seat
    /// number cannot be sent, and is an error rather than a guess.
    fn to_oft(&self) -> Result<OftConfig, TimerError> {
        let seats = self
            .seats
            .iter()
            .map(|seat| {
                Ok(OftSeatConfig {
                    seat: seat.seat.0.parse().map_err(|_| TimerError::BadSeat)?,
                    freq_mhz: seat.freq_mhz,
                    enabled: seat.enabled,
                })
            })
            .collect::<Result<_, TimerError>>()?;
        Ok(OftConfig {
            seats,
            seats_per_node: self.seats_per_node,
            threshold: match self.threshold {
                TimerThreshold::Auto {
                    snr_enter,
                    hysteresis,
                } => OftThreshold::Auto {
                    snr_enter,
                    hysteresis,
                },
                TimerThreshold::Manual { enter, exit } => OftThreshold::Manual { enter, exit },
            },
            min_lap_ms: self.min_lap_ms,
        })
    }
}

/// One seat's levels right now, in tenths of a dB.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SeatLevels {
    pub seat: SeatRef,
    /// The latest signal level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub level: Option<i32>,
    /// The noise floor the timer is tracking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub floor: Option<i32>,
    /// The level that opens a crossing, as the timer is using it now.
    pub enter: i32,
    /// The level that closes it.
    pub exit: i32,
    /// Whether the seat is inside a crossing right now.
    pub crossing: bool,
}

/// What a timer's connection sends to live listeners. Never logged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum TimerLive {
    /// A batch of signal samples for one seat, as the timer sent them.
    Signal(SignalChunk),
    /// Every seat's levels, about once a second.
    Levels(Vec<SeatLevels>),
}

/// Why a directory call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimerError {
    #[error("A timer needs a name.")]
    NoName,
    #[error("A timer needs an address, like 192.168.1.50:7270.")]
    BadAddress,
    #[error("There is no such timer.")]
    NotFound,
    #[error("Could not save the timers: {0}")]
    Io(String),
    #[error("That seat is not one this timer has.")]
    BadSeat,
    /// The timer could not be asked, or said no. The text says which, for the director.
    #[error("{0}")]
    Unreachable(String),
    #[error("The timer did not take the change. It is still set the way it was.")]
    NotTaken,
}

impl From<TimerError> for ProtocolError {
    fn from(error: TimerError) -> Self {
        let code = match error {
            TimerError::NoName | TimerError::BadAddress | TimerError::BadSeat => {
                ErrorCode::BadRequest
            }
            TimerError::Unreachable(_) | TimerError::NotTaken => ErrorCode::Refused,
            TimerError::NotFound => ErrorCode::UnknownScope,
            TimerError::Io(_) => ErrorCode::Internal,
        };
        ProtocolError::new(code, error.to_string())
    }
}

struct Inner {
    timers: BTreeMap<AdapterId, Timer>,
    status: BTreeMap<AdapterId, TimerStatus>,
    /// Where each timer's connection sends live signal and levels.
    live: BTreeMap<AdapterId, broadcast::Sender<TimerLive>>,
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
                live: BTreeMap::new(),
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
        // Dropping the sender ends every live feed of this timer.
        inner.live.remove(id);
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

impl TimerDirectory {
    /// One timer's record.
    pub fn get(&self, id: &AdapterId) -> Option<Timer> {
        self.read().timers.get(id).cloned()
    }

    /// Send something to a timer's live listeners. With nobody listening it is dropped.
    pub fn publish(&self, id: &AdapterId, live: TimerLive) {
        if let Some(sender) = self.read().live.get(id) {
            let _ = sender.send(live);
        }
    }

    /// Listen to a timer's live signal and levels. `None` for a timer that is not in the
    /// directory.
    pub fn listen(&self, id: &AdapterId) -> Option<broadcast::Receiver<TimerLive>> {
        let mut inner = self.write();
        if !inner.timers.contains_key(id) {
            return None;
        }
        Some(
            inner
                .live
                .entry(id.clone())
                // Room for a few seconds of batches. A listener that falls further behind
                // skips ahead: live signal is for looking at now.
                .or_insert_with(|| broadcast::Sender::new(512))
                .subscribe(),
        )
    }
}

/// How long a timer is given to answer a settings request.
const TIMER_ANSWER: Duration = Duration::from_secs(3);

fn config_url(timer: &Timer) -> String {
    format!("http://{}/oft/v1/config", timer.address)
}

/// The sentence for a timer that did not answer. The technical cause goes to the
/// diagnostics log, not the screen: it names URLs and socket errors a director cannot act on.
fn unreachable(timer: &Timer, cause: &reqwest::Error) -> TimerError {
    eprintln!(
        "WARNING: timer {:?} at {} did not answer: {cause}",
        timer.name, timer.address
    );
    TimerError::Unreachable(format!(
        "Could not reach {}. Check that it is switched on and on the network.",
        timer.name
    ))
}

/// Read a timer's settings from the timer.
async fn read_config(timer: &Timer) -> Result<OftConfig, TimerError> {
    let unreachable = |e: reqwest::Error| unreachable(timer, &e);
    reqwest::Client::new()
        .get(config_url(timer))
        .timeout(TIMER_ANSWER)
        .send()
        .await
        .map_err(unreachable)?
        .error_for_status()
        .map_err(unreachable)?
        .json()
        .await
        .map_err(unreachable)
}

/// Write a timer's settings, then read them back. The write only counts if the timer then
/// reports exactly what was sent.
async fn write_config(timer: &Timer, config: &OftConfig) -> Result<OftConfig, TimerError> {
    let response = reqwest::Client::new()
        .put(config_url(timer))
        .timeout(TIMER_ANSWER)
        .json(config)
        .send()
        .await
        .map_err(|e| unreachable(timer, &e))?;
    if !response.status().is_success() {
        // The timer says why in `{"error": "..."}`.
        let why = response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|body| body["error"].as_str().map(str::to_owned))
            .unwrap_or_else(|| "it gave no reason".to_owned());
        return Err(TimerError::Unreachable(format!(
            "{} refused the change: {why}.",
            timer.name
        )));
    }
    let now = read_config(timer).await?;
    if &now != config {
        return Err(TimerError::NotTaken);
    }
    Ok(now)
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

/// `GET /timers/{timer_id}/config` — the timer's settings, read from the timer. Open read.
pub async fn get_timer_config(
    State(registry): State<EventRegistry>,
    Path(timer_id): Path<AdapterId>,
) -> Result<Json<TimerConfig>, ProtocolError> {
    let timer = registry
        .timers()
        .get(&timer_id)
        .ok_or(TimerError::NotFound)?;
    Ok(Json(read_config(&timer).await?.into()))
}

/// `PUT /timers/{timer_id}/config` — change the timer's settings, RD-gated. Answers with
/// the settings as the timer reports them afterwards. If those are not what was sent, the
/// answer is `Refused`: a write that did not land must never look like one that did.
pub async fn put_timer_config(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(timer_id): Path<AdapterId>,
    Json(body): Json<TimerConfig>,
) -> Result<Json<TimerConfig>, ProtocolError> {
    let timer = registry
        .timers()
        .get(&timer_id)
        .ok_or(TimerError::NotFound)?;
    let config = body.to_oft()?;
    Ok(Json(write_config(&timer, &config).await?.into()))
}

/// `GET /timers/{timer_id}/live` — server-sent events for tuning. Open read.
///
/// Sends `signal` events (a [`SignalChunk`] each) and `levels` events (a list of
/// [`SeatLevels`]), as JSON. Nothing is sent while the timer is not connected. The feed
/// ends when the timer is removed.
pub async fn get_timer_live(
    State(registry): State<EventRegistry>,
    Path(timer_id): Path<AdapterId>,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, ProtocolError> {
    let listener = registry
        .timers()
        .listen(&timer_id)
        .ok_or(TimerError::NotFound)?;
    let stream = stream::unfold(listener, |mut listener| async move {
        loop {
            match listener.recv().await {
                Ok(live) => {
                    let (name, data) = match &live {
                        TimerLive::Signal(chunk) => ("signal", serde_json::to_string(chunk)),
                        TimerLive::Levels(levels) => ("levels", serde_json::to_string(levels)),
                    };
                    if let Ok(data) = data {
                        let event = SseEvent::default().event(name).data(data);
                        return Some((Ok(event), listener));
                    }
                }
                // Fell behind: skip to what is happening now.
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
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

    // --- settings, against a fake timer ---------------------------------------------------

    use crate::app::router;
    use crate::events::EventRegistry;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::sync::Mutex;
    use tower::ServiceExt;

    /// How the fake timer treats a write.
    #[derive(Clone, Copy, PartialEq)]
    enum Fake {
        /// Takes it.
        Honest,
        /// Says yes and changes nothing.
        Forgetful,
        /// Says no, with a reason.
        Strict,
    }

    /// A fake OFT timer serving `/oft/v1/config`. Gives its `host:port`.
    async fn fake_timer(kind: Fake) -> String {
        let stored: Arc<Mutex<serde_json::Value>> = Arc::new(Mutex::new(serde_json::json!({
            "seats": [{"seat": 0, "freq_mhz": 5800, "enabled": true}],
            "seats_per_node": 8,
            "threshold": {"mode": "auto", "snr_enter": 150, "hysteresis": 50},
            "min_lap_ms": 2000,
        })));
        let read = {
            let stored = stored.clone();
            move || {
                let stored = stored.clone();
                async move { Json(stored.lock().unwrap().clone()) }
            }
        };
        let write = move |Json(body): Json<serde_json::Value>| {
            let stored = stored.clone();
            async move {
                match kind {
                    Fake::Honest => {
                        *stored.lock().unwrap() = body.clone();
                        (StatusCode::OK, Json(body))
                    }
                    Fake::Forgetful => (StatusCode::OK, Json(body)),
                    Fake::Strict => (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({"error": "freq_mhz out of range"})),
                    ),
                }
            }
        };
        let app = axum::Router::new().route("/oft/v1/config", axum::routing::get(read).put(write));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        addr.to_string()
    }

    async fn call(
        registry: &EventRegistry,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        let body = match body {
            Some(json) => {
                builder = builder.header("Content-Type", "application/json");
                Body::from(json.to_string())
            }
            None => Body::empty(),
        };
        let response = router(registry.clone())
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    async fn with_timer(kind: Fake) -> (EventRegistry, String) {
        let registry = EventRegistry::new(None).unwrap();
        let address = fake_timer(kind).await;
        let timer = registry
            .timers()
            .create(&request("Gate", &address))
            .unwrap();
        (registry, format!("/timers/{}/config", timer.id.0))
    }

    fn changed() -> serde_json::Value {
        serde_json::json!({
            "seats": [
                {"seat": "0", "freq_mhz": 5658, "enabled": true},
                {"seat": "1", "freq_mhz": 5917, "enabled": true},
            ],
            "seats_per_node": 4,
            "threshold": {"Manual": {"enter": -700, "exit": -760}},
            "min_lap_ms": 3000,
        })
    }

    #[tokio::test]
    async fn settings_are_read_from_the_timer() {
        let (registry, uri) = with_timer(Fake::Honest).await;
        let (status, body) = call(&registry, "GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        let config: TimerConfig = serde_json::from_value(body).unwrap();
        assert_eq!(config.seats[0].seat, SeatRef("0".into()));
        assert_eq!(config.seats[0].freq_mhz, 5800);
        assert_eq!(
            config.threshold,
            TimerThreshold::Auto {
                snr_enter: 150,
                hysteresis: 50
            }
        );
    }

    #[tokio::test]
    async fn a_change_is_written_and_read_back() {
        let (registry, uri) = with_timer(Fake::Honest).await;
        let (status, body) = call(&registry, "PUT", &uri, Some(changed())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, changed());
        // And it is what the timer now reports.
        let (_, body) = call(&registry, "GET", &uri, None).await;
        assert_eq!(body, changed());
    }

    #[tokio::test]
    async fn a_write_the_timer_did_not_keep_is_never_reported_as_done() {
        let (registry, uri) = with_timer(Fake::Forgetful).await;
        let (status, body) = call(&registry, "PUT", &uri, Some(changed())).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body["message"],
            "The timer did not take the change. It is still set the way it was."
        );
    }

    #[tokio::test]
    async fn a_timers_own_refusal_is_passed_on_with_its_reason() {
        let (registry, uri) = with_timer(Fake::Strict).await;
        let (status, body) = call(&registry, "PUT", &uri, Some(changed())).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body["message"],
            "Gate refused the change: freq_mhz out of range."
        );
    }

    #[tokio::test]
    async fn a_seat_the_timer_cannot_have_is_a_bad_request_and_nothing_is_sent() {
        let (registry, uri) = with_timer(Fake::Honest).await;
        let mut body = changed();
        body["seats"][0]["seat"] = serde_json::json!("left");
        let (status, _) = call(&registry, "PUT", &uri, Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (_, now) = call(&registry, "GET", &uri, None).await;
        assert_eq!(now["seats"][0]["freq_mhz"], 5800);
    }

    #[tokio::test]
    async fn a_timer_that_cannot_be_reached_says_so_and_an_unknown_timer_is_a_404() {
        let registry = EventRegistry::new(None).unwrap();
        // A port nothing listens on.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        drop(listener);
        let timer = registry
            .timers()
            .create(&request("Gate", &address))
            .unwrap();
        let (status, body) = call(
            &registry,
            "GET",
            &format!("/timers/{}/config", timer.id.0),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .starts_with("Could not reach Gate"),
            "{body}"
        );
        for uri in ["/timers/nope/config", "/timers/nope/live"] {
            let (status, _) = call(&registry, "GET", uri, None).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        }
    }

    #[tokio::test]
    async fn live_data_reaches_listeners_and_the_feed_ends_with_the_timer() {
        let directory = TimerDirectory::new(None);
        let timer = directory.create(&request("Gate", "10.0.0.5:7270")).unwrap();
        // With nobody listening, publishing is harmless.
        directory.publish(&timer.id, TimerLive::Levels(Vec::new()));
        let mut listener = directory.listen(&timer.id).unwrap();
        let levels = TimerLive::Levels(vec![SeatLevels {
            seat: SeatRef("0".into()),
            level: Some(-880),
            floor: Some(-900),
            enter: -750,
            exit: -800,
            crossing: false,
        }]);
        directory.publish(&timer.id, levels.clone());
        assert_eq!(listener.recv().await.unwrap(), levels);
        directory.delete(&timer.id).unwrap();
        assert!(matches!(
            listener.recv().await,
            Err(broadcast::error::RecvError::Closed)
        ));
        assert!(directory.listen(&timer.id).is_none());
    }
}
