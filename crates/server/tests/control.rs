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
use gridfpv_server::app::AppState;
use gridfpv_server::app::router;
use gridfpv_server::control::{Command, CommandAck};
use gridfpv_server::error::ErrorCode;
use gridfpv_server::events::{EventRegistry, PRACTICE_EVENT_ID};
use gridfpv_server::scope::EventId;
use gridfpv_server::scope::{Scope, SubscribeRequest};
use gridfpv_server::snapshot::{HeatPhase, ProjectionBody};
use gridfpv_server::stream::{Change, StreamMessage};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::http::Uri;
use tokio_tungstenite::tungstenite::{ClientRequestBuilder, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Serve `router(state)` on an ephemeral port; return the base address, a freshly-minted
/// **RD bearer token** (control is RD-gated since #44), and the server task handle (dropped
/// at test end, aborting the task).
async fn serve(registry: EventRegistry) -> (String, String, tokio::task::JoinHandle<()>) {
    let rd_token = registry.tokens().issue_rd_token();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(registry);
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("{addr}"), rd_token, handle)
}

/// A fresh registry whose in-memory **Practice** event the control tests drive against, plus
/// its [`AppState`] (for token ops / direct appends). Every per-event path is rooted under
/// `/events/practice` (issue #72).
fn practice_registry() -> (EventRegistry, AppState) {
    let registry = EventRegistry::new(None).unwrap();
    let state = registry
        .resolve(&EventId(PRACTICE_EVENT_ID.into()))
        .expect("Practice is always present");
    (registry, state)
}

fn heat() -> HeatId {
    HeatId("q-1".into())
}

/// The HTTP status `POST /control` returns for `command`, authenticated with the optional
/// bearer `token` (the raw status line code, so an auth rejection — 401 — is visible).
async fn post_status(addr: &str, command: &Command, token: Option<&str>) -> u16 {
    let (status, _ack) = post_raw(addr, command, token).await;
    status
}

/// POST one command to `http://{addr}/control` with the optional bearer `token`; return the
/// HTTP status and (when the body parses) the [`CommandAck`].
async fn post_raw(addr: &str, command: &Command, token: Option<&str>) -> (u16, Option<CommandAck>) {
    // A tiny manual HTTP/1.1 POST so the test pulls in no extra HTTP client dependency.
    let body = serde_json::to_string(command).unwrap();
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST /events/practice/control HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
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

/// POST one command authenticated with an RD `token`, asserting it acks (200) and returning
/// the ack. The happy-path helper for the existing write-path tests.
async fn post_command(addr: &str, command: &Command, token: &str) -> CommandAck {
    let (status, ack) = post_raw(addr, command, Some(token)).await;
    assert_eq!(status, 200, "authenticated control should be admitted");
    ack.expect("body is a CommandAck")
}

/// Connect the control WS at `ws://{addr}/control`, authenticated with the RD `token`.
async fn control_ws(addr: &str, token: &str) -> Ws {
    let uri: Uri = format!("ws://{addr}/events/practice/control")
        .parse()
        .unwrap();
    let request =
        ClientRequestBuilder::new(uri).with_header("Authorization", format!("Bearer {token}"));
    let (ws, _) = connect_async(request).await.unwrap();
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
    let (mut ws, _) = connect_async(format!("ws://{addr}/events/practice/stream"))
        .await
        .unwrap();
    let request = SubscribeRequest {
        scope,
        from: None,
        contract_version: None,
        token: None,
    };
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
    let (registry, _state) = practice_registry();
    let (addr, rd, _server) = serve(registry.clone()).await;

    // Schedule the heat, then subscribe so the subscriber starts from the scheduled state.
    let ack = post_command(
        &addr,
        &Command::ScheduleHeat {
            heat: heat(),
            lineup: vec![CompetitorRef("A".into()), CompetitorRef("B".into())],
        },
        &rd,
    )
    .await;
    assert!(ack.ok, "schedule should ack ok: {ack:?}");

    let mut stream = subscribe_stream(&addr, Scope::Heat { heat: heat() }).await;
    // A fresh subscribe replays the log from the start, so the first envelope is the
    // already-scheduled state; consume it before driving new transitions.
    assert_eq!(next_phase(&mut stream).await, HeatPhase::Scheduled);

    // Stage via the control path; the subscriber observes the resulting transition.
    let ack = post_command(&addr, &Command::Stage { heat: heat() }, &rd).await;
    assert!(ack.ok, "stage should ack ok: {ack:?}");
    assert_eq!(next_phase(&mut stream).await, HeatPhase::Staged);

    // Arm, again read back off the stream.
    let ack = post_command(&addr, &Command::Arm { heat: heat() }, &rd).await;
    assert!(ack.ok, "arm should ack ok: {ack:?}");
    assert_eq!(next_phase(&mut stream).await, HeatPhase::Armed);
}

/// `GET /control` (the bidirectional WS): a Stage→Arm→Start sequence acks ok per command,
/// an illegal command is rejected with the shared error shape, and the resulting state is
/// readable on `/stream`.
#[tokio::test]
async fn control_ws_acks_each_command_and_rejects_illegal() {
    let (registry, _state) = practice_registry();
    let (addr, rd, _server) = serve(registry.clone()).await;

    let mut control = control_ws(&addr, &rd).await;

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
    // The `Finished` transition enters the `Finished` live-state phase.
    assert_eq!(next_phase(&mut stream).await, HeatPhase::Finished);
}

/// A malformed control frame is answered with a failed ack, not a dropped socket: the next
/// well-formed command still works on the same session.
#[tokio::test]
async fn control_ws_survives_a_malformed_frame() {
    let (registry, _state) = practice_registry();
    let (addr, rd, _server) = serve(registry.clone()).await;
    let mut control = control_ws(&addr, &rd).await;

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

/// `POST /control` is RD-token-gated (#44): no token, a read-only join-token, and a revoked
/// token are all rejected `401 Unauthorized`; a valid RD token is admitted (`200`).
#[tokio::test]
async fn control_post_requires_a_valid_rd_token() {
    let (registry, state) = practice_registry();
    let join_token = state.tokens().issue_join_token();
    let (addr, rd, _server) = serve(registry.clone()).await;
    let cmd = Command::ScheduleHeat {
        heat: heat(),
        lineup: vec![],
    };

    // No token → 401.
    assert_eq!(post_status(&addr, &cmd, None).await, 401);
    // A read-only join-token never grants control → 401.
    assert_eq!(post_status(&addr, &cmd, Some(&join_token)).await, 401);
    // An unknown/garbage token → 401.
    assert_eq!(post_status(&addr, &cmd, Some("not-a-token")).await, 401);

    // A valid RD token is admitted.
    assert_eq!(post_status(&addr, &cmd, Some(&rd)).await, 200);

    // Revoke the RD token; it stops working. (Issue a second RD token first so control stays
    // gated after the revoke — removing the *last* credential would open the Director.)
    let keep = state.tokens().issue_rd_token();
    assert!(state.tokens().revoke(&rd));
    assert_eq!(post_status(&addr, &cmd, Some(&rd)).await, 401);
    assert_eq!(post_status(&addr, &cmd, Some(&keep)).await, 200);
}

/// `GET /control` (the WS upgrade) is gated the same way: a connection without a valid RD
/// token is refused at the HTTP handshake (a non-101 status), and the
/// [`StatusCode::UNAUTHORIZED`] is surfaced by the handshake error.
#[tokio::test]
async fn control_ws_upgrade_requires_a_valid_rd_token() {
    let (registry, state) = practice_registry();
    let join_token = state.tokens().issue_join_token();
    let (addr, rd, _server) = serve(registry.clone()).await;

    // No token: the upgrade is refused.
    let no_token = connect_async(format!("ws://{addr}/events/practice/control")).await;
    assert!(
        no_token.is_err(),
        "an unauthenticated control upgrade is refused"
    );

    // A read-only join-token: still refused with 401.
    let uri: Uri = format!("ws://{addr}/events/practice/control")
        .parse()
        .unwrap();
    let req =
        ClientRequestBuilder::new(uri).with_header("Authorization", format!("Bearer {join_token}"));
    match connect_async(req).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
        other => panic!("expected a 401 http error, got {other:?}"),
    }

    // A valid RD token upgrades successfully.
    let mut control = control_ws(&addr, &rd).await;
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

/// Reads stay open on the LAN (#44, §5): a `/stream` subscribe with **no** token works, and
/// a read-only **join-token** authenticates a read all the same — neither grants control.
#[tokio::test]
async fn reads_are_open_and_a_join_token_authenticates_reads() {
    let (registry, state) = practice_registry();
    let join_token = state.tokens().issue_join_token();
    let (addr, rd, _server) = serve(registry.clone()).await;

    // Schedule a heat (as the RD) so there is a live state to read.
    let ack = post_command(
        &addr,
        &Command::ScheduleHeat {
            heat: heat(),
            lineup: vec![CompetitorRef("A".into())],
        },
        &rd,
    )
    .await;
    assert!(ack.ok);

    // An anonymous (no-token) reader gets the live state.
    let mut anon = subscribe_stream(&addr, Scope::Heat { heat: heat() }).await;
    assert_eq!(next_phase(&mut anon).await, HeatPhase::Scheduled);

    // A reader presenting the read-only join-token also gets the live state.
    let (mut ws, _) = connect_async(format!("ws://{addr}/events/practice/stream"))
        .await
        .unwrap();
    let request = SubscribeRequest {
        scope: Scope::Heat { heat: heat() },
        from: None,
        contract_version: None,
        token: Some(join_token),
    };
    ws.send(Message::text(serde_json::to_string(&request).unwrap()))
        .await
        .unwrap();
    assert_eq!(next_phase(&mut ws).await, HeatPhase::Scheduled);
}

/// Contract-version negotiation on the WS subscribe (#46, §7): an out-of-band version gets
/// the "please refresh" close (a `VersionMismatch` error in the close frame), while an
/// in-band version connects and streams.
#[tokio::test]
async fn out_of_band_contract_version_is_told_to_refresh() {
    use gridfpv_server::ContractVersion;
    use gridfpv_server::error::{ErrorCode, ProtocolError};

    let (registry, _state) = practice_registry();
    let (addr, rd, _server) = serve(registry.clone()).await;
    let ack = post_command(
        &addr,
        &Command::ScheduleHeat {
            heat: heat(),
            lineup: vec![],
        },
        &rd,
    )
    .await;
    assert!(ack.ok);

    // Too-new version → the stream closes with a VersionMismatch refresh signal.
    let (mut ws, _) = connect_async(format!("ws://{addr}/events/practice/stream"))
        .await
        .unwrap();
    let too_new = SubscribeRequest {
        scope: Scope::Heat { heat: heat() },
        from: None,
        contract_version: Some(ContractVersion::new(9_999)),
        token: None,
    };
    ws.send(Message::text(serde_json::to_string(&too_new).unwrap()))
        .await
        .unwrap();
    // The refresh signal is the typed error in a text frame (the close frame that follows
    // carries only the short error code, to stay under the WS control-frame limit).
    let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("timed out")
        .expect("closed")
        .expect("ws error");
    match frame {
        Message::Text(text) => {
            let err: ProtocolError =
                serde_json::from_str(&text).expect("refresh frame is a ProtocolError");
            assert_eq!(err.code, ErrorCode::VersionMismatch);
        }
        other => panic!("expected a refresh error frame, got {other:?}"),
    }

    // An in-band version connects and streams normally.
    let (mut ws, _) = connect_async(format!("ws://{addr}/events/practice/stream"))
        .await
        .unwrap();
    let in_band = SubscribeRequest {
        scope: Scope::Heat { heat: heat() },
        from: None,
        contract_version: Some(gridfpv_server::CONTRACT_VERSION),
        token: None,
    };
    ws.send(Message::text(serde_json::to_string(&in_band).unwrap()))
        .await
        .unwrap();
    assert_eq!(next_phase(&mut ws).await, HeatPhase::Scheduled);
}
