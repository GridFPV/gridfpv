//! WebSocket change-stream integration tests (protocol.html §3) — issue #43.
//!
//! These spin the real [`router`] on an ephemeral `127.0.0.1` port with `axum::serve` and
//! drive it with a real WebSocket client (`tokio-tungstenite`) — no Docker, no mocks of the
//! transport. Each test:
//!
//! 1. builds an [`AppState`] over an [`InMemoryLog`], keeping a clone to append through;
//! 2. serves [`router`] in a background task on an OS-assigned port;
//! 3. connects, sends a [`SubscribeRequest`], and appends events via
//!    [`AppState::append`] (the same write path the control endpoint #45 will use);
//! 4. asserts the client receives ordered, gap-free [`ChangeEnvelope`]s converging to the
//!    server's folded state.
//!
//! Determinism: appends are explicit and the client awaits each expected frame, so there is
//! no reliance on timing — a short `timeout` only guards against a hang on a missing frame.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use gridfpv_events::{
    AdapterId, CompetitorRef, Event, GateIndex, HeatId, HeatTransition, LogRef, Pass,
    SignalHistory, SourceTime,
};
use gridfpv_server::app::{AppState, router};
use gridfpv_server::events::{CreateEventRequest, EventRegistry};
use gridfpv_server::scope::{EventId, Scope, SubscribeRequest};
use gridfpv_server::snapshot::{HeatPhase, LiveRaceState, ProjectionBody};
use gridfpv_server::stream::{Change, Cursor, StreamMessage};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Serve `router(state)` on an ephemeral port; return the `ws://…/stream` URL for the
/// registry's single event and the server task's join handle (dropped at test end, which
/// aborts the task).
async fn serve(registry: EventRegistry) -> (String, tokio::task::JoinHandle<()>) {
    let event = sole_event(&registry);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(registry);
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("ws://{addr}/events/{}/stream", event.0), handle)
}

/// The id of the registry's single event.
fn sole_event(registry: &EventRegistry) -> EventId {
    let mut list = registry.list();
    assert_eq!(list.len(), 1, "one created event per test registry");
    list.remove(0).id
}

/// A fresh registry holding one **created** event, plus that event's [`AppState`] — the
/// change-stream tests append through its log and subscribe under `/events/{id}/stream`
/// (issue #72). There is no built-in event any more (#414), so the fixture creates one
/// through the real creation path.
fn test_registry() -> (EventRegistry, AppState) {
    let registry = EventRegistry::new(None).unwrap();
    let event = registry
        .create(&CreateEventRequest::named("Test Event"))
        .expect("create the test event")
        .id;
    let state = registry.resolve(&event).expect("the created event");
    (registry, state)
}

/// Connect to the stream endpoint and send a subscribe frame.
async fn subscribe(url: &str, request: &SubscribeRequest) -> Ws {
    let (mut ws, _) = connect_async(url).await.unwrap();
    let json = serde_json::to_string(request).unwrap();
    ws.send(Message::text(json)).await.unwrap();
    ws
}

/// Await the next [`StreamMessage`] text frame (with a timeout so a missing frame fails the
/// test rather than hanging).
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

/// The `LiveRaceState` body of a `Change` stream message, asserting it is a fresh value
/// (the only encoding emitted today, §9.2 deferral).
fn live_body(message: &StreamMessage) -> &gridfpv_server::snapshot::LiveRaceState {
    match message {
        StreamMessage::Change(env) => match &env.change {
            Change::FreshValue(ProjectionBody::LiveRaceState(ls)) => ls,
            other => panic!("expected a fresh-value live-state, got {other:?}"),
        },
        other => panic!("expected a Change, got {other:?}"),
    }
}

fn heat_scheduled(id: &str, lineup: &[&str]) -> Event {
    Event::HeatScheduled {
        heat: HeatId(id.into()),
        lineup: lineup.iter().map(|c| CompetitorRef((*c).into())).collect(),
        class: None,
        round: None,
        frequencies: vec![],
        label: None,
    }
}

fn heat_changed(id: &str, transition: HeatTransition) -> Event {
    Event::HeatStateChanged {
        heat: HeatId(id.into()),
        transition,
    }
}

fn event_scope() -> Scope {
    Scope::Event {
        event: EventId("spring-cup".into()),
    }
}

/// Subscribe fresh (from the start), append events, and assert the client receives ordered,
/// gap-free envelopes whose final state matches the server's fold.
#[tokio::test]
async fn streams_ordered_envelopes_from_a_fresh_subscribe() {
    let (registry, state) = test_registry();
    let (url, _server) = serve(registry.clone()).await;

    let mut ws = subscribe(
        &url,
        &SubscribeRequest {
            scope: event_scope(),
            from: None,
            contract_version: None,
            token: None,
        },
    )
    .await;

    // Append a heat scheduling then drive it through the loop. Each append that *changes*
    // the live-state fold yields one envelope.
    state
        .append(heat_scheduled("q-1", &["A", "B"]), None)
        .unwrap();
    let m1 = next_message(&mut ws).await;
    assert_eq!(seq(&m1), 1, "first envelope is per-stream sequence 1");
    assert_eq!(live_body(&m1).current_heat, Some(HeatId("q-1".into())));
    assert_eq!(live_body(&m1).phase, HeatPhase::Scheduled);

    state
        .append(heat_changed("q-1", HeatTransition::Staged), None)
        .unwrap();
    let m2 = next_message(&mut ws).await;
    assert_eq!(seq(&m2), 2, "sequence increments by one, gap-free");
    assert_eq!(live_body(&m2).phase, HeatPhase::Staged);

    state
        .append(heat_changed("q-1", HeatTransition::Armed), None)
        .unwrap();
    let m3 = next_message(&mut ws).await;
    assert_eq!(seq(&m3), 3);
    assert_eq!(live_body(&m3).phase, HeatPhase::Armed);

    state
        .append(heat_changed("q-1", HeatTransition::Running), None)
        .unwrap();
    let m4 = next_message(&mut ws).await;
    assert_eq!(seq(&m4), 4);
    assert_eq!(live_body(&m4).phase, HeatPhase::Running);
}

/// Resume from a mid-stream cursor: a second connection presenting an in-window log offset
/// receives only the changes *after* that offset, with its own per-stream sequence restarting
/// at 1.
#[tokio::test]
async fn resume_from_a_mid_cursor_replays_only_the_tail() {
    let (registry, state) = test_registry();
    let (url, _server) = serve(registry.clone()).await;

    // Pre-load three events (offsets 0,1,2 → log length 3).
    state
        .append(heat_scheduled("q-1", &["A", "B"]), None)
        .unwrap();
    state
        .append(heat_changed("q-1", HeatTransition::Staged), None)
        .unwrap();
    state
        .append(heat_changed("q-1", HeatTransition::Armed), None)
        .unwrap();

    // Resume from offset 2 (the snapshot cursor a client would have after the Staged
    // change): it must NOT replay the earlier two, only fold offset 2 onward.
    let mut ws = subscribe(
        &url,
        &SubscribeRequest {
            scope: event_scope(),
            from: Some(Cursor::new(2)),
            contract_version: None,
            token: None,
        },
    )
    .await;

    // Offset 2 (the Armed change) is the first event after the cursor → first envelope.
    let m1 = next_message(&mut ws).await;
    assert_eq!(seq(&m1), 1, "a resumed stream's own sequence restarts at 1");
    assert_eq!(live_body(&m1).phase, HeatPhase::Armed);

    // A further append continues the resumed stream in order.
    state
        .append(heat_changed("q-1", HeatTransition::Running), None)
        .unwrap();
    let m2 = next_message(&mut ws).await;
    assert_eq!(seq(&m2), 2);
    assert_eq!(live_body(&m2).phase, HeatPhase::Running);
}

/// A resume cursor older than the bounded retained window gets the re-snapshot-required
/// signal instead of a replay (protocol.html §3, §9.3).
#[tokio::test]
async fn too_old_cursor_requires_re_snapshot() {
    let (registry, state) = test_registry();
    let (url, _server) = serve(registry.clone()).await;

    // Push the log tail well past the retained window so a low cursor is out of range.
    let tail = gridfpv_server::ws::RETAINED_WINDOW + 50;
    for _ in 0..tail {
        state.append(heat_scheduled("q-1", &["A"]), None).unwrap();
    }

    // Resume from offset 1 — far below `tail - RETAINED_WINDOW`.
    let mut ws = subscribe(
        &url,
        &SubscribeRequest {
            scope: event_scope(),
            from: Some(Cursor::new(1)),
            contract_version: None,
            token: None,
        },
    )
    .await;

    match next_message(&mut ws).await {
        StreamMessage::ReSnapshotRequired(err) => {
            assert_eq!(err.code, gridfpv_server::error::ErrorCode::StaleCursor);
        }
        other => panic!("expected ReSnapshotRequired, got {other:?}"),
    }
}

/// A transition that does not alter the scoped projection emits no envelope (the engine emits
/// one envelope per *change*, keeping the per-stream sequence a faithful change count) — except
/// a `HeatScheduled`, which always wakes the stream so the heats lists re-read (see
/// `scheduling_a_heat_wakes_the_stream_even_when_the_body_is_unchanged`, below).
#[tokio::test]
async fn unchanged_fold_emits_no_envelope() {
    let (registry, state) = test_registry();
    let (url, _server) = serve(registry.clone()).await;

    let mut ws = subscribe(
        &url,
        &SubscribeRequest {
            scope: event_scope(),
            from: None,
            contract_version: None,
            token: None,
        },
    )
    .await;

    state
        .append(heat_scheduled("q-1", &["A", "B"]), None)
        .unwrap();
    let m1 = next_message(&mut ws).await;
    assert_eq!(seq(&m1), 1);
    assert_eq!(live_body(&m1).phase, HeatPhase::Scheduled);

    // Re-applying the same Running transition folds to the same live state → no new envelope.
    // A following Running transition (after a fold-changing Staged) proves the no-op did not
    // consume a sequence: drive Staged (a change, seq 2) then Running (a change, seq 3) and
    // re-append Running (a no-op) — the next message read is the genuine Running at seq 3.
    state
        .append(heat_changed("q-1", HeatTransition::Staged), None)
        .unwrap();
    let m2 = next_message(&mut ws).await;
    assert_eq!(seq(&m2), 2);
    assert_eq!(live_body(&m2).phase, HeatPhase::Staged);

    state
        .append(heat_changed("q-1", HeatTransition::Running), None)
        .unwrap();
    // A redundant Running transition folds to the SAME body → no envelope; it must not consume
    // a sequence, so the Running message arrives at seq 3.
    state
        .append(heat_changed("q-1", HeatTransition::Running), None)
        .unwrap();
    let m3 = next_message(&mut ws).await;
    assert_eq!(
        seq(&m3),
        3,
        "the no-op transition did not consume a sequence"
    );
    assert_eq!(live_body(&m3).phase, HeatPhase::Running);
}

/// A bare `HeatScheduled` that *fills* a heat must wake the change stream even when the folded
/// `LiveRaceState` body is unchanged (fill-no-steal does not move `current_heat`, and an
/// appended heat behind the on-deck one does not move `on_deck`). The end-to-end proof that the
/// scheduled heat reaches consumers so the Live heat picker / Rounds & Heats list re-read
/// `/heats` without waiting for a transition.
#[tokio::test]
async fn scheduling_a_heat_wakes_the_stream_even_when_the_body_is_unchanged() {
    let (registry, state) = test_registry();
    let (url, _server) = serve(registry.clone()).await;

    let mut ws = subscribe(
        &url,
        &SubscribeRequest {
            scope: event_scope(),
            from: None,
            contract_version: None,
            token: None,
        },
    )
    .await;

    // q-1 scheduled + staged (current), q-2 scheduled (on-deck).
    state
        .append(heat_scheduled("q-1", &["A", "B"]), None)
        .unwrap();
    assert_eq!(seq(&next_message(&mut ws).await), 1);
    state
        .append(heat_changed("q-1", HeatTransition::Staged), None)
        .unwrap();
    assert_eq!(seq(&next_message(&mut ws).await), 2);
    state
        .append(heat_scheduled("q-2", &["C", "D"]), None)
        .unwrap();
    let m_ondeck = next_message(&mut ws).await;
    assert_eq!(seq(&m_ondeck), 3);
    assert_eq!(live_body(&m_ondeck).on_deck, Some(HeatId("q-2".into())));

    // q-3 scheduled behind q-2: neither `current_heat` (still q-1) nor `on_deck` (still q-2)
    // moves, so the folded body is unchanged — yet the schedule must still wake the stream.
    state
        .append(heat_scheduled("q-3", &["E", "F"]), None)
        .unwrap();
    let m_wake = next_message(&mut ws).await;
    assert_eq!(seq(&m_wake), 4, "a schedule wakes the stream");
    let live = live_body(&m_wake);
    // current_heat is untouched (no focus steal) and on_deck is unchanged.
    assert_eq!(live.current_heat, Some(HeatId("q-1".into())));
    assert_eq!(live.on_deck, Some(HeatId("q-2".into())));
}

/// A malformed first frame closes the socket with a BadRequest close (no panic, no hang).
#[tokio::test]
async fn malformed_subscribe_closes_the_socket() {
    let (registry, _state) = test_registry();
    let (url, _server) = serve(registry).await;

    let (mut ws, _) = connect_async(&url).await.unwrap();
    ws.send(Message::text("not a subscribe request"))
        .await
        .unwrap();

    // The server replies with a close frame and ends the stream.
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(frame) = ws.next().await {
            if matches!(frame, Ok(Message::Close(_))) || frame.is_err() {
                return true;
            }
        }
        true // stream ended
    })
    .await
    .expect("timed out waiting for the socket to close");
    assert!(closed);
}

/// The per-stream sequence of a `Change` message.
fn seq(message: &StreamMessage) -> u64 {
    match message {
        StreamMessage::Change(env) => env.sequence.seq,
        other => panic!("expected a Change, got {other:?}"),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════════════
// #422 — a reconnect must never show a live count stepping backwards
// ═══════════════════════════════════════════════════════════════════════════════════════
//
// Two halves, and the tests below hold both:
//
//   1. every envelope echoes the log offset it was folded through, so a client's resume
//      cursor is EXACT rather than the `+1`-per-applied-envelope lower bound it used to
//      infer; and
//   2. whatever span a resume does ask for is folded once at its end, so even a client
//      still presenting a stale-but-in-window cursor is shown one settled state.
//
// These deliberately subscribe from a **stale** cursor and assert on the **whole**
// envelope sequence. `min_lap_floor_conformance.rs` reads every stream at `from: tail` and
// asserts only the last envelope, which is structurally blind to a replayed body — a test
// shaped like that one passes for the reason that one did, not because the bug is gone.

/// The echoed resume cursor of a `Change` message — the log offset its body was folded through.
fn env_cursor(message: &StreamMessage) -> u64 {
    match message {
        StreamMessage::Change(env) => env.cursor.seq,
        other => panic!("expected a Change, got {other:?}"),
    }
}

/// One competitor's `laps_completed` in a live body (0 if the competitor is absent).
fn laps_of(live: &LiveRaceState, competitor: &str) -> u32 {
    live.progress
        .iter()
        .find(|p| p.competitor == CompetitorRef(competitor.into()))
        .map(|p| p.laps_completed)
        .unwrap_or(0)
}

/// Collect every frame the stream sends until it goes quiet for `quiet`.
///
/// This is what makes "assert on the whole sequence" possible: a staircase of replayed folds
/// arrives as several frames, and only reading *all* of them can see the dip. Deterministic
/// because every append under test is already on the log before the subscribe.
async fn collect_until_quiet(ws: &mut Ws, quiet: Duration) -> Vec<StreamMessage> {
    let mut out = Vec::new();
    while let Ok(frame) = tokio::time::timeout(quiet, ws.next()).await {
        match frame
            .expect("stream closed unexpectedly")
            .expect("websocket error")
        {
            Message::Text(text) => out.push(serde_json::from_str(&text).expect("StreamMessage")),
            Message::Close(frame) => panic!("server closed the stream: {frame:?}"),
            _ => continue,
        }
    }
    out
}

/// How long a stream must stay silent before we call it settled.
const QUIET: Duration = Duration::from_millis(300);

fn pass(competitor: &str, at_micros: i64, sequence: u64) -> Event {
    Event::Pass(Pass {
        adapter: AdapterId("sim".into()),
        competitor: CompetitorRef(competitor.into()),
        at: SourceTime::from_micros(at_micros),
        sequence: Some(sequence),
        gate: GateIndex::LAP,
        signal: None,
        heat: Some(HeatId("q-1".into())),
    })
}

/// A dense RSSI append: a real, logged event that moves no projection — the exact shape of
/// append that used to widen the client's cursor drift, since it emits no envelope to count.
fn signal_chunk(base: u64) -> Event {
    Event::SignalHistory(SignalHistory {
        adapter: AdapterId("sim".into()),
        competitor: CompetitorRef("A".into()),
        times: vec![0, 1_000, 2_000],
        rssi: vec![50, 60, 70],
        base,
    })
}

/// Append an event that *does* move the live fold; when `watch` is a connected stream, await the
/// envelope it produces before returning.
///
/// Awaiting is what keeps a live console's view of the heat honest: without it the appends race
/// the stream's first wake, all ten land as one catch-up span, and the test measures the collapse
/// instead of the drift it is trying to reproduce.
async fn append_watched(
    state: &AppState,
    event: Event,
    watch: &mut Option<&mut Ws>,
    seen: &mut Vec<StreamMessage>,
) -> u64 {
    let offset = state.append(event, None).unwrap();
    if let Some(ws) = watch.as_deref_mut() {
        seen.push(next_message(ws).await);
    }
    offset
}

/// Run a heat on `state` in which A completes four laps, with a signal chunk between each pass.
///
/// Returns the log offset of A's **last** pass (the marshaling target the void tests use) and, if
/// `watch` is a connected stream, every envelope that stream emitted along the way.
/// Layout — ten offsets, of which seven move the projection and three do not:
///
/// | offset | event                | emits |
/// |--------|----------------------|-------|
/// | 0      | q-1 scheduled        | yes   |
/// | 1      | q-1 Running          | yes   |
/// | 2      | A pass (holeshot)    | yes   |
/// | 3      | A pass → lap 1       | yes   |
/// | 4      | signal chunk         | **no**|
/// | 5      | A pass → lap 2       | yes   |
/// | 6      | signal chunk         | **no**|
/// | 7      | A pass → lap 3       | yes   |
/// | 8      | signal chunk         | **no**|
/// | 9      | A pass → lap 4       | yes   |
///
/// The three silent offsets are the whole mechanism: they are ordinary log appends that a
/// `+1`-per-envelope client can never count, so its cursor falls three behind the tail.
async fn run_four_lap_heat(
    state: &AppState,
    mut watch: Option<&mut Ws>,
) -> (u64, Vec<StreamMessage>) {
    let seen = &mut Vec::new();
    append_watched(state, heat_scheduled("q-1", &["A", "B"]), &mut watch, seen).await;
    append_watched(
        state,
        heat_changed("q-1", HeatTransition::Running),
        &mut watch,
        seen,
    )
    .await;
    append_watched(state, pass("A", 1_000_000, 0), &mut watch, seen).await;
    append_watched(state, pass("A", 4_000_000, 1), &mut watch, seen).await;
    state.append(signal_chunk(0), None).unwrap();
    append_watched(state, pass("A", 7_000_000, 2), &mut watch, seen).await;
    state.append(signal_chunk(3), None).unwrap();
    append_watched(state, pass("A", 10_000_000, 3), &mut watch, seen).await;
    state.append(signal_chunk(6), None).unwrap();
    let last = append_watched(state, pass("A", 13_000_000, 4), &mut watch, seen).await;
    (last, std::mem::take(seen))
}

/// The log length after an append that landed at `offset` — the true tail a snapshot taken
/// at that instant would hand out as its cursor (offsets are dense from 0).
fn tail_after(offset: u64) -> u64 {
    offset + 1
}

/// **#422 — a resume from a stale (but in-window) cursor delivers no backwards step.**
///
/// The premise is reproduced, not assumed: a client streams the whole heat live and counts the
/// envelopes it applied, which is *exactly* the `+1`-per-applied-envelope resume cursor the old
/// client inferred. Seven envelopes against a ten-offset log — the three signal chunks moved no
/// projection, emitted nothing, and so were never counted. That three-offset drift is the bug.
///
/// Resubscribing from that stale cursor used to replay offsets 7..10 one at a time: an envelope
/// carrying A on **3** laps, then one carrying A on 4. The console, already showing 4, dropped to
/// 3 and climbed back — a pilot visibly losing a lap mid-heat, indistinguishable from a marshal
/// voiding a pass. The whole resumed sequence is asserted here, so that dip cannot hide.
#[tokio::test]
async fn a_resume_from_a_stale_cursor_never_steps_the_lap_count_backwards() {
    let (registry, state) = test_registry();
    let (url, _server) = serve(registry.clone()).await;

    // A console connected for the whole heat, applying envelopes as they land.
    let mut live = subscribe(
        &url,
        &SubscribeRequest {
            scope: event_scope(),
            from: None,
            contract_version: None,
            token: None,
        },
    )
    .await;
    let (last_pass, applied) = run_four_lap_heat(&state, Some(&mut live)).await;
    let tail = tail_after(last_pass);

    assert_eq!(tail, 10, "ten appends went onto the log");
    assert!(
        collect_until_quiet(&mut live, QUIET).await.is_empty(),
        "the live stream emitted nothing beyond one envelope per changed offset"
    );
    assert_eq!(
        applied.len(),
        7,
        "three of the ten appends moved no projection and emitted nothing"
    );
    let displayed = laps_of(live_body(applied.last().unwrap()), "A");
    assert_eq!(displayed, 4, "the console is showing A on four laps");

    // The old client's inferred cursor: one per APPLIED envelope, from the snapshot's 0.
    let inferred = applied.len() as u64;
    assert!(
        inferred < tail,
        "the inferred cursor ({inferred}) really does lag the true tail ({tail}) — the premise"
    );
    // The server's own answer, echoed on the last envelope, is the true offset.
    assert_eq!(
        env_cursor(applied.last().unwrap()),
        tail,
        "every envelope states the offset it was folded through"
    );
    drop(live);

    // The blip: resubscribe from the STALE cursor, without re-snapshotting.
    let mut resumed = subscribe(
        &url,
        &SubscribeRequest {
            scope: event_scope(),
            from: Some(Cursor::new(inferred)),
            contract_version: None,
            token: None,
        },
    )
    .await;
    let replay = collect_until_quiet(&mut resumed, QUIET).await;

    // The whole sequence, not just the last frame: no envelope may carry FEWER laps than the
    // console already had. This is the assertion the bug fails.
    for (i, message) in replay.iter().enumerate() {
        let laps = laps_of(live_body(message), "A");
        assert!(
            laps >= displayed,
            "envelope {i} of the resumed stream stepped A back from {displayed} to {laps} laps"
        );
    }
    // And the span is collapsed: one settled envelope, not a staircase.
    assert_eq!(
        replay.len(),
        1,
        "a replayed span is one settled fold, not one envelope per offset"
    );
    assert_eq!(laps_of(live_body(&replay[0]), "A"), 4);
    assert_eq!(env_cursor(&replay[0]), tail);
}

/// **#422 — a resume from the ECHOED cursor has nothing to replay at all.**
///
/// The collapse is the safety net; this is the cause fixed. A client that stores each envelope's
/// own `cursor` resumes at the exact offset it stands on, so the engine seeds itself with the
/// identical fold and the stream stays silent until something genuinely new lands.
#[tokio::test]
async fn a_resume_from_the_echoed_cursor_replays_nothing() {
    let (registry, state) = test_registry();
    let (url, _server) = serve(registry.clone()).await;

    let mut live = subscribe(
        &url,
        &SubscribeRequest {
            scope: event_scope(),
            from: None,
            contract_version: None,
            token: None,
        },
    )
    .await;
    let (last_pass, applied) = run_four_lap_heat(&state, Some(&mut live)).await;
    let exact = env_cursor(applied.last().unwrap());
    assert_eq!(exact, tail_after(last_pass));
    drop(live);

    let mut resumed = subscribe(
        &url,
        &SubscribeRequest {
            scope: event_scope(),
            from: Some(Cursor::new(exact)),
            contract_version: None,
            token: None,
        },
    )
    .await;
    assert!(
        collect_until_quiet(&mut resumed, QUIET).await.is_empty(),
        "an exact resume cursor leaves the server nothing to send"
    );

    // …and the resumed stream is still live: the next real change arrives, moving forward.
    state
        .append(heat_changed("q-1", HeatTransition::Finished), None)
        .unwrap();
    let next = next_message(&mut resumed).await;
    assert_eq!(
        seq(&next),
        1,
        "the resumed stream's own sequence starts at 1"
    );
    assert_eq!(live_body(&next).phase, HeatPhase::Unofficial);
    assert_eq!(laps_of(live_body(&next), "A"), 4, "no lap was lost");
}

/// **#422 constraint — a genuine marshaling correction must still lower the count.**
///
/// GridFPV has real corrections: a marshal voids a pass and a lap legitimately disappears. That is
/// the thing the spurious replay was indistinguishable from, so removing the replay must not also
/// remove the real one. Both paths are checked: the void reaching a *connected* console as its own
/// envelope, and the void reaching a *reconnecting* one through the collapsed catch-up span — where
/// the settled fold is genuinely lower than what that client last displayed, and is sent anyway.
#[tokio::test]
async fn a_marshal_void_still_lowers_the_live_lap_count() {
    let (registry, state) = test_registry();
    let (url, _server) = serve(registry.clone()).await;

    let (last_pass, _) = run_four_lap_heat(&state, None).await;
    let before_void = tail_after(last_pass);

    // 1. The connected console: the void lands as its own tail envelope and the count drops.
    let mut connected = subscribe(
        &url,
        &SubscribeRequest {
            scope: event_scope(),
            from: Some(Cursor::new(before_void)),
            contract_version: None,
            token: None,
        },
    )
    .await;
    state
        .append(
            Event::DetectionVoided {
                target: LogRef(last_pass),
            },
            None,
        )
        .unwrap();
    let voided = next_message(&mut connected).await;
    assert_eq!(
        laps_of(live_body(&voided), "A"),
        3,
        "voiding A's closing pass takes the lap back on the live stream"
    );
    drop(connected);

    // 2. The reconnecting console: it last displayed four laps and resumes from a cursor BEFORE
    //    the void. The collapsed span must still deliver the corrected — lower — count. A fix that
    //    merely clamped the stream to "never decrease" would hide the marshal's ruling here.
    let mut resumed = subscribe(
        &url,
        &SubscribeRequest {
            scope: event_scope(),
            from: Some(Cursor::new(before_void)),
            contract_version: None,
            token: None,
        },
    )
    .await;
    let replay = collect_until_quiet(&mut resumed, QUIET).await;
    assert_eq!(replay.len(), 1, "one settled fold for the replayed span");
    assert_eq!(
        laps_of(live_body(&replay[0]), "A"),
        3,
        "the real correction survives the collapse — the count goes down, as it should"
    );
}

/// **#422 constraint — the stale-cursor guard is neither widened nor narrowed.**
///
/// The collapse changes what an *in-window* resume is shown; it must not change which cursors are
/// in window. Pinned from both sides of the boundary: `tail - RETAINED_WINDOW` is served, one
/// offset below it is refused. (`too_old_cursor_requires_re_snapshot`, above, covers the far past.)
#[tokio::test]
async fn the_retained_window_boundary_is_unchanged() {
    let (registry, state) = test_registry();
    let (url, _server) = serve(registry.clone()).await;

    let mut last = 0;
    for _ in 0..(gridfpv_server::ws::RETAINED_WINDOW + 50) {
        last = state.append(heat_scheduled("q-1", &["A"]), None).unwrap();
    }
    let tail = tail_after(last);
    let oldest_replayable = tail - gridfpv_server::ws::RETAINED_WINDOW;

    // Exactly at the boundary: still replayable, so it is served (collapsed) rather than refused.
    let mut inside = subscribe(
        &url,
        &SubscribeRequest {
            scope: event_scope(),
            from: Some(Cursor::new(oldest_replayable)),
            contract_version: None,
            token: None,
        },
    )
    .await;
    let served = collect_until_quiet(&mut inside, QUIET).await;
    assert_eq!(
        served.len(),
        1,
        "a cursor at the boundary is replayed (collapsed to one envelope), never refused"
    );
    assert!(matches!(served[0], StreamMessage::Change(_)));

    // One offset below it: refused, exactly as before.
    let mut outside = subscribe(
        &url,
        &SubscribeRequest {
            scope: event_scope(),
            from: Some(Cursor::new(oldest_replayable - 1)),
            contract_version: None,
            token: None,
        },
    )
    .await;
    match next_message(&mut outside).await {
        StreamMessage::ReSnapshotRequired(err) => {
            assert_eq!(err.code, gridfpv_server::error::ErrorCode::StaleCursor);
        }
        other => panic!("expected ReSnapshotRequired, got {other:?}"),
    }
}
