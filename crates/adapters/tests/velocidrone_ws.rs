//! Velocidrone WebSocket transport integration test (#26).
//!
//! Proves the live WS plumbing + decode end to end **without** a real Velocidrone:
//! it spins up an in-process mock WebSocket server (tungstenite `accept` on an
//! ephemeral `TcpListener`), replays the recorded Velocidrone frames from the
//! adapter fixture as text messages, points a [`VelocidroneConnection`] at it, and
//! asserts the translated [`Event`]s match exactly what the pure adapter produces
//! for those same frames (lifecycle, CompetitorSeen, Passes).
//!
//! **Fidelity caveat (#484):** this file's inline tungstenite mock sends **text** frames,
//! and the transport under test only decodes text frames — but the real 1.17.13 game
//! sends every message as a **binary** frame (see the KNOWN BROKEN note in
//! `src/velocidrone/transport.rs`). So this test proves the plumbing and the translation,
//! *not* real-game compatibility. When the #483 adapter work fixes the transport, switch
//! the server here to the wire-faithful `gridfpv_testkit::vd_mock` (binary frames, game
//! handshake, host gating) and this caveat disappears.
//!
//! Gated behind the `live` feature (it needs the transport), but **not** `#[ignore]`
//! — the mock server is in-process, so it needs no external service; `cargo xtask test`
//! runs it explicitly (the `test_vd_ws` leg):
//!
//! ```sh
//! cargo test -p gridfpv-adapters --features live --test velocidrone_ws
//! ```
#![cfg(feature = "live")]

use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};

use gridfpv_adapters::Adapter;
use gridfpv_adapters::velocidrone::transport::VelocidroneConnection;
use gridfpv_adapters::velocidrone::{Raw, VelocidroneAdapter};
use gridfpv_events::Event;
use tungstenite::Message;

/// The recorded session, shared with the adapter's own unit tests — one JSON array
/// of Velocidrone frames. Reused here so the WS path is asserted against the very
/// same fixture the translator is.
const FIXTURE: &str = include_str!("../src/velocidrone/fixtures/sprint.frames.json");

/// The expected canonical event stream: what the *pure* adapter produces for the
/// fixture frames. The transport must reproduce this exactly over the wire.
fn expected_events() -> Vec<Event> {
    let frames: Vec<Raw> = serde_json::from_str(FIXTURE).expect("fixture frames parse");
    let mut adapter = VelocidroneAdapter::with_default_id();
    let mut events = Vec::new();
    for frame in frames {
        events.extend(adapter.translate(frame));
    }
    events
}

/// The fixture frames, each re-serialized to the one-object-per-frame text form
/// Velocidrone puts on the wire.
fn fixture_frame_texts() -> Vec<String> {
    let frames: Vec<serde_json::Value> =
        serde_json::from_str(FIXTURE).expect("fixture is a JSON array of frames");
    frames
        .iter()
        .map(|f| serde_json::to_string(f).expect("frame re-serializes"))
        .collect()
}

/// Start an in-process mock Velocidrone WebSocket server on an ephemeral port.
///
/// Accepts exactly one connection, completes the WS handshake, sends each fixture
/// frame as a text message (plus a leading empty-string keep-alive, to prove the
/// client skips it), then keeps the socket open — draining the client's keep-alive
/// pings — until the client disconnects. Returns the `ws://…` URL to dial.
fn start_mock_server(frames: Vec<String>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");

    thread::spawn(move || {
        let (stream, _peer) = listener.accept().expect("accept one client");
        let mut ws = tungstenite::accept(stream).expect("ws handshake");

        // A leading empty-string keep-alive: the client must skip it, not decode it.
        ws.send(Message::text("")).expect("send keep-alive");
        for frame in &frames {
            ws.send(Message::text(frame.clone())).expect("send frame");
        }
        ws.flush().expect("flush frames");

        // Keep the connection open and drain whatever the client sends (its own
        // keep-alive pings) until it disconnects; then close.
        loop {
            match ws.read() {
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => {}
            }
        }
        let _ = ws.close(None);
    });

    format!("ws://{addr}/velocidrone")
}

/// Drain `conn` until it has accumulated at least `want` events or `timeout`
/// elapses; returns everything drained.
fn drain_until(conn: &VelocidroneConnection, want: usize, timeout: Duration) -> Vec<Event> {
    let deadline = Instant::now() + timeout;
    let mut got = Vec::new();
    while Instant::now() < deadline {
        got.extend(conn.events());
        if got.len() >= want {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    got
}

#[test]
fn replays_fixture_frames_over_a_mock_ws_server() {
    let expected = expected_events();
    assert!(!expected.is_empty(), "fixture should yield events");

    let url = start_mock_server(fixture_frame_texts());

    let conn = VelocidroneConnection::connect(&url).expect("connect to mock server");
    let got = drain_until(&conn, expected.len(), Duration::from_secs(10));
    conn.disconnect();

    assert_eq!(
        got, expected,
        "WS-transported events must match the pure adapter's output for the fixture"
    );
}

/// Sanity-check the empty-string keep-alive is treated as a ping, not a frame:
/// after the fixture, the event stream contains no spurious extra events from the
/// leading empty frame the mock server sends.
#[test]
fn empty_keep_alive_frames_are_skipped() {
    let expected = expected_events();
    let url = start_mock_server(fixture_frame_texts());

    let conn = VelocidroneConnection::connect(&url).expect("connect to mock server");
    let got = drain_until(&conn, expected.len(), Duration::from_secs(10));
    conn.disconnect();

    // Exactly the expected count — the empty keep-alive added nothing.
    assert_eq!(got.len(), expected.len());
}
