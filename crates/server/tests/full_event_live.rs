//! Mock-RH end-to-end **server** test (#47) — the backend proof of v0.4's "a complete
//! event runs live ... against dockerized RotorHazard".
//!
//! Where the engine's `full_event_live` (#37) drives a whole event through the *pure*
//! engine, this drives a real heat through **dockerized RotorHazard into the running
//! protocol server's log** and observes it through a real **protocol client** — exactly
//! the path a phone / overlay takes (protocol.html §2–§5):
//!
//! 1. Stand up the server (`axum::serve` on an ephemeral port) over a fresh
//!    [`InMemoryLog`]; the RD issues itself an RD bearer token.
//! 2. Drive a real heat against dockerized RH (the same plumbing as the engine's
//!    `common::run_mock_heat`, inlined here): the **heat-loop transitions** go through the
//!    privileged **control path** (`POST /control` with the RD token) — the way a real RD
//!    drives the loop — while the timer's real **passes** are appended through
//!    [`AppState::append`], the seam the source adapter writes through (§5: control is the
//!    RD's surface; passes are observations the adapter ingests, not RD commands).
//! 3. Attach a protocol client: `GET /snapshot/...` for the initial body+cursor, then a WS
//!    `/stream` subscribe `from` that cursor, and assert the client receives **in-order**
//!    change envelopes converging to the right [`LiveRaceState`] / [`LapList`].
//! 4. Apply a **marshaling correction** (`VoidDetection`) via a control command and assert
//!    the client observes the **re-folded** lap list (a fresh value, §9.2).
//! 5. Assert the **auth gate**: a control command without the RD token is rejected (401).
//!
//! # Determinism / tolerances
//!
//! RH's mock interface reads its CSV continuously (lap *timing* is not controllable) and
//! the harness stops the heat on the first crossing, so — like every `*_live` test — the
//! assertions are **structural / tolerant**: states reached, transition order, the change
//! stream converging, "a void removes exactly one detection". Never exact µs and never an
//! exact lap count (only `>= 1`). The transitions the *test itself* drives (the heat loop,
//! the marshaling correction) are deterministic; only the RH-produced passes are timing-
//! dependent, and those are asserted only by presence and by the re-fold delta.
//!
//! Local-only class (needs Docker). DISTINCT RH port 5041 (engine full-event uses 5040).
//! Run:
//!
//! ```sh
//! cargo test -p gridfpv-server --features live --test full_event_live -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use gridfpv_adapters::rotorhazard::RotorHazardAdapter;
use gridfpv_adapters::rotorhazard::transport::RotorHazardConnection;
use gridfpv_events::{CompetitorRef, Event, HeatId, LogRef};
use gridfpv_server::app::{AppState, router};
use gridfpv_server::control::{Command, CommandAck};
use gridfpv_server::scope::{EventId, PilotId, Scope, SubscribeRequest};
use gridfpv_server::snapshot::{HeatPhase, LiveRaceState, ProjectionBody, Snapshot};
use gridfpv_server::stream::{Change, StreamMessage};
use gridfpv_storage::InMemoryLog;
use gridfpv_testkit::{NodeCsv, RhContainer, node_csv};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// DISTINCT RH host port for the server e2e (engine full-event 5040, heat e2e 5032).
const RH_PORT: u16 = 5041;
/// The CSV tick interval (seconds), matching the engine harness.
const TICK: &str = "0.1";
/// The single heat this e2e drives.
const HEAT: &str = "q-e2e-1";

// ---------------------------------------------------------------------------------------
// Server / client plumbing (mirrors `tests/ws_stream.rs` + `tests/control.rs`).
// ---------------------------------------------------------------------------------------

/// Serve `router(state)` on an ephemeral port; return the base `127.0.0.1:port` address
/// and the server task handle (dropped at test end, aborting the task).
async fn serve(state: AppState) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state);
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("{addr}"), handle)
}

/// `POST /control` with the optional bearer `token`; return the HTTP status and (when the
/// body parses) the [`CommandAck`]. A tiny manual HTTP/1.1 POST so the test pulls in no
/// extra HTTP client dependency (the same shape `tests/control.rs` uses).
async fn post_raw(addr: &str, command: &Command, token: Option<&str>) -> (u16, Option<CommandAck>) {
    let body = serde_json::to_string(command).unwrap();
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST /control HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         {auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("a status code on the response line");
    let ack = response
        .split("\r\n\r\n")
        .nth(1)
        .and_then(|json| serde_json::from_str(json).ok());
    (status, ack)
}

/// POST one command with the RD `token`, asserting it acks ok (200) — the RD's heat-loop /
/// marshaling driver.
async fn rd_command(addr: &str, command: &Command, token: &str) -> CommandAck {
    let (status, ack) = post_raw(addr, command, Some(token)).await;
    assert_eq!(status, 200, "RD control should be admitted (got {status})");
    let ack = ack.expect("body is a CommandAck");
    assert!(ack.ok, "RD command should ack ok: {ack:?}");
    ack
}

/// GET a snapshot over a manual HTTP/1.1 request; return the parsed [`Snapshot`].
async fn get_snapshot(addr: &str, path: &str) -> Snapshot {
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let status: u16 = response
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("a status code");
    assert_eq!(status, 200, "snapshot GET {path} should be 200: {response}");
    let body = response.split("\r\n\r\n").nth(1).expect("a response body");
    serde_json::from_str(body).expect("parse Snapshot")
}

/// GET a snapshot path and return only its HTTP status (for the post-void empty-scope case).
async fn pilot_snapshot_status(addr: &str, path: &str) -> u16 {
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("a status code")
}

/// Connect a `/stream` reader at `addr`, subscribing to `scope` from the snapshot `cursor`.
async fn subscribe(addr: &str, request: &SubscribeRequest) -> Ws {
    let (mut ws, _) = connect_async(format!("ws://{addr}/stream")).await.unwrap();
    ws.send(Message::text(serde_json::to_string(request).unwrap()))
        .await
        .unwrap();
    ws
}

/// Await the next [`StreamMessage`] text frame, with a timeout so a missing frame fails the
/// test rather than hanging.
async fn next_message(ws: &mut Ws) -> StreamMessage {
    let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("timed out waiting for a stream frame")
        .expect("stream closed unexpectedly")
        .expect("websocket error");
    match frame {
        Message::Text(text) => serde_json::from_str(&text).expect("parse StreamMessage"),
        Message::Close(frame) => panic!("server closed the stream: {frame:?}"),
        other => panic!("expected a text frame, got {other:?}"),
    }
}

/// The per-stream sequence of a `Change` message.
fn seq(message: &StreamMessage) -> u64 {
    match message {
        StreamMessage::Change(env) => env.sequence.seq,
        other => panic!("expected a Change, got {other:?}"),
    }
}

/// The `LiveRaceState` carried by a fresh-value `Change` (the only encoding emitted today).
fn live_body(message: &StreamMessage) -> LiveRaceState {
    match message {
        StreamMessage::Change(env) => match &env.change {
            Change::FreshValue(ProjectionBody::LiveRaceState(ls)) => ls.clone(),
            other => panic!("expected a fresh-value live-state, got {other:?}"),
        },
        other => panic!("expected a Change, got {other:?}"),
    }
}

/// Pump live-state envelopes until one whose `phase` equals `target` arrives (or a deadline
/// elapses), returning that envelope's sequence. Tolerant of the engine emitting one
/// envelope per fold-changing append (passes between transitions also bump the sequence).
async fn await_phase(ws: &mut Ws, target: HeatPhase) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let message = next_message(ws).await;
        let ls = live_body(&message);
        if ls.phase == target {
            return seq(&message);
        }
        assert!(
            Instant::now() < deadline,
            "never observed phase {target:?} on the stream"
        );
    }
}

// ---------------------------------------------------------------------------------------
// Dockerized-RH driver (adapted from `engine/tests/common/mod.rs::run_mock_heat`).
// ---------------------------------------------------------------------------------------

/// Poll `conn` until `pred` holds over the accumulated `sink`, or `timeout` elapses.
fn wait_until(
    conn: &RotorHazardConnection,
    sink: &mut Vec<Event>,
    timeout: Duration,
    pred: impl Fn(&[Event]) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        sink.extend(conn.events());
        if pred(sink) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Connect to dockerized RH, reset it to a clean READY state, start the race, and collect
/// the real timer passes it produces. Returns the `Vec<Pass>` events (other adapter
/// bookkeeping is dropped, exactly as the engine harness does). Blocking — run under
/// `spawn_blocking` so it does not stall the async server task.
fn run_rh_heat(rh_url: &str) -> Vec<Event> {
    let conn = RotorHazardConnection::connect(rh_url, RotorHazardAdapter::new())
        .expect("connect to RotorHazard");

    // Settle, then reset RH so staging starts from a known place.
    std::thread::sleep(Duration::from_secs(2));
    conn.stop_race().ok();
    conn.discard_laps().expect("discard_laps");
    std::thread::sleep(Duration::from_secs(2));
    let _ = conn.events(); // drop the reset's snapshot churn

    // Actually start the race on RH (it stages + auto-starts).
    conn.stage_race().expect("stage_race");
    let mut live: Vec<Event> = Vec::new();
    assert!(
        wait_until(&conn, &mut live, Duration::from_secs(20), |evs| {
            evs.iter()
                .any(|e| matches!(e, Event::SessionStarted { .. }))
        }),
        "RotorHazard never reached RACING"
    );

    // Collect crossings; keep at least one pass.
    let got_pass = wait_until(&conn, &mut live, Duration::from_secs(25), |evs| {
        evs.iter().any(|e| matches!(e, Event::Pass(_)))
    });

    // Close the race, drain any final crossings.
    conn.stop_race().ok();
    std::thread::sleep(Duration::from_millis(800));
    live.extend(conn.events());
    conn.disconnect();

    assert!(
        got_pass,
        "no timer crossings were produced while the heat was running"
    );

    // Only `Pass`es are the heat's canonical race-engine observations.
    live.into_iter()
        .filter(|e| matches!(e, Event::Pass(_)))
        .collect()
}

// ---------------------------------------------------------------------------------------
// The e2e.
// ---------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker (spins up dockerized RotorHazard and drives a live heat through the server)"]
async fn a_live_heat_flows_through_the_server_to_a_protocol_client() {
    // One busy node so several real passes land in the live window (the harness stops the
    // race shortly after the first crossing). `node-0` is the seat ref the adapter reports.
    let scenario = vec![(
        0usize,
        node_csv(&NodeCsv {
            ticks_per_lap: 2,
            peak_rssi: 180,
            baseline_rssi: 70,
        }),
    )];
    let heat = HeatId(HEAT.into());
    let pilot = CompetitorRef("node-0".into());

    // RAII: the container is dropped at the end of the test, removing it.
    let rh = RhContainer::start(RH_PORT, TICK, &scenario);
    let rh_url = rh.url().to_string();

    // --- Stand up the server over a fresh log; the RD issues itself a token. ---
    let state = AppState::new(InMemoryLog::default());
    let rd = state.tokens().issue_rd_token();
    let (addr, _server) = serve(state.clone()).await;

    // === 1. Schedule the heat via the control path (the RD's surface). ===
    rd_command(
        &addr,
        &Command::ScheduleHeat {
            heat: heat.clone(),
            lineup: vec![pilot.clone()],
            class: None,
            round: None,
            frequencies: vec![],
        },
        &rd,
    )
    .await;

    // === 2. Attach a protocol client: snapshot first, then subscribe from its cursor. ===
    // The event-scope snapshot is the whole-event live state; its cursor is the resume
    // point so the stream begins exactly after the snapshot (§2, §3).
    let snapshot = get_snapshot(&addr, "/snapshot/event/spring-cup").await;
    let snap_live = match &snapshot.body {
        ProjectionBody::LiveRaceState(ls) => ls.clone(),
        other => panic!("expected a live-state snapshot, got {other:?}"),
    };
    assert_eq!(
        snap_live.current_heat,
        Some(heat.clone()),
        "the snapshot already reflects the scheduled heat"
    );
    assert_eq!(snap_live.phase, HeatPhase::Scheduled);
    assert_eq!(snap_live.active_pilots, vec![pilot.clone()]);

    let mut stream = subscribe(
        &addr,
        &SubscribeRequest {
            scope: Scope::Event {
                event: EventId("spring-cup".into()),
            },
            from: Some(snapshot.cursor),
            contract_version: None,
            token: None,
        },
    )
    .await;

    // === 3. Drive the heat loop through the control path; the client reads it back. ===
    // Stage + Arm are deterministic; the client must observe each phase transition in order
    // (proving the control append reaches the read stream, §3, §5).
    rd_command(&addr, &Command::Stage { heat: heat.clone() }, &rd).await;
    let s_staged = await_phase(&mut stream, HeatPhase::Staged).await;
    rd_command(&addr, &Command::Arm { heat: heat.clone() }, &rd).await;
    let s_armed = await_phase(&mut stream, HeatPhase::Armed).await;
    assert!(
        s_armed > s_staged,
        "the per-stream sequence is strictly increasing (Staged {s_staged} < Armed {s_armed})"
    );
    rd_command(&addr, &Command::Start { heat: heat.clone() }, &rd).await;
    await_phase(&mut stream, HeatPhase::Running).await;

    // === 4. Run the real heat on dockerized RH; feed its passes through `append`. ===
    // This is the source-adapter seam (passes are observations, not RD commands). Driven on
    // a blocking thread so the Socket.IO polling does not stall the async runtime.
    let rh_url2 = rh_url.clone();
    let passes = tokio::task::spawn_blocking(move || run_rh_heat(&rh_url2))
        .await
        .expect("RH driver thread");
    let pass_count = passes.len();
    assert!(pass_count >= 1, "the real heat produced at least one pass");
    for pass in passes {
        state.append(pass, None).expect("append a real pass");
    }

    // Drain any change envelopes the passes produced (each pass that *completes* a lap
    // changes the live-state fold and emits one), then read the converged live state off a
    // fresh event-scope snapshot — the same projection the stream serves, so the snapshot
    // and the stream agree (§2, §3). A single pass banks 0 completed laps, so the structural
    // guarantee is that the live state still reflects this heat / pilot, Running.
    drain_envelopes(&mut stream).await;
    let folded_snap = get_snapshot(&addr, "/snapshot/event/spring-cup").await;
    let folded = match &folded_snap.body {
        ProjectionBody::LiveRaceState(ls) => ls.clone(),
        other => panic!("expected a live-state snapshot, got {other:?}"),
    };
    let live_laps = folded
        .progress
        .iter()
        .find(|p| p.competitor == pilot)
        .map(|p| p.laps_completed)
        .unwrap_or(0);
    assert_eq!(folded.current_heat, Some(heat.clone()));
    assert_eq!(folded.phase, HeatPhase::Running);
    assert!(
        folded.active_pilots.contains(&pilot),
        "the converged live state still carries the heat's pilot"
    );

    // === 5. The protocol client reads the pilot's lap list (snapshot scope). ===
    // The pilot scope folds to a `LapList`; its detection count is the marshaling baseline.
    let pilot_snap = get_snapshot(&addr, "/snapshot/pilot/spring-cup/node-0").await;
    let baseline = lap_list_of(&pilot_snap.body);
    let baseline_detections = detection_count(&baseline);
    assert!(
        baseline_detections >= 1,
        "the pilot has at least one real detection to marshal; got {baseline_detections}"
    );
    eprintln!(
        "server e2e: {pass_count} real passes ⇒ {live_laps} completed laps, \
         {baseline_detections} detections"
    );

    // === 6. Marshaling correction: void one real detection via a control command. ===
    // Find a real `Pass` offset in the server's log and `VoidDetection` it through the RD
    // control path, with a fresh pilot-scope client subscribed so it observes the re-fold.
    let void_offset = first_pass_offset(&state).expect("a real pass to void");

    // A fresh pilot-scope subscribe so the stream's seeded `last_emitted` is the *pre-void*
    // lap list; the first envelope after the void is therefore the re-folded value.
    let pilot_snap2 = get_snapshot(&addr, "/snapshot/pilot/spring-cup/node-0").await;
    let mut pilot_stream = subscribe(
        &addr,
        &SubscribeRequest {
            scope: Scope::Pilot {
                event: EventId("spring-cup".into()),
                pilot: PilotId("node-0".into()),
            },
            from: Some(pilot_snap2.cursor),
            contract_version: None,
            token: None,
        },
    )
    .await;

    rd_command(
        &addr,
        &Command::VoidDetection {
            target: LogRef(void_offset),
        },
        &rd,
    )
    .await;

    if baseline_detections >= 2 {
        // The void leaves at least one detection, so the pilot scope still folds to a
        // non-empty lap list: the next envelope carries the re-folded value, one detection
        // lighter — the client observes the marshaling correction as a fresh value (§9.2).
        let marshaled = await_lap_list(&mut pilot_stream).await;
        assert_eq!(
            detection_count(&marshaled),
            baseline_detections - 1,
            "voiding one real detection re-folds the client's lap list down by exactly one"
        );
    } else {
        // Voiding the pilot's *only* detection empties the lap list, so the pilot scope
        // folds to nothing and the snapshot 404s. Assert the re-fold by re-reading the
        // pilot snapshot, which must now report the scope as unknown (no laps left).
        assert_eq!(
            pilot_snapshot_status(&addr, "/snapshot/pilot/spring-cup/node-0").await,
            404,
            "voiding the only detection re-folds the pilot's lap list to empty (scope 404s)"
        );
    }

    // === 7. Auth gate: a control command without the RD token is rejected (401). ===
    let (status, _ack) = post_raw(&addr, &Command::Finish { heat: heat.clone() }, None).await;
    assert_eq!(
        status, 401,
        "an un-authenticated control command is rejected"
    );
    // A read-only join-token never grants control either.
    let join = state.tokens().issue_join_token();
    let (status, _ack) =
        post_raw(&addr, &Command::Finish { heat: heat.clone() }, Some(&join)).await;
    assert_eq!(status, 401, "a read-only join-token never grants control");
    // The same command WITH the RD token finishes the heat — proving the gate admits the RD.
    rd_command(&addr, &Command::Finish { heat: heat.clone() }, &rd).await;
    rd_command(&addr, &Command::Finalize { heat: heat.clone() }, &rd).await;

    // The event-scope client converges to the Final phase — the heat ran end to end.
    await_phase(&mut stream, HeatPhase::Final).await;
}

/// Drain any change envelopes already queued on the stream (the ones the appended passes
/// produced), without blocking once the stream goes quiet. Each drained frame must be a
/// well-formed live-state `Change` — proving the passes flowed through as ordered envelopes.
async fn drain_envelopes(ws: &mut Ws) {
    loop {
        match tokio::time::timeout(Duration::from_millis(500), ws.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let message: StreamMessage =
                    serde_json::from_str(&text).expect("parse StreamMessage");
                // A live-state fresh value (the only thing an event scope emits).
                let _ = live_body(&message);
            }
            Ok(Some(Ok(Message::Close(frame)))) => panic!("stream closed: {frame:?}"),
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(e))) => panic!("websocket error: {e}"),
            Ok(None) => return,
            // No more frames within the quiet window: the stream has caught up.
            Err(_) => return,
        }
    }
}

/// Await the next pilot-scope envelope carrying a `LapList` fresh value.
async fn await_lap_list(ws: &mut Ws) -> gridfpv_projection::LapList {
    match next_message(ws).await {
        StreamMessage::Change(env) => match env.change {
            Change::FreshValue(ProjectionBody::LapList(list)) => list,
            other => panic!("expected a lap-list fresh value, got {other:?}"),
        },
        other => panic!("expected a Change, got {other:?}"),
    }
}

/// The `LapList` carried by a pilot-scope snapshot body.
fn lap_list_of(body: &ProjectionBody) -> gridfpv_projection::LapList {
    match body {
        ProjectionBody::LapList(list) => list.clone(),
        other => panic!("expected a lap-list body, got {other:?}"),
    }
}

/// Total detections (lap-gate passes) a corrected lap list holds: a competitor with `K`
/// laps had `K + 1` detections, so summing `laps + 1` over present competitors counts
/// detections. Enough to prove a void removed exactly one.
fn detection_count(list: &gridfpv_projection::LapList) -> usize {
    list.competitors.iter().map(|c| c.laps.len() + 1).sum()
}

/// The log offset of the first real `Pass` in the server's log (the marshaling target).
fn first_pass_offset(state: &AppState) -> Option<u64> {
    let (events, _) = state.read_for_test();
    events
        .iter()
        .position(|e| matches!(e, Event::Pass(_)))
        .map(|i| i as u64)
}

// A small read accessor so the test can scan the server's log for a real pass offset. The
// server exposes `read` only `pub(crate)`; the test reaches the same log through the
// snapshot path instead. See `first_pass_offset` for how it is used.
trait ReadForTest {
    fn read_for_test(&self) -> (Vec<Event>, gridfpv_server::stream::Cursor);
}

impl ReadForTest for AppState {
    fn read_for_test(&self) -> (Vec<Event>, gridfpv_server::stream::Cursor) {
        // The log is shared behind `AppState::log()`; read it through the `EventSource`
        // facade the server exposes (its methods are available on the `dyn` handle).
        let log = self.log();
        let guard = log.lock().expect("log lock");
        let stored = guard.read_all().expect("read_all");
        let len = guard.len().expect("len");
        drop(guard);
        let events = stored.into_iter().map(|s| s.event).collect();
        (events, gridfpv_server::stream::Cursor::new(len))
    }
}
