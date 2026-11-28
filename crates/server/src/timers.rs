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
//! | `GET /timers/discovered` | OFT timers found on the network, added or not |
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
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use futures_util::stream::{self, Stream};
use gridfpv_adapters::oft::{OftConfig, OftInfo, OftSeatConfig, OftThreshold};
use gridfpv_events::{AdapterId, SeatRef, SignalChunk, SignalUnit};
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
    /// A RotorHazard timer, as it ships, over its own socket. Nothing is installed on it.
    RotorHazard,
}

/// A timer as a person set it up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct Timer {
    /// The handle passes and seats carry. Made by the Director; never shown.
    pub id: AdapterId,
    /// The name people see ("Gate"). A person chooses it and can change it. It is Grid's
    /// own: the timer is not told, and the name the timer gives itself is `own_id`.
    pub name: String,
    pub kind: TimerKind,
    /// Where it is on the network: `host:port`. For a timer with an id of its own this
    /// follows the timer: when it is found on the network at a new address (a new DHCP
    /// lease, say), the Director moves the record there by itself.
    pub address: String,
    /// The id the timer gives itself, where its kind has one. An OFT timer does
    /// ("oft-3fa2"): it says it when it connects and when it announces itself on the
    /// network. It is what makes a timer the same timer at a new address. Other kinds
    /// leave it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub own_id: Option<String>,
}

/// What `POST /timers` takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CreateTimerRequest {
    pub name: String,
    pub kind: TimerKind,
    /// `host:port`.
    pub address: String,
    /// The timer's own id, when it is known already (it was found on the network). Left
    /// out, the Director learns it when the timer first connects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub own_id: Option<String>,
}

/// What `PUT /timers/{id}` takes: the name people see. Nothing else about a timer is
/// changed this way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct RenameTimerRequest {
    pub name: String,
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
    /// Whether each seat can carry its own threshold. A timer that cannot has one
    /// threshold for all its seats.
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub seat_thresholds: bool,
    /// Whether the timer can keep its settings across a restart. Grid asks such a timer to
    /// keep them after every change. A timer that cannot starts on its defaults.
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub keeps_settings: bool,
    /// Whether the timer has a pass window: it can take several crossings close together
    /// as one pass. The event's lap rules set it (`docs/primitives.md` §12.6).
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub pass_window: bool,
    /// The pass window the timer has, in milliseconds, as Grid last read it back. `0` is
    /// off. Absent until Grid has read it, or on a timer with no pass window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pass_window_ms: Option<u32>,
    /// Why the event's pass window could not be put on the timer, in words. Absent when it
    /// was, or when nothing has been tried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pass_window_problem: Option<String>,
    /// What the timer's level numbers measure, when every seat reports in the same unit. A
    /// console shows the timer's levels and thresholds in it. Absent until the timer has
    /// connected, and on a timer whose seats do not share one unit: each seat's live levels
    /// then say their own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub level_unit: Option<SignalUnit>,
    /// Who may change the timer's settings right now. An OFT timer lets one program at a
    /// time change it (OFT spec §6.6); Grid asks to be that program when it connects, and
    /// the first to ask is the one. Absent until the timer has said, on a timer that is
    /// not connected, and on a timer that lets anyone write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub control: Option<TimerControl>,
    /// The name Grid asks for control under, as the timer's own page lists it ("GridFPV
    /// race-laptop"). A person handing control to Grid on that page looks for this name.
    /// Absent on a timer that lets anyone write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub control_name: Option<String>,
    /// A number the timer changes whenever its settings change, whoever changed them. A
    /// console that shows the settings reads them again when it moves. It starts again
    /// when the timer does. Absent on a timer that does not give one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub config_rev: Option<u32>,
    /// What the timer says is wrong with it, as the short names it gives (OFT spec §6.3):
    /// `node_missing`, `safe_mode`, `crashed`, or a name Grid does not know. A console
    /// turns the ones it knows into words. Empty when the timer knows of nothing wrong,
    /// while it is not connected, and on a timer that does not say.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub timer_problems: Vec<String>,
    /// The address of the timer's own page, where its network, wiring and firmware are
    /// set. Absent until the timer has said, and on a timer with no such page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub manage_url: Option<String>,
}

/// Who may change a timer's settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
#[serde(tag = "holder")]
pub enum TimerControl {
    /// No program has control, so any may write, Grid included.
    Nobody,
    /// Grid has control.
    Grid,
    /// Another program has control. Grid can read the timer and take its laps, but the
    /// timer refuses Grid's changes until a person hands control over on the timer's own
    /// page.
    Other {
        /// The name that program gave itself ("RotorHazard pi-timer").
        name: String,
    },
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
    /// The seat's own threshold. Absent means it uses the timer-wide one. Only a timer
    /// whose status says `seat_thresholds` takes these.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub threshold: Option<TimerThreshold>,
}

impl From<OftThreshold> for TimerThreshold {
    fn from(threshold: OftThreshold) -> Self {
        match threshold {
            OftThreshold::Auto {
                snr_enter,
                hysteresis,
            } => Self::Auto {
                snr_enter,
                hysteresis,
            },
            OftThreshold::Manual { enter, exit } => Self::Manual { enter, exit },
        }
    }
}

impl From<TimerThreshold> for OftThreshold {
    fn from(threshold: TimerThreshold) -> Self {
        match threshold {
            TimerThreshold::Auto {
                snr_enter,
                hysteresis,
            } => Self::Auto {
                snr_enter,
                hysteresis,
            },
            TimerThreshold::Manual { enter, exit } => Self::Manual { enter, exit },
        }
    }
}

/// How a timer sets the levels it detects crossings against. Values are in the timer's own
/// level unit ([`TimerStatus::level_unit`]): tenths of a dB on most, millivolts on some.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum TimerThreshold {
    /// Follow the noise floor: enter `snr_enter` above it, exit `hysteresis` below that.
    Auto { snr_enter: i32, hysteresis: i32 },
    /// Fixed enter and exit levels.
    Manual { enter: i32, exit: i32 },
}

/// A timer's settings, as Grid changes them. `threshold` is the timer-wide one; a seat may
/// carry its own on a timer that takes them (OFT from protocol v0.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct TimerConfig {
    pub seats: Vec<TimerSeatConfig>,
    pub threshold: TimerThreshold,
    /// The timer's lap cooldown, in milliseconds: after a crossing on a seat, the timer
    /// reports no other on that seat for this long. It is there to swallow the bounce of
    /// one pass, at the timer. It is not the race's minimum lap: that is the event's lap
    /// rule, and Grid applies it to what the timer reports. A write that leaves it out
    /// keeps what the timer has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub lap_cooldown_ms: Option<u32>,
    /// How long the signal must stay below the exit level before a crossing closes, in
    /// milliseconds. Absent on a timer that does not have the setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub exit_hold_ms: Option<u32>,
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
                    threshold: seat.threshold.map(Into::into),
                })
                .collect(),
            threshold: config.threshold.into(),
            lap_cooldown_ms: Some(config.min_lap_ms),
            exit_hold_ms: config.exit_hold_ms,
        }
    }
}

impl TimerConfig {
    /// The settings in an OFT Timer's own shape. A write replaces the whole of a Timer's
    /// settings, so what Grid does not change is carried over from `now`, the settings the
    /// Timer has. A seat handle that is not an OFT seat number cannot be sent, and is an
    /// error rather than a guess.
    fn to_oft(&self, now: &OftConfig) -> Result<OftConfig, TimerError> {
        let seats = self
            .seats
            .iter()
            .map(|seat| {
                Ok(OftSeatConfig {
                    seat: seat.seat.0.parse().map_err(|_| TimerError::BadSeat)?,
                    freq_mhz: seat.freq_mhz,
                    enabled: seat.enabled,
                    threshold: seat.threshold.map(Into::into),
                })
            })
            .collect::<Result<_, TimerError>>()?;
        Ok(OftConfig {
            seats,
            threshold: self.threshold.into(),
            min_lap_ms: self.lap_cooldown_ms.unwrap_or(now.min_lap_ms),
            exit_hold_ms: self.exit_hold_ms,
            // The pass window is the event's to set, not this page's: what the Timer has
            // is carried over. Left out, the Timer would switch it off.
            pass_window_ms: now.pass_window_ms,
        })
    }
}

/// One seat's levels right now, in `unit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SeatLevels {
    pub seat: SeatRef,
    /// What the numbers measure: the unit of the receiver serving this seat.
    pub unit: SignalUnit,
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

/// An OFT timer found on the network (it advertises itself over mDNS). Finding one adds
/// nothing: a person adds it. What it says about itself here is a hint; the timer itself is
/// the truth once it is connected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct DiscoveredTimer {
    /// The name the timer gives itself.
    pub name: String,
    /// The id the timer gives itself, when it announces one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub id: Option<String>,
    /// Where it is: `host:port`, ready to be used as a timer's address.
    pub address: String,
    /// How many seats it says it has on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub seats: Option<u32>,
    /// Whether this timer is already in the directory: one with its id, or one at this
    /// address.
    pub added: bool,
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
    #[error(
        "This timer has one threshold for all its seats. It cannot take one for a single seat."
    )]
    OneThreshold,
    /// The timer could not be asked, or said no. The text says which, for the director.
    #[error("{0}")]
    Unreachable(String),
    #[error("The timer did not take the change. It is still set the way it was.")]
    NotTaken,
    #[error(
        "The timer took the change, but did not keep it for its next start. Apply it again after the timer restarts."
    )]
    NotKept,
    #[error(
        "Changing a RotorHazard timer's settings from Grid is not built yet. Use its own Settings page for now."
    )]
    NotBuilt,
    /// Another program has control of the timer (OFT spec §6.6), so it refuses Grid's
    /// writes. The text names the timer and the program.
    #[error("{0}")]
    Controlled(String),
}

/// The sentence for a timer another program has control of.
fn controlled(timer: &Timer, holder: &str) -> TimerError {
    let holder = holder.trim();
    let who = if holder.is_empty() {
        "Another program has".to_owned()
    } else {
        format!("\u{201c}{holder}\u{201d} has")
    };
    TimerError::Controlled(format!(
        "{who} control of {}, so Grid cannot change it. Open the timer's own page and give control to Grid.",
        timer.name
    ))
}

impl From<TimerError> for ProtocolError {
    fn from(error: TimerError) -> Self {
        let code = match error {
            TimerError::NoName
            | TimerError::BadAddress
            | TimerError::BadSeat
            | TimerError::OneThreshold => ErrorCode::BadRequest,
            TimerError::Unreachable(_)
            | TimerError::NotTaken
            | TimerError::NotKept
            | TimerError::NotBuilt
            | TimerError::Controlled(_) => ErrorCode::Refused,
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
    /// Timers found on the network, by the full name they advertise under.
    discovered: BTreeMap<String, DiscoveredTimer>,
    /// Each timer's seat channels, as last read from it. See [`seat_channels`].
    channels: BTreeMap<AdapterId, KnownChannels>,
    /// The key each timer gave Grid while Grid has control of it (OFT spec §6.6). Every
    /// write to that timer carries it. Never saved and never sent to a console.
    control_keys: BTreeMap<AdapterId, String>,
    data_dir: Option<PathBuf>,
}

/// A timer's seat channels as last read, and when the Director last looked.
#[derive(Debug, Clone)]
struct KnownChannels {
    /// The switched-on seats and their frequencies, from the last read that worked.
    seats: Vec<(SeatRef, u32)>,
    /// When the Director last asked the timer, whether or not it answered.
    asked_at: Instant,
    /// Whether someone is asking right now, so two askers do not both go to the timer.
    asking: bool,
}

/// How long a timer's seat channels are taken as read before the Director asks it again.
/// However many phones are watching, a timer is asked at most once in this long.
pub const CHANNELS_MAX_AGE: Duration = Duration::from_secs(15);

/// The video channel of one seat: which frequency a pilot on that seat flies on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SeatChannel {
    pub adapter: AdapterId,
    pub seat: SeatRef,
    /// In MHz, such as 5658.
    pub freq_mhz: u32,
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
                discovered: BTreeMap::new(),
                channels: BTreeMap::new(),
                control_keys: BTreeMap::new(),
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
            own_id: request
                .own_id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_owned),
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
    /// Give a timer another name. Only the name people see changes: the handle, the
    /// address and the connection stay as they are, and so does everything already timed.
    pub fn rename(&self, id: &AdapterId, name: &str) -> Result<Timer, TimerError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(TimerError::NoName);
        }
        let mut inner = self.write();
        let record = inner.timers.get_mut(id).ok_or(TimerError::NotFound)?;
        let before = std::mem::replace(&mut record.name, name.to_owned());
        let renamed = record.clone();
        if let Err(e) = save(&inner) {
            if let Some(record) = inner.timers.get_mut(id) {
                record.name = before;
            }
            return Err(e);
        }
        drop(inner);
        self.generation.send_modify(|g| *g += 1);
        Ok(renamed)
    }

    pub fn delete(&self, id: &AdapterId) -> Result<(), TimerError> {
        let mut inner = self.write();
        let removed = inner.timers.remove(id).ok_or(TimerError::NotFound)?;
        if let Err(e) = save(&inner) {
            inner.timers.insert(id.clone(), removed);
            return Err(e);
        }
        inner.status.remove(id);
        inner.channels.remove(id);
        inner.control_keys.remove(id);
        // Dropping the sender ends every live feed of this timer.
        inner.live.remove(id);
        drop(inner);
        self.generation.send_modify(|g| *g += 1);
        Ok(())
    }

    /// Remember a timer's seat channels from settings that were just read from it or
    /// written to it. Seats that are switched off have no channel to fly on.
    pub fn note_channels(&self, id: &AdapterId, config: &TimerConfig, now: Instant) {
        let mut inner = self.write();
        if !inner.timers.contains_key(id) {
            return;
        }
        inner.channels.insert(
            id.clone(),
            KnownChannels {
                seats: config
                    .seats
                    .iter()
                    .filter(|seat| seat.enabled)
                    .map(|seat| (seat.seat.clone(), seat.freq_mhz))
                    .collect(),
                asked_at: now,
                asking: false,
            },
        );
    }

    /// A timer's seat channels as last read. Empty when it has never answered.
    pub fn known_channels(&self, id: &AdapterId) -> Vec<SeatChannel> {
        self.read()
            .channels
            .get(id)
            .map(|known| {
                known
                    .seats
                    .iter()
                    .map(|(seat, freq_mhz)| SeatChannel {
                        adapter: id.clone(),
                        seat: seat.clone(),
                        freq_mhz: *freq_mhz,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether the caller should ask the timer for its channels now: nobody else is
    /// asking, and it has not been asked within [`CHANNELS_MAX_AGE`]. A `true` answer marks
    /// the timer as being asked; the caller must end with [`note_channels`](Self::note_channels)
    /// or [`channels_not_read`](Self::channels_not_read).
    pub fn should_ask_channels(&self, id: &AdapterId, now: Instant) -> bool {
        let mut inner = self.write();
        if !inner.timers.contains_key(id) {
            return false;
        }
        match inner.channels.get_mut(id) {
            Some(known) if known.asking => false,
            Some(known) if now.duration_since(known.asked_at) < CHANNELS_MAX_AGE => false,
            Some(known) => {
                known.asking = true;
                true
            }
            None => {
                inner.channels.insert(
                    id.clone(),
                    KnownChannels {
                        seats: Vec::new(),
                        asked_at: now,
                        asking: true,
                    },
                );
                true
            }
        }
    }

    /// The timer did not answer. What was known before is kept, and the timer is not asked
    /// again until [`CHANNELS_MAX_AGE`] has passed: a timer that is switched off must not be
    /// asked on every request.
    pub fn channels_not_read(&self, id: &AdapterId, now: Instant) {
        if let Some(known) = self.write().channels.get_mut(id) {
            known.asked_at = now;
            known.asking = false;
        }
    }

    /// Keep, or forget, the key a timer gave Grid for its writes. Called by whatever holds
    /// the timer's connection, as the timer says who has control. A timer that has been
    /// removed is ignored.
    pub fn set_control_key(&self, id: &AdapterId, key: Option<String>) {
        let mut inner = self.write();
        match key {
            Some(key) if inner.timers.contains_key(id) => {
                inner.control_keys.insert(id.clone(), key);
            }
            _ => {
                inner.control_keys.remove(id);
            }
        }
    }

    /// The key Grid's writes to a timer must carry, while Grid has control of it.
    pub fn control_key(&self, id: &AdapterId) -> Option<String> {
        self.read().control_keys.get(id).cloned()
    }

    /// One timer's status right now. A timer that has reported nothing has the default.
    pub fn status(&self, id: &AdapterId) -> TimerStatus {
        self.read().status.get(id).cloned().unwrap_or_default()
    }

    /// Make a timer's channels count as old, so the next asker goes to the timer: its
    /// settings changed, or a test wants it asked.
    pub fn age_channels(&self, id: &AdapterId) {
        if let Some(known) = self.write().channels.get_mut(id) {
            known.asked_at = known
                .asked_at
                .checked_sub(CHANNELS_MAX_AGE * 2)
                .unwrap_or(known.asked_at);
        }
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
    /// A timer was found on the network, or what it says about itself changed. `key` is the
    /// full name it advertises under.
    ///
    /// A timer in the directory with the same id of its own is the same timer. If it is
    /// now at another address, its record is moved there, and whatever holds its connection
    /// is told to connect again. Discovery only reports an address that answered.
    pub fn found(&self, key: &str, timer: DiscoveredTimer) {
        let mut inner = self.write();
        let mut moved = false;
        if let Some(own_id) = timer.id.as_deref() {
            let before = inner.timers.clone();
            for record in inner.timers.values_mut() {
                if record.own_id.as_deref() == Some(own_id) && record.address != timer.address {
                    eprintln!(
                        "Timer {:?} moved from {} to {}.",
                        record.name, record.address, timer.address
                    );
                    record.address = timer.address.clone();
                    moved = true;
                }
            }
            if moved && let Err(e) = save(&inner) {
                // Not saved, so not moved: the next announcement tries again.
                eprintln!("WARNING: a timer's new address could not be saved: {e}");
                inner.timers = before;
                moved = false;
            }
        }
        inner.discovered.insert(key.to_owned(), timer);
        drop(inner);
        if moved {
            self.generation.send_modify(|g| *g += 1);
        }
    }

    /// Note the id a timer gave for itself when it connected. Kept with its record, so the
    /// timer can be followed to a new address later. Nothing changes when it is already
    /// known.
    pub fn learn_own_id(&self, id: &AdapterId, own_id: &str) {
        let own_id = own_id.trim();
        if own_id.is_empty() {
            return;
        }
        let mut inner = self.write();
        let Some(record) = inner.timers.get_mut(id) else {
            return;
        };
        if record.own_id.as_deref() == Some(own_id) {
            return;
        }
        let before = record.own_id.replace(own_id.to_owned());
        if let Err(e) = save(&inner) {
            eprintln!("WARNING: a timer's own id could not be saved: {e}");
            if let Some(record) = inner.timers.get_mut(id) {
                record.own_id = before;
            }
        }
    }

    /// A timer found earlier has gone from the network.
    pub fn lost(&self, key: &str) {
        self.write().discovered.remove(key);
    }

    /// The timers found on the network, by name, each marked if a timer at its address is
    /// already in the directory.
    pub fn discovered(&self) -> Vec<DiscoveredTimer> {
        let inner = self.read();
        let mut found: Vec<DiscoveredTimer> = inner
            .discovered
            .values()
            .map(|timer| DiscoveredTimer {
                added: inner.timers.values().any(|t| {
                    t.address == timer.address || (timer.id.is_some() && t.own_id == timer.id)
                }),
                ..timer.clone()
            })
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.address.cmp(&b.address)));
        found
    }

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
/// Read what an OFT timer says about its receivers (`GET /oft/v1/info`). A timer's `hello`
/// carries the same list, but only once per connection, and a receiver can report in, or
/// come back from a firmware update measuring in another unit, after that.
pub async fn read_info(timer: &Timer) -> Result<OftInfo, TimerError> {
    let unreachable = |e: reqwest::Error| unreachable(timer, &e);
    reqwest::Client::new()
        .get(format!("http://{}/oft/v1/info", timer.address))
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

/// The header a write carries while a claim holds control of the timer (OFT spec §6.6).
const CONTROL_HEADER: &str = "OFT-Control";

/// How long a refused write waits for the timer to say that Grid has control after all.
const CONTROL_SETTLES: Duration = Duration::from_secs(1);

/// Send a write to an OFT timer, carrying Grid's key when Grid has control of the timer.
/// With nobody in control the timer takes a write with or without a key.
///
/// Control comes to Grid a moment before Grid hears of it: the timer makes Grid's claim
/// the holder, and the key follows on the stream. A write in that moment goes out with no
/// key, or an old one, and is refused with a `423`. So after a `423` this waits a moment
/// for a key the write did not carry, and sends the write once more with it. A `423`
/// that stands is given back as the answer, for [`refusal`] to put into words.
async fn send_write(
    directory: &TimerDirectory,
    timer: &Timer,
    method: reqwest::Method,
    url: String,
    body: Option<&OftConfig>,
) -> Result<reqwest::Response, TimerError> {
    let send = |key: Option<String>| {
        let mut request = reqwest::Client::new()
            .request(method.clone(), url.clone())
            .timeout(TIMER_ANSWER);
        if let Some(key) = key {
            request = request.header(CONTROL_HEADER, key);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        request.send()
    };
    let sent_with = directory.control_key(&timer.id);
    let response = send(sent_with.clone())
        .await
        .map_err(|e| unreachable(timer, &e))?;
    if response.status() != reqwest::StatusCode::LOCKED {
        return Ok(response);
    }
    let waited = Instant::now();
    loop {
        let key = directory.control_key(&timer.id);
        let ours = directory.status(&timer.id).control == Some(TimerControl::Grid);
        if ours && key.is_some() && key != sent_with {
            return send(key).await.map_err(|e| unreachable(timer, &e));
        }
        if waited.elapsed() >= CONTROL_SETTLES {
            return Ok(response);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Stop a write the timer is known to refuse: another program has control of it. Saying so
/// here names the program; the timer is still the judge of every write that is sent.
fn may_write(directory: &TimerDirectory, timer: &Timer) -> Result<(), TimerError> {
    match directory.status(&timer.id).control {
        Some(TimerControl::Other { name }) => Err(controlled(timer, &name)),
        _ => Ok(()),
    }
}

/// Why a timer refused a write, from its answer. A `423` means another program has
/// control (OFT spec §6.6); anything else is passed on with the timer's own reason.
async fn refusal(timer: &Timer, response: reqwest::Response) -> TimerError {
    let locked = response.status() == reqwest::StatusCode::LOCKED;
    // The timer says why in `{"error": "..."}`.
    let why = response
        .json::<serde_json::Value>()
        .await
        .ok()
        .and_then(|body| body["error"].as_str().map(str::to_owned));
    if locked {
        // "controlled by RotorHazard".
        let holder = why
            .as_deref()
            .and_then(|why| why.strip_prefix("controlled by "))
            .unwrap_or("");
        return controlled(timer, holder);
    }
    TimerError::Unreachable(format!(
        "{} refused the change: {}.",
        timer.name,
        why.as_deref().unwrap_or("it gave no reason")
    ))
}

/// Write a timer's settings, then read them back. The write only counts if the timer then
/// reports exactly what was sent.
async fn write_config(
    directory: &TimerDirectory,
    timer: &Timer,
    config: &OftConfig,
) -> Result<OftConfig, TimerError> {
    may_write(directory, timer)?;
    let url = config_url(timer);
    let response = send_write(directory, timer, reqwest::Method::PUT, url, Some(config)).await?;
    if !response.status().is_success() {
        return Err(refusal(timer, response).await);
    }
    let now = read_config(timer).await?;
    if &now != config {
        return Err(TimerError::NotTaken);
    }
    Ok(now)
}

/// Ask a timer to keep the settings it is using as its power-on settings (OFT
/// `POST /config/save`). The write only counts if the timer then says they are kept.
async fn keep_config(directory: &TimerDirectory, timer: &Timer) -> Result<(), TimerError> {
    #[derive(Deserialize)]
    struct Kept {
        #[serde(default)]
        current_is_saved: bool,
    }
    let unreachable = |e: reqwest::Error| unreachable(timer, &e);
    let url = format!("{}/save", config_url(timer));
    let response = send_write(directory, timer, reqwest::Method::POST, url, None).await?;
    if response.status() == reqwest::StatusCode::LOCKED {
        return Err(refusal(timer, response).await);
    }
    let kept: Kept = response
        .error_for_status()
        .map_err(unreachable)?
        .json()
        .await
        .map_err(unreachable)?;
    if kept.current_is_saved {
        Ok(())
    } else {
        Err(TimerError::NotKept)
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

/// `GET /timers/discovered` — OFT timers found on the network. An open read.
pub async fn list_discovered(State(registry): State<EventRegistry>) -> Json<Vec<DiscoveredTimer>> {
    Json(registry.timers().discovered())
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
/// `PUT /timers/{timer_id}` — give a timer another name, RD-gated. Answers with the timer
/// as it now is. A blank name is a **400**; an unknown timer a **404**.
pub async fn rename_timer(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(timer_id): Path<AdapterId>,
    Json(body): Json<RenameTimerRequest>,
) -> Result<Json<Timer>, ProtocolError> {
    Ok(Json(registry.timers().rename(&timer_id, &body.name)?))
}

pub async fn delete_timer(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(timer_id): Path<AdapterId>,
) -> Result<StatusCode, ProtocolError> {
    registry.timers().delete(&timer_id)?;
    // Its saved profiles go with it. A list that could not be saved is not worth failing
    // the removal for: the timer is already gone.
    if let Err(e) = registry.library().forget_timer(&timer_id) {
        eprintln!("WARNING: the removed timer's profiles could not be cleared: {e}");
    }
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
    if timer.kind != TimerKind::Oft {
        return Err(TimerError::NotBuilt.into());
    }
    let config: TimerConfig = read_config(&timer).await?.into();
    registry
        .timers()
        .note_channels(&timer_id, &config, Instant::now());
    Ok(Json(config))
}

/// A timer's seat channels: which frequency each switched-on seat is on.
///
/// The answer comes from what the Director last read from the timer. The timer itself is
/// asked at most once in [`CHANNELS_MAX_AGE`], whoever is asking and however often, so a
/// field full of phones costs the timer nothing. A timer that does not answer keeps the
/// channels it last gave. Only an OFT timer says its channels.
pub async fn seat_channels(registry: &EventRegistry, timer_id: &AdapterId) -> Vec<SeatChannel> {
    let directory = registry.timers();
    let Some(timer) = directory.get(timer_id) else {
        return Vec::new();
    };
    if timer.kind == TimerKind::Oft && directory.should_ask_channels(timer_id, Instant::now()) {
        match read_config(&timer).await {
            Ok(config) => directory.note_channels(timer_id, &config.into(), Instant::now()),
            Err(_) => directory.channels_not_read(timer_id, Instant::now()),
        }
    }
    directory.known_channels(timer_id)
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
    Ok(Json(apply_config(&registry, &timer_id, &body).await?))
}

/// Write settings to a timer and answer with what it then reports. Used by a settings
/// write and by applying a saved profile.
pub async fn apply_config(
    registry: &EventRegistry,
    timer_id: &AdapterId,
    body: &TimerConfig,
) -> Result<TimerConfig, ProtocolError> {
    let timer = registry
        .timers()
        .get(timer_id)
        .ok_or(TimerError::NotFound)?;
    if timer.kind != TimerKind::Oft {
        return Err(TimerError::NotBuilt.into());
    }
    // A timer that has one threshold for every seat would drop a seat's own without a
    // word, and the write would then fail its readback with a vaguer message. Say it here.
    let status = registry.timers().status(timer_id);
    if !status.seat_thresholds && body.seats.iter().any(|seat| seat.threshold.is_some()) {
        return Err(TimerError::OneThreshold.into());
    }
    // Said before anything is read, so a director is not told about a typing mistake in a
    // change the timer would refuse anyway.
    may_write(&registry.timers(), &timer)?;
    let config = body.to_oft(&read_config(&timer).await?)?;
    let written = write_config(&registry.timers(), &timer, &config).await?;
    // A timer that can keep its settings is asked to, every time: after "Apply" the timer
    // is set, and a power cycle at the field must not quietly put its defaults back.
    if status.keeps_settings {
        keep_config(&registry.timers(), &timer).await?;
    }
    let written: TimerConfig = written.into();
    registry
        .timers()
        .note_channels(timer_id, &written, Instant::now());
    Ok(written)
}

/// Put the event's pass window on every timer that can take one, and note on each what it
/// then has (`docs/primitives.md` §12.6).
///
/// `window_ms` is the open event's lap rule, `0` for off. Each connected OFT timer that
/// has a pass window is read; where it differs it is written and read back, as every
/// write to a timer is. A timer that cannot be reached, or does not take the write, gets
/// the reason on its status, where the Run page shows it. So does a timer another program
/// has control of, when its window is not the event's: Grid cannot put it right. A timer
/// with no pass window is left alone: its status already says it has none.
///
/// The Timer's other settings are never touched: the write sends back exactly what was
/// read, with the window changed.
pub async fn sync_pass_window(registry: &EventRegistry, window_ms: u32) {
    for view in registry.timers().list() {
        let TimerView { timer, status } = view;
        if timer.kind != TimerKind::Oft || !status.connected || !status.pass_window {
            continue;
        }
        let outcome = async {
            let now = read_config(&timer).await?;
            if now.pass_window_ms == Some(window_ms) {
                return Ok(window_ms);
            }
            let wanted = OftConfig {
                pass_window_ms: Some(window_ms),
                ..now
            };
            let written = write_config(&registry.timers(), &timer, &wanted).await?;
            Ok::<u32, TimerError>(written.pass_window_ms.unwrap_or(0))
        }
        .await;
        registry
            .timers()
            .report(&timer.id, |status| match &outcome {
                Ok(has) => {
                    status.pass_window_ms = Some(*has);
                    status.pass_window_problem = None;
                }
                Err(err) => {
                    status.pass_window_problem = Some(ProtocolError::from(err.clone()).message);
                }
            });
    }
}

/// The open event's pass window, in milliseconds: `0` when it sets none, or no event is open.
pub fn open_events_pass_window(registry: &EventRegistry) -> u32 {
    let Some(event) = registry.active() else {
        return 0;
    };
    crate::control::with_event(registry, &event.id, |session| {
        Ok(session.state().lap_rules.pass_window_micros)
    })
    .ok()
    .flatten()
    .map_or(0, |micros| u32::try_from(micros / 1000).unwrap_or(0))
}

/// Bring the timers into step with the open event's pass window, in the background. Called
/// whenever either side may have changed: the lap rules were set, a heat went on the timer,
/// a race was started, another event was opened, or a timer connected.
pub fn spawn_pass_window_sync(registry: &EventRegistry) {
    let registry = registry.clone();
    tokio::spawn(async move {
        let window_ms = open_events_pass_window(&registry);
        sync_pass_window(&registry, window_ms).await;
    });
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
            own_id: None,
        }
    }

    /// Settings with seats on these frequencies; `None` is a seat that is switched off.
    fn settings(seats: &[Option<u32>]) -> TimerConfig {
        TimerConfig {
            seats: seats
                .iter()
                .enumerate()
                .map(|(index, freq)| TimerSeatConfig {
                    seat: SeatRef(index.to_string()),
                    freq_mhz: freq.unwrap_or(5800),
                    enabled: freq.is_some(),
                    threshold: None,
                })
                .collect(),
            threshold: TimerThreshold::Auto {
                snr_enter: 10,
                hysteresis: 3,
            },
            lap_cooldown_ms: Some(2000),
            exit_hold_ms: None,
        }
    }

    #[test]
    fn a_timers_seat_channels_are_remembered_and_only_for_seats_that_are_on() {
        let directory = TimerDirectory::new(None);
        let gate = directory
            .create(&request("Gate", "10.0.0.5:7270"))
            .unwrap()
            .id;
        assert!(
            directory.known_channels(&gate).is_empty(),
            "nothing is known at first"
        );
        directory.note_channels(
            &gate,
            &settings(&[Some(5658), None, Some(5732)]),
            Instant::now(),
        );
        assert_eq!(
            directory.known_channels(&gate),
            [
                SeatChannel {
                    adapter: gate.clone(),
                    seat: SeatRef("0".into()),
                    freq_mhz: 5658
                },
                SeatChannel {
                    adapter: gate.clone(),
                    seat: SeatRef("2".into()),
                    freq_mhz: 5732
                }
            ]
        );
        // Newer settings replace the old ones whole.
        directory.note_channels(&gate, &settings(&[Some(5800)]), Instant::now());
        assert_eq!(directory.known_channels(&gate).len(), 1);
        assert_eq!(directory.known_channels(&gate)[0].freq_mhz, 5800);
    }

    #[test]
    fn a_timer_is_asked_for_its_channels_once_and_then_not_again_for_a_while() {
        let directory = TimerDirectory::new(None);
        let gate = directory
            .create(&request("Gate", "10.0.0.5:7270"))
            .unwrap()
            .id;
        let start = Instant::now();
        assert!(
            directory.should_ask_channels(&gate, start),
            "never asked: ask"
        );
        // While that asker is at it, nobody else goes to the timer.
        for _ in 0..50 {
            assert!(!directory.should_ask_channels(&gate, start));
        }
        directory.note_channels(&gate, &settings(&[Some(5658)]), start);
        // Fresh: not asked again, right up to the limit.
        assert!(!directory.should_ask_channels(&gate, start));
        let nearly = start + CHANNELS_MAX_AGE - Duration::from_millis(1);
        assert!(!directory.should_ask_channels(&gate, nearly));
        // Old: one asker goes, and only one.
        let later = start + CHANNELS_MAX_AGE;
        assert!(directory.should_ask_channels(&gate, later));
        assert!(!directory.should_ask_channels(&gate, later));
    }

    #[test]
    fn a_timer_that_does_not_answer_keeps_its_last_channels_and_is_left_alone_for_a_while() {
        let directory = TimerDirectory::new(None);
        let gate = directory
            .create(&request("Gate", "10.0.0.5:7270"))
            .unwrap()
            .id;
        let start = Instant::now();
        directory.note_channels(&gate, &settings(&[Some(5658)]), start);
        let later = start + CHANNELS_MAX_AGE;
        assert!(directory.should_ask_channels(&gate, later));
        directory.channels_not_read(&gate, later);
        assert_eq!(
            directory.known_channels(&gate)[0].freq_mhz,
            5658,
            "the old answer stands"
        );
        assert!(
            !directory.should_ask_channels(&gate, later),
            "not asked again at once"
        );
        assert!(directory.should_ask_channels(&gate, later + CHANNELS_MAX_AGE));
        // A timer that has never answered: nothing known, and the same patience.
        let dead = directory
            .create(&request("Dead", "10.0.0.6:7270"))
            .unwrap()
            .id;
        assert!(directory.should_ask_channels(&dead, start));
        directory.channels_not_read(&dead, start);
        assert!(directory.known_channels(&dead).is_empty());
        assert!(!directory.should_ask_channels(&dead, start));
    }

    #[test]
    fn channels_are_per_timer_and_go_when_the_timer_does() {
        let directory = TimerDirectory::new(None);
        let gate = directory
            .create(&request("Gate", "10.0.0.5:7270"))
            .unwrap()
            .id;
        let other = directory
            .create(&request("Other", "10.0.0.6:7270"))
            .unwrap()
            .id;
        let now = Instant::now();
        directory.note_channels(&gate, &settings(&[Some(5658)]), now);
        assert!(directory.known_channels(&other).is_empty());
        assert!(
            directory.should_ask_channels(&other, now),
            "the other timer is its own"
        );
        directory.delete(&gate).unwrap();
        assert!(directory.known_channels(&gate).is_empty());
        // A timer that is not in the directory is never asked and never remembered.
        assert!(!directory.should_ask_channels(&gate, now));
        directory.note_channels(&gate, &settings(&[Some(5658)]), now);
        assert!(directory.known_channels(&gate).is_empty());
        let unknown = AdapterId("no-such-timer".into());
        assert!(!directory.should_ask_channels(&unknown, now));
        assert!(directory.known_channels(&unknown).is_empty());
        directory.channels_not_read(&unknown, now);
        directory.age_channels(&unknown);
    }

    #[test]
    fn channels_made_old_are_asked_for_again() {
        let directory = TimerDirectory::new(None);
        let gate = directory
            .create(&request("Gate", "10.0.0.5:7270"))
            .unwrap()
            .id;
        let now = Instant::now();
        directory.note_channels(&gate, &settings(&[Some(5658)]), now);
        assert!(!directory.should_ask_channels(&gate, now));
        directory.age_channels(&gate);
        assert!(directory.should_ask_channels(&gate, now));
    }

    #[test]
    fn a_timer_is_renamed_and_keeps_its_handle_address_and_own_id() {
        let dir = std::env::temp_dir().join(format!(
            "gridfpv-timers-rename-{}-{}",
            std::process::id(),
            tail()
        ));
        let directory = TimerDirectory::new(Some(dir.clone()));
        let timer = directory
            .create(&CreateTimerRequest {
                name: "oft-3fa2".into(),
                kind: TimerKind::Oft,
                address: "10.0.0.5:7270".into(),
                own_id: Some("oft-3fa2".into()),
            })
            .unwrap();
        let watch = directory.watch();
        let before = *watch.borrow();
        let renamed = directory.rename(&timer.id, "  Start gate ").unwrap();
        assert_eq!(renamed.name, "Start gate");
        // Only the name moved. The handle passes carry is the same, so nothing already
        // timed loses its timer; the name the timer gives itself is kept beside it.
        assert_eq!(
            Timer {
                name: timer.name.clone(),
                ..renamed.clone()
            },
            timer
        );
        assert_eq!(renamed.own_id.as_deref(), Some("oft-3fa2"));
        assert!(*watch.borrow() > before);
        // It is kept across a restart.
        let again = TimerDirectory::new(Some(dir.clone()));
        assert_eq!(again.records(), vec![renamed]);
        // A blank name and an unknown timer are refused, and nothing changes.
        assert_eq!(directory.rename(&timer.id, "  "), Err(TimerError::NoName));
        assert_eq!(
            directory.rename(&AdapterId("nope".into()), "Gate"),
            Err(TimerError::NotFound)
        );
        assert_eq!(directory.records()[0].name, "Start gate");
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

    /// A fake OFT timer that takes every write and counts how often it is asked to keep its
    /// settings. `keeps` is whether the settings in use are then the kept ones.
    async fn fake_keeper(keeps: bool) -> (String, Arc<std::sync::atomic::AtomicU32>) {
        use std::sync::atomic::{AtomicU32, Ordering};
        let stored = Arc::new(Mutex::new(serde_json::json!({
            "seats": [{"seat": 0, "freq_mhz": 5800, "enabled": true}],
            "threshold": {"mode": "auto", "snr_enter": 150, "hysteresis": 50},
            "min_lap_ms": 2000,
            "exit_hold_ms": 30,
        })));
        let asked = Arc::new(AtomicU32::new(0));
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
                *stored.lock().unwrap() = body.clone();
                Json(body)
            }
        };
        let keep = {
            let asked = asked.clone();
            move || {
                let asked = asked.clone();
                async move {
                    asked.fetch_add(1, Ordering::SeqCst);
                    Json(serde_json::json!({"saved": keeps, "current_is_saved": keeps}))
                }
            }
        };
        let app = axum::Router::new()
            .route("/oft/v1/config", axum::routing::get(read).put(write))
            .route("/oft/v1/config/save", axum::routing::post(keep));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (addr.to_string(), asked)
    }

    /// A registry with one timer at a [`fake_keeper`], whose status says whether it can keep
    /// settings.
    async fn with_keeper(
        can_keep: bool,
        keeps: bool,
    ) -> (EventRegistry, String, Arc<std::sync::atomic::AtomicU32>) {
        let registry = EventRegistry::new(None).unwrap();
        let (address, asked) = fake_keeper(keeps).await;
        let timer = registry
            .timers()
            .create(&request("Gate", &address))
            .unwrap();
        registry
            .timers()
            .report(&timer.id, |status| status.keeps_settings = can_keep);
        (registry, format!("/timers/{}/config", timer.id.0), asked)
    }

    /// A fake OFT timer that has a pass window. Gives its `host:port`, what it holds, and
    /// how many times it was written to. `takes` says whether a write lands.
    async fn fake_window_timer(
        window_ms: u32,
        takes: bool,
    ) -> (
        String,
        Arc<Mutex<serde_json::Value>>,
        Arc<std::sync::atomic::AtomicU32>,
    ) {
        use std::sync::atomic::Ordering;
        let stored = Arc::new(Mutex::new(serde_json::json!({
            "seats": [{"seat": 0, "freq_mhz": 5800, "enabled": true,
                       "threshold": {"mode": "manual", "enter": -700, "exit": -760}}],
            "threshold": {"mode": "auto", "snr_enter": 150, "hysteresis": 50},
            "min_lap_ms": 2000,
            "exit_hold_ms": 80,
            "pass_window_ms": window_ms,
        })));
        let writes = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let read = {
            let stored = stored.clone();
            move || {
                let stored = stored.clone();
                async move { Json(stored.lock().unwrap().clone()) }
            }
        };
        let write = {
            let stored = stored.clone();
            let writes = writes.clone();
            move |Json(body): Json<serde_json::Value>| {
                let stored = stored.clone();
                let writes = writes.clone();
                async move {
                    writes.fetch_add(1, Ordering::SeqCst);
                    if takes {
                        *stored.lock().unwrap() = body.clone();
                    }
                    Json(body)
                }
            }
        };
        let app = axum::Router::new().route("/oft/v1/config", axum::routing::get(read).put(write));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (addr.to_string(), stored, writes)
    }

    /// A registry with one connected timer at `address` that says it has a pass window.
    fn with_window_timer(address: &str, has_window: bool) -> (EventRegistry, AdapterId) {
        let registry = EventRegistry::new(None).unwrap();
        let timer = registry.timers().create(&request("Gate", address)).unwrap();
        registry.timers().report(&timer.id, |status| {
            status.connected = true;
            status.pass_window = has_window;
            status.seat_thresholds = true;
        });
        (registry, timer.id)
    }

    fn status_of(registry: &EventRegistry, timer: &AdapterId) -> TimerStatus {
        registry
            .timers()
            .list()
            .into_iter()
            .find(|view| &view.timer.id == timer)
            .unwrap()
            .status
    }

    #[tokio::test]
    async fn the_events_pass_window_is_put_on_the_timer_and_nothing_else_is_touched() {
        use std::sync::atomic::Ordering;
        let (address, stored, writes) = fake_window_timer(0, true).await;
        let (registry, timer) = with_window_timer(&address, true);
        let before = stored.lock().unwrap().clone();

        sync_pass_window(&registry, 400).await;
        let after = stored.lock().unwrap().clone();
        assert_eq!(after["pass_window_ms"], 400);
        // Every other setting is exactly as it was: the seat's own levels, the exit hold.
        let mut expected = before.clone();
        expected["pass_window_ms"] = serde_json::json!(400);
        assert_eq!(after, expected);
        assert_eq!(writes.load(Ordering::SeqCst), 1);
        let status = status_of(&registry, &timer);
        assert_eq!(status.pass_window_ms, Some(400));
        assert_eq!(status.pass_window_problem, None);

        // Already in step: it is read, and not written again.
        sync_pass_window(&registry, 400).await;
        assert_eq!(writes.load(Ordering::SeqCst), 1);
        // Off is a value like any other.
        sync_pass_window(&registry, 0).await;
        assert_eq!(stored.lock().unwrap()["pass_window_ms"], 0);
        assert_eq!(status_of(&registry, &timer).pass_window_ms, Some(0));
        assert_eq!(writes.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_timer_that_does_not_take_the_window_says_so_and_keeps_what_it_had() {
        let (address, stored, _) = fake_window_timer(0, false).await;
        let (registry, timer) = with_window_timer(&address, true);
        sync_pass_window(&registry, 400).await;
        assert_eq!(stored.lock().unwrap()["pass_window_ms"], 0);
        let status = status_of(&registry, &timer);
        assert!(status.pass_window_problem.is_some(), "{status:?}");
        assert_eq!(status.pass_window_ms, None, "what it has was not confirmed");
        // The next time it is in step, the problem is gone.
        sync_pass_window(&registry, 0).await;
        let status = status_of(&registry, &timer);
        assert_eq!(status.pass_window_problem, None);
        assert_eq!(status.pass_window_ms, Some(0));
    }

    #[tokio::test]
    async fn a_timer_with_no_pass_window_or_not_connected_is_left_alone() {
        use std::sync::atomic::Ordering;
        let (address, stored, writes) = fake_window_timer(0, true).await;
        // It does not say it has one.
        let (registry, timer) = with_window_timer(&address, false);
        sync_pass_window(&registry, 400).await;
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        assert_eq!(stored.lock().unwrap()["pass_window_ms"], 0);
        assert_eq!(status_of(&registry, &timer).pass_window_ms, None);
        // It has one, and is not connected.
        registry.timers().report(&timer, |status| {
            status.pass_window = true;
            status.connected = false;
        });
        sync_pass_window(&registry, 400).await;
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        // A timer that cannot be reached at all gets the reason, and nothing hangs.
        let (gone, lost) = with_window_timer("127.0.0.1:9", true);
        sync_pass_window(&gone, 400).await;
        assert!(status_of(&gone, &lost).pass_window_problem.is_some());
    }

    #[tokio::test]
    async fn applying_seat_settings_keeps_the_pass_window_the_timer_has() {
        // The Seats page does not know the pass window: it is the event's. A settings write
        // from it must carry over what the timer has, or the timer would switch it off.
        let (address, stored, _) = fake_window_timer(400, true).await;
        let (registry, timer) = with_window_timer(&address, true);
        let uri = format!("/timers/{}/config", timer.0);
        let (status, written) = call(&registry, "PUT", &uri, Some(changed())).await;
        assert_eq!(status, StatusCode::OK, "{written}");
        let now = stored.lock().unwrap().clone();
        assert_eq!(now["pass_window_ms"], 400);
        assert_eq!(now["seats"][0]["freq_mhz"], 5658);
        // And it is not one of the settings the Seats page is told or can send.
        assert!(written.get("pass_window_ms").is_none(), "{written}");
    }

    #[tokio::test]
    async fn the_open_events_pass_window_is_read_in_milliseconds_and_is_zero_with_none() {
        use crate::events::CreateEventRequest;
        let registry = EventRegistry::new(None).unwrap();
        assert_eq!(open_events_pass_window(&registry), 0, "no event is open");
        let event = registry
            .create(&CreateEventRequest::named("Night"))
            .unwrap();
        registry.set_active(&event.id).unwrap();
        assert_eq!(open_events_pass_window(&registry), 0, "it sets none");
        crate::control::with_event(&registry, &event.id, |session| {
            session.commands(vec![gridfpv_events::Command::Record(Box::new(
                gridfpv_events::Event::LapRulesSet {
                    rules: gridfpv_events::LapRules {
                        min_lap_micros: None,
                        ignore_before_micros: None,
                        pass_window_micros: Some(1_500_000),
                    },
                },
            ))])
        })
        .unwrap();
        assert_eq!(open_events_pass_window(&registry), 1500);
        // Another event that is not open does not count.
        let other = registry
            .create(&CreateEventRequest::named("Other"))
            .unwrap();
        registry.set_active(&other.id).unwrap();
        assert_eq!(open_events_pass_window(&registry), 0);
    }

    #[tokio::test]
    async fn the_lap_cooldown_is_written_and_a_write_without_it_keeps_the_timers() {
        let (registry, uri) = with_timer(Fake::Honest).await;
        let mut body = changed();
        body["lap_cooldown_ms"] = serde_json::json!(1500);
        let (status, written) = call(&registry, "PUT", &uri, Some(body.clone())).await;
        assert_eq!(status, StatusCode::OK, "{written}");
        assert_eq!(written, body);
        // Left out of a write: the timer keeps the one it has.
        let mut without = changed();
        without.as_object_mut().unwrap().remove("lap_cooldown_ms");
        let (status, written) = call(&registry, "PUT", &uri, Some(without)).await;
        assert_eq!(status, StatusCode::OK, "{written}");
        assert_eq!(written["lap_cooldown_ms"], serde_json::json!(1500));
    }

    #[tokio::test]
    async fn a_profile_is_saved_from_the_timer_and_applying_it_writes_it_back() {
        let (registry, uri) = with_timer(Fake::Honest).await;
        let profiles = uri.replace("/config", "/profiles");
        // No profile yet.
        let (status, none) = call(&registry, "GET", &profiles, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(none, serde_json::json!({"profiles": []}));
        // Save what the timer has now. Saving writes nothing to the timer.
        let (_, at_first) = call(&registry, "GET", &uri, None).await;
        let body = serde_json::json!({"name": " Garage ", "config": at_first});
        let (status, saved) = call(&registry, "POST", &profiles, Some(body.clone())).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert_eq!(saved["name"], "Garage");
        let id = saved["id"].as_str().unwrap().to_owned();
        // The same name again is refused, in words.
        let (status, taken) = call(&registry, "POST", &profiles, Some(body)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            taken["message"].as_str().unwrap().contains("Garage"),
            "{taken}"
        );
        // Change the timer, then apply the profile: the timer is back as it was.
        let (status, _) = call(&registry, "PUT", &uri, Some(changed())).await;
        assert_eq!(status, StatusCode::OK);
        let apply = format!("{profiles}/{id}/apply");
        let (status, applied) = call(&registry, "POST", &apply, None).await;
        assert_eq!(status, StatusCode::OK, "{applied}");
        assert_eq!(applied, at_first);
        let (_, on_timer) = call(&registry, "GET", &uri, None).await;
        assert_eq!(on_timer, at_first);
        let (_, listed) = call(&registry, "GET", &profiles, None).await;
        assert_eq!(listed["active"], serde_json::json!(id));
        // An unknown profile and an unknown timer are both not found.
        let (status, _) = call(&registry, "POST", &format!("{profiles}/nope/apply"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(&registry, "GET", "/timers/nope/profiles", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        // Removing the timer forgets its profiles.
        let timer = registry.timers().records()[0].id.clone();
        let (status, _) = call(&registry, "DELETE", &format!("/timers/{}", timer.0), None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(registry.library().profiles(&timer).profiles.is_empty());
    }

    #[tokio::test]
    async fn a_profile_saved_in_one_unit_is_not_applied_once_the_timer_measures_in_another() {
        let (registry, uri) = with_timer(Fake::Honest).await;
        let timer = registry.timers().records()[0].id.clone();
        let profiles = uri.replace("/config", "/profiles");
        // Saved while the timer's receiver reported tenths of a dB.
        registry.timers().report(&timer, |status| {
            status.level_unit = Some(SignalUnit::TenthsDb);
        });
        let (_, at_first) = call(&registry, "GET", &uri, None).await;
        let body = serde_json::json!({"name": "Garage", "config": at_first});
        let (_, saved) = call(&registry, "POST", &profiles, Some(body)).await;
        assert_eq!(saved["level_unit"], "TenthsDb", "{saved}");
        let apply = format!("{profiles}/{}/apply", saved["id"].as_str().unwrap());
        // The receiver comes back measuring millivolts. The profile's 150 would now mean
        // 150 mV, not 15 dB: it is refused, in words, and the timer is not written to.
        registry.timers().report(&timer, |status| {
            status.level_unit = Some(SignalUnit::Millivolts);
        });
        let (status, _) = call(&registry, "PUT", &uri, Some(changed())).await;
        assert_eq!(status, StatusCode::OK);
        let (_, before) = call(&registry, "GET", &uri, None).await;
        let (status, refused) = call(&registry, "POST", &apply, None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        let message = refused["message"].as_str().unwrap();
        assert!(
            message.contains("dB") && message.contains("millivolts"),
            "{message}"
        );
        let (_, after) = call(&registry, "GET", &uri, None).await;
        assert_eq!(after, before);
        // Back in dB, it applies.
        registry.timers().report(&timer, |status| {
            status.level_unit = Some(SignalUnit::TenthsDb);
        });
        let (status, applied) = call(&registry, "POST", &apply, None).await;
        assert_eq!(status, StatusCode::OK, "{applied}");
        assert_eq!(applied, at_first);
    }

    #[tokio::test]
    async fn a_profile_the_timer_did_not_take_is_refused_and_does_not_become_active() {
        let (registry, uri) = with_timer(Fake::Forgetful).await;
        let profiles = uri.replace("/config", "/profiles");
        // A first profile, saved from the timer, is the active one.
        let (_, at_first) = call(&registry, "GET", &uri, None).await;
        let body = serde_json::json!({"name": "Garage", "config": at_first});
        let (_, garage) = call(&registry, "POST", &profiles, Some(body)).await;
        // A second one holds different settings. Saving it makes it active, as saved from
        // the timer; put the first back as active to tell the two apart.
        let body = serde_json::json!({"name": "Field", "config": changed()});
        let (status, field) = call(&registry, "POST", &profiles, Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{field}");
        let timer = registry.timers().records()[0].id.clone();
        registry
            .library()
            .set_active(&timer, garage["id"].as_str().unwrap())
            .unwrap();
        // This timer answers yes and changes nothing, so the readback does not match.
        let apply = format!("{profiles}/{}/apply", field["id"].as_str().unwrap());
        let (status, refusal) = call(&registry, "POST", &apply, None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refusal}");
        let (_, listed) = call(&registry, "GET", &profiles, None).await;
        assert_eq!(listed["active"], garage["id"]);
    }

    #[tokio::test]
    async fn saved_formats_are_listed_added_changed_and_removed_over_http() {
        let registry = EventRegistry::new(None).unwrap();
        let (status, formats) = call(&registry, "GET", "/formats", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(formats.as_array().unwrap().len(), 4);
        let mut copy = formats[0]["format"].clone();
        // A name in use is refused with a sentence that names it.
        let (status, taken) = call(&registry, "POST", "/formats", Some(copy.clone())).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            taken["message"].as_str().unwrap().contains("Practice"),
            "{taken}"
        );
        copy["name"] = serde_json::json!("Warm up");
        let (status, saved) = call(&registry, "POST", "/formats", Some(copy.clone())).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        let uri = format!("/formats/{}", saved["id"].as_str().unwrap());
        copy["race"]["staging_micros"] = serde_json::json!(3_000_000);
        let (status, changed) = call(&registry, "PUT", &uri, Some(copy)).await;
        assert_eq!(status, StatusCode::OK, "{changed}");
        assert_eq!(
            changed["format"]["race"]["staging_micros"],
            serde_json::json!(3_000_000)
        );
        let (status, _) = call(&registry, "DELETE", &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = call(&registry, "DELETE", &uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (_, formats) = call(&registry, "GET", "/formats", None).await;
        assert_eq!(formats.as_array().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn the_exit_hold_is_read_from_the_timer_and_written_back() {
        let (registry, uri, _) = with_keeper(false, false).await;
        let (status, mut config) = call(&registry, "GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(config["exit_hold_ms"], serde_json::json!(30));
        config["exit_hold_ms"] = serde_json::json!(80);
        let (status, written) = call(&registry, "PUT", &uri, Some(config.clone())).await;
        assert_eq!(status, StatusCode::OK, "{written}");
        assert_eq!(written["exit_hold_ms"], serde_json::json!(80));
    }

    #[tokio::test]
    async fn a_timer_that_can_keep_its_settings_is_asked_to_after_a_change() {
        use std::sync::atomic::Ordering;
        let (registry, uri, asked) = with_keeper(true, true).await;
        let (_, config) = call(&registry, "GET", &uri, None).await;
        let (status, _) = call(&registry, "PUT", &uri, Some(config)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(asked.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_timer_that_cannot_keep_its_settings_is_not_asked_to() {
        use std::sync::atomic::Ordering;
        let (registry, uri, asked) = with_keeper(false, false).await;
        let (_, config) = call(&registry, "GET", &uri, None).await;
        let (status, _) = call(&registry, "PUT", &uri, Some(config)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(asked.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_change_the_timer_did_not_keep_is_refused() {
        let (registry, uri, _) = with_keeper(true, false).await;
        let (_, config) = call(&registry, "GET", &uri, None).await;
        let (status, body) = call(&registry, "PUT", &uri, Some(config)).await;
        assert_ne!(status, StatusCode::OK, "{body}");
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
            "threshold": {"Manual": {"enter": -700, "exit": -760}},
            "lap_cooldown_ms": 2000,
        })
    }

    #[tokio::test]
    async fn a_timer_is_renamed_over_http() {
        let (registry, uri) = with_timer(Fake::Honest).await;
        let timer = uri.trim_end_matches("/config").to_owned();
        let body = serde_json::json!({"name": " Start gate "});
        let (status, renamed) = call(&registry, "PUT", &timer, Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{renamed}");
        assert_eq!(renamed["name"], "Start gate");
        let (_, listed) = call(&registry, "GET", "/timers", None).await;
        assert_eq!(listed[0]["timer"]["name"], "Start gate");
        // A blank name is refused in words; an unknown timer is not found.
        let blank = serde_json::json!({"name": "  "});
        let (status, said) = call(&registry, "PUT", &timer, Some(blank)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(said["message"], "A timer needs a name.");
        let body = serde_json::json!({"name": "Gate"});
        let (status, _) = call(&registry, "PUT", "/timers/nope", Some(body)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
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
            unit: SignalUnit::TenthsDb,
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

    #[tokio::test]
    async fn a_threshold_for_one_seat_is_only_sent_to_a_timer_that_takes_them() {
        let (registry, uri) = with_timer(Fake::Honest).await;
        let timer = registry.timers().records()[0].id.clone();
        let mut body = changed();
        body["seats"][1]["threshold"] =
            serde_json::json!({"Manual": {"enter": -650, "exit": -700}});

        // The timer has not said it takes them: refused here, in plain words, not sent.
        let (status, answer) = call(&registry, "PUT", &uri, Some(body.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            answer["message"],
            "This timer has one threshold for all its seats. It cannot take one for a single seat."
        );
        let (_, now) = call(&registry, "GET", &uri, None).await;
        assert_eq!(now["seats"][0]["freq_mhz"], 5800);

        // Once its connection reports that it does, the seat's threshold goes through and
        // reads back; a seat without one has no key at all.
        registry
            .timers()
            .report(&timer, |status| status.seat_thresholds = true);
        let (status, answer) = call(&registry, "PUT", &uri, Some(body.clone())).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(answer, body);
        assert!(answer["seats"][0].get("threshold").is_none());
    }

    #[test]
    fn a_timer_found_at_a_new_address_under_its_own_id_is_the_same_timer() {
        let directory = TimerDirectory::new(None);
        let announced = |id: Option<&str>, address: &str| DiscoveredTimer {
            name: "oft-a867".into(),
            id: id.map(str::to_owned),
            address: address.into(),
            seats: Some(1),
            added: false,
        };
        let gate = directory
            .create(&request("Gate", "192.168.36.3:7270"))
            .unwrap();
        let mut watch = directory.watch();
        watch.mark_unchanged();

        // Its own id is not known yet (it has never connected), so the timer announced at
        // another address is not taken for it: nothing is guessed from a name.
        let key = "oft-a867._oft._tcp.local.";
        directory.found(key, announced(Some("oft-a867"), "192.168.36.23:7270"));
        assert_eq!(
            directory.get(&gate.id).unwrap().address,
            "192.168.36.3:7270"
        );
        assert!(!directory.discovered()[0].added);
        assert!(!watch.has_changed().unwrap());

        // It connects and says its id. Learning it is not a reason to reconnect.
        directory.learn_own_id(&gate.id, " oft-a867 ");
        directory.learn_own_id(&gate.id, "");
        assert_eq!(
            directory.get(&gate.id).unwrap().own_id.as_deref(),
            Some("oft-a867")
        );
        assert!(!watch.has_changed().unwrap());

        // Announced again at the new address: the record moves there, it is listed as
        // added, and whatever holds its connection is told to look again.
        directory.found(key, announced(Some("oft-a867"), "192.168.36.23:7270"));
        let moved = directory.get(&gate.id).unwrap();
        assert_eq!(moved.address, "192.168.36.23:7270");
        assert_eq!((moved.id, moved.name), (gate.id.clone(), gate.name));
        assert!(directory.discovered()[0].added);
        assert!(watch.has_changed().unwrap());
        watch.mark_unchanged();

        // The same announcement again changes nothing. Nor does one with another id, or
        // with none.
        directory.found(key, announced(Some("oft-a867"), "192.168.36.23:7270"));
        directory.found("other", announced(Some("oft-ffff"), "192.168.36.40:7270"));
        directory.found("anon", announced(None, "192.168.36.41:7270"));
        assert_eq!(
            directory.get(&gate.id).unwrap().address,
            "192.168.36.23:7270"
        );
        assert!(!watch.has_changed().unwrap());
        assert_eq!(directory.list().len(), 1);
    }

    #[test]
    fn a_timer_added_from_the_network_carries_its_own_id_from_the_start() {
        let directory = TimerDirectory::new(None);
        let mut request = request("oft-a867", "192.168.36.3:7270");
        request.own_id = Some("oft-a867".into());
        let gate = directory.create(&request).unwrap();
        assert_eq!(gate.own_id.as_deref(), Some("oft-a867"));
        // A record saved before timers had ids of their own still loads.
        let old: Timer = serde_json::from_str(
            r#"{"id":"gate-1","name":"Gate","kind":"Oft","address":"10.0.0.5:7270"}"#,
        )
        .unwrap();
        assert_eq!(old.own_id, None);
    }

    #[test]
    fn timers_found_on_the_network_are_listed_and_marked_once_added() {
        let directory = TimerDirectory::new(None);
        let found = |name: &str, address: &str| DiscoveredTimer {
            name: name.into(),
            id: None,
            address: address.into(),
            seats: Some(1),
            added: false,
        };
        directory.found("b._oft._tcp.local.", found("Gate B", "10.0.0.6:7270"));
        directory.found("a._oft._tcp.local.", found("Gate A", "10.0.0.5:7270"));
        let names: Vec<String> = directory.discovered().into_iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["Gate A", "Gate B"]);
        assert!(directory.discovered().iter().all(|t| !t.added));
        // Finding a timer adds nothing.
        assert!(directory.list().is_empty());

        directory.create(&request("Gate", "10.0.0.5:7270")).unwrap();
        let added: Vec<bool> = directory
            .discovered()
            .into_iter()
            .map(|t| t.added)
            .collect();
        assert_eq!(added, vec![true, false]);

        // The same name found again replaces what was known; one that leaves is dropped.
        directory.found("b._oft._tcp.local.", found("Gate B", "10.0.0.9:7270"));
        assert_eq!(directory.discovered()[1].address, "10.0.0.9:7270");
        directory.lost("a._oft._tcp.local.");
        assert_eq!(directory.discovered().len(), 1);
    }

    // ── Control: one writer at a time (OFT spec v0.9, §6.6) ────────────────

    /// What a [`fake_guarded`] timer holds, and what it was asked.
    #[derive(Clone)]
    struct Guarded {
        stored: Arc<Mutex<serde_json::Value>>,
        /// The key of the claim that holds control. `None`: nobody does.
        holder_key: Arc<Mutex<Option<String>>>,
        /// Writes that got past the guard: settings, then "keep these".
        writes: Arc<std::sync::atomic::AtomicU32>,
        keeps: Arc<std::sync::atomic::AtomicU32>,
        /// Writes the guard refused.
        refused: Arc<std::sync::atomic::AtomicU32>,
        /// The `OFT-Control` header of every write, as it arrived.
        keys_seen: Arc<Mutex<Vec<Option<String>>>>,
    }

    /// A fake OFT timer with control, as `oftd` does it: while a claim holds control, a
    /// write without that claim's key is a `423` naming the holder, and changes nothing.
    async fn fake_guarded(holder_key: Option<&str>) -> (String, Guarded) {
        use axum::http::HeaderMap;
        use std::sync::atomic::Ordering;
        let fake = Guarded {
            stored: Arc::new(Mutex::new(serde_json::json!({
                "seats": [{"seat": 0, "freq_mhz": 5800, "enabled": true}],
                "threshold": {"mode": "auto", "snr_enter": 150, "hysteresis": 50},
                "min_lap_ms": 2000,
                "exit_hold_ms": 30,
                "pass_window_ms": 0,
            }))),
            holder_key: Arc::new(Mutex::new(holder_key.map(str::to_owned))),
            writes: Arc::default(),
            keeps: Arc::default(),
            refused: Arc::default(),
            keys_seen: Arc::default(),
        };
        // `Err` is the refusal a write without the holder's key gets.
        fn guard(
            fake: &Guarded,
            headers: &HeaderMap,
        ) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
            let sent = headers
                .get("OFT-Control")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            fake.keys_seen.lock().unwrap().push(sent.clone());
            let holder = fake.holder_key.lock().unwrap().clone();
            if holder.is_some() && sent != holder {
                fake.refused.fetch_add(1, Ordering::SeqCst);
                return Err((
                    StatusCode::LOCKED,
                    Json(serde_json::json!({"error": "controlled by RotorHazard pi-timer"})),
                ));
            }
            Ok(())
        }
        let read = {
            let fake = fake.clone();
            move || {
                let fake = fake.clone();
                async move { Json(fake.stored.lock().unwrap().clone()) }
            }
        };
        let write = {
            let fake = fake.clone();
            move |headers: HeaderMap, Json(body): Json<serde_json::Value>| {
                let fake = fake.clone();
                async move {
                    guard(&fake, &headers)?;
                    fake.writes.fetch_add(1, Ordering::SeqCst);
                    *fake.stored.lock().unwrap() = body.clone();
                    Ok::<_, (StatusCode, Json<serde_json::Value>)>(Json(body))
                }
            }
        };
        let keep = {
            let fake = fake.clone();
            move |headers: HeaderMap| {
                let fake = fake.clone();
                async move {
                    guard(&fake, &headers)?;
                    fake.keeps.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, (StatusCode, Json<serde_json::Value>)>(Json(
                        serde_json::json!({"saved": true, "current_is_saved": true}),
                    ))
                }
            }
        };
        let app = axum::Router::new()
            .route("/oft/v1/config", axum::routing::get(read).put(write))
            .route("/oft/v1/config/save", axum::routing::post(keep));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (addr.to_string(), fake)
    }

    /// A registry with one connected timer at a [`fake_guarded`] that keeps settings and
    /// has a pass window.
    async fn with_guarded(holder_key: Option<&str>) -> (EventRegistry, AdapterId, String, Guarded) {
        let (address, fake) = fake_guarded(holder_key).await;
        let registry = EventRegistry::new(None).unwrap();
        let timer = registry
            .timers()
            .create(&request("Gate", &address))
            .unwrap();
        registry.timers().report(&timer.id, |status| {
            status.connected = true;
            status.keeps_settings = true;
            status.pass_window = true;
        });
        let uri = format!("/timers/{}/config", timer.id.0);
        (registry, timer.id, uri, fake)
    }

    fn other_holds(registry: &EventRegistry, timer: &AdapterId, name: &str) {
        registry.timers().report(timer, |status| {
            status.control = Some(TimerControl::Other {
                name: name.to_owned(),
            });
        });
    }

    #[tokio::test]
    async fn with_control_every_write_carries_grids_key() {
        use std::sync::atomic::Ordering;
        let (registry, timer, uri, fake) = with_guarded(Some("9f3a61c2")).await;
        registry.timers().report(&timer, |status| {
            status.control = Some(TimerControl::Grid);
        });
        registry
            .timers()
            .set_control_key(&timer, Some("9f3a61c2".into()));

        let (status, answer) = call(&registry, "PUT", &uri, Some(changed())).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(answer["seats"][1]["freq_mhz"], 5917);
        assert_eq!(fake.writes.load(Ordering::SeqCst), 1);
        // Keeping the settings is a write too, and carried the key.
        assert_eq!(fake.keeps.load(Ordering::SeqCst), 1);
        assert_eq!(fake.refused.load(Ordering::SeqCst), 0);
        assert_eq!(
            *fake.keys_seen.lock().unwrap(),
            vec![Some("9f3a61c2".to_owned()), Some("9f3a61c2".to_owned())]
        );
        // So does the event's pass window.
        sync_pass_window(&registry, 400).await;
        assert_eq!(fake.stored.lock().unwrap()["pass_window_ms"], 400);
        assert_eq!(status_of(&registry, &timer).pass_window_problem, None);
        assert_eq!(fake.refused.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn with_nobody_in_control_a_write_needs_no_key_and_a_key_does_no_harm() {
        use std::sync::atomic::Ordering;
        let (registry, timer, uri, fake) = with_guarded(None).await;
        registry.timers().report(&timer, |status| {
            status.control = Some(TimerControl::Nobody);
        });
        let (status, answer) = call(&registry, "PUT", &uri, Some(changed())).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(fake.keys_seen.lock().unwrap()[0], None);
        // A key left from before (the timer's page freed control) is sent and accepted.
        registry
            .timers()
            .set_control_key(&timer, Some("0ld".into()));
        let (status, answer) = call(&registry, "PUT", &uri, Some(changed())).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(fake.writes.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_timer_another_program_controls_is_not_written_to_and_the_answer_names_it() {
        use std::sync::atomic::Ordering;
        let (registry, timer, uri, fake) = with_guarded(Some("theirs")).await;
        other_holds(&registry, &timer, "RotorHazard pi-timer");
        let before = fake.stored.lock().unwrap().clone();

        let (status, said) = call(&registry, "PUT", &uri, Some(changed())).await;
        assert_eq!(status, StatusCode::CONFLICT, "{said}");
        assert_eq!(said["code"], "Refused");
        let message = said["message"].as_str().unwrap();
        assert!(message.contains("RotorHazard pi-timer"), "{message}");
        assert!(message.contains("Gate"), "{message}");
        assert!(message.contains("timer's own page"), "{message}");
        // Nothing was sent: Grid knew, and did not knock.
        assert!(fake.keys_seen.lock().unwrap().is_empty());
        assert_eq!(*fake.stored.lock().unwrap(), before);

        // Even a change that is wrong in itself is answered with the real reason.
        let mut bad = changed();
        bad["seats"][0]["seat"] = serde_json::json!("not-a-seat");
        let (status, said) = call(&registry, "PUT", &uri, Some(bad)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{said}");
        assert!(said["message"].as_str().unwrap().contains("control"));

        // Reading is open to everyone, whoever has control.
        let (status, read) = call(&registry, "GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK, "{read}");
        assert_eq!(read["seats"][0]["freq_mhz"], 5800);
        assert_eq!(fake.writes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_423_from_the_timer_is_passed_on_naming_the_holder() {
        use std::sync::atomic::Ordering;
        // Grid has not heard yet that another program took control (or its key is old).
        for (control, key) in [
            (None, None),
            (Some(TimerControl::Nobody), None),
            (Some(TimerControl::Grid), Some("stale")),
        ] {
            let (registry, timer, uri, fake) = with_guarded(Some("theirs")).await;
            registry
                .timers()
                .report(&timer, |status| status.control = control.clone());
            registry
                .timers()
                .set_control_key(&timer, key.map(str::to_owned));
            let before = fake.stored.lock().unwrap().clone();
            let (status, said) = call(&registry, "PUT", &uri, Some(changed())).await;
            assert_eq!(status, StatusCode::CONFLICT, "{said}");
            let message = said["message"].as_str().unwrap();
            assert!(
                message.contains("\u{201c}RotorHazard pi-timer\u{201d} has control of Gate"),
                "{message}"
            );
            assert_eq!(fake.refused.load(Ordering::SeqCst), 1);
            assert_eq!(fake.keeps.load(Ordering::SeqCst), 0);
            assert_eq!(*fake.stored.lock().unwrap(), before);
        }
    }

    #[tokio::test]
    async fn a_refused_keep_is_not_reported_as_kept() {
        use std::sync::atomic::Ordering;
        // Control moves between the write and the "keep these".
        let (registry, timer, _, fake) = with_guarded(None).await;
        let timer_record = registry.timers().get(&timer).unwrap();
        *fake.holder_key.lock().unwrap() = Some("theirs".into());
        let kept = keep_config(&registry.timers(), &timer_record).await;
        let Err(TimerError::Controlled(message)) = kept else {
            panic!("expected a refusal, got {kept:?}");
        };
        assert!(message.contains("RotorHazard pi-timer"), "{message}");
        assert_eq!(fake.keeps.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn the_pass_window_on_a_timer_another_program_controls() {
        use std::sync::atomic::Ordering;
        let (registry, timer, _, fake) = with_guarded(Some("theirs")).await;
        other_holds(&registry, &timer, "RotorHazard pi-timer");

        // Already what the event wants: nothing to write, nothing to complain about.
        sync_pass_window(&registry, 0).await;
        let status = status_of(&registry, &timer);
        assert_eq!(status.pass_window_ms, Some(0));
        assert_eq!(status.pass_window_problem, None);

        // Different: Grid cannot put it right, and says why on the timer's status.
        sync_pass_window(&registry, 400).await;
        let status = status_of(&registry, &timer);
        let problem = status.pass_window_problem.expect("a problem is reported");
        assert!(problem.contains("RotorHazard pi-timer"), "{problem}");
        assert_eq!(fake.stored.lock().unwrap()["pass_window_ms"], 0);
        assert!(fake.keys_seen.lock().unwrap().is_empty());

        // Control comes to Grid: the same call now lands and the problem clears.
        *fake.holder_key.lock().unwrap() = Some("ours".into());
        registry.timers().report(&timer, |status| {
            status.control = Some(TimerControl::Grid);
        });
        registry
            .timers()
            .set_control_key(&timer, Some("ours".into()));
        sync_pass_window(&registry, 400).await;
        let status = status_of(&registry, &timer);
        assert_eq!(status.pass_window_ms, Some(400));
        assert_eq!(status.pass_window_problem, None);
        assert_eq!(fake.writes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_profile_is_not_applied_to_a_timer_another_program_controls() {
        let (registry, timer, _, fake) = with_guarded(Some("theirs")).await;
        // Saved while Grid could still read the timer; saving writes nothing to it.
        let profiles = format!("/timers/{}/profiles", timer.0);
        let body = serde_json::json!({"name": "Field", "config": changed()});
        let (status, saved) = call(&registry, "POST", &profiles, Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        let other = serde_json::json!({"name": "Garage", "config": changed()});
        let (status, garage) = call(&registry, "POST", &profiles, Some(other)).await;
        assert_eq!(status, StatusCode::OK, "{garage}");

        other_holds(&registry, &timer, "RotorHazard pi-timer");
        let apply = format!("{profiles}/{}/apply", saved["id"].as_str().unwrap());
        let (status, said) = call(&registry, "POST", &apply, None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{said}");
        assert!(
            said["message"]
                .as_str()
                .unwrap()
                .contains("RotorHazard pi-timer")
        );
        // The active profile is the one it was, and the timer was not touched.
        let (_, listed) = call(&registry, "GET", &profiles, None).await;
        assert_eq!(listed["active"], garage["id"]);
        assert!(fake.keys_seen.lock().unwrap().is_empty());
    }

    #[test]
    fn a_control_key_is_kept_per_timer_and_goes_with_the_timer() {
        let directory = TimerDirectory::new(None);
        let gate = directory.create(&request("Gate", "10.0.0.1:7270")).unwrap();
        let split = directory
            .create(&request("Split", "10.0.0.2:7270"))
            .unwrap();
        assert_eq!(directory.control_key(&gate.id), None);
        directory.set_control_key(&gate.id, Some("aa".into()));
        directory.set_control_key(&split.id, Some("bb".into()));
        assert_eq!(directory.control_key(&gate.id).as_deref(), Some("aa"));
        assert_eq!(directory.control_key(&split.id).as_deref(), Some("bb"));
        directory.set_control_key(&gate.id, None);
        assert_eq!(directory.control_key(&gate.id), None);
        assert_eq!(directory.control_key(&split.id).as_deref(), Some("bb"));
        directory.delete(&split.id).unwrap();
        assert_eq!(directory.control_key(&split.id), None);
        // A connection that outlives its timer cannot leave a key behind.
        directory.set_control_key(&split.id, Some("cc".into()));
        assert_eq!(directory.control_key(&split.id), None);
    }

    #[test]
    fn the_key_never_reaches_a_console_and_control_is_said_in_the_status() {
        let directory = TimerDirectory::new(None);
        let gate = directory.create(&request("Gate", "10.0.0.1:7270")).unwrap();
        directory.set_control_key(&gate.id, Some("s3cr3t".into()));
        directory.report(&gate.id, |status| {
            status.control = Some(TimerControl::Other {
                name: "RotorHazard".into(),
            });
            status.control_name = Some("GridFPV race-laptop".into());
            status.config_rev = Some(7);
        });
        let listed = serde_json::to_value(directory.list()).unwrap();
        assert!(!listed.to_string().contains("s3cr3t"), "{listed}");
        assert_eq!(
            listed[0]["status"]["control"],
            serde_json::json!({"holder": "Other", "name": "RotorHazard"})
        );
        assert_eq!(listed[0]["status"]["control_name"], "GridFPV race-laptop");
        assert_eq!(listed[0]["status"]["config_rev"], 7);
        // A timer with no control says nothing about it.
        let plain = directory
            .create(&request("Plain", "10.0.0.3:7270"))
            .unwrap();
        directory.report(&plain.id, |status| status.connected = true);
        let listed = serde_json::to_value(directory.list()).unwrap();
        let plain = &listed[1]["status"];
        assert!(plain.get("control").is_none(), "{plain}");
        assert!(plain.get("config_rev").is_none(), "{plain}");
        // A visitor is told none of it.
        let view = directory.list().remove(0);
        let visitor = serde_json::to_value(crate::spectate::timer_for_visitors(view)).unwrap();
        assert!(visitor["status"].get("control").is_none(), "{visitor}");
        assert!(visitor["status"].get("control_name").is_none(), "{visitor}");
    }

    #[tokio::test]
    async fn a_write_sent_a_moment_before_grid_hears_it_has_control_is_sent_again_with_the_key() {
        use std::sync::atomic::Ordering;
        // The timer has made Grid's claim the holder; the key is still on its way.
        for stale in [None, Some("from-before")] {
            let (registry, timer, uri, fake) = with_guarded(Some("9f3a61c2")).await;
            registry
                .timers()
                .set_control_key(&timer, stale.map(str::to_owned));
            let arrives = {
                let (registry, timer) = (registry.clone(), timer.clone());
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    registry
                        .timers()
                        .set_control_key(&timer, Some("9f3a61c2".into()));
                    registry.timers().report(&timer, |status| {
                        status.control = Some(TimerControl::Grid);
                    });
                })
            };
            let (status, answer) = call(&registry, "PUT", &uri, Some(changed())).await;
            arrives.await.unwrap();
            assert_eq!(status, StatusCode::OK, "{answer}");
            assert_eq!(fake.stored.lock().unwrap()["seats"][1]["freq_mhz"], 5917);
            // Refused once, sent again with the key, then kept with the key.
            assert_eq!(fake.refused.load(Ordering::SeqCst), 1);
            assert_eq!(
                *fake.keys_seen.lock().unwrap(),
                vec![
                    stale.map(str::to_owned),
                    Some("9f3a61c2".to_owned()),
                    Some("9f3a61c2".to_owned())
                ]
            );
        }
    }

    #[tokio::test]
    async fn a_refused_write_is_sent_again_at_most_once() {
        use std::sync::atomic::Ordering;
        // Grid is told it has control, with a key the timer does not accept (control
        // moved on again). One more try, then the refusal stands.
        let (registry, timer, uri, fake) = with_guarded(Some("theirs")).await;
        let arrives = {
            let (registry, timer) = (registry.clone(), timer.clone());
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                registry
                    .timers()
                    .set_control_key(&timer, Some("wrong".into()));
                registry.timers().report(&timer, |status| {
                    status.control = Some(TimerControl::Grid);
                });
            })
        };
        let (status, said) = call(&registry, "PUT", &uri, Some(changed())).await;
        arrives.await.unwrap();
        assert_eq!(status, StatusCode::CONFLICT, "{said}");
        assert!(
            said["message"]
                .as_str()
                .unwrap()
                .contains("RotorHazard pi-timer")
        );
        assert_eq!(fake.refused.load(Ordering::SeqCst), 2);
        assert_eq!(fake.writes.load(Ordering::SeqCst), 0);
    }
}
