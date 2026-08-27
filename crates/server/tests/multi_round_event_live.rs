//! Mock-RH end-to-end **multi-round event** (#47) — a whole event, driven live against
//! dockerized RotorHazard, observed only through the protocol surface.
//!
//! `tests/full_event_live.rs` is the sibling of this test: it drives **one** heat very hard
//! (marshaling corrections, the 200-vs-404 pilot-scope rule, the auth gate). That is a genuine
//! subset of #47, not a completion of it — the issue asks for a *full multi-round event*. This
//! test is that arc:
//!
//! 1. a **`timed_qual`** round (two qualifying heats over a four-pilot class), each heat flown
//!    live against dockerized RH, real timer passes ingested through the adapter seam;
//! 2. **`SeedingRule::FromRanking`** — a second round whose field is drawn from round 1's
//!    ranking, *not* from the roster (the two orders are deliberately different, so a
//!    seeding regression cannot pass);
//! 3. a **`head_to_head`** main over that seeded field, flown live;
//! 4. the **round + class standings** as the final assertion.
//!
//! # Everything through the wire
//!
//! The event is *built* through the RD's HTTP surface (`POST /pilots`, `POST /classes`,
//! `POST /timers`, `POST /events`, `PUT …/roster`, `PUT …/classes/…/membership`,
//! `POST …/rounds`) and *driven* through the control path (`POST /events/{e}/control`:
//! `FillRound`, `SetCurrentHeat`, `Stage`, `Start`, `SkipCountdown`, `ForceEnd`, `Finalize`,
//! `Advance`). Every assertion
//! reads a protocol surface — the WS change stream, the event/heat snapshot endpoints, and the
//! round-ranking / round-standings / class-standings reads. The engine is never called directly.
//!
//! The one non-wire seam is the **source adapter's** own: the timer's passes are appended
//! through [`AppState::append`], remapped `node-{n} → lineup[n]` and stamped with the running
//! heat, exactly as the Director's RH bridge does (`gridfpv-app`'s `source::rotorhazard::remap`).
//! That is an *observation* being ingested, not an RD command — protocol.html §5.
//!
//! # `EventOutcome`
//!
//! [`ProjectionBody::EventOutcome`](gridfpv_server::snapshot::ProjectionBody::EventOutcome)
//! exists in the contract but **no route serves it today**, so the wire-visible "whole event
//! result" is the round ranking + round standings + class standings triple this test ends on.
//! When an event-outcome route lands, assert it here too.
//!
//! # Determinism — why this test cannot pass by luck
//!
//! Lap *timing* is not controllable (RH's mock interface reads its CSV continuously), so, like
//! every `*_live` test, the assertions are structural. What is NOT left to chance is the
//! **outcome**:
//!
//! - **Node cadences are fixed and well separated.** Each node's `ticks_per_lap` ([`NODE_TICKS`])
//!   is chosen so its lap time is 1.2s / 2.0s / 3.0s / 4.0s, and every value **divides
//!   [`gridfpv_testkit::TOTAL_TICKS`] (600)** so the CSV loops seamlessly at EOF. A
//!   non-dividing cadence produces one spurious short crossing on every wrap — measured, and
//!   exactly the kind of stray lap that would invert a best-lap ranking.
//! - **Every heat is held open until *every* seated node has produced [`MIN_CROSSINGS`]
//!   crossings** (the discipline `crates/engine/tests/common/mod.rs::run_mock_heat_until` added
//!   after three engine tests were found passing by luck: stopping on the first crossing makes
//!   the pass count race the stop-and-drain window). The exact count still varies by a crossing
//!   or two on the fast nodes — that is what a real timer does — so **no assertion depends on
//!   it**: the test asserts a *floor* on every pilot's laps and a *strict ordering* between
//!   pilots, both of which the ~4x cadence spread makes unambiguous.
//! - **The finishing order of every heat is fixed by the cadences**, and the round-1 ranking
//!   ([`QUAL_ORDER`]), the round-2 seeded lineup, the round-2 ranking ([`MAIN_ORDER`]) and the
//!   class standings ([`EXPECTED_STANDINGS`]) are all derived consequences of it, worked out up
//!   front rather than read back from the run.
//!
//! # Why the heats are long
//!
//! #403 reached the field because RotorHazard's own win condition truncated a run at three laps
//! and *every* harness heat was too short to notice. Each heat here runs until the slowest node
//! has banked [`MIN_CROSSINGS`] crossings — the fastest node banks ~20 laps — and the test
//! asserts both a per-pilot lap floor and a strictly-decreasing lap count down the finishing
//! order. A timer that stopped counting at lap 3 would flatten that order and fail here.
//!
//! # Failing loudly
//!
//! Every stage asserts **positively** that it advanced, so a stage that silently stopped
//! advancing fails at that stage rather than sliding to a green end-of-test:
//!
//! - a fill acks [`FillStop::SingleStep`] carrying **exactly one** scheduled heat (never a bare
//!   "ok" with nothing appended — the ambiguity #395 closed);
//! - each phase transition is *observed on the change stream* for that heat, with a strictly
//!   increasing sequence;
//! - the loop between qualifying heats runs on [`AdvanceStop::Generated`] /
//!   [`AdvanceStop::LoadedOnDeck`] and asserts Live control really moved (#401); every other
//!   `AdvanceStop` — including `AwaitingResult` and `Blocked` — is a hard failure naming the
//!   stage;
//! - each heat's scored result is read back and asserted, competitor by competitor;
//! - each round's *completion* is asserted twice over, as [`AdvanceStop::RoundComplete`] and as
//!   [`FillStop::Complete`].
//!
//! Local-only class (needs Docker). DISTINCT RH port 5046. Run:
//!
//! ```sh
//! cargo test -p gridfpv-server --features live --test multi_round_event_live -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use gridfpv_adapters::rotorhazard::RotorHazardAdapter;
use gridfpv_adapters::rotorhazard::transport::RotorHazardConnection;
use gridfpv_engine::format::RankEntry;
use gridfpv_engine::scoring::{HeatResult, WinCondition};
use gridfpv_events::{ClassId, CompetitorRef, Event, HeatId, RoundId};
use gridfpv_server::app::{AppState, router};
use gridfpv_server::classes::{Class, CreateClassRequest};
use gridfpv_server::control::{
    AdvanceOutcome, AdvanceStop, Command, CommandAck, CommandOutcome, FillMode, FillRoundOutcome,
    FillStop, ScheduledHeat,
};
use gridfpv_server::events::EventRegistry;
use gridfpv_server::events::{
    ChannelMode, CreateEventRequest, EventMeta, MemberSlot, NewRoundReq, RoundDef, SeedingRule,
    SetClassMembershipRequest, SetEventClassesRequest, SetEventRosterRequest,
};
use gridfpv_server::pilots::{CreatePilotRequest, Pilot};
use gridfpv_server::round_engine::{ClassStandings, RoundStanding};
use gridfpv_server::scope::{EventId, Scope, SubscribeRequest};
use gridfpv_server::snapshot::{HeatPhase, LiveRaceState, ProjectionBody, Snapshot};
use gridfpv_server::stream::{Change, StreamMessage};
use gridfpv_server::timers::{
    ChannelCapability, CreateTimerRequest, PluginPresence, SetEventTimersRequest, Timer, TimerKind,
};
use gridfpv_testkit::{NodeCsv, RhContainer, node_csv};
use serde::de::DeserializeOwned;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

// ---------------------------------------------------------------------------------------
// The scenario — every number here is load-bearing for determinism. See the module docs.
// ---------------------------------------------------------------------------------------

/// DISTINCT RH host port for this e2e — every live target owns one so no two containers can
/// collide (adapters 5030-5031, engine 5032-5039 + 5044, server single-heat e2e 5041, app
/// 5042-5043 + 5045, here 5046).
const RH_PORT: u16 = 5046;

/// The CSV tick interval (seconds), matching every other harness.
const TICK: &str = "0.1";

/// Ticks per lap for RH mock nodes 0..3, i.e. lap cadences of **3.0s / 1.2s / 2.0s / 4.0s** at
/// the 0.1s tick.
///
/// Two properties are deliberate and must be preserved together:
///
/// 1. **Every value divides `gridfpv_testkit::TOTAL_TICKS` (600).** The mock CSV loops at EOF;
///    a cadence that does not divide the file length puts a *partial* lap at the wrap, which RH
///    records as one extra, very short crossing on that node. Measured on a 4-node probe: the
///    node at `ticks_per_lap: 16` (600/16 = 37.5) logged a 0.8s lap where its cadence is 1.6s,
///    while the nodes at 12 / 20 / 24 wrapped cleanly. A stray lap like that can invert a
///    best-lap ranking, so the whole ladder divides 600.
/// 2. **The speed order is not the seat order** — node 1 is fastest, then node 2, then node 0,
///    then node 3. Round 1's ranking is therefore a genuine permutation of the roster, so
///    round 2's `FromRanking` field is provably not the roster order, and the round-2 result is
///    a different permutation again (see [`QUAL_ORDER`] / [`MAIN_ORDER`]).
const NODE_TICKS: [usize; 4] = [30, 12, 20, 40];

/// How many crossings **every seated node** must produce before a heat is closed.
///
/// The slowest node (4.0s laps) governs, so a heat runs ~25s and the fastest node banks ~20
/// laps — long enough that a timer that stopped counting at lap 3 (#403) could not pass the
/// per-heat lap assertions. Holding for a stated count (rather than stopping on the first
/// crossing) is the `run_mock_heat_until` discipline; see the module docs.
const MIN_CROSSINGS: usize = 6;

/// The lap floor every pilot must clear in every heat: `K` crossings are `K - 1` laps.
const MIN_LAPS: u32 = (MIN_CROSSINGS as u32) - 1;

/// The class's four pilots, in **membership order** — the order round 1 seeds `FromRoster` from
/// and the order its heats line up in (`node-{n}` flies `lineup[n]`).
const CALLSIGNS: [&str; 4] = ["Ratchet", "Kestrel", "Vulcan", "Torrent"];

/// The finishing order of any heat that lines the field up in membership order, as **indices
/// into the heat's lineup**: nodes sorted fastest-first ([`NODE_TICKS`] ascending) — node 1,
/// node 2, node 0, node 3.
const SPEED_ORDER: [usize; 4] = [1, 2, 0, 3];

/// Round 1's expected ranking, as indices into [`CALLSIGNS`]. The qualifying heats line the
/// class up in membership order, so the ranking is exactly [`SPEED_ORDER`]: Kestrel, Vulcan,
/// Ratchet, Torrent.
const QUAL_ORDER: [usize; 4] = SPEED_ORDER;

/// Round 2's expected ranking, as indices into [`CALLSIGNS`].
///
/// The main's lineup is round 1's ranking ([`QUAL_ORDER`]), so lineup position `j` is flown by
/// `CALLSIGNS[QUAL_ORDER[j]]` on node `j` — a *re-seat*, which reshuffles the result. Position
/// `k` of the main is flown by lineup slot `SPEED_ORDER[k]`, i.e.
/// `QUAL_ORDER[SPEED_ORDER[k]]` = `[2, 0, 1, 3]`: Vulcan, Ratchet, Kestrel, Torrent.
const MAIN_ORDER: [usize; 4] = [2, 0, 1, 3];

/// The expected **class standings** order and points, as `(CALLSIGNS index, points)`.
///
/// A round awards `field_size - position + 1`, so over a 4-pilot field each round pays 4/3/2/1
/// down its ranking. Summing [`QUAL_ORDER`] and [`MAIN_ORDER`]:
///
/// | pilot   | qual pos → pts | main pos → pts | total |
/// |---------|----------------|----------------|-------|
/// | Vulcan  | 2 → 3          | 1 → 4          | **7** |
/// | Kestrel | 1 → 4          | 3 → 2          | **6** |
/// | Ratchet | 3 → 2          | 2 → 3          | **5** |
/// | Torrent | 4 → 1          | 4 → 1          | **2** |
///
/// Every total is distinct, so the standings order carries no tie-break and cannot flicker.
const EXPECTED_STANDINGS: [(usize, u32); 4] = [(2, 7), (1, 6), (0, 5), (3, 2)];

/// The channels the four class members are permanently assigned (Raceband R1–R4) — a static
/// (GQ-style) qualifying round races its members on their fixed channels.
const MEMBER_CHANNELS: [u16; 4] = [5658, 5695, 5732, 5769];

/// How many qualifying heats round 1 runs (its `rounds` / "heats per pilot" param). Two, so the
/// round-driven fill loop is exercised more than once before the round reports complete.
const QUAL_HEATS: usize = 2;

// ---------------------------------------------------------------------------------------
// HTTP / WS plumbing (a hand-rolled HTTP/1.1 client, so the test pulls in no HTTP client dep —
// the same shape `tests/control.rs` and `tests/full_event_live.rs` use).
// ---------------------------------------------------------------------------------------

/// Serve `router(registry)` on an ephemeral port; return the `127.0.0.1:port` address and the
/// server task handle (dropped at test end, aborting the task).
async fn serve(registry: EventRegistry) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(registry);
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("{addr}"), handle)
}

/// One HTTP/1.1 request; returns `(status, body)`.
async fn request(
    addr: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<String>,
) -> (u16, String) {
    let body = body.unwrap_or_default();
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let raw = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         {auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(raw.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no status line on {method} {path}: {response}"));
    let body = response
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or_default()
        .to_string();
    (status, body)
}

/// An RD-authenticated write (`POST`/`PUT`) that MUST succeed; parses the JSON response.
async fn rd_write<Req: serde::Serialize, Res: DeserializeOwned>(
    addr: &str,
    method: &str,
    path: &str,
    token: &str,
    body: &Req,
) -> Res {
    let (status, response) = request(
        addr,
        method,
        path,
        Some(token),
        Some(serde_json::to_string(body).unwrap()),
    )
    .await;
    assert_eq!(status, 200, "{method} {path} should succeed: {response}");
    serde_json::from_str(&response)
        .unwrap_or_else(|e| panic!("{method} {path} response did not parse ({e}): {response}"))
}

/// An open read that MUST succeed; parses the JSON response.
async fn read_json<T: DeserializeOwned>(addr: &str, path: &str) -> T {
    let (status, response) = request(addr, "GET", path, None, None).await;
    assert_eq!(status, 200, "GET {path} should be 200: {response}");
    serde_json::from_str(&response)
        .unwrap_or_else(|e| panic!("GET {path} did not parse ({e}): {response}"))
}

/// `POST /events/{event}/control` with the RD token; asserts the command was accepted.
async fn rd_command(addr: &str, event: &EventId, command: &Command, token: &str) -> CommandAck {
    let path = format!("/events/{}/control", event.0);
    let (status, response) = request(
        addr,
        "POST",
        &path,
        Some(token),
        Some(serde_json::to_string(command).unwrap()),
    )
    .await;
    assert_eq!(
        status, 200,
        "control {command:?} should be admitted: {response}"
    );
    let ack: CommandAck = serde_json::from_str(&response)
        .unwrap_or_else(|e| panic!("ack did not parse ({e}): {response}"));
    assert!(ack.ok, "control {command:?} should ack ok: {ack:?}");
    ack
}

/// The [`FillRoundOutcome`] a `FillRound` ack carries — never inferred from an absence (#395).
fn fill_outcome(ack: &CommandAck) -> FillRoundOutcome {
    match ack.outcome.clone() {
        Some(CommandOutcome::FillRound(outcome)) => outcome,
        other => panic!("a FillRound ack must carry a FillRound outcome, got {other:?}"),
    }
}

/// Connect a `/stream` reader, subscribing to `scope` from the snapshot `cursor`.
async fn subscribe(addr: &str, event: &EventId, subscribe: &SubscribeRequest) -> Ws {
    let (mut ws, _) = connect_async(format!("ws://{addr}/events/{}/stream", event.0))
        .await
        .unwrap();
    ws.send(Message::text(serde_json::to_string(subscribe).unwrap()))
        .await
        .unwrap();
    ws
}

/// Await the next [`StreamMessage`] text frame, with a timeout so a missing frame fails the
/// test rather than hanging.
async fn next_message(ws: &mut Ws) -> StreamMessage {
    let frame = tokio::time::timeout(Duration::from_secs(10), ws.next())
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

/// The `LiveRaceState` carried by a fresh-value `Change` (the only encoding an event scope emits).
fn live_body(message: &StreamMessage) -> LiveRaceState {
    match message {
        StreamMessage::Change(env) => match &env.change {
            Change::FreshValue(ProjectionBody::LiveRaceState(ls)) => ls.clone(),
            other => panic!("expected a fresh-value live-state, got {other:?}"),
        },
        other => panic!("expected a Change, got {other:?}"),
    }
}

/// Pump live-state envelopes until one for `heat` whose `phase` is `target` arrives, returning
/// that envelope's sequence. Tolerant of the engine emitting one envelope per fold-changing
/// append (passes between transitions also bump the sequence).
async fn await_phase(ws: &mut Ws, heat: &HeatId, target: HeatPhase) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let message = next_message(ws).await;
        let ls = live_body(&message);
        if ls.current_heat.as_ref() == Some(heat) && ls.phase == target {
            return seq(&message);
        }
        assert!(
            Instant::now() < deadline,
            "never observed heat {heat:?} in phase {target:?} on the stream"
        );
    }
}

/// Drain any change envelopes already queued (the ones the appended passes produced), without
/// blocking once the stream goes quiet. Each drained frame must be a well-formed live-state
/// `Change` — proving the passes flowed through as ordered envelopes.
async fn drain_envelopes(ws: &mut Ws) {
    loop {
        match tokio::time::timeout(Duration::from_millis(500), ws.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let message: StreamMessage =
                    serde_json::from_str(&text).expect("parse StreamMessage");
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

// ---------------------------------------------------------------------------------------
// The dockerized-RH driver (the source-adapter seam).
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

/// The RH node index `node-{n}` encodes, if any — the seat handle the adapter reports.
fn node_index(competitor: &CompetitorRef) -> Option<usize> {
    competitor.0.strip_prefix("node-")?.parse().ok()
}

/// Per-node crossing counts over a batch of adapter events.
fn crossings_per_node(events: &[Event]) -> BTreeMap<usize, usize> {
    let mut counts = BTreeMap::new();
    for event in events {
        if let Event::Pass(pass) = event {
            if let Some(node) = node_index(&pass.competitor) {
                *counts.entry(node).or_insert(0) += 1;
            }
        }
    }
    counts
}

/// Run **one heat** on the dockerized RotorHazard at `rh_url` over `seats` nodes, holding the
/// race open until **every** seated node has produced at least [`MIN_CROSSINGS`] crossings.
///
/// Returns the `Event::Pass`es the timer produced (still carrying `node-{n}` refs — the caller
/// remaps them onto the heat's lineup, as the Director's RH bridge does). Blocking; run under
/// `spawn_blocking` so the Socket.IO polling does not stall the async server task.
///
/// Panics — loudly, naming the per-node counts — if any seat falls short. A heat that quietly
/// produced two laps instead of twenty is precisely the failure this test exists to catch.
fn run_rh_heat(rh_url: &str, seats: usize, heat: &str) -> Vec<Event> {
    let conn = RotorHazardConnection::connect(rh_url, RotorHazardAdapter::new())
        .expect("connect to RotorHazard");

    // Settle, then reset RH so this heat's race starts from a known place regardless of the
    // previous heat's state (the container is shared across the whole event).
    std::thread::sleep(Duration::from_secs(2));
    conn.stop_race().ok();
    conn.discard_laps().expect("discard_laps");
    std::thread::sleep(Duration::from_secs(2));
    let _ = conn.events(); // drop the reset's snapshot churn

    // Zero RH's own staging delays / time limit so the race starts the instant we stage it —
    // the same call the Director's bridge makes at Stage (Grid owns all timing).
    let _ = conn.prepare_instant_start();
    std::thread::sleep(Duration::from_millis(500));
    let _ = conn.events();

    conn.stage_race().expect("stage_race");
    let mut live: Vec<Event> = Vec::new();
    assert!(
        wait_until(&conn, &mut live, Duration::from_secs(20), |evs| {
            evs.iter()
                .any(|e| matches!(e, Event::SessionStarted { .. }))
        }),
        "RotorHazard never reached RACING for heat {heat}"
    );

    // Hold the race open until EVERY seat has banked MIN_CROSSINGS crossings — the slowest node
    // governs, so the fast nodes bank many more. 120s is a generous ceiling: the slowest node's
    // cadence is 4.0s, so the target is reached in ~25s.
    let reached = wait_until(&conn, &mut live, Duration::from_secs(120), |evs| {
        let counts = crossings_per_node(evs);
        (0..seats).all(|node| counts.get(&node).copied().unwrap_or(0) >= MIN_CROSSINGS)
    });

    // Close the race and drain the final crossings.
    conn.stop_race().ok();
    std::thread::sleep(Duration::from_millis(800));
    live.extend(conn.events());
    conn.disconnect();

    let counts = crossings_per_node(&live);
    assert!(
        reached,
        "heat {heat} was held open for {MIN_CROSSINGS} crossings on each of {seats} seats, \
         but the timer only produced {counts:?}"
    );

    // Only `Pass`es are the heat's canonical race-engine observations; and only the seated
    // nodes' (an idle node outside the lineup is dropped, exactly as the bridge's remap does).
    live.into_iter()
        .filter(|e| match e {
            Event::Pass(p) => node_index(&p.competitor).is_some_and(|n| n < seats),
            _ => false,
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// Event setup, through the RD's HTTP surface.
// ---------------------------------------------------------------------------------------

/// The four directory pilots, an "Open" class, a 4-node RotorHazard timer, and an event that
/// selects all three — every step a real RD's console performs, over the real routes.
///
/// Returns `(event id, class id, pilot refs in membership order)`.
async fn build_event(
    registry: &EventRegistry,
    addr: &str,
    token: &str,
    rh_url: &str,
) -> (EventId, ClassId, Vec<CompetitorRef>) {
    // --- The application-level directories (pilots / classes / timers) ---
    let mut pilots: Vec<Pilot> = Vec::new();
    for callsign in CALLSIGNS {
        pilots.push(
            rd_write(
                addr,
                "POST",
                "/pilots",
                token,
                &CreatePilotRequest {
                    callsign: callsign.to_string(),
                    ..Default::default()
                },
            )
            .await,
        );
    }
    let class: Class = rd_write(
        addr,
        "POST",
        "/classes",
        token,
        &CreateClassRequest {
            name: "Open".into(),
            source: Default::default(),
            reference: None,
            description: None,
        },
    )
    .await;
    // The timer names the very RotorHazard this event races on. The server never dials it (the
    // Director's bridge does, in `gridfpv-app`); here it supplies the node cap and channel pool
    // the round engine allocates from.
    let timer: Timer = rd_write(
        addr,
        "POST",
        "/timers",
        token,
        &CreateTimerRequest {
            name: "Harness RH".into(),
            kind: TimerKind::Rotorhazard {
                url: rh_url.to_string(),
            },
            channel_capability: Some(ChannelCapability::Flexible),
            node_count: Some(CALLSIGNS.len() as u32),
            available_channels: Some(MEMBER_CHANNELS.to_vec()),
        },
    )
    .await;

    // This test runs `gridfpv-server` WITHOUT the Director's RH bridge (see above) — nothing ever
    // dials the timer, so its `PluginPresence` would stay `None` forever. Since #405 a RotorHazard
    // timer can only be selected for an event once a probe has shown the GridFPV plugin present,
    // so stand in for that probe the same way this test already stands in for the bridge. Without
    // it the selection below is correctly refused with "it has not been connected yet".
    registry.timers().set_plugin(
        &timer.id,
        PluginPresence::Present {
            plugin_version: "e2e".into(),
            rhapi_version: "1.3".into(),
            capabilities: vec!["hello".into(), "live_pass".into()],
        },
    );

    // --- The event, and its selections ---
    let event: EventMeta = rd_write(
        addr,
        "POST",
        "/events",
        token,
        &CreateEventRequest {
            name: "Multi-round e2e".into(),
            date: None,
            location: None,
            description: None,
            organizer: None,
        },
    )
    .await;
    let event_id = event.id.clone();
    let _: EventMeta = rd_write(
        addr,
        "PUT",
        &format!("/events/{}/timers", event_id.0),
        token,
        &SetEventTimersRequest {
            ids: vec![timer.id.clone()],
            primary: Some(timer.id.clone()),
        },
    )
    .await;
    let _: EventMeta = rd_write(
        addr,
        "PUT",
        &format!("/events/{}/classes", event_id.0),
        token,
        &SetEventClassesRequest {
            ids: vec![class.id.clone()],
        },
    )
    .await;
    let _: EventMeta = rd_write(
        addr,
        "PUT",
        &format!("/events/{}/roster", event_id.0),
        token,
        &SetEventRosterRequest {
            pilot_ids: pilots.iter().map(|p| p.id.clone()).collect(),
        },
    )
    .await;
    // Class membership WITH fixed channels — what a static (GQ-style) qualifying round races on.
    let meta: EventMeta = rd_write(
        addr,
        "PUT",
        &format!("/events/{}/classes/{}/membership", event_id.0, class.id.0),
        token,
        &SetClassMembershipRequest {
            pilots: pilots
                .iter()
                .zip(MEMBER_CHANNELS)
                .map(|(pilot, channel)| MemberSlot {
                    pilot: pilot.id.clone(),
                    channel: Some(channel),
                })
                .collect(),
        },
    )
    .await;
    assert_eq!(
        meta.classes,
        vec![class.id.clone()],
        "the event selected its class"
    );

    // The round engine addresses competitors by their pilot id (the field it builds from
    // membership), so that is the ref the lineups, results and standings all carry.
    let field: Vec<CompetitorRef> = pilots
        .iter()
        .map(|p| CompetitorRef(p.id.0.clone()))
        .collect();
    (event_id, class.id, field)
}

/// Add the **qualifying** round: a static-channel `timed_qual` over `class`, seeded from the
/// roster, scored on most laps in a (generous) timed window.
async fn add_qualifying(addr: &str, token: &str, event: &EventId, class: &ClassId) -> RoundDef {
    rd_write(
        addr,
        "POST",
        &format!("/events/{}/rounds", event.0),
        token,
        &NewRoundReq {
            label: "Qualifying".into(),
            classes: vec![class.clone()],
            // #117 S3: this event names no channel layouts, so its rounds fly none —
            // assignment falls to the IMD auto-pick, which is what this e2e exercised before.
            layouts: Vec::new(),
            format: "timed_qual".into(),
            params: BTreeMap::from([("rounds".to_string(), QUAL_HEATS.to_string())]),
            // Most laps in the window. The window is far longer than a harness heat, so every
            // banked lap counts — the ranking is by raw lap count, which is a *count*, immune to
            // the stray-fast-lap hazard a best-lap condition would carry.
            win_condition: Some(WinCondition::Timed {
                window_micros: 120_000_000,
            }),
            seeding: SeedingRule::FromRoster,
            time_limit_secs: Some(120),
            // The console's default for `timed_qual`; stated explicitly because the whole
            // scenario (fixed per-member channels, one channel-balanced heat per format round)
            // depends on it.
            channel_mode: Some(ChannelMode::Static),
            staging_timer_secs: None,
            start_procedure: None,
            grace_window: None,
            protest_window: None,
            min_lap_secs: None,
        },
    )
    .await
}

/// Add the **main**: a `head_to_head` round whose field is drawn from the qualifying round's
/// ranking ([`SeedingRule::FromRanking`]) — the whole top four in one heat.
async fn add_main(
    addr: &str,
    token: &str,
    event: &EventId,
    class: &ClassId,
    qualifying: &RoundId,
) -> RoundDef {
    rd_write(
        addr,
        "POST",
        &format!("/events/{}/rounds", event.0),
        token,
        &NewRoundReq {
            label: "A-Main".into(),
            classes: vec![class.clone()],
            // #117 S3: this event names no channel layouts, so its rounds fly none —
            // assignment falls to the IMD auto-pick, which is what this e2e exercised before.
            layouts: Vec::new(),
            format: "head_to_head".into(),
            params: BTreeMap::from([
                ("group_size".to_string(), CALLSIGNS.len().to_string()),
                ("rotations".to_string(), "1".to_string()),
                ("scoring".to_string(), "placement".to_string()),
            ]),
            win_condition: Some(WinCondition::Timed {
                window_micros: 120_000_000,
            }),
            seeding: SeedingRule::FromRanking {
                source_rounds: vec![qualifying.clone()],
                top_n: CALLSIGNS.len(),
            },
            time_limit_secs: Some(120),
            channel_mode: Some(ChannelMode::PerHeat),
            staging_timer_secs: None,
            start_procedure: None,
            grace_window: None,
            protest_window: None,
            min_lap_secs: None,
        },
    )
    .await
}

// ---------------------------------------------------------------------------------------
// Driving one heat, end to end, through the protocol.
// ---------------------------------------------------------------------------------------

/// The context one heat needs: the addresses and handles that stay fixed for the whole event.
struct Rig {
    addr: String,
    event: EventId,
    token: String,
    state: AppState,
    rh_url: String,
}

/// Fill the round's **next** heat, asserting the fill positively reported scheduling exactly one.
async fn fill_next_heat(rig: &Rig, round: &RoundId) -> ScheduledHeat {
    let ack = rd_command(
        &rig.addr,
        &rig.event,
        &Command::FillRound {
            round: round.clone(),
            mode: FillMode::Next,
        },
        &rig.token,
    )
    .await;
    let outcome = fill_outcome(&ack);
    assert_eq!(
        outcome.stopped,
        FillStop::SingleStep,
        "a single-step fill on a round with racing left must report SingleStep, not \
         {:?} ({})",
        outcome.stopped,
        outcome.detail
    );
    assert_eq!(
        outcome.scheduled.len(),
        1,
        "a SingleStep fill schedules exactly one heat, got {:?} ({})",
        outcome.scheduled,
        outcome.detail
    );
    let scheduled = outcome.scheduled.into_iter().next().expect("one heat");
    // A fill puts the heat **on deck**; the RD then brings it into Live control. (Only
    // `HeatStateChanged` / `CurrentHeatSelected` move `current_heat`, by design — scheduling a
    // heat mid-event must not yank Live control off the heat on the timer.) Mid-round the
    // `Advance` below does this for the RD; the first heat of a round is selected explicitly.
    rd_command(
        &rig.addr,
        &rig.event,
        &Command::SetCurrentHeat {
            heat: scheduled.heat.clone(),
        },
        &rig.token,
    )
    .await;
    scheduled
}

/// **Advance** a finalized heat — the RD's "this one is done, what's next?" — and return the
/// [`AdvanceOutcome`] saying what Live control moved on to (#401). Never inferred from an
/// absence: the outcome states the answer positively.
async fn advance_heat(rig: &Rig, heat: &HeatId) -> AdvanceOutcome {
    let ack = rd_command(
        &rig.addr,
        &rig.event,
        &Command::Advance { heat: heat.clone() },
        &rig.token,
    )
    .await;
    match ack.outcome {
        Some(CommandOutcome::Advance(outcome)) => outcome,
        other => panic!("an Advance ack must carry an Advance outcome, got {other:?}"),
    }
}

/// Assert the round is **finished**: another fill appends nothing and says so positively.
async fn assert_round_complete(rig: &Rig, round: &RoundId) {
    let ack = rd_command(
        &rig.addr,
        &rig.event,
        &Command::FillRound {
            round: round.clone(),
            mode: FillMode::Next,
        },
        &rig.token,
    )
    .await;
    let outcome = fill_outcome(&ack);
    assert_eq!(
        outcome.stopped,
        FillStop::Complete,
        "the round should be finished; the fill said {:?} ({})",
        outcome.stopped,
        outcome.detail
    );
    assert!(
        outcome.scheduled.is_empty(),
        "a finished round schedules nothing more, got {:?}",
        outcome.scheduled
    );
}

/// The event-scope live state, read fresh off the snapshot endpoint.
async fn event_live(rig: &Rig) -> LiveRaceState {
    let snapshot: Snapshot = read_json(
        &rig.addr,
        &format!("/events/{0}/snapshot/event/{0}", rig.event.0),
    )
    .await;
    match snapshot.body {
        ProjectionBody::LiveRaceState(ls) => ls,
        other => panic!("expected a live-state snapshot, got {other:?}"),
    }
}

/// Drive one scheduled heat all the way to `Final` against dockerized RH, asserting positively
/// at every step, and return its scored [`HeatResult`] read back off the heat-scope snapshot.
async fn run_heat(rig: &Rig, stream: &mut Ws, scheduled: &ScheduledHeat) -> HeatResult {
    let heat = scheduled.heat.clone();
    let lineup = scheduled.lineup.clone();
    assert_eq!(
        lineup.len(),
        CALLSIGNS.len(),
        "every heat in this event lines up the whole four-pilot field: {lineup:?}"
    );
    assert_eq!(
        scheduled.frequencies.len(),
        lineup.len(),
        "heat {:?} ({}) should carry a channel per pilot, got {:?}",
        heat,
        scheduled.name,
        scheduled.frequencies
    );

    // The snapshot already reflects the scheduled heat — the fill's append reached the read path.
    let scheduled_state = event_live(rig).await;
    assert_eq!(
        scheduled_state.current_heat.as_ref(),
        Some(&heat),
        "the freshly filled heat is the current heat"
    );
    assert_eq!(scheduled_state.phase, HeatPhase::Scheduled);
    assert_eq!(
        scheduled_state.active_pilots, lineup,
        "the live state carries the heat's lineup in seeding order"
    );

    // --- The heat loop, driven through the control path and read back off the stream. ---
    rd_command(
        &rig.addr,
        &rig.event,
        &Command::Stage { heat: heat.clone() },
        &rig.token,
    )
    .await;
    let staged = await_phase(stream, &heat, HeatPhase::Staged).await;
    rd_command(
        &rig.addr,
        &rig.event,
        &Command::Start { heat: heat.clone() },
        &rig.token,
    )
    .await;
    let armed = await_phase(stream, &heat, HeatPhase::Armed).await;
    assert!(
        armed > staged,
        "the per-stream sequence is strictly increasing (Staged {staged} < Armed {armed})"
    );
    // SkipCountdown stands in for the Director's runtime auto-start (this test drives the loop
    // by hand rather than running the clock): force Armed -> Running.
    rd_command(
        &rig.addr,
        &rig.event,
        &Command::SkipCountdown { heat: heat.clone() },
        &rig.token,
    )
    .await;
    let running = await_phase(stream, &heat, HeatPhase::Running).await;
    assert!(
        running > armed,
        "the per-stream sequence is strictly increasing (Armed {armed} < Running {running})"
    );

    // --- The real race on dockerized RH, ingested through the source-adapter seam. ---
    let rh_url = rig.rh_url.clone();
    let heat_label = heat.0.clone();
    let seats = lineup.len();
    let passes = tokio::task::spawn_blocking(move || run_rh_heat(&rh_url, seats, &heat_label))
        .await
        .expect("RH driver thread");
    let counts = crossings_per_node(&passes);
    for node in 0..seats {
        assert!(
            counts.get(&node).copied().unwrap_or(0) >= MIN_CROSSINGS,
            "seat node-{node} produced {} crossings in heat {:?}; every seat must bank at least \
             {MIN_CROSSINGS} (counts: {counts:?})",
            counts.get(&node).copied().unwrap_or(0),
            heat
        );
    }
    // Remap `node-{n} -> lineup[n]` and stamp the running heat, exactly as the Director's RH
    // bridge does before it appends (`gridfpv-app`'s `source::rotorhazard::remap` + `PassSink`).
    for event in passes {
        let Event::Pass(mut pass) = event else {
            continue;
        };
        let Some(index) = node_index(&pass.competitor) else {
            continue;
        };
        let Some(competitor) = lineup.get(index).cloned() else {
            continue;
        };
        pass.competitor = competitor;
        pass.heat = Some(heat.clone());
        rig.state
            .append(Event::Pass(pass), None)
            .expect("append a real timer pass");
    }
    drain_envelopes(stream).await;

    // The live state has folded the passes: every pilot is banking laps while Running.
    let folded = event_live(rig).await;
    assert_eq!(folded.current_heat.as_ref(), Some(&heat));
    assert_eq!(folded.phase, HeatPhase::Running);
    for competitor in &lineup {
        let laps = folded
            .progress
            .iter()
            .find(|p| &p.competitor == competitor)
            .map(|p| p.laps_completed)
            .unwrap_or(0);
        assert!(
            laps >= MIN_LAPS,
            "live progress for {competitor:?} in heat {heat:?} is {laps} laps; every pilot must \
             clear the {MIN_LAPS}-lap floor (a truncated timer run, #403, lands here)"
        );
    }

    // --- Close the heat out. ForceEnd stands in for the runtime's auto-complete. ---
    rd_command(
        &rig.addr,
        &rig.event,
        &Command::ForceEnd { heat: heat.clone() },
        &rig.token,
    )
    .await;
    await_phase(stream, &heat, HeatPhase::Unofficial).await;
    rd_command(
        &rig.addr,
        &rig.event,
        &Command::Finalize { heat: heat.clone() },
        &rig.token,
    )
    .await;
    await_phase(stream, &heat, HeatPhase::Final).await;

    // --- The scored heat, off the protocol's heat-result projection. ---
    let snapshot: Snapshot = read_json(
        &rig.addr,
        &format!(
            "/events/{}/snapshot/heat/{}?projection=result",
            rig.event.0, heat.0
        ),
    )
    .await;
    let result = match snapshot.body {
        ProjectionBody::HeatResult(result) => result,
        other => panic!("expected a heat-result snapshot, got {other:?}"),
    };
    assert_eq!(
        result.places.len(),
        lineup.len(),
        "the scored heat places every pilot in the lineup"
    );
    result
}

/// Assert one heat's finishing order is the one the node cadences fix, with strictly decreasing
/// lap counts down the order.
///
/// The strictness is the point: a timer that stopped counting early would flatten the lap counts
/// into a tie and fail here, whatever the positions said.
fn assert_heat_order(result: &HeatResult, lineup: &[CompetitorRef], heat: &HeatId) {
    let mut places = result.places.clone();
    places.sort_by_key(|p| p.position);
    let finishers: Vec<CompetitorRef> = places
        .iter()
        .map(|p| p.competitor.competitor.clone())
        .collect();
    let expected: Vec<CompetitorRef> = SPEED_ORDER
        .iter()
        .map(|seat| lineup[*seat].clone())
        .collect();
    assert_eq!(
        finishers, expected,
        "heat {heat:?} must finish in node-cadence order (node 1 fastest, then 2, 0, 3)"
    );
    for (index, place) in places.iter().enumerate() {
        assert_eq!(
            place.position,
            index as u32 + 1,
            "heat {heat:?} positions are 1..n with no tie: {places:?}"
        );
        assert!(
            place.laps >= MIN_LAPS,
            "{:?} scored {} laps in heat {heat:?}; the floor is {MIN_LAPS}",
            place.competitor.competitor,
            place.laps
        );
    }
    for window in places.windows(2) {
        assert!(
            window[0].laps > window[1].laps,
            "heat {heat:?} must separate its pilots by laps ({:?} {} vs {:?} {}) — equal lap \
             counts mean the run was truncated before the cadences could tell them apart",
            window[0].competitor.competitor,
            window[0].laps,
            window[1].competitor.competitor,
            window[1].laps
        );
    }
}

/// The competitor refs of a ranking, in ranking order, asserting the positions are a clean
/// 1..n with no ties.
fn ranked(ranking: &[RankEntry], what: &str) -> Vec<CompetitorRef> {
    for (index, entry) in ranking.iter().enumerate() {
        assert_eq!(
            entry.position,
            index as u32 + 1,
            "{what} must rank 1..n with no ties: {ranking:?}"
        );
    }
    ranking.iter().map(|e| e.competitor.clone()).collect()
}

/// The expected competitor order for one of the `*_ORDER` index tables.
fn order_of(field: &[CompetitorRef], order: &[usize; 4]) -> Vec<CompetitorRef> {
    order.iter().map(|i| field[*i].clone()).collect()
}

// ---------------------------------------------------------------------------------------
// The e2e.
// ---------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker (spins up dockerized RotorHazard and drives a full multi-round event through the server)"]
async fn a_full_multi_round_event_runs_live_through_the_protocol() {
    // Four mock nodes at fixed, well-separated cadences (see NODE_TICKS).
    let scenario: Vec<(usize, String)> = NODE_TICKS
        .iter()
        .enumerate()
        .map(|(node, ticks)| {
            (
                node,
                node_csv(&NodeCsv {
                    ticks_per_lap: *ticks,
                    peak_rssi: 180,
                    baseline_rssi: 70,
                    seed: node as u64,
                }),
            )
        })
        .collect();

    // RAII: the container is dropped (and removed) at the end of the test. One container serves
    // the whole event — each heat resets it and runs its own race, as a real timer does.
    let rh = RhContainer::start(RH_PORT, TICK, &scenario);
    let rh_url = rh.url().to_string();

    // --- Stand the server up; the RD issues itself a token (which gates control from here on).
    let registry = EventRegistry::new(None).expect("fresh registry");
    let token = registry.tokens().issue_rd_token();
    let (addr, _server) = serve(registry.clone()).await;

    // === Setup: build the event through the RD's HTTP surface. ===
    let (event, class, field) = build_event(&registry, &addr, &token, &rh_url).await;
    let state = registry
        .resolve(&event)
        .expect("the created event resolves to its own log");
    let rig = Rig {
        addr: addr.clone(),
        event: event.clone(),
        token: token.clone(),
        state,
        rh_url: rh_url.clone(),
    };

    let qualifying = add_qualifying(&addr, &token, &event, &class).await;
    let main = add_main(&addr, &token, &event, &class, &qualifying.id).await;
    assert_eq!(
        main.seeding,
        SeedingRule::FromRanking {
            source_rounds: vec![qualifying.id.clone()],
            top_n: CALLSIGNS.len(),
        },
        "the main is seeded from the qualifying round's ranking"
    );

    // Attach the protocol client: snapshot first (for the cursor), then subscribe from it, so
    // the first envelope the client applies is exactly the one after the snapshot (§2, §3).
    let snapshot: Snapshot =
        read_json(&addr, &format!("/events/{0}/snapshot/event/{0}", event.0)).await;
    let mut stream = subscribe(
        &addr,
        &event,
        &SubscribeRequest {
            scope: Scope::Event {
                event: event.clone(),
            },
            from: Some(snapshot.cursor),
            contract_version: None,
            token: None,
        },
    )
    .await;

    // === Stage 1: the qualifying round, live. ===
    //
    // The RD's real loop: fill the round's first heat, drive it, then `Advance` — which records
    // the heat's `Final -> Advanced` and puts the round's NEXT heat in front of the RD. Both
    // acks report what they actually did (#395, #401), and both are asserted, so a round that
    // quietly stopped producing heats fails here instead of skipping to the standings.
    let mut heats_run = 0usize;
    let mut scheduled = fill_next_heat(&rig, &qualifying.id).await;
    loop {
        heats_run += 1;
        eprintln!(
            "multi-round e2e: qualifying heat {heats_run}/{QUAL_HEATS} — {} ({:?})",
            scheduled.name, scheduled.heat
        );
        assert_eq!(
            scheduled.lineup, field,
            "a FromRoster qualifying heat lines the class up in membership order"
        );
        let heat = scheduled.heat.clone();
        let result = run_heat(&rig, &mut stream, &scheduled).await;
        assert_heat_order(&result, &scheduled.lineup, &heat);

        let advanced = advance_heat(&rig, &heat).await;
        match advanced.stopped {
            // The round has another heat: `Advance` drew (or found) it and Live control is on it.
            AdvanceStop::Generated | AdvanceStop::LoadedOnDeck => {
                assert!(
                    heats_run < QUAL_HEATS,
                    "the qualifying round produced heat {} of a {QUAL_HEATS}-heat round: {}",
                    heats_run + 1,
                    advanced.detail
                );
                scheduled = advanced
                    .loaded
                    .clone()
                    .unwrap_or_else(|| panic!("{:?} must name the heat it loaded", advanced));
                assert_eq!(
                    event_live(&rig).await.current_heat.as_ref(),
                    Some(&scheduled.heat),
                    "Advance moved Live control onto the heat it loaded ({})",
                    advanced.detail
                );
            }
            // The round is finished — the terminal `Advance` answer, stated positively.
            AdvanceStop::RoundComplete => {
                assert!(
                    advanced.loaded.is_none(),
                    "a completed round loads nothing: {advanced:?}"
                );
                break;
            }
            other => panic!(
                "the qualifying round stopped advancing after heat {heats_run}: {other:?} ({})",
                advanced.detail
            ),
        }
    }
    assert_eq!(
        heats_run, QUAL_HEATS,
        "the qualifying round ran every heat its format asked for"
    );
    // …and the fill path agrees the round is finished (the same terminal state, its own ack).
    assert_round_complete(&rig, &qualifying.id).await;

    // The qualifying ranking, off the protocol read the seeding carry itself consumes.
    let qual_ranking: Vec<RankEntry> = read_json(
        &addr,
        &format!("/events/{}/rounds/{}/ranking", event.0, qualifying.id.0),
    )
    .await;
    let qual_order = ranked(&qual_ranking, "the qualifying ranking");
    assert_eq!(
        qual_order,
        order_of(&field, &QUAL_ORDER),
        "the qualifying ranking follows the node cadences, not the roster order"
    );
    assert_ne!(
        qual_order, field,
        "the qualifying ranking must differ from the roster order — otherwise the FromRanking \
         assertion below could not tell seeding from a passthrough"
    );

    // …and its standings, in the same order, with every pilot's banked laps above the floor.
    let qual_standings: Vec<RoundStanding> = read_json(
        &addr,
        &format!("/events/{}/rounds/{}/standings", event.0, qualifying.id.0),
    )
    .await;
    assert_eq!(
        qual_standings
            .iter()
            .map(|s| s.competitor.clone())
            .collect::<Vec<_>>(),
        qual_order,
        "round standings are served in ranking order"
    );
    for standing in &qual_standings {
        assert!(
            standing.laps >= MIN_LAPS,
            "{:?} banked {} laps across qualifying; the floor is {MIN_LAPS}",
            standing.competitor,
            standing.laps
        );
        assert!(
            standing.best_lap_micros.is_some(),
            "{:?} should have a best lap after two qualifying heats",
            standing.competitor
        );
    }
    for window in qual_standings.windows(2) {
        assert!(
            window[0].laps > window[1].laps,
            "qualifying standings must separate pilots by laps: {:?} {} vs {:?} {}",
            window[0].competitor,
            window[0].laps,
            window[1].competitor,
            window[1].laps
        );
    }

    // === Stage 2 + 3: the main — seeded FromRanking, then flown. ===
    let scheduled_main = fill_next_heat(&rig, &main.id).await;
    eprintln!(
        "multi-round e2e: main — {} ({:?})",
        scheduled_main.name, scheduled_main.heat
    );
    // THE seeding assertion: the main's field is the qualifying ranking, in ranking order — and
    // (asserted above) that order is not the roster order, so a FromRoster regression fails here.
    assert_eq!(
        scheduled_main.lineup, qual_order,
        "the head-to-head main is seeded from the qualifying ranking (SeedingRule::FromRanking)"
    );
    let main_result = run_heat(&rig, &mut stream, &scheduled_main).await;
    assert_heat_order(&main_result, &scheduled_main.lineup, &scheduled_main.heat);
    let advanced = advance_heat(&rig, &scheduled_main.heat).await;
    assert_eq!(
        advanced.stopped,
        AdvanceStop::RoundComplete,
        "advancing the main finishes the round; got {:?} ({})",
        advanced.stopped,
        advanced.detail
    );
    assert_round_complete(&rig, &main.id).await;

    // === Stage 4: the event's standings. ===
    let main_ranking: Vec<RankEntry> = read_json(
        &addr,
        &format!("/events/{}/rounds/{}/ranking", event.0, main.id.0),
    )
    .await;
    let main_order = ranked(&main_ranking, "the main's ranking");
    assert_eq!(
        main_order,
        order_of(&field, &MAIN_ORDER),
        "the main re-seats the field, so its ranking is a different permutation again"
    );
    assert_ne!(
        main_order, qual_order,
        "the main must actually re-rank the field, not echo the qualifying ranking"
    );

    let standings: ClassStandings = read_json(
        &addr,
        &format!("/events/{}/classes/{}/standings", event.0, class.0),
    )
    .await;
    assert_eq!(standings.class, class);
    assert_eq!(
        standings.standings.len(),
        CALLSIGNS.len(),
        "every pilot who raced the class has a standings row"
    );
    let expected: Vec<(CompetitorRef, u32)> = EXPECTED_STANDINGS
        .iter()
        .map(|(pilot, points)| (field[*pilot].clone(), *points))
        .collect();
    let actual: Vec<(CompetitorRef, u32)> = standings
        .standings
        .iter()
        .map(|s| (s.competitor.clone(), s.points))
        .collect();
    assert_eq!(
        actual, expected,
        "the class standings sum both rounds' points (4/3/2/1 down each ranking) in order"
    );
    for (index, standing) in standings.standings.iter().enumerate() {
        assert_eq!(
            standing.position,
            index as u32 + 1,
            "the class standings are 1..n with no tie: {:?}",
            standings.standings
        );
        assert_eq!(
            standing.rounds_entered, 2,
            "{:?} raced both the qualifying round and the main",
            standing.competitor
        );
        assert!(
            standing.total_laps >= MIN_LAPS * (QUAL_HEATS as u32 + 1),
            "{:?} banked {} laps across {} heats; the floor is {MIN_LAPS} per heat",
            standing.competitor,
            standing.total_laps,
            QUAL_HEATS + 1
        );
        assert!(
            standing.best_lap_micros.is_some(),
            "{:?} should carry a best lap in the class standings",
            standing.competitor
        );
    }

    eprintln!(
        "multi-round e2e: {} heats over 2 rounds; qualifying {:?}; main {:?}; standings {:?}",
        QUAL_HEATS + 1,
        qual_order.iter().map(|c| &c.0).collect::<Vec<_>>(),
        main_order.iter().map(|c| &c.0).collect::<Vec<_>>(),
        actual
            .iter()
            .map(|(c, p)| (c.0.as_str(), *p))
            .collect::<Vec<_>>()
    );
}
