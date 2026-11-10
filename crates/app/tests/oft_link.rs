//! The OFT connection against a fake timer.
//!
//! The fake is a small WebSocket server that speaks the part of the OFT protocol Grid uses:
//! `hello` on connect, a replay of crossings after the `since` the client asks for, and a
//! `pong` for every `ping`. It runs in this process, so the test needs no hardware and no
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
}

async fn stream(State(fake): State<Fake>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| serve(fake, socket))
}

async fn serve(fake: Fake, mut socket: WebSocket) {
    let hello = serde_json::json!({
        "type": "hello", "proto": 1, "timer": "oft-fake", "boot_id": "b001",
        "time_us": fake.booted.elapsed().as_micros() as u64,
        "seats": [{"seat": 0, "node": 0, "rate_hz": 500}, {"seat": 1, "node": 0, "rate_hz": 500}],
    });
    if socket.send(Message::text(hello.to_string())).await.is_err() {
        return;
    }
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
                if !subscribed {
                    continue;
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
    };
    let app = Router::new()
        .route("/oft/v1/stream", get(stream))
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
            },
            completion: CompletionRule::DirectorDecides,
            ranking: None,
            points: Vec::new(),
        }),
        false_start_limit: None,
        display_hint: None,
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
