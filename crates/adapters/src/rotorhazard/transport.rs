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

use rust_socketio::client::Client;
use rust_socketio::{ClientBuilder, Payload, RawClient};
use serde_json::json;

use super::{Raw, RawCurrentLaps, RawNodeData, RawPassRecord, RawRaceStatus, RotorHazardAdapter};
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
        _ => None,
    }
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
}

impl RotorHazardConnection {
    /// Connect to `url` (e.g. `http://localhost:5000`) and start translating the
    /// RotorHazard socket stream through `adapter`.
    pub fn connect(url: &str, adapter: RotorHazardAdapter) -> Result<Self, rust_socketio::Error> {
        let adapter = Arc::new(Mutex::new(adapter));
        let events: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
        // Starts alive; flipped to `false` by the `close`/`error` reserved-event handlers below.
        let alive = Arc::new(AtomicBool::new(true));

        // `rust_socketio`'s reserved events: on a dropped socket the poll loop fires `error`
        // (the engine.io read failed) and, on a clean disconnect packet, `close`. Either way the
        // link is no longer usable, so flip `alive` to `false` — the truth the driver monitors.
        let drop_handler = |alive: Arc<AtomicBool>| {
            move |_payload: Payload, _client: RawClient| {
                alive.store(false, Ordering::Relaxed);
            }
        };

        // One handler per translated event: decode -> translate -> accumulate.
        let handler = |name: &'static str,
                       adapter: Arc<Mutex<RotorHazardAdapter>>,
                       sink: Arc<Mutex<Vec<Event>>>| {
            move |payload: Payload, _client: RawClient| {
                if let Some(raw) = raw_from_socket(name, &payload) {
                    let translated = adapter.lock().unwrap().translate(raw);
                    if !translated.is_empty() {
                        sink.lock().unwrap().extend(translated);
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
                handler("race_status", adapter.clone(), events.clone()),
            )
            .on(
                "current_laps",
                handler("current_laps", adapter.clone(), events.clone()),
            )
            .on(
                "node_data",
                handler("node_data", adapter.clone(), events.clone()),
            )
            .on(
                "pass_record",
                handler("pass_record", adapter.clone(), events.clone()),
            )
            .connect()?;

        // Warm initial state on (re)connect: ask RH to send current per-node RSSI so
        // the signal-context cache is populated before the first pass. `current_laps`
        // and `race_status` arrive via the normal snapshot stream.
        let _ = client.emit("load_data", json!({ "load_types": ["node_data"] }));

        Ok(Self {
            client,
            events,
            adapter,
            alive,
        })
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

    /// Stop the current race — driving helper for tests.
    pub fn stop_race(&self) -> Result<(), rust_socketio::Error> {
        self.client.emit("stop_race", Payload::Text(vec![]))
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
