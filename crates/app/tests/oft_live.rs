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
                // The fake pilot's laps vary by default (since OFT 0882057). This test
                // checks lap times to 5 ms, so its pilot flies every lap the same.
                "--fake-lap-jitter-s",
                "0",
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
    // Through the route, as the console does: starting a race sets the timer's minimum lap
    // to the format's.
    let (status, answer) = call(
        registry,
        "POST",
        &format!("/events/{}/commands", event.0),
        Some(json!({"StartRace": {"heat": "heat"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
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
            own_id: None,
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
    // The daemon names its own page (spec v0.12: `manage_url` is "/"), read as soon as it
    // connects, and says nothing is wrong: its simulated receiver is always there.
    let page = eventually("the timer's own page to be known", 3, || {
        registry.timers().list()[0].status.manage_url.clone()
    })
    .await;
    assert_eq!(page, format!("http://{}/", view.timer.address));
    // There is a page there: the daemon answers a plain `GET /` with a 200.
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut socket = tokio::net::TcpStream::connect(&view.timer.address)
            .await
            .unwrap();
        let ask = format!(
            "GET / HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            view.timer.address
        );
        socket.write_all(ask.as_bytes()).await.unwrap();
        let mut answer = vec![0u8; 64];
        let read = socket.read(&mut answer).await.unwrap();
        let answer = String::from_utf8_lossy(&answer[..read]).into_owned();
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    }
    assert!(registry.timers().list()[0].status.timer_problems.is_empty());
    // On one machine the clock is known to well under a millisecond, within two seconds of
    // connecting (the link opens with a burst of quick measurements).
    eventually("a good clock measurement", 2, || {
        let error = registry.timers().list()[0].status.clock_error_micros?;
        (error < 1_000).then_some(())
    })
    .await;

    // ── Settings: a change is written to the timer and read back. The timer's lap
    //    cooldown starts at 2 s, which would swallow every other 1.5 s crossing, so it is
    //    set to half a second here. Starting a race never touches it. ───
    let uri = format!("/timers/{}/config", timer.0);
    let (status, mut config) = call(&registry, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(config["lap_cooldown_ms"], json!(2000));
    config["lap_cooldown_ms"] = json!(500);
    // The exit hold is on the timer and comes through; a changed one is written back.
    assert!(config["exit_hold_ms"].is_u64(), "{config}");
    config["exit_hold_ms"] = json!(45);
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
            if let Ok(gridfpv_server::timers::TimerLive::Levels(levels)) = live.recv().await
                && levels[0].enter == -700
            {
                return levels;
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
    let (_, racing) = call(&registry, "GET", &uri, None).await;
    assert_eq!(racing["lap_cooldown_ms"], json!(500));
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
    assert_eq!(after_reboot["lap_cooldown_ms"], json!(2000));
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

// ── Control: one writer at a time (OFT spec v0.9, §6.6) ────────────────────────

/// One plain HTTP request straight to the daemon, as a program that is not Grid would send
/// it. Gives the status and the JSON body.
async fn direct(
    addr: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let body = body.map(|json| json.to_string()).unwrap_or_default();
    let mut request = format!(
        "{method} /oft/v1{path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    if !body.is_empty() {
        request.push_str("Content-Type: application/json\r\n");
    }
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(&body);
    let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    socket.write_all(request.as_bytes()).await.unwrap();
    let mut answer = Vec::new();
    socket.read_to_end(&mut answer).await.unwrap();
    let answer = String::from_utf8_lossy(&answer).into_owned();
    let status = answer
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no status in: {answer}"));
    let json = answer
        .find('{')
        .and_then(|at| serde_json::from_str(answer[at..].trim_end_matches(['\r', '\n', '0'])).ok())
        .unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// The timer's own page handing control to a claim, or to nobody: `POST /control` with the
/// `Origin` a browser on that page would send.
async fn page_grants(addr: &str, grant: Option<u64>) {
    let origin = format!("http://{addr}");
    let (status, answer) = direct(
        addr,
        "POST",
        "/control",
        &[("Origin", &origin)],
        Some(json!({ "grant": grant })),
    )
    .await;
    assert_eq!(status, 200, "{answer}");
}

fn control_of(registry: &EventRegistry) -> Option<gridfpv_server::timers::TimerControl> {
    registry.timers().list().remove(0).status.control
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the OFT daemon: set GRIDFPV_OFTD_BIN"]
async fn control_is_shared_with_another_program_on_the_real_oft_daemon() {
    use futures_util::{SinkExt, StreamExt};
    use gridfpv_server::timers::TimerControl;
    use tokio_tungstenite::tungstenite::Message;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    drop(listener);
    let mut daemon = Daemon::start(&addr);

    // Another program connects first and claims control, as RotorHazard's plugin would.
    let mut theirs = eventually("the daemon to listen", 10, || {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                tokio_tungstenite::connect_async(format!("ws://{addr}/oft/v1/stream"))
                    .await
                    .ok()
            })
        })
    })
    .await
    .0;
    theirs
        .send(Message::text(
            r#"{"type":"control","claim":"RotorHazard test"}"#,
        ))
        .await
        .unwrap();
    // Keep its stream read, so the daemon is never blocked on it.
    let (mut their_sink, mut their_stream) = theirs.split();
    let their_reader = tokio::spawn(async move { while their_stream.next().await.is_some() {} });

    let registry = EventRegistry::new(None).unwrap();
    let timer = registry
        .timers()
        .create(&CreateTimerRequest {
            name: "Gate".into(),
            kind: TimerKind::Oft,
            address: addr.clone(),
            own_id: None,
        })
        .unwrap()
        .id;
    let links = gridfpv_app::oft::spawn(registry.clone());
    let config_uri = format!("/timers/{}/config", timer.0);

    // Grid sees who has control, and stands in line behind it.
    eventually("Grid to see the other program in control", 10, || {
        (control_of(&registry)
            == Some(TimerControl::Other {
                name: "RotorHazard test".into(),
            }))
        .then_some(())
    })
    .await;
    let grid_name = registry.timers().list()[0]
        .status
        .control_name
        .clone()
        .expect("Grid says the name it claimed under");
    let grid_claim = eventually("Grid's claim to stand on the timer", 10, || {
        let (_, listed) = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(direct(&addr, "GET", "/control", &[], None))
        });
        listed["claims"]
            .as_array()?
            .iter()
            .find(|claim| claim["name"] == grid_name.as_str())
            .and_then(|claim| claim["id"].as_u64())
    })
    .await;

    // A change from Grid is refused, naming the program, and the timer is untouched.
    let (_, before) = direct(&addr, "GET", "/config", &[], None).await;
    let (status, read) = call(&registry, "GET", &config_uri, None).await;
    assert_eq!(status, StatusCode::OK, "{read}");
    let mut wanted = read.clone();
    wanted["seats"][0]["freq_mhz"] = json!(5917);
    let (status, said) = call(&registry, "PUT", &config_uri, Some(wanted.clone())).await;
    assert_eq!(status, StatusCode::CONFLICT, "{said}");
    assert!(
        said["message"]
            .as_str()
            .unwrap()
            .contains("RotorHazard test"),
        "{said}"
    );
    assert_eq!(direct(&addr, "GET", "/config", &[], None).await.1, before);

    // The person at the timer's own page hands control to Grid.
    page_grants(&addr, Some(grid_claim)).await;
    eventually("control to come to Grid", 10, || {
        (control_of(&registry) == Some(TimerControl::Grid)).then_some(())
    })
    .await;
    let rev = registry.timers().list()[0].status.config_rev;
    let (status, written) = call(&registry, "PUT", &config_uri, Some(wanted.clone())).await;
    assert_eq!(status, StatusCode::OK, "{written}");
    let (_, now) = direct(&addr, "GET", "/config", &[], None).await;
    assert_eq!(now["seats"][0]["freq_mhz"], 5917);
    // The timer counted the change, and Grid's status carries the new number.
    eventually("config_rev to move", 10, || {
        let now = registry.timers().list()[0].status.config_rev;
        (now.is_some() && now != rev).then_some(())
    })
    .await;

    // The real guard: a write with no key, or a wrong one, is a 423 while Grid has control.
    let (status, refused) = direct(&addr, "PUT", "/config", &[], Some(before.clone())).await;
    assert_eq!(status, 423, "{refused}");
    assert_eq!(refused["error"], format!("controlled by {grid_name}"));
    let (status, _) = direct(
        &addr,
        "PUT",
        "/config",
        &[("OFT-Control", "00000000")],
        Some(before.clone()),
    )
    .await;
    assert_eq!(status, 423);
    assert_eq!(direct(&addr, "GET", "/config", &[], None).await.1, now);
    // And only the timer's own page chooses the writer: a program cannot grant itself.
    let (status, _) = direct(&addr, "POST", "/control", &[], Some(json!({"grant": null}))).await;
    assert_eq!(status, 403);

    // The page frees control: nobody has it, and both programs may write.
    page_grants(&addr, None).await;
    eventually("nobody to be in control", 10, || {
        (control_of(&registry) == Some(TimerControl::Nobody)).then_some(())
    })
    .await;
    wanted["seats"][0]["freq_mhz"] = json!(5732);
    let (status, written) = call(&registry, "PUT", &config_uri, Some(wanted.clone())).await;
    assert_eq!(status, StatusCode::OK, "{written}");
    let (status, _) = direct(&addr, "PUT", "/config", &[], Some(before.clone())).await;
    assert_eq!(status, 200);
    // Grid notices the other program's change and reads the settings fresh.
    let (_, read) = call(&registry, "GET", &config_uri, None).await;
    assert_eq!(read["seats"][0]["freq_mhz"], before["seats"][0]["freq_mhz"]);

    // The other program takes control back at the page, then goes away for good: its
    // stream closes, so its claim ends after the ten seconds a holder is given, and Grid,
    // next in line, has control without anyone doing anything.
    let (_, listed) = direct(&addr, "GET", "/control", &[], None).await;
    let their_claim = listed["claims"]
        .as_array()
        .unwrap()
        .iter()
        .find(|claim| claim["name"] == "RotorHazard test")
        .and_then(|claim| claim["id"].as_u64())
        .unwrap();
    page_grants(&addr, Some(their_claim)).await;
    eventually("the other program to be in control again", 10, || {
        matches!(control_of(&registry), Some(TimerControl::Other { .. })).then_some(())
    })
    .await;
    their_sink.close().await.ok();
    their_reader.abort();
    eventually("control to pass to Grid", 20, || {
        (control_of(&registry) == Some(TimerControl::Grid)).then_some(())
    })
    .await;
    let (status, written) = call(&registry, "PUT", &config_uri, Some(wanted.clone())).await;
    assert_eq!(status, StatusCode::OK, "{written}");

    // The timer restarts. Grid claims afresh on the new boot and is the writer again.
    daemon.stop();
    eventually("the link to drop", 15, || {
        (!registry.timers().list()[0].status.connected).then_some(())
    })
    .await;
    daemon = Daemon::start(&addr);
    eventually("Grid to have control after the restart", 20, || {
        let status = registry.timers().list().remove(0).status;
        (status.connected && status.control == Some(TimerControl::Grid)).then_some(())
    })
    .await;
    let (_, read) = call(&registry, "GET", &config_uri, None).await;
    let mut wanted = read.clone();
    wanted["seats"][0]["freq_mhz"] = json!(5658);
    let (status, written) = call(&registry, "PUT", &config_uri, Some(wanted)).await;
    assert_eq!(status, StatusCode::OK, "{written}");
    assert_eq!(
        direct(&addr, "GET", "/config", &[], None).await.1["seats"][0]["freq_mhz"],
        5658
    );

    links.abort();
    daemon.stop();
}
