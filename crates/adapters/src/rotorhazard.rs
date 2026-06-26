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
    AdapterId, CompetitorRef, Event, GateIndex, Pass, SessionId, SignalChunk, SignalContext,
    SignalHistory, SignalThresholds, SourceTime,
};
use serde::{Deserialize, Serialize};

use crate::dedup::Deduplicator;
use crate::{Adapter, Capabilities};

/// Live Socket.IO transport (feature `live`): connects to a running RotorHazard,
/// decodes its socket events into [`Raw`], and feeds this adapter. See
/// `tests/rh_live.rs` and `docker/rotorhazard/`.
#[cfg(feature = "live")]
pub mod transport;

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
    /// cache used to annotate subsequent passes, and (for a signal-capable adapter while a
    /// race is active) emits a [`Event::SignalChunk`] trace sample per node.
    NodeData(RawNodeData),
    /// An `enter_and_exit_at_levels` message carrying the per-node detection thresholds.
    /// Emits [`Event::SignalThresholds`] for a signal-capable adapter.
    EnterExitLevels(RawEnterExitLevels),
    /// A `current_marshal_data` response (`RHUI.emit_race_marshal_data`), requested at heat end on
    /// **newer** RotorHazard. Carries the **dense** per-node `history_values`/`history_times` trace
    /// for every seat at once; emits one [`Event::SignalHistory`] per node for a signal-capable
    /// adapter (the full-fidelity trace that supersedes the coarse streamed [`SignalChunk`]s — see
    /// [`RawMarshalData`]).
    MarshalData(RawMarshalData),
    /// A `race_list` response (`RHUI.emit_race_list`), listing the **saved** races and their
    /// per-pilot `pilotrace_id`s. On the RotorHazard build whose marshal API is per-pilotrace
    /// ([`Raw::RaceDetails`]), the transport reads these ids to pull each seat's dense history.
    /// Emits no canonical events itself (it is a transport routing payload); the adapter exposes the
    /// ids via [`take_pilotrace_requests`](RotorHazardAdapter::take_pilotrace_requests).
    RaceList(RawRaceList),
    /// A `race_details` response (`get_pilotrace`), the **per-pilotrace** dense marshal payload on
    /// the RotorHazard build that has no aggregate `current_marshal_data`. Carries one seat's
    /// `history_values`/`history_times` + `enter_at`/`exit_at`; emits a [`Event::SignalHistory`]
    /// (and refreshes [`SignalThresholds`]) for that seat — see [`RawRaceDetails`].
    RaceDetails(RawRaceDetails),
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
///
/// RotorHazard's `emit_node_data` (`RHUI.py`) re-sends these scalar arrays on its heartbeat
/// cadence while a race runs — they are the **latest aggregate** per node, *not* a per-tick
/// history array (the full `history_values` trace lives in the request-driven
/// `current_marshal_data`, which a live translator does not subscribe to). So the trace this
/// adapter captures samples `node_peak_rssi` at the `node_data` emit cadence — see
/// [`SignalChunk`](gridfpv_events::SignalChunk)'s fidelity bound.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawNodeData {
    /// Per-node peak RSSI of the most recent pass (array index = node index). This is
    /// the per-pass RSSI source for a [`Pass`]'s [`SignalContext`]; `0` under mock nodes.
    #[serde(default)]
    pub pass_peak_rssi: Vec<f32>,
    /// Per-node **current** peak RSSI (the node's running signal level, reset each crossing;
    /// array index = node index). This is the per-tick value the captured trace samples — it
    /// is the closest live proxy to "current RSSI" that `node_data` exposes. Absent on older
    /// payloads (defaults empty), in which case the trace falls back to `pass_peak_rssi`.
    #[serde(default)]
    pub node_peak_rssi: Vec<f32>,
}

/// A RotorHazard `enter_and_exit_at_levels` message (`RHUI.emit_enter_and_exit_at_levels`):
/// the per-node detection thresholds the timer is calibrated with. The signal-as-evidence
/// layer captures these so a marshal sees the levels a call was made against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawEnterExitLevels {
    /// Per-node enter threshold (RSSI rising above this opens a pass; array index = node index).
    #[serde(default)]
    pub enter_at_levels: Vec<f32>,
    /// Per-node exit threshold (RSSI falling below this closes the pass; array index = node index).
    #[serde(default)]
    pub exit_at_levels: Vec<f32>,
}

/// A RotorHazard `current_marshal_data` response (`RHUI.emit_race_marshal_data`), the
/// **request-driven** dense marshal payload its own marshal page pulls *after* a race.
///
/// Shape (validated against `src/server/RHUI.py::emit_race_marshal_data`):
/// `{ "race": { "start_time": <monotonic-seconds>, … }, "seats": { "<index>": { history_values,
/// history_times, enter_at, exit_at, laps, … }, … } }`. The `seats` map is keyed by **stringified
/// node index** (JSON object keys are strings). Each seat carries the detector's own per-tick
/// `history_values` (RSSI integers) paired with `history_times` (monotonic-clock **seconds**,
/// floats), the dense trace RotorHazard's marshal graph renders. `race.start_time` is the
/// monotonic-second origin of the race; subtracting it makes each `history_times` value
/// race-relative — the same origin as `lap_time_stamp` / [`SourceTime`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawMarshalData {
    /// The race-level header. Carries `start_time` (the monotonic-second origin) used to make the
    /// per-seat `history_times` race-relative. Optional: absent on a payload built before a race.
    #[serde(default)]
    pub race: Option<RawMarshalRace>,
    /// Per-seat dense data, keyed by **stringified node index** (`"0"`, `"1"`, …).
    #[serde(default)]
    pub seats: std::collections::BTreeMap<String, RawMarshalSeat>,
}

/// The `race` header of a `current_marshal_data` payload (see [`RawMarshalData`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawMarshalRace {
    /// The race's monotonic-clock start time in **seconds** (`race.start_time_monotonic`). The
    /// origin the per-seat `history_times` are measured from; subtracting it yields race-relative
    /// time. Absent on some payloads (defaults to `0.0`, i.e. times already race-relative).
    #[serde(default)]
    pub start_time: f64,
}

/// One seat's dense marshal data within a `current_marshal_data` payload (see [`RawMarshalData`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawMarshalSeat {
    /// The detector's per-tick RSSI history (filtered ADC counts), parallel to `history_times`.
    /// Accepts either a JSON array or a JSON-encoded string (different RH builds wire it either way).
    #[serde(default, deserialize_with = "de_f64_history")]
    pub history_values: Vec<f64>,
    /// The monotonic-clock **seconds** timestamp of each `history_values` sample, parallel to it.
    #[serde(default, deserialize_with = "de_f64_history")]
    pub history_times: Vec<f64>,
}

/// A `race_list` response (`RHUI.emit_race_list`): the saved-race tree whose leaves carry the
/// `pilotrace_id`s the per-pilotrace marshal request ([`Raw::RaceDetails`]) needs.
///
/// Shape: `{ "heats": { "<heat_id>": { "rounds": { "<round_id>": { "start_time": <monotonic-sec>,
/// "pilotraces": [ { "pilotrace_id", "node_index" }, … ] }, … } }, … } }`. The adapter walks it for
/// the `(pilotrace_id, node_index)` pairs the transport then pulls one at a time, carrying the
/// round's `start_time` so the per-pilotrace history can be made race-relative.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawRaceList {
    /// Saved heats by stringified heat id.
    #[serde(default)]
    pub heats: std::collections::BTreeMap<String, RawRaceListHeat>,
}

/// One heat in a `race_list` (see [`RawRaceList`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawRaceListHeat {
    /// Saved rounds by stringified round id.
    #[serde(default)]
    pub rounds: std::collections::BTreeMap<String, RawRaceListRound>,
}

/// One saved round in a `race_list` (see [`RawRaceList`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawRaceListRound {
    /// The round's monotonic-clock start time in **seconds** — the origin the per-pilotrace
    /// `history_times` are measured from (used to make the dense history race-relative).
    #[serde(default)]
    pub start_time: f64,
    /// The per-pilot saved-race entries, each with the `pilotrace_id` the marshal request targets.
    #[serde(default)]
    pub pilotraces: Vec<RawRaceListPilotRace>,
}

/// One per-pilot saved-race entry in a `race_list` round (see [`RawRaceListRound`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawRaceListPilotRace {
    /// The id `get_pilotrace` targets to fetch this seat's dense history.
    pub pilotrace_id: i64,
    /// The node seat this saved pilotrace belongs to (maps to `node-{index}`).
    #[serde(default)]
    pub node_index: Option<usize>,
}

/// A `race_details` response (`get_pilotrace`): one saved pilotrace's dense history + thresholds.
///
/// Shape: `{ "node_index", "history_values", "history_times", "enter_at", "exit_at", … }`. The
/// history arrays wire as **JSON-encoded strings** on this RH build (`json.dumps(...)`), so they are
/// parsed leniently. `history_times` are monotonic **seconds**; the adapter subtracts the round's
/// `start_time` (carried from the `race_list`) — or the first sample when no start is known — to make
/// the dense trace race-relative.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawRaceDetails {
    /// The node seat this pilotrace belongs to (maps to `node-{index}`).
    #[serde(default)]
    pub node_index: Option<usize>,
    /// The detector's per-tick RSSI history (filtered ADC counts), parallel to `history_times`.
    #[serde(default, deserialize_with = "de_f64_history")]
    pub history_values: Vec<f64>,
    /// The monotonic-clock **seconds** timestamp of each `history_values` sample, parallel to it.
    #[serde(default, deserialize_with = "de_f64_history")]
    pub history_times: Vec<f64>,
    /// The node's enter threshold the call was made against, if reported.
    #[serde(default)]
    pub enter_at: Option<f64>,
    /// The node's exit threshold the call was made against, if reported.
    #[serde(default)]
    pub exit_at: Option<f64>,
}

/// Deserialize a history array that may arrive either as a JSON array (`[1, 2, 3]`) or as a
/// JSON-**encoded string** (`"[1, 2, 3]"`). RotorHazard's `get_pilotrace` `json.dumps`es the history
/// while `current_marshal_data` sends a bare array — accept both so one [`Raw`] shape covers both RH
/// builds. A malformed string yields an empty history (no panic).
fn de_f64_history<'de, D>(deserializer: D) -> Result<Vec<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ArrayOrString {
        Array(Vec<f64>),
        Str(String),
    }
    Ok(match ArrayOrString::deserialize(deserializer)? {
        ArrayOrString::Array(v) => v,
        ArrayOrString::Str(s) => serde_json::from_str(&s).unwrap_or_default(),
    })
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
    /// Whether to capture the RSSI trace (marshaling Slice 1). Gated on the adapter's signal
    /// capability so only signal-capable sources produce [`SignalChunk`]/[`SignalThresholds`];
    /// `true` for a real RotorHazard. A non-signal source would set this `false` and emit none.
    signal_capture: bool,
    /// The capture cadence: microseconds between consecutive trace samples, used to place each
    /// `node_data` sample on the source clock. RotorHazard's `node_data` is heartbeat-driven, so
    /// this mirrors its emit interval ([`DEFAULT_NODE_DATA_PERIOD_MICROS`]).
    node_data_period_micros: u32,
    /// Whether a race is currently running (between the `SessionStarted` and `SessionEnded`
    /// edges). Trace samples are only captured while racing — idle `node_data` is monitoring
    /// churn, not evidence for a heat.
    race_active: bool,
    /// Per-node trace sample counter, reset each race. The `from` of a node's chunk is
    /// `index * node_data_period_micros`, so concatenated chunks reconstruct a contiguous trace
    /// deterministically without the adapter reading a clock.
    sample_index: std::collections::HashMap<usize, u64>,
    /// Last `(enter, exit)` thresholds emitted per node, so an unchanged
    /// `enter_and_exit_at_levels` re-send does not re-emit a [`SignalThresholds`] fact.
    last_thresholds: std::collections::HashMap<usize, (u16, u16)>,
    /// Set when a race reaches `DONE` (a signal-capable adapter only): a flag the **transport** drains
    /// via [`take_marshal_request`](Self::take_marshal_request) to know it should send RotorHazard's
    /// `current_race_marshal` request and pull the dense `current_marshal_data` history at heat end.
    /// The pure translator cannot speak the socket, so it records the *intent* here and the transport
    /// acts on it — keeping all wire knowledge in the transport while the trigger stays in the
    /// translator (driven by the same `race_status` stream it already folds).
    pending_marshal_request: bool,
    /// Pending per-pilotrace marshal pulls discovered from a `race_list`, drained by the transport
    /// via [`take_pilotrace_requests`](Self::take_pilotrace_requests). On the RotorHazard build whose
    /// marshal API is per-pilotrace (`get_pilotrace` -> `race_details`), the heat-end flow is:
    /// save laps -> request `race_list` -> pull each `(pilotrace_id)` here -> fold `race_details` into
    /// dense history. Each entry carries the round `start_time` so the history can be made
    /// race-relative when the `race_details` response (which omits it) comes back.
    pending_pilotrace_requests: Vec<PilotRaceRequest>,
    /// The round `start_time` (monotonic seconds) per `node_index`, learned from the most recent
    /// `race_list`, so a `race_details` response (which carries no start time) can be made
    /// race-relative. Cleared each new race.
    pilotrace_start_time: std::collections::HashMap<usize, f64>,
}

/// A pending per-pilotrace marshal pull the transport issues (`get_pilotrace { pilotrace_id }`),
/// discovered from a `race_list`. See [`RotorHazardAdapter::take_pilotrace_requests`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PilotRaceRequest {
    /// The id to fetch (`get_pilotrace`'s `pilotrace_id`).
    pub pilotrace_id: i64,
}

/// Default trace-capture cadence: RotorHazard's `node_data` heartbeat emit interval. The real
/// server re-sends `node_data` a few times a second; `100_000` µs (10 Hz) is a representative
/// default. The exact realtime cadence is the fidelity bound to confirm on hardware
/// (marshaling.html §4) — the capture is faithful to *what RH streams*, which is one aggregate
/// sample per emit, not the detector's internal per-tick history.
pub const DEFAULT_NODE_DATA_PERIOD_MICROS: u32 = 100_000;

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
            // RotorHazard is the full-signal case, so trace capture is on by default.
            signal_capture: true,
            node_data_period_micros: DEFAULT_NODE_DATA_PERIOD_MICROS,
            race_active: false,
            sample_index: std::collections::HashMap::new(),
            last_thresholds: std::collections::HashMap::new(),
            pending_marshal_request: false,
            pending_pilotrace_requests: Vec::new(),
            pilotrace_start_time: std::collections::HashMap::new(),
        }
    }

    /// Take (and clear) the per-pilotrace marshal pulls discovered from the most recent `race_list`.
    ///
    /// The transport calls this after feeding a `race_list` payload: each returned
    /// [`PilotRaceRequest`] should be issued as a `get_pilotrace` so its `race_details` response folds
    /// into dense history. Draining clears the queue so the same `race_list` is not pulled twice.
    pub fn take_pilotrace_requests(&mut self) -> Vec<PilotRaceRequest> {
        std::mem::take(&mut self.pending_pilotrace_requests)
    }

    /// Take (and clear) the "request the dense marshal data" flag, set when a race reached `DONE`.
    ///
    /// The transport calls this after feeding a `race_status` payload through the adapter: a `true`
    /// return means a heat just ended on a signal-capable adapter, so the transport should emit
    /// RotorHazard's `current_race_marshal` request and let the `current_marshal_data` response feed
    /// back through [`translate`](Adapter::translate) as [`Event::SignalHistory`]. It is one-shot per
    /// heat end — draining clears it so a re-sent `DONE` (a reconnect replay) does not re-request.
    pub fn take_marshal_request(&mut self) -> bool {
        std::mem::take(&mut self.pending_marshal_request)
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
            race_status::RACING => {
                // A genuinely new race starts here (this arm only runs on a real
                // transition *into* RACING — `previous != Some(RACING)`). RotorHazard
                // resets each node's `lap_number` to 0 at the start of every race, and
                // the per-lap dedup is keyed on that `(competitor, sequence=lap_number)`.
                // Without a reset, heat 2's lap 0–N collide with heat 1's already-seen
                // lap 0–N over a persistent connection and every lap past the first heat
                // is suppressed (#105 cross-heat bug). Reset the per-race dedup + seat
                // state on the RACING edge so each heat starts fresh.
                //
                // This is safe for the reconnect-dedup guarantee: a re-sent
                // `current_laps` snapshot carries *no* status transition, so it never
                // reaches this arm; and a *mid-race* reconnect keeps
                // `last_race_status == RACING` (so this arm does NOT fire — no reset),
                // leaving the replayed snapshot suppressed. The persistent driver
                // **reuses one `RotorHazardAdapter` across reconnects** (#105 fix:
                // `connect` takes it, `disconnect` returns it), so `last_race_status`
                // and the dedup are continuous — a mid-race reconnect does not reset
                // and its replayed laps stay deduped. This reset therefore fires only
                // on a true new-race edge within that adapter's (now connection-
                // spanning) lifetime.
                self.dedup = Deduplicator::new();
                self.seen_seats.clear();
                // Marshaling Slice 1: a fresh race resets the trace's time base so each heat's
                // captured chunks start at source-time 0 — deterministic and heat-local.
                self.race_active = true;
                self.sample_index.clear();
                self.last_thresholds.clear();
                // A fresh race invalidates any stale marshal-pull state from the previous heat.
                self.pending_marshal_request = false;
                self.pending_pilotrace_requests.clear();
                self.pilotrace_start_time.clear();
                out.push(Event::SessionStarted {
                    adapter: self.id.clone(),
                    session: Self::session_id(status.race_heat_id),
                });
            }
            race_status::DONE => {
                // Stop capturing trace samples once the race closes (idle `node_data` is not
                // heat evidence).
                self.race_active = false;
                // The heat just ended: a signal-capable adapter should now pull RotorHazard's dense
                // `current_marshal_data` history (the full-fidelity trace its marshal page reviews).
                // Record the intent for the transport to act on — the pure translator can't emit the
                // socket request itself. Only on a genuine RACING/STAGING -> DONE edge (this arm runs
                // once per transition), so a re-sent DONE on a reconnect does not re-request.
                if self.signal_capture {
                    self.pending_marshal_request = true;
                }
                out.push(Event::SessionEnded {
                    adapter: self.id.clone(),
                    session: Self::session_id(status.race_heat_id),
                })
            }
            // READY (reset) and STAGING (pre-roll) carry no canonical lifecycle edge.
            race_status::READY | race_status::STAGING => {}
            _ => {}
        }
    }

    /// Update the per-node RSSI cache from a `node_data` message, and — for a signal-capable
    /// adapter while a race is active — emit one [`Event::SignalChunk`] trace sample per node.
    ///
    /// The cache (`pass_peak_rssi`) annotates subsequent passes' [`SignalContext`] (unchanged).
    /// The trace samples `node_peak_rssi` (the live signal level), falling back to
    /// `pass_peak_rssi` on an older payload that omits it. Each sample is placed on the source
    /// clock at `sample_index[node] * node_data_period_micros`, so concatenating a node's chunks
    /// reconstructs a contiguous, deterministic trace (`gridfpv_projection::signal_trace`). RSSI
    /// is clamped into `u16` ADC counts (RH's stock range is ~0–255; negatives clamp to 0).
    fn update_node_data(&mut self, data: RawNodeData, out: &mut Vec<Event>) {
        for (node_index, &rssi) in data.pass_peak_rssi.iter().enumerate() {
            self.pass_peak_rssi.insert(node_index, rssi);
        }

        // Trace capture is gated on the signal capability and only runs while a race is active.
        if !self.signal_capture || !self.race_active {
            return;
        }
        // Sample the live node peak where present; older payloads without it fall back to the
        // per-pass peak so a trace is still captured at the same cadence.
        let samples = if data.node_peak_rssi.is_empty() {
            &data.pass_peak_rssi
        } else {
            &data.node_peak_rssi
        };
        for (node_index, &rssi) in samples.iter().enumerate() {
            let sample = rssi.round().clamp(0.0, u16::MAX as f32) as u16;
            let index = self.sample_index.entry(node_index).or_insert(0);
            let from = SourceTime::from_micros(*index as i64 * self.node_data_period_micros as i64);
            *index += 1;
            out.push(Event::SignalChunk(SignalChunk {
                adapter: self.id.clone(),
                competitor: seat_ref(node_index),
                from,
                period_micros: self.node_data_period_micros,
                rssi: vec![sample],
            }));
        }
    }

    /// Translate a `current_marshal_data` response (newer RotorHazard) into one
    /// [`Event::SignalHistory`] per seat that carries a dense trace — the **full-fidelity** per-tick
    /// RSSI history RotorHazard records, which supersedes the coarse streamed [`SignalChunk`] samples
    /// in the `signal_trace` projection. Each seat's `start_time` is the race origin from the payload
    /// header. Gated on the signal capability.
    fn translate_marshal_data(&mut self, data: RawMarshalData, out: &mut Vec<Event>) {
        if !self.signal_capture {
            return;
        }
        let start_time = data.race.as_ref().map(|r| r.start_time).unwrap_or(0.0);
        for (key, seat) in data.seats {
            // Seats are keyed by stringified node index; a non-numeric key is not a node seat.
            let Ok(node_index) = key.parse::<usize>() else {
                continue;
            };
            self.emit_dense_history(
                node_index,
                start_time,
                &seat.history_times,
                &seat.history_values,
                out,
            );
        }
    }

    /// Translate a `race_list` (saved-race tree) into the per-pilotrace pulls the transport issues.
    ///
    /// Walks every heat/round, recording each `(pilotrace_id)` to pull and the round `start_time` per
    /// `node_index` (so the later `race_details` history can be made race-relative). The pulls queue
    /// in [`pending_pilotrace_requests`](Self::pending_pilotrace_requests) for the transport to drain;
    /// this emits no canonical events. Idempotent within a heat: a re-sent `race_list` rebuilds the
    /// same queue (the transport drains it once per send).
    fn translate_race_list(&mut self, list: RawRaceList) {
        if !self.signal_capture {
            return;
        }
        let mut requests = Vec::new();
        for heat in list.heats.into_values() {
            for round in heat.rounds.into_values() {
                for pr in round.pilotraces {
                    if let Some(node_index) = pr.node_index {
                        self.pilotrace_start_time
                            .insert(node_index, round.start_time);
                    }
                    requests.push(PilotRaceRequest {
                        pilotrace_id: pr.pilotrace_id,
                    });
                }
            }
        }
        self.pending_pilotrace_requests = requests;
    }

    /// Translate a `race_details` (one saved pilotrace) into that seat's dense [`Event::SignalHistory`]
    /// — the per-pilotrace marshal path on the RotorHazard build with no aggregate
    /// `current_marshal_data`. Refreshes the seat's [`SignalThresholds`] from `enter_at`/`exit_at`
    /// when present. Uses the round `start_time` learned from the `race_list` as the race origin (or
    /// the first sample when none is known). Gated on the signal capability.
    fn translate_race_details(&mut self, details: RawRaceDetails, out: &mut Vec<Event>) {
        if !self.signal_capture {
            return;
        }
        let Some(node_index) = details.node_index else {
            return;
        };
        // Prefer the race start learned from the race_list; else anchor on the first sample so the
        // trace is trace-relative (starts at 0) — matching the coarse trace's per-heat origin.
        let start_time = self
            .pilotrace_start_time
            .get(&node_index)
            .copied()
            .or_else(|| details.history_times.first().copied())
            .unwrap_or(0.0);
        self.emit_dense_history(
            node_index,
            start_time,
            &details.history_times,
            &details.history_values,
            out,
        );
        // Refresh thresholds the call was made against, if reported (last-writer-wins downstream).
        if let (Some(enter), Some(exit)) = (details.enter_at, details.exit_at) {
            let enter = enter.round().clamp(0.0, u16::MAX as f32 as f64) as u16;
            let exit = exit.round().clamp(0.0, u16::MAX as f32 as f64) as u16;
            out.push(Event::SignalThresholds(SignalThresholds {
                adapter: self.id.clone(),
                competitor: seat_ref(node_index),
                enter,
                exit,
            }));
        }
    }

    /// Emit one dense [`Event::SignalHistory`] for a seat from its parallel `times`/`values` arrays.
    ///
    /// `start_time` is the race origin in **seconds**; each `times[i]` (also seconds) is made
    /// race-relative by subtracting it, then converted to integer microseconds (the [`SourceTime`]
    /// unit). RSSI clamps into `u16` ADC counts, native units, **no resampling** (the Slice 1
    /// fidelity caution). Mismatched-length arrays use the common prefix; an empty history emits
    /// nothing. Shared by the `current_marshal_data` and `race_details` paths so the two RH builds
    /// fold identically.
    fn emit_dense_history(
        &self,
        node_index: usize,
        start_time: f64,
        history_times: &[f64],
        history_values: &[f64],
        out: &mut Vec<Event>,
    ) {
        let n = history_values.len().min(history_times.len());
        if n == 0 {
            return;
        }
        let mut times = Vec::with_capacity(n);
        let mut rssi = Vec::with_capacity(n);
        for i in 0..n {
            // Race-relative seconds -> integer microseconds (round half away from zero); clamp to
            // non-negative so a sample fractionally before the recorded start can't go negative.
            let rel_secs = history_times[i] - start_time;
            let micros = (rel_secs * 1_000_000.0).round().max(0.0) as i64;
            times.push(micros);
            rssi.push(history_values[i].round().clamp(0.0, u16::MAX as f32 as f64) as u16);
        }
        out.push(Event::SignalHistory(SignalHistory {
            adapter: self.id.clone(),
            competitor: seat_ref(node_index),
            times,
            rssi,
        }));
    }

    /// Emit per-node [`Event::SignalThresholds`] from an `enter_and_exit_at_levels` message —
    /// for a signal-capable adapter only. A node is emitted once and then only when its
    /// `(enter, exit)` pair changes, so a steady re-send does not spam the log.
    fn update_thresholds(&mut self, levels: RawEnterExitLevels, out: &mut Vec<Event>) {
        if !self.signal_capture {
            return;
        }
        let n = levels
            .enter_at_levels
            .len()
            .min(levels.exit_at_levels.len());
        for node_index in 0..n {
            let enter = levels.enter_at_levels[node_index]
                .round()
                .clamp(0.0, u16::MAX as f32) as u16;
            let exit = levels.exit_at_levels[node_index]
                .round()
                .clamp(0.0, u16::MAX as f32) as u16;
            if self.last_thresholds.get(&node_index) == Some(&(enter, exit)) {
                continue;
            }
            self.last_thresholds.insert(node_index, (enter, exit));
            out.push(Event::SignalThresholds(SignalThresholds {
                adapter: self.id.clone(),
                competitor: seat_ref(node_index),
                enter,
                exit,
            }));
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
            Raw::NodeData(data) => self.update_node_data(data, &mut out),
            Raw::EnterExitLevels(levels) => self.update_thresholds(levels, &mut out),
            Raw::MarshalData(data) => self.translate_marshal_data(data, &mut out),
            Raw::RaceList(list) => self.translate_race_list(list),
            Raw::RaceDetails(details) => self.translate_race_details(details, &mut out),
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
        let all = run(&mut adapter, parse(SESSION_FIXTURE));
        // The fixture's `node_data` ticks now also produce SignalChunk trace samples (Slice 1);
        // this test asserts the lap/lifecycle backbone, so filter the trace out and assert it in
        // `recorded_session_captures_signal_trace` separately.
        let events: Vec<Event> = all
            .iter()
            .filter(|e| !matches!(e, Event::SignalChunk(_) | Event::SignalThresholds(_)))
            .cloned()
            .collect();

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
            node_peak_rssi: vec![201.5, 0.0],
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

    /// Helper: collect the `SignalChunk`s in an event slice.
    fn chunks(events: &[Event]) -> Vec<&SignalChunk> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::SignalChunk(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn node_data_captures_a_trace_only_while_racing() {
        let mut adapter = RotorHazardAdapter::new();
        // Idle `node_data` before RACING is monitoring churn — no trace captured.
        let idle = adapter.translate(Raw::NodeData(RawNodeData {
            pass_peak_rssi: vec![70.0, 60.0],
            node_peak_rssi: vec![70.0, 60.0],
        }));
        assert!(chunks(&idle).is_empty(), "no trace before the race starts");

        // RACING opens capture.
        adapter.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::RACING,
            race_heat_id: Some(0),
        }));
        let t0 = adapter.translate(Raw::NodeData(RawNodeData {
            pass_peak_rssi: vec![150.0, 120.0],
            node_peak_rssi: vec![150.0, 120.0],
        }));
        let t1 = adapter.translate(Raw::NodeData(RawNodeData {
            pass_peak_rssi: vec![151.0, 121.0],
            node_peak_rssi: vec![151.0, 121.0],
        }));

        // One chunk per node per tick, sampling node_peak_rssi, anchored on the per-node index.
        let c0 = chunks(&t0);
        assert_eq!(c0.len(), 2);
        assert_eq!(c0[0].competitor, CompetitorRef("node-0".into()));
        assert_eq!(c0[0].rssi, vec![150]);
        assert_eq!(c0[0].from, SourceTime::from_micros(0));
        assert_eq!(c0[0].period_micros, DEFAULT_NODE_DATA_PERIOD_MICROS);
        assert_eq!(c0[1].competitor, CompetitorRef("node-1".into()));
        assert_eq!(c0[1].rssi, vec![120]);

        // The second tick advances each node's sample index by one period.
        let c1 = chunks(&t1);
        assert_eq!(c1[0].from, SourceTime::from_micros(100_000));
        assert_eq!(c1[0].rssi, vec![151]);

        // DONE stops capture.
        let done = adapter.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::DONE,
            race_heat_id: Some(0),
        }));
        assert!(chunks(&done).is_empty());
        let after = adapter.translate(Raw::NodeData(RawNodeData {
            pass_peak_rssi: vec![70.0, 60.0],
            node_peak_rssi: vec![70.0, 60.0],
        }));
        assert!(chunks(&after).is_empty(), "no trace after the race ends");
    }

    #[test]
    fn node_data_trace_falls_back_to_pass_peak_when_node_peak_absent() {
        let mut adapter = RotorHazardAdapter::new();
        adapter.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::RACING,
            race_heat_id: Some(0),
        }));
        // An older payload with no node_peak_rssi: the trace samples pass_peak_rssi instead.
        let t = adapter.translate(Raw::NodeData(RawNodeData {
            pass_peak_rssi: vec![88.0],
            node_peak_rssi: vec![],
        }));
        let c = chunks(&t);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].rssi, vec![88]);
    }

    #[test]
    fn race_restart_resets_the_trace_time_base() {
        let mut adapter = RotorHazardAdapter::new();
        adapter.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::RACING,
            race_heat_id: Some(1),
        }));
        adapter.translate(Raw::NodeData(RawNodeData {
            pass_peak_rssi: vec![100.0],
            node_peak_rssi: vec![100.0],
        }));
        adapter.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::DONE,
            race_heat_id: Some(1),
        }));
        // A new heat resets the per-node sample index back to 0.
        adapter.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::RACING,
            race_heat_id: Some(2),
        }));
        let t = adapter.translate(Raw::NodeData(RawNodeData {
            pass_peak_rssi: vec![100.0],
            node_peak_rssi: vec![100.0],
        }));
        assert_eq!(chunks(&t)[0].from, SourceTime::from_micros(0));
    }

    #[test]
    fn enter_exit_levels_emit_thresholds_once_until_changed() {
        let mut adapter = RotorHazardAdapter::new();
        let first = adapter.translate(Raw::EnterExitLevels(RawEnterExitLevels {
            enter_at_levels: vec![90.0, 92.0],
            exit_at_levels: vec![80.0, 82.0],
        }));
        let thresholds: Vec<&SignalThresholds> = first
            .iter()
            .filter_map(|e| match e {
                Event::SignalThresholds(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(thresholds.len(), 2);
        assert_eq!((thresholds[0].enter, thresholds[0].exit), (90, 80));

        // An unchanged re-send emits nothing.
        let resent = adapter.translate(Raw::EnterExitLevels(RawEnterExitLevels {
            enter_at_levels: vec![90.0, 92.0],
            exit_at_levels: vec![80.0, 82.0],
        }));
        assert!(
            !resent
                .iter()
                .any(|e| matches!(e, Event::SignalThresholds(_))),
            "steady thresholds are not re-emitted"
        );

        // A changed value re-emits for that node only.
        let changed = adapter.translate(Raw::EnterExitLevels(RawEnterExitLevels {
            enter_at_levels: vec![90.0, 95.0],
            exit_at_levels: vec![80.0, 82.0],
        }));
        let changed_t: Vec<&SignalThresholds> = changed
            .iter()
            .filter_map(|e| match e {
                Event::SignalThresholds(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(changed_t.len(), 1);
        assert_eq!(changed_t[0].competitor, CompetitorRef("node-1".into()));
        assert_eq!((changed_t[0].enter, changed_t[0].exit), (95, 82));
    }

    #[test]
    fn recorded_session_captures_signal_trace() {
        // The recorded fixture drives RACING then several all-zero `node_data` ticks: the trace
        // captures one zero sample per node per tick while racing, anchored at source-time 0.
        let mut adapter = RotorHazardAdapter::with_id(AdapterId("rh".into()));
        let events = run(&mut adapter, parse(SESSION_FIXTURE));
        let view = gridfpv_projection::signal_trace(&events);
        // Three nodes appear in node_data; each gets a trace.
        assert_eq!(view.competitors.len(), 3);
        let node0 = view
            .competitors
            .iter()
            .find(|c| c.competitor.competitor.0 == "node-0")
            .expect("node-0 trace");
        // The fixture sends 4 node_data ticks while racing -> 4 samples, all zero (mock nodes).
        assert_eq!(node0.samples.len(), 4);
        assert!(node0.samples.iter().all(|&s| s == 0));
        assert_eq!(node0.from, Some(SourceTime::from_micros(0)));
        assert_eq!(node0.period_micros, DEFAULT_NODE_DATA_PERIOD_MICROS);
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

    /// Count the `Pass`es in a slice of events.
    fn pass_count(events: &[Event]) -> usize {
        events
            .iter()
            .filter(|e| matches!(e, Event::Pass(_)))
            .count()
    }

    /// The cross-heat regression (#105): over one persistent connection RotorHazard
    /// resets each node's `lap_number` to 0 at the start of every race, so heat 2's
    /// laps reuse heat 1's sequences. Resetting dedup on the RACING transition makes
    /// heat 2 ingest its laps; pre-fix all four were suppressed (0 passes).
    #[test]
    fn cross_heat_laps_are_not_deduped_against_the_previous_heat() {
        let mut adapter = RotorHazardAdapter::new();

        // Heat 1: RACING, then node-0 laps 0..=3.
        adapter.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::RACING,
            race_heat_id: Some(1),
        }));
        let heat1 = adapter.translate(snapshot(
            0,
            0,
            vec![
                lap(0, 1_000.0),
                lap(1, 2_000.0),
                lap(2, 3_000.0),
                lap(3, 4_000.0),
            ],
        ));
        assert_eq!(pass_count(&heat1), 4, "heat 1 emits all four laps");

        // Heat 1 finishes.
        adapter.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::DONE,
            race_heat_id: Some(1),
        }));

        // Heat 2: a fresh RACING transition — RH restarts lap_number at 0.
        adapter.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::RACING,
            race_heat_id: Some(2),
        }));
        let heat2 = adapter.translate(snapshot(
            0,
            0,
            vec![
                lap(0, 1_000.0),
                lap(1, 2_000.0),
                lap(2, 3_000.0),
                lap(3, 4_000.0),
            ],
        ));
        assert_eq!(
            pass_count(&heat2),
            4,
            "heat 2 must emit four fresh passes (pre-fix: 0 — all deduped against heat 1)"
        );
    }

    /// The #105 reconnect invariant still holds within a race: a re-sent `current_laps`
    /// snapshot with **no** status transition must remain deduped (the RACING reset only
    /// fires on a genuine new-race edge, not on a snapshot replay).
    #[test]
    fn resent_snapshot_within_a_race_is_still_deduped() {
        let mut adapter = RotorHazardAdapter::new();

        adapter.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::RACING,
            race_heat_id: Some(1),
        }));
        let laps = vec![
            lap(0, 1_000.0),
            lap(1, 2_000.0),
            lap(2, 3_000.0),
            lap(3, 4_000.0),
        ];
        let first = adapter.translate(snapshot(0, 0, laps.clone()));
        assert_eq!(pass_count(&first), 4, "first snapshot emits all four laps");

        // The same snapshot re-sent (e.g. a reconnect replays it) — no status edge.
        let resent = adapter.translate(snapshot(0, 0, laps));
        assert_eq!(
            pass_count(&resent),
            0,
            "a re-sent snapshot within the same race emits no new passes"
        );
    }

    /// The mid-race reconnect regression (#105): when the persistent driver's RH link drops and
    /// reconnects *during* a running heat, RotorHazard re-sends the full in-progress `current_laps`
    /// snapshot on the new socket — with `last_race_status` still `RACING`, so there is **no** status
    /// transition (no #156 reset). The fix persists the SAME adapter across the reconnect, so its
    /// dedup already holds those laps and the replay is suppressed (0 new passes). The old behavior —
    /// building a *fresh* adapter on every reconnection — is what this test encodes as the bug: a
    /// fresh adapter has an empty dedup and re-emits every in-progress lap, which the lap projection
    /// (no sequence dedup) turns into duplicate laps. We assert both: the persisted adapter dedups,
    /// the fresh adapter double-emits.
    #[test]
    fn mid_race_reconnect_with_persisted_adapter_does_not_double_count() {
        // A heat is racing; node-0 has run four in-progress laps.
        let mut adapter = RotorHazardAdapter::new();
        adapter.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::RACING,
            race_heat_id: Some(7),
        }));
        let in_progress = vec![
            lap(0, 1_000.0),
            lap(1, 2_000.0),
            lap(2, 3_000.0),
            lap(3, 4_000.0),
        ];
        let before = adapter.translate(snapshot(0, 0, in_progress.clone()));
        assert_eq!(
            pass_count(&before),
            4,
            "the four in-progress laps are ingested"
        );

        // The link drops and reconnects mid-race. The driver REUSES this adapter (the #105 fix:
        // `connect` takes it, `disconnect` returns it), so `last_race_status` is still RACING. On a
        // reconnect RH replays the full in-progress state: first the same `race_status=RACING` (NOT
        // a transition — `previous == Some(RACING)` — so no SessionStarted, no #156 dedup reset)...
        let replayed_status = adapter.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::RACING,
            race_heat_id: Some(7),
        }));
        assert!(
            replayed_status.is_empty(),
            "re-sent RACING after a mid-race reconnect is not a transition (no reset)"
        );
        // ...then the re-sent `current_laps` snapshot of the very same laps.
        let after = adapter.translate(snapshot(0, 0, in_progress.clone()));
        assert_eq!(
            pass_count(&after),
            0,
            "a mid-race reconnect's replayed snapshot must emit no new passes (no double-count)"
        );

        // Contrast — the OLD behavior the fix removes: a FRESH adapter (empty dedup) re-emits every
        // replayed lap as a duplicate. This is exactly the double-count the persisted adapter avoids.
        let mut fresh = RotorHazardAdapter::new();
        fresh.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::RACING,
            race_heat_id: Some(7),
        }));
        let fresh_after = fresh.translate(snapshot(0, 0, in_progress));
        assert_eq!(
            pass_count(&fresh_after),
            4,
            "a fresh adapter (old reconnect behavior) double-emits the in-progress laps"
        );
    }

    /// Collect the `SignalHistory` events in a slice.
    fn histories(events: &[Event]) -> Vec<&SignalHistory> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::SignalHistory(h) => Some(h),
                _ => None,
            })
            .collect()
    }

    /// Build a `current_marshal_data` payload with a race `start_time` and one seat.
    fn marshal_data(start_time: f64, seat: usize, times: &[f64], values: &[f64]) -> Raw {
        let mut seats = std::collections::BTreeMap::new();
        seats.insert(
            seat.to_string(),
            RawMarshalSeat {
                history_values: values.to_vec(),
                history_times: times.to_vec(),
            },
        );
        Raw::MarshalData(RawMarshalData {
            race: Some(RawMarshalRace { start_time }),
            seats,
        })
    }

    #[test]
    fn done_transition_flags_a_marshal_request() {
        let mut adapter = RotorHazardAdapter::new();
        adapter.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::RACING,
            race_heat_id: Some(1),
        }));
        // No request before the heat ends.
        assert!(!adapter.take_marshal_request());
        adapter.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::DONE,
            race_heat_id: Some(1),
        }));
        // The DONE edge flags exactly one request; draining clears it (a re-sent DONE won't re-flag).
        assert!(
            adapter.take_marshal_request(),
            "DONE flags a marshal request"
        );
        assert!(!adapter.take_marshal_request(), "the flag is one-shot");
        // A re-sent DONE (no transition) does not re-flag.
        adapter.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::DONE,
            race_heat_id: Some(1),
        }));
        assert!(
            !adapter.take_marshal_request(),
            "re-sent DONE is not a transition"
        );
    }

    #[test]
    fn marshal_data_emits_dense_history_race_relative_micros() {
        let mut adapter = RotorHazardAdapter::with_id(AdapterId("rh".into()));
        // start_time 10.0s; samples at 10.1/10.2/10.3s -> 100k/200k/300k µs race-relative.
        let events = adapter.translate(marshal_data(
            10.0,
            2,
            &[10.1, 10.2, 10.3],
            &[70.0, 150.0, 71.0],
        ));
        let hs = histories(&events);
        assert_eq!(hs.len(), 1);
        let h = hs[0];
        assert_eq!(h.competitor, CompetitorRef("node-2".into()));
        assert_eq!(h.adapter, AdapterId("rh".into()));
        assert_eq!(h.times, vec![100_000, 200_000, 300_000]);
        assert_eq!(h.rssi, vec![70, 150, 71]);
    }

    #[test]
    fn marshal_data_skips_empty_or_mismatched_seats() {
        let mut adapter = RotorHazardAdapter::new();
        // Empty history: no event. Mismatched lengths: take the common prefix.
        let empty = adapter.translate(marshal_data(0.0, 0, &[], &[]));
        assert!(histories(&empty).is_empty(), "an empty seat emits nothing");

        let mut seats = std::collections::BTreeMap::new();
        seats.insert(
            "0".to_string(),
            RawMarshalSeat {
                history_values: vec![70.0, 150.0, 71.0],
                // One fewer time than values: the common prefix (2) is used.
                history_times: vec![0.1, 0.2],
            },
        );
        let mismatched = adapter.translate(Raw::MarshalData(RawMarshalData {
            race: Some(RawMarshalRace { start_time: 0.0 }),
            seats,
        }));
        let h = histories(&mismatched);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].times.len(), 2);
        assert_eq!(h[0].rssi, vec![70, 150]);
    }

    #[test]
    fn marshal_data_clamps_negative_times_and_rssi_range() {
        let mut adapter = RotorHazardAdapter::new();
        // A sample fractionally before start_time clamps to 0; RSSI clamps into u16.
        let events = adapter.translate(marshal_data(5.0, 0, &[4.999_999, 5.5], &[-3.0, 70000.0]));
        let h = histories(&events);
        assert_eq!(h[0].times[0], 0, "a pre-start sample clamps to 0");
        assert_eq!(h[0].rssi, vec![0, u16::MAX]);
    }

    #[test]
    fn marshal_data_is_silent_without_signal_capability() {
        let mut adapter = RotorHazardAdapter::new();
        adapter.signal_capture = false;
        let events = adapter.translate(marshal_data(0.0, 0, &[0.1, 0.2], &[70.0, 150.0]));
        assert!(
            histories(&events).is_empty(),
            "a non-signal source emits no dense history"
        );
    }

    #[test]
    fn race_list_queues_pilotrace_requests() {
        let mut adapter = RotorHazardAdapter::new();
        let raw = r#"{"event":"race_list","heats":{"1":{"rounds":{"1":{"start_time":987741.3,
            "pilotraces":[{"pilotrace_id":1,"node_index":0},{"pilotrace_id":2,"node_index":1}]}}}}}"#;
        let parsed: Raw = serde_json::from_str(raw).expect("race_list parses");
        let events = adapter.translate(parsed);
        // race_list emits no canonical events; it queues the per-pilotrace pulls.
        assert!(events.is_empty());
        let reqs = adapter.take_pilotrace_requests();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].pilotrace_id, 1);
        assert_eq!(reqs[1].pilotrace_id, 2);
        // Draining clears the queue.
        assert!(adapter.take_pilotrace_requests().is_empty());
    }

    #[test]
    fn race_details_emits_dense_history_relative_to_race_list_start() {
        let mut adapter = RotorHazardAdapter::with_id(AdapterId("rh".into()));
        // First the race_list teaches node-0's race start (987741.0s); then the per-pilotrace history.
        let list = r#"{"event":"race_list","heats":{"1":{"rounds":{"1":{"start_time":987741.0,
            "pilotraces":[{"pilotrace_id":1,"node_index":0}]}}}}}"#;
        adapter.translate(serde_json::from_str::<Raw>(list).unwrap());
        // history_times/values arrive as JSON-ENCODED STRINGS (this RH build's `json.dumps`).
        let details = r#"{"event":"race_details","node_index":0,
            "history_values":"[70, 150, 71]",
            "history_times":"[987741.1, 987741.2, 987741.3]",
            "enter_at":90,"exit_at":80}"#;
        let events = adapter.translate(serde_json::from_str::<Raw>(details).unwrap());
        let h = histories(&events);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].competitor, CompetitorRef("node-0".into()));
        // Times are race-relative (minus the race_list start 987741.0): 0.1/0.2/0.3s -> µs.
        assert_eq!(h[0].times, vec![100_000, 200_000, 300_000]);
        assert_eq!(h[0].rssi, vec![70, 150, 71]);
        // Thresholds are refreshed from enter_at/exit_at.
        let t = events
            .iter()
            .find_map(|e| match e {
                Event::SignalThresholds(t) => Some(t),
                _ => None,
            })
            .expect("thresholds emitted");
        assert_eq!((t.enter, t.exit), (90, 80));
    }

    #[test]
    fn race_details_without_race_list_anchors_on_first_sample() {
        // No prior race_list (so no known start): the trace anchors on its first sample (-> 0).
        let mut adapter = RotorHazardAdapter::new();
        let details = r#"{"event":"race_details","node_index":2,
            "history_values":[120, 121, 122],
            "history_times":[5.5, 5.6, 5.7]}"#;
        let events = adapter.translate(serde_json::from_str::<Raw>(details).unwrap());
        let h = histories(&events);
        assert_eq!(h[0].competitor, CompetitorRef("node-2".into()));
        assert_eq!(h[0].times, vec![0, 100_000, 200_000]);
        assert_eq!(h[0].rssi, vec![120, 121, 122]);
    }

    #[test]
    fn race_restart_clears_pending_marshal_state() {
        // A new race must not carry stale pilotrace pulls / start times from the previous heat.
        let mut adapter = RotorHazardAdapter::new();
        let list = r#"{"event":"race_list","heats":{"1":{"rounds":{"1":{"start_time":1.0,
            "pilotraces":[{"pilotrace_id":9,"node_index":0}]}}}}}"#;
        adapter.translate(serde_json::from_str::<Raw>(list).unwrap());
        // A fresh RACING transition clears the queue.
        adapter.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::RACING,
            race_heat_id: Some(2),
        }));
        assert!(adapter.take_pilotrace_requests().is_empty());
        assert!(!adapter.take_marshal_request());
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
