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
use gridfpv_events::{CompetitorRef, Event, HeatId, HeatTransition};
use gridfpv_server::app::{AppState, router};
use gridfpv_server::events::{EventRegistry, PRACTICE_EVENT_ID};
use gridfpv_server::scope::{EventId, Scope, SubscribeRequest};
use gridfpv_server::snapshot::{HeatPhase, ProjectionBody};
use gridfpv_server::stream::{Change, Cursor, StreamMessage};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Serve `router(state)` on an ephemeral port; return the `ws://…/stream` URL and the
/// server task's join handle (dropped at test end, which aborts the task).
async fn serve(registry: EventRegistry) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(registry);
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("ws://{addr}/events/practice/stream"), handle)
}

/// A fresh registry plus its in-memory **Practice** [`AppState`] — the change-stream tests
/// append through Practice's log and subscribe under `/events/practice/stream` (issue #72).
fn practice_registry() -> (EventRegistry, AppState) {
    let registry = EventRegistry::new(None).unwrap();
    let state = registry
        .resolve(&EventId(PRACTICE_EVENT_ID.into()))
        .expect("Practice is always present");
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
    let (registry, state) = practice_registry();
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
    let (registry, state) = practice_registry();
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
    let (registry, state) = practice_registry();
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

/// A change that does not alter the scoped projection emits no envelope (the engine emits
/// one envelope per *change*, keeping the per-stream sequence a faithful change count).
#[tokio::test]
async fn unchanged_fold_emits_no_envelope() {
    let (registry, state) = practice_registry();
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

    // Re-scheduling the same heat with the same lineup folds to the same live state → no
    // new envelope. A following Running transition *does* change it and must arrive as
    // sequence 2 (proving sequence 2 was not silently consumed by the no-op append).
    state
        .append(heat_scheduled("q-1", &["A", "B"]), None)
        .unwrap();
    state
        .append(heat_changed("q-1", HeatTransition::Running), None)
        .unwrap();
    let m2 = next_message(&mut ws).await;
    assert_eq!(seq(&m2), 2, "the no-op append did not consume a sequence");
    assert_eq!(live_body(&m2).phase, HeatPhase::Running);
}

/// A malformed first frame closes the socket with a BadRequest close (no panic, no hang).
#[tokio::test]
async fn malformed_subscribe_closes_the_socket() {
    let (registry, _state) = practice_registry();
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
