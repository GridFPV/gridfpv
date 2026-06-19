//! RotorHazard adapter (#23, reworked against real captured frames #25).
//!
//! The first real-world target and the **full-signal case**. RotorHazard (an
//! open-source FPV race timer, <https://github.com/RotorHazard/RotorHazard>) exposes
//! its race over Socket.IO / RHAPI. Each lap pass becomes a [`Pass`] with
//! [`SignalContext`]; node seats are reported via [`Event::CompetitorSeen`]; race
//! staging/finish become [`Event::SessionStarted`] / [`Event::SessionEnded`]; the
//! server clock is the source clock. Ref: `docs/timer-adapters.html` §8.
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
//! # Real wire format — what `Raw` mirrors (validated #25)
//!
//! The [`Raw`] variants mirror the **real** frames captured from a live dockerized
//! RotorHazard (8 mock nodes) — see
//! `crates/adapters/src/rotorhazard/fixtures/captured-mock-race.json` and
//! `docker/rotorhazard/README.md`. The earlier `Raw::Lap` shape was a documented
//! guess; this is the corrected, capture-validated model.
//!
//! - **`race_status`** — [`Raw::RaceStatus`]. An integer `race_status`:
//!   `STAGING = 3`, `RACING = 1`, `DONE = 2`, `READY = 0`. The transition *into*
//!   `RACING` is [`Event::SessionStarted`]; *into* `DONE` is [`Event::SessionEnded`].
//! - **`current_laps`** — [`Raw::CurrentLaps`]. A **full snapshot**, re-sent on every
//!   update: `{ "current": { "node_index": [ { laps: [ … ], pilot, … }, … ] } }`.
//!   The **outer array index is the node index**. Each lap carries `lap_index`,
//!   `lap_number`, `lap_raw` (ms), `lap_time` (a `"M:SS.mmm"` *string*),
//!   `lap_time_stamp` (cumulative ms since race start, float), `splits` and
//!   `late_lap`. There is **no** `source`, **no** `deleted`, **no** per-lap
//!   `peak_rssi`, and **no** per-lap `node_index` — deleted laps are filtered out
//!   server-side before the snapshot is built. The adapter diffs each snapshot
//!   against what it has already emitted per node and emits a [`Pass`] only for the
//!   *new* laps.
//! - **`pass_record`** — [`Raw::PassRecord`]. Fires once per crossing:
//!   `{ node, frequency, timestamp }` where `timestamp` is epoch-milliseconds. This
//!   is a real-time *cross-check* signal (it confirms a crossing happened on a node);
//!   we **do not** mint a [`Pass`] from it, because its epoch timestamp is on a
//!   different clock from `lap_time_stamp` (race-relative) and `current_laps` is the
//!   authoritative, server-deduplicated lap source. See the note on `pass_record`
//!   below.
//! - **`node_data`** — [`Raw::NodeData`]. Carries per-node `pass_peak_rssi[]` (plus
//!   `node_peak_rssi[]`/nadir variants). This is the per-pass RSSI source; under the
//!   mock nodes every value is `0`. We cache `pass_peak_rssi[node]` and use it to
//!   annotate subsequent passes' [`SignalContext`].
//!
//! ## Time base and units (stated explicitly)
//!
//! `lap_time_stamp` is **milliseconds since race start** (a float;
//! `RHRace.add_lap`). [`SourceTime`] is integer microseconds, so the adapter
//! multiplies by `1000.0` and **rounds to the nearest microsecond**
//! (`(ms * 1000.0).round() as i64`) — round-half-to-even per Rust's `f64::round`
//! semantics is *not* used; `f64::round` rounds half away from zero, which is what we
//! want for a monotonic timestamp. The origin is race start, the natural session
//! anchor for [`crate::clock::ClockAlignment::capture`].
//!
//! ## RSSI mapping
//!
//! `node_data.pass_peak_rssi[node]` is an integer RSSI reading (filtered ADC counts,
//! ~0–255 on stock hardware; `0` under mock nodes). It maps to
//! [`SignalContext::rssi_peak`] as an `f32`. A pass emitted before any `node_data`
//! for its node has been seen carries no signal context (`signal = None`).
//!
//! ## On `pass_record` (cross-check only)
//!
//! `pass_record` arrives per crossing and *could* drive passes, but its `timestamp`
//! is epoch-ms (wall clock) while `current_laps.lap_time_stamp` is race-relative —
//! mixing them would corrupt interval math. `current_laps` is also the source RH
//! itself trusts (deleted laps already removed, lap numbers assigned). So we treat
//! `pass_record` as advisory only and translate it to no events; the snapshot diff is
//! the single source of truth for passes.

use gridfpv_events::{
    AdapterId, CompetitorRef, Event, GateIndex, Pass, SessionId, SignalContext, SourceTime,
};
use serde::{Deserialize, Serialize};

use crate::dedup::Deduplicator;
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
/// `{"event": "...", ...fields}`. The `event` discriminator is the adapter's *own*
/// replay envelope — the field names inside each variant mirror RotorHazard's **real**
/// captured payloads (see the [module docs](self) and the captured fixture), so a
/// recorded session is one JSON array of these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Raw {
    /// A `race_status` message (`RHUI.emit_race_status`). Drives the session
    /// lifecycle via its integer `race_status` ([`race_status`]).
    RaceStatus(RawRaceStatus),
    /// A `current_laps` **snapshot** (`RHUI.emit_current_laps`). The whole lap table
    /// for every node, re-sent on each update; the adapter diffs it (see module docs).
    CurrentLaps(RawCurrentLaps),
    /// A `pass_record` (per-crossing, epoch-ms). Advisory cross-check only — emits no
    /// events (see module docs).
    PassRecord(RawPassRecord),
    /// A `node_data` message carrying per-node `pass_peak_rssi[]`. Updates the RSSI
    /// cache used to annotate subsequent passes.
    NodeData(RawNodeData),
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

/// A RotorHazard `current_laps` snapshot (see [`Raw::CurrentLaps`]).
///
/// Mirrors the real payload: `{ "current": { "node_index": [ … ] } }`. The outer
/// `node_index` array is positional — array index *is* the node index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawCurrentLaps {
    /// The `current` table.
    pub current: RawCurrent,
}

/// The `current` object of a `current_laps` snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawCurrent {
    /// One entry per node; the array index is the node index.
    pub node_index: Vec<RawNode>,
}

/// A single node's lap table within a `current_laps` snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawNode {
    /// This node's laps, in order (`lap_number` ascending). `lap_number` 0 is the
    /// holeshot (launch-to-first-gate).
    #[serde(default)]
    pub laps: Vec<RawLap>,
    /// The pilot assigned to this node, if any. `null` under the mock nodes; a real
    /// heat carries `{ id, name, callsign }`.
    #[serde(default)]
    pub pilot: Option<RawPilot>,
}

/// A pilot record attached to a node in `current_laps` (`build_laps_list`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawPilot {
    /// The pilot's callsign, where present.
    #[serde(default)]
    pub callsign: Option<String>,
}

/// A single lap entry within a node's `laps` array (see [`RawNode`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawLap {
    /// Position within this node's lap table (RotorHazard `lap_index`). Advisory.
    #[serde(default)]
    pub lap_index: Option<i64>,
    /// Per-node monotonic lap counter (RotorHazard `lap_number`). `0` is the
    /// holeshot. Carried through as the pass `sequence` and the dedup key.
    pub lap_number: u64,
    /// The lap duration in milliseconds (RotorHazard `lap_raw`). Advisory only — the
    /// engine derives laps from the pass stream — so it is carried for reference.
    #[serde(default)]
    pub lap_raw: Option<f64>,
    /// RotorHazard's pretty `"M:SS.mmm"` lap-time **string**. Advisory.
    #[serde(default)]
    pub lap_time: Option<String>,
    /// Crossing time in **cumulative milliseconds since race start** (RotorHazard
    /// `lap_time_stamp`, a float). Converted to microseconds for [`SourceTime`].
    pub lap_time_stamp: f64,
    /// Whether RotorHazard flagged this as a late lap (over the time limit). Advisory.
    #[serde(default)]
    pub late_lap: bool,
}

/// A RotorHazard `pass_record` (see [`Raw::PassRecord`]). Advisory cross-check only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawPassRecord {
    /// The node that recorded the crossing.
    pub node: u32,
    /// The node's frequency in MHz.
    #[serde(default)]
    pub frequency: Option<i64>,
    /// The crossing's **epoch-milliseconds** timestamp (a different clock from
    /// `lap_time_stamp`; see module docs).
    pub timestamp: f64,
}

/// A RotorHazard `node_data` message (see [`Raw::NodeData`]). Per-node RSSI arrays.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawNodeData {
    /// Per-node peak RSSI of the most recent pass (array index = node index). This is
    /// the per-pass RSSI source; `0` under mock nodes.
    #[serde(default)]
    pub pass_peak_rssi: Vec<f32>,
}

/// The competitor handle for a RotorHazard node seat: `"node-{index}"`. Stable across
/// pilot reassignment (the binding to a GridFPV pilot is a registration action, not
/// an adapter event — see `gridfpv_events::CompetitorRef`).
fn seat_ref(node_index: usize) -> CompetitorRef {
    CompetitorRef(format!("node-{node_index}"))
}

/// The default adapter id for a RotorHazard source.
const DEFAULT_ADAPTER_ID: &str = "rotorhazard";

/// Translates already-decoded RotorHazard messages into canonical events.
///
/// Pure translator (no IO): feed it [`Raw`] messages via
/// [`translate`](Adapter::translate). It is **stateful** so the `current_laps`
/// snapshot can be diffed:
///
/// - `last_race_status` makes lifecycle edges fire once per transition.
/// - `seen_seats` makes each node yield one [`Event::CompetitorSeen`].
/// - `pass_peak_rssi` caches the latest `node_data.pass_peak_rssi[node]`.
/// - A [`Deduplicator`] keyed on the pass `sequence` (the per-node `lap_number`)
///   guarantees a re-sent snapshot — or a reconnect that replays one — never
///   double-emits a [`Pass`].
#[derive(Debug, Clone)]
pub struct RotorHazardAdapter {
    id: AdapterId,
    /// Last `race_status` value observed, so lifecycle edges fire once per transition.
    last_race_status: Option<i64>,
    /// Node seats already announced, so each seat yields one `CompetitorSeen`.
    seen_seats: std::collections::HashSet<usize>,
    /// Latest `pass_peak_rssi[node]` per node, from the most recent `node_data`.
    pass_peak_rssi: std::collections::HashMap<usize, f32>,
    /// Suppresses passes already emitted (re-sent snapshot / reconnect). Keyed on the
    /// pass `(adapter, competitor, sequence)` by [`Deduplicator`].
    dedup: Deduplicator,
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
            pass_peak_rssi: std::collections::HashMap::new(),
            dedup: Deduplicator::new(),
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

    /// Convert a `lap_time_stamp` (cumulative ms since race start, float) to a
    /// [`SourceTime`], rounding to the nearest microsecond. `f64::round` rounds half
    /// away from zero; timestamps are non-negative so that is round-half-up.
    fn lap_stamp_to_source_time(lap_time_stamp: f64) -> SourceTime {
        SourceTime::from_micros((lap_time_stamp * 1_000.0).round() as i64)
    }

    /// Translate a `current_laps` snapshot. For each node array index, emit a `Pass`
    /// for every lap the [`Deduplicator`] has not already accepted (keyed on the
    /// per-node `lap_number`). A node's first surfaced lap also announces the seat as
    /// a [`Event::CompetitorSeen`]. Annotates passes with the cached per-node RSSI.
    fn translate_current_laps(&mut self, snapshot: RawCurrentLaps, out: &mut Vec<Event>) {
        for (node_index, node) in snapshot.current.node_index.into_iter().enumerate() {
            let competitor = seat_ref(node_index);

            for lap in node.laps {
                let signal = self
                    .pass_peak_rssi
                    .get(&node_index)
                    .map(|&rssi_peak| SignalContext {
                        rssi_peak: Some(rssi_peak),
                    });

                let pass = Pass {
                    adapter: self.id.clone(),
                    competitor: competitor.clone(),
                    at: Self::lap_stamp_to_source_time(lap.lap_time_stamp),
                    // The per-node lap_number is the monotonic sequence: it orders
                    // passes and anchors snapshot/reconnect dedup.
                    sequence: Some(lap.lap_number),
                    // RotorHazard reports the lap gate only (single start/finish gate).
                    gate: GateIndex::LAP,
                    signal,
                };

                // A re-sent snapshot replays every lap; only accept genuinely new ones.
                if !self.dedup.observe(&pass) {
                    continue;
                }

                // First genuinely new lap for this seat implies the seat is active.
                if self.seen_seats.insert(node_index) {
                    let callsign = node.pilot.as_ref().and_then(|p| p.callsign.clone());
                    out.push(Event::CompetitorSeen {
                        adapter: self.id.clone(),
                        // The competitor handle is always the node seat (stable across
                        // pilot edits); a known callsign is informational only.
                        competitor: callsign
                            .map(CompetitorRef)
                            .unwrap_or_else(|| competitor.clone()),
                    });
                }

                out.push(Event::Pass(pass));
            }
        }
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

    /// Update the per-node RSSI cache from a `node_data` message. Emits no events; the
    /// cache annotates subsequent passes' [`SignalContext`].
    fn update_node_data(&mut self, data: RawNodeData) {
        for (node_index, &rssi) in data.pass_peak_rssi.iter().enumerate() {
            self.pass_peak_rssi.insert(node_index, rssi);
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
            Raw::RaceStatus(status) => self.translate_race_status(status, &mut out),
            Raw::CurrentLaps(snapshot) => self.translate_current_laps(snapshot, &mut out),
            // pass_record is an advisory cross-check; it mints no canonical events.
            Raw::PassRecord(_) => {}
            Raw::NodeData(data) => self.update_node_data(data),
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small RECORDED RotorHazard session, derived from the **real** captured frames
    /// (`captured-mock-race.json`, #25): a staged→racing→done lifecycle, growing
    /// `current_laps` snapshots (one re-sent to prove dedup), `node_data` RSSI and
    /// `pass_record` cross-checks. Three nodes; node-0 runs three laps.
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

    /// Build a single-node `current_laps` snapshot for a given node index.
    fn snapshot(up_to_node: usize, node_index: usize, laps: Vec<RawLap>) -> Raw {
        let mut nodes: Vec<RawNode> = (0..=up_to_node)
            .map(|_| RawNode {
                laps: Vec::new(),
                pilot: None,
            })
            .collect();
        nodes[node_index].laps = laps;
        Raw::CurrentLaps(RawCurrentLaps {
            current: RawCurrent { node_index: nodes },
        })
    }

    fn lap(lap_number: u64, lap_time_stamp: f64) -> RawLap {
        RawLap {
            lap_index: Some(lap_number as i64),
            lap_number,
            lap_raw: None,
            lap_time: None,
            lap_time_stamp,
            late_lap: false,
        }
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
        let heat = SessionId("heat-0".into());
        let expected = vec![
            // STAGING (3) carries no lifecycle edge. RACING (1) starts the session.
            Event::SessionStarted {
                adapter: rh.clone(),
                session: heat.clone(),
            },
            // node-0 holeshot (lap 0): ts 2215.296... ms -> 2_215_296 µs, RSSI 0.
            Event::CompetitorSeen {
                adapter: rh.clone(),
                competitor: CompetitorRef("node-0".into()),
            },
            Event::Pass(Pass {
                adapter: rh.clone(),
                competitor: CompetitorRef("node-0".into()),
                at: SourceTime::from_micros(2_215_296),
                sequence: Some(0),
                gate: GateIndex::LAP,
                signal: Some(SignalContext {
                    rssi_peak: Some(0.0),
                }),
            }),
            // node-1 holeshot (lap 0): ts 5416.201... ms -> 5_416_202 µs.
            Event::CompetitorSeen {
                adapter: rh.clone(),
                competitor: CompetitorRef("node-1".into()),
            },
            Event::Pass(Pass {
                adapter: rh.clone(),
                competitor: CompetitorRef("node-1".into()),
                at: SourceTime::from_micros(5_416_202),
                sequence: Some(0),
                gate: GateIndex::LAP,
                signal: Some(SignalContext {
                    rssi_peak: Some(0.0),
                }),
            }),
            // node-0 lap 1: ts 7416.519... ms -> 7_416_519 µs (re-sent snapshot adds it).
            Event::Pass(Pass {
                adapter: rh.clone(),
                competitor: CompetitorRef("node-0".into()),
                at: SourceTime::from_micros(7_416_519),
                sequence: Some(1),
                gate: GateIndex::LAP,
                signal: Some(SignalContext {
                    rssi_peak: Some(0.0),
                }),
            }),
            // node-0 lap 2: ts 10017.685... ms -> 10_017_685 µs.
            Event::Pass(Pass {
                adapter: rh.clone(),
                competitor: CompetitorRef("node-0".into()),
                at: SourceTime::from_micros(10_017_685),
                sequence: Some(2),
                gate: GateIndex::LAP,
                signal: Some(SignalContext {
                    rssi_peak: Some(0.0),
                }),
            }),
            // DONE (2) ends the session.
            Event::SessionEnded {
                adapter: rh,
                session: heat,
            },
        ];

        assert_eq!(events, expected);
    }

    #[test]
    fn lap_time_stamp_milliseconds_round_to_microseconds() {
        // A fractional cumulative ms rounds half-away-from-zero to the nearest µs.
        let mut adapter = RotorHazardAdapter::new();
        let events = adapter.translate(snapshot(2, 2, vec![lap(5, 12_345.678_9)]));
        let pass = events
            .iter()
            .find_map(|e| match e {
                Event::Pass(p) => Some(p),
                _ => None,
            })
            .expect("a pass");
        // 12_345.6789 ms * 1000 = 12_345_678.9 µs -> 12_345_679.
        assert_eq!(pass.at, SourceTime::from_micros(12_345_679));
        assert_eq!(pass.sequence, Some(5));
        assert_eq!(pass.competitor, CompetitorRef("node-2".into()));
        // No node_data seen yet -> no signal context (still a valid pass).
        assert!(pass.signal.is_none());
    }

    #[test]
    fn node_data_pass_peak_rssi_becomes_signal_context() {
        let mut adapter = RotorHazardAdapter::new();
        // node_data first, then a lap on node 0 picks up its cached RSSI.
        adapter.translate(Raw::NodeData(RawNodeData {
            pass_peak_rssi: vec![201.5, 0.0],
        }));
        let events = adapter.translate(snapshot(0, 0, vec![lap(0, 1_000.0)]));
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
    fn resent_snapshot_does_not_double_emit() {
        let mut adapter = RotorHazardAdapter::new();
        let first = adapter.translate(snapshot(0, 0, vec![lap(0, 1_000.0)]));
        // One CompetitorSeen + one Pass on the first sighting.
        assert_eq!(
            first.iter().filter(|e| matches!(e, Event::Pass(_))).count(),
            1
        );

        // The same snapshot re-sent (RH re-sends the whole table on every update).
        let again = adapter.translate(snapshot(0, 0, vec![lap(0, 1_000.0)]));
        assert!(again.is_empty(), "re-sent snapshot emits nothing");

        // A grown snapshot adds only the new lap.
        let grown = adapter.translate(snapshot(0, 0, vec![lap(0, 1_000.0), lap(1, 3_500.0)]));
        let passes: Vec<u64> = grown
            .iter()
            .filter_map(|e| match e {
                Event::Pass(p) => p.sequence,
                _ => None,
            })
            .collect();
        assert_eq!(passes, vec![1], "only the new lap is emitted");
    }

    #[test]
    fn competitor_seen_emitted_once_per_seat() {
        let mut adapter = RotorHazardAdapter::new();
        let first = adapter.translate(snapshot(0, 0, vec![lap(0, 1_000.0)]));
        let grown = adapter.translate(snapshot(0, 0, vec![lap(0, 1_000.0), lap(1, 3_000.0)]));
        let seen = first
            .iter()
            .chain(grown.iter())
            .filter(|e| matches!(e, Event::CompetitorSeen { .. }))
            .count();
        assert_eq!(seen, 1, "seat announced exactly once across snapshots");
    }

    #[test]
    fn competitor_seen_uses_callsign_when_present() {
        let mut adapter = RotorHazardAdapter::new();
        let snap = Raw::CurrentLaps(RawCurrentLaps {
            current: RawCurrent {
                node_index: vec![RawNode {
                    laps: vec![lap(0, 1_000.0)],
                    pilot: Some(RawPilot {
                        callsign: Some("Ace".into()),
                    }),
                }],
            },
        });
        let events = adapter.translate(snap);
        assert!(events.iter().any(|e| matches!(
            e,
            Event::CompetitorSeen { competitor, .. } if competitor.0 == "Ace"
        )));
        // The pass still uses the stable node seat handle.
        let pass = events
            .iter()
            .find_map(|e| match e {
                Event::Pass(p) => Some(p),
                _ => None,
            })
            .unwrap();
        assert_eq!(pass.competitor, CompetitorRef("node-0".into()));
    }

    #[test]
    fn pass_record_emits_nothing() {
        let mut adapter = RotorHazardAdapter::new();
        let events = adapter.translate(Raw::PassRecord(RawPassRecord {
            node: 0,
            frequency: Some(5658),
            timestamp: 1_781_909_632_912.0,
        }));
        assert!(events.is_empty(), "pass_record is advisory only");
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
