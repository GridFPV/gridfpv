//! Whole races against the **real OFT daemon** running its fake Node. Local only:
//! `#[ignore]`d, so CI never runs it (CI has no OFT checkout). The test starts and stops
//! the daemon itself, so it can also restart it to stand in for a timer that rebooted.
//!
//! ```text
//! (cd ~/development/fpv/open-fpv-timer/oft && cargo build -p oftd)
//! GRIDFPV_OFTD_BIN=~/development/fpv/open-fpv-timer/oft/target/debug/oftd \
//!   cargo test -p gridfpv-app --test oft_live -- --ignored
//! ```
//!
//! The fake Node makes a crossing on seat 0 every 1.5 seconds (`--fake-lap-s 1.5`).

use std::process::{Child, Command as Process, Stdio};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use gridfpv_events::{
    AdapterId, Command, CompletionRule, EndCondition, Event, FirstLap, Format, HeatSeat, PhaseDef,
    PhaseId, PhaseKind, PilotId, RaceId, RaceRules, SeatRef, SeatSource, SignalUnit,
};
use gridfpv_server::control::with_event;
use gridfpv_server::events::{CreateEventRequest, EventId, EventRegistry};
use gridfpv_server::timers::{CreateTimerRequest, TimerKind};
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

const LAP: i64 = 1_500_000;

/// The daemon, killed when the test ends however it ends.
struct Daemon(Child);

impl Daemon {
    fn start(addr: &str) -> Self {
        let bin =
            std::env::var("GRIDFPV_OFTD_BIN").expect("set GRIDFPV_OFTD_BIN to the oftd binary");
        let child = Process::new(bin)
            // No mDNS: this daemon is for this test only and must not show up on the network.
            .args([
                "--fake",
                "--fake-lap-s",
                "1.5",
                "--no-mdns",
                "--listen",
                addr,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start oftd");
        Self(child)
    }
    fn stop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop();
    }
}

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

async fn call(
    registry: &EventRegistry,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(json) => {
            builder = builder.header("Content-Type", "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    let response = gridfpv_server::app::router(registry.clone())
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

/// Start the heat's next race and wait until it is running (the countdown is one second).
async fn start(registry: &EventRegistry, event: &EventId) {
    with_event(registry, event, |session| {
        session.commands(vec![Command::StartRace {
            heat: PhaseId("heat".into()),
        }])
    })
    .unwrap();
    eventually("the race to be running", 10, || {
        with_event(registry, event, |session| {
            Ok(session.state().races.running().is_some())
        })
        .unwrap()
        .then_some(())
    })
    .await;
}

/// The lap times, in microseconds, of seat 0 in a race.
fn laps(registry: &EventRegistry, event: &EventId, timer: &AdapterId, race: &RaceId) -> Vec<i64> {
    with_event(registry, event, |session| {
        let state = session.state();
        let found = state.races.get(race).unwrap();
        let laps = state.laps.seat(
            race,
            timer,
            &SeatRef("0".into()),
            state.filters_for(found, timer),
        );
        Ok(laps.laps().map(|lap| lap.micros()).collect())
    })
    .unwrap()
}

fn stop_and_save(registry: &EventRegistry, event: &EventId, race: &RaceId) {
    with_event(registry, event, |session| {
        session.commands(vec![Command::StopRace { race: race.clone() }])?;
        session.commands(vec![Command::SaveRace {
            race: race.clone(),
            needs_marshal: false,
            note: None,
        }])
    })
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the OFT daemon: set GRIDFPV_OFTD_BIN"]
async fn races_are_timed_by_the_real_oft_daemon() {
    // A port of our own, so a daemon someone else left running is not the one under test.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    drop(listener);
    let mut daemon = Daemon::start(&addr);

    let registry = EventRegistry::new(None).unwrap();
    let timer = registry
        .timers()
        .create(&CreateTimerRequest {
            name: "Gate".into(),
            kind: TimerKind::Oft,
            address: addr.clone(),
        })
        .unwrap()
        .id;
    let event = registry
        .create(&CreateEventRequest::named("Live"))
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
                staging_micros: 1_000_000,
                min_lap_micros: Some(500_000),
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
    with_event(&registry, &event, |session| {
        session.commands(vec![Command::Record(Box::new(Event::PhaseDefined {
            def: heat,
        }))])
    })
    .unwrap();

    let links = gridfpv_app::links::spawn(registry.clone());
    let connected = |registry: &EventRegistry| {
        let view = registry.timers().list().remove(0);
        (view.status.connected && view.status.clock_error_micros.is_some()).then_some(view)
    };
    let view = eventually("the timer to connect", 15, || connected(&registry)).await;
    assert_eq!(view.status.seats, vec![SeatRef("0".into())]);
    // On one machine the clock is known to well under a millisecond, within two seconds of
    // connecting (the link opens with a burst of quick measurements).
    eventually("a good clock measurement", 2, || {
        let error = registry.timers().list()[0].status.clock_error_micros?;
        (error < 1_000).then_some(())
    })
    .await;

    // ── Settings: a change is written to the timer and read back. The timer's own minimum
    //    lap starts at 2 s, which would swallow every other 1.5 s crossing. ──────────────
    let uri = format!("/timers/{}/config", timer.0);
    let (status, mut config) = call(&registry, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    config["min_lap_ms"] = json!(700);
    config["threshold"] = json!({"Auto": {"snr_enter": 120, "hysteresis": 40}});
    let (status, written) = call(&registry, "PUT", &uri, Some(config.clone())).await;
    assert_eq!(status, StatusCode::OK, "{written}");
    assert_eq!(written, config);
    let (_, read_back) = call(&registry, "GET", &uri, None).await;
    assert_eq!(read_back, config);
    // A threshold for one seat (OFT protocol v0.4): written, read back, and in use.
    assert!(
        registry.timers().list()[0].status.seat_thresholds,
        "this daemon should say it takes a threshold per seat"
    );
    let mut per_seat = config.clone();
    per_seat["seats"][0]["threshold"] = json!({"Manual": {"enter": -700, "exit": -760}});
    let (status, written) = call(&registry, "PUT", &uri, Some(per_seat.clone())).await;
    assert_eq!(status, StatusCode::OK, "{written}");
    assert_eq!(written, per_seat);
    let mut live = registry.timers().listen(&timer).unwrap();
    let levels = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(gridfpv_server::timers::TimerLive::Levels(levels)) = live.recv().await {
                if levels[0].enter == -700 {
                    return levels;
                }
            }
        }
    })
    .await
    .expect("the timer never reported the seat's own levels");
    assert_eq!((levels[0].enter, levels[0].exit), (-700, -760));
    // Back to the timer-wide threshold: the seat's own is gone, not left behind.
    let (status, written) = call(&registry, "PUT", &uri, Some(config.clone())).await;
    assert_eq!(status, StatusCode::OK);
    assert!(written["seats"][0].get("threshold").is_none());

    // The timer's own refusal comes through with its reason.
    let mut bad = config.clone();
    bad["seats"][0]["freq_mhz"] = json!(9999);
    let (status, refusal) = call(&registry, "PUT", &uri, Some(bad)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        refusal["message"]
            .as_str()
            .unwrap()
            .contains("refused the change"),
        "{refusal}"
    );

    // ── A race: laps from the timer, lap 1 from the tone, signal and levels logged ──────
    start(&registry, &event).await;
    tokio::time::sleep(Duration::from_millis(6_500)).await;
    let first = RaceId("heat#1".into());
    let times = laps(&registry, &event, &timer, &first);
    // A crossing every 1.5 s for 6.5 s. One that falls inside the first half second after
    // the tone is under Grid's minimum lap and does not end lap 1, so at least three laps.
    assert!(times.len() >= 3, "{times:?}");
    // Lap 1 runs from the tone to the first crossing: more than nothing, at most one lap
    // (plus the minimum lap, since a crossing inside it would be filtered).
    assert!(
        (1..=LAP + 500_000).contains(&times[0]),
        "lap 1 was {} µs",
        times[0]
    );
    // Every later lap is the fake Node's 1.5 seconds, to within 5 ms.
    for lap in &times[1..] {
        assert!((lap - LAP).abs() < 5_000, "a lap was {lap} µs: {times:?}");
    }
    stop_and_save(&registry, &event, &first);
    // The tail of the signal is logged a few seconds after the stop.
    tokio::time::sleep(Duration::from_millis(4_500)).await;
    let (status, signal) = call(
        &registry,
        "GET",
        &format!("/events/{}/signal?race=heat%231", event.0),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let seat = &signal["seats"][0];
    let samples: usize = seat["chunks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|chunk| chunk["levels"].as_array().unwrap().len())
        .sum();
    // About a second of countdown, five of racing, and three either side, at 1 kHz.
    assert!((9_000..16_000).contains(&samples), "{samples} samples");
    // No gaps: the timer sent every sample and the link dropped none. Each chunk starts
    // where the one before ended, to well within one sample.
    let chunks = seat["chunks"].as_array().unwrap();
    assert!(
        chunks.len() <= 20,
        "{} chunks: batches were not joined",
        chunks.len()
    );
    // On a quiet machine there are none at all. On a busy one the daemon can miss a batch
    // or two, so the test allows a little: what it must never see is a trace in pieces.
    let mut missing = 0;
    for pair in chunks.windows(2) {
        let end = pair[0]["from"].as_i64().unwrap()
            + pair[0]["levels"].as_array().unwrap().len() as i64 * 1000;
        let gap = pair[1]["from"].as_i64().unwrap() - end;
        assert!(gap > -500, "chunks overlap by {} µs", -gap);
        if gap >= 500 {
            missing += gap;
        }
    }
    assert!(missing < 100_000, "{missing} µs of signal are missing");
    assert_eq!(seat["chunks"][0]["unit"], json!(SignalUnit::TenthsDb));
    assert!(seat["start_at"].is_number());
    assert!(!seat["thresholds"].as_array().unwrap().is_empty());
    // The crossings sit on the trace: at each lap's end the level is well above the floor.
    let level_at = |at: i64| -> Option<(i64, i64)> {
        chunks.iter().find_map(|chunk| {
            let from = chunk["from"].as_i64().unwrap();
            let levels = chunk["levels"].as_array().unwrap();
            let index = (at - from) / 1000;
            (index >= 0 && (index as usize) < levels.len()).then(|| {
                (
                    levels[index as usize].as_i64().unwrap(),
                    chunk["noise_floor"].as_i64().unwrap(),
                )
            })
        })
    };
    with_event(&registry, &event, |session| {
        let state = session.state();
        let found = state.races.get(&first).unwrap();
        let laps = state.laps.seat(
            &first,
            &timer,
            &SeatRef("0".into()),
            state.filters_for(found, &timer),
        );
        for lap in laps.laps() {
            let (level, floor) = level_at(lap.ended_at.micros).expect("a lap end off the trace");
            assert!(
                level > floor + 200,
                "at a crossing the level was {level}, the floor {floor}"
            );
        }
        Ok(())
    })
    .unwrap();

    // ── The timer reboots between races: its clock starts again, and the next race is
    //    still timed right ─────────────────────────────────────────────────────────────
    daemon.stop();
    eventually("the drop to be noticed", 15, || {
        (!registry.timers().list()[0].status.connected).then_some(())
    })
    .await;
    let mut daemon = Daemon::start(&addr);
    eventually("the timer to come back", 30, || connected(&registry)).await;
    // A restarted daemon is back on its own defaults: settings live on the timer.
    let (_, after_reboot) = call(&registry, "GET", &uri, None).await;
    assert_eq!(after_reboot["min_lap_ms"], json!(2000));
    let (status, _) = call(&registry, "PUT", &uri, Some(config.clone())).await;
    assert_eq!(status, StatusCode::OK);
    // The new clock has to be in the log before a race is placed on it.
    let offsets = |registry: &EventRegistry| {
        events(registry, &event)
            .iter()
            .filter(|e| matches!(e, Event::ClockOffset { .. }))
            .count()
    };
    let before = offsets(&registry);
    eventually("the new clock to be logged", 10, || {
        (offsets(&registry) > before || before >= 2).then_some(())
    })
    .await;
    with_event(&registry, &event, |session| {
        session.commands(vec![Command::Record(Box::new(Event::HeatReopened {
            heat: PhaseId("heat".into()),
        }))])
    })
    .ok();
    start(&registry, &event).await;
    tokio::time::sleep(Duration::from_millis(4_000)).await;
    let second = RaceId("heat#2".into());
    let times = laps(&registry, &event, &timer, &second);
    assert!(times.len() >= 2, "after a reboot: {times:?}");
    assert!(
        (1..=LAP + 500_000).contains(&times[0]),
        "lap 1 was {} µs",
        times[0]
    );
    for lap in &times[1..] {
        assert!((lap - LAP).abs() < 5_000, "a lap was {lap} µs: {times:?}");
    }
    stop_and_save(&registry, &event, &second);
    // The first race's laps did not change when the timer came back with low numbers.
    assert!(laps(&registry, &event, &timer, &first).len() >= 3);

    // ── The timer reboots in the middle of a race: the laps either side are kept, and
    //    nothing is timed across the reboot ──────────────────────────────────────────────
    with_event(&registry, &event, |session| {
        session.commands(vec![Command::Record(Box::new(Event::HeatReopened {
            heat: PhaseId("heat".into()),
        }))])
    })
    .ok();
    start(&registry, &event).await;
    tokio::time::sleep(Duration::from_millis(4_200)).await;
    let third = RaceId("heat#3".into());
    let before_reboot = laps(&registry, &event, &timer, &third).len();
    assert!(
        before_reboot >= 2,
        "before the reboot: {before_reboot} laps"
    );
    daemon.stop();
    let mut daemon = Daemon::start(&addr);
    eventually("the timer to come back mid-race", 30, || {
        connected(&registry)
    })
    .await;
    let (status, _) = call(&registry, "PUT", &uri, Some(config.clone())).await;
    assert_eq!(status, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(6_500)).await;
    with_event(&registry, &event, |session| {
        let state = session.state();
        let found = state.races.get(&third).unwrap();
        let laps = state.laps.seat(
            &third,
            &timer,
            &SeatRef("0".into()),
            state.filters_for(found, &timer),
        );
        use gridfpv_engine::laps::LapKind;
        let kinds: Vec<LapKind> = laps.intervals.iter().map(|lap| lap.kind).collect();
        // Exactly one stretch with no time: the reboot.
        assert_eq!(
            kinds
                .iter()
                .filter(|k| **k == LapKind::TimerRestarted)
                .count(),
            1,
            "{kinds:?}"
        );
        // Laps after the reboot count, and are the fake Node's 1.5 s again.
        let after: Vec<i64> = laps
            .intervals
            .iter()
            .skip_while(|lap| lap.kind != LapKind::TimerRestarted)
            .skip(1)
            .map(|lap| lap.micros())
            .collect();
        assert!(after.len() >= 3, "after the reboot: {after:?}");
        // The first lap after a boot is left out of the check: the timer's detector is
        // still settling on the noise floor then, and places its first crossing up to a
        // tenth of a second late (seen: 1.406 s for a 1.5 s lap). That is the timer's
        // doing, not Grid's.
        for lap in &after[1..] {
            assert!((lap - LAP).abs() < 5_000, "a lap was {lap} µs: {after:?}");
        }
        // No crossing after the reboot was thrown away as "before the start" because its
        // clock is new. (At most one real one: a crossing made just before the tone and
        // reported just after it, which is what that filter is for.)
        let before_start = laps
            .uncounted
            .iter()
            .filter(|c| c.why == gridfpv_engine::laps::Uncounted::BeforeStart)
            .count();
        assert!(before_start <= 1, "{:?}", laps.uncounted);
        assert!(laps.lap_count() >= before_reboot + 2);
        Ok(())
    })
    .unwrap();
    stop_and_save(&registry, &event, &third);

    daemon.stop();
    links.abort();
}
