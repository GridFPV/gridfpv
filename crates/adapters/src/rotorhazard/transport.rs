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
    Raw, RawCurrentLaps, RawEnterExitLevels, RawHeatData, RawMarshalData, RawNodeData,
    RawPassRecord, RawPilotData, RawRaceDetails, RawRaceList, RawRaceStatus, RotorHazardAdapter,
};
use crate::Adapter;
use gridfpv_events::Event;

/// Decode a RotorHazard socket event (`event` name + its payload) into a [`Raw`].
///
/// Returns `None` for events we don't translate, or payloads that don't match the
/// expected shape — the transport simply ignores those.
pub fn raw_from_socket(event: &str, payload: &Payload) -> Option<Raw> {
    // RotorHazard wraps each emit's data in a one-element array.
    let value = match payload {
        Payload::Text(values) => values.first()?.clone(),
        _ => return None,
    };
    match event {
        "race_status" => serde_json::from_value::<RawRaceStatus>(value)
            .ok()
            .map(Raw::RaceStatus),
        "current_laps" => serde_json::from_value::<RawCurrentLaps>(value)
            .ok()
            .map(Raw::CurrentLaps),
        "pass_record" => serde_json::from_value::<RawPassRecord>(value)
            .ok()
            .map(Raw::PassRecord),
        "node_data" => serde_json::from_value::<RawNodeData>(value)
            .ok()
            .map(Raw::NodeData),
        "enter_and_exit_at_levels" => serde_json::from_value::<RawEnterExitLevels>(value)
            .ok()
            .map(Raw::EnterExitLevels),
        "current_marshal_data" => serde_json::from_value::<RawMarshalData>(value)
            .ok()
            .map(Raw::MarshalData),
        "race_list" => serde_json::from_value::<RawRaceList>(value)
            .ok()
            .map(Raw::RaceList),
        "race_details" => serde_json::from_value::<RawRaceDetails>(value)
            .ok()
            .map(Raw::RaceDetails),
        "heat_data" => serde_json::from_value::<RawHeatData>(value)
            .ok()
            .map(Raw::HeatData),
        "pilot_data" => serde_json::from_value::<RawPilotData>(value)
            .ok()
            .map(Raw::PilotData),
        _ => None,
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
    /// Capabilities the plugin declares it implements (e.g. `["hello"]`; later `"live_signal"`,
    /// `"clean_control"`, `"recalc"`). The Director keys transport decisions off these.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// The plugin's node/seat count.
    #[serde(default)]
    pub node_count: u32,
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
}

impl RotorHazardConnection {
    /// Connect to `url` (e.g. `http://localhost:5000`) and start translating the
    /// RotorHazard socket stream through `adapter`.
    pub fn connect(url: &str, adapter: RotorHazardAdapter) -> Result<Self, rust_socketio::Error> {
        let adapter = Arc::new(Mutex::new(adapter));
        let events: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
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
        let handler = |name: &'static str,
                       adapter: Arc<Mutex<RotorHazardAdapter>>,
                       sink: Arc<Mutex<Vec<Event>>>,
                       savable_heat: Arc<Mutex<Option<u64>>>,
                       current_format: Arc<Mutex<Option<i64>>>| {
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
                if let Some(raw) = raw_from_socket(name, &payload) {
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
            .on(
                "race_status",
                handler(
                    "race_status",
                    adapter.clone(),
                    events.clone(),
                    savable_heat.clone(),
                    current_format.clone(),
                ),
            )
            .on(
                "current_laps",
                handler(
                    "current_laps",
                    adapter.clone(),
                    events.clone(),
                    savable_heat.clone(),
                    current_format.clone(),
                ),
            )
            .on(
                "node_data",
                handler(
                    "node_data",
                    adapter.clone(),
                    events.clone(),
                    savable_heat.clone(),
                    current_format.clone(),
                ),
            )
            .on(
                "pass_record",
                handler(
                    "pass_record",
                    adapter.clone(),
                    events.clone(),
                    savable_heat.clone(),
                    current_format.clone(),
                ),
            )
            .on(
                "enter_and_exit_at_levels",
                handler(
                    "enter_and_exit_at_levels",
                    adapter.clone(),
                    events.clone(),
                    savable_heat.clone(),
                    current_format.clone(),
                ),
            )
            .on(
                "current_marshal_data",
                handler(
                    "current_marshal_data",
                    adapter.clone(),
                    events.clone(),
                    savable_heat.clone(),
                    current_format.clone(),
                ),
            )
            .on(
                "race_list",
                handler(
                    "race_list",
                    adapter.clone(),
                    events.clone(),
                    savable_heat.clone(),
                    current_format.clone(),
                ),
            )
            .on(
                "race_details",
                handler(
                    "race_details",
                    adapter.clone(),
                    events.clone(),
                    savable_heat.clone(),
                    current_format.clone(),
                ),
            )
            .on(
                "heat_data",
                handler(
                    "heat_data",
                    adapter.clone(),
                    events.clone(),
                    savable_heat.clone(),
                    current_format.clone(),
                ),
            )
            .on(
                "pilot_data",
                handler(
                    "pilot_data",
                    adapter.clone(),
                    events.clone(),
                    savable_heat.clone(),
                    current_format.clone(),
                ),
            )
            // The GridFPV plugin's handshake reply (D16, S1): a plugin-equipped RH answers our
            // `gridfpv_hello` (emitted below) with `gridfpv_hello_ack`. Stash it for the driver.
            .on("gridfpv_hello_ack", {
                let hello = hello.clone();
                move |payload: Payload, _client: RawClient| {
                    if let Some(parsed) = parse_hello(&payload) {
                        *hello.lock().expect("plugin-hello lock") = Some(parsed);
                    }
                }
            })
            .connect()?;

        // Warm initial state on (re)connect: ask RH to send current per-node RSSI, the enter/exit
        // detection thresholds, and the current race status (so the **current format id** is learned
        // early — `prepare_instant_start` needs it to zero that format's staging). `current_laps`
        // also arrives via the normal snapshot stream.
        let _ = client.emit(
            "load_data",
            json!({ "load_types": ["node_data", "enter_and_exit_at_levels", "race_status"] }),
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
        })
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

    /// **Prepare RotorHazard for an instant start** — the Grid-owns-all-timing model: GridFPV's own
    /// start procedure (the randomized Armed hold + the start tone) is the *only* delay; RH must just
    /// begin recording the instant Grid says go, with **no RH-side staging hold or tones**.
    ///
    /// Stock RotorHazard formats ship with a multi-second staging sequence — staging *tones* plus a
    /// fixed/random *start delay* (`start_delay_min_ms`/`start_delay_max_ms`, typically 1–2 s) — that
    /// `on_stage_race` runs as its own STAGING→RACING countdown ("Staging new race, format: … →
    /// tones → Race started"). That ran *on top of* Grid's start procedure (two competing start
    /// sequences — the root of the staging-race-eats-laps bug the `STAGE_RESET_SETTLE` band-aid
    /// papered over). This zeroes the **current** format's staging fields so `stage_race`'s
    /// `staging_total_ms` is 0 and RH transitions straight to RACING:
    ///   * `staging_fixed_tones = 0`, `staging_delay_tones = 0` — no staging tones;
    ///   * `start_delay_min_ms = 0`, `start_delay_max_ms = 0` — no fixed/random staging delay;
    ///   * `unlimited_time = 1` — RH never auto-expires the race; **Grid owns the stop**, not RH's
    ///     race-format timer.
    ///
    /// The only residual is RotorHazard's fixed `RACE_START_DELAY_EXTRA_SECS` prestage (a
    /// `Config.GENERAL` value, ~0.9 s by default, with no socket setter), which is *constant* and so
    /// **does not affect lap-time correctness**: RH timestamps every pass relative to its own race
    /// start, and Grid derives lap times as pass-to-pass deltas on that clock, so a constant prestage
    /// offset cancels out. (If a deployment needs RH RACING to coincide exactly with Grid's tone, set
    /// `RACE_START_DELAY_EXTRA_SECS = 0` in the timer's config — outside this socket API.)
    ///
    /// Targets the current format learned from the `race_status` stream; re-applies it as current so
    /// `RaceContext.race.format` reflects the zeroed staging. **Must be emitted while RH is READY** —
    /// `alter_race_format`/`set_race_format` are rejected during an active race — so the bridge calls
    /// this at **Stage** (pre-Armed, pre-go), not at the start instant. A no-op (best-effort `Ok`) if
    /// no current format id has been learned yet. Idempotent: re-zeroing an already-zeroed format is
    /// harmless.
    pub fn prepare_instant_start(&self) -> Result<(), rust_socketio::Error> {
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

    /// Inject a simulated pass on `node` (0-based) — driving helper for tests.
    pub fn simulate_lap(&self, node: u64) -> Result<(), rust_socketio::Error> {
        self.client.emit("simulate_lap", json!({ "node": node }))
    }

    /// **Tune** `node` (0-based) to `frequency` MHz (race redesign Slice 4a) — the engine allocates
    /// the channel, the adapter applies it (RE §7.3). Emits RotorHazard's `set_frequency` handler
    /// (`{ node, frequency }`); the server retunes that node's receiver. Best-effort: a failed emit
    /// on a dropped socket surfaces as an `Err` the caller logs.
    pub fn set_frequency(&self, node: u64, frequency: u16) -> Result<(), rust_socketio::Error> {
        self.client.emit(
            "set_frequency",
            json!({ "node": node, "frequency": frequency }),
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

    /// Re-request the per-node enter/exit detection thresholds (`load_data` /
    /// `enter_and_exit_at_levels`) — a driving helper so a test can re-capture thresholds after
    /// draining the connect-time burst.
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
