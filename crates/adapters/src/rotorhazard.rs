//! RotorHazard adapter (#23).
//!
//! The first real-world target and the **full-signal case**. RotorHazard (an
//! open-source FPV race timer, <https://github.com/RotorHazard/RotorHazard>) exposes
//! its race over Socket.IO / RHAPI: lap/pass records carry a per-node peak RSSI and
//! the node's EnterAt/ExitAt calibration thresholds, the server tracks RSSI history,
//! and a race start/stop lifecycle drives sessions. Each lap pass becomes a [`Pass`]
//! with [`SignalContext`]; node seats are reported via [`Event::CompetitorSeen`];
//! race staging/finish become [`Event::SessionStarted`] / [`Event::SessionEnded`];
//! the server clock is the source clock. Ref: `docs/timer-adapters.html` §8.
//!
//! Capabilities: live passes ✓, signal ✓, calibration ✓, signal-based recovery ✓,
//! frequency mgmt ✓, source lifecycle ✓.
//!
//! # Scope of this module (#23)
//!
//! This is a **pure translator**: it turns already-decoded RotorHazard messages
//! ([`Raw`]) into canonical [`Event`]s. It deliberately does **not** open a
//! Socket.IO connection or speak the network protocol — wiring a live
//! `socketio`/`tokio` client that decodes frames into [`Raw`] and feeds
//! [`translate`](RotorHazardAdapter::translate) is **deferred** (tracked alongside the
//! dockerized-RH integration work, #25). Tests drive recorded JSON fixtures parsed
//! into [`Raw`] through `translate`, exactly as a live client eventually would.
//!
//! # Source format — what `Raw` mirrors and where it came from
//!
//! The [`Raw`] variants mirror real RotorHazard server structures, traced through the
//! upstream source (paths relative to `src/server/` in the RotorHazard repo):
//!
//! - **Lap/pass record** — [`Raw::Lap`]. RotorHazard records a pass in
//!   `RHRace.add_lap` (`RHRace.py`), building a `Crossing` dataclass with
//!   `node_index`, `lap_number`, `lap_time_stamp`, `lap_time`, `source`, `deleted`
//!   and `peak_rssi`, and triggers the `RACE_LAP_RECORDED` event with
//!   `{node_index, peak_rssi, frequency, lap, …}`. The client-facing `current_laps`
//!   socket payload (`RHUI.emit_current_laps` → `RHRace.build_laps_list`) carries the
//!   same per-lap fields **except** `peak_rssi`. [`Raw::Lap`] unions the two: the
//!   `current_laps` lap fields plus the `peak_rssi` that the server already has at the
//!   crossing. See the **assumptions** note below.
//! - **Race lifecycle** — [`Raw::RaceStatus`]. `RHUI.emit_race_status` emits a
//!   `race_status` socket message whose integer `race_status` is the `RaceStatus`
//!   enum (`RHRace.py`): `READY = 0`, `RACING = 1`, `DONE = 2`, `STAGING = 3`. We map
//!   the transition *into* `RACING` to [`Event::SessionStarted`] and *into* `DONE` to
//!   [`Event::SessionEnded`].
//! - **Seat → pilot assignment** — [`Raw::Seat`]. `RHUI.emit_heat_data` emits per-node
//!   `slots` with `node_index` and `pilot_id`; `build_laps_list` also attaches a
//!   `pilot {id, name, callsign}` per node. A seat going active is a
//!   [`Event::CompetitorSeen`].
//!
//! ## Time base and units (stated explicitly)
//!
//! RotorHazard's `lap_time_stamp` is **milliseconds since race start**
//! (`RHRace.add_lap`: `lap_time_stamp = (lap_timestamp_absolute -
//! start_time_monotonic); lap_time_stamp *= 1000`). [`SourceTime`] is microseconds,
//! so the adapter multiplies by **1000** (ms → µs). The origin is race start, which
//! is the natural session anchor for [`crate::clock::ClockAlignment::capture`].
//!
//! ## RSSI mapping
//!
//! RotorHazard's `peak_rssi` is an integer RSSI reading (filtered ADC counts, ~0–255
//! on stock hardware). It maps to [`SignalContext::rssi_peak`] as an `f32`. When a
//! lap carries no peak (e.g. a manually inserted lap), `rssi_peak` is `None`.
//!
//! ## ⚠ Assumptions to validate against dockerized RotorHazard (#25)
//!
//! The fixtures here are **synthesized from the documented/observed format**, not yet
//! captured from a live server. Validate, against a real capture / dockerized RH:
//!
//! 1. **`peak_rssi` delivery.** The stock client-facing `current_laps` payload does
//!    **not** include `peak_rssi`; the value is on the server-side `Crossing` /
//!    `RACE_LAP_RECORDED` event and in `node_data.pass_peak_rssi`. [`Raw::Lap`]
//!    assumes the live client obtains the per-pass peak (via the `pass_record` /
//!    `node_data` events, an RHAPI call, or a small server plugin). If the chosen
//!    transport can't deliver a per-pass peak, `peak_rssi` will simply be `None` and
//!    passes still translate — only [`SignalContext`] is then empty.
//! 2. **`source` strings.** `lap.source` is an enum-to-string from the interface
//!    (e.g. real-time vs. manual/recovered). The exact strings aren't pinned here.
//! 3. **Deleted laps.** RotorHazard can mark a lap `deleted` (race-winner cutoff, lap
//!    discard). We **skip** `deleted` laps (emit no `Pass`) — confirm that's the
//!    desired projection behavior vs. emitting and letting a later action void it.
//! 4. **Lifecycle granularity.** We treat `RACING`→session-start and `DONE`→
//!    session-end. `STAGING` is ignored (pre-roll). Confirm whether a race
//!    re-run/clear (`READY`) should end a session too.

use gridfpv_events::{
    AdapterId, CompetitorRef, Event, GateIndex, Pass, SessionId, SignalContext, SourceTime,
};
use serde::{Deserialize, Serialize};

use crate::{Adapter, Capabilities};

/// The integer `race_status` values RotorHazard reports on its `race_status` socket
/// message — mirrors `RaceStatus` in upstream `RHRace.py`.
mod race_status {
    /// Race is reset / idle.
    pub const READY: i64 = 0;
    /// Race is running; laps are being registered.
    pub const RACING: i64 = 1;
    /// Race finished.
    pub const DONE: i64 = 2;
    /// Staging (pre-start countdown).
    pub const STAGING: i64 = 3;
}

/// A single already-decoded RotorHazard Socket.IO message, in the shape this adapter
/// translates. Construct these from decoded JSON frames (a live client) or from
/// recorded fixtures (tests); this module never touches the network itself.
///
/// `#[serde(tag = "event")]` gives each variant a flat, fixture-friendly JSON shape
/// `{"event": "...", ...fields}`. This is the adapter's *own* envelope for replay —
/// the field names inside each variant mirror RotorHazard's real payloads (see the
/// [module docs](self)), but the `event` discriminator is ours, so a recorded session
/// is one JSON array of these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Raw {
    /// A node lap/pass record. Mirrors a `current_laps` per-lap entry
    /// (`RHRace.build_laps_list`) enriched with the crossing's `peak_rssi`
    /// (`Crossing.peak_rssi` / `RACE_LAP_RECORDED`). One per gate crossing.
    Lap(RawLap),
    /// A `race_status` message (`RHUI.emit_race_status`). Drives the session
    /// lifecycle via its integer `race_status` ([`race_status`]).
    RaceStatus(RawRaceStatus),
    /// A node seat → pilot assignment (from `emit_heat_data` slots /
    /// `build_laps_list` per-node pilot). A seat appearing is a competitor sighting.
    Seat(RawSeat),
}

/// A RotorHazard lap/pass record (see [`Raw::Lap`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawLap {
    /// Zero-based timer node index — the seat that crossed. RotorHazard's
    /// `node_index`; used as the source-local competitor handle.
    pub node_index: u32,
    /// Per-node monotonic lap counter (RotorHazard `lap_number`). `0` is the holeshot
    /// (launch-to-first-gate); subsequent laps increment. Carried through as the
    /// pass `sequence` for ordering + reconnect dedup.
    pub lap_number: u64,
    /// Crossing time in **milliseconds since race start** (RotorHazard
    /// `lap_time_stamp`). Converted to microseconds for [`SourceTime`].
    pub lap_time_stamp: i64,
    /// The lap duration in milliseconds (RotorHazard `lap_time`). Advisory only — the
    /// engine derives laps from the pass stream — so it is carried for reference but
    /// not emitted as a canonical field.
    #[serde(default)]
    pub lap_time: Option<i64>,
    /// RotorHazard's lap `source` string (real-time vs. manual/recovered). Advisory.
    #[serde(default)]
    pub source: Option<String>,
    /// Peak RSSI at the crossing (RotorHazard `peak_rssi`), if the transport delivers
    /// it. Becomes [`SignalContext::rssi_peak`]. See assumption (1) in the module docs.
    #[serde(default)]
    pub peak_rssi: Option<f32>,
    /// Whether RotorHazard has marked this lap deleted (race-winner cutoff / discard).
    /// Deleted laps are skipped (no `Pass`). See assumption (3).
    #[serde(default)]
    pub deleted: bool,
}

/// A RotorHazard `race_status` message (see [`Raw::RaceStatus`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawRaceStatus {
    /// The `RaceStatus` enum value (`READY=0, RACING=1, DONE=2, STAGING=3`).
    pub race_status: i64,
    /// RotorHazard's current heat id, used to label the session. Optional because a
    /// reset race may report none.
    #[serde(default)]
    pub race_heat_id: Option<i64>,
}

/// A RotorHazard node seat → pilot assignment (see [`Raw::Seat`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawSeat {
    /// Zero-based timer node index the seat occupies.
    pub node_index: u32,
    /// The assigned pilot's callsign, if known (RotorHazard `pilot.callsign`).
    /// Purely informational here; the competitor handle is still the node seat so it
    /// stays stable across pilot edits.
    #[serde(default)]
    pub pilot_callsign: Option<String>,
}

/// The competitor handle for a RotorHazard node seat: `"node-{index}"`. Stable across
/// pilot reassignment (the binding to a GridFPV pilot is a registration action, not
/// an adapter event — see `gridfpv_events::CompetitorRef`).
fn seat_ref(node_index: u32) -> CompetitorRef {
    CompetitorRef(format!("node-{node_index}"))
}

/// The default adapter id for a RotorHazard source.
const DEFAULT_ADAPTER_ID: &str = "rotorhazard";

/// Translates already-decoded RotorHazard messages into canonical events.
///
/// Pure translator (no IO): feed it [`Raw`] messages via
/// [`translate`](Adapter::translate). It tracks the last seen `race_status` so it only
/// emits a [`Event::SessionStarted`] / [`Event::SessionEnded`] on an actual
/// transition, and emits [`Event::CompetitorSeen`] once per node seat.
#[derive(Debug, Clone)]
pub struct RotorHazardAdapter {
    id: AdapterId,
    /// Last `race_status` value observed, so lifecycle edges fire once per transition.
    last_race_status: Option<i64>,
    /// Node seats already announced, so each seat yields one `CompetitorSeen`.
    seen_seats: std::collections::HashSet<u32>,
}

impl RotorHazardAdapter {
    /// A RotorHazard adapter with the default id (`"rotorhazard"`).
    pub fn new() -> Self {
        Self::with_id(AdapterId(DEFAULT_ADAPTER_ID.to_string()))
    }

    /// A RotorHazard adapter with an explicit id (e.g. to distinguish two timers).
    pub fn with_id(id: AdapterId) -> Self {
        Self {
            id,
            last_race_status: None,
            seen_seats: std::collections::HashSet::new(),
        }
    }

    /// The capability profile for RotorHazard — the full-signal case (see module docs).
    pub fn capabilities_for_source() -> Capabilities {
        Capabilities::none()
            .with_live_passes()
            .with_signal_context()
            .with_calibration()
            .with_signal_recovery()
            .with_frequency_mgmt()
            .with_source_lifecycle()
    }

    /// The session id RotorHazard exposes for a heat, or a generic label when none.
    fn session_id(heat_id: Option<i64>) -> SessionId {
        match heat_id {
            Some(id) => SessionId(format!("heat-{id}")),
            None => SessionId("race".to_string()),
        }
    }

    /// Translate one lap record. Skips deleted laps; otherwise emits a
    /// `CompetitorSeen` (first sighting of the seat) followed by the `Pass`.
    fn translate_lap(&mut self, lap: RawLap, out: &mut Vec<Event>) {
        if lap.deleted {
            return;
        }
        let competitor = seat_ref(lap.node_index);

        // A pass implies the seat is active; announce it once.
        if self.seen_seats.insert(lap.node_index) {
            out.push(Event::CompetitorSeen {
                adapter: self.id.clone(),
                competitor: competitor.clone(),
            });
        }

        // RotorHazard lap_time_stamp is milliseconds since race start; SourceTime is µs.
        let at = SourceTime::from_micros(lap.lap_time_stamp * 1_000);
        let signal = lap.peak_rssi.map(|rssi_peak| SignalContext {
            rssi_peak: Some(rssi_peak),
        });

        out.push(Event::Pass(Pass {
            adapter: self.id.clone(),
            competitor,
            at,
            // RotorHazard's per-node lap_number is the monotonic sequence; it
            // disambiguates passes sharing a timestamp and anchors reconnect dedup.
            sequence: Some(lap.lap_number),
            // RotorHazard reports the lap gate only (single start/finish gate).
            gate: GateIndex::LAP,
            signal,
        }));
    }

    /// Translate a `race_status` change into lifecycle edges (start/end on transition).
    fn translate_race_status(&mut self, status: RawRaceStatus, out: &mut Vec<Event>) {
        let previous = self.last_race_status;
        self.last_race_status = Some(status.race_status);

        // Only act on an actual transition into the state.
        if previous == Some(status.race_status) {
            return;
        }

        match status.race_status {
            race_status::RACING => out.push(Event::SessionStarted {
                adapter: self.id.clone(),
                session: Self::session_id(status.race_heat_id),
            }),
            race_status::DONE => out.push(Event::SessionEnded {
                adapter: self.id.clone(),
                session: Self::session_id(status.race_heat_id),
            }),
            // READY (reset) and STAGING (pre-roll) carry no canonical lifecycle edge.
            race_status::READY | race_status::STAGING => {}
            _ => {}
        }
    }

    /// Translate a seat assignment into a competitor sighting (once per seat).
    fn translate_seat(&mut self, seat: RawSeat, out: &mut Vec<Event>) {
        if self.seen_seats.insert(seat.node_index) {
            out.push(Event::CompetitorSeen {
                adapter: self.id.clone(),
                competitor: seat_ref(seat.node_index),
            });
        }
    }
}

impl Default for RotorHazardAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl Adapter for RotorHazardAdapter {
    type Raw = Raw;

    fn id(&self) -> &AdapterId {
        &self.id
    }

    fn capabilities(&self) -> Capabilities {
        Self::capabilities_for_source()
    }

    fn translate(&mut self, raw: Self::Raw) -> Vec<Event> {
        let mut out = Vec::new();
        match raw {
            Raw::Lap(lap) => self.translate_lap(lap, &mut out),
            Raw::RaceStatus(status) => self.translate_race_status(status, &mut out),
            Raw::Seat(seat) => self.translate_seat(seat, &mut out),
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small RECORDED RotorHazard session, **synthesized from the documented format**
    /// (RotorHazard `RHRace.py` / `RHUI.py`; see module docs) — validate against a real
    /// capture / dockerized RH (#25). Two seats, a staged→racing→done lifecycle, and a
    /// few laps with peak RSSI.
    const SESSION_FIXTURE: &str = include_str!("rotorhazard/fixtures/recorded-session.json");

    fn parse(json: &str) -> Vec<Raw> {
        serde_json::from_str(json).expect("fixture parses into Raw")
    }

    /// Drive every fixture message through one adapter, flattening the events.
    fn run(adapter: &mut RotorHazardAdapter, raws: Vec<Raw>) -> Vec<Event> {
        raws.into_iter()
            .flat_map(|r| adapter.translate(r))
            .collect()
    }

    #[test]
    fn capabilities_are_the_full_signal_case() {
        let adapter = RotorHazardAdapter::new();
        let caps = adapter.capabilities();
        assert!(caps.live_passes);
        assert!(caps.signal_context);
        assert!(caps.calibration);
        assert!(caps.signal_recovery);
        assert!(caps.frequency_mgmt);
        assert!(caps.source_lifecycle);
        // RotorHazard reports a single lap gate, no splits.
        assert!(!caps.gates_splits);
        assert_eq!(adapter.id().0, "rotorhazard");
    }

    #[test]
    fn recorded_session_translates_to_canonical_events() {
        let mut adapter = RotorHazardAdapter::with_id(AdapterId("rh".into()));
        let events = run(&mut adapter, parse(SESSION_FIXTURE));

        let rh = AdapterId("rh".into());
        let heat = SessionId("heat-7".into());
        let expected = vec![
            // STAGING (3) carries no lifecycle edge; first seats announce competitors.
            Event::CompetitorSeen {
                adapter: rh.clone(),
                competitor: CompetitorRef("node-0".into()),
            },
            Event::CompetitorSeen {
                adapter: rh.clone(),
                competitor: CompetitorRef("node-1".into()),
            },
            // RACING (1) starts the session.
            Event::SessionStarted {
                adapter: rh.clone(),
                session: heat.clone(),
            },
            // node-0 holeshot: ts 4200ms -> 4_200_000µs, seq 0, RSSI 142.
            Event::Pass(Pass {
                adapter: rh.clone(),
                competitor: CompetitorRef("node-0".into()),
                at: SourceTime::from_micros(4_200_000),
                sequence: Some(0),
                gate: GateIndex::LAP,
                signal: Some(SignalContext {
                    rssi_peak: Some(142.0),
                }),
            }),
            // node-1 holeshot.
            Event::Pass(Pass {
                adapter: rh.clone(),
                competitor: CompetitorRef("node-1".into()),
                at: SourceTime::from_micros(4_500_000),
                sequence: Some(0),
                gate: GateIndex::LAP,
                signal: Some(SignalContext {
                    rssi_peak: Some(138.0),
                }),
            }),
            // node-0 lap 1.
            Event::Pass(Pass {
                adapter: rh.clone(),
                competitor: CompetitorRef("node-0".into()),
                at: SourceTime::from_micros(28_350_000),
                sequence: Some(1),
                gate: GateIndex::LAP,
                signal: Some(SignalContext {
                    rssi_peak: Some(150.0),
                }),
            }),
            // node-0 lap 2 is `deleted` -> skipped (no Pass).
            // DONE (2) ends the session.
            Event::SessionEnded {
                adapter: rh,
                session: heat,
            },
        ];

        assert_eq!(events, expected);
    }

    #[test]
    fn lap_time_stamp_milliseconds_convert_to_microseconds() {
        let mut adapter = RotorHazardAdapter::new();
        let events = adapter.translate(Raw::Lap(RawLap {
            node_index: 2,
            lap_number: 5,
            lap_time_stamp: 12_345, // ms
            lap_time: None,
            source: None,
            peak_rssi: None,
            deleted: false,
        }));
        // First event is the CompetitorSeen for the freshly seen seat.
        let pass = events
            .iter()
            .find_map(|e| match e {
                Event::Pass(p) => Some(p),
                _ => None,
            })
            .expect("a pass");
        assert_eq!(pass.at, SourceTime::from_micros(12_345_000));
        assert_eq!(pass.sequence, Some(5));
        assert_eq!(pass.competitor, CompetitorRef("node-2".into()));
        // No RSSI delivered -> no signal context (still a valid pass).
        assert!(pass.signal.is_none());
    }

    #[test]
    fn peak_rssi_becomes_signal_context() {
        let mut adapter = RotorHazardAdapter::new();
        let events = adapter.translate(Raw::Lap(RawLap {
            node_index: 0,
            lap_number: 0,
            lap_time_stamp: 1_000,
            lap_time: None,
            source: None,
            peak_rssi: Some(201.5),
            deleted: false,
        }));
        let pass = events
            .iter()
            .find_map(|e| match e {
                Event::Pass(p) => Some(p),
                _ => None,
            })
            .expect("a pass");
        assert_eq!(
            pass.signal,
            Some(SignalContext {
                rssi_peak: Some(201.5)
            })
        );
    }

    #[test]
    fn deleted_laps_emit_nothing() {
        let mut adapter = RotorHazardAdapter::new();
        let events = adapter.translate(Raw::Lap(RawLap {
            node_index: 0,
            lap_number: 9,
            lap_time_stamp: 5_000,
            lap_time: None,
            source: None,
            peak_rssi: Some(100.0),
            deleted: true,
        }));
        assert!(events.is_empty(), "deleted lap yields no events");
    }

    #[test]
    fn competitor_seen_emitted_once_per_seat() {
        let mut adapter = RotorHazardAdapter::new();
        // Seat announce, then a lap on the same seat: only one CompetitorSeen total.
        let mut events = adapter.translate(Raw::Seat(RawSeat {
            node_index: 0,
            pilot_callsign: None,
        }));
        events.extend(adapter.translate(Raw::Lap(RawLap {
            node_index: 0,
            lap_number: 0,
            lap_time_stamp: 1_000,
            lap_time: None,
            source: None,
            peak_rssi: None,
            deleted: false,
        })));
        let seen = events
            .iter()
            .filter(|e| matches!(e, Event::CompetitorSeen { .. }))
            .count();
        assert_eq!(seen, 1, "seat announced exactly once");
    }

    #[test]
    fn lifecycle_edges_fire_once_per_transition() {
        let mut adapter = RotorHazardAdapter::new();
        // Repeated RACING status should only start the session once.
        let first = adapter.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::RACING,
            race_heat_id: Some(3),
        }));
        let again = adapter.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::RACING,
            race_heat_id: Some(3),
        }));
        assert_eq!(first.len(), 1);
        assert!(matches!(first[0], Event::SessionStarted { .. }));
        assert!(again.is_empty(), "duplicate RACING is not a new transition");

        // STAGING and READY carry no lifecycle edge.
        assert!(
            adapter
                .translate(Raw::RaceStatus(RawRaceStatus {
                    race_status: race_status::STAGING,
                    race_heat_id: Some(3),
                }))
                .is_empty()
        );
        assert!(
            adapter
                .translate(Raw::RaceStatus(RawRaceStatus {
                    race_status: race_status::READY,
                    race_heat_id: Some(3),
                }))
                .is_empty()
        );
    }

    #[test]
    fn raw_round_trips_through_json() {
        // The fixture envelope is stable: Raw -> JSON -> Raw is identity.
        let raws = parse(SESSION_FIXTURE);
        let json = serde_json::to_string(&raws).unwrap();
        let back: Vec<Raw> = serde_json::from_str(&json).unwrap();
        assert_eq!(raws, back);
    }
}
