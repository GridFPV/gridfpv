//! Live RotorHazard Socket.IO transport (feature `live`).
//!
//! Connects to a running RotorHazard server, decodes its socket events into the
//! adapter's [`Raw`] messages, runs them through [`RotorHazardAdapter`], and
//! accumulates the canonical [`Event`]s. This is the thin network layer the pure
//! translator (the rest of the module) was designed to sit behind: all wire-format
//! knowledge stays in `Raw`/`translate`; this file only moves bytes.
//!
//! The RotorHazard server emits each payload as a one-element array
//! (`[ {…} ]`); we decode the first element into the matching `Raw` variant.
//!
//! Read-only in production (drain [`RotorHazardConnection::events`]); the
//! `stage_race` / `simulate_lap` / `stop_race` helpers exist to **drive** a
//! dockerized RH from the live integration test.

// `rust_socketio::Error` is a large external enum; we thread it through unchanged
// rather than box every signature in this thin wrapper.
#![allow(clippy::result_large_err)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long [`RotorHazardConnection::seat_heat`] waits for each `heat_data` / `pilot_data` response
/// before giving up on that step. Bounds the case where a quirky/slow RH never answers, so seating
/// never stalls staging — the caller then falls back to the practice-mode (no-current-heat) flow,
/// which still records via RH's `current_heat is HEAT_ID_NONE` pass-gate branch.
const SEAT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);

/// The `gridfpv_*` wire-protocol version the Director speaks (D16, S1). Sent in the
/// `gridfpv_hello` probe so a plugin can negotiate; the Director treats a plugin whose
/// `protocol_version` differs as *incompatible* (the guided-install path then offers the matching
/// build). Bump only on a breaking change to the handshake/message shapes.
pub const DIRECTOR_PROTOCOL_VERSION: u32 = 1;

use rust_socketio::client::Client;
use rust_socketio::{ClientBuilder, Payload, RawClient};
use serde_json::json;

use super::{
    Raw, RawCurrentLaps, RawEnterExitLevels, RawGridPass, RawGridSignal, RawHeatData,
    RawMarshalData, RawNodeData, RawPassRecord, RawPilotData, RawRaceDetails, RawRaceList,
    RawRaceStatus, RotorHazardAdapter, reported_nodes_from_frequency_data,
    reported_nodes_from_levels,
};
use crate::Adapter;
use gridfpv_events::Event;

/// What one RotorHazard socket frame decoded to — see [`decode_socket`].
///
/// The three cases are deliberately distinct (#400). Collapsing "we don't translate this event"
/// and "we *do* translate it but could not read this payload" into one `None` is what let schema
/// drift — a RotorHazard or plugin version whose shapes moved — look exactly like a gate that
/// stopped detecting: the frames arrived, the laps didn't, and nothing anywhere said why.
#[derive(Debug)]
pub enum Decoded {
    /// A frame we translate, decoded into its [`Raw`].
    Translated(Raw),
    /// An event this adapter does not translate. RotorHazard broadcasts plenty of them; ignoring
    /// these is normal and is *not* counted.
    Untranslated,
    /// A frame for an event we DO translate whose payload did not match the expected shape. The
    /// frame is dropped — but loudly: the transport reports it to
    /// [`RotorHazardAdapter::note_malformed_frame`].
    Malformed {
        /// The serde error (or shape problem), for the diagnostic line.
        detail: String,
    },
}

/// Decode a RotorHazard socket event (`event` name + its payload) into a [`Decoded`].
///
/// RotorHazard wraps each emit's data in a one-element array; we decode the first element into the
/// matching `Raw` variant. A decode failure on an event we translate is a *reportable* drop, not a
/// shrug — see [`Decoded`].
pub fn decode_socket(event: &str, payload: &Payload) -> Decoded {
    // Unknown event first: a payload we would never have read cannot be "malformed".
    if !translates(event) {
        return Decoded::Untranslated;
    }
    let value = match payload {
        Payload::Text(values) => match values.first() {
            Some(value) => value.clone(),
            // A known event carrying no data at all: RotorHazard always wraps a payload, so this
            // is a wire-shape fault like any other decode failure.
            None => {
                return Decoded::Malformed {
                    detail: "empty payload array".to_string(),
                };
            }
        },
        // Binary / legacy-string payloads: RotorHazard sends JSON for every event we translate,
        // so this is drift too, not an event we chose to skip.
        other => {
            return Decoded::Malformed {
                detail: format!("non-JSON payload ({})", payload_kind(other)),
            };
        }
    };
    /// Decode `value` into `$t`, mapping a serde error onto [`Decoded::Malformed`].
    macro_rules! decode {
        ($t:ty, $variant:expr) => {
            match serde_json::from_value::<$t>(value) {
                Ok(decoded) => Decoded::Translated($variant(decoded)),
                Err(error) => Decoded::Malformed {
                    detail: error.to_string(),
                },
            }
        };
    }
    match event {
        "race_status" => decode!(RawRaceStatus, Raw::RaceStatus),
        "current_laps" => decode!(RawCurrentLaps, Raw::CurrentLaps),
        "pass_record" => decode!(RawPassRecord, Raw::PassRecord),
        "node_data" => decode!(RawNodeData, Raw::NodeData),
        "enter_and_exit_at_levels" => decode!(RawEnterExitLevels, Raw::EnterExitLevels),
        "current_marshal_data" => decode!(RawMarshalData, Raw::MarshalData),
        "race_list" => decode!(RawRaceList, Raw::RaceList),
        "race_details" => decode!(RawRaceDetails, Raw::RaceDetails),
        "heat_data" => decode!(RawHeatData, Raw::HeatData),
        "pilot_data" => decode!(RawPilotData, Raw::PilotData),
        // The GridFPV plugin's live signal push (D16, Slice 2). Absent on a stock RH.
        "gridfpv_signal" => decode!(RawGridSignal, Raw::GridSignal),
        // The GridFPV plugin's native per-node pass (D16, Slice 3). Absent on a stock RH.
        "gridfpv_pass" => decode!(RawGridPass, Raw::GridPass),
        // Unreachable: `translates` above is the same list. Kept total rather than panicking.
        _ => Decoded::Untranslated,
    }
}

/// Whether `event` is one this adapter translates — the single list [`decode_socket`] keys both
/// its "not ours" shortcut and its decode table off.
fn translates(event: &str) -> bool {
    matches!(
        event,
        "race_status"
            | "current_laps"
            | "pass_record"
            | "node_data"
            | "enter_and_exit_at_levels"
            | "current_marshal_data"
            | "race_list"
            | "race_details"
            | "heat_data"
            | "pilot_data"
            | "gridfpv_signal"
            | "gridfpv_pass"
    )
}

/// Everything one socket-frame handler writes through, cloned once per registered event.
///
/// `rust_socketio` wants an owned closure per event and there are a dozen of them, so this bundles
/// the four shared cells (and the tune-telemetry tap) rather than repeating them at every `.on`.
#[derive(Clone)]
struct FrameCtx {
    /// The pure translator every decoded frame is folded through.
    adapter: Arc<Mutex<RotorHazardAdapter>>,
    /// Where the canonical [`Event`]s accumulate until the driver drains them.
    sink: Arc<Mutex<Vec<Event>>>,
    /// The newest configured heat id learned from a `heat_data` response.
    savable_heat: Arc<Mutex<Option<u64>>>,
    /// RotorHazard's current race-format id, learned from the `race_status` stream.
    current_format: Arc<Mutex<Option<i64>>>,
    /// How many nodes the timer has reported (#412) — the `enter_and_exit_at_levels` fallback
    /// writes here; the dedicated `frequency_data` handler is the primary source.
    reported_nodes: Arc<Mutex<Option<u32>>>,
    /// The tune-telemetry tap (#355 S2a) — **read-only from the event path's point of view**: it
    /// is written to, never read back into a [`Raw`], and nothing it holds can become an `Event`.
    tap: SignalTap,
}

/// A short name for a non-`Text` payload, for the malformed-frame diagnostic.
fn payload_kind(payload: &Payload) -> &'static str {
    match payload {
        Payload::Text(_) => "text",
        Payload::Binary(_) => "binary",
        _ => "unrecognized",
    }
}

/// Decode a RotorHazard socket event into a [`Raw`], or `None` when it is not one we translate
/// **or** its payload did not decode.
///
/// Prefer [`decode_socket`]: this wrapper cannot tell those two apart, and treating them alike is
/// #400's diagnosability gap. Kept for callers that only want the happy path.
pub fn raw_from_socket(event: &str, payload: &Payload) -> Option<Raw> {
    match decode_socket(event, payload) {
        Decoded::Translated(raw) => Some(raw),
        Decoded::Untranslated | Decoded::Malformed { .. } => None,
    }
}

// ---------------------------------------------------------------------------------------------
// Tune telemetry (#355, slice 2a) — the ephemeral per-node signal tap.
// ---------------------------------------------------------------------------------------------

/// The widest per-node store a tuning snapshot will ever hold.
///
/// RotorHazard tops out at 8 seats today. The cap is not about RH: it bounds the store against a
/// drifting/hostile frame whose arrays claim hundreds of nodes, so "cost per tick is O(nodes)"
/// stays a fact rather than a hope.
const MAX_TUNE_NODES: usize = 64;

/// A RotorHazard `heartbeat` frame (`BaseHardwareInterface.get_heartbeat_json`, 10 Hz from boot).
///
/// **Deliberately not a [`Raw`] variant, and deliberately private.** `Raw` is the *only* input to
/// [`RotorHazardAdapter::translate`], which is the *only* thing that mints an [`Event`] — so
/// keeping the heartbeat out of `Raw` is what makes "heartbeat data can never become a
/// `SignalChunk`, a `SignalHistory`, or reach a log" a **structural** guarantee rather than a
/// convention a later refactor can quietly break. There is no function anywhere that takes a
/// `RawHeartbeat` and returns an `Event`, and none can be written without first adding a `Raw`
/// variant on purpose.
///
/// `crossing_flag` is read as raw JSON because RotorHazard builds have wired it both as a bool and
/// as a 0/1 int; see [`truthy`].
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct RawHeartbeat {
    /// Per-node live RSSI (filtered ADC counts), array index = node index.
    #[serde(default)]
    current_rssi: Vec<f32>,
    /// Per-node tuned frequency in MHz; `0` on an untuned node.
    #[serde(default)]
    frequency: Vec<i64>,
    /// Per-node detector loop time in microseconds — the "is this timer keeping up?" readout.
    #[serde(default)]
    loop_time: Vec<i64>,
    /// Per-node crossing state at this heartbeat (bool on stock RH; some builds send 0/1).
    #[serde(default)]
    crossing_flag: Vec<serde_json::Value>,
}

/// A RotorHazard `node_crossing_change` frame: one node's crossing **edge**.
///
/// Not a [`Raw`] variant either, for the same structural reason as [`RawHeartbeat`].
#[derive(Debug, Clone, serde::Deserialize)]
struct RawNodeCrossing {
    /// Which node transitioned.
    node_index: usize,
    /// The new crossing state (bool, or 0/1 on some builds — see [`truthy`]).
    #[serde(default)]
    crossing_flag: serde_json::Value,
}

/// Read a RotorHazard crossing flag that may be wired as a bool **or** as a 0/1 number.
fn truthy(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_f64().is_some_and(|v| v != 0.0),
        _ => false,
    }
}

/// What happened to a tune-telemetry frame — the observable that makes the pre-parse gate
/// *testable* rather than merely intended.
///
/// [`Gated`](TapOutcome::Gated) is returned **before the payload is looked at**, so a test that
/// feeds a deliberately unreadable payload with the subscription closed and gets `Gated` (rather
/// than [`Unreadable`](TapOutcome::Unreadable)) has proved the parse never ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapOutcome {
    /// No subscription is open: the frame was dropped **without being deserialized**.
    Gated,
    /// The frame was deserialized and folded into the per-node store.
    Folded,
    /// A subscription is open but the payload did not match the expected shape. Dropped quietly —
    /// unlike the [`Raw`] frames, nothing downstream depends on this, so a drifting heartbeat costs
    /// a blank readout, not a missing lap.
    Unreadable,
}

/// Fold a `heartbeat` frame into `tap`, **checking the subscription gate before parsing**.
///
/// The order of the two statements below is the whole point of this function existing separately
/// from its `.on("heartbeat", …)` closure: `capturing()` is a relaxed load on a cold `bool`, and it
/// stands in front of a `from_value` that allocates four `Vec`s — ten times a second, a hundred
/// with RotorHazard's frequency scanner on, on the single socket callback thread that also parses
/// `current_laps` (#392).
fn tap_heartbeat(tap: &SignalTap, payload: &Payload) -> TapOutcome {
    if !tap.capturing() {
        return TapOutcome::Gated;
    }
    match first_text(payload).and_then(|v| serde_json::from_value::<RawHeartbeat>(v).ok()) {
        Some(hb) => {
            tap.note_heartbeat(&hb);
            TapOutcome::Folded
        }
        None => TapOutcome::Unreadable,
    }
}

/// Fold a `node_crossing_change` edge into `tap`, gated before parsing exactly as
/// [`tap_heartbeat`] is.
fn tap_crossing(tap: &SignalTap, payload: &Payload) -> TapOutcome {
    if !tap.capturing() {
        return TapOutcome::Gated;
    }
    match first_text(payload).and_then(|v| serde_json::from_value::<RawNodeCrossing>(v).ok()) {
        Some(change) => {
            tap.note_crossing(&change);
            TapOutcome::Folded
        }
        None => TapOutcome::Unreadable,
    }
}

/// The first element of a Socket.IO text payload (RotorHazard wraps every emit in a one-element
/// array), cloned out for deserialization.
fn first_text(payload: &Payload) -> Option<serde_json::Value> {
    match payload {
        Payload::Text(values) => values.first().cloned(),
        _ => None,
    }
}

/// The **latest** signal readings for one RotorHazard node — last-value-wins, no history.
///
/// Everything a tuning UI shows for a node, in one flat record. Nothing here is a lap, a pass or
/// an event: it is a live readout, overwritten on the next frame and forgotten when the
/// [`SignalTap`]'s subscription lapses.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeTick {
    /// Whether RotorHazard has ever reported this node. An **unseated** node still reports — "is
    /// this node even alive?" is half the diagnostic, so a tuning snapshot must include it.
    pub seen: bool,
    /// Live RSSI from the newest `heartbeat` (filtered ADC counts).
    pub rssi: Option<f32>,
    /// The node's tuned frequency in MHz; `None` when RotorHazard reports `0` (untuned).
    pub frequency_mhz: Option<u16>,
    /// The detector's loop time in microseconds.
    pub loop_time_micros: Option<u32>,
    /// The crossing state as of the newest frame (heartbeat level, or a `node_crossing_change` edge).
    pub crossing: bool,
    /// **Sticky**: any crossing observed since the last [`SignalTap::take`]. The Director decimates
    /// to ~5 Hz, so a crossing that opens and closes between two samples would otherwise vanish;
    /// this is what carries the edge through the decimation.
    pub crossed: bool,
    /// `node_data.node_peak_rssi` — the node's running peak.
    pub node_peak_rssi: Option<f32>,
    /// `node_data.node_nadir_rssi` — the node's running nadir.
    pub node_nadir_rssi: Option<f32>,
    /// `node_data.pass_peak_rssi` — the peak of the most recent pass.
    pub pass_peak_rssi: Option<f32>,
    /// `node_data.pass_nadir_rssi` — the nadir of the most recent pass.
    pub pass_nadir_rssi: Option<f32>,
    /// `node_data.debug_pass_count` — how many passes this node has detected.
    pub pass_count: Option<u32>,
    /// The node's enter threshold, from `enter_and_exit_at_levels`.
    pub enter_at: Option<f32>,
    /// The node's exit threshold, from `enter_and_exit_at_levels`.
    pub exit_at: Option<f32>,
}

/// The **on-demand, in-memory** per-node signal tap the Tune page's telemetry is read from (#355).
///
/// Two pieces, both load-bearing:
///
/// * **A relaxed atomic gate** ([`capturing`](Self::capturing)). `rust_socketio` binds every
///   handler at `ClientBuilder` time, so the `heartbeat` handler is *always* registered and there
///   is no "unsubscribe" to reach for. The gate is therefore checked **before** the frame is
///   deserialized — a `load(Relaxed)` on a cold `bool` in front of a `serde_json::from_value` that
///   allocates four `Vec`s, ten times a second, on the single socket callback thread that also
///   parses `current_laps`. That thread is the #392 hazard, and it is the whole reason the gate is
///   here rather than an `if wanted { … }` after the parse.
/// * **A bounded, last-value-wins store.** One [`NodeTick`] per node, overwritten in place. There
///   is no ring here: the *history* is the Director's business (it decimates onto its own
///   cadence), so the transport's cost per frame is O(nodes) and independent of how long the Tune
///   page has been open.
///
/// Nothing in this type produces an [`Event`]. See [`RawHeartbeat`] for why that is structural.
#[derive(Clone, Default)]
pub struct SignalTap {
    /// The pre-parse subscription gate. Relaxed throughout: it guards no other memory, and a frame
    /// landing on either side of the flip is equally correct.
    capture: Arc<AtomicBool>,
    /// Latest reading per node index. Grows to the widest array a frame has reported, capped at
    /// [`MAX_TUNE_NODES`].
    nodes: Arc<Mutex<Vec<NodeTick>>>,
}

impl SignalTap {
    /// Whether a subscription is currently open — the check every gated handler makes **first**.
    pub fn capturing(&self) -> bool {
        self.capture.load(Ordering::Relaxed)
    }

    /// Open or close the subscription, returning the **previous** state so a caller can act on the
    /// edge. Closing empties the store: a lapsed Tune page must leave nothing behind.
    fn set_capturing(&self, on: bool) -> bool {
        let was = self.capture.swap(on, Ordering::Relaxed);
        if was && !on {
            self.nodes.lock().expect("signal-tap lock").clear();
        }
        was
    }

    /// The current per-node readings, clearing the sticky [`NodeTick::crossed`] flags so the next
    /// read reports only crossings seen since this one.
    fn take(&self) -> Vec<NodeTick> {
        let mut nodes = self.nodes.lock().expect("signal-tap lock");
        let snapshot = nodes.clone();
        for node in nodes.iter_mut() {
            node.crossed = false;
        }
        snapshot
    }

    /// Widen the store to `len` nodes (capped), returning the guard to write through.
    fn widen(&self, len: usize) -> std::sync::MutexGuard<'_, Vec<NodeTick>> {
        let mut nodes = self.nodes.lock().expect("signal-tap lock");
        let want = len.min(MAX_TUNE_NODES);
        if nodes.len() < want {
            nodes.resize(want, NodeTick::default());
        }
        nodes
    }

    /// Fold a `heartbeat` frame in. Called **only** when [`capturing`](Self::capturing) is true.
    fn note_heartbeat(&self, hb: &RawHeartbeat) {
        let len = hb
            .current_rssi
            .len()
            .max(hb.frequency.len())
            .max(hb.loop_time.len())
            .max(hb.crossing_flag.len());
        let mut nodes = self.widen(len);
        for (index, node) in nodes.iter_mut().enumerate() {
            let mut touched = false;
            if let Some(&rssi) = hb.current_rssi.get(index) {
                node.rssi = Some(rssi);
                touched = true;
            }
            if let Some(&mhz) = hb.frequency.get(index) {
                // RotorHazard reports `0` for a node tuned to nothing; that is an absence, not a
                // 0 MHz channel, and the panel must be able to say so.
                node.frequency_mhz = u16::try_from(mhz).ok().filter(|mhz| *mhz != 0);
                touched = true;
            }
            if let Some(&loop_time) = hb.loop_time.get(index) {
                node.loop_time_micros = u32::try_from(loop_time).ok();
                touched = true;
            }
            if let Some(flag) = hb.crossing_flag.get(index) {
                let crossing = truthy(flag);
                node.crossing = crossing;
                node.crossed |= crossing;
                touched = true;
            }
            node.seen |= touched;
        }
    }

    /// Fold a `node_crossing_change` edge in. Called only while capturing.
    fn note_crossing(&self, change: &RawNodeCrossing) {
        if change.node_index >= MAX_TUNE_NODES {
            return;
        }
        let mut nodes = self.widen(change.node_index + 1);
        if let Some(node) = nodes.get_mut(change.node_index) {
            let crossing = truthy(&change.crossing_flag);
            node.crossing = crossing;
            node.crossed |= crossing;
            node.seen = true;
        }
    }

    /// Fold a `node_data` frame's peak / nadir / pass-count readouts in.
    ///
    /// `heartbeat` carries **only** rssi / frequency / loop-time / crossing, so every peak, nadir
    /// and pass count a tuning panel shows comes from here. Both feeds are needed; neither is a
    /// subset of the other. The frame is parsed regardless (it is a [`Raw`] the adapter already
    /// translates), so this adds no parse — only the fold, which is itself gated.
    fn note_node_data(&self, data: &RawNodeData) {
        let len = data
            .node_peak_rssi
            .len()
            .max(data.node_nadir_rssi.len())
            .max(data.pass_peak_rssi.len())
            .max(data.pass_nadir_rssi.len())
            .max(data.debug_pass_count.len());
        let mut nodes = self.widen(len);
        for (index, node) in nodes.iter_mut().enumerate() {
            let mut touched = false;
            for (slot, source) in [
                (&mut node.node_peak_rssi, &data.node_peak_rssi),
                (&mut node.node_nadir_rssi, &data.node_nadir_rssi),
                (&mut node.pass_peak_rssi, &data.pass_peak_rssi),
                (&mut node.pass_nadir_rssi, &data.pass_nadir_rssi),
            ] {
                if let Some(&value) = source.get(index) {
                    *slot = Some(value);
                    touched = true;
                }
            }
            if let Some(&count) = data.debug_pass_count.get(index) {
                node.pass_count = u32::try_from(count).ok();
                touched = true;
            }
            node.seen |= touched;
        }
    }

    /// Fold an `enter_and_exit_at_levels` frame's per-node thresholds in.
    ///
    /// Tuning needs these with **no event and no armed heat**, which is exactly what the app
    /// layer's lineup remap cannot provide (it drops every node outside the armed heat), so the
    /// tap reads them straight off the wire.
    fn note_levels(&self, levels: &RawEnterExitLevels) {
        let len = levels
            .enter_at_levels
            .len()
            .max(levels.exit_at_levels.len());
        let mut nodes = self.widen(len);
        for (index, node) in nodes.iter_mut().enumerate() {
            if let Some(&enter) = levels.enter_at_levels.get(index) {
                node.enter_at = Some(enter);
            }
            if let Some(&exit) = levels.exit_at_levels.get(index) {
                node.exit_at = Some(exit);
            }
        }
    }
}

/// The GridFPV plugin's `gridfpv_hello_ack` payload — the in-process RH plugin's reply to
/// the Director's `gridfpv_hello` probe (RH plugin design D16, Slice 1). Present only when a
/// plugin-equipped RH answered the handshake; a stock RH never registers the handler, so the
/// probe simply times out (the Director then surfaces the guided install). Pure wire data —
/// the app layer maps it to its `PluginPresence` status.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PluginHello {
    /// The `gridfpv_*` wire-protocol version (negotiated against the Director's supported range).
    pub protocol_version: u32,
    /// The plugin build's own version string (e.g. `"0.1.0"`).
    #[serde(default)]
    pub plugin_version: String,
    /// The RHAPI version the plugin reports (e.g. `"1.4"`).
    #[serde(default)]
    pub rhapi_version: String,
    /// Capabilities the plugin declares it implements (e.g. `["hello"]`, `"live_signal"`,
    /// [`CAP_LIVE_PASS`]; later `"clean_control"`, `"recalc"`). The Director keys transport
    /// decisions off these — see [`PluginHello::advertises`].
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// The plugin's node/seat count.
    #[serde(default)]
    pub node_count: u32,
    /// The id of the plugin's **Grid-owned race format** row (D16, S3b / #404), if it could be
    /// created. `None` from an older plugin build that has no such concept, and `None` **with**
    /// [`grid_format_error`](Self::grid_format_error) set when this build tried and failed.
    #[serde(default)]
    pub grid_format_id: Option<i64>,
    /// The plugin's name for that format row (`"GridFPV"`), for diagnostics an RD can act on.
    #[serde(default)]
    pub grid_format_name: Option<String>,
    /// Why the plugin could not create its owned race format at load, if it could not. Announced
    /// through the [`crate::diag`] sink: a timer whose race decisions were not neutralised is
    /// exactly #403, and it must never be a silent condition.
    #[serde(default)]
    pub grid_format_error: Option<String>,
}

impl PluginHello {
    /// Whether the plugin advertised `capability` in its handshake.
    pub fn advertises(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }
}

/// The plugin capability that makes it the **authoritative pass source** (#389): it emits
/// `gridfpv_pass` natively from `RACE_LAP_RECORDED`. The plugin earns this with a load-time
/// self-check and omits it when its lap atom is unreadable, so its presence in the handshake — and
/// nothing else — decides whether the adapter takes plugin passes or RotorHazard's `current_laps`.
pub const CAP_LIVE_PASS: &str = "live_pass";

/// The plugin capability that makes **Grid own its RotorHazard race format** (#404, #405): the
/// plugin creates (find-or-create, once) a `GridFPV` format row with every RH-side race decision
/// neutralised, and selects it on request. Its presence is what switches
/// [`prepare_instant_start`](RotorHazardConnection::prepare_instant_start) from mutating the race
/// director's own active format to selecting Grid's.
pub const CAP_OWNED_FORMAT: &str = "owned_format";

/// How long [`prepare_instant_start`](RotorHazardConnection::prepare_instant_start) waits for the
/// plugin's `gridfpv_format_ack` the **first** time it selects the Grid-owned format on a
/// connection. Only the first selection blocks: the Director asks at the heat's Stage transition
/// (pre-Armed, seconds before "go"), so a short confirm there costs nothing, and every later call
/// is fire-and-forget because the format is already proven. On a timeout the fallback engages and
/// is announced — Grid never races on an unconfirmed neutralisation.
const FORMAT_ACK_TIMEOUT: Duration = Duration::from_secs(3);

/// The plugin's `gridfpv_format_ack` — its reply to a `gridfpv_select_format` request (D16, S3b).
///
/// Carries the outcome of the plugin's find-or-create-and-select of its `GridFPV` race format:
/// which row it is, whether this call created or repaired it, and the race director's own format
/// that Grid displaced (so handing the timer back is a known id, not a guess). `ok: false` with
/// `error` set when the plugin could not get the timer into a neutral state — which the Director
/// announces and then falls back from.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FormatAck {
    /// Whether the Grid-owned format is now RotorHazard's current race format.
    pub ok: bool,
    /// The `GridFPV` format row id, when there is one.
    #[serde(default)]
    pub format_id: Option<i64>,
    /// The plugin's name for that row (`"GridFPV"`).
    #[serde(default)]
    pub format_name: Option<String>,
    /// Whether this call created the row (as opposed to reusing the existing one) — `false` on
    /// every call after the first ever, which is what idempotency looks like on the wire.
    #[serde(default)]
    pub created: bool,
    /// Conduct fields that had drifted off neutral and were written back (normally empty).
    #[serde(default)]
    pub repaired: Vec<String>,
    /// The race director's own format that Grid took the timer over from, if any.
    #[serde(default)]
    pub previous_format_id: Option<i64>,
    /// That format's name, for a diagnostic an RD recognises (the raw id alone is not a name).
    #[serde(default)]
    pub previous_format_name: Option<String>,
    /// Why it failed, when `ok` is false.
    #[serde(default)]
    pub error: Option<String>,
}

/// Parse a `gridfpv_hello_ack` socket payload (a one-element array, like every RH emit) into a
/// [`PluginHello`]. `None` for a malformed/unexpected shape (treated as no answer).
fn parse_hello(payload: &Payload) -> Option<PluginHello> {
    let value = match payload {
        Payload::Text(values) => values.first()?.clone(),
        _ => return None,
    };
    serde_json::from_value(value).ok()
}

/// Parse a `gridfpv_format_ack` socket payload into a [`FormatAck`]. `None` for a
/// malformed/unexpected shape (treated as no answer — the confirm then times out and the
/// fallback engages, loudly).
fn parse_format_ack(payload: &Payload) -> Option<FormatAck> {
    let value = match payload {
        Payload::Text(values) => values.first()?.clone(),
        _ => return None,
    };
    serde_json::from_value(value).ok()
}

/// What this connection knows about the plugin's **Grid-owned race format** (#404).
///
/// One instance per connection (never carried across a reconnect): a timer whose plugin was
/// removed, downgraded, or whose format row was deleted while we were away must re-earn all of
/// this on the new socket rather than inherit a stale "yes, it's neutralised".
#[derive(Debug, Default)]
struct OwnedFormat {
    /// Whether the connected plugin advertised [`CAP_OWNED_FORMAT`].
    advertised: bool,
    /// The `GridFPV` format row id, once the plugin has named one.
    id: Option<i64>,
    /// Set once a `gridfpv_format_ack` confirmed the format is **selected** on this connection.
    selected: bool,
    /// The plugin's failure text, from a failed ack or a failed load-time create.
    error: Option<String>,
    /// One-shot latch: the fallback to mutating the RD's own format has already been announced on
    /// this connection. Announced once, not once per heat — a per-stage repeat would bury it.
    announced: bool,
}

/// A live connection to a RotorHazard server, translating its socket stream into
/// canonical [`Event`]s.
pub struct RotorHazardConnection {
    client: Client,
    events: Arc<Mutex<Vec<Event>>>,
    /// The adapter driving translation, held behind a handle so the **persistent** driver can
    /// recover it on [`disconnect`](Self::disconnect) and reuse it across a reconnect (#105). The
    /// adapter's dedup / `last_race_status` must survive a mid-race reconnect: a fresh adapter has an
    /// empty dedup and would re-emit RotorHazard's re-sent `current_laps` snapshot as duplicate laps.
    adapter: Arc<Mutex<RotorHazardAdapter>>,
    /// Liveness flag flipped to `false` by `rust_socketio`'s reserved `close`/`error` handlers when
    /// the socket drops. With `.reconnect(false)` (see [`connect`](Self::connect)) a dropped link is
    /// a real, final close — `rust_socketio` no longer silently buffers emits and auto-reconnects —
    /// so the driver can read [`is_alive`](Self::is_alive) as the source of truth for a drop (#105).
    alive: Arc<AtomicBool>,
    /// The newest configured heat id learned from the most recent `heat_data` response (the highest
    /// id, i.e. the freshest — the one `ensure_savable_heat` just added). The `heat_data` socket
    /// handler stashes it here; the **driver thread** drains it via [`take_savable_heat`] and selects
    /// it synchronously (`set_current_heat`) before staging, so the run is savable (the dense-history
    /// precondition) without any emit-per-`heat_data` feedback loop on the socket callback.
    savable_heat: Arc<Mutex<Option<u64>>>,
    /// RotorHazard's **current race-format id**, learned from the `race_status` stream
    /// (`emit_race_status`'s `race_format_id`, which arrives on connect and on every status change).
    /// [`prepare_instant_start`](Self::prepare_instant_start) zeroes *this* format's staging delays
    /// so `stage_race` transitions straight to RACING with no RH-side staging hold/tones — the
    /// Grid-owns-all-timing model (#…): Grid's start procedure is the only delay; RH just records on
    /// command. `None` until the first `race_status` carrying a format id is folded.
    current_format: Arc<Mutex<Option<i64>>>,
    /// The GridFPV plugin's handshake reply (`gridfpv_hello_ack`), once it arrives. The Director
    /// emits `gridfpv_hello` on (re)connect; a plugin-equipped RH answers and the `gridfpv_hello_ack`
    /// handler stashes the [`PluginHello`] here, which the driver reads via
    /// [`wait_for_plugin`](Self::wait_for_plugin). Stays `None` against a stock RH (no handler
    /// registered) — that absence is what drives the Director's guided-install prompt (D16, S1).
    hello: Arc<Mutex<Option<PluginHello>>>,
    /// What this connection knows about the plugin's **Grid-owned race format** (#404) — see
    /// [`OwnedFormat`]. Written by the `gridfpv_hello_ack` / `gridfpv_format_ack` handlers, read
    /// by [`prepare_instant_start`](Self::prepare_instant_start) to decide whether Grid selects
    /// its own format row or falls back to mutating the race director's.
    owned_format: Arc<Mutex<OwnedFormat>>,
    /// **How many nodes the timer reported** on this connection (#412), or `None` until it has
    /// said. Written by the `frequency_data` handler (and, as a fallback, by the
    /// `enter_and_exit_at_levels` one); read by [`wait_for_reported_nodes`](Self::wait_for_reported_nodes).
    ///
    /// Per-connection, never carried across a reconnect: a timer that came back with a node missing
    /// must re-report rather than inherit a stale width.
    reported_nodes: Arc<Mutex<Option<u32>>>,
    /// The **tune-telemetry tap** (#355 S2a): the gate the `heartbeat` / `node_crossing_change`
    /// handlers check before parsing, plus the bounded last-value-wins per-node store they and the
    /// `node_data` / `enter_and_exit_at_levels` handlers write into.
    ///
    /// Deliberately parallel to — and disjoint from — the `events` sink. Nothing in here is an
    /// [`Event`], nothing in here reaches a log, and nothing in here survives
    /// [`set_signal_capture(false)`](Self::set_signal_capture).
    tap: SignalTap,
}

impl RotorHazardConnection {
    /// Connect to `url` (e.g. `http://localhost:5000`) and start translating the
    /// RotorHazard socket stream through `adapter`.
    pub fn connect(url: &str, adapter: RotorHazardAdapter) -> Result<Self, rust_socketio::Error> {
        let adapter = Arc::new(Mutex::new(adapter));
        let events: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
        // Fresh link, fresh pass-source decision (#389). The adapter is REUSED across reconnects
        // (the #105 fix), so a stale `live_pass` would survive a plugin being uninstalled and leave
        // the adapter waiting for passes that can no longer come. Start from "no plugin has
        // spoken" — `current_laps` — and let this connection's `gridfpv_hello_ack` re-earn it.
        //
        // This is exactly the mid-race-reconnect path of #400: the reused adapter may still be
        // holding laps `current_laps` reported while the plugin was quiet, and the switch mints
        // them. The sink exists first so those laps go straight onto it instead of being dropped.
        let carried = adapter
            .lock()
            .expect("adapter lock")
            .set_plugin_live_pass(false);
        if !carried.is_empty() {
            events.lock().expect("event sink lock").extend(carried);
        }
        // Starts alive; flipped to `false` by the `close`/`error` reserved-event handlers below.
        let alive = Arc::new(AtomicBool::new(true));
        // The newest savable heat id, stashed by the `heat_data` handler and drained by the driver
        // (see the struct field). Starts empty (no heat learned yet).
        let savable_heat: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));
        // The current race-format id, learned from the `race_status` stream (see the struct field).
        // Starts empty (no status folded yet); the first `race_status` on connect populates it.
        let current_format: Arc<Mutex<Option<i64>>> = Arc::new(Mutex::new(None));
        // The GridFPV plugin handshake reply, stashed by the `gridfpv_hello_ack` handler below and
        // read by the driver (see the struct field). Empty until/unless a plugin-equipped RH answers.
        let hello: Arc<Mutex<Option<PluginHello>>> = Arc::new(Mutex::new(None));
        // Fresh link, fresh owned-format state: nothing is assumed neutralised until THIS socket's
        // plugin says so (see `OwnedFormat`).
        let owned_format: Arc<Mutex<OwnedFormat>> = Arc::new(Mutex::new(OwnedFormat::default()));
        // The tune-telemetry tap (#355 S2a), closed. A fresh link starts NOT capturing: the Tune
        // page's lease is what opens it, and a reconnect under a still-open lease is re-opened by
        // the driver's next tick (which re-reads the lease and re-warms the store).
        // #412 node discovery: filled by the `frequency_data` handler below, and by
        // `enter_and_exit_at_levels` as a fallback. Per-connection.
        let reported_nodes: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
        let tap = SignalTap::default();
        let ctx = FrameCtx {
            adapter: adapter.clone(),
            sink: events.clone(),
            savable_heat: savable_heat.clone(),
            current_format: current_format.clone(),
            reported_nodes: reported_nodes.clone(),
            tap: tap.clone(),
        };

        // `rust_socketio`'s reserved events: on a dropped socket the poll loop fires `error`
        // (the engine.io read failed) and, on a clean disconnect packet, `close`. Either way the
        // link is no longer usable, so flip `alive` to `false` — the truth the driver monitors.
        let drop_handler = |alive: Arc<AtomicBool>| {
            move |_payload: Payload, _client: RawClient| {
                alive.store(false, Ordering::Relaxed);
            }
        };

        // One handler per translated event: decode -> translate -> accumulate. After translating, if
        // the adapter flagged a heat-end marshal-data request (set on the DONE transition, drained
        // here so it fires once), emit RotorHazard's `current_race_marshal` on the socket the callback
        // was handed. RotorHazard replies with `current_marshal_data` — the dense `history_values`/
        // `history_times` trace — which the `current_marshal_data` handler below feeds back through the
        // same adapter as `SignalHistory`. Driving the request from the `race_status` callback keeps
        // all wire IO in the transport while the trigger stays in the pure translator.
        let handler = |name: &'static str, ctx: FrameCtx| {
            let FrameCtx {
                adapter,
                sink,
                savable_heat,
                current_format,
                reported_nodes,
                tap,
            } = ctx;
            move |payload: Payload, client: RawClient| {
                // Learn the current race-format id from the `race_status` stream (it carries
                // `race_format_id`). `prepare_instant_start` zeroes that format's staging delays so
                // `stage_race` reaches RACING with no RH-side staging hold (Grid owns all timing).
                // Parsed straight off the payload here (independent of the translator's `Raw`) so the
                // adapter's shape is untouched.
                if name == "race_status" {
                    if let Payload::Text(values) = &payload {
                        if let Some(id) = values
                            .first()
                            .and_then(|v| v.get("race_format_id"))
                            .and_then(|v| v.as_i64())
                        {
                            *current_format.lock().unwrap() = Some(id);
                        }
                    }
                }
                let raw = match decode_socket(name, &payload) {
                    Decoded::Translated(raw) => Some(raw),
                    // Not ours — RotorHazard broadcasts plenty we don't translate. Normal.
                    Decoded::Untranslated => None,
                    // Ours, but unreadable: the frame is dropped either way, so at minimum say
                    // so and count it (#400). Silently swallowing this made a plugin/RH version
                    // skew look identical to a gate that stopped detecting.
                    Decoded::Malformed { detail } => {
                        adapter
                            .lock()
                            .expect("adapter lock")
                            .note_malformed_frame(name, &detail);
                        None
                    }
                };
                if let Some(raw) = raw {
                    // Tune telemetry (#355 S2a). These two frames are decoded *anyway* — they are
                    // `Raw`s the adapter translates — so the tap adds no parse here, only the
                    // O(nodes) fold, and that stays behind the same subscription gate as the
                    // heartbeat so an idle timer pays nothing. `node_data` carries every peak /
                    // nadir / pass-count readout (the heartbeat carries none of them); the levels
                    // carry the thresholds the tuning graph draws its handles at, which the app
                    // layer's lineup remap cannot supply because tuning has no armed heat.
                    if tap.capturing() {
                        match &raw {
                            Raw::NodeData(data) => tap.note_node_data(data),
                            Raw::EnterExitLevels(levels) => tap.note_levels(levels),
                            _ => {}
                        }
                    }
                    // Node-count discovery **fallback** (#412). `enter_at_levels` is explicitly
                    // sliced `[:num_nodes]` on both v4.3.0 and v4.4.0, so its length is the node
                    // count too. `frequency_data` is preferred (a list of dicts, unambiguous), so
                    // this only fills in when that frame has not arrived — an RH build or plugin
                    // that answers one `load_data` type and not the other still gets discovered
                    // rather than silently falling back to the 8-node default.
                    if let Raw::EnterExitLevels(levels) = &raw {
                        if let Some(nodes) = reported_nodes_from_levels(levels) {
                            let mut slot = reported_nodes.lock().expect("reported-nodes lock");
                            if slot.is_none() {
                                *slot = Some(nodes);
                            }
                        }
                    }
                    let (translated, request_marshal, pilotrace_requests, heat_ids) = {
                        let mut a = adapter.lock().unwrap();
                        let translated = a.translate(raw);
                        // Drain the heat-end intents: the one-shot marshal request (set on the DONE
                        // edge), any per-pilotrace pulls discovered from a `race_list`, and the
                        // configured heat ids learned from a `heat_data` (so a savable heat can be
                        // selected before staging — the dense-history precondition).
                        let request_marshal = a.take_marshal_request();
                        let pilotrace_requests = a.take_pilotrace_requests();
                        let heat_ids = a.take_heat_ids();
                        (translated, request_marshal, pilotrace_requests, heat_ids)
                    };
                    if !translated.is_empty() {
                        sink.lock().unwrap().extend(translated);
                    }
                    // A `heat_data` response lists the configured heats: stash the highest (newest) id
                    // so the **driver thread** can select it as the current (savable) heat
                    // synchronously, before staging (see `RotorHazardConnection::take_savable_heat`).
                    // We do NOT emit `set_current_heat` from this socket callback: `heat_data` is
                    // broadcast on every heat mutation, and an emit-per-`heat_data` would feed back
                    // (set_current_heat -> heat/current-heat re-emits) and flood the link. The driver
                    // selects once, deterministically, on its own thread instead.
                    if let Some(&heat) = heat_ids.iter().max() {
                        if heat >= 0 {
                            savable_heat.lock().unwrap().replace(heat as u64);
                        }
                    }
                    if request_marshal {
                        // Heat just ended: pull the dense history. Two RotorHazard builds expose it
                        // differently, so drive both — whichever the server implements answers:
                        //  • newer RH: the aggregate `current_race_marshal` -> `current_marshal_data`;
                        //  • older RH: per-pilotrace — `save_laps` (persist the run), then request the
                        //    saved-race tree (`race_list`) whose ids drive `get_pilotrace` below.
                        // All best-effort: a failed emit on a dropped link just leaves the coarse
                        // streamed trace, which the driver's reconnect path tolerates.
                        let _ = client.emit("current_race_marshal", Payload::Text(vec![]));
                        let _ = client.emit("save_laps", Payload::Text(vec![]));
                        let _ = client.emit("load_data", json!({ "load_types": ["race_list"] }));
                    }
                    // A `race_list` yields the per-pilotrace ids to pull; issue each `get_pilotrace`
                    // so its `race_details` (the dense history) folds back through this same adapter.
                    for req in pilotrace_requests {
                        let _ = client
                            .emit("get_pilotrace", json!({ "pilotrace_id": req.pilotrace_id }));
                    }
                }
            }
        };

        // RotorHazard timers are LAN devices; a box served over HTTPS will almost always carry a
        // **self-signed** cert. Accept invalid certs/hostnames for the timer connection so a
        // self-signed RH still works. This LAN-trust relaxation is scoped to the **timer adapter
        // only** — it is explicitly NOT the posture for cloud/internet traffic, which must verify
        // TLS properly (the cloud rule). Plain-HTTP RotorHazard — the common case — is unaffected
        // (no handshake occurs). `rust_socketio` uses the same `.expect()` for its own connector;
        // building one from flags performs no I/O and does not realistically fail.
        let tls = native_tls::TlsConnector::builder()
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true)
            .build()
            .expect("build a relaxed TLS connector for the LAN RotorHazard timer");

        let client = ClientBuilder::new(url.to_string())
            .tls_config(tls)
            // Do NOT let `rust_socketio` auto-reconnect (#105). With `.reconnect(true)` a dropped
            // socket is invisible: the client buffers emits and returns `Ok` while silently
            // reconnecting in the background, so `probe_liveness`'s emit never errors and a real
            // drop is never detected. With `.reconnect(false)` a drop becomes a real, final close
            // that fires the `close`/`error` reserved events below — and the *driver* owns
            // reconnection (it has backoff and **reuses this adapter across reconnects**:
            // `connect` takes it, [`disconnect`](Self::disconnect) returns it). On its reconnect
            // RotorHazard re-sends the full `current_laps` snapshot; because the adapter's per-lap
            // dedup persists across the reconnect, that replay is suppressed (no double-counted
            // laps, #105) — see the dedup module + the rh_signal snapshot-dedup assertion.
            .reconnect(false)
            .on("error", drop_handler(alive.clone()))
            .on("close", drop_handler(alive.clone()))
            .on("race_status", handler("race_status", ctx.clone()))
            .on("current_laps", handler("current_laps", ctx.clone()))
            .on("node_data", handler("node_data", ctx.clone()))
            .on("pass_record", handler("pass_record", ctx.clone()))
            .on("enter_and_exit_at_levels", handler("enter_and_exit_at_levels", ctx.clone()))
            .on("current_marshal_data", handler("current_marshal_data", ctx.clone()))
            .on("race_list", handler("race_list", ctx.clone()))
            .on("race_details", handler("race_details", ctx.clone()))
            .on("heat_data", handler("heat_data", ctx.clone()))
            .on("pilot_data", handler("pilot_data", ctx.clone()))
            // The GridFPV plugin's live signal push (D16, S2): folds straight through the same
            // translator as the RH-native signal events (→ SignalThresholds/SignalHistory).
            .on("gridfpv_signal", handler("gridfpv_signal", ctx.clone()))
            // The GridFPV plugin's native per-node pass (D16, S3): folds to a Pass, deduped with
            // the current_laps path.
            .on("gridfpv_pass", handler("gridfpv_pass", ctx.clone()))
            // The GridFPV plugin's handshake reply (D16, S1): a plugin-equipped RH answers our
            // `gridfpv_hello` (emitted below) with `gridfpv_hello_ack`. Stash it for the driver.
            // It also carries the plugin's capabilities, which is where the **pass source** is
            // decided (#389): `live_pass` present ⇒ the plugin's `gridfpv_pass` mints the laps and
            // `current_laps` becomes a checked backstop; absent ⇒ `current_laps` mints them and
            // plugin passes are ignored. Declared here, not inferred from whichever stream happens
            // to arrive first.
            // ---------------------------------------------------------------------------------
            // Tune telemetry (#355 S2a): the two frames NOTHING else in this adapter consumes.
            //
            // Both handlers are bound here, unconditionally, because `rust_socketio` binds at
            // `ClientBuilder` time and there is no way to attach one later — so the subscription
            // is expressed as a **gate inside** the handler, checked BEFORE the payload is
            // deserialized. `heartbeat` runs at 10 Hz from RotorHazard's boot (and at 100 Hz when
            // RH's own frequency scanner is on), on the same single socket callback thread that
            // parses `current_laps`; paying a `from_value` + four `Vec` allocations per tick for a
            // Tune page nobody has open is precisely the #392 hazard. The relaxed load in front of
            // it is the cheapest thing that can stand there.
            .on("heartbeat", {
                let tap = tap.clone();
                move |payload: Payload, _client: RawClient| {
                    // The gate lives inside `tap_heartbeat`, ahead of the parse — see there.
                    tap_heartbeat(&tap, &payload);
                }
            })
            // Crossing **edges**. The heartbeat's `crossing_flag` is a level sampled at 10 Hz and
            // the Director decimates below that, so a short crossing can fall between two samples;
            // these edges are what make the crossing lamp honest. Same pre-parse gate.
            // **Node-count discovery** (#412). `frequency_data` carries one `fdata` entry per
            // node, which is the only unambiguous statement of width RotorHazard makes on the
            // socket. Bound unconditionally (there is no way to attach a handler later) but it
            // costs nothing when idle: RH emits it on `load_data` — which `connect` asks for below
            // — and thereafter only when a frequency actually changes.
            .on("frequency_data", {
                let reported_nodes = reported_nodes.clone();
                move |payload: Payload, _client: RawClient| {
                    let Some(value) = first_text(&payload) else {
                        return;
                    };
                    // `frequency_data` is NOT a `Raw` variant, deliberately (like `heartbeat`):
                    // a node count is an observation about the hardware, and `Raw` is the only
                    // input to the translator that mints `Event`s. The parse is the shared,
                    // always-compiled one so its wire contract is unit-testable without a socket.
                    if let Some(nodes) = reported_nodes_from_frequency_data(&value) {
                        *reported_nodes.lock().expect("reported-nodes lock") = Some(nodes);
                    }
                }
            })
            .on("node_crossing_change", {
                let tap = tap.clone();
                move |payload: Payload, _client: RawClient| {
                    tap_crossing(&tap, &payload);
                }
            })
            .on("gridfpv_hello_ack", {
                let hello = hello.clone();
                let adapter = adapter.clone();
                let sink = events.clone();
                let owned_format = owned_format.clone();
                move |payload: Payload, _client: RawClient| {
                    if let Some(parsed) = parse_hello(&payload) {
                        // The Grid-owned race format (#404): learn the row the plugin made for us,
                        // or the reason it could not. A plugin that advertises the capability but
                        // carries no id is a timer whose race decisions are NOT neutralised — say
                        // so now, not after a heat comes up four laps short (#403).
                        {
                            let mut owned = owned_format.lock().expect("owned-format lock");
                            owned.advertised = parsed.advertises(CAP_OWNED_FORMAT);
                            owned.id = parsed.grid_format_id;
                            owned.error = parsed.grid_format_error.clone();
                        }
                        if parsed.advertises(CAP_OWNED_FORMAT) && parsed.grid_format_id.is_none() {
                            crate::diag!(
                                "gridfpv: rotorhazard: the GridFPV plugin (v{}) could not create \
                                 its `{}` race format ({}) — it will retry at each heat's stage, \
                                 and until it succeeds Grid falls back to altering the race \
                                 director's own format (#403/#404)",
                                parsed.plugin_version,
                                parsed
                                    .grid_format_name
                                    .clone()
                                    .unwrap_or_else(|| "GridFPV".to_string()),
                                parsed
                                    .grid_format_error
                                    .clone()
                                    .unwrap_or_else(|| "no reason given".to_string()),
                            );
                        }
                        let live_pass = parsed.advertises(CAP_LIVE_PASS);
                        // The switch mints any laps held for the plugin instead of dropping them
                        // (#400) — forward them to the same sink the frame handlers feed.
                        let carried = adapter
                            .lock()
                            .expect("adapter lock")
                            .set_plugin_live_pass(live_pass);
                        if !carried.is_empty() {
                            sink.lock().expect("event sink lock").extend(carried);
                        }
                        if !live_pass {
                            crate::diag!(
                                "gridfpv: rotorhazard: the GridFPV plugin (v{}) did not advertise \
                                 `{CAP_LIVE_PASS}` — it could not prove it can read this timer's \
                                 lap atom, so RotorHazard's own lap table is the pass source (#389)",
                                parsed.plugin_version,
                            );
                        }
                        *hello.lock().expect("plugin-hello lock") = Some(parsed);
                    }
                }
            })
            // The plugin's reply to `gridfpv_select_format` (D16, S3b): the Grid-owned race format
            // is (or is not) now RotorHazard's current format. `prepare_instant_start` blocks on
            // this the first time; afterwards it is the channel through which a *later* failure —
            // an RD editing the row mid-event, a format change refused because RH was not READY —
            // still gets announced instead of silently un-neutralising the timer.
            .on("gridfpv_format_ack", {
                let owned_format = owned_format.clone();
                move |payload: Payload, _client: RawClient| {
                    let Some(ack) = parse_format_ack(&payload) else {
                        return;
                    };
                    let name = ack
                        .format_name
                        .clone()
                        .unwrap_or_else(|| "GridFPV".to_string());
                    // Whether this ack is the moment the takeover took effect on this link —
                    // the Director asks at every heat's stage, so only the transition is news.
                    let first_selection = {
                        let mut owned = owned_format.lock().expect("owned-format lock");
                        let first = ack.ok && !owned.selected;
                        owned.id = ack.format_id.or(owned.id);
                        owned.selected = ack.ok;
                        owned.error = ack.error.clone();
                        first
                    };
                    if !ack.ok {
                        crate::diag!(
                            "gridfpv: rotorhazard: the GridFPV plugin could not select its `{name}` \
                             race format ({}) — RotorHazard's own race decisions are NOT \
                             neutralised on this timer, so it may declare a winner and delete \
                             later crossings at source (#403)",
                            ack.error
                                .clone()
                                .unwrap_or_else(|| "no reason given".to_string()),
                        );
                        return;
                    }
                    // Worth a line when something actually changed: the first takeover names the
                    // RD's format so they know what to re-select to get their timer back, and a
                    // repair means the row had drifted off neutral since we last looked.
                    if ack.created {
                        crate::diag!(
                            "gridfpv: rotorhazard: created the Grid-owned `{name}` race format — \
                             RotorHazard will declare no winner, apply no lap cap, no time limit \
                             and no team aggregation while Grid drives (#403/#404)"
                        );
                    }
                    if !ack.repaired.is_empty() {
                        crate::diag!(
                            "gridfpv: rotorhazard: the `{name}` race format had drifted off \
                             neutral ({}) — repaired before staging",
                            ack.repaired.join(", "),
                        );
                    }
                    if let (true, Some(previous)) =
                        (first_selection, ack.previous_format_name.clone())
                    {
                        crate::diag!(
                            "gridfpv: rotorhazard: racing on the Grid-owned `{name}` race format; \
                             this timer's own format `{previous}` is untouched — select it again \
                             in RotorHazard to hand the timer back"
                        );
                    }
                }
            })
            .connect()?;

        // Warm initial state on (re)connect: ask RH to send current per-node RSSI, the enter/exit
        // detection thresholds, the current race status (so the **current format id** is learned
        // early — `prepare_instant_start` needs it to zero that format's staging), and
        // `frequency_data` — whose `fdata` length is how many nodes the timer has (#412).
        // `current_laps` also arrives via the normal snapshot stream.
        let _ = client.emit(
            "load_data",
            json!({
                "load_types": [
                    "node_data",
                    "enter_and_exit_at_levels",
                    "race_status",
                    "frequency_data",
                ]
            }),
        );

        // Probe for the GridFPV plugin (D16, S1): emit `gridfpv_hello` over the connection we just
        // opened. A plugin-equipped RH replies with `gridfpv_hello_ack` (handled above); a stock RH
        // has no handler, so nothing comes back and the driver's `wait_for_plugin` times out. We
        // announce the Director's supported protocol version so the plugin can negotiate later.
        let _ = client.emit(
            "gridfpv_hello",
            json!({ "protocol_version": DIRECTOR_PROTOCOL_VERSION }),
        );

        Ok(Self {
            client,
            events,
            adapter,
            alive,
            savable_heat,
            current_format,
            hello,
            owned_format,
            reported_nodes,
            tap,
        })
    }

    /// Open or close the **tune-telemetry subscription** on this link (#355 S2a), returning `true`
    /// when this call *opened* it (the rising edge).
    ///
    /// This is the only way the gate moves. Called from the driver thread on every maintain tick
    /// with the current state of the Tune page's TTL lease, so:
    ///
    /// * a closed tab, a crashed browser or a lost network stops the stream when the lease lapses —
    ///   there is no flag left set forever by a client that never said goodbye;
    /// * a reconnect under a still-open lease re-opens by itself on the next tick.
    ///
    /// On the rising edge it asks RotorHazard to re-send the two frames that are **not** periodic
    /// enough to wait for: `node_data`'s peak/nadir/count readouts and the enter/exit thresholds
    /// the tuning graph draws its handles at. Without that, a Tune page opened long after connect
    /// would show blank readouts until RH happened to re-broadcast. Best-effort: a failed emit on a
    /// dying link just means the first snapshot carries rssi only.
    pub fn set_signal_capture(&self, on: bool) -> bool {
        let was = self.tap.set_capturing(on);
        let rising = on && !was;
        if rising {
            let _ = self.client.emit(
                "load_data",
                json!({ "load_types": ["node_data", "enter_and_exit_at_levels"] }),
            );
        }
        rising
    }

    /// The latest per-node readings, clearing the sticky crossing flags (see [`NodeTick::crossed`]).
    ///
    /// Bounded by construction: one [`NodeTick`] per node and nothing else, so the cost is
    /// O(nodes) however long the Tune page has been open. Empty while the subscription is closed.
    pub fn take_signal(&self) -> Vec<NodeTick> {
        self.tap.take()
    }

    /// Take (and clear) the newest savable heat id learned from a `heat_data` response, if any.
    ///
    /// The driver calls this after [`ensure_savable_heat`](Self::ensure_savable_heat) requested the
    /// heat list: a `Some(id)` means the heat list arrived and `id` is the freshest heat to make
    /// current (`set_current_heat`) before staging, so the run persists its dense history. `None`
    /// until the `heat_data` response has been folded.
    pub fn take_savable_heat(&self) -> Option<u64> {
        self.savable_heat.lock().expect("savable-heat lock").take()
    }

    /// Take (and clear) the adapter's latest **pass-source warning** (#389), if any.
    ///
    /// `Some` means the timer's plugin advertised `live_pass` but did not deliver laps RotorHazard
    /// itself reported, so the adapter fell back to the `current_laps` snapshot. Laps keep flowing;
    /// the operator needs to know the plugin is faulty on this timer. The warning is written to stderr
    /// when it fires; this is the hook for the driver to surface it in the Director UI too — a
    /// silent degrade is exactly what made #389 undiagnosable.
    pub fn take_pass_warning(&self) -> Option<String> {
        self.adapter
            .lock()
            .expect("adapter lock")
            .take_pass_warning()
    }

    /// Whether the connected plugin advertised the `live_pass` capability, i.e. whether the plugin
    /// is the selected pass source (#389). `false` against a stock RH or a plugin whose lap-atom
    /// self-check failed.
    pub fn plugin_live_pass(&self) -> bool {
        self.adapter
            .lock()
            .expect("adapter lock")
            .plugin_live_pass()
    }

    /// Wait up to `timeout` for the GridFPV plugin's `gridfpv_hello_ack` (D16, S1), returning the
    /// [`PluginHello`] if a plugin-equipped RH answered, or `None` if none did (a stock RH — the
    /// guided-install case). Blocking poll (small sleeps): the driver calls this on its own thread
    /// right after [`connect`](Self::connect), so it never stalls the async runtime. The
    /// `gridfpv_hello` probe is emitted by `connect`, so by the time this is called the reply may
    /// already be in; we poll rather than block on a channel to keep the transport lock-free.
    pub fn wait_for_plugin(&self, timeout: Duration) -> Option<PluginHello> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(hello) = self.hello.lock().expect("plugin-hello lock").clone() {
                return Some(hello);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Wait up to `timeout` for the timer to say **how many nodes it has** (#412), or `None` if it
    /// never did.
    ///
    /// `connect` asks for `frequency_data` (and `enter_and_exit_at_levels`) in its warm-up
    /// `load_data`, so by the time the driver calls this the answer is usually already in. Blocking
    /// poll with small sleeps, exactly like [`wait_for_plugin`](Self::wait_for_plugin), and called
    /// from the driver thread so it never stalls the async runtime.
    ///
    /// `None` is not a failure to handle loudly — a stock RH that answers neither `load_data` type
    /// simply leaves GridFPV on its configured width, which is where it was before #412.
    pub fn wait_for_reported_nodes(&self, timeout: Duration) -> Option<u32> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(nodes) = *self.reported_nodes.lock().expect("reported-nodes lock") {
                return Some(nodes);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// **Seat a heat's bound pilots on RotorHazard nodes** so RH records *and* attributes passes for
    /// each bound node — the laps-attribute fix. Without this, GridFPV's node→pilot binding lives only
    /// in the Director's log; RH races a current heat with **no seated pilots**, and its pass gate
    /// (`server.py` `do_pass_record_callback`: a pass on a node with `pilot_id == PILOT_ID_NONE` while
    /// a heat is current is *dismissed* "Pilot not defined") rejects every crossing → zero laps even
    /// with clear gate crossings.
    ///
    /// `seats` is the heat's `(node_index, callsign)` bind, one entry per **bound** node (unbound
    /// nodes are simply left unseated — RH won't record there, which is correct). For each seat this:
    ///   1. adds a fresh RH pilot (`add_pilot`) and learns its id (the highest from `pilot_data`),
    ///   2. names it with the GridFPV callsign (`alter_pilot { callsign }`) so RH's own view + its
    ///      "Racing heat … pilots: …" log are right,
    ///   3. assigns it to the heat's slot at that node (`alter_heat { heat, slot_id, pilot }`).
    ///
    /// Then it selects the heat as current (`set_current_heat`) so the seats take effect for the race.
    ///
    /// The heat is **freshly added** here (`add_heat`) and this is the heat the finish-time dense save
    /// then reuses (it is already current + savable), so seating doubles as the savable-heat selection
    /// — there is no separate empty heat. Returns the seated heat id (so the driver records it and the
    /// finish path skips re-adding an empty one), or `None` if the seating could not complete (no
    /// `heat_data`/`pilot_data` response within the bounded waits — the caller falls back to the
    /// practice-mode flow, which still records via the `current_heat is HEAT_ID_NONE` gate branch).
    ///
    /// Runs **synchronously on the driver thread** (like the finish dance) so its emits stay ordered
    /// and off the socket callback. Best-effort and bounded: a quirky/slow RH that never answers
    /// `heat_data`/`pilot_data` times out rather than stalling staging. A failed emit on a dropped
    /// socket surfaces as `Err` so the caller reconnects.
    pub fn seat_heat(&self, seats: &[(u64, String)]) -> Result<Option<u64>, rust_socketio::Error> {
        if seats.is_empty() {
            return Ok(None);
        }
        // Add a fresh heat and learn its per-node slot ids (the `HeatNode` PKs `alter_heat` targets).
        self.adapter.lock().unwrap().take_heat_slots();
        self.client.emit("add_heat", Payload::Text(vec![]))?;
        self.client
            .emit("load_data", json!({ "load_types": ["heat_data"] }))?;
        let Some((heat_id, node_to_slot)) = self.wait_for_heat_slots() else {
            return Ok(None);
        };

        // Learn the current highest pilot id BEFORE creating any, so each `add_pilot` can be
        // identified as "the new id strictly greater than the floor" rather than the bare max — RH
        // also broadcasts `pilot_data` on an `alter_pilot` rename, so a stale broadcast carrying an
        // *existing* (lower-or-equal) id must not be mistaken for the just-added pilot.
        self.client
            .emit("load_data", json!({ "load_types": ["pilot_data"] }))?;
        // The current highest pilot id (0 if RH has no pilots yet); the next created pilot exceeds it.
        let mut pilot_floor = self.wait_for_pilot_above(i64::MIN).unwrap_or(0);

        let mut seated_any = false;
        for (node_index, callsign) in seats {
            let Some(&slot_id) = node_to_slot.get(&(*node_index as usize)) else {
                // The freshly-added heat has no slot for this node index (more bound nodes than RH
                // nodes) — skip it; RH won't record there, which is the correct degradation.
                continue;
            };
            // Create a pilot and learn its id (the new id strictly above the running floor).
            self.adapter.lock().unwrap().take_pilot_ids();
            self.client.emit("add_pilot", Payload::Text(vec![]))?;
            self.client
                .emit("load_data", json!({ "load_types": ["pilot_data"] }))?;
            let Some(pilot_id) = self.wait_for_pilot_above(pilot_floor) else {
                continue;
            };
            pilot_floor = pilot_id;
            // Name it with the GridFPV callsign so RH's own view + its staging log show the callsign.
            self.client.emit(
                "alter_pilot",
                json!({ "pilot_id": pilot_id, "callsign": callsign }),
            )?;
            // Seat the pilot on the heat's node slot.
            self.client.emit(
                "alter_heat",
                json!({ "heat": heat_id, "slot_id": slot_id, "pilot": pilot_id }),
            )?;
            seated_any = true;
        }

        // Only make the heat current (and claim it as savable + seated) if at least one bound pilot
        // was actually assigned. If nothing seated — every node slot missing, or every `add_pilot`
        // timed out — selecting this empty heat as current would make RH dismiss every crossing
        // ("Pilot not defined") → zero laps, strictly worse than NOT selecting it. Returning `None`
        // leaves the connection in practice mode (no current heat), where RH still records via its
        // `current_heat is HEAT_ID_NONE` gate branch, and the finish path adds its own savable heat.
        if !seated_any {
            return Ok(None);
        }
        // Make the seated heat current so the seats take effect (and so it is the savable heat the
        // finish-time dense pull reuses — no separate empty heat).
        self.client
            .emit("set_current_heat", json!({ "heat": heat_id }))?;
        Ok(Some(heat_id as u64))
    }

    /// Wait (bounded) for a `heat_data` response after [`seat_heat`]'s `add_heat`, returning the
    /// **freshest** heat (highest id) and its `node_index → slot_id` map — but only once that map is
    /// **non-empty** (RH may broadcast a `heat_data` for a freshly-added heat before its `HeatNode`
    /// rows carry a `node_index`; accepting an empty map would seat nobody yet still mark the heat
    /// savable). `None` on timeout.
    fn wait_for_heat_slots(&self) -> Option<(i64, std::collections::HashMap<usize, i64>)> {
        let deadline = Instant::now() + SEAT_RESPONSE_TIMEOUT;
        loop {
            {
                let mut a = self.adapter.lock().unwrap();
                let slots = a.take_heat_slots();
                // Pick the freshest heat (highest id) that actually carries node→slot mappings.
                if let Some((&heat_id, node_to_slot)) = slots
                    .iter()
                    .filter(|(_, m)| !m.is_empty())
                    .max_by_key(|(id, _)| **id)
                {
                    return Some((heat_id, node_to_slot.clone()));
                }
            }
            if Instant::now() >= deadline || !self.is_alive() {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Wait (bounded) for a `pilot_data` response carrying a pilot id **strictly greater than**
    /// `floor`, returning that id (the highest such) — used to identify the pilot a preceding
    /// `add_pilot` just created. Passing `i64::MIN` returns the current highest id (the seating
    /// floor). `None` on timeout (no id above `floor` arrived).
    fn wait_for_pilot_above(&self, floor: i64) -> Option<i64> {
        let deadline = Instant::now() + SEAT_RESPONSE_TIMEOUT;
        loop {
            {
                let mut a = self.adapter.lock().unwrap();
                let id = a
                    .take_pilot_ids()
                    .into_iter()
                    .filter(|id| *id > floor)
                    .max();
                if let Some(id) = id {
                    return Some(id);
                }
            }
            if Instant::now() >= deadline || !self.is_alive() {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Whether the socket is still live (#105). The reserved `close`/`error` handlers flip this to
    /// `false` the moment `rust_socketio` observes the connection drop; with `.reconnect(false)`
    /// that is final, so this is the driver's source of truth for detecting a drop — unlike an
    /// emit, which a buffering client could still report as `Ok`.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// Take everything translated since the last call.
    pub fn events(&self) -> Vec<Event> {
        let mut guard = self.events.lock().unwrap();
        std::mem::take(&mut *guard)
    }

    /// Stage (and auto-start) a race — for driving a dockerized RH from tests.
    pub fn stage_race(&self) -> Result<(), rust_socketio::Error> {
        // 0-arg server handler: emit with no payload args.
        self.client.emit("stage_race", Payload::Text(vec![]))
    }

    /// **Put RotorHazard in a state where it makes no race decisions** — it detects crossings,
    /// Grid referees.
    ///
    /// Two referees is how #403 happened: an open-practice heat in which the pilot flew 8 gate
    /// crossings and Grid recorded 4, because RotorHazard declared a winner at lap 3 and numbered
    /// the rest `-1`, marking them late/deleted *at source*. Grid correctly skips deleted laps, so
    /// four crossings the timer had detected perfectly were gone before Grid could see them.
    ///
    /// Two paths, in preference order:
    ///
    /// 1. **The Grid-owned format** (#404/#405, the plugin's [`CAP_OWNED_FORMAT`]). The plugin
    ///    creates a `GridFPV` race format once — every conduct field neutral: no win condition, no
    ///    lap cap, no time limit, no team aggregation, holeshot lap numbering, no staging tones or
    ///    start delay — and selects it. The race director's own format is **never touched**, so the
    ///    takeover is reversible by construction (`race.raceformat = <theirs>` puts it back) with
    ///    no snapshot/restore bookkeeping to get wrong and nothing left behind if Grid dies
    ///    mid-race. See [`select_owned_format`](Self::select_owned_format).
    /// 2. **The legacy in-place path** — kept working for the transition, while plugin builds older
    ///    than the owned format are still in the field. It zeroes the *staging* half of whichever
    ///    format is current (tones + start delays + `unlimited_time`) by mutating the RD's own row.
    ///    That is what shipped before this change; it fixes the start but leaves RH's *stopping*
    ///    and *counting* decisions intact, which is precisely #403. Engaging it is announced.
    ///
    /// Either way this is about *staging*: RotorHazard rejects `alter_race_format`/`set_race_format`
    /// during an active race, so the bridge calls this at **Stage** (pre-Armed, pre-go), never at
    /// the start instant. Idempotent — the driver calls it again immediately before `stage_race`,
    /// because seating a heat can switch the effective format.
    ///
    /// The only residual delay is RotorHazard's fixed `RACE_START_DELAY_EXTRA_SECS` prestage (a
    /// `Config.GENERAL` value, ~0.9 s by default; the plugin zeroes it at load). It is *constant*
    /// and so does not affect lap-time correctness: RH timestamps every pass relative to its own
    /// race start and Grid derives lap times as pass-to-pass deltas on that clock, so a constant
    /// offset cancels out.
    pub fn prepare_instant_start(&self) -> Result<(), rust_socketio::Error> {
        if self.select_owned_format()? {
            return Ok(());
        }
        self.neutralize_active_format()
    }

    /// Ask the plugin to select its **Grid-owned `GridFPV` race format**; `Ok(true)` when the timer
    /// is confirmed racing on it (#404).
    ///
    /// `Ok(false)` means the caller must fall back to [`neutralize_active_format`] — and by then
    /// the reason has already been announced through the [`crate::diag`] sink, once per connection.
    /// The confirm only blocks the **first** selection on a connection: the Director asks at the
    /// heat's Stage transition, seconds of Armed hold before "go", so waiting there is free, while
    /// the second call (immediately before `stage_race`, at the go instant) is fire-and-forget
    /// against an already-proven format. A later failure still surfaces — the `gridfpv_format_ack`
    /// handler announces any `ok: false` whenever it arrives.
    fn select_owned_format(&self) -> Result<bool, rust_socketio::Error> {
        let (advertised, selected, gave_up) = {
            // Clear any stale failure — the plugin's load-time error, or a previous stage's — so
            // only an ack for the request we are about to send can end the confirm below. The
            // plugin retries the create on every request, so yesterday's reason is not evidence.
            let mut owned = self.owned_format.lock().expect("owned-format lock");
            owned.error = None;
            (owned.advertised, owned.selected, owned.announced)
        };
        // No plugin, or a plugin build predating the owned format: the legacy path, announced once.
        if !advertised {
            return Ok(false);
        }
        self.client.emit("gridfpv_select_format", json!({}))?;
        if selected {
            return Ok(true);
        }
        // Already gave up (and said so) on this connection: the request above still went out, so a
        // plugin that recovers is picked up at the next stage — but don't re-block every stage
        // waiting for one that won't.
        if gave_up {
            return Ok(false);
        }
        // First selection on this link: confirm before racing on it.
        let deadline = Instant::now() + FORMAT_ACK_TIMEOUT;
        loop {
            {
                let owned = self.owned_format.lock().expect("owned-format lock");
                if owned.selected {
                    return Ok(true);
                }
                // A failed ack already announced itself in its handler; stop waiting.
                if owned.error.is_some() {
                    break;
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut owned = self.owned_format.lock().expect("owned-format lock");
        if !owned.announced {
            owned.announced = true;
            let why = match owned.error.as_ref() {
                Some(error) => format!("it reported: {error}"),
                None => format!("it did not answer within {}s", FORMAT_ACK_TIMEOUT.as_secs()),
            };
            crate::diag!(
                "gridfpv: rotorhazard: the GridFPV plugin advertised `{CAP_OWNED_FORMAT}` but its \
                 race format is not selected — {why}. Falling back to altering this timer's own \
                 race format, which neutralises its START but NOT its stopping or counting: \
                 RotorHazard may still declare a winner and delete later crossings at source (#403)"
            );
        }
        Ok(false)
    }

    /// The **legacy** neutralisation: mutate whichever race format is currently selected on the
    /// timer, zeroing its staging so `stage_race` transitions straight to RACING.
    ///
    /// This is what shipped before the Grid-owned format, kept working for the transition period —
    /// a stock RotorHazard, or a plugin build older than [`CAP_OWNED_FORMAT`], still needs *some*
    /// neutralisation, and losing the instant start would be a worse regression than the gap this
    /// leaves. But understand what it does not do: it zeroes four *starting* fields and
    /// `unlimited_time`, and says nothing about **stopping** or **counting**. Whatever
    /// `win_condition` / `number_laps_win` the RD last set still governs the race, which is #403.
    /// It also edits the RD's own row, which is why #404 could not answer "restore, or don't?".
    ///
    /// Both of those are why the owned-format path above exists and is preferred. Engaging this
    /// one is announced once per connection so a timer running on the weaker guarantee is never a
    /// silent condition.
    ///
    /// Targets the current format learned from the `race_status` stream, then re-selects it so
    /// `RaceContext.race.format` picks up the zeroed staging (altering the row alone does not
    /// refresh the in-memory current-format object on every RH build). A no-op (best-effort `Ok`)
    /// if no current format id has been learned yet. Idempotent.
    fn neutralize_active_format(&self) -> Result<(), rust_socketio::Error> {
        {
            let mut owned = self.owned_format.lock().expect("owned-format lock");
            if !owned.announced {
                owned.announced = true;
                crate::diag!(
                    "gridfpv: rotorhazard: no GridFPV plugin race format on this timer — Grid is \
                     altering the timer's own active format to zero its staging. That does NOT \
                     neutralise RotorHazard's win condition or lap cap, so a heat run past the \
                     timer's configured lap limit can still lose crossings (#403). Install/update \
                     the GridFPV plugin to race on a Grid-owned format instead."
                );
            }
        }
        let Some(format_id) = *self.current_format.lock().expect("current-format lock") else {
            // No format id known yet (no `race_status` folded): cannot target a format. Best-effort
            // no-op — staging then keeps whatever the format ships, which the reset/stage flow still
            // tolerates (the start is just not guaranteed instant on this connection).
            return Ok(());
        };
        self.client.emit(
            "alter_race_format",
            json!({
                "format_id": format_id,
                "staging_fixed_tones": 0,
                "staging_delay_tones": 0,
                "start_delay_min_ms": 0,
                "start_delay_max_ms": 0,
                "unlimited_time": 1,
            }),
        )?;
        // Re-select the format as current so `RaceContext.race.format` picks up the zeroed staging
        // (altering the row alone does not refresh the in-memory current-format object on every RH).
        self.client
            .emit("set_race_format", json!({ "race_format": format_id }))
    }

    /// The id of the **Grid-owned `GridFPV` race format** on this connection, once the plugin has
    /// named one. `None` against a stock RH, an older plugin build, or a plugin that could not
    /// create it.
    pub fn owned_format_id(&self) -> Option<i64> {
        self.owned_format.lock().expect("owned-format lock").id
    }

    /// Whether this connection is confirmed racing on the Grid-owned race format — i.e. whether
    /// RotorHazard has been stripped of *every* race decision, not just its staging (#403/#404).
    /// `false` means the legacy in-place path is in use (and has been announced).
    pub fn owned_format_selected(&self) -> bool {
        self.owned_format
            .lock()
            .expect("owned-format lock")
            .selected
    }

    /// RotorHazard's currently selected race-format id, as last reported by the `race_status`
    /// stream. `None` until the first status carrying one has been folded.
    pub fn current_format_id(&self) -> Option<i64> {
        *self.current_format.lock().expect("current-format lock")
    }

    /// **Give a race format a lap-count win condition** — a driving helper that recreates #403's
    /// field configuration on a disposable RotorHazard, so a regression test can prove Grid no
    /// longer inherits it.
    ///
    /// `win_condition` takes RotorHazard's `RHRace.WinCondition` values (`2` = `FIRST_TO_LAP_X`,
    /// the one that bit); `number_laps_win` is the cap. With both set, RH marks a node finished at
    /// `lap_number >= number_laps_win` and flags every later crossing late — `RHRace.py` then sets
    /// `lap_data.deleted`, and the crossing never reaches Grid at all. Never for a production
    /// timer: this deliberately mis-configures the format it targets.
    pub fn set_race_format_win_condition(
        &self,
        format_id: i64,
        win_condition: i64,
        number_laps_win: i64,
    ) -> Result<(), rust_socketio::Error> {
        self.client.emit(
            "alter_race_format",
            json!({
                "format_id": format_id,
                "win_condition": win_condition,
                "number_laps_win": number_laps_win,
            }),
        )?;
        self.client
            .emit("set_race_format", json!({ "race_format": format_id }))
    }

    /// Inject a simulated pass on `node` (0-based) — driving helper for tests.
    pub fn simulate_lap(&self, node: u64) -> Result<(), rust_socketio::Error> {
        self.client.emit("simulate_lap", json!({ "node": node }))
    }

    /// **Tune** `node` (0-based) to `frequency` MHz (race redesign Slice 4a; #413) — the engine
    /// allocates the channel, the adapter applies it (RE §7.3). Emits RotorHazard's `set_frequency`
    /// handler; the server retunes that node's receiver. Best-effort: a failed emit on a dropped
    /// socket surfaces as an `Err` the caller logs.
    ///
    /// ## `label` is not decoration — it is half the write
    ///
    /// `on_set_frequency` accepts `{ node, frequency, band?, channel? }` (`server.py`) and stores
    /// the band/channel pair on the **active profile** when it is given. Emitting the frequency
    /// alone leaves RotorHazard's own UI showing a bare number where its channel label goes, and an
    /// RD who validates a channel change by refreshing that page reads an unlabelled frequency as
    /// *"it half worked"*. So a caller that knows the catalog entry — the Tune page's channel write
    /// does (#413) — passes `Some(("Raceband", "R7"))`, and the two keys are simply **omitted**
    /// (rather than sent as nulls) when it does not: RotorHazard leaves whatever label it had in
    /// place, which is better than overwriting it with an empty string.
    ///
    /// The frequency is still the authoritative half. `label` never changes what the receiver tunes
    /// to; it only decides what RotorHazard's screen calls it.
    pub fn set_frequency(
        &self,
        node: u64,
        frequency: u16,
        label: Option<(&str, &str)>,
    ) -> Result<(), rust_socketio::Error> {
        let mut payload = json!({ "node": node, "frequency": frequency });
        if let (Some((band, channel)), Some(map)) = (label, payload.as_object_mut()) {
            map.insert("band".into(), json!(band));
            map.insert("channel".into(), json!(channel));
        }
        self.client.emit("set_frequency", payload)
    }

    /// **Set node `node`'s enter threshold** to `level` (#355) — the calibration write.
    ///
    /// Emits RotorHazard's `set_enter_at_level` handler with `{ node, enter_at_level }`, where
    /// `node` is the 0-based seat index. **Verified identical on v4.3.0 and v4.4.0**
    /// (`server.py::on_set_enter_at_level`): same event name, same two payload keys; v4.4.0 only
    /// adds an `int(… or 0)` coercion around the value. It carries **no authentication** — the
    /// `@requires_auth` decorators in that file guard Flask HTTP routes, not socket handlers — so
    /// the Director can calibrate on the socket it is already holding, with no plugin involved.
    ///
    /// The handler runs `calibration.py::set_enter_at_level`, which writes the active profile's
    /// `enter_ats`, **pushes the level to the timing hardware** (`interface.set_enter_at_level`),
    /// and fires `Evt.ENTER_AT_LEVEL_SET`.
    ///
    /// ## Two traps this exists to avoid
    ///
    /// * **`level` must never be `0`.** `calibration.py` tests the value for *truthiness*, so a `0`
    ///   is read as "re-read the level off the node" and the old threshold survives — while the
    ///   write looks perfectly successful. Callers clamp to a minimum of 1 (`RSSI_MIN`).
    /// * **RotorHazard does not echo this.** The handler emits nothing at all, so an `Ok` here means
    ///   only that the emit was accepted. [`request_thresholds`](Self::request_thresholds) is the
    ///   readback, and the caller fires it after a write so the confirming
    ///   `enter_and_exit_at_levels` broadcast lands on this socket.
    ///
    /// Callers **must** gate this on heat phase — a threshold that moves mid-race changes what
    /// counts as a lap while it is being counted. This layer only moves the bytes.
    pub fn set_enter_at_level(&self, node: u64, level: u32) -> Result<(), rust_socketio::Error> {
        self.client.emit(
            "set_enter_at_level",
            json!({ "node": node, "enter_at_level": level }),
        )
    }

    /// **Set node `node`'s exit threshold** to `level` (#355) — the twin of
    /// [`set_enter_at_level`](Self::set_enter_at_level), and everything said there applies here.
    ///
    /// Emits `set_exit_at_level` with `{ node, exit_at_level }` (`server.py::on_set_exit_at_level`,
    /// identical on v4.3.0 and v4.4.0). `0` is falsy to `calibration.py` and silently re-reads the
    /// node's own level instead of setting one; there is no echo, so confirmation is by readback.
    pub fn set_exit_at_level(&self, node: u64, level: u32) -> Result<(), rust_socketio::Error> {
        self.client.emit(
            "set_exit_at_level",
            json!({ "node": node, "exit_at_level": level }),
        )
    }

    /// Set RotorHazard's **minimum lap time** (general setting `MIN_LAP_TIME`, in **seconds**) —
    /// a driving helper so the sim/test harness does not trip RH's "Pass record under lap
    /// minimum" filter.
    ///
    /// RotorHazard defaults `MIN_LAP_TIME` to **10s** and logs `Pass record under lap minimum (10)`
    /// for any crossing closer than that to the previous one — which the test harness's rapid
    /// `simulate_lap` injections (and short-lap sim CSVs) routinely are, so RH spams the warning.
    /// Emitting RotorHazard's `set_option` handler with `{ option: "MIN_LAP_TIME", value: "<sec>" }`
    /// persists the setting server-side; passing `0` disables the minimum entirely so every
    /// short sim lap records cleanly. Best-effort (a failed emit on a dropped socket is the
    /// caller's to log); intended for the disposable test RH only, never a production timer.
    pub fn set_min_lap_time(&self, seconds: u64) -> Result<(), rust_socketio::Error> {
        self.client.emit(
            "set_option",
            json!({ "option": "MIN_LAP_TIME", "value": seconds.to_string() }),
        )
    }

    /// Stop the current race — driving helper for tests.
    pub fn stop_race(&self) -> Result<(), rust_socketio::Error> {
        self.client.emit("stop_race", Payload::Text(vec![]))
    }

    /// **Re-request the per-node enter/exit detection thresholds** — `load_data` with
    /// `{"load_types": ["enter_and_exit_at_levels"]}`, which RotorHazard answers with an
    /// `enter_and_exit_at_levels` emit addressed to this socket (`nobroadcast`).
    ///
    /// This is the **calibration readback** (#355), not merely a test helper. Neither
    /// `set_enter_at_level` nor `set_exit_at_level` echoes, so the only way to learn whether a write
    /// landed is to ask: the driver fires this immediately after a calibration emit, the adapter
    /// parses the reply into the per-node thresholds, and the Tune page sees the value come back on
    /// its next `GET /timers/{id}/signal` poll.
    ///
    /// ⚠️ It reads **RotorHazard's active profile row**, not the node. `RHUI.emit_enter_and_exit_at_levels`
    /// serialises `profile.enter_ats` / `profile.exit_ats`, and `calibration.py` writes that row
    /// *before* `interface.set_enter_at_level` — which then drops the value if
    /// `Node.is_valid_rssi` rejects it. So the readback confirms "RotorHazard took it", which is one
    /// step short of "the detector holds it"; keeping the level inside the valid range is what
    /// closes the gap.
    ///
    /// Also used as a driving helper so a test can re-capture thresholds after draining the
    /// connect-time burst.
    pub fn request_thresholds(&self) -> Result<(), rust_socketio::Error> {
        self.client.emit(
            "load_data",
            json!({ "load_types": ["enter_and_exit_at_levels"] }),
        )
    }

    /// Add a heat (`add_heat`, 0-arg) — a driving helper for the dense-marshal-data test so a saved
    /// heat exists to select (RotorHazard's per-pilotrace marshal path needs a saved race).
    pub fn add_heat(&self) -> Result<(), rust_socketio::Error> {
        self.client.emit("add_heat", Payload::Text(vec![]))
    }

    /// Persist the just-finished race (`save_laps`, 0-arg) so its per-pilotrace history is written to
    /// the DB and becomes pullable via `get_pilotrace` — a driving helper for the dense-history test.
    pub fn save_laps(&self) -> Result<(), rust_socketio::Error> {
        self.client.emit("save_laps", Payload::Text(vec![]))
    }

    /// Ensure a **savable current heat** exists on RotorHazard so the next run persists its dense
    /// per-tick RSSI history (the marshaling Slice 1 / path-2 precondition).
    ///
    /// RotorHazard only writes a run's `history_values`/`history_times` (the dense trace its marshal
    /// page reviews, pulled via `current_marshal_data` / `get_pilotrace`) when a heat is current —
    /// `on_save_laps` and `emit_race_marshal_data` both no-op while `current_heat == HEAT_ID_NONE`,
    /// the default in practice mode. The production staging path drives RH through
    /// `stop_race`/`discard_laps`/`stage_race` but never selects a heat, so without this the dense
    /// pull always comes back empty and only the coarse streamed [`SignalChunk`]s survive.
    ///
    /// This adds a fresh heat (`add_heat`) and **requests** `heat_data`; the `heat_data` handler
    /// stashes the newest heat id, which the **driver** then reads via [`take_savable_heat`] and
    /// selects synchronously (`set_current_heat`) before staging. Selection is deliberately NOT done
    /// in the socket callback: `heat_data` is broadcast on every heat mutation, so an emit-per-event
    /// would feed back and flood the link (the regression that dropped a staging connection).
    /// Best-effort: a failed emit on a dropped link just leaves the coarse trace, which the reconnect
    /// path tolerates. Adding a heat each call is acceptable for the dockerized test rig; a future
    /// refinement could reuse an existing empty heat instead of always adding one.
    pub fn ensure_savable_heat(&self) -> Result<(), rust_socketio::Error> {
        self.client.emit("add_heat", Payload::Text(vec![]))?;
        self.client
            .emit("load_data", json!({ "load_types": ["heat_data"] }))
    }

    /// Select RotorHazard's **current heat** (`set_current_heat`, `{ heat: <id> }`) — a driving
    /// helper for the dense-marshal-data test.
    ///
    /// RotorHazard's `emit_race_marshal_data` only answers when a **saved heat** is current
    /// (`current_heat != HEAT_ID_NONE`); the default practice mode has no heat, so the test selects
    /// one before the race so the post-race dense history can be pulled. Best-effort.
    pub fn set_current_heat(&self, heat: u64) -> Result<(), rust_socketio::Error> {
        self.client
            .emit("set_current_heat", json!({ "heat": heat }))
    }

    /// Request RotorHazard's dense **post-race marshal data** (`current_race_marshal`) — the
    /// request-driven `current_marshal_data` with each node's `history_values`/`history_times`.
    ///
    /// In normal operation the adapter auto-requests this on the heat-end (`DONE`) transition (see
    /// the `race_status` handler); this explicit helper lets a test pull it on demand after staging a
    /// race down, so the dense-history capture can be asserted deterministically. RotorHazard only
    /// answers while the race is `DONE` (`emit_race_marshal_data` returns early otherwise).
    pub fn request_marshal_data(&self) -> Result<(), rust_socketio::Error> {
        self.client
            .emit("current_race_marshal", Payload::Text(vec![]))
    }

    /// Discard the current race's laps, returning RotorHazard to a READY state —
    /// driving helper so a test can stage cleanly regardless of prior state.
    pub fn discard_laps(&self) -> Result<(), rust_socketio::Error> {
        self.client.emit("discard_laps", Payload::Text(vec![]))
    }

    /// Probe that the socket is still live without driving the race (#105). Re-requests the current
    /// per-node data — a cheap, idempotent server query the adapter's dedup makes side-effect-free —
    /// so a quiet-but-healthy idle link confirms it is up, while a dropped socket surfaces an emit
    /// error the caller can treat as a disconnect. Used by the persistent connection's monitor.
    pub fn probe_liveness(&self) -> Result<(), rust_socketio::Error> {
        self.client
            .emit("load_data", json!({ "load_types": ["node_data"] }))
    }

    /// **Restart the RotorHazard server** — re-execute its process so it re-imports its plugins
    /// (#386).
    ///
    /// RotorHazard imports every plugin **once, at startup**, so a freshly-dropped-in
    /// `plugins/gridfpv/` does nothing until the server restarts. RH exposes that restart on the
    /// socket we already hold — `@SOCKET_IO.on('restart_server')` / `on_restart_server()`
    /// ("Re-execute the current process"), v4.4.0 `server.py:1881`, identical on v4.3.0. It carries
    /// **no authentication**: the `@requires_auth` decorators in that file guard Flask HTTP routes,
    /// not this socket handler. So the Director can complete the guided plugin install without the
    /// RD ever opening RotorHazard's web UI.
    ///
    /// **Deliberately the only power control we wire.** `server.py` exposes `shutdown_pi` and
    /// `reboot_pi` right beside this one; both take the RD's timing hardware *down* rather than
    /// bringing it back, so they stay out of reach and must not be added here.
    ///
    /// The emit is fire-and-forget: RH re-execs immediately, so the socket drops within a moment
    /// and the connection's `close` handler flips [`is_alive`](Self::is_alive) to `false`. The
    /// persistent driver then reconnects with backoff and **re-probes the plugin on the new
    /// connection**, which is what flips a timer's plugin presence `Missing → Present` with no
    /// further plumbing. An `Ok` here means only that the emit was accepted, never that RH came
    /// back — the reconnect loop is the source of truth for that.
    ///
    /// Restarting mid-race is destructive (it takes the timing hardware down with the race on it),
    /// so callers **must** gate this on heat phase; this layer only moves the bytes.
    pub fn restart_server(&self) -> Result<(), rust_socketio::Error> {
        // 0-arg server handler: emit with no payload args.
        self.client.emit("restart_server", Payload::Text(vec![]))
    }

    /// Disconnect from the server, **returning the adapter** so the persistent driver can carry its
    /// dedup / `last_race_status` into the next connection (#105). Reusing the adapter is what keeps
    /// a mid-race reconnect from double-counting: RotorHazard re-sends the full `current_laps`
    /// snapshot on the new socket, and only a dedup that already saw those laps suppresses the
    /// replay. The socket disconnect itself is best-effort — even if it errors the adapter (the
    /// state we care about) is recovered. The `Arc<Mutex<…>>` is uniquely held here once the socket
    /// is torn down (the `rust_socketio` handler clones are dropped with the client), so unwrapping
    /// it back to an owned adapter cannot fail in practice; on the off chance it is still shared we
    /// fall back to cloning the inner adapter (its dedup/state clone is cheap and lossless).
    pub fn disconnect(self) -> RotorHazardAdapter {
        self.client.disconnect().ok();
        // Drop the client first so its registered socket handlers (which hold `adapter` clones) are
        // released, leaving this connection the sole owner of the adapter handle.
        drop(self.client);
        match Arc::try_unwrap(self.adapter) {
            Ok(mutex) => mutex.into_inner().expect("adapter mutex poisoned"),
            Err(shared) => shared.lock().expect("adapter mutex poisoned").clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: serde_json::Value) -> Payload {
        Payload::Text(vec![value])
    }

    /// A stock RotorHazard 4.3.0 heartbeat, as it arrives at idle with no race running.
    fn heartbeat(rssi: [f32; 4], crossing: [bool; 4]) -> Payload {
        text(json!({
            "current_rssi": rssi,
            "frequency": [5658, 5695, 5760, 0],
            "loop_time": [1200, 1180, 1210, 1195],
            "crossing_flag": crossing,
        }))
    }

    /// The gate is checked **before** the payload is parsed — the #392 hazard, structurally.
    ///
    /// The proof is the outcome on an *unreadable* payload with the subscription closed: a parse
    /// that ran would report `Unreadable`. Reporting `Gated` can only mean the deserializer was
    /// never reached. That is the property that matters, because the cost being avoided is the
    /// parse itself (10–100 frames/sec on the socket callback thread), not the fold after it.
    #[test]
    fn the_subscription_gate_is_checked_before_the_payload_is_parsed() {
        let tap = SignalTap::default();
        // Closed subscription, a payload no `RawHeartbeat` could ever come out of.
        let garbage = text(json!("not an object at all"));
        assert_eq!(tap_heartbeat(&tap, &garbage), TapOutcome::Gated);
        assert_eq!(tap_crossing(&tap, &garbage), TapOutcome::Gated);
        // A perfectly good heartbeat is dropped just as early, and leaves nothing behind.
        assert_eq!(
            tap_heartbeat(&tap, &heartbeat([40.0, 41.0, 42.0, 43.0], [false; 4])),
            TapOutcome::Gated
        );
        assert!(tap.take().is_empty(), "a closed tap stores nothing");

        // Open the subscription: now — and only now — the same garbage reaches the parser and is
        // reported as what it is.
        tap.set_capturing(true);
        assert_eq!(tap_heartbeat(&tap, &garbage), TapOutcome::Unreadable);
        assert_eq!(
            tap_heartbeat(&tap, &heartbeat([40.0, 41.0, 42.0, 43.0], [false; 4])),
            TapOutcome::Folded
        );
        assert_eq!(tap.take().len(), 4);
    }

    /// Closing the subscription leaves nothing behind: a lapsed Tune page must not keep a node's
    /// last RSSI alive in memory, and must not have it reappear if the page comes back.
    #[test]
    fn closing_the_subscription_empties_the_store() {
        let tap = SignalTap::default();
        tap.set_capturing(true);
        tap_heartbeat(&tap, &heartbeat([40.0, 41.0, 42.0, 43.0], [false; 4]));
        assert_eq!(tap.take().len(), 4);
        assert!(
            tap.set_capturing(false),
            "the gate reports its previous state"
        );
        assert!(tap.take().is_empty());
        assert!(
            !tap.set_capturing(true),
            "and reports the rising edge as such"
        );
        assert!(tap.take().is_empty(), "a reopened tap starts from nothing");
    }

    /// **Both feeds surface.** `get_heartbeat_json` carries only rssi / frequency / loop-time /
    /// crossing; every peak, nadir and pass count a tuning panel shows comes from `node_data`, and
    /// the thresholds from `enter_and_exit_at_levels`. A snapshot missing either half cannot answer
    /// the question the RD is asking, so all three must land on the same [`NodeTick`].
    #[test]
    fn both_rotorhazard_feeds_land_on_the_same_node() {
        let tap = SignalTap::default();
        tap.set_capturing(true);
        tap_heartbeat(
            &tap,
            &heartbeat([48.0, 12.0, 0.0, 0.0], [true, false, false, false]),
        );
        tap.note_node_data(&RawNodeData {
            pass_peak_rssi: vec![118.0, 0.0, 0.0, 0.0],
            node_peak_rssi: vec![132.0, 0.0, 0.0, 0.0],
            node_nadir_rssi: vec![12.0, 0.0, 0.0, 0.0],
            pass_nadir_rssi: vec![41.0, 0.0, 0.0, 0.0],
            debug_pass_count: vec![7, 0, 0, 0],
        });
        tap.note_levels(&RawEnterExitLevels {
            enter_at_levels: vec![90.0, 90.0, 90.0, 90.0],
            exit_at_levels: vec![80.0, 80.0, 80.0, 80.0],
        });

        let nodes = tap.take();
        let first = &nodes[0];
        // The heartbeat half.
        assert_eq!(first.rssi, Some(48.0));
        assert_eq!(first.frequency_mhz, Some(5658));
        assert_eq!(first.loop_time_micros, Some(1200));
        assert!(first.crossing);
        // The `node_data` half — none of which the heartbeat carries.
        assert_eq!(first.node_peak_rssi, Some(132.0));
        assert_eq!(first.node_nadir_rssi, Some(12.0));
        assert_eq!(first.pass_peak_rssi, Some(118.0));
        assert_eq!(first.pass_nadir_rssi, Some(41.0));
        assert_eq!(first.pass_count, Some(7));
        // The thresholds the tuning graph draws its handles at.
        assert_eq!(first.enter_at, Some(90.0));
        assert_eq!(first.exit_at, Some(80.0));
    }

    /// **Every node the timer reports, including ones no heat has seated.** "Is this node even
    /// alive?" is half the diagnostic a mistuned timer needs, and the tap is the layer that must
    /// not filter — the app layer's lineup remap drops off-lineup nodes, which is exactly why tune
    /// telemetry does not go through it.
    #[test]
    fn unseated_nodes_are_reported_too() {
        let tap = SignalTap::default();
        tap.set_capturing(true);
        // Four nodes; only the first two are tuned to anything and only the first has any signal.
        tap_heartbeat(
            &tap,
            &heartbeat([48.0, 11.0, 9.0, 8.0], [true, false, false, false]),
        );

        let nodes = tap.take();
        assert_eq!(
            nodes.len(),
            4,
            "no node is filtered out of a tuning snapshot"
        );
        assert!(
            nodes.iter().all(|n| n.seen),
            "every reported node is marked seen"
        );
        // Node 3 is untuned — RotorHazard reports 0 MHz, which is an absence, not a channel.
        assert_eq!(nodes[3].frequency_mhz, None);
        assert_eq!(nodes[3].rssi, Some(8.0));
    }

    /// A crossing that opens and closes between two Director samples must still light the lamp.
    /// The level (`crossing`) is last-value-wins; the edge (`crossed`) is sticky until read.
    #[test]
    fn a_crossing_between_samples_survives_as_a_sticky_edge() {
        let tap = SignalTap::default();
        tap.set_capturing(true);
        // Open and close within one sample interval.
        tap_crossing(
            &tap,
            &text(json!({ "node_index": 1, "crossing_flag": true })),
        );
        tap_crossing(
            &tap,
            &text(json!({ "node_index": 1, "crossing_flag": false })),
        );

        let nodes = tap.take();
        assert!(!nodes[1].crossing, "the level is back down");
        assert!(nodes[1].crossed, "but the edge is not lost");
        // Reading clears it: the next snapshot reports only what happened since.
        let nodes = tap.take();
        assert!(!nodes[1].crossed);
    }

    /// Some RotorHazard builds wire the crossing flag as a 0/1 int rather than a bool.
    #[test]
    fn a_numeric_crossing_flag_reads_the_same_as_a_bool() {
        let tap = SignalTap::default();
        tap.set_capturing(true);
        tap_crossing(&tap, &text(json!({ "node_index": 0, "crossing_flag": 1 })));
        assert!(tap.take()[0].crossing);
        tap_crossing(&tap, &text(json!({ "node_index": 0, "crossing_flag": 0 })));
        assert!(!tap.take()[0].crossing);
    }

    /// The per-node store is bounded by [`MAX_TUNE_NODES`], so a drifting or hostile frame cannot
    /// make "cost per tick is O(nodes)" untrue.
    #[test]
    fn the_node_store_is_capped() {
        let tap = SignalTap::default();
        tap.set_capturing(true);
        let wide: Vec<f32> = (0..10_000).map(|i| i as f32).collect();
        tap.note_heartbeat(&RawHeartbeat {
            current_rssi: wide,
            ..Default::default()
        });
        assert_eq!(tap.take().len(), MAX_TUNE_NODES);
        // An out-of-range crossing edge is dropped rather than widening the store.
        tap_crossing(
            &tap,
            &text(json!({ "node_index": 9_999, "crossing_flag": true })),
        );
        assert_eq!(tap.take().len(), MAX_TUNE_NODES);
    }

    /// Neither tune-telemetry frame is a [`Raw`], which is what makes "heartbeat data can never
    /// become an `Event`" structural rather than conventional: `translate` takes a `Raw` and there
    /// is no `Raw` to build from a heartbeat.
    #[test]
    fn the_tune_telemetry_frames_are_not_translatable_events() {
        assert!(matches!(
            decode_socket("heartbeat", &heartbeat([1.0; 4], [false; 4])),
            Decoded::Untranslated
        ));
        assert!(matches!(
            decode_socket(
                "node_crossing_change",
                &text(json!({ "node_index": 0, "crossing_flag": true }))
            ),
            Decoded::Untranslated
        ));
    }

    /// An event we don't translate is not a fault — RotorHazard broadcasts plenty. It must stay
    /// distinct from a frame we *do* translate but could not read (#400).
    #[test]
    fn an_unknown_event_is_untranslated_not_malformed() {
        assert!(matches!(
            decode_socket("some_other_rh_event", &text(json!({ "anything": 1 }))),
            Decoded::Untranslated
        ));
    }

    #[test]
    fn a_well_formed_frame_translates() {
        assert!(matches!(
            decode_socket("race_status", &text(json!({ "race_status": 1 }))),
            Decoded::Translated(Raw::RaceStatus(RawRaceStatus { race_status: 1, .. }))
        ));
    }

    /// Schema drift on an event we translate: the frame is dropped either way, but it must be
    /// *reportable*. Swallowing it is what made a plugin/RH version skew look exactly like a gate
    /// that stopped detecting (#400).
    #[test]
    fn schema_drift_on_a_translated_event_is_malformed() {
        // `current_laps` without its `current` key — the shape a version skew produces.
        let Decoded::Malformed { detail } =
            decode_socket("current_laps", &text(json!({ "laps": [] })))
        else {
            panic!("a payload we cannot read must be reported, not silently skipped");
        };
        assert!(!detail.is_empty(), "the decode error names the drift");
        // The compatibility wrapper still just says "nothing to translate".
        assert!(raw_from_socket("current_laps", &text(json!({ "laps": [] }))).is_none());
    }

    #[test]
    fn an_empty_or_non_json_payload_on_a_translated_event_is_malformed() {
        assert!(matches!(
            decode_socket("node_data", &Payload::Text(vec![])),
            Decoded::Malformed { .. }
        ));
        assert!(matches!(
            decode_socket("node_data", &Payload::Binary(vec![0u8, 1].into())),
            Decoded::Malformed { .. }
        ));
        // ...but the same payloads on an event we never read stay untranslated.
        assert!(matches!(
            decode_socket("whatever", &Payload::Text(vec![])),
            Decoded::Untranslated
        ));
    }

    /// A `-1` lap number is RotorHazard talking, not drift: once it declares a winner it numbers
    /// every later crossing `-1` (*recorded, but not counted*). Typing that field `u64` made serde
    /// fail the **whole `current_laps` frame**, so the valid laps beside it were thrown away too
    /// and the loss was charged to the malformed-frame counter — "schema drift", pointing an RD at
    /// a plugin-version mismatch, when the real fault was the timer still refereeing (#406).
    #[test]
    fn a_negative_lap_number_decodes_and_keeps_the_rest_of_its_frame() {
        // The frame as RotorHazard 4.4 sends it with a lap-count win condition in force.
        let frame = text(json!({
            "current": { "node_index": [{
                "pilot": { "callsign": "ZIP" },
                "laps": [
                    { "lap_index": 0, "lap_number": 0, "lap_time_stamp": 0.0, "late_lap": false },
                    { "lap_index": 1, "lap_number": 1, "lap_time_stamp": 31000.0, "late_lap": false },
                    // RotorHazard declared the pilot finished here and stopped counting.
                    { "lap_index": 2, "lap_number": -1, "lap_time_stamp": 62000.0,
                      "late_lap": true, "deleted": true },
                ],
            }] }
        }));

        let Decoded::Translated(raw) = decode_socket("current_laps", &frame) else {
            panic!(
                "a `-1` lap number must decode: it is RotorHazard's own value, not drift (#406)"
            );
        };

        let mut adapter = RotorHazardAdapter::new();
        adapter.translate(Raw::RaceStatus(RawRaceStatus {
            race_status: 1,
            race_heat_id: Some(1),
        }));
        let events = adapter.translate(raw);

        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, gridfpv_events::Event::Pass(_)))
                .count(),
            2,
            "the two counted laps in the frame survive — that is the regression"
        );
        assert_eq!(
            adapter.counts.uncounted, 1,
            "and the uncounted crossing is counted as one"
        );
        assert_eq!(
            adapter.counts.malformed_frames, 0,
            "nothing about this frame is malformed"
        );
    }

    /// The transport's half of the contract: a frame it drops lands on the adapter's counter, so
    /// the heat summary can say "laps may be missing, and here is why".
    #[test]
    fn a_malformed_frame_is_reported_to_the_adapter() {
        let mut adapter = RotorHazardAdapter::new();
        let Decoded::Malformed { detail } = decode_socket("current_laps", &text(json!({}))) else {
            panic!("expected a malformed frame");
        };
        adapter.note_malformed_frame("current_laps", &detail);
        assert_eq!(adapter.counts.malformed_frames, 1);
    }
}
