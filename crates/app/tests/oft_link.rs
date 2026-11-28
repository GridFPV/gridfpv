//! The OFT connection against a fake timer.
//!
//! The fake is a small WebSocket server that speaks the part of the OFT protocol Grid uses:
//! `hello` on connect, a replay of crossings after the `since` the client asks for, a
//! `pong` for every `ping`, and (when switched on) control: claims, a holder, and a `/config`
//! that refuses a write without the holder's key (spec §6.6). It runs in this process, so the test needs no hardware and no
//! checkout of the OFT project.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use gridfpv_events::{
    AdapterId, Command, CompletionRule, EndCondition, Event, FirstLap, Format, HeatSeat, PhaseDef,
    PhaseId, PhaseKind, PilotId, RaceId, RaceRules, SeatRef, SeatSource,
};
use gridfpv_server::control::with_event;
use gridfpv_server::events::{CreateEventRequest, EventId, EventRegistry};
use gridfpv_server::timers::{CreateTimerRequest, TimerKind};

/// What the fake timer holds: its boot, and every crossing it has "detected" as
/// (seq, seat, t_mid).
#[derive(Clone)]
struct Fake {
    booted: Instant,
    crossings: Arc<Mutex<Vec<(u64, u32, u64)>>>,
    /// The `since` of each subscription received, oldest first (`None` when left out).
    subscriptions: Arc<Mutex<Vec<Option<u64>>>>,
    /// Set to make the fake drop the connection once, to force a reconnect.
    hang_up: Arc<Mutex<bool>>,
    /// The receivers the timer lists, in its `hello` and at `/info`. Empty: none yet.
    nodes: Arc<Mutex<serde_json::Value>>,
    /// The receiver its `status` says serves every seat. `None`: it sends no `status`
    /// (unless it has control, below: then it always does).
    status_node: Arc<Mutex<Option<String>>>,
    /// Its boot id. A new one is a restarted timer.
    boot: Arc<Mutex<String>>,
    /// Who may write (spec v0.9, §6.6), and the settings that guards.
    control: Arc<Mutex<FakeControl>>,
    /// Counts the connections, so each claim knows which one has it.
    connections: Arc<Mutex<u64>>,
    /// The `manage_url` at `/info` (spec v0.12). `None`: left out, as an older timer does.
    manage_url: Arc<Mutex<Option<String>>>,
    /// The `problems` in its `status` (spec v0.12). `None`: left out, as an older timer
    /// does. Set, the fake sends `status`.
    problems: Arc<Mutex<Option<Vec<String>>>>,
}

/// One standing claim on the fake.
#[derive(Clone, Debug)]
struct FakeClaim {
    id: u32,
    name: String,
    key: String,
    /// The connection that has it. `None`: its stream dropped and it may still come back.
    /// `Some(0)` is a program that is not Grid, played by the test.
    connection: Option<u64>,
}

/// The fake's control, kept the way spec §6.6 says, without the ten-second clock: a
/// holder whose stream drops keeps control until the test calls [`FakeControl::expire`].
#[derive(Default)]
struct FakeControl {
    /// Whether the timer has control at all. Off, it is a timer older than v0.9.
    enabled: bool,
    next_id: u32,
    /// Oldest first.
    claims: Vec<FakeClaim>,
    holder: Option<u32>,
    /// Set to refuse every new claim with `control_full`.
    full: bool,
    /// Counts every change, so each connection knows to say where things stand.
    changes: u64,
    /// Every `control` message a client sent, oldest first.
    received: Vec<serde_json::Value>,
    /// The settings, and how often they changed.
    config: serde_json::Value,
    config_rev: u32,
    /// The `OFT-Control` header of every write that arrived.
    write_keys: Vec<Option<String>>,
}

impl FakeControl {
    fn holder(&self) -> Option<&FakeClaim> {
        self.claims
            .iter()
            .find(|claim| Some(claim.id) == self.holder)
    }

    /// When the holder's claim ends, control passes to the claim that has stood longest.
    fn pass_on(&mut self) {
        if self.holder().is_none() {
            self.holder = self.claims.first().map(|claim| claim.id);
        }
        self.changes += 1;
    }

    /// A claim arrives on `connection`. `Err` is `control_full`.
    fn claim(&mut self, connection: u64, name: &str, key: Option<&str>) -> Result<u32, ()> {
        if let Some(claim) = self
            .claims
            .iter_mut()
            .find(|claim| key.is_some_and(|key| key == claim.key))
        {
            // The key of a standing claim takes that claim up on the new stream.
            claim.connection = Some(connection);
            self.changes += 1;
            return Ok(claim.id);
        }
        if let Some(claim) = self
            .claims
            .iter_mut()
            .find(|claim| claim.connection == Some(connection))
        {
            claim.name = name.to_owned();
            self.changes += 1;
            return Ok(claim.id);
        }
        if self.full {
            return Err(());
        }
        self.next_id += 1;
        let id = self.next_id;
        self.claims.push(FakeClaim {
            id,
            name: name.to_owned(),
            key: format!("key{id:05x}"),
            connection: Some(connection),
        });
        if self.holder.is_none() {
            self.holder = Some(id);
        }
        self.changes += 1;
        Ok(id)
    }

    /// Another program, not Grid, claims. Gives the claim's id.
    fn other_claims(&mut self, name: &str) -> u32 {
        self.claim(0, name, None).unwrap()
    }

    /// A claim ends: it released, or its program went away for good.
    fn end(&mut self, id: u32) {
        self.claims.retain(|claim| claim.id != id);
        if self.holder == Some(id) {
            self.holder = None;
        }
        self.pass_on();
    }

    /// A stream closed. A waiting claim ends at once; the holder's stands, away.
    fn closed(&mut self, connection: u64) {
        let holder = self.holder;
        self.claims
            .retain(|claim| claim.connection != Some(connection) || Some(claim.id) == holder);
        for claim in &mut self.claims {
            if claim.connection == Some(connection) {
                claim.connection = None;
            }
        }
        self.changes += 1;
    }

    /// The ten seconds are up for a holder whose stream dropped.
    fn expire(&mut self) {
        if let Some(id) = self
            .holder()
            .filter(|claim| claim.connection.is_none())
            .map(|claim| claim.id)
        {
            self.end(id);
        }
    }

    /// The timer's own page hands control to a claim, or to nobody.
    fn grant(&mut self, id: Option<u32>) {
        self.holder = id;
        self.changes += 1;
    }

    /// A restart: no claims, and the settings' changes are counted from the start.
    fn restart(&mut self) {
        self.claims.clear();
        self.holder = None;
        self.config_rev = 0;
        self.changes += 1;
    }

    /// The `control` message for one connection.
    fn message(&self, connection: u64) -> serde_json::Value {
        let mine = self
            .claims
            .iter()
            .find(|claim| claim.connection == Some(connection));
        let holder = self.holder();
        let mut message = serde_json::json!({
            "type": "control",
            "holder": holder.map(|h| serde_json::json!({"id": h.id, "name": h.name})),
            "claim": mine.map(|claim| claim.id),
        });
        if let (Some(mine), Some(holder)) = (mine, holder)
            && mine.id == holder.id
        {
            message["key"] = holder.key.clone().into();
        }
        message
    }
}

async fn read_config(State(fake): State<Fake>) -> axum::Json<serde_json::Value> {
    axum::Json(fake.control.lock().unwrap().config.clone())
}

/// `PUT /config`, guarded as spec §6.6 says: while a claim holds control, a write without
/// its key is a `423` and changes nothing.
async fn write_config(
    State(fake): State<Fake>,
    headers: axum::http::HeaderMap,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> (axum::http::StatusCode, axum::Json<serde_json::Value>) {
    let sent = headers
        .get("OFT-Control")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let mut control = fake.control.lock().unwrap();
    control.write_keys.push(sent.clone());
    if let Some(holder) = control.holder()
        && sent.as_deref() != Some(holder.key.as_str())
    {
        let refusal = serde_json::json!({"error": format!("controlled by {}", holder.name)});
        return (axum::http::StatusCode::LOCKED, axum::Json(refusal));
    }
    control.config = body.clone();
    control.config_rev += 1;
    (axum::http::StatusCode::OK, axum::Json(body))
}

async fn info(State(fake): State<Fake>) -> axum::Json<serde_json::Value> {
    let mut info = serde_json::json!({
        "id": "oft-fake", "boot_id": "b001", "proto": 1, "calibrated": false,
        "nodes": fake.nodes.lock().unwrap().clone(),
    });
    if let Some(url) = fake.manage_url.lock().unwrap().clone() {
        info["manage_url"] = url.into();
    }
    axum::Json(info)
}

async fn stream(State(fake): State<Fake>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| serve(fake, socket))
}

async fn serve(fake: Fake, mut socket: WebSocket) {
    let connection = {
        let mut connections = fake.connections.lock().unwrap();
        *connections += 1;
        *connections
    };
    let has_control = fake.control.lock().unwrap().enabled;
    let mut hello = serde_json::json!({
        "type": "hello", "proto": 1, "timer": "oft-fake",
        "boot_id": fake.boot.lock().unwrap().clone(),
        "time_us": fake.booted.elapsed().as_micros() as u64,
        "seats": [{"seat": 0, "node": 0, "rate_hz": 500}, {"seat": 1, "node": 0, "rate_hz": 500}],
        "nodes": fake.nodes.lock().unwrap().clone(),
    });
    if has_control {
        hello["capabilities"] = serde_json::json!({
            "config": {"persist": false, "seat_thresholds": true, "pass_window_ms": true},
            "control": {},
        });
    }
    if socket.send(Message::text(hello.to_string())).await.is_err() {
        return;
    }
    // Where control stands goes out straight after `hello`, subscribed or not, and again
    // after every change.
    let mut control_said: Option<u64> = None;
    if has_control {
        let (changes, message) = {
            let control = fake.control.lock().unwrap();
            (control.changes, control.message(connection))
        };
        control_said = Some(changes);
        if socket
            .send(Message::text(message.to_string()))
            .await
            .is_err()
        {
            fake.control.lock().unwrap().closed(connection);
            return;
        }
    }
    serve_stream(&fake, &mut socket, connection, control_said).await;
    if has_control {
        fake.control.lock().unwrap().closed(connection);
    }
}

async fn serve_stream(
    fake: &Fake,
    socket: &mut WebSocket,
    connection: u64,
    mut control_said: Option<u64>,
) {
    // The highest crossing sent on this connection. Nothing is sent before a subscription.
    let mut sent: Option<u64> = None;
    let mut subscribed = false;
    // Signal: ten 1 ms samples every tick for seat 0, following on exactly, once asked for.
    let mut samples = false;
    let mut sample_t0: u64 = 0;
    let mut tick = tokio::time::interval(Duration::from_millis(10));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                // Hang up once when asked, to force a reconnect.
                if std::mem::take(&mut *fake.hang_up.lock().unwrap()) {
                    return;
                }
                if let Some(said) = control_said {
                    let (changes, message) = {
                        let control = fake.control.lock().unwrap();
                        (control.changes, control.message(connection))
                    };
                    if changes != said {
                        control_said = Some(changes);
                        if socket.send(Message::text(message.to_string())).await.is_err() {
                            return;
                        }
                    }
                }
                if !subscribed {
                    continue;
                }
                let status_node = fake.status_node.lock().unwrap().clone();
                let problems = fake.problems.lock().unwrap().clone();
                if status_node.is_some() || control_said.is_some() || problems.is_some() {
                    let mut status = serde_json::json!({
                        "type": "status", "uptime_s": 1,
                        "seats": [{"seat": 0, "rate_hz": 500, "rssi": 410,
                                   "floor": 400, "enter": 600, "exit": 550, "crossing": false}],
                    });
                    if let Some(node) = status_node {
                        status["seats"][0]["node"] = node.into();
                    }
                    if let Some(problems) = problems {
                        status["problems"] = problems.into();
                    }
                    if control_said.is_some() {
                        let control = fake.control.lock().unwrap();
                        status["config_rev"] = control.config_rev.into();
                        status["control"] = control
                            .holder()
                            .map(|h| serde_json::json!({"id": h.id, "name": h.name}))
                            .into();
                    }
                    if socket.send(Message::text(status.to_string())).await.is_err() {
                        return;
                    }
                }
                if samples {
                    let batch = serde_json::json!({
                        "type": "samples", "seat": 0, "t0": sample_t0, "dt_us": 1000,
                        "p": [-900, -900, -880, -700, -400, -400, -700, -880, -900, -900],
                        "nf": -910,
                    });
                    sample_t0 += 10_000;
                    if socket.send(Message::text(batch.to_string())).await.is_err() {
                        return;
                    }
                }
                let fresh: Vec<_> = fake
                    .crossings
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(seq, _, _)| sent.is_none_or(|sent| *seq > sent))
                    .copied()
                    .collect();
                for (seq, seat, t_mid) in fresh {
                    let crossing = serde_json::json!({
                        "type": "crossing", "seq": seq, "seat": seat,
                        "t_enter": t_mid - 10_000, "t_exit": t_mid + 10_000, "t_mid": t_mid,
                        "peak": -235, "snr": 685, "flags": [],
                    });
                    if socket.send(Message::text(crossing.to_string())).await.is_err() {
                        return;
                    }
                    sent = Some(seq);
                }
            }
            frame = socket.recv() => {
                let Some(Ok(frame)) = frame else { return };
                let Message::Text(text) = frame else { continue };
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                    continue;
                };
                match value["type"].as_str() {
                    Some("ping") => {
                        let pong = serde_json::json!({
                            "type": "pong",
                            "time_us": fake.booted.elapsed().as_micros() as u64,
                        });
                        if socket.send(Message::text(pong.to_string())).await.is_err() {
                            return;
                        }
                    }
                    Some("sub") => {
                        let since = value["crossings"]["since"].as_u64();
                        fake.subscriptions.lock().unwrap().push(since);
                        sent = since;
                        subscribed = true;
                        samples = value.get("samples").is_some();
                    }
                    Some("control") if control_said.is_some() => {
                        let made = {
                            let mut control = fake.control.lock().unwrap();
                            control.received.push(value.clone());
                            if value["release"].as_bool() == Some(true) {
                                let mine = control
                                    .claims
                                    .iter()
                                    .find(|claim| claim.connection == Some(connection))
                                    .map(|claim| claim.id);
                                if let Some(id) = mine {
                                    control.end(id);
                                }
                                Ok(0)
                            } else {
                                let name = value["claim"].as_str().unwrap_or("unnamed");
                                control.claim(connection, name, value["key"].as_str())
                            }
                        };
                        if made.is_err() {
                            let full = serde_json::json!({
                                "type": "error", "code": "control_full",
                                "msg": "this Timer has no room for another claim",
                            });
                            if socket.send(Message::text(full.to_string())).await.is_err() {
                                return;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn fake_timer() -> (Fake, SocketAddr) {
    let fake = Fake {
        booted: Instant::now(),
        crossings: Arc::default(),
        subscriptions: Arc::default(),
        hang_up: Arc::default(),
        nodes: Arc::new(Mutex::new(serde_json::json!([]))),
        status_node: Arc::default(),
        boot: Arc::new(Mutex::new("b001".into())),
        control: Arc::new(Mutex::new(FakeControl {
            config: serde_json::json!({
                "seats": [{"seat": 0, "freq_mhz": 5800, "enabled": true},
                          {"seat": 1, "freq_mhz": 5658, "enabled": true}],
                "threshold": {"mode": "auto", "snr_enter": 150, "hysteresis": 50},
                "min_lap_ms": 2000,
                "exit_hold_ms": 30,
                "pass_window_ms": 0,
            }),
            ..FakeControl::default()
        })),
        connections: Arc::default(),
        manage_url: Arc::default(),
        problems: Arc::default(),
    };
    let app = Router::new()
        .route("/oft/v1/stream", get(stream))
        .route("/oft/v1/info", get(info))
        .route("/oft/v1/config", get(read_config).put(write_config))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (fake, addr)
}

/// Wait until `check` gives a value, or fail after five seconds.
async fn eventually<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn events(registry: &EventRegistry, event: &EventId) -> Vec<Event> {
    registry
        .log(event)
        .unwrap()
        .lock()
        .unwrap()
        .read_all()
        .unwrap()
        .into_iter()
        .map(|stored| stored.event)
        .collect()
}

/// One active event holding a one-seat open heat on the timer's seat 0.
fn event_with_heat(registry: &EventRegistry, timer: &AdapterId) -> EventId {
    let event = registry
        .create(&CreateEventRequest::named("Bench"))
        .unwrap()
        .id;
    registry.set_active(&event).unwrap();
    let heat = PhaseDef {
        id: PhaseId("heat".into()),
        label: "Heat".into(),
        parent: None,
        kind: PhaseKind::Heat {
            lineup: vec![HeatSeat {
                adapter: timer.clone(),
                seat: SeatRef("0".into()),
                source: SeatSource::Pilot(PilotId("ana".into())),
            }],
        },
        format: Some(Format {
            name: "Open".into(),
            race: RaceRules {
                end: EndCondition::Open,
                staging_micros: 0,
                min_lap_micros: None,
                first_lap: FirstLap::HoleShot,
                stop_after_all_finish_micros: None,
                max_micros: None,
                drop_idle_seat_micros: None,
            },
            completion: CompletionRule::DirectorDecides,
            ranking: None,
            points: Vec::new(),
        }),
        false_start_limit: None,
        display_hint: None,
        class: None,
    };
    with_event(registry, &event, |session| {
        session.commands(vec![Command::Record(Box::new(Event::PhaseDefined {
            def: heat,
        }))])
    })
    .unwrap();
    event
}

#[tokio::test]
async fn a_timer_in_the_directory_is_connected_and_its_crossings_become_laps() {
    let (fake, addr) = fake_timer().await;
    let registry = EventRegistry::new(None).unwrap();
    let timer_id = registry
        .timers()
        .create(&CreateTimerRequest {
            name: "Gate".into(),
            kind: TimerKind::Oft,
            address: addr.to_string(),
            own_id: None,
        })
        .unwrap()
        .id;
    let event = event_with_heat(&registry, &timer_id);

    let links = gridfpv_app::oft::spawn(registry.clone());

    // It connects, reports its seats, and its clock gets measured.
    eventually("the timer to connect", || {
        let view = registry.timers().list().remove(0);
        (view.status.connected && view.status.clock_error_micros.is_some()).then_some(())
    })
    .await;
    assert_eq!(
        registry.timers().list()[0].status.seats,
        vec![SeatRef("0".into()), SeatRef("1".into())]
    );
    eventually("the clock offset to be logged", || {
        events(&registry, &event)
            .iter()
            .any(|e| matches!(e, Event::ClockOffset { adapter, .. } if adapter == &timer_id))
            .then_some(())
    })
    .await;

    // A crossing before any race belongs to no race.
    fake.crossings.lock().unwrap().push((1, 0, 1_000_000));
    eventually("the first crossing to be logged", || {
        events(&registry, &event)
            .iter()
            .any(|e| matches!(e, Event::Pass(pass) if pass.sequence == Some(1)))
            .then_some(())
    })
    .await;

    // The timer has been sending signal all along. With no race on, none of it is logged.
    let chunks = |registry: &EventRegistry| -> Vec<gridfpv_events::SignalChunk> {
        events(registry, &event)
            .into_iter()
            .filter_map(|e| match e {
                Event::SignalChunk(chunk) => Some(chunk),
                _ => None,
            })
            .collect()
    };
    assert!(chunks(&registry).is_empty());

    // Start a race; crossings 2 and 3 arrive during it, one lap of ten seconds.
    with_event(&registry, &event, |session| {
        session.commands(vec![Command::StartRace {
            heat: PhaseId("heat".into()),
        }])
    })
    .unwrap();
    fake.crossings.lock().unwrap().push((2, 0, 20_000_000));
    eventually("crossing 2 to be logged", || {
        events(&registry, &event)
            .iter()
            .any(|e| matches!(e, Event::Pass(pass) if pass.sequence == Some(2)))
            .then_some(())
    })
    .await;
    // The link drops mid-race. Crossing 3 happens while it is down; the reconnect asks for
    // everything after 2 and gets it.
    *fake.hang_up.lock().unwrap() = true;
    eventually("the link to drop", || {
        (!registry.timers().list()[0].status.connected).then_some(())
    })
    .await;
    fake.crossings.lock().unwrap().push((3, 0, 30_000_000));
    eventually("the race crossings to be logged", || {
        events(&registry, &event)
            .iter()
            .any(|e| matches!(e, Event::Pass(pass) if pass.sequence == Some(3)))
            .then_some(())
    })
    .await;
    let race = RaceId("heat#1".into());
    with_event(&registry, &event, |session| {
        let state = session.state();
        let found = state.races.get(&race).unwrap();
        let laps = state.laps.seat(
            &race,
            &timer_id,
            &SeatRef("0".into()),
            state.filters_for(found, &timer_id),
        );
        assert_eq!(laps.lap_count(), 1);
        assert_eq!(laps.best_lap_micros(), Some(10_000_000));
        Ok(())
    })
    .unwrap();

    // The race's signal is in the log, exactly as sent.
    let logged = eventually("the race's signal to be logged", || {
        let logged = chunks(&registry);
        (!logged.is_empty()).then_some(logged)
    })
    .await;
    assert_eq!(logged[0].period_micros, 1000);
    assert_eq!(logged[0].unit, gridfpv_events::SignalUnit::TenthsDb);
    assert_eq!(logged[0].noise_floor, Some(-910));
    assert_eq!(&logged[0].levels[..5], [-900, -900, -880, -700, -400]);
    // (The first chunk is short here: the link dropped mid-race, and a drop logs what a
    // race had so far rather than losing it.)
    assert_eq!(logged[0].levels.len() % 10, 0);

    // Every crossing was logged exactly once, though the link dropped and replayed.
    let sequences: Vec<u64> = events(&registry, &event)
        .iter()
        .filter_map(|e| match e {
            Event::Pass(pass) => pass.sequence,
            _ => None,
        })
        .collect();
    assert_eq!(sequences, vec![1, 2, 3]);
    // And each reconnect asked only for what it had not seen.
    let subscriptions = fake.subscriptions.lock().unwrap().clone();
    assert_eq!(subscriptions, vec![None, Some(2)]);
    // The drop itself is in the log.
    assert!(
        events(&registry, &event)
            .iter()
            .any(|e| matches!(e, Event::AdapterDisconnected { .. }))
    );

    links.abort();
}

#[tokio::test]
async fn a_timer_that_cannot_be_reached_says_why_and_is_dropped_when_removed() {
    let registry = EventRegistry::new(None).unwrap();
    // A port nothing listens on.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let timer = registry
        .timers()
        .create(&CreateTimerRequest {
            name: "Gate".into(),
            kind: TimerKind::Oft,
            address: addr.to_string(),
            own_id: None,
        })
        .unwrap();
    let links = gridfpv_app::oft::spawn(registry.clone());
    let problem = eventually("the problem to be reported", || {
        registry.timers().list().remove(0).status.problem
    })
    .await;
    assert!(problem.contains("Could not reach the timer"), "{problem}");
    assert!(!registry.timers().list()[0].status.connected);

    registry.timers().delete(&timer.id).unwrap();
    assert!(registry.timers().list().is_empty());
    links.abort();
}

/// Spec v0.8: a receiver says what its levels measure. One built on an RX5808 reports
/// millivolts, and it can report in after the timer's `hello` has gone out. Grid must not
/// log its levels as tenths of a dB.
#[tokio::test]
async fn a_receiver_that_reports_millivolts_after_the_hello_is_logged_in_millivolts() {
    use gridfpv_events::SignalUnit;

    let (fake, addr) = fake_timer().await;
    let registry = EventRegistry::new(None).unwrap();
    let timer_id = registry
        .timers()
        .create(&CreateTimerRequest {
            name: "Gate".into(),
            kind: TimerKind::Oft,
            address: addr.to_string(),
            own_id: None,
        })
        .unwrap()
        .id;
    let event = event_with_heat(&registry, &timer_id);
    let _links = gridfpv_app::oft::spawn(registry.clone());

    // No receiver listed yet: tenths of a dB, as the spec says.
    eventually("the timer to connect", || {
        registry.timers().list()[0].status.connected.then_some(())
    })
    .await;
    assert_eq!(
        registry.timers().list()[0].status.level_unit,
        Some(SignalUnit::TenthsDb)
    );

    // The receiver reports in. The timer's status names it, and `/info` now lists it.
    *fake.nodes.lock().unwrap() = serde_json::json!([{
        "id": "0011223344ff", "chip": "stm32f1-rx5808", "fw": "0.2.0",
        "range_mhz": [5645, 5945], "max_seats": 8,
        "level": {"unit": "mV", "scale": 1}, "link": "uart",
    }]);
    *fake.status_node.lock().unwrap() = Some("0011223344ff".into());
    eventually("the timer's unit to become millivolts", || {
        (registry.timers().list()[0].status.level_unit == Some(SignalUnit::Millivolts))
            .then_some(())
    })
    .await;

    // A race: its signal, its levels and its crossing's peak are all in millivolts.
    with_event(&registry, &event, |session| {
        session.commands(vec![Command::StartRace {
            heat: PhaseId("heat".into()),
        }])
    })
    .unwrap();
    fake.crossings.lock().unwrap().push((1, 0, 20_000_000));
    let pass = eventually("the crossing to be logged", || {
        events(&registry, &event).into_iter().find_map(|e| match e {
            Event::Pass(pass) => Some(pass),
            _ => None,
        })
    })
    .await;
    // The fake's peak is -235: as millivolts it is not divided by ten.
    assert_eq!(pass.signal.unwrap().rssi_peak, Some(-235.0));
    let thresholds = eventually("the race's levels to be logged", || {
        events(&registry, &event).into_iter().find_map(|e| match e {
            Event::SignalThresholds(thresholds) => Some(thresholds),
            _ => None,
        })
    })
    .await;
    assert_eq!(
        (thresholds.enter, thresholds.exit, thresholds.unit),
        (600, 550, SignalUnit::Millivolts)
    );
    with_event(&registry, &event, |session| {
        session.commands(vec![Command::StopRace {
            race: RaceId("heat#1".into()),
        }])
    })
    .ok();
    // (Signal from the second before the receiver's unit was read is the race's lead-in,
    // and keeps the unit it arrived under; everything since is in millivolts.)
    let chunk = eventually("the race's signal to be logged", || {
        events(&registry, &event)
            .into_iter()
            .rev()
            .find_map(|e| match e {
                Event::SignalChunk(chunk) => Some(chunk),
                _ => None,
            })
            .filter(|chunk| chunk.unit == SignalUnit::Millivolts)
    })
    .await;
    assert_eq!(&chunk.levels[..3], [-900, -900, -880]);
}

// ── The timer's own page, and what it says is wrong (OFT spec v0.12, §6.3) ─────

/// A fake timer in a directory, with the link to it running.
async fn linked_timer(before: impl FnOnce(&Fake)) -> (Fake, EventRegistry, SocketAddr) {
    let (fake, addr) = fake_timer().await;
    before(&fake);
    let registry = EventRegistry::new(None).unwrap();
    registry
        .timers()
        .create(&CreateTimerRequest {
            name: "Gate".into(),
            kind: TimerKind::Oft,
            address: addr.to_string(),
            own_id: None,
        })
        .unwrap();
    (fake, registry, addr)
}

fn now(registry: &EventRegistry) -> gridfpv_server::timers::TimerStatus {
    registry.timers().list().remove(0).status
}

#[tokio::test]
async fn the_timers_own_page_is_read_as_soon_as_it_connects() {
    let (fake, registry, addr) =
        linked_timer(|fake| *fake.manage_url.lock().unwrap() = Some("/".into())).await;
    let _links = gridfpv_app::oft::spawn(registry.clone());
    // Well inside the fifteen seconds between the later reads of `/info`.
    let page = tokio::time::timeout(
        Duration::from_secs(5),
        eventually("the timer's page to be known", || now(&registry).manage_url),
    )
    .await
    .expect("the page is read at once, not at the next slow turn");
    assert_eq!(page, format!("http://{addr}/"));
    // The link dropping does not lose it: a timer in trouble is when the page is wanted.
    *fake.hang_up.lock().unwrap() = true;
    eventually("the link to drop and come back", || {
        (fake.subscriptions.lock().unwrap().len() >= 2 && now(&registry).connected).then_some(())
    })
    .await;
    assert_eq!(now(&registry).manage_url, Some(format!("http://{addr}/")));
}

#[tokio::test]
async fn a_timer_with_no_page_of_its_own_has_no_link() {
    let (fake, registry, _) = linked_timer(|_| {}).await;
    let _links = gridfpv_app::oft::spawn(registry.clone());
    eventually("the timer to connect", || {
        now(&registry).connected.then_some(())
    })
    .await;
    // Give the first read of `/info` time to land: it must leave nothing.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(now(&registry).manage_url, None);
    assert!(now(&registry).timer_problems.is_empty());
    drop(fake);
}

#[tokio::test]
async fn what_the_timer_says_is_wrong_is_on_its_status_and_goes_when_it_does() {
    let (fake, registry, _) = linked_timer(|fake| {
        *fake.problems.lock().unwrap() = Some(vec!["node_missing".into(), "low_voltage".into()]);
    })
    .await;
    let _links = gridfpv_app::oft::spawn(registry.clone());
    // Names Grid knows and names it does not, in the timer's order.
    eventually("the timer's problems to show", || {
        (now(&registry).timer_problems == ["node_missing", "low_voltage"]).then_some(())
    })
    .await;
    assert!(now(&registry).connected);

    // The timer's list changes: the status follows.
    *fake.problems.lock().unwrap() = Some(vec!["crashed".into()]);
    eventually("the changed problems to show", || {
        (now(&registry).timer_problems == ["crashed"]).then_some(())
    })
    .await;

    // The receiver is back: an empty list clears it.
    *fake.problems.lock().unwrap() = Some(Vec::new());
    eventually("the problems to clear", || {
        now(&registry).timer_problems.is_empty().then_some(())
    })
    .await;
}

#[tokio::test]
async fn problems_are_not_kept_across_a_dropped_link() {
    let (fake, registry, _) = linked_timer(|fake| {
        *fake.problems.lock().unwrap() = Some(vec!["safe_mode".into()]);
    })
    .await;
    let links = gridfpv_app::oft::spawn(registry.clone());
    eventually("the timer's problems to show", || {
        (now(&registry).timer_problems == ["safe_mode"]).then_some(())
    })
    .await;
    // The timer comes back saying nothing is wrong (and sends no `status` to say so: an
    // older firmware). What it said before the drop must not stand.
    *fake.problems.lock().unwrap() = None;
    *fake.hang_up.lock().unwrap() = true;
    eventually("the link to drop and come back", || {
        (fake.subscriptions.lock().unwrap().len() >= 2 && now(&registry).connected).then_some(())
    })
    .await;
    assert!(now(&registry).timer_problems.is_empty());
    links.abort();
}

// ── Control: one writer at a time (OFT spec v0.9, §6.6) ────────────────────────

use gridfpv_server::timers::{TimerConfig, TimerControl, TimerStatus, apply_config};

/// A fake timer that has control, in a directory, with the link to it running.
async fn controlled_timer(
    before: impl FnOnce(&mut FakeControl),
) -> (Fake, EventRegistry, AdapterId, tokio::task::JoinHandle<()>) {
    let (fake, addr) = fake_timer().await;
    {
        let mut control = fake.control.lock().unwrap();
        control.enabled = true;
        before(&mut control);
    }
    let registry = EventRegistry::new(None).unwrap();
    let timer = registry
        .timers()
        .create(&CreateTimerRequest {
            name: "Gate".into(),
            kind: TimerKind::Oft,
            address: addr.to_string(),
            own_id: None,
        })
        .unwrap()
        .id;
    let links = gridfpv_app::oft::spawn(registry.clone());
    (fake, registry, timer, links)
}

fn status(registry: &EventRegistry) -> TimerStatus {
    registry.timers().list().remove(0).status
}

/// Wait until the timer's status says `control`.
async fn control_is(registry: &EventRegistry, control: TimerControl) {
    let what = format!("control to be {control:?}");
    eventually(&what, || {
        let now = status(registry);
        (now.connected && now.control.as_ref() == Some(&control)).then_some(())
    })
    .await;
}

fn other(name: &str) -> TimerControl {
    TimerControl::Other { name: name.into() }
}

/// Settings to write: seat 0 moved to 5917 MHz.
fn moved() -> TimerConfig {
    serde_json::from_value(serde_json::json!({
        "seats": [
            {"seat": "0", "freq_mhz": 5917, "enabled": true},
            {"seat": "1", "freq_mhz": 5658, "enabled": true},
        ],
        "threshold": {"Auto": {"snr_enter": 150, "hysteresis": 50}},
        "lap_cooldown_ms": 2000,
        "exit_hold_ms": 30,
    }))
    .unwrap()
}

fn seat_0_mhz(fake: &Fake) -> u64 {
    fake.control.lock().unwrap().config["seats"][0]["freq_mhz"]
        .as_u64()
        .unwrap()
}

/// Watch the timer's status while `during` runs, and give every state of control seen.
async fn control_seen_during(
    registry: &EventRegistry,
    during: impl std::future::Future<Output = ()>,
) -> Vec<Option<TimerControl>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let watcher = {
        let (registry, seen) = (registry.clone(), seen.clone());
        tokio::spawn(async move {
            loop {
                let now = status(&registry).control;
                {
                    let mut seen = seen.lock().unwrap();
                    if seen.last() != Some(&now) {
                        seen.push(now);
                    }
                }
                tokio::time::sleep(Duration::from_micros(200)).await;
            }
        })
    };
    during.await;
    watcher.abort();
    seen.lock().unwrap().clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grid_claims_control_when_it_connects_and_its_writes_carry_the_key() {
    let (fake, registry, timer, _links) = controlled_timer(|_| {}).await;
    control_is(&registry, TimerControl::Grid).await;

    // It claimed once, under a name that starts "GridFPV" and that a timer would keep
    // whole, with no key: it had none yet.
    let received = fake.control.lock().unwrap().received.clone();
    assert_eq!(received.len(), 1, "{received:?}");
    let name = received[0]["claim"].as_str().unwrap().to_owned();
    assert!(name.starts_with("GridFPV"), "{name}");
    assert!(
        name.len() <= 32 && name.chars().all(|c| (' '..='~').contains(&c)),
        "{name}"
    );
    assert!(received[0].get("key").is_none(), "{received:?}");
    // The status tells a console the name to look for on the timer's own page.
    assert_eq!(
        status(&registry).control_name.as_deref(),
        Some(name.as_str())
    );
    // The key is Grid's to send, never a console's to see.
    let key = fake.control.lock().unwrap().holder().unwrap().key.clone();
    assert_eq!(registry.timers().control_key(&timer), Some(key.clone()));
    let listed = serde_json::to_string(&registry.timers().list()).unwrap();
    assert!(!listed.contains(&key), "{listed}");

    // A write lands, and carried the key.
    let written = apply_config(&registry, &timer, &moved()).await.unwrap();
    assert_eq!(written.seats[0].freq_mhz, 5917);
    assert_eq!(seat_0_mhz(&fake), 5917);
    assert_eq!(
        fake.control
            .lock()
            .unwrap()
            .write_keys
            .last()
            .unwrap()
            .as_deref(),
        Some(key.as_str())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_timer_with_no_control_is_not_claimed_and_is_written_to_as_before() {
    let (fake, addr) = fake_timer().await;
    let registry = EventRegistry::new(None).unwrap();
    let timer = registry
        .timers()
        .create(&CreateTimerRequest {
            name: "Gate".into(),
            kind: TimerKind::Oft,
            address: addr.to_string(),
            own_id: None,
        })
        .unwrap()
        .id;
    let _links = gridfpv_app::oft::spawn(registry.clone());
    eventually("the timer to connect", || {
        let now = status(&registry);
        (now.connected && now.clock_error_micros.is_some()).then_some(())
    })
    .await;
    let now = status(&registry);
    assert_eq!(now.control, None);
    assert_eq!(now.control_name, None);
    assert_eq!(now.config_rev, None);
    assert_eq!(registry.timers().control_key(&timer), None);
    apply_config(&registry, &timer, &moved()).await.unwrap();
    assert_eq!(seat_0_mhz(&fake), 5917);
    let control = fake.control.lock().unwrap();
    assert!(control.received.is_empty(), "{:?}", control.received);
    assert_eq!(control.write_keys, vec![None]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_program_in_control_is_named_and_grid_waits_its_turn() {
    // RotorHazard connected first.
    let mut theirs = 0;
    let (fake, registry, timer, _links) = controlled_timer(|control| {
        theirs = control.other_claims("RotorHazard pi-timer");
        // It set a pass window of its own. No event of Grid's asks for one.
        control.config["pass_window_ms"] = 250.into();
    })
    .await;
    control_is(&registry, other("RotorHazard pi-timer")).await;
    // Grid still claimed: it stands in line.
    eventually("Grid's claim to arrive", || {
        (fake.control.lock().unwrap().claims.len() == 2).then_some(())
    })
    .await;
    assert_eq!(registry.timers().control_key(&timer), None);

    // The window is not the event's and Grid cannot put it right: the status says so.
    let problem = eventually("the pass window problem to be reported", || {
        status(&registry).pass_window_problem
    })
    .await;
    assert!(problem.contains("RotorHazard pi-timer"), "{problem}");

    // A change is refused in words that name the program, and never reaches the timer.
    let refused = apply_config(&registry, &timer, &moved()).await.unwrap_err();
    assert!(
        refused.message.contains("RotorHazard pi-timer"),
        "{}",
        refused.message
    );
    assert!(refused.message.contains("Gate"), "{}", refused.message);
    assert_eq!(seat_0_mhz(&fake), 5800);
    assert!(fake.control.lock().unwrap().write_keys.is_empty());

    // Laps still come in: control is about changing the timer, not listening to it.
    let event = event_with_heat(&registry, &timer);
    fake.crossings.lock().unwrap().push((1, 0, 1_000_000));
    eventually("a crossing to be logged", || {
        events(&registry, &event)
            .iter()
            .any(|e| matches!(e, Event::Pass(pass) if pass.sequence == Some(1)))
            .then_some(())
    })
    .await;

    // RotorHazard goes away. Grid's claim has stood longest, so control comes to it.
    fake.control.lock().unwrap().end(theirs);
    control_is(&registry, TimerControl::Grid).await;
    let key = fake.control.lock().unwrap().holder().unwrap().key.clone();
    assert_eq!(registry.timers().control_key(&timer), Some(key));
    // Grid sends its settings again: the pass window goes back to the event's (none).
    eventually("the pass window to be put back", || {
        (fake.control.lock().unwrap().config["pass_window_ms"] == 0).then_some(())
    })
    .await;
    eventually("the problem to clear", || {
        let now = status(&registry);
        (now.pass_window_problem.is_none() && now.pass_window_ms == Some(0)).then_some(())
    })
    .await;
    // And the change that was refused now lands.
    apply_config(&registry, &timer, &moved()).await.unwrap();
    assert_eq!(seat_0_mhz(&fake), 5917);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_timers_page_can_move_control_away_from_grid_and_back_and_free_it() {
    let (fake, registry, timer, _links) = controlled_timer(|_| {}).await;
    control_is(&registry, TimerControl::Grid).await;
    let grid = fake.control.lock().unwrap().holder().unwrap().id;

    // A person takes control at the timer's own page.
    let page = {
        let mut control = fake.control.lock().unwrap();
        let page = control.other_claims("Timer page");
        control.grant(Some(page));
        page
    };
    control_is(&registry, other("Timer page")).await;
    // The key Grid had is dead, and is not sent again.
    eventually("the key to be dropped", || {
        registry
            .timers()
            .control_key(&timer)
            .is_none()
            .then_some(())
    })
    .await;
    let refused = apply_config(&registry, &timer, &moved()).await.unwrap_err();
    assert!(
        refused.message.contains("Timer page"),
        "{}",
        refused.message
    );
    assert!(fake.control.lock().unwrap().write_keys.is_empty());

    // The page frees control: nobody has it, so anyone may write, Grid included.
    fake.control.lock().unwrap().grant(None);
    control_is(&registry, TimerControl::Nobody).await;
    apply_config(&registry, &timer, &moved()).await.unwrap();
    assert_eq!(seat_0_mhz(&fake), 5917);

    // The page hands control back to Grid's claim, which stood the whole time.
    fake.control.lock().unwrap().grant(Some(grid));
    control_is(&registry, TimerControl::Grid).await;
    let key = fake.control.lock().unwrap().holder().unwrap().key.clone();
    eventually("Grid's key to be back", || {
        (registry.timers().control_key(&timer).as_ref() == Some(&key)).then_some(())
    })
    .await;
    let mut back = moved();
    back.seats[0].freq_mhz = 5732;
    apply_config(&registry, &timer, &back).await.unwrap();
    assert_eq!(seat_0_mhz(&fake), 5732);
    assert_eq!(
        fake.control
            .lock()
            .unwrap()
            .write_keys
            .last()
            .unwrap()
            .as_deref(),
        Some(key.as_str())
    );
    // The page's claim is still standing and waiting; Grid never asked twice.
    assert!(
        fake.control
            .lock()
            .unwrap()
            .claims
            .iter()
            .any(|c| c.id == page)
    );
    assert_eq!(fake.control.lock().unwrap().received.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_link_gets_its_place_back_with_the_key() {
    let (fake, registry, timer, _links) = controlled_timer(|_| {}).await;
    control_is(&registry, TimerControl::Grid).await;
    let (grid, key) = {
        let control = fake.control.lock().unwrap();
        let holder = control.holder().unwrap();
        (holder.id, holder.key.clone())
    };
    // Another program is waiting in line, so a lost place would go to it.
    fake.control
        .lock()
        .unwrap()
        .other_claims("RotorHazard pi-timer");

    let seen = control_seen_during(&registry, async {
        *fake.hang_up.lock().unwrap() = true;
        eventually("the link to drop", || {
            (!status(&registry).connected).then_some(())
        })
        .await;
        // While the link is down the timer still holds Grid's place, and still takes a
        // write that carries the key.
        assert_eq!(registry.timers().control_key(&timer), Some(key.clone()));
        apply_config(&registry, &timer, &moved()).await.unwrap();
        assert_eq!(seat_0_mhz(&fake), 5917);
        control_is(&registry, TimerControl::Grid).await;
    })
    .await;

    // Grid claimed again with its key, and has the same claim: same id, same key.
    let control = fake.control.lock().unwrap();
    assert_eq!(control.received.len(), 2, "{:?}", control.received);
    assert_eq!(control.received[1]["key"], key.as_str());
    let holder = control.holder().unwrap();
    assert_eq!((holder.id, holder.key.as_str()), (grid, key.as_str()));
    assert!(holder.connection.is_some());
    drop(control);
    assert_eq!(registry.timers().control_key(&timer), Some(key));
    // The holder the timer named straight after its hello was Grid's own claim. Grid
    // never called that "another program", not even for a moment.
    assert!(
        !seen
            .iter()
            .any(|c| matches!(c, Some(TimerControl::Other { .. }))),
        "{seen:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_link_down_too_long_loses_its_place_and_says_who_has_it() {
    let (fake, registry, timer, _links) = controlled_timer(|_| {}).await;
    control_is(&registry, TimerControl::Grid).await;
    let key = fake.control.lock().unwrap().holder().unwrap().key.clone();

    // The link drops, the ten seconds run out, and RotorHazard is next in line.
    {
        let mut control = fake.control.lock().unwrap();
        control.other_claims("RotorHazard pi-timer");
        *fake.hang_up.lock().unwrap() = true;
    }
    eventually("the link to drop", || {
        (!status(&registry).connected).then_some(())
    })
    .await;
    fake.control.lock().unwrap().expire();

    control_is(&registry, other("RotorHazard pi-timer")).await;
    // Grid tried its old key; it matched nothing, so that was an ordinary new claim.
    {
        let control = fake.control.lock().unwrap();
        assert_eq!(control.received.last().unwrap()["key"], key.as_str());
        assert_eq!(control.holder().unwrap().name, "RotorHazard pi-timer");
        assert!(control.claims.iter().any(|c| c.name.starts_with("GridFPV")));
    }
    // The dead key is dropped, and nothing is written with it.
    eventually("the dead key to be dropped", || {
        registry
            .timers()
            .control_key(&timer)
            .is_none()
            .then_some(())
    })
    .await;
    let refused = apply_config(&registry, &timer, &moved()).await.unwrap_err();
    assert!(
        refused.message.contains("RotorHazard pi-timer"),
        "{}",
        refused.message
    );
    assert!(fake.control.lock().unwrap().write_keys.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_timer_with_no_room_for_grids_claim_still_says_who_has_control() {
    // First contact: another program holds control and the timer is full.
    let (fake, registry, timer, _links) = controlled_timer(|control| {
        control.other_claims("RotorHazard pi-timer");
        control.full = true;
    })
    .await;
    control_is(&registry, other("RotorHazard pi-timer")).await;
    eventually("Grid's claim to be refused", || {
        (!fake.control.lock().unwrap().received.is_empty()).then_some(())
    })
    .await;
    assert_eq!(fake.control.lock().unwrap().claims.len(), 1);
    let refused = apply_config(&registry, &timer, &moved()).await.unwrap_err();
    assert!(
        refused.message.contains("RotorHazard pi-timer"),
        "{}",
        refused.message
    );

    // Nobody in control and still no room: Grid has no claim, and writes all the same.
    {
        let mut control = fake.control.lock().unwrap();
        let theirs = control.holder().unwrap().id;
        control.end(theirs);
    }
    control_is(&registry, TimerControl::Nobody).await;
    apply_config(&registry, &timer, &moved()).await.unwrap();
    assert_eq!(seat_0_mhz(&fake), 5917);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_timer_after_a_lost_place_is_not_left_saying_nothing() {
    let (fake, registry, timer, _links) = controlled_timer(|_| {}).await;
    control_is(&registry, TimerControl::Grid).await;
    // Grid's place is lost while the link is down, and the timer has no room when Grid
    // comes back with its old key: the timer answers with an error, not a `control`.
    {
        let mut control = fake.control.lock().unwrap();
        control.other_claims("RotorHazard pi-timer");
        *fake.hang_up.lock().unwrap() = true;
    }
    eventually("the link to drop", || {
        (!status(&registry).connected).then_some(())
    })
    .await;
    {
        let mut control = fake.control.lock().unwrap();
        control.expire();
        control.full = true;
    }
    control_is(&registry, other("RotorHazard pi-timer")).await;
    assert_eq!(registry.timers().control_key(&timer), None);
    assert_eq!(fake.control.lock().unwrap().claims.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restarted_timer_is_claimed_afresh_with_no_key() {
    let (fake, registry, timer, _links) = controlled_timer(|_| {}).await;
    control_is(&registry, TimerControl::Grid).await;
    let old_key = fake.control.lock().unwrap().holder().unwrap().key.clone();
    apply_config(&registry, &timer, &moved()).await.unwrap();
    eventually("config_rev to be seen", || {
        status(&registry).config_rev.filter(|rev| *rev >= 1)
    })
    .await;

    // The timer restarts: a new boot, no claims, its change count back at nothing.
    {
        let mut control = fake.control.lock().unwrap();
        control.restart();
        control.received.clear();
        *fake.boot.lock().unwrap() = "b002".into();
        *fake.hang_up.lock().unwrap() = true;
    }
    eventually("Grid to claim on the new boot", || {
        (!fake.control.lock().unwrap().received.is_empty()).then_some(())
    })
    .await;
    // A key from the old boot means nothing to the new one, and is not offered.
    let received = fake.control.lock().unwrap().received.clone();
    assert!(received[0].get("key").is_none(), "{received:?}");
    control_is(&registry, TimerControl::Grid).await;
    let key = fake.control.lock().unwrap().holder().unwrap().key.clone();
    assert_ne!(key, old_key);
    eventually("the new key to be in use", || {
        (registry.timers().control_key(&timer).as_ref() == Some(&key)).then_some(())
    })
    .await;
    eventually("config_rev to start again", || {
        (status(&registry).config_rev == Some(0)).then_some(())
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_changed_by_someone_else_are_noticed_through_config_rev() {
    use gridfpv_server::timers::seat_channels;

    // Nobody has control on this timer's page... a person frees it, so two programs may
    // write. Grid is one of them.
    let (fake, registry, timer, _links) = controlled_timer(|_| {}).await;
    control_is(&registry, TimerControl::Grid).await;
    fake.control.lock().unwrap().grant(None);
    control_is(&registry, TimerControl::Nobody).await;
    let rev = eventually("config_rev to be seen", || status(&registry).config_rev).await;

    // Grid has read the channels, and would not ask again for fifteen seconds.
    let channels = seat_channels(&registry, &timer).await;
    assert_eq!(channels[0].freq_mhz, 5800);

    // Someone else moves seat 0 and sets a pass window, straight on the timer.
    {
        let mut control = fake.control.lock().unwrap();
        control.config["seats"][0]["freq_mhz"] = 5917.into();
        control.config["pass_window_ms"] = 300.into();
        control.config_rev += 1;
    }
    // The status carries the new number, for a console to read the settings again.
    eventually("the new config_rev", || {
        status(&registry).config_rev.filter(|now| *now > rev)
    })
    .await;
    // The channels are read again by the next asker, without the fifteen-second wait.
    eventually("the new channel", || {
        futures_now(seat_channels(&registry, &timer))
            .filter(|channels| channels[0].freq_mhz == 5917)
    })
    .await;
    // And the event's pass window (none) is put back: that setting is the event's.
    eventually("the pass window to be put back", || {
        (fake.control.lock().unwrap().config["pass_window_ms"] == 0).then_some(())
    })
    .await;
    // Only the window was touched: the other program's channel stays.
    assert_eq!(seat_0_mhz(&fake), 5917);
}

/// Run a future to its answer from inside [`eventually`]'s plain closure.
fn futures_now<T>(future: impl std::future::Future<Output = T>) -> Option<T> {
    Some(tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(future)
    }))
}
