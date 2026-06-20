//! RD control write-path integration tests (protocol.html §5) — issue #45.
//!
//! These spin the real [`router`] on an ephemeral `127.0.0.1` port with `axum::serve` and
//! drive the privileged control surface end to end — no Docker, no mocks of the transport:
//!
//! - `POST /control` — one [`Command`] in, one [`CommandAck`] back;
//! - `GET /control` — the bidirectional control WebSocket (commands up, acks down);
//! - and crucially the **read-back**: after a control append, a `/stream` subscriber
//!   observes the resulting change (§3, §5 — the resulting state reaches the RD on the read
//!   stream, not in the ack).
//!
//! Determinism: every command is explicit and each expected frame is awaited under a short
//! timeout, so there is no reliance on timing.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use gridfpv_events::{CompetitorRef, HeatId};
use gridfpv_server::app::{AppState, router};
use gridfpv_server::control::{Command, CommandAck};
use gridfpv_server::error::ErrorCode;
use gridfpv_server::scope::{Scope, SubscribeRequest};
use gridfpv_server::snapshot::{HeatPhase, ProjectionBody};
use gridfpv_server::stream::{Change, StreamMessage};
use gridfpv_storage::InMemoryLog;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Serve `router(state)` on an ephemeral port; return the base `http://…` address and the
/// server task handle (dropped at test end, aborting the task).
async fn serve(state: AppState) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state);
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("{addr}"), handle)
}

fn heat() -> HeatId {
    HeatId("q-1".into())
}

/// POST one command to `http://{addr}/control` and return the ack.
async fn post_command(addr: &str, command: &Command) -> CommandAck {
    // A tiny manual HTTP/1.1 POST so the test pulls in no extra HTTP client dependency.
    let body = serde_json::to_string(command).unwrap();
    let request = format!(
        "POST /control HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let json = response
        .split("\r\n\r\n")
        .nth(1)
        .expect("response has a body");
    serde_json::from_str(json).expect("body is a CommandAck")
}

/// Connect the control WS at `ws://{addr}/control`.
async fn control_ws(addr: &str) -> Ws {
    let (ws, _) = connect_async(format!("ws://{addr}/control")).await.unwrap();
    ws
}

/// Send a command frame on the control socket and await its ack frame.
async fn send_command(ws: &mut Ws, command: &Command) -> CommandAck {
    ws.send(Message::text(serde_json::to_string(command).unwrap()))
        .await
        .unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("timed out waiting for an ack")
        .expect("control socket closed")
        .expect("websocket error");
    match frame {
        Message::Text(text) => serde_json::from_str(&text).expect("parse CommandAck"),
        other => panic!("expected a text ack, got {other:?}"),
    }
}

/// Subscribe a `/stream` reader and await the next live-state phase.
async fn subscribe_stream(addr: &str, scope: Scope) -> Ws {
    let (mut ws, _) = connect_async(format!("ws://{addr}/stream")).await.unwrap();
    let request = SubscribeRequest { scope, from: None };
    ws.send(Message::text(serde_json::to_string(&request).unwrap()))
        .await
        .unwrap();
    ws
}

/// Await the next stream message's live-state phase.
async fn next_phase(ws: &mut Ws) -> HeatPhase {
    let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("timed out waiting for a stream frame")
        .expect("stream closed")
        .expect("websocket error");
    let message: StreamMessage = match frame {
        Message::Text(text) => serde_json::from_str(&text).unwrap(),
        Message::Close(c) => panic!("stream closed: {c:?}"),
        other => panic!("expected text, got {other:?}"),
    };
    match message {
        StreamMessage::Change(env) => match env.change {
            Change::FreshValue(ProjectionBody::LiveRaceState(ls)) => ls.phase,
            other => panic!("expected live-state, got {other:?}"),
        },
        other => panic!("expected a Change, got {other:?}"),
    }
}

/// `POST /control`: a legal heat-loop command acks ok and the resulting `HeatStateChanged`
/// reaches a `/stream` subscriber (the read-back, §5).
#[tokio::test]
async fn post_command_drives_heat_loop_and_reaches_stream() {
    let state = AppState::new(InMemoryLog::default());
    let (addr, _server) = serve(state.clone()).await;

    // Schedule the heat, then subscribe so the subscriber starts from the scheduled state.
    let ack = post_command(
        &addr,
        &Command::ScheduleHeat {
            heat: heat(),
            lineup: vec![CompetitorRef("A".into()), CompetitorRef("B".into())],
        },
    )
    .await;
    assert!(ack.ok, "schedule should ack ok: {ack:?}");

    let mut stream = subscribe_stream(&addr, Scope::Heat { heat: heat() }).await;
    // A fresh subscribe replays the log from the start, so the first envelope is the
    // already-scheduled state; consume it before driving new transitions.
    assert_eq!(next_phase(&mut stream).await, HeatPhase::Scheduled);

    // Stage via the control path; the subscriber observes the resulting transition.
    let ack = post_command(&addr, &Command::Stage { heat: heat() }).await;
    assert!(ack.ok, "stage should ack ok: {ack:?}");
    assert_eq!(next_phase(&mut stream).await, HeatPhase::Staged);

    // Arm, again read back off the stream.
    let ack = post_command(&addr, &Command::Arm { heat: heat() }).await;
    assert!(ack.ok, "arm should ack ok: {ack:?}");
    assert_eq!(next_phase(&mut stream).await, HeatPhase::Armed);
}

/// `GET /control` (the bidirectional WS): a Stage→Arm→Start sequence acks ok per command,
/// an illegal command is rejected with the shared error shape, and the resulting state is
/// readable on `/stream`.
#[tokio::test]
async fn control_ws_acks_each_command_and_rejects_illegal() {
    let state = AppState::new(InMemoryLog::default());
    let (addr, _server) = serve(state.clone()).await;

    let mut control = control_ws(&addr).await;

    // Schedule then subscribe.
    let ack = send_command(
        &mut control,
        &Command::ScheduleHeat {
            heat: heat(),
            lineup: vec![CompetitorRef("A".into())],
        },
    )
    .await;
    assert!(ack.ok);

    let mut stream = subscribe_stream(&addr, Scope::Heat { heat: heat() }).await;
    // Consume the replayed scheduled state (fresh subscribe replays from the start).
    assert_eq!(next_phase(&mut stream).await, HeatPhase::Scheduled);

    // Legal forward path over the same control socket.
    for (command, expected) in [
        (Command::Stage { heat: heat() }, HeatPhase::Staged),
        (Command::Arm { heat: heat() }, HeatPhase::Armed),
        (Command::Start { heat: heat() }, HeatPhase::Running),
    ] {
        let ack = send_command(&mut control, &command).await;
        assert!(ack.ok, "{command:?} should ack ok: {ack:?}");
        assert_eq!(next_phase(&mut stream).await, expected);
    }

    // An illegal command (Stage while Running) is a failed ack carrying the shared error.
    let ack = send_command(&mut control, &Command::Stage { heat: heat() }).await;
    assert!(!ack.ok);
    assert_eq!(ack.error.unwrap().code, ErrorCode::BadRequest);

    // The log still reflects only the legal transitions (nothing was appended for the
    // rejected command): the next legal command (Finish) acks ok.
    let ack = send_command(&mut control, &Command::Finish { heat: heat() }).await;
    assert!(ack.ok, "finish after running should ack ok: {ack:?}");
    // The `Finished` transition lands in the `Landed` live-state phase.
    assert_eq!(next_phase(&mut stream).await, HeatPhase::Landed);
}

/// A malformed control frame is answered with a failed ack, not a dropped socket: the next
/// well-formed command still works on the same session.
#[tokio::test]
async fn control_ws_survives_a_malformed_frame() {
    let state = AppState::new(InMemoryLog::default());
    let (addr, _server) = serve(state.clone()).await;
    let mut control = control_ws(&addr).await;

    control
        .send(Message::text("{not a command}"))
        .await
        .unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(5), control.next())
        .await
        .expect("timed out")
        .expect("closed")
        .expect("ws error");
    let ack: CommandAck = match frame {
        Message::Text(text) => serde_json::from_str(&text).unwrap(),
        other => panic!("expected an ack, got {other:?}"),
    };
    assert!(!ack.ok);
    assert_eq!(ack.error.unwrap().code, ErrorCode::BadRequest);

    // The session survives — a real command now acks ok.
    let ack = send_command(
        &mut control,
        &Command::ScheduleHeat {
            heat: heat(),
            lineup: vec![],
        },
    )
    .await;
    assert!(ack.ok);
}
