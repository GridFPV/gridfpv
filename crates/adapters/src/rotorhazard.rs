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
//!   `lap_number`, `lap_time_stamp` (cumulative ms since race start, float), `splits`
//!   and `late_lap`. **The lap-time / deletion shape changed across RH versions:** on
//!   RH ≤ 4.0 the duration was `lap_raw` (ms) with `lap_time` a `"M:SS.mmm"` *string*
//!   and deleted laps pre-filtered server-side; on **RH 4.3+/4.4** `lap_time` is a
//!   *numeric* ms duration (the pretty string moved to `lap_time_formatted`) and laps
//!   now carry `source` and `deleted` inline. The adapter only reads `lap_number` and
//!   `lap_time_stamp` (both stable); it parses `lap_time` permissively (string *or*
//!   number) and **skips laps RotorHazard did not count** — the ones it flags `deleted`, and the
//!   ones it stops numbering (`lap_number: -1`, its late-lap path once a win condition or lap cap
//!   declared the seat finished; see [`RawLapNumber`]). Each is counted and the first of each named,
//!   so a lap an RD deleted in RotorHazard's own UI, a seat RotorHazard stopped counting, and a lap
//!   that never happened are three distinguishable things (#400, #406).
//!   It diffs each snapshot against what it has already emitted per node and emits a [`Pass`]
//!   only for the *new* laps.
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
//!
//! # Which source mints a pass (#389)
//!
//! Two streams can carry the same lap: RotorHazard's own `current_laps` snapshot and — on a
//! plugin-equipped timer — the plugin's `gridfpv_pass` broadcast. The source is an **explicit,
//! declared decision**, never a race:
//!
//! - The plugin is authoritative **only when it advertised the `live_pass` capability** in its
//!   `gridfpv_hello_ack` (the transport calls
//!   [`set_plugin_live_pass`](RotorHazardAdapter::set_plugin_live_pass)). The plugin earns that
//!   capability with a load-time self-check, so advertising it means "I have proven I can read a
//!   lap atom on this RH build".
//! - Otherwise `current_laps` is authoritative and `gridfpv_pass` broadcasts are **ignored**.
//! - When the plugin is authoritative, a lap that `current_laps` reports and the plugin never
//!   delivered triggers a **loud** liveness fallback: the adapter emits the lap from the snapshot,
//!   surfaces a warning ([`take_pass_warning`](RotorHazardAdapter::take_pass_warning)), and
//!   switches this race's pass source back to `current_laps` for good.
//!
//! - A lap held for the plugin is **never dropped by a change of source** (#400). Every exit from
//!   the holding pen mints it: the liveness fallback, the `DONE` edge, a (re)connect handshake
//!   re-declaring the capability, and the per-race reset on the `RACING` edge. `current_laps`
//!   reported the lap, so it happened; the dedup makes a late plugin delivery a no-op.
//!
//! Before #389 both paths simply shared the dedup and "whichever arrived first won". That is
//! timing-dependent — RotorHazard triggers `RACE_LAP_RECORDED` into a **spawned gevent greenlet**
//! and then calls `emit_current_laps()` inline, so which broadcast reaches the socket first is not
//! actually determined — and, worse, a bad plugin atom that won the race silently suppressed the
//! correct `current_laps` value with no way to notice. Explicit selection removes both.

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
    /// A `heat_data` response (`RHUI.emit_heat_data`), the list of configured heats with their ids.
    /// Emits no canonical events; the adapter records the heat ids so the transport can **select a
    /// savable heat** before staging (a heat must be current for RotorHazard to persist the run's
    /// dense history — `on_save_laps`/`emit_race_marshal_data` no-op while `current_heat` is None in
    /// the default practice mode). Exposed via [`take_heat_ids`](RotorHazardAdapter::take_heat_ids).
    HeatData(RawHeatData),
    /// A `pilot_data` response (`RHUI.emit_pilot_data`), the configured pilots with their ids. Emits
    /// no canonical events; the adapter records the ids so the transport can learn the id of a pilot
    /// it just created (`add_pilot`) — the newest (highest) id — to then assign it onto a heat seat
    /// when **seating** a heat's bound pilots before racing (the laps-attribute fix). Exposed via
    /// [`take_pilot_ids`](RotorHazardAdapter::take_pilot_ids).
    PilotData(RawPilotData),
    /// A `gridfpv_signal` broadcast from the **GridFPV RH plugin** (D16, Slice 2): live per-node
    /// signal pushed in-process — `current_rssi`, the enter/exit detection levels, and the dense
    /// `history_values`/`history_times` window — plus the race-start clock origin. The adapter folds
    /// it into the same canonical [`SignalThresholds`]/[`SignalHistory`] facts the post-race
    /// save-then-pull produced, but **live**, and (once seen) suppresses that pull entirely. Absent on
    /// a stock RH (no plugin), so the socket fallback still pulls. See [`RawGridSignal`].
    GridSignal(RawGridSignal),
    /// A `gridfpv_pass` broadcast from the GridFPV RH plugin (D16, Slice 3): the per-node pass atom
    /// emitted natively from `RACE_LAP_RECORDED`. Folds to a [`Pass`], deduped against the
    /// `current_laps` snapshot path on the per-node `lap_number`. See [`RawGridPass`].
    GridPass(RawGridPass),
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
    /// Per-node lap counter (RotorHazard `lap_number`). `0` is the holeshot — but RotorHazard
    /// also uses this field to say *recorded, not counted* (`-1`), so it is a
    /// [`RawLapNumber`], not a number. Only a [counted](RawLapNumber::counted) one becomes the
    /// pass `sequence` and the dedup key.
    pub lap_number: RawLapNumber,
    /// The lap duration in milliseconds (RotorHazard `lap_raw`). Advisory only — the
    /// engine derives laps from the pass stream — so it is carried for reference.
    /// Present on RH ≤ 4.0; **renamed to a numeric `lap_time`** on RH 4.3+/4.4 (see
    /// `lap_time` below), so this is `None` against current RotorHazard.
    #[serde(default)]
    pub lap_raw: Option<f64>,
    /// RotorHazard's lap-time field. **Wire-shape changed across RH versions:** a pretty
    /// `"M:SS.mmm"` *string* on RH ≤ 4.0, a *numeric* duration in ms on RH 4.3+/4.4
    /// (where the pretty string moved to `lap_time_formatted`). Advisory and unused — we
    /// type it as a permissive [`serde_json::Value`] so either shape (or its absence)
    /// deserializes cleanly. Lap timing is derived from `lap_time_stamp`, not this.
    #[serde(default)]
    pub lap_time: Option<serde_json::Value>,
    /// Crossing time in **cumulative milliseconds since race start** (RotorHazard
    /// `lap_time_stamp`, a float). Converted to microseconds for [`SourceTime`].
    pub lap_time_stamp: f64,
    /// Whether RotorHazard flagged this as a late lap (over the time limit). Advisory.
    #[serde(default)]
    pub late_lap: bool,
    /// Whether RotorHazard has **deleted** this lap. Absent on RH ≤ 4.0 (deleted laps
    /// were pre-filtered server-side → `None`); present on RH 4.3+/4.4, which may carry
    /// deleted laps inline. We skip `Some(true)` laps so a deletion never mints a pass.
    #[serde(default)]
    pub deleted: Option<bool>,
}

/// RotorHazard's per-node `lap_number` — which is **not always a lap count**.
///
/// A stock RotorHazard numbers a seat's crossings `0, 1, 2, …` (`0` is the holeshot). But once it
/// declares that seat finished — a win condition, or a lap cap — `RHRace.py` numbers every later
/// crossing **`-1`**: RotorHazard's own way of saying *recorded, but not counted*. It is a real
/// value on the wire, not drift.
///
/// Typing the field `u64` made that value fail serde, which failed the **whole `current_laps`
/// frame** — losing the valid laps sitting beside it and charging the loss to the malformed-frame
/// counter instead of the deleted/uncounted one. The diagnostic then said "schema drift" (a
/// plugin/RH version mismatch) where the truth was "RotorHazard stopped counting" — two very
/// different field fixes (#406).
///
/// Modelling the negative explicitly keeps the frame decodable **and** keeps a non-lap out of the
/// pass path by construction: [`counted`](Self::counted) is the only way to a lap number, so an
/// uncounted crossing cannot become a [`Pass`] by omission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "i64", into = "i64")]
pub enum RawLapNumber {
    /// A lap RotorHazard counted: its per-node monotonic number, `0` = holeshot.
    Counted(u64),
    /// A crossing RotorHazard recorded but did **not** count as a lap, carrying the value exactly
    /// as RotorHazard sent it (`-1` in every build we have seen). Never mints a [`Pass`].
    Uncounted(i64),
}

impl RawLapNumber {
    /// The lap's number when RotorHazard counted it; `None` for a crossing it recorded but did not
    /// count. The only route to a pass `sequence`/dedup key, so a `-1` cannot become one.
    pub fn counted(self) -> Option<u64> {
        match self {
            Self::Counted(number) => Some(number),
            Self::Uncounted(_) => None,
        }
    }

    /// The value exactly as RotorHazard sent it — for diagnostics, which should quote the timer
    /// rather than paraphrase it.
    pub fn raw(self) -> i64 {
        match self {
            // Round-trips exactly: `Counted` only ever holds a value that arrived as an `i64`.
            Self::Counted(number) => number as i64,
            Self::Uncounted(number) => number,
        }
    }
}

impl From<i64> for RawLapNumber {
    fn from(number: i64) -> Self {
        u64::try_from(number).map_or(Self::Uncounted(number), Self::Counted)
    }
}

impl From<RawLapNumber> for i64 {
    fn from(number: RawLapNumber) -> Self {
        number.raw()
    }
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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
    /// Per-node **current** nadir RSSI (the node's running noise floor). Read by the tune
    /// telemetry tap only (#355): `heartbeat` does not carry it, and it is what tells an RD
    /// whether an enter threshold is set above the noise or inside it.
    #[serde(default)]
    pub node_nadir_rssi: Vec<f32>,
    /// Per-node nadir RSSI of the most recent pass. Tune telemetry only (#355).
    #[serde(default)]
    pub pass_nadir_rssi: Vec<f32>,
    /// Per-node count of passes the detector has recorded (`debug_pass_count`). Tune telemetry
    /// only (#355) — the "did this gate see anything at all?" counter, which is the question a
    /// zero-lap heat leaves an RD unable to answer.
    #[serde(default)]
    pub debug_pass_count: Vec<i64>,
}

/// A RotorHazard `frequency_data` message (`RHUI.emit_frequency_data`) — **the node-count
/// discovery source** (#412).
///
/// RotorHazard publishes **no `num_nodes` scalar on the socket**. Verified against `RHUI.py` /
/// `server.py` on **v4.3.0** (read out of a running container) **and v4.4.0** (the tagged source):
/// `num_nodes` appears only as a server-side loop bound, in HTML template rendering, and on the
/// *HTTP* `/api/status` endpoint — never in a socket emit.
///
/// What it does publish is per-node payloads sized by it, and `emit_frequency_data` is the clearest
/// of them. On **both** versions, identically:
///
/// ```python
/// fdata = []
/// for idx in range(self._racecontext.race.num_nodes):
///     fdata.append({'band': ..., 'channel': ..., 'frequency': ...})
/// emit_payload = {'fdata': fdata}
/// ```
///
/// So `fdata.len() == num_nodes`, exactly. It is preferred over the alternatives because it is a
/// list of **dicts** rather than parallel scalar arrays (its length cannot be misread), and because
/// it arrives on demand via `load_data` at connect rather than waiting for a heartbeat.
///
/// Only the **length** is read — the band/channel/frequency values are RotorHazard's own tuning
/// config, which GridFPV neither reads back as truth nor stores (D27).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RawFrequencyData {
    /// One entry per node: `{band, channel, frequency}`.
    #[serde(default)]
    pub fdata: Vec<serde_json::Value>,
}

/// How many nodes a `frequency_data` payload says the timer has (#412), or `None` when the frame
/// says nothing usable.
///
/// An **empty `fdata` is `None`, not zero**: a frame that told us nothing must not be recorded as a
/// zero-node timer, which would cap every heat to no pilots.
pub fn reported_nodes_from_frequency_data(value: &serde_json::Value) -> Option<u32> {
    let parsed: RawFrequencyData = serde_json::from_value(value.clone()).ok()?;
    (!parsed.fdata.is_empty()).then_some(parsed.fdata.len() as u32)
}

/// How many nodes an `enter_and_exit_at_levels` payload says the timer has (#412) — the
/// **fallback** discovery source, for a timer that answers this `load_data` type but not
/// `frequency_data`.
///
/// `RHUI.emit_enter_and_exit_at_levels` slices both arrays `[:num_nodes]` explicitly, on v4.3.0 and
/// v4.4.0 alike, so the length is the node count. Empty is `None` for the same reason as above.
pub fn reported_nodes_from_levels(levels: &RawEnterExitLevels) -> Option<u32> {
    let len = levels
        .enter_at_levels
        .len()
        .max(levels.exit_at_levels.len());
    (len > 0).then_some(len as u32)
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

/// A RotorHazard `heat_data` response (`RHUI.emit_heat_data`): the configured heats.
///
/// Shape: `{ "heats": [ { "id": <heat_id>, "slots": [ { "id", "node_index", … }, … ], … }, … ] }`.
/// The transport pulls this (via `load_data { heat_data }`) so it can select a **savable** current
/// heat before staging — RH only persists a run's dense history for a saved heat
/// (`current_heat != HEAT_ID_NONE`) — and, when **seating** a heat's bound pilots, learn each
/// node's **slot id** (the `HeatNode` primary key `alter_heat` targets to assign a pilot to a node).
/// The adapter records each heat's id and its node→slot map; the rest is ignored here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawHeatData {
    /// The configured heats; each heat's `id` and per-node `slots` are read.
    #[serde(default)]
    pub heats: Vec<RawHeat>,
}

/// One heat in a `heat_data` response (see [`RawHeatData`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawHeat {
    /// The heat's id, used to select it as the current (savable) heat.
    pub id: i64,
    /// The heat's seats — one [`RawHeatSlot`] per node — used to assign a pilot to a node
    /// (`alter_heat { heat, slot_id, pilot }`) when seating the heat's bound pilots before racing.
    #[serde(default)]
    pub slots: Vec<RawHeatSlot>,
}

/// One seat (RotorHazard `HeatNode`) of a heat in a `heat_data` response (see [`RawHeat`]).
///
/// `alter_heat` assigns a pilot to a heat by **slot id** (the `HeatNode` primary key), not by node
/// index — so seating the heat's bound pilots reads each seat's `(node_index, id)` here and emits
/// `alter_heat { heat, slot_id: id, pilot }` for the seat at the bound node index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawHeatSlot {
    /// The slot's id (`HeatNode` primary key) — the `slot_id` `alter_heat` targets.
    pub id: i64,
    /// The node index this slot seats a pilot on (0-based). `None` for an unprogrammed slot.
    #[serde(default)]
    pub node_index: Option<usize>,
}

/// A RotorHazard `pilot_data` response (`RHUI.emit_pilot_data`): the configured pilots.
///
/// Shape: `{ "pilots": [ { "pilot_id": <id>, … }, … ] }`. The transport pulls this after creating a
/// pilot (`add_pilot`) so it can learn the new pilot's id — the **highest** id, the one just added —
/// to assign onto a heat seat when seating a heat's bound pilots. The rest of each pilot object is
/// ignored here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawPilotData {
    /// The configured pilots; only each pilot's `pilot_id` is read.
    #[serde(default)]
    pub pilots: Vec<RawPilotEntry>,
}

/// One pilot in a `pilot_data` response (see [`RawPilotData`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawPilotEntry {
    /// The pilot's id (`Pilot` primary key), used to assign the pilot onto a heat seat.
    pub pilot_id: i64,
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

/// A `gridfpv_signal` broadcast from the GridFPV RH plugin (see [`Raw::GridSignal`]).
///
/// Live per-node signal pushed in-process while a race runs. `race_start` is RotorHazard's
/// monotonic race origin in **seconds** (the same clock as each node's `history_times`), used to make
/// the dense history race-relative — exactly as the marshal-data path does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawGridSignal {
    /// The race-start origin in seconds (RotorHazard monotonic). `None` if no race is running.
    #[serde(default)]
    pub race_start: Option<f64>,
    /// Per-node signal snapshots.
    #[serde(default)]
    pub nodes: Vec<RawGridSignalNode>,
}

/// One node's entry in a [`RawGridSignal`] broadcast.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawGridSignalNode {
    /// Zero-based node/seat index.
    pub index: usize,
    /// The accumulator index this incremental slice starts at (S2.1): `0` means **replace** (a full
    /// snapshot — the first broadcast of a race, or the final flush); a value `== the current
    /// accumulated length` means **append** this slice. Anything else is an out-of-sync slice (a
    /// missed/duplicated tick) the adapter skips until the next replace. Defaults to `0` (replace).
    #[serde(default)]
    pub base: usize,
    /// The node's current live RSSI (advisory; the live coarse trace still comes from `node_data`).
    #[serde(default)]
    pub current_rssi: Option<f64>,
    /// The node's enter detection level (rising past this opens a pass).
    #[serde(default)]
    pub enter_at: Option<f64>,
    /// The node's exit detection level (falling below this closes a pass).
    #[serde(default)]
    pub exit_at: Option<f64>,
    /// The dense per-sample RSSI history window (parallel to `history_times`).
    #[serde(default, deserialize_with = "de_f64_history")]
    pub history_values: Vec<f64>,
    /// The dense per-sample timestamps in seconds (RotorHazard monotonic; parallel to
    /// `history_values`).
    #[serde(default, deserialize_with = "de_f64_history")]
    pub history_times: Vec<f64>,
}

/// A `gridfpv_pass` broadcast from the GridFPV RH plugin (see [`Raw::GridPass`]) — the per-node pass
/// atom the plugin emits natively from `RACE_LAP_RECORDED` (D16, Slice 3), attributed by node seat.
/// Folds to the same canonical [`Pass`] the `current_laps` snapshot does, deduped on the per-node
/// `lap_number`. Honoured **only** when the plugin advertised the `live_pass` capability — see the
/// [source-selection rules](self#which-source-mints-a-pass-389); a stock RH never sends one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawGridPass {
    /// Zero-based node/seat index the lap was recorded on.
    pub node_index: usize,
    /// Per-node lap counter (`0` is the holeshot) — the pass `sequence` + dedup key. The plugin
    /// forwards RotorHazard's own `lap.lap_number` verbatim, so a finished seat's *recorded but not
    /// counted* `-1` reaches this atom exactly as it reaches `current_laps`: same
    /// [`RawLapNumber`], same disposition, same frame-killing `u64` before #406.
    pub lap_number: RawLapNumber,
    /// Crossing time in cumulative milliseconds since race start (same unit as `current_laps`).
    pub lap_time_stamp: f64,
    /// The pass's peak RSSI, if RH reported one — becomes the [`Pass`]'s [`SignalContext`].
    #[serde(default)]
    pub peak_rssi: Option<f64>,
}

/// Which stream is currently authoritative for minting a [`Pass`] — see the
/// [source-selection rules](self#which-source-mints-a-pass-389). Reported by
/// [`RotorHazardAdapter::pass_source`] so the decision is inspectable rather than implied by
/// whatever happened to arrive first (#389).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassSource {
    /// The GridFPV RH plugin's native `gridfpv_pass` broadcast. Selected because the plugin
    /// advertised `live_pass`; `current_laps` is then a checked backstop, not a second source.
    Plugin,
    /// RotorHazard's own `current_laps` snapshot — the stock path. Selected when no plugin
    /// advertised `live_pass`, or after the liveness fallback fired for this race.
    CurrentLaps,
}

/// A lap seen in a `current_laps` snapshot that the authoritative plugin has not delivered *yet*.
///
/// Held for exactly one snapshot round before the fallback treats it as a confirmed miss: RH
/// dispatches the plugin's `RACE_LAP_RECORDED` handler on a spawned greenlet but emits
/// `current_laps` inline, so a snapshot legitimately arriving *before* the plugin's pass must not
/// be mistaken for a dead plugin. Everything needed to mint the pass later is kept here.
#[derive(Debug, Clone)]
struct PendingLap {
    /// How many `current_laps` snapshots have carried this lap while the plugin stayed silent.
    ///
    /// The grace is counted in **snapshots, not wall time**, and RotorHazard emits one snapshot per
    /// recorded lap — so when two seats cross inside one gevent scheduling window, seat A's lap
    /// appears in the snapshot from its own crossing *and* in the snapshot from B's, both before
    /// A's spawned `RACE_LAP_RECORDED` greenlet has run. Demoting on the second sighting therefore
    /// fired on a plugin that was working correctly. Requiring [`PLUGIN_GRACE_SNAPSHOTS`] absorbs a
    /// full field crossing together without meaningfully delaying a real miss.
    seen: u32,
    /// The lap's `lap_time_stamp` (cumulative ms since race start).
    lap_time_stamp: f64,
    /// The seat's pilot callsign where the snapshot carried one — used to name the seat in the
    /// fallback warning (and in `CompetitorSeen`) rather than leaking a raw `node-N` handle.
    callsign: Option<String>,
}

/// How many `current_laps` snapshots may carry a lap the plugin has not delivered before the
/// adapter calls it a confirmed miss and falls back. One snapshot lands per recorded lap, so this
/// must exceed the number of seats that can plausibly cross inside one RotorHazard scheduling
/// window — otherwise a full field crossing together looks like a broken plugin.
const PLUGIN_GRACE_SNAPSHOTS: u32 = 8;

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
    /// Whether the GridFPV RH plugin is pushing **live signal** (`gridfpv_signal`, Slice 2). Set on
    /// the first such broadcast and kept for the adapter's life (it persists across reconnects, like
    /// the dedup). While set, the dense trace arrives live, so the adapter **suppresses the post-race
    /// save-then-pull** ([`pending_marshal_request`](Self::pending_marshal_request) is not raised on
    /// the DONE edge). A stock RH never sends `gridfpv_signal`, so this stays `false` and the pull
    /// remains the fallback.
    live_signal_active: bool,
    /// Per-node accumulated dense trace (race-relative µs + RSSI), reset each race (S2.1). The plugin
    /// streams the dense history **incrementally** (only new samples each tick); the adapter appends
    /// them here (or replaces on a full snapshot) and emits the accumulated [`SignalHistory`] when it
    /// changes — so the wire stays small while the projection still gets the full trace.
    dense_accum: std::collections::HashMap<usize, (Vec<i64>, Vec<u16>)>,
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
    /// Configured RotorHazard heat ids learned from the most recent `heat_data`, drained by the
    /// transport via [`take_heat_ids`](Self::take_heat_ids) so it can select a **savable** current
    /// heat before staging (RH only persists a run's dense history for a saved heat). Empty until a
    /// `heat_data` is folded.
    pending_heat_ids: Vec<i64>,
    /// The per-node **slot ids** of each configured heat, learned from the most recent `heat_data`,
    /// drained by the transport via [`take_heat_slots`](Self::take_heat_slots). Seating a heat's
    /// bound pilots needs each node's slot id (the `HeatNode` PK `alter_heat` targets). Keyed by heat
    /// id → `(node_index → slot_id)`. Empty until a `heat_data` is folded.
    pending_heat_slots: std::collections::HashMap<i64, std::collections::HashMap<usize, i64>>,
    /// Configured RotorHazard pilot ids learned from the most recent `pilot_data`, drained by the
    /// transport via [`take_pilot_ids`](Self::take_pilot_ids) so it can learn the id of a pilot it
    /// just created (`add_pilot` — the highest id) to assign onto a heat seat when seating. Empty
    /// until a `pilot_data` is folded.
    pending_pilot_ids: Vec<i64>,
    /// Whether the connected GridFPV plugin advertised the **`live_pass`** capability (#389). Set
    /// by the transport from the `gridfpv_hello_ack`, and reset to `false` on every (re)connect so
    /// a timer whose plugin was removed degrades to `current_laps` instead of waiting forever for
    /// passes that will never come. `true` makes the plugin the authoritative pass source; `false`
    /// makes `gridfpv_pass` broadcasts inert.
    plugin_live_pass: bool,
    /// `(node_index, lap_number)` of every lap the plugin's `gridfpv_pass` delivered this race —
    /// the record of what the authoritative source actually produced, which is what lets a
    /// `current_laps` lap be recognised as a genuine plugin miss. Reset each race.
    plugin_passes: std::collections::HashSet<(usize, u64)>,
    /// Laps a `current_laps` snapshot reported that the authoritative plugin has not delivered yet,
    /// held one snapshot round (see [`PendingLap`]). Ordered so a flush emits deterministically by
    /// `(node, lap)`. Always empty unless the plugin is authoritative. Reset each race.
    pending_snapshot_laps: std::collections::BTreeMap<(usize, u64), PendingLap>,
    /// Set when the liveness fallback fires: the plugin advertised `live_pass` but `current_laps`
    /// showed a lap it never delivered. From then on (for this race) `current_laps` is
    /// authoritative and `gridfpv_pass` is ignored, so a plugin producing wrong atoms cannot
    /// re-poison the stream. Reset each race.
    pass_fallback_engaged: bool,
    /// The most recent pass-source warning, drained by the app via
    /// [`take_pass_warning`](Self::take_pass_warning) to surface it to the operator. #389's
    /// symptom was a *silent* degrade; this is what makes the next one self-announcing.
    pass_warning: Option<String>,
    /// Per-race diagnostics counters: passes minted from the plugin path, passes minted from the
    /// `current_laps` path, laps dropped by the dedup, and `gridfpv_pass` broadcasts ignored
    /// because the plugin is not the selected source. Logged as one line at each race end (#380
    /// makes that reachable in the field).
    counts: PassCounts,
    /// One-shot latch so an un-advertised plugin's `gridfpv_pass` flood logs once per adapter, not
    /// once per lap.
    warned_unadvertised_pass: bool,
    /// One-shot latch so a plugin whose dense slices stop lining up with the accumulator warns once
    /// per race, not once per broadcast (see [`translate_grid_signal`](Self::translate_grid_signal)).
    warned_dense_desync: bool,
    /// Every crossing this race that RotorHazard reported but did **not** count as a lap — the
    /// ones it flagged `deleted` and the ones it stopped numbering (`-1`) — already counted, so it
    /// is counted once. `current_laps` is a full snapshot, so a skipped crossing reappears in every
    /// later one; without this the counters would climb with the snapshot rate instead of with the
    /// skips. Shared by both pass paths (see [`LapKey`]) so a crossing the plugin forwarded and the
    /// snapshot repeated is one skip, not two. Reset each race.
    skipped_laps: std::collections::HashSet<(usize, LapKey)>,
    /// One-shot latch so a marshaled heat announces the *first* deleted lap and then just counts
    /// the rest — a deletion pass over a whole field is one operator action, not N incidents.
    /// Reset each race.
    warned_deleted_lap: bool,
    /// One-shot latch so a seat RotorHazard has stopped counting announces the *first* uncounted
    /// crossing and then just counts the rest — every later crossing of that heat carries `-1`, so
    /// this is one race-format fault, not N incidents (#406). Reset each race.
    warned_uncounted_lap: bool,
    /// One-shot latch so an undecodable socket frame (schema drift) says so once per race rather
    /// than once per frame — `node_data` alone arrives ~10 Hz, so a per-frame line would bury the
    /// log it is meant to make readable. Reset each race.
    warned_malformed_frame: bool,
    /// Per-seat pilot callsign, learned from `current_laps` (which carries each node's pilot, with
    /// or without laps, from the moment a heat is staged). It names the seat in
    /// [`Event::CompetitorSeen`] and in the fallback warning whichever stream mints the pass — the
    /// plugin's `gridfpv_pass` atom carries no pilot, and announcing a raw `node-N` where a
    /// callsign is known is the friendly-name leak the project rules forbid.
    seat_callsign: std::collections::HashMap<usize, String>,
}

/// Identity of one crossing inside a node's lap table, stable across snapshots *and* across
/// RotorHazard giving up on numbering.
///
/// A counted lap is identified by its lap number. An uncounted one cannot be: RotorHazard numbers
/// **every** crossing after a seat finishes `-1`, so the number identifies the whole tail rather
/// than a crossing — keying on it would count four lost crossings as one. `lap_time_stamp` is the
/// per-crossing identity RotorHazard does keep, and it is byte-identical in every re-send of the
/// snapshot (and in the plugin's atom for the same crossing), so its bits are the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum LapKey {
    /// A lap RotorHazard numbered: that number.
    Counted(u64),
    /// A crossing RotorHazard did not number: the raw bits of its `lap_time_stamp`.
    Uncounted(u64),
}

impl LapKey {
    /// The key for a lap with this number and crossing time.
    fn new(lap_number: RawLapNumber, lap_time_stamp: f64) -> Self {
        match lap_number.counted() {
            Some(number) => Self::Counted(number),
            None => Self::Uncounted(lap_time_stamp.to_bits()),
        }
    }
}

/// Per-race ingest counters for the pass path (#389 field diagnostics).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PassCounts {
    /// Passes minted from the plugin's `gridfpv_pass`.
    plugin: u64,
    /// Passes minted from the `current_laps` snapshot.
    snapshot: u64,
    /// Laps the [`Deduplicator`] suppressed (a re-sent snapshot, a reconnect replay, or the
    /// non-authoritative stream re-reporting a lap already emitted).
    deduped: u64,
    /// `gridfpv_pass` broadcasts discarded because the plugin is not the selected source.
    ignored_plugin: u64,
    /// Laps RotorHazard itself reports as **deleted** (`lap.deleted == true`) and this adapter
    /// therefore skips — counted once per `(node, lap)`, since the snapshot re-sends a deleted lap
    /// forever. Zero on a heat nobody marshaled; non-zero is the trace of an RD deleting a lap in
    /// RotorHazard's own UI, which used to leave none at all on the Grid side (#400).
    deleted: u64,
    /// Crossings RotorHazard reported with **no lap number** (`lap_number: -1` — *recorded, but
    /// not counted*) and this adapter therefore skips — counted once per crossing, keyed on its
    /// timestamp, because RotorHazard gives every one of them the same `-1`. Non-zero means the
    /// timer declared a seat finished and stopped counting its laps (a win condition or lap cap
    /// still in force — #403), which is a different fault from an RD deleting a lap by hand and a
    /// different fault again from schema drift. Before #406 this was invisible: the negative failed
    /// the whole frame and landed in `malformed_frames` instead.
    uncounted: u64,
    /// Socket frames for an event we *do* translate that could not be decoded — schema drift
    /// (a RotorHazard or plugin version we don't match) or a malformed payload. Reported by the
    /// transport via [`RotorHazardAdapter::note_malformed_frame`]. Non-zero means laps may be
    /// missing for a reason that is **not** a dead gate (#400).
    malformed_frames: u64,
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
            live_signal_active: false,
            dense_accum: std::collections::HashMap::new(),
            pending_pilotrace_requests: Vec::new(),
            pilotrace_start_time: std::collections::HashMap::new(),
            pending_heat_ids: Vec::new(),
            pending_heat_slots: std::collections::HashMap::new(),
            pending_pilot_ids: Vec::new(),
            // No plugin has spoken yet: `current_laps` is authoritative until one advertises
            // `live_pass` (#389).
            plugin_live_pass: false,
            plugin_passes: std::collections::HashSet::new(),
            pending_snapshot_laps: std::collections::BTreeMap::new(),
            pass_fallback_engaged: false,
            pass_warning: None,
            counts: PassCounts::default(),
            warned_unadvertised_pass: false,
            warned_dense_desync: false,
            skipped_laps: std::collections::HashSet::new(),
            warned_deleted_lap: false,
            warned_uncounted_lap: false,
            warned_malformed_frame: false,
            seat_callsign: std::collections::HashMap::new(),
        }
    }

    /// Declare whether the connected GridFPV plugin advertised the **`live_pass`** capability
    /// (#389) — the *explicit* pass-source selection.
    ///
    /// The transport calls this from the `gridfpv_hello_ack` handler, and calls it with `false` on
    /// every (re)connect before any handler can fire, so the decision always reflects the plugin
    /// actually in front of us. `true` ⇒ the plugin's `gridfpv_pass` mints the laps and
    /// `current_laps` is a checked backstop; `false` ⇒ `current_laps` mints the laps and
    /// `gridfpv_pass` is ignored.
    ///
    /// **Returns the passes this switch minted, and the caller must forward them** (#400). A
    /// source switch invalidates the in-flight *liveness* bookkeeping — whether the plugin is
    /// still on trial for a given lap — but not the laps themselves: anything held in
    /// [`pending_snapshot_laps`](Self::pending_snapshot_laps) was reported by RotorHazard's own
    /// `current_laps` and is real. Dropping it lost a recorded lap with no flush, no counter and
    /// no log line, and a mid-race reconnect (this runs on every handshake) was enough to trigger
    /// it. So the held laps are **flushed through the snapshot path** before the bookkeeping is
    /// cleared; the dedup makes a plugin that later delivers the same lap a no-op.
    #[must_use = "a source switch can mint held laps — forward them or they are lost (#400)"]
    pub fn set_plugin_live_pass(&mut self, advertised: bool) -> Vec<Event> {
        if self.plugin_live_pass != advertised {
            crate::diag!(
                "gridfpv: rotorhazard: pass source = {} (plugin `live_pass` capability {})",
                if advertised {
                    "GridFPV plugin (gridfpv_pass)"
                } else {
                    "RotorHazard current_laps"
                },
                if advertised {
                    "advertised"
                } else {
                    "not advertised"
                },
            );
        }
        self.plugin_live_pass = advertised;
        // A source switch invalidates the in-flight liveness bookkeeping, not the dedup: laps
        // already emitted stay emitted. The laps still *held* for the plugin are neither — mint
        // them here rather than drop them (#400); `current_laps` said they happened.
        let mut out = Vec::new();
        let held = self.pending_snapshot_laps.len();
        if held > 0 {
            let who = self.held_lap_seat_names();
            crate::diag!(
                "gridfpv: rotorhazard: pass source switched with {held} lap(s) still held for the \
                 plugin ({who}) — minting them from RotorHazard's own lap table rather than \
                 dropping them (#400)"
            );
        }
        self.flush_pending_snapshot_laps(&mut out);
        self.warned_unadvertised_pass = false;
        out
    }

    /// The seats named in the currently held [`PendingLap`]s, as a display string — the pilot
    /// callsign wherever one is known, the raw seat handle only as a last resort (project rule:
    /// a raw `node-N` never reaches an operator-facing line that has a friendly name).
    fn held_lap_seat_names(&self) -> String {
        let mut names: Vec<String> = self
            .pending_snapshot_laps
            .iter()
            .map(|(&(node_index, _), held)| self.seat_name(node_index, held.callsign.as_deref()))
            .collect();
        names.dedup();
        names.join(", ")
    }

    /// Display name for a node seat: the pass's own callsign, else the last one `current_laps`
    /// gave this seat, else the raw `node-N` handle as a last resort.
    fn seat_name(&self, node_index: usize, callsign: Option<&str>) -> String {
        callsign
            .map(|c| c.to_string())
            .or_else(|| self.seat_callsign.get(&node_index).cloned())
            .unwrap_or_else(|| seat_ref(node_index).0)
    }

    /// Whether the connected plugin advertised `live_pass`.
    pub fn plugin_live_pass(&self) -> bool {
        self.plugin_live_pass
    }

    /// The stream currently authoritative for minting passes — see [`PassSource`].
    pub fn pass_source(&self) -> PassSource {
        if self.plugin_live_pass && !self.pass_fallback_engaged {
            PassSource::Plugin
        } else {
            PassSource::CurrentLaps
        }
    }

    /// Take (and clear) the latest pass-source warning, if the liveness fallback fired.
    ///
    /// A `Some` means the plugin advertised `live_pass` but did not deliver laps RotorHazard
    /// reported, so the adapter fell back to `current_laps`. It is a **loud** fallback by design:
    /// #389 was undiagnosable precisely because the degrade was silent.
    pub fn take_pass_warning(&mut self) -> Option<String> {
        self.pass_warning.take()
    }

    /// Record that the transport received a frame for an event it *does* translate but could not
    /// decode — schema drift against this RotorHazard/plugin build, or a malformed payload (#400).
    ///
    /// The transport used to drop these on the floor (`.ok()`, no counter, no line), which made a
    /// plugin-version skew look exactly like a gate that stopped detecting: laps simply stopped
    /// arriving. It is counted per race and surfaced in the heat pass summary, and the **first**
    /// one per race is logged with the offending event and the decode error — enough to name a
    /// version mismatch in seconds, without a per-frame flood (`node_data` alone is ~10 Hz).
    pub fn note_malformed_frame(&mut self, event: &str, detail: &str) {
        self.counts.malformed_frames += 1;
        if !self.warned_malformed_frame {
            self.warned_malformed_frame = true;
            crate::diag!(
                "gridfpv: rotorhazard: WARNING — could not decode a `{event}` frame from the \
                 timer, so it was DROPPED: {detail}. This is schema drift (a RotorHazard or \
                 GridFPV-plugin version this build does not match), not a dead gate — laps can go \
                 missing while every node still detects. Further undecodable frames this heat are \
                 counted in the heat pass summary (#400)."
            );
        }
    }

    /// Take (and clear) the configured heat ids learned from the most recent `heat_data`.
    ///
    /// The transport calls this after feeding a `heat_data` payload: it selects one of the returned
    /// ids as the current (savable) heat (`set_current_heat`) so RotorHazard persists the run's dense
    /// history. Empty when no `heat_data` has been folded since the last drain.
    pub fn take_heat_ids(&mut self) -> Vec<i64> {
        std::mem::take(&mut self.pending_heat_ids)
    }

    /// Take (and clear) the per-node slot ids of each configured heat learned from the most recent
    /// `heat_data`. The transport calls this when **seating** a heat's bound pilots: it picks the
    /// freshest heat (the one it just `add_heat`ed) and reads each node's slot id to assign a pilot
    /// (`alter_heat { heat, slot_id, pilot }`). Empty when no `heat_data` has been folded.
    pub fn take_heat_slots(
        &mut self,
    ) -> std::collections::HashMap<i64, std::collections::HashMap<usize, i64>> {
        std::mem::take(&mut self.pending_heat_slots)
    }

    /// Take (and clear) the configured pilot ids learned from the most recent `pilot_data`. The
    /// transport calls this after creating a pilot (`add_pilot`) when seating a heat's bound pilots:
    /// the **highest** id is the pilot just added, to be assigned onto a heat seat. Empty when no
    /// `pilot_data` has been folded.
    pub fn take_pilot_ids(&mut self) -> Vec<i64> {
        std::mem::take(&mut self.pending_pilot_ids)
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

    /// Translate a `current_laps` snapshot.
    ///
    /// **When `current_laps` is the selected source** (the stock path — no plugin advertised
    /// `live_pass`, or the liveness fallback already fired) this emits a `Pass` for every lap the
    /// [`Deduplicator`] has not already accepted, keyed on the per-node `lap_number`. A node's
    /// first surfaced lap also announces the seat as [`Event::CompetitorSeen`], and passes are
    /// annotated with the cached per-node RSSI.
    ///
    /// **When the plugin is the selected source** the snapshot stops being a pass source and
    /// becomes the *check* on the authoritative one (#389): a lap the plugin already delivered is
    /// dropped, and one it has not is held for a single snapshot round ([`PendingLap`]) — because
    /// RH dispatches the plugin's handler on a spawned greenlet while emitting `current_laps`
    /// inline, so a snapshot arriving first is normal and is not evidence of a dead plugin. A lap
    /// still undelivered when the *next* snapshot repeats it is a confirmed miss: the fallback
    /// engages loudly and `current_laps` mints the laps for the rest of the race.
    fn translate_current_laps(&mut self, snapshot: RawCurrentLaps, out: &mut Vec<Event>) {
        for (node_index, node) in snapshot.current.node_index.into_iter().enumerate() {
            // Learn the seat's pilot from the snapshot even when it has no laps yet: it is the
            // only place RotorHazard names the seat, and the plugin's pass atom never does.
            // Mirror the latest snapshot exactly — including *forgetting* a seat that is no
            // longer seated — so a later heat can never be announced under a previous heat's
            // pilot. RotorHazard emits `current_laps` at staging, before any lap, so the names are
            // always current by the time a pass can arrive.
            let callsign = node.pilot.as_ref().and_then(|p| p.callsign.clone());
            match callsign.clone() {
                Some(callsign) => self.seat_callsign.insert(node_index, callsign),
                None => self.seat_callsign.remove(&node_index),
            };

            for lap in node.laps {
                // RotorHazard has two ways of saying "I recorded this crossing and it is not a
                // lap": it flags the lap `deleted`, or it stops numbering and sends `-1`. Neither
                // may mint a pass — but both are evidence, and skipping them silently is what made
                // a marshaled heat and a dead gate look identical (#400/#406). `minted_lap_number`
                // counts and names them; `None` means this crossing is not ours to mint.
                let Some(lap_number) =
                    self.minted_lap_number(node_index, &lap, callsign.as_deref())
                else {
                    continue;
                };
                let key = (node_index, lap_number);

                // --- the plugin is authoritative: this snapshot only checks it ---------------
                if self.pass_source() == PassSource::Plugin {
                    if self.plugin_passes.contains(&key) {
                        // The authoritative source already minted this lap. Expected on every
                        // snapshot after every lap, so counted rather than logged per-drop.
                        self.counts.deduped += 1;
                        continue;
                    }
                    if let std::collections::btree_map::Entry::Vacant(slot) =
                        self.pending_snapshot_laps.entry(key)
                    {
                        // First sighting — give the plugin its round to deliver.
                        slot.insert(PendingLap {
                            seen: 1,
                            lap_time_stamp: lap.lap_time_stamp,
                            callsign: callsign.clone(),
                        });
                        continue;
                    }
                    if let Some(pending) = self.pending_snapshot_laps.get_mut(&key) {
                        pending.seen += 1;
                        if pending.seen < PLUGIN_GRACE_SNAPSHOTS {
                            // Still inside the grace — the plugin's greenlet may simply not have run
                            // yet. Keep holding; `current_laps` remains the check, not the source.
                            continue;
                        }
                    }
                    // Second sighting with the plugin still silent: a confirmed miss.
                    self.engage_pass_fallback(node_index, lap_number, callsign.as_deref());
                    self.flush_pending_snapshot_laps(out);
                    // This lap was among the flushed pending set, so it is already out.
                    continue;
                }

                // --- `current_laps` is authoritative: mint the pass -------------------------
                self.emit_snapshot_pass(
                    node_index,
                    lap_number,
                    lap.lap_time_stamp,
                    callsign.as_deref(),
                    out,
                );
            }
        }
    }

    /// The lap number Grid will mint this `current_laps` lap under — or `None` when RotorHazard
    /// recorded the crossing without counting it as a lap, in which case the skip is counted here
    /// (once per crossing) and the first of each kind is named.
    ///
    /// Two dispositions, deliberately counted apart because they call for different field fixes:
    ///
    /// - **`deleted: true`** — usually an RD deleting a lap in RotorHazard's own UI (#400).
    /// - **an uncounted `lap_number`** (`-1`) — RotorHazard declared the seat finished and stopped
    ///   counting: a win condition or lap cap is still in force on the race format (#403). RH sets
    ///   *both* fields on such a crossing, so the number is checked first: `deleted` alone would
    ///   report an operator action where the truth is a race-format fault, and sending an RD to
    ///   look for a marshaling mistake is exactly the wrong-cause diagnosis #406 is about.
    fn minted_lap_number(
        &mut self,
        node_index: usize,
        lap: &RawLap,
        callsign: Option<&str>,
    ) -> Option<u64> {
        match lap.lap_number.counted() {
            Some(number) if lap.deleted != Some(true) => Some(number),
            counted => {
                // Counted once per crossing: `current_laps` is a full snapshot, so it re-sends
                // every skipped crossing in every later frame for the rest of the race.
                if self
                    .skipped_laps
                    .insert((node_index, LapKey::new(lap.lap_number, lap.lap_time_stamp)))
                {
                    if counted.is_none() {
                        self.note_uncounted_crossing(node_index, lap.lap_number, callsign);
                    } else {
                        self.counts.deleted += 1;
                        if !self.warned_deleted_lap {
                            self.warned_deleted_lap = true;
                            let who = self.seat_name(node_index, callsign);
                            crate::diag!(
                                "gridfpv: rotorhazard: RotorHazard reports lap {} for {who} as \
                                 DELETED — not minting a pass for it. Further deletions this heat \
                                 are counted in the heat pass summary (#400).",
                                lap.lap_number.raw(),
                            );
                        }
                    }
                }
                None
            }
        }
    }

    /// Count a crossing RotorHazard recorded but did not number (`-1`), and name the first one of
    /// the race. The caller has already established that this crossing has not been counted before.
    fn note_uncounted_crossing(
        &mut self,
        node_index: usize,
        lap_number: RawLapNumber,
        callsign: Option<&str>,
    ) {
        self.counts.uncounted += 1;
        if !self.warned_uncounted_lap {
            self.warned_uncounted_lap = true;
            let who = self.seat_name(node_index, callsign);
            crate::diag!(
                "gridfpv: rotorhazard: RotorHazard recorded a crossing for {who} but numbered it \
                 {} — it has STOPPED COUNTING laps for that seat, so this is not a lap and no pass \
                 is minted. That is the timer refereeing: a win condition or lap cap on the active \
                 race format declared the pilot finished, and RotorHazard numbers every later \
                 crossing -1 (#403). Fix the race format — Grid's own format neutralises all of it \
                 (#405). This is NOT a dead gate and NOT schema drift. Further uncounted crossings \
                 this heat are counted in the heat pass summary (#406).",
                lap_number.raw(),
            );
        }
    }

    /// Mint a [`Pass`] (and, for a seat's first, a [`Event::CompetitorSeen`]) from a `current_laps`
    /// lap. Runs through the shared [`Deduplicator`], so a re-sent snapshot or a reconnect replay
    /// is suppressed exactly as before.
    fn emit_snapshot_pass(
        &mut self,
        node_index: usize,
        lap_number: u64,
        lap_time_stamp: f64,
        callsign: Option<&str>,
        out: &mut Vec<Event>,
    ) {
        let competitor = seat_ref(node_index);
        let signal = self
            .pass_peak_rssi
            .get(&node_index)
            .map(|&rssi_peak| SignalContext {
                rssi_peak: Some(rssi_peak),
            });

        let pass = Pass {
            adapter: self.id.clone(),
            competitor: competitor.clone(),
            at: Self::lap_stamp_to_source_time(lap_time_stamp),
            // The per-node lap_number is the monotonic sequence: it orders passes and anchors
            // snapshot/reconnect dedup.
            sequence: Some(lap_number),
            // RotorHazard reports the lap gate only (single start/finish gate).
            gate: GateIndex::LAP,
            signal,
            // The adapter doesn't know the heat; the bridge sink stamps it at append.
            heat: None,
        };

        // A re-sent snapshot replays every lap; only accept genuinely new ones.
        if !self.dedup.observe(&pass) {
            self.counts.deduped += 1;
            return;
        }

        // First genuinely new lap for this seat implies the seat is active.
        if self.seen_seats.insert(node_index) {
            out.push(Event::CompetitorSeen {
                adapter: self.id.clone(),
                // Name the seat by its pilot where RotorHazard gave us one — the project rule is
                // that a raw `node-N` handle never reaches a surface that has a friendly name. The
                // seat ref stays the durable handle everything else keys on.
                competitor: callsign
                    .map(|c| c.to_string())
                    .or_else(|| self.seat_callsign.get(&node_index).cloned())
                    .map(CompetitorRef)
                    .unwrap_or_else(|| competitor.clone()),
            });
        }

        self.counts.snapshot += 1;
        out.push(Event::Pass(pass));
    }

    /// Engage the **loud** liveness fallback (#389): the plugin advertised `live_pass` but
    /// RotorHazard's own snapshot reports a lap it never delivered.
    ///
    /// Switches this race's [`PassSource`] back to `current_laps` for good — a plugin that missed
    /// a lap has already shown its stream is not trustworthy, and letting it keep minting could
    /// re-poison the dedup — and records a warning the app drains via
    /// [`take_pass_warning`](Self::take_pass_warning). Silent fallback is what made #389 cost a day
    /// of bisecting; this names it in seconds.
    fn engage_pass_fallback(&mut self, node_index: usize, lap_number: u64, callsign: Option<&str>) {
        self.pass_fallback_engaged = true;
        // Name the seat by its pilot where we know one; the raw `node-N` handle is the last resort.
        let who = self.seat_name(node_index, callsign);
        let warning = format!(
            "The timer's GridFPV plugin advertised live passes but never delivered lap {lap_number} \
             for {who}, which RotorHazard itself reports. Falling back to RotorHazard's own lap \
             table for the rest of this race — laps are still being recorded, but the plugin's \
             pass stream is not trustworthy on this timer (#389)."
        );
        crate::diag!("gridfpv: rotorhazard: WARNING — {warning}");
        self.pass_warning = Some(warning);
    }

    /// Emit every held [`PendingLap`] (deterministically, by `(node, lap)`) through the
    /// `current_laps` path. Called when the fallback engages, and again at race end so a lap held
    /// at the final snapshot is never lost.
    fn flush_pending_snapshot_laps(&mut self, out: &mut Vec<Event>) {
        let pending = std::mem::take(&mut self.pending_snapshot_laps);
        for ((node_index, lap_number), held) in pending {
            self.emit_snapshot_pass(
                node_index,
                lap_number,
                held.lap_time_stamp,
                held.callsign.as_deref(),
                out,
            );
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
                // Before any of that reset: a lap still held for the plugin belongs to the race
                // that is *ending*, and this edge is its last chance. The DONE path normally
                // flushes it first, but that is an ordering assumption across two code paths — an
                // aborted connection or a missed DONE would otherwise drop a recorded lap here as
                // silently as #400's source switch did. Flush against the OLD dedup (below it is
                // replaced), so the laps land in the previous session, before `SessionStarted`.
                if !self.pending_snapshot_laps.is_empty() {
                    let held = self.pending_snapshot_laps.len();
                    let who = self.held_lap_seat_names();
                    crate::diag!(
                        "gridfpv: rotorhazard: a new race started with {held} lap(s) still held \
                         for the plugin from the previous one ({who}) — that race never reached \
                         DONE; minting them now rather than dropping them (#400)"
                    );
                    self.flush_pending_snapshot_laps(out);
                }
                self.dedup = Deduplicator::new();
                self.seen_seats.clear();
                // Pass-source bookkeeping is per race too (#389): a new heat re-offers the plugin
                // the authoritative role, and last heat's delivered/held laps say nothing about
                // this one. The advertised capability itself (`plugin_live_pass`) persists — it is
                // a property of the connected plugin, refreshed on each (re)connect handshake.
                self.plugin_passes.clear();
                // Already emptied by the flush above; kept so the per-race reset stays complete
                // and obvious rather than depending on the flush having run.
                self.pending_snapshot_laps.clear();
                self.pass_fallback_engaged = false;
                self.counts = PassCounts::default();
                self.skipped_laps.clear();
                self.warned_deleted_lap = false;
                self.warned_uncounted_lap = false;
                self.warned_malformed_frame = false;
                // Marshaling Slice 1: a fresh race resets the trace's time base so each heat's
                // captured chunks start at source-time 0 — deterministic and heat-local.
                self.race_active = true;
                self.sample_index.clear();
                self.last_thresholds.clear();
                // A fresh race invalidates any stale marshal-pull state from the previous heat.
                self.pending_marshal_request = false;
                // The dense trace is per-heat: drop the previous heat's accumulator so the new heat
                // builds a fresh trace. (`live_signal_active` persists — once the plugin is
                // streaming, it streams every heat.)
                self.dense_accum.clear();
                self.warned_dense_desync = false;
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
                // A lap held for the plugin at the final snapshot has no next snapshot to confirm
                // it, so the race end is its deadline: fall back loudly and emit it rather than
                // lose it (#389).
                if !self.pending_snapshot_laps.is_empty() {
                    let held = self.pending_snapshot_laps.len();
                    let (node_index, lap_number) = *self
                        .pending_snapshot_laps
                        .keys()
                        .next()
                        .expect("non-empty pending laps");
                    let callsign = self
                        .pending_snapshot_laps
                        .values()
                        .next()
                        .and_then(|lap| lap.callsign.clone());
                    crate::diag!(
                        "gridfpv: rotorhazard: race ended with {held} lap(s) the `live_pass` \
                         plugin never delivered"
                    );
                    self.engage_pass_fallback(node_index, lap_number, callsign.as_deref());
                    self.flush_pending_snapshot_laps(out);
                }
                // One diagnostic line per heat: where the laps actually came from. #389 had no
                // field diagnostics at all, so "the plugin delivered 0 of 14" was unanswerable.
                // Skipped for a DONE that carried no laps at all (the status RotorHazard replays
                // on connect), which would otherwise print a line of zeros per reconnect.
                if self.counts != PassCounts::default() {
                    crate::diag!(
                        "gridfpv: rotorhazard: heat pass summary — source={:?}, plugin={}, \
                         current_laps={}, deduped={}, ignored_plugin_passes={}, \
                         rh_deleted_laps={}, rh_uncounted_laps={}, undecodable_frames={}",
                        self.pass_source(),
                        self.counts.plugin,
                        self.counts.snapshot,
                        self.counts.deduped,
                        self.counts.ignored_plugin,
                        self.counts.deleted,
                        self.counts.uncounted,
                        self.counts.malformed_frames,
                    );
                }
                // The heat just ended: a signal-capable adapter should now pull RotorHazard's dense
                // `current_marshal_data` history (the full-fidelity trace its marshal page reviews).
                // Record the intent for the transport to act on — the pure translator can't emit the
                // socket request itself. Only on a genuine RACING/STAGING -> DONE edge (this arm runs
                // once per transition), so a re-sent DONE on a reconnect does not re-request.
                // ...unless the GridFPV plugin is already pushing the dense trace live (Slice 2):
                // then the full-fidelity history has arrived in-band and the pull is redundant.
                if self.signal_capture && !self.live_signal_active {
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
            // A pulled history is the seat's whole run, so it replaces whatever the fold holds.
            base: 0,
        }));
    }

    /// Record the configured heat ids from a `heat_data` response for the transport to drain.
    ///
    /// Emits no canonical events — `heat_data` is a transport routing payload. The ids queue in
    /// [`pending_heat_ids`](Self::pending_heat_ids); the transport picks one to make current so the
    /// run is savable (the dense-history precondition). A re-sent `heat_data` rebuilds the list.
    fn translate_heat_data(&mut self, data: RawHeatData) {
        self.pending_heat_ids = data.heats.iter().map(|h| h.id).collect();
        self.pending_heat_slots = data
            .heats
            .into_iter()
            .map(|h| {
                let node_to_slot = h
                    .slots
                    .into_iter()
                    .filter_map(|s| s.node_index.map(|n| (n, s.id)))
                    .collect();
                (h.id, node_to_slot)
            })
            .collect();
    }

    /// Record the configured pilot ids from a `pilot_data` response for the transport to drain.
    ///
    /// Emits no canonical events — `pilot_data` is a transport routing payload. The ids queue in
    /// [`pending_pilot_ids`](Self::pending_pilot_ids); the transport reads the highest (the pilot it
    /// just `add_pilot`ed) to assign onto a heat seat when seating. A re-sent `pilot_data` rebuilds
    /// the list.
    fn translate_pilot_data(&mut self, data: RawPilotData) {
        self.pending_pilot_ids = data.pilots.into_iter().map(|p| p.pilot_id).collect();
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
            self.emit_threshold(
                node_index,
                levels.enter_at_levels[node_index] as f64,
                levels.exit_at_levels[node_index] as f64,
                out,
            );
        }
    }

    /// Emit a [`SignalThresholds`] for one seat, deduped against the last `(enter, exit)` seen so a
    /// re-sent (unchanged) level does not re-emit. Shared by the `enter_and_exit_at_levels` path and
    /// the plugin's `gridfpv_signal` path.
    fn emit_threshold(&mut self, node_index: usize, enter: f64, exit: f64, out: &mut Vec<Event>) {
        let enter = enter.round().clamp(0.0, u16::MAX as f64) as u16;
        let exit = exit.round().clamp(0.0, u16::MAX as f64) as u16;
        if self.last_thresholds.get(&node_index) == Some(&(enter, exit)) {
            return;
        }
        self.last_thresholds.insert(node_index, (enter, exit));
        out.push(Event::SignalThresholds(SignalThresholds {
            adapter: self.id.clone(),
            competitor: seat_ref(node_index),
            enter,
            exit,
        }));
    }

    /// Fold a `gridfpv_signal` broadcast from the GridFPV RH plugin (D16, Slice 2) into canonical
    /// signal facts — the **live** equivalent of the post-race save-then-pull. Per node it refreshes
    /// the detection [`SignalThresholds`] and, when the dense history has **grown**, emits an updated
    /// dense [`SignalHistory`] (the same full-fidelity trace the marshal-data path produces, made
    /// race-relative via `race_start`). Seeing any broadcast marks [`live_signal_active`], which
    /// suppresses the redundant post-race pull on the DONE edge.
    ///
    /// # The emitted event carries the **slice**, not the accumulator (#392)
    ///
    /// The accumulator is the adapter's own working state — what it needs to recognise the next
    /// slice as contiguous. The event carries only the new samples, stamped with the
    /// [`base`](SignalHistory::base) offset they belong at, and the projection folds them the same
    /// way this function does: replace at `0`, append at the current length, skip anything else.
    ///
    /// A `base == 0` snapshot — the plugin's opening tick and its end-of-race flush — is always
    /// passed on, even when it only restates what the slices already delivered. That snapshot is the
    /// stream's resync point, and it is worth nothing unless it is in the *log*: a heat window that
    /// missed a slice can recover from a snapshot inside it and from nothing else. It costs one O(n)
    /// event per seat per heat, which is not per-tick cost.
    ///
    /// This is the whole of #392. Emitting the accumulated whole on every tick cost O(n) per tick
    /// and O(n^2) per heat per seat: at the plugin's 2 Hz the heat's log took two copies of the
    /// race-to-date trace every second, which woke `/stream` with an unchanged projection twice a
    /// second (the console replaying the last lap) and saturated the single `rust_socketio` callback
    /// thread that also parses `current_laps`. **The invariant: one tick's cost must not grow with
    /// heat length.** The live coarse trace still comes from `node_data` until the first dense
    /// history supersedes it in the projection.
    fn translate_grid_signal(&mut self, sig: RawGridSignal, out: &mut Vec<Event>) {
        if !self.signal_capture {
            return;
        }
        self.live_signal_active = true;
        for node in sig.nodes {
            let node_index = node.index;
            if let (Some(enter), Some(exit)) = (node.enter_at, node.exit_at) {
                self.emit_threshold(node_index, enter, exit, out);
            }
            // Dense history (S2.1, incremental): convert this slice to race-relative µs and apply it
            // to the per-node accumulator — REPLACE on a full snapshot (`base == 0`), APPEND when it
            // continues the accumulator (`base == len`), else skip an out-of-sync slice. What goes
            // on the wire is the slice that was applied, at its offset — never the accumulator
            // (#392) — and only when it actually changed something, so a redundant final flush of an
            // already-complete trace stays a no-op.
            let Some(start) = sig.race_start else {
                continue;
            };
            let n = node.history_values.len().min(node.history_times.len());
            let mut times = Vec::with_capacity(n);
            let mut rssi = Vec::with_capacity(n);
            for i in 0..n {
                let rel_secs = node.history_times[i] - start;
                times.push((rel_secs * 1_000_000.0).round().max(0.0) as i64);
                rssi.push(node.history_values[i].round().clamp(0.0, u16::MAX as f64) as u16);
            }
            let acc = self.dense_accum.entry(node_index).or_default();
            let emit = if node.base == 0 {
                // A full snapshot: the plugin's first tick of a race, and its end-of-race flush.
                // Replace the accumulator and put it on the wire whenever it carries samples —
                // including a flush that only restates what the slices already delivered.
                //
                // A delta stream is worth exactly what its resync points are worth, and the resync
                // point has to be **in the log**. The accumulator says what the *adapter* took; the
                // heat's log is a separate, durable artifact, and a fold over a window that missed
                // any slice can only recover from a snapshot inside that window. So the flush always
                // lands: one O(n) event per seat per heat, never per tick — which is the invariant
                // that matters, and what makes the marshaling trace self-sufficient (#392).
                *acc = (times.clone(), rssi.clone());
                (!times.is_empty()).then_some((0, times, rssi))
            } else if node.base == acc.0.len() && n > 0 {
                // The hot path: a contiguous slice. Extend the accumulator and emit *just the new
                // samples*, at the offset they start from — O(slice), never O(trace).
                let base = acc.0.len() as u64;
                acc.0.extend_from_slice(&times);
                acc.1.extend_from_slice(&rssi);
                Some((base, times, rssi))
            } else {
                // Out of sync: the slice neither restates the trace nor continues it, so applying it
                // would splice a gap or a duplicate into the marshaling evidence. Drop it and wait
                // for the next full snapshot to resync — but say so, because a plugin that keeps
                // missing leaves the live trace running on the end-of-race flush alone.
                if !self.warned_dense_desync {
                    self.warned_dense_desync = true;
                    crate::diag!(
                        "gridfpv: rotorhazard: the plugin's dense signal slice for seat {} starts \
                         at sample {} but this seat's trace holds {} — skipping it until a full \
                         snapshot resyncs the trace (#392)",
                        node_index,
                        node.base,
                        acc.0.len(),
                    );
                }
                None
            };
            if let Some((base, times, rssi)) = emit {
                out.push(Event::SignalHistory(SignalHistory {
                    adapter: self.id.clone(),
                    competitor: seat_ref(node_index),
                    times,
                    rssi,
                    base,
                }));
            }
        }
    }

    /// Fold a `gridfpv_pass` broadcast (D16, Slice 3) into a canonical [`Pass`], attributed by node
    /// seat — **only when the plugin is the selected pass source** (#389).
    ///
    /// The plugin is selected when it advertised `live_pass` and the liveness fallback has not
    /// fired this race; otherwise the broadcast is inert and `current_laps` mints the lap. That is
    /// the whole of the source decision: no arrival-order race, and a plugin that never earned the
    /// capability cannot pre-empt the stock path. Deduped on `(competitor, sequence=lap_number)`
    /// like the snapshot path, so a lap already emitted is never double-counted, and a seat's first
    /// surfaced pass announces it as [`Event::CompetitorSeen`].
    fn translate_grid_pass(&mut self, p: RawGridPass, out: &mut Vec<Event>) {
        if !self.plugin_live_pass {
            self.counts.ignored_plugin += 1;
            if !self.warned_unadvertised_pass {
                self.warned_unadvertised_pass = true;
                crate::diag!(
                    "gridfpv: rotorhazard: ignoring `gridfpv_pass` broadcasts — the timer's \
                     GridFPV plugin did not advertise the `live_pass` capability, so RotorHazard's \
                     own lap table is the pass source (#389)"
                );
            }
            return;
        }
        if self.pass_fallback_engaged {
            // The fallback already ruled this stream untrustworthy for the rest of the race.
            self.counts.ignored_plugin += 1;
            return;
        }

        let node_index = p.node_index;
        // The plugin forwards RotorHazard's lap number verbatim, so a seat RotorHazard has
        // declared finished reaches us here as `-1` too — *recorded, but not counted*. It is not a
        // lap and must not become a pass; count it and move on. Keyed on the crossing time in the
        // same set the snapshot path uses, so the snapshot repeating this crossing (it will, for
        // the rest of the race) is the same skip, counted once.
        let Some(lap_number) = p.lap_number.counted() else {
            if self
                .skipped_laps
                .insert((node_index, LapKey::new(p.lap_number, p.lap_time_stamp)))
            {
                self.note_uncounted_crossing(node_index, p.lap_number, None);
            }
            return;
        };
        let competitor = seat_ref(node_index);
        // Record what the authoritative source actually produced — this is what lets a
        // `current_laps` lap be recognised as a genuine miss rather than a duplicate.
        self.plugin_passes.insert((node_index, lap_number));
        self.pending_snapshot_laps.remove(&(node_index, lap_number));
        let signal = p.peak_rssi.map(|rssi| SignalContext {
            rssi_peak: Some(rssi as f32),
        });
        let pass = Pass {
            adapter: self.id.clone(),
            competitor: competitor.clone(),
            at: Self::lap_stamp_to_source_time(p.lap_time_stamp),
            sequence: Some(lap_number),
            gate: GateIndex::LAP,
            signal,
            // The adapter doesn't know the heat; the bridge sink stamps it at append.
            heat: None,
        };
        if !self.dedup.observe(&pass) {
            self.counts.deduped += 1;
            return;
        }
        if self.seen_seats.insert(node_index) {
            out.push(Event::CompetitorSeen {
                adapter: self.id.clone(),
                // Name the seat by its pilot where the snapshot told us one — the same handle the
                // `current_laps` path announces, so the pass source cannot change how a seat is
                // introduced. The raw node handle is the last resort.
                competitor: self
                    .seat_callsign
                    .get(&node_index)
                    .map(|c| CompetitorRef(c.clone()))
                    .unwrap_or_else(|| competitor.clone()),
            });
        }
        self.counts.plugin += 1;
        out.push(Event::Pass(pass));
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
            Raw::HeatData(data) => self.translate_heat_data(data),
            Raw::PilotData(data) => self.translate_pilot_data(data),
            Raw::GridSignal(sig) => self.translate_grid_signal(sig, &mut out),
            Raw::GridPass(pass) => self.translate_grid_pass(pass, &mut out),
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

    // ── #412: discovering the node count from the wire ───────────────────────────────────

    /// A `frequency_data` payload exactly as `RHUI.emit_frequency_data` builds it — one `fdata`
    /// entry per node, `{band, channel, frequency}`. Identical on v4.3.0 and v4.4.0.
    fn frequency_data(nodes: usize) -> serde_json::Value {
        serde_json::json!({
            "fdata": (0..nodes)
                .map(|i| serde_json::json!({
                    "band": "R",
                    "channel": i + 1,
                    "frequency": 5658 + (i as i64) * 37,
                }))
                .collect::<Vec<_>>()
        })
    }

    #[test]
    fn frequency_data_states_the_timers_node_count() {
        // The bench case #412 was filed for: a real 4-node NuclearHazard. RotorHazard never says
        // "4" as a scalar anywhere on the socket — but `fdata` is exactly four entries long, on
        // both v4.3.0 and v4.4.0, because `emit_frequency_data` loops `range(race.num_nodes)`.
        assert_eq!(
            reported_nodes_from_frequency_data(&frequency_data(4)),
            Some(4)
        );
        // …and the 8-seat timer GridFPV used to assume everything was.
        assert_eq!(
            reported_nodes_from_frequency_data(&frequency_data(8)),
            Some(8)
        );
    }

    #[test]
    fn an_empty_or_unreadable_frequency_data_reports_nothing_rather_than_zero() {
        // "Zero nodes" is not a thing a timer can be: it would cap every heat to no pilots, which
        // is a worse failure than the one #412 fixes. A frame that says nothing must report
        // nothing, leaving GridFPV on its configured width.
        assert_eq!(
            reported_nodes_from_frequency_data(&serde_json::json!({ "fdata": [] })),
            None
        );
        assert_eq!(
            reported_nodes_from_frequency_data(&serde_json::json!({})),
            None
        );
        assert_eq!(
            reported_nodes_from_frequency_data(&serde_json::json!("not an object")),
            None
        );
    }

    #[test]
    fn enter_and_exit_at_levels_is_the_fallback_node_count() {
        // `RHUI.emit_enter_and_exit_at_levels` slices both arrays `[:num_nodes]` explicitly (v4.3.0
        // and v4.4.0 alike), so their length is the node count too — the fallback for a timer that
        // answers this `load_data` type but not `frequency_data`.
        let levels = RawEnterExitLevels {
            enter_at_levels: vec![90.0, 90.0, 90.0, 90.0],
            exit_at_levels: vec![80.0, 80.0, 80.0, 80.0],
        };
        assert_eq!(reported_nodes_from_levels(&levels), Some(4));
        assert_eq!(
            reported_nodes_from_levels(&RawEnterExitLevels {
                enter_at_levels: vec![],
                exit_at_levels: vec![],
            }),
            None
        );
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
            lap_number: RawLapNumber::Counted(lap_number),
            lap_raw: None,
            lap_time: None,
            lap_time_stamp,
            late_lap: false,
            deleted: None,
        }
    }

    /// A crossing RotorHazard recorded but did not count — the shape `RHRace.py` emits once a win
    /// condition or lap cap declares the seat finished: no lap number (`-1`), flagged late, and
    /// (on RH 4.3+/4.4, which carries them inline) `deleted`.
    fn uncounted_lap(lap_time_stamp: f64) -> RawLap {
        RawLap {
            lap_index: None,
            lap_number: RawLapNumber::Uncounted(-1),
            lap_raw: None,
            lap_time: None,
            lap_time_stamp,
            late_lap: true,
            deleted: Some(true),
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
                heat: None,
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
                heat: None,
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
                heat: None,
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
                heat: None,
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
        }));
        let t1 = adapter.translate(Raw::NodeData(RawNodeData {
            pass_peak_rssi: vec![151.0, 121.0],
            node_peak_rssi: vec![151.0, 121.0],
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
    fn heat_data_records_ids_for_the_transport_and_emits_no_events() {
        // `heat_data` is a transport routing payload: it mints no canonical events but records the
        // configured heat ids so the transport can select a savable current heat before staging
        // (the marshaling path-2 precondition).
        let mut adapter = RotorHazardAdapter::new();
        let events = adapter.translate(Raw::HeatData(RawHeatData {
            heats: vec![
                RawHeat {
                    id: 1,
                    slots: vec![],
                },
                RawHeat {
                    id: 4,
                    slots: vec![],
                },
                RawHeat {
                    id: 2,
                    slots: vec![],
                },
            ],
        }));
        assert!(events.is_empty(), "heat_data emits no canonical events");
        // The ids are exposed for the transport to drain, then cleared (one-shot per send).
        assert_eq!(adapter.take_heat_ids(), vec![1, 4, 2]);
        assert!(
            adapter.take_heat_ids().is_empty(),
            "draining clears the heat ids"
        );
    }

    #[test]
    fn heat_data_records_per_node_slot_ids_for_seating() {
        // Seating a heat's bound pilots needs each node's slot id (the `HeatNode` PK `alter_heat`
        // targets). The adapter records heat id → (node_index → slot_id) from `heat_data` for the
        // transport to drain when it seats.
        let mut adapter = RotorHazardAdapter::new();
        adapter.translate(Raw::HeatData(RawHeatData {
            heats: vec![RawHeat {
                id: 7,
                slots: vec![
                    RawHeatSlot {
                        id: 21,
                        node_index: Some(0),
                    },
                    RawHeatSlot {
                        id: 22,
                        node_index: Some(1),
                    },
                    // An unprogrammed slot (no node) is ignored.
                    RawHeatSlot {
                        id: 23,
                        node_index: None,
                    },
                ],
            }],
        }));
        let slots = adapter.take_heat_slots();
        let heat = slots.get(&7).expect("heat 7 slots recorded");
        assert_eq!(heat.get(&0), Some(&21), "node-0 maps to slot 21");
        assert_eq!(heat.get(&1), Some(&22), "node-1 maps to slot 22");
        assert_eq!(heat.len(), 2, "the unprogrammed (no-node) slot is dropped");
        assert!(
            adapter.take_heat_slots().is_empty(),
            "draining clears the heat slots"
        );
    }

    #[test]
    fn pilot_data_records_ids_for_seating_and_emits_no_events() {
        // `pilot_data` is a transport routing payload: it mints no canonical events but records the
        // configured pilot ids so the transport can learn the id of a pilot it just `add_pilot`ed
        // (the highest) to assign onto a heat seat when seating.
        let mut adapter = RotorHazardAdapter::new();
        let events = adapter.translate(Raw::PilotData(RawPilotData {
            pilots: vec![
                RawPilotEntry { pilot_id: 1 },
                RawPilotEntry { pilot_id: 5 },
                RawPilotEntry { pilot_id: 3 },
            ],
        }));
        assert!(events.is_empty(), "pilot_data emits no canonical events");
        // The transport reads the highest (the just-added pilot).
        assert_eq!(adapter.take_pilot_ids().into_iter().max(), Some(5));
        assert!(
            adapter.take_pilot_ids().is_empty(),
            "draining clears the pilot ids"
        );
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

    /// Build a `gridfpv_signal` broadcast (D16, S2) with one node's dense slice + thresholds. `base`
    /// is the accumulator index the slice starts at (0 = full snapshot/replace; `== len` = append).
    fn grid_signal(
        race_start: f64,
        node: usize,
        base: usize,
        enter: f64,
        exit: f64,
        times: &[f64],
        values: &[f64],
    ) -> Raw {
        Raw::GridSignal(RawGridSignal {
            race_start: Some(race_start),
            nodes: vec![RawGridSignalNode {
                index: node,
                base,
                current_rssi: values.last().copied(),
                enter_at: Some(enter),
                exit_at: Some(exit),
                history_values: values.to_vec(),
                history_times: times.to_vec(),
            }],
        })
    }

    fn thresholds(events: &[Event]) -> Vec<&SignalThresholds> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::SignalThresholds(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn grid_signal_emits_dense_history_and_thresholds() {
        let mut a = RotorHazardAdapter::with_id(AdapterId("rh".into()));
        // start 10.0s; samples 10.1/10.2/10.3s -> 100k/200k/300k µs race-relative (like marshal data).
        let events = a.translate(grid_signal(
            10.0,
            2,
            0,
            90.0,
            80.0,
            &[10.1, 10.2, 10.3],
            &[70.0, 150.0, 71.0],
        ));
        let hs = histories(&events);
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].competitor, CompetitorRef("node-2".into()));
        assert_eq!(hs[0].times, vec![100_000, 200_000, 300_000]);
        assert_eq!(hs[0].rssi, vec![70, 150, 71]);
        let th = thresholds(&events);
        assert_eq!(th.len(), 1);
        assert_eq!((th[0].enter, th[0].exit), (90, 80));
        assert_eq!(th[0].competitor, CompetitorRef("node-2".into()));
    }

    #[test]
    fn grid_signal_accumulates_incremental_slices() {
        let mut a = RotorHazardAdapter::new();
        // First broadcast (base 0 = full snapshot): emits the dense history + thresholds.
        let first = a.translate(grid_signal(
            0.0,
            0,
            0,
            90.0,
            80.0,
            &[0.1, 0.2],
            &[70.0, 150.0],
        ));
        assert_eq!(
            histories(&first).len(),
            1,
            "first snapshot emits a dense history"
        );
        assert_eq!(histories(&first)[0].rssi, vec![70, 150]);
        assert_eq!(
            thresholds(&first).len(),
            1,
            "first snapshot emits thresholds"
        );

        assert_eq!(histories(&first)[0].base, 0, "a snapshot lands at offset 0");

        // An APPEND slice (base == current length 2) extends the accumulator, and the emitted event
        // carries ONLY the new sample, stamped with the offset it belongs at (#392) — not the
        // accumulated trace. Unchanged thresholds are not re-emitted.
        let appended = a.translate(grid_signal(0.0, 0, 2, 90.0, 80.0, &[0.3], &[71.0]));
        assert_eq!(histories(&appended).len(), 1, "an append emits the slice");
        assert_eq!(histories(&appended)[0].rssi, vec![71]);
        assert_eq!(histories(&appended)[0].times, vec![300_000]);
        assert_eq!(
            histories(&appended)[0].base,
            2,
            "the slice carries the offset the fold must place it at"
        );
        assert!(
            thresholds(&appended).is_empty(),
            "unchanged thresholds are not re-emitted"
        );

        // A full snapshot restating the accumulated trace — the plugin's end-of-race flush — still
        // lands, at `base = 0`. It is the log's resync point, so suppressing it because the ADAPTER
        // already holds those samples would disarm the delta stream's only safety net (#392).
        let resent = a.translate(grid_signal(
            0.0,
            0,
            0,
            90.0,
            80.0,
            &[0.1, 0.2, 0.3],
            &[70.0, 150.0, 71.0],
        ));
        assert_eq!(
            histories(&resent).len(),
            1,
            "the end-of-race flush lands as a full snapshot"
        );
        assert_eq!(histories(&resent)[0].base, 0);
        assert_eq!(histories(&resent)[0].rssi, vec![70, 150, 71]);

        // An out-of-sync append (base != length, no replace) is skipped, not mis-appended.
        let desync = a.translate(grid_signal(0.0, 0, 99, 90.0, 80.0, &[9.9], &[123.0]));
        assert!(
            histories(&desync).is_empty(),
            "an out-of-sync slice is skipped"
        );

        // The slices the adapter emitted fold back into the whole trace — the accumulator's job is
        // to recognise contiguity, the projection's is to reassemble.
        let log: Vec<Event> = first.into_iter().chain(appended).chain(resent).collect();
        let trace = gridfpv_projection::signal_trace(&log)
            .competitor(&gridfpv_projection::CompetitorKey {
                adapter: AdapterId(DEFAULT_ADAPTER_ID.into()),
                competitor: CompetitorRef("node-0".into()),
            })
            .expect("node-0 trace")
            .clone();
        assert_eq!(trace.samples, vec![70, 150, 71]);
        assert_eq!(
            trace.times.as_deref(),
            Some([100_000, 200_000, 300_000].as_slice())
        );
    }

    #[test]
    fn a_long_heat_does_not_grow_the_emitted_history(/* #392 */) {
        // The regression that reached the field. The plugin broadcasts at 2 Hz for the whole heat,
        // so the adapter re-emitting its accumulated trace each tick cost O(n) per tick and O(n^2)
        // per heat, per seat — two copies of the race-to-date trace appended to the heat's log every
        // second, waking `/stream` with a projection that had nothing new in it (the console
        // repeating the last lap), and saturating the socket callback thread that also parses
        // `current_laps`. Every mock heat in the harness is ~10-20s, which is exactly why nothing
        // caught it; this is that heat made long, and cheap.
        //
        // THE INVARIANT: one tick's emitted payload must not grow with heat length.
        const TICKS: usize = 400; // 400 broadcasts at 2 Hz = a ~3.5 minute heat
        const PER_TICK: usize = 25; // 25 dense samples per broadcast (~50 Hz detector sampling)

        let mut a = RotorHazardAdapter::with_id(AdapterId("rh".into()));
        let mut log: Vec<Event> = Vec::new();
        let mut base = 0usize;
        for tick in 0..TICKS {
            let times: Vec<f64> = (0..PER_TICK).map(|i| (base + i) as f64 * 0.02).collect();
            let values: Vec<f64> = (0..PER_TICK)
                .map(|i| 70.0 + ((base + i) % 90) as f64)
                .collect();
            let events = a.translate(grid_signal(0.0, 0, base, 90.0, 80.0, &times, &values));
            let hs = histories(&events);
            assert_eq!(hs.len(), 1, "tick {tick} must emit exactly one history");
            assert_eq!(
                hs[0].rssi.len(),
                PER_TICK,
                "tick {tick} emitted {} samples for a {PER_TICK}-sample slice — the payload is \
                 growing with heat length (#392)",
                hs[0].rssi.len(),
            );
            assert_eq!(hs[0].times.len(), PER_TICK);
            assert_eq!(hs[0].base, base as u64);
            log.extend(events);
            base += PER_TICK;
        }

        // Linear in the heat's samples, not quadratic. Under the old behavior this sum was
        // TICKS*(TICKS+1)/2*PER_TICK ≈ 2,005,000 samples for the 10,000 the heat actually recorded.
        let emitted: usize = histories(&log).iter().map(|h| h.rssi.len()).sum();
        assert_eq!(
            emitted,
            TICKS * PER_TICK,
            "the log must carry each dense sample exactly once"
        );

        // ...and the trace the marshal reviews is still every one of those samples, in order.
        let trace = gridfpv_projection::signal_trace(&log)
            .competitor(&gridfpv_projection::CompetitorKey {
                adapter: AdapterId("rh".into()),
                competitor: CompetitorRef("node-0".into()),
            })
            .expect("node-0 trace")
            .clone();
        assert_eq!(trace.samples.len(), TICKS * PER_TICK);
        assert_eq!(trace.from, Some(SourceTime::from_micros(0)));
        assert_eq!(
            trace.times.as_ref().and_then(|t| t.last()).copied(),
            Some(((TICKS * PER_TICK - 1) as f64 * 0.02 * 1_000_000.0) as i64),
        );

        // The plugin's end-of-race flush (`base = 0`, the whole trace) guarantees a complete
        // marshaling trace however the live stream went. It lands as ONE full-trace event — the
        // resync point the log needs — which is O(n) once per heat, not the O(n)-per-tick this test
        // exists to keep out.
        let flush_times: Vec<f64> = (0..TICKS * PER_TICK).map(|i| i as f64 * 0.02).collect();
        let flush_values: Vec<f64> = (0..TICKS * PER_TICK)
            .map(|i| 70.0 + (i % 90) as f64)
            .collect();
        let flush = a.translate(grid_signal(
            0.0,
            0,
            0,
            90.0,
            80.0,
            &flush_times,
            &flush_values,
        ));
        assert_eq!(
            histories(&flush).len(),
            1,
            "the end-of-race flush must land as a full snapshot"
        );
        assert_eq!(histories(&flush)[0].base, 0);
        assert_eq!(histories(&flush)[0].rssi.len(), TICKS * PER_TICK);

        // The whole heat therefore costs the log two copies of the trace — the slices, plus the one
        // closing snapshot — where it used to cost TICKS/2 of them.
        log.extend(flush);
        let total: usize = histories(&log).iter().map(|h| h.rssi.len()).sum();
        assert_eq!(total, 2 * TICKS * PER_TICK);
    }

    #[test]
    fn live_signal_suppresses_the_marshal_pull_on_done() {
        let mut a = RotorHazardAdapter::new();
        a.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::RACING,
            race_heat_id: Some(1),
        }));
        // The plugin pushes live signal during the race...
        a.translate(grid_signal(0.0, 0, 0, 90.0, 80.0, &[0.1], &[150.0]));
        a.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::DONE,
            race_heat_id: Some(1),
        }));
        // ...so the dense trace is already in hand: the DONE edge must NOT request the post-race pull.
        assert!(
            !a.take_marshal_request(),
            "live plugin signal makes the post-race save-then-pull redundant"
        );
    }

    #[test]
    fn grid_pass_emits_pass_seen_and_dedups_with_current_laps() {
        let mut a = RotorHazardAdapter::with_id(AdapterId("rh".into()));
        // The plugin advertised `live_pass`, so it is the selected pass source (#389).
        advertise_live_pass(&mut a, true);
        a.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::RACING,
            race_heat_id: Some(1),
        }));
        // A native plugin pass for node 0, lap 1.
        let evs = a.translate(Raw::GridPass(RawGridPass {
            node_index: 0,
            lap_number: RawLapNumber::Counted(1),
            lap_time_stamp: 1500.0,
            peak_rssi: Some(180.0),
        }));
        let passes: Vec<_> = evs
            .iter()
            .filter_map(|e| {
                if let Event::Pass(p) = e {
                    Some(p)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(passes.len(), 1);
        assert_eq!(passes[0].competitor, CompetitorRef("node-0".into()));
        assert_eq!(passes[0].sequence, Some(1));
        assert_eq!(passes[0].at, SourceTime::from_micros(1_500_000));
        assert!(passes[0].signal.as_ref().unwrap().rssi_peak.is_some());
        assert!(
            evs.iter()
                .any(|e| matches!(e, Event::CompetitorSeen { .. })),
            "a seat's first pass announces it"
        );

        // The current_laps snapshot re-reporting the SAME lap is deduped — no double Pass.
        let snap = a.translate(snapshot(0, 0, vec![lap(1, 1500.0)]));
        let dup = snap.iter().filter(|e| matches!(e, Event::Pass(_))).count();
        assert_eq!(
            dup, 0,
            "current_laps re-pass of the same (node, lap) is deduped"
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

    // ---------------------------------------------------------------------------------
    // #389 — explicit pass-source selection. Before this, `gridfpv_pass` and `current_laps`
    // shared one dedup and "whichever arrived first won", so a bad plugin atom silently
    // suppressed the correct snapshot value and the timer recorded no laps at all.
    // ---------------------------------------------------------------------------------

    /// Count the `Pass` events in a batch.
    fn passes(events: &[Event]) -> usize {
        events
            .iter()
            .filter(|e| matches!(e, Event::Pass(_)))
            .count()
    }

    /// Start a race on an adapter, discarding the lifecycle events.
    fn start_race(a: &mut RotorHazardAdapter) {
        a.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::RACING,
            race_heat_id: Some(1),
        }));
    }

    /// Declare the plugin's `live_pass` capability, asserting the switch had nothing held to mint.
    /// That is the ordinary case; the #400 tests below drive the switch that *does* hold laps and
    /// use the returned events directly.
    fn advertise_live_pass(a: &mut RotorHazardAdapter, advertised: bool) {
        assert!(
            a.set_plugin_live_pass(advertised).is_empty(),
            "this switch was not holding any laps"
        );
    }

    fn grid_pass(node_index: usize, lap_number: u64, lap_time_stamp: f64) -> Raw {
        Raw::GridPass(RawGridPass {
            node_index,
            lap_number: RawLapNumber::Counted(lap_number),
            lap_time_stamp,
            peak_rssi: Some(180.0),
        })
    }

    /// Advertised `live_pass` ⇒ the plugin mints the lap and the `current_laps` snapshot that
    /// re-reports it does **not** double-count.
    #[test]
    fn advertised_plugin_is_the_pass_source_and_current_laps_does_not_double_count() {
        let mut a = RotorHazardAdapter::with_id(AdapterId("rh".into()));
        advertise_live_pass(&mut a, true);
        start_race(&mut a);
        assert_eq!(a.pass_source(), PassSource::Plugin);

        let from_plugin = a.translate(grid_pass(0, 1, 1500.0));
        assert_eq!(passes(&from_plugin), 1, "the plugin mints the lap");

        // RH re-reports the same lap in every subsequent snapshot, forever.
        let snap_one = a.translate(snapshot(0, 0, vec![lap(1, 1500.0)]));
        let snap_two = a.translate(snapshot(0, 0, vec![lap(1, 1500.0)]));
        assert_eq!(
            passes(&snap_one) + passes(&snap_two),
            0,
            "current_laps must not re-mint a lap the authoritative plugin delivered"
        );
        assert_eq!(a.pass_source(), PassSource::Plugin, "no fallback fired");
        assert!(a.take_pass_warning().is_none(), "nothing to warn about");
    }

    /// RotorHazard emits `current_laps` inline but dispatches the plugin's handler on a spawned
    /// greenlet, so the snapshot legitimately arrives FIRST. That must not read as a dead plugin:
    /// the lap is held one round, the plugin's pass lands, and exactly one pass is emitted.
    #[test]
    fn a_snapshot_arriving_before_the_plugin_pass_is_not_a_miss() {
        let mut a = RotorHazardAdapter::with_id(AdapterId("rh".into()));
        advertise_live_pass(&mut a, true);
        start_race(&mut a);

        let early = a.translate(snapshot(0, 0, vec![lap(0, 900.0)]));
        assert_eq!(passes(&early), 0, "held for the authoritative source");

        let late = a.translate(grid_pass(0, 0, 900.0));
        assert_eq!(passes(&late), 1, "the plugin's pass mints it");
        assert_eq!(a.pass_source(), PassSource::Plugin);
        assert!(
            a.take_pass_warning().is_none(),
            "an out-of-order arrival is not a plugin failure"
        );

        // And the next snapshot still does not double-count it.
        let again = a.translate(snapshot(0, 0, vec![lap(0, 900.0)]));
        assert_eq!(passes(&again), 0);
    }

    /// No `live_pass` capability ⇒ `current_laps` is the source and plugin passes are inert. This
    /// is the stock-RH path, and the safe degrade for a plugin whose self-check failed.
    #[test]
    fn without_the_capability_current_laps_is_the_pass_source() {
        let mut a = RotorHazardAdapter::with_id(AdapterId("rh".into()));
        start_race(&mut a);
        assert_eq!(a.pass_source(), PassSource::CurrentLaps);

        // A plugin broadcasting passes it never earned the right to mint is ignored — including a
        // degenerate zero-filled one, the shape that made #389 destructive.
        let ignored = a.translate(grid_pass(0, 0, 0.0));
        assert_eq!(passes(&ignored), 0, "an un-advertised plugin mints nothing");

        // ...and the snapshot path still works, unpoisoned.
        let snap = a.translate(snapshot(0, 0, vec![lap(0, 900.0), lap(1, 1500.0)]));
        assert_eq!(passes(&snap), 2, "current_laps mints both laps");
        let times: Vec<_> = snap
            .iter()
            .filter_map(|e| match e {
                Event::Pass(p) => Some(p.at),
                _ => None,
            })
            .collect();
        assert_eq!(
            times,
            vec![
                SourceTime::from_micros(900_000),
                SourceTime::from_micros(1_500_000)
            ],
            "the correct snapshot timestamps survive"
        );
    }

    /// Advertised but silent: the plugin claimed `live_pass` and never delivered. Once a lap has
    /// survived [`PLUGIN_GRACE_SNAPSHOTS`] snapshots undelivered it is a confirmed miss, and the
    /// fallback fires LOUDLY — emitting the laps from `current_laps` and surfacing a warning —
    /// instead of dropping them.
    ///
    /// The grace spans a whole field on purpose: one snapshot lands per recorded lap, so a field
    /// crossing together puts a lap in several snapshots before its own plugin greenlet runs.
    /// Confirming on the *second* sighting mistook that for a broken plugin.
    #[test]
    fn advertised_but_silent_plugin_falls_back_loudly() {
        let mut a = RotorHazardAdapter::with_id(AdapterId("rh".into()));
        advertise_live_pass(&mut a, true);
        start_race(&mut a);

        // Every snapshot inside the grace holds the lap rather than minting it.
        for n in 1..PLUGIN_GRACE_SNAPSHOTS {
            let held = a.translate(snapshot(0, 0, vec![lap(0, 900.0)]));
            assert_eq!(passes(&held), 0, "snapshot {n} is still within the grace");
        }

        // The grace is spent and lap 0 is still undelivered: a confirmed miss. This snapshot also
        // carries a new lap, so both come out through `current_laps`.
        let second = a.translate(snapshot(0, 0, vec![lap(0, 900.0), lap(1, 1500.0)]));
        assert_eq!(
            passes(&second),
            2,
            "the fallback emits the held lap AND the new one"
        );
        assert_eq!(
            a.pass_source(),
            PassSource::CurrentLaps,
            "the source switches back for the rest of the race"
        );
        let warning = a.take_pass_warning().expect("the fallback must be loud");
        assert!(
            warning.contains("live passes") && warning.contains("#389"),
            "the warning names the fault: {warning}"
        );

        // A plugin that starts talking again does not get to re-poison the stream this race.
        let late = a.translate(grid_pass(0, 2, 2100.0));
        assert_eq!(passes(&late), 0, "the demoted source mints nothing");
        // And laps keep flowing from the snapshot.
        let third = a.translate(snapshot(
            0,
            0,
            vec![lap(0, 900.0), lap(1, 1500.0), lap(2, 2100.0)],
        ));
        assert_eq!(passes(&third), 1, "lap 2 still lands, from current_laps");
    }

    /// A lap held for the plugin at the FINAL snapshot has no next snapshot to confirm it, so the
    /// race end is its deadline — it must be flushed, not lost.
    #[test]
    fn a_lap_held_for_the_plugin_is_flushed_at_race_end() {
        let mut a = RotorHazardAdapter::with_id(AdapterId("rh".into()));
        advertise_live_pass(&mut a, true);
        start_race(&mut a);

        assert_eq!(
            passes(&a.translate(snapshot(0, 0, vec![lap(3, 4200.0)]))),
            0
        );

        let done = a.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::DONE,
            race_heat_id: Some(1),
        }));
        assert_eq!(passes(&done), 1, "the held lap is emitted at race end");
        assert!(a.take_pass_warning().is_some(), "and it is announced");
    }

    /// The fallback is per race: a fresh heat re-offers the plugin the authoritative role.
    #[test]
    fn the_pass_fallback_resets_on_the_next_race() {
        let mut a = RotorHazardAdapter::with_id(AdapterId("rh".into()));
        advertise_live_pass(&mut a, true);
        start_race(&mut a);
        // Spend the grace so the fallback actually engages (see PLUGIN_GRACE_SNAPSHOTS).
        for _ in 0..PLUGIN_GRACE_SNAPSHOTS {
            a.translate(snapshot(0, 0, vec![lap(0, 900.0)]));
        }
        assert_eq!(a.pass_source(), PassSource::CurrentLaps);

        // Finish that heat and start the next one (the reset rides the RACING *transition*).
        a.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::DONE,
            race_heat_id: Some(1),
        }));
        a.take_pass_warning();
        a.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::RACING,
            race_heat_id: Some(2),
        }));
        assert_eq!(
            a.pass_source(),
            PassSource::Plugin,
            "a new heat starts the plugin back on trial"
        );
        assert_eq!(passes(&a.translate(grid_pass(0, 0, 900.0))), 1);
    }

    /// A reconnect against a timer whose plugin was uninstalled must not leave the adapter waiting
    /// for passes that can never come — the transport clears the capability and `current_laps`
    /// takes over immediately. The held lap comes *with* it (#400): the switch mints it.
    #[test]
    fn clearing_the_capability_returns_the_source_to_current_laps() {
        let mut a = RotorHazardAdapter::with_id(AdapterId("rh".into()));
        advertise_live_pass(&mut a, true);
        start_race(&mut a);
        assert_eq!(passes(&a.translate(snapshot(0, 0, vec![lap(0, 900.0)]))), 0);

        let carried = a.set_plugin_live_pass(false);
        assert_eq!(a.pass_source(), PassSource::CurrentLaps);
        assert_eq!(passes(&carried), 1, "the held lap is minted, not dropped");
        assert_eq!(
            passes(&a.translate(snapshot(0, 0, vec![lap(0, 900.0)]))),
            0,
            "and the snapshot that re-reports it does not double-count"
        );
    }

    /// #400: a **mid-race reconnect** while the plugin is the pass source calls
    /// `set_plugin_live_pass` on every handshake. Laps `current_laps` reported and the plugin had
    /// not yet delivered are sitting in `pending_snapshot_laps`; the switch used to `clear()` them
    /// — no flush, no counter, no line. RotorHazard recorded those laps, so the switch must mint
    /// them.
    #[test]
    fn a_source_switch_flushes_held_laps_instead_of_dropping_them() {
        let mut a = RotorHazardAdapter::with_id(AdapterId("rh".into()));
        advertise_live_pass(&mut a, true);
        start_race(&mut a);

        // Three laps across two seats land in `current_laps` while the plugin stays quiet — all
        // still inside the grace, so all held rather than emitted.
        assert_eq!(
            passes(&a.translate(snapshot(1, 0, vec![lap(0, 900.0), lap(1, 1500.0)]))),
            0
        );
        assert_eq!(passes(&a.translate(snapshot(1, 1, vec![lap(0, 950.0)]))), 0);

        // The reconnect handshake re-declares the capability — even unchanged (`true` again), the
        // in-flight liveness bookkeeping is reset. The laps must come out.
        let carried = a.set_plugin_live_pass(true);
        assert_eq!(
            passes(&carried),
            3,
            "every held lap is minted by the switch"
        );
        // Deterministically ordered by (node, lap), through the snapshot path.
        let minted: Vec<_> = carried
            .iter()
            .filter_map(|e| match e {
                Event::Pass(p) => Some((p.competitor.clone(), p.sequence, p.at)),
                _ => None,
            })
            .collect();
        assert_eq!(
            minted,
            vec![
                (
                    CompetitorRef("node-0".into()),
                    Some(0),
                    SourceTime::from_micros(900_000)
                ),
                (
                    CompetitorRef("node-0".into()),
                    Some(1),
                    SourceTime::from_micros(1_500_000)
                ),
                (
                    CompetitorRef("node-1".into()),
                    Some(0),
                    SourceTime::from_micros(950_000)
                ),
            ],
        );
        assert_eq!(a.counts.snapshot, 3, "and they are counted, not silent");

        // The plugin waking up and delivering the same laps is a no-op, not a double-count: the
        // dedup survives a source switch (only the liveness bookkeeping is invalidated).
        assert_eq!(passes(&a.translate(grid_pass(0, 0, 900.0))), 0);
        assert_eq!(
            passes(&a.translate(snapshot(1, 0, vec![lap(0, 900.0), lap(1, 1500.0)]))),
            0
        );
    }

    /// #400, the sibling clear: the per-race reset on the `RACING` edge drops the same map. It is
    /// *normally* unreachable with laps in it because the DONE path flushes first — but that is an
    /// ordering assumption across two code paths. Drive a race that never reaches DONE (an aborted
    /// connection, a missed status) straight into the next one: the held lap must still be minted,
    /// under the previous race, before the reset.
    #[test]
    fn the_race_transition_reset_cannot_lose_a_held_lap() {
        let mut a = RotorHazardAdapter::with_id(AdapterId("rh".into()));
        advertise_live_pass(&mut a, true);
        start_race(&mut a);
        assert_eq!(
            passes(&a.translate(snapshot(0, 0, vec![lap(2, 3300.0)]))),
            0
        );

        // No DONE — the race is abandoned (a stop, a dropped link) and the next one is staged
        // straight away. STAGING carries no lifecycle edge, so the reset lands on RACING.
        assert!(
            a.translate(Raw::RaceStatus(RawRaceStatus {
                race_status: race_status::STAGING,
                race_heat_id: Some(2),
            }))
            .is_empty(),
            "staging is not a lifecycle edge and must not touch the held lap"
        );
        let next = a.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::RACING,
            race_heat_id: Some(2),
        }));
        assert_eq!(passes(&next), 1, "the held lap is minted, not dropped");
        let lap_before_start = next
            .iter()
            .position(|e| matches!(e, Event::Pass(_)))
            .expect("a pass")
            < next
                .iter()
                .position(|e| matches!(e, Event::SessionStarted { .. }))
                .expect("the new session");
        assert!(
            lap_before_start,
            "it belongs to the race that ended, so it precedes the new SessionStarted"
        );

        // And the new race really did reset: the same lap number is a fresh lap now.
        assert_eq!(
            passes(&a.translate(snapshot(0, 0, vec![lap(2, 3300.0)]))),
            0,
            "held for the plugin again — a new heat puts it back on trial"
        );
        assert_eq!(a.pass_source(), PassSource::Plugin);
    }

    /// A lap RotorHazard itself reports as deleted still mints nothing — but it is now counted
    /// (once per lap, not once per snapshot) so a marshal's deletion leaves a trace on the Grid
    /// side instead of looking like a crossing that never happened (#400).
    #[test]
    fn a_deleted_lap_is_counted_not_silently_skipped() {
        let mut a = RotorHazardAdapter::with_id(AdapterId("rh".into()));
        start_race(&mut a);

        let mut deleted = lap(1, 1500.0);
        deleted.deleted = Some(true);
        let evs = a.translate(snapshot(0, 0, vec![lap(0, 900.0), deleted.clone()]));
        assert_eq!(passes(&evs), 1, "only the live lap mints a pass");
        assert_eq!(a.counts.deleted, 1, "the deleted lap is counted");

        // `current_laps` is a full snapshot, so the deleted lap comes back forever. The count is
        // per lap, not per frame.
        a.translate(snapshot(0, 0, vec![lap(0, 900.0), deleted.clone()]));
        a.translate(snapshot(0, 0, vec![lap(0, 900.0), deleted]));
        assert_eq!(a.counts.deleted, 1, "re-sends do not inflate the counter");

        // A second deletion is its own count.
        let mut also_deleted = lap(2, 2100.0);
        also_deleted.deleted = Some(true);
        a.translate(snapshot(0, 0, vec![lap(0, 900.0), also_deleted]));
        assert_eq!(a.counts.deleted, 2);

        // And it is per race, like every other pass counter.
        a.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::DONE,
            race_heat_id: Some(1),
        }));
        start_race(&mut a);
        assert_eq!(a.counts.deleted, 0, "the next heat starts clean");
    }

    /// RotorHazard's `-1` is a value, not drift: it decodes, and it round-trips back to the same
    /// integer so a recorded frame still replays byte-for-byte (#406).
    #[test]
    fn a_lap_number_carries_rotorhazards_own_value_either_way() {
        use serde_json::json;

        assert_eq!(
            serde_json::from_value::<RawLapNumber>(json!(0)).unwrap(),
            RawLapNumber::Counted(0),
            "0 is the holeshot, not a sentinel"
        );
        assert_eq!(
            serde_json::from_value::<RawLapNumber>(json!(-1)).unwrap(),
            RawLapNumber::Uncounted(-1),
            "a negative lap number decodes instead of failing its frame"
        );
        assert_eq!(
            serde_json::to_value(RawLapNumber::Counted(3)).unwrap(),
            json!(3)
        );
        assert_eq!(
            serde_json::to_value(RawLapNumber::Uncounted(-1)).unwrap(),
            json!(-1)
        );

        // Only a counted lap yields a number — the `-1` cannot reach a pass `sequence` by omission.
        assert_eq!(RawLapNumber::Counted(3).counted(), Some(3));
        assert_eq!(RawLapNumber::Uncounted(-1).counted(), None);
        assert_eq!(RawLapNumber::Uncounted(-1).raw(), -1);
    }

    /// #403's field shape: RotorHazard declared the seat finished and numbered every later crossing
    /// `-1`. The whole snapshot used to die on that negative — valid laps included — and the loss
    /// was charged to the malformed-frame counter, so the diagnostic said "schema drift" where the
    /// truth was "the timer stopped counting" (#406).
    #[test]
    fn an_uncounted_lap_does_not_take_its_frame_down_with_it() {
        // Decoded from the wire, not hand-built: the bug was in the *deserialisation*, so the test
        // has to start where RotorHazard does. This is the frame RH 4.4 sends with a lap-count win
        // condition in force — two counted laps, then a crossing it recorded and did not count.
        let frame: RawCurrentLaps = serde_json::from_value(serde_json::json!({
            "current": { "node_index": [{
                "pilot": { "callsign": "ZIP" },
                "laps": [
                    { "lap_index": 0, "lap_number": 0, "lap_time_stamp": 900.0,
                      "late_lap": false },
                    { "lap_index": 1, "lap_number": 1, "lap_time_stamp": 1500.0,
                      "late_lap": false },
                    { "lap_index": 2, "lap_number": -1, "lap_time_stamp": 2100.0,
                      "late_lap": true, "deleted": true },
                ],
            }] }
        }))
        .expect("a `-1` lap number is RotorHazard's own value, not schema drift (#406)");

        let mut a = RotorHazardAdapter::with_id(AdapterId("rh".into()));
        start_race(&mut a);

        let evs = a.translate(Raw::CurrentLaps(frame));

        assert_eq!(
            passes(&evs),
            2,
            "the valid laps beside the `-1` still mint passes"
        );
        let sequences: Vec<_> = evs
            .iter()
            .filter_map(|e| match e {
                Event::Pass(p) => Some(p.sequence),
                _ => None,
            })
            .collect();
        assert_eq!(
            sequences,
            vec![Some(0), Some(1)],
            "the uncounted crossing is not among them: RotorHazard said it did not count it"
        );
        assert_eq!(a.counts.snapshot, 2);
        assert_eq!(
            a.counts.uncounted, 1,
            "the `-1` lands on the uncounted counter — the timer is still refereeing (#403)"
        );
        assert_eq!(
            a.counts.deleted, 0,
            "and not on the deleted counter, though RH flags both: `deleted` alone would send an \
             RD hunting a marshaling mistake that never happened"
        );
        assert_eq!(
            a.counts.malformed_frames, 0,
            "a `-1` is RotorHazard's own wire value, not a version skew"
        );
    }

    /// RotorHazard numbers *every* crossing after the winner `-1`, so the number names the whole
    /// tail rather than a crossing. The skip is keyed on the crossing instead — four lost crossings
    /// must count four, and a re-sent snapshot must still count one.
    #[test]
    fn each_uncounted_crossing_is_counted_once_however_often_the_snapshot_repeats_it() {
        let mut a = RotorHazardAdapter::with_id(AdapterId("rh".into()));
        start_race(&mut a);

        let first = vec![lap(0, 900.0), uncounted_lap(2100.0)];
        a.translate(snapshot(0, 0, first.clone()));
        a.translate(snapshot(0, 0, first.clone()));
        a.translate(snapshot(0, 0, first));
        assert_eq!(
            a.counts.uncounted, 1,
            "one crossing, however many snapshots carry it"
        );

        // The pilot keeps flying and RotorHazard keeps not counting: same `-1`, new crossing.
        a.translate(snapshot(
            0,
            0,
            vec![lap(0, 900.0), uncounted_lap(2100.0), uncounted_lap(2700.0)],
        ));
        assert_eq!(
            a.counts.uncounted, 2,
            "the second lost crossing is its own count — keying on `-1` would have hidden it"
        );

        // Per race, like every other pass counter.
        a.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::DONE,
            race_heat_id: Some(1),
        }));
        start_race(&mut a);
        assert_eq!(a.counts.uncounted, 0, "the next heat starts clean");
    }

    /// The plugin reads `lap.lap_number` off RotorHazard's own atom and forwards it verbatim, so a
    /// finished seat's `-1` arrives on the native pass path too — same hole, same verdict (#406).
    #[test]
    fn an_uncounted_plugin_pass_is_not_a_pass() {
        let mut a = RotorHazardAdapter::with_id(AdapterId("rh".into()));
        advertise_live_pass(&mut a, true);
        start_race(&mut a);

        let evs = a.translate(Raw::GridPass(RawGridPass {
            node_index: 0,
            lap_number: RawLapNumber::Uncounted(-1),
            lap_time_stamp: 2100.0,
            peak_rssi: Some(180.0),
        }));
        assert_eq!(
            passes(&evs),
            0,
            "RotorHazard did not count it, so neither do we"
        );
        assert_eq!(a.counts.plugin, 0);
        assert_eq!(a.counts.uncounted, 1);
        assert!(
            a.plugin_passes.is_empty(),
            "a non-lap must not be recorded as a lap the plugin delivered"
        );

        // The snapshot repeats the same crossing for the rest of the race. Both paths key the skip
        // on the crossing, so it stays one skip.
        a.translate(snapshot(0, 0, vec![uncounted_lap(2100.0)]));
        assert_eq!(
            a.counts.uncounted, 1,
            "one crossing, one skip — whichever stream reported it"
        );
        assert!(
            a.pending_snapshot_laps.is_empty(),
            "and it is never held as a lap the plugin owes us"
        );
    }

    /// A socket frame the transport could not decode (schema drift) is counted and announced
    /// rather than swallowed — a plugin-version skew must not look like a dead gate (#400).
    #[test]
    fn an_undecodable_frame_is_counted() {
        let mut a = RotorHazardAdapter::with_id(AdapterId("rh".into()));
        start_race(&mut a);
        assert_eq!(a.counts.malformed_frames, 0);

        a.note_malformed_frame("current_laps", "missing field `current`");
        a.note_malformed_frame("current_laps", "missing field `current`");
        assert_eq!(
            a.counts.malformed_frames, 2,
            "every dropped frame is counted, even though only the first is logged"
        );

        // Per race, like the rest of the pass counters.
        a.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: race_status::DONE,
            race_heat_id: Some(1),
        }));
        start_race(&mut a);
        assert_eq!(a.counts.malformed_frames, 0, "the next heat starts clean");
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
