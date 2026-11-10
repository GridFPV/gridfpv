//! A whole Grid race against a **live RotorHazard**. Local only: `#[ignore]`d, so CI never
//! runs it (CI has no RotorHazard). Run it against a RotorHazard you can afford to disturb:
//!
//! ```text
//! docker run -d --rm --name grid-rh-live -p 5091:5000 gridfpv-rotorhazard:4.4.0
//! GRIDFPV_RH_ADDR=127.0.0.1:5091 cargo test -p gridfpv-app --test rh_live -- --ignored
//! ```
//!
//! It uses RotorHazard's own "simulate a lap" message to make crossings, so it needs no
//! receivers. Nothing is installed on the timer.

use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use gridfpv_adapters::rotorhazard::wire::{self, Frame};
use gridfpv_events::{
    AdapterId, Command, CompletionRule, EndCondition, Event, FirstLap, Format, HeatSeat, PhaseDef,
    PhaseId, PhaseKind, PilotId, RaceId, RaceRules, SeatRef, SeatSource,
};
use gridfpv_server::control::with_event;
use gridfpv_server::events::{CreateEventRequest, EventId, EventRegistry};
use gridfpv_server::timers::{CreateTimerRequest, TimerKind};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

async fn eventually<T>(what: &str, secs: u64, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A second, plain connection to RotorHazard, standing in for a person at its own page:
/// it makes laps, and reads RotorHazard's race status.
struct Side {
    socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

impl Side {
    async fn open(addr: &str) -> Self {
        let url = format!("ws://{addr}{}", wire::SOCKET_PATH);
        let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        // Open, then connect.
        socket.next().await;
        socket.send(Message::text(wire::connect())).await.unwrap();
        Self { socket }
    }

    async fn send(&mut self, name: &str, arg: Option<serde_json::Value>) {
        self.socket
            .send(Message::text(wire::event(name, arg.as_ref())))
            .await
            .unwrap();
    }

    /// Ask for RotorHazard's race status and wait for the answer.
    async fn race_status(&mut self) -> i64 {
        self.send("load_data", Some(json!({"load_types": ["race_status"]})))
            .await;
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(5), self.socket.next())
                .await
                .expect("RotorHazard went quiet")
                .unwrap()
                .unwrap();
            let Message::Text(text) = frame else { continue };
            match wire::parse(&text) {
                Frame::Ping => {
                    self.socket.send(Message::text(wire::pong())).await.unwrap();
                }
                Frame::Event { name, args } if name == "race_status" => {
                    return args[0]["race_status"].as_i64().unwrap();
                }
                _ => {}
            }
        }
    }
}

fn passes(registry: &EventRegistry, event: &EventId) -> Vec<gridfpv_events::Pass> {
    registry
        .log(event)
        .unwrap()
        .lock()
        .unwrap()
        .read_all()
        .unwrap()
        .into_iter()
        .filter_map(|stored| match stored.event {
            Event::Pass(pass) => Some(pass),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live RotorHazard: set GRIDFPV_RH_ADDR"]
async fn a_grid_race_is_timed_by_a_stock_rotorhazard() {
    let addr = std::env::var("GRIDFPV_RH_ADDR").expect("set GRIDFPV_RH_ADDR to host:port");
    let mut side = Side::open(&addr).await;
    // Start from a clean RotorHazard.
    side.send("stop_race", None).await;
    for _ in 0..50 {
        side.send("discard_laps", None).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        if side.race_status().await == 0 {
            break;
        }
    }

    let registry = EventRegistry::new(None).unwrap();
    let timer = registry
        .timers()
        .create(&CreateTimerRequest {
            name: "Gate".into(),
            kind: TimerKind::RotorHazard,
            address: addr.clone(),
        })
        .unwrap()
        .id;
    let event = registry
        .create(&CreateEventRequest::named("Live"))
        .unwrap()
        .id;
    registry.set_active(&event).unwrap();
    let seat = |node: &str, who: &str| HeatSeat {
        adapter: timer.clone(),
        seat: SeatRef(node.into()),
        source: SeatSource::Pilot(PilotId(who.into())),
    };
    let heat = PhaseDef {
        id: PhaseId("heat".into()),
        label: "Heat".into(),
        parent: None,
        kind: PhaseKind::Heat {
            lineup: vec![seat("0", "ana"), seat("1", "ben")],
        },
        format: Some(Format {
            name: "Open".into(),
            race: RaceRules {
                end: EndCondition::Open,
                // A two second countdown: RotorHazard needs the start to be ahead of it.
                staging_micros: 2_000_000,
                min_lap_micros: None,
                first_lap: FirstLap::FromTone,
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
    let send = |command: Command| {
        with_event(&registry, &event, |session| session.commands(vec![command])).unwrap();
    };
    send(Command::Record(Box::new(Event::PhaseDefined { def: heat })));

    let links = gridfpv_app::links::spawn(registry.clone());
    eventually(
        "the timer to connect and its clock to be measured",
        15,
        || {
            let view = registry.timers().list().remove(0);
            (view.status.connected && view.status.clock_error_micros.is_some()).then_some(())
        },
    )
    .await;
    assert!(
        registry.timers().list()[0].status.seats.len() >= 2,
        "RotorHazard should report its nodes as seats"
    );
    assert_eq!(side.race_status().await, 0);

    // Start Grid's race. RotorHazard must end up racing, without anyone touching it.
    send(Command::StartRace {
        heat: PhaseId("heat".into()),
    });
    let race = RaceId("heat#1".into());
    eventually("Grid's race to be running", 10, || {
        // Looking is what logs the start when the countdown ends.
        with_event(&registry, &event, |session| {
            Ok(session.state().races.running().is_some())
        })
        .unwrap()
        .then_some(())
    })
    .await;
    let mut status = side.race_status().await;
    let deadline = Instant::now() + Duration::from_secs(5);
    while status != 1 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        status = side.race_status().await;
    }
    assert_eq!(
        status, 1,
        "RotorHazard should be racing once Grid's race runs"
    );

    // Ana crosses twice, Ben once.
    side.send("simulate_lap", Some(json!({"node": 0}))).await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    side.send("simulate_lap", Some(json!({"node": 0}))).await;
    side.send("simulate_lap", Some(json!({"node": 1}))).await;
    let logged = eventually("three crossings to be logged", 10, || {
        let logged = passes(&registry, &event);
        (logged.len() == 3).then_some(logged)
    })
    .await;
    assert!(logged.iter().all(|pass| pass.race == Some(race.clone())));

    // Grid's laps: lap 1 is timed from the tone, so Ana has two laps and Ben one.
    with_event(&registry, &event, |session| {
        let state = session.state();
        let found = state.races.get(&race).unwrap();
        let laps = |node: &str| {
            state.laps.seat(
                &race,
                &timer,
                &SeatRef(node.into()),
                state.filters_for(found, &timer),
            )
        };
        let ana = laps("0");
        assert_eq!(ana.lap_count(), 2, "{ana:?}");
        // Her first lap runs from the tone to her first crossing: a fraction of a second
        // to a few seconds, never negative and never huge. That is the clocks lining up.
        let first = ana.laps().next().unwrap().micros();
        assert!((0..10_000_000).contains(&first), "first lap was {first} µs");
        let second = ana.laps().nth(1).unwrap().micros();
        assert!(
            (600_000..900_000).contains(&second),
            "second lap was {second} µs"
        );
        assert_eq!(laps("1").lap_count(), 1);
        Ok(())
    })
    .unwrap();

    // Stop Grid's race: RotorHazard is stopped and cleared, ready for the next one.
    send(Command::StopRace { race: race.clone() });
    let mut status = side.race_status().await;
    let deadline = Instant::now() + Duration::from_secs(8);
    while status != 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        status = side.race_status().await;
    }
    assert_eq!(
        status, 0,
        "RotorHazard should be ready again after Grid's stop"
    );
    // Clearing RotorHazard's laps did not add or lose a crossing in Grid.
    assert_eq!(passes(&registry, &event).len(), 3);
    send(Command::SaveRace {
        race: race.clone(),
        needs_marshal: false,
        note: None,
    });

    // A second race of the same heat starts cleanly.
    with_event(&registry, &event, |session| {
        session.commands(vec![Command::Record(Box::new(Event::HeatReopened {
            heat: PhaseId("heat".into()),
        }))])
    })
    .ok();
    send(Command::StartRace {
        heat: PhaseId("heat".into()),
    });
    eventually("the second race to be running", 10, || {
        with_event(&registry, &event, |session| {
            Ok(session.state().races.running().is_some())
        })
        .unwrap()
        .then_some(())
    })
    .await;
    side.send("simulate_lap", Some(json!({"node": 1}))).await;
    let logged = eventually("the second race's crossing", 10, || {
        let logged = passes(&registry, &event);
        (logged.len() == 4).then_some(logged)
    })
    .await;
    assert_eq!(logged[3].race, Some(RaceId("heat#2".into())));
    assert_eq!(logged[3].adapter, AdapterId(timer.0.clone()));
    send(Command::StopRace {
        race: RaceId("heat#2".into()),
    });
    tokio::time::sleep(Duration::from_secs(3)).await;
    links.abort();
}
