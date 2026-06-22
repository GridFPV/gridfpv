//! The Director-API **race-flow e2e** (race redesign Slice 3a) — the regression net for
//! the round-driven engine across Slices 3–6.
//!
//! This drives the realistic *mock-race* path end to end against the exact router `main`
//! serves ([`build_app`]) plus the **per-event source bridge** ([`spawn_registry_bridge`])
//! running the built-in **Mock** timer, so it exercises the whole keystone slice:
//!
//! 1. `POST /events` → select a class → set its membership → define a `timed_qual` round
//!    (`POST …/rounds`) → make the event active.
//! 2. `FillRound` (`POST …/control`) draws the first heat from the class membership; the
//!    test drives `Stage → Arm → Start`, the **mock bridge emits laps**, then
//!    `Finish → Score`.
//! 3. `FillRound` again → the 1-round qual is **Complete**, with a final ranking.
//! 4. A second **bracket** round seeded `FromRanking(top 2)` of the qual is `FillRound`ed
//!    and its first heat lines up the **top two of the qual ranking** — the bracket carry.
//!
//! Assertions are over the **log** (read through the event's own `AppState`, which the
//! bridge shares): each round-scheduled `HeatScheduled` carries the right `round`/`class`,
//! the lineup matches the class membership / the qual top-2, the qual completes, and a
//! ranking is produced. Lap *timing* is mock-paced so the test waits **by condition** (poll
//! the log for the expected passes) rather than by fixed sleeps — fast + deterministic.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use gridfpv_app::director::build_app;
use gridfpv_app::source::{SIM_ADAPTER, SimSource, SourceConfig, spawn_registry_bridge};
use gridfpv_events::{AdapterId, ClassId, Event, HeatId, RoundId};
use gridfpv_server::app::AppState;
use gridfpv_server::classes::CreateClassRequest;
use gridfpv_server::control::{Command, CommandAck};
use gridfpv_server::events::{EventRegistry, NewRoundReq, RoundDef, SeedingRule};
use gridfpv_server::pilots::CreatePilotRequest;
use gridfpv_server::scope::EventId;
use gridfpv_server::timers::{MOCK_TIMER_ID, TimerId, TimerKind, UpdateTimerRequest};
use http_body_util::BodyExt;
use std::collections::BTreeMap;
use tower::ServiceExt;

/// A throwaway assets dir (the e2e never serves the SPA).
fn no_assets() -> std::path::PathBuf {
    std::env::temp_dir().join("gridfpv-race-flow-no-assets")
}

/// Build a registry whose built-in Mock runs a tiny, fast heat so the whole flow finishes
/// in a few ms (the bridge poll interval dominates, so keep laps small).
fn fast_registry(laps: u32, lap_ms: u64) -> EventRegistry {
    let registry = EventRegistry::new(None).unwrap();
    registry
        .timers()
        .update(
            &TimerId(MOCK_TIMER_ID.to_string()),
            &UpdateTimerRequest {
                name: None,
                kind: Some(TimerKind::Mock { laps, lap_ms }),
                ..Default::default()
            },
        )
        .unwrap();
    registry
}

/// One JSON request against the Director router; returns the status and the body string.
async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> (StatusCode, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        builder = builder.header("Authorization", format!("Bearer {t}"));
    }
    let request = match body {
        Some(json) => builder
            .header("Content-Type", "application/json")
            .body(Body::from(json.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// Send a control `Command` and assert it acked ok.
async fn control_ok(app: &axum::Router, event: &EventId, token: &str, command: &Command) {
    let (status, body) = call(
        app,
        "POST",
        &format!("/events/{}/control", event.0),
        Some(token),
        Some(serde_json::to_value(command).unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "control HTTP failed: {body}");
    let ack: CommandAck = serde_json::from_str(&body).unwrap();
    assert!(ack.ok, "command {command:?} was rejected: {ack:?}");
}

/// Read the whole event log through its shared `AppState`.
fn read_log(state: &AppState) -> Vec<Event> {
    state
        .log()
        .lock()
        .unwrap()
        .read_all()
        .unwrap()
        .into_iter()
        .map(|s| s.event)
        .collect()
}

/// Poll the event log until `cond` holds or fail after `deadline` — deterministic-by-condition.
async fn wait_until(state: &AppState, deadline: Duration, mut cond: impl FnMut(&[Event]) -> bool) {
    let start = std::time::Instant::now();
    loop {
        if cond(&read_log(state)) {
            return;
        }
        if start.elapsed() > deadline {
            panic!("race-flow condition not met within {deadline:?}");
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The lineup of the most recent `HeatScheduled` tagged with `round`, plus its class.
fn round_heat(events: &[Event], round: &str) -> Option<(HeatId, Option<ClassId>, Vec<String>)> {
    let mut found = None;
    for e in events {
        if let Event::HeatScheduled {
            heat,
            lineup,
            class,
            round: Some(r),
            ..
        } = e
        {
            if r.0 == round {
                found = Some((
                    heat.clone(),
                    class.clone(),
                    lineup.iter().map(|c| c.0.clone()).collect(),
                ));
            }
        }
    }
    found
}

/// Lap-gate passes attributed to a heat's run window (between its Running and the next
/// terminal transition) — used to wait for the mock to finish emitting before scoring.
fn passes_in_running_window(events: &[Event]) -> usize {
    let mut running = false;
    let mut count = 0usize;
    for e in events {
        match e {
            Event::HeatStateChanged { transition, .. } => match transition {
                gridfpv_events::HeatTransition::Running => running = true,
                gridfpv_events::HeatTransition::Finished
                | gridfpv_events::HeatTransition::Scored
                | gridfpv_events::HeatTransition::Aborted
                | gridfpv_events::HeatTransition::Restarted => running = false,
                _ => {}
            },
            Event::Pass(p) if running && p.gate.is_lap_gate() => count += 1,
            _ => {}
        }
    }
    count
}

/// Drive one round-scheduled heat through the full loop with the mock bridge emitting laps:
/// FillRound → Stage → Arm → Start → (wait for laps) → Finish → Score. Returns the heat id.
async fn run_one_heat(
    app: &axum::Router,
    state: &AppState,
    event: &EventId,
    token: &str,
    round: &str,
    pilots: usize,
    laps: u32,
) -> HeatId {
    // FillRound draws + schedules the next heat.
    control_ok(
        app,
        event,
        token,
        &Command::FillRound {
            round: RoundId(round.into()),
        },
    )
    .await;
    let (heat, _class, lineup) = {
        let events = read_log(state);
        round_heat(&events, round).expect("FillRound scheduled a heat for the round")
    };
    assert_eq!(lineup.len(), pilots, "the heat lineup is the round field");

    control_ok(app, event, token, &Command::Stage { heat: heat.clone() }).await;
    control_ok(app, event, token, &Command::Arm { heat: heat.clone() }).await;
    control_ok(app, event, token, &Command::Start { heat: heat.clone() }).await;

    // The mock bridge emits a holeshot + `laps` lap-gate passes per pilot. Wait until they
    // have all landed before closing the heat.
    let want = pilots * (laps as usize + 1);
    wait_until(state, Duration::from_secs(10), move |events| {
        passes_in_running_window(events) >= want
    })
    .await;

    control_ok(app, event, token, &Command::Finish { heat: heat.clone() }).await;
    control_ok(app, event, token, &Command::Score { heat: heat.clone() }).await;
    heat
}

#[tokio::test]
async fn round_driven_mock_race_flow_e2e() {
    // A 1-lap, fast mock so the heat finishes quickly and deterministically-by-condition.
    let laps = 1u32;
    let registry = fast_registry(laps, 2);

    // Seed the directory: a class + four pilots the round will field.
    let class_id = registry
        .classes()
        .create(&CreateClassRequest {
            name: "Open".into(),
            source: Default::default(),
            reference: None,
            description: None,
        })
        .unwrap()
        .id;
    let mut pilots = Vec::new();
    for cs in ["alpha", "bravo", "charlie", "delta"] {
        let p = registry
            .pilots()
            .create(&CreatePilotRequest {
                callsign: cs.into(),
                ..Default::default()
            })
            .unwrap();
        pilots.push(p.id);
    }

    let token = registry.tokens().issue_rd_token();

    // Spawn the per-event source bridge (runs the Mock on a Running transition) and the
    // Director router over the same registry.
    let _bridge = spawn_registry_bridge(
        registry.clone(),
        SourceConfig::Sim(SimSource::new(laps, Duration::from_millis(2))),
        AdapterId(SIM_ADAPTER.to_string()),
    );
    let app = build_app(registry.clone(), &no_assets());

    // 1) Create the event over HTTP.
    let (status, body) = call(
        &app,
        "POST",
        "/events",
        Some(&token),
        Some(serde_json::json!({ "name": "Race Flow" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create event: {body}");
    let event_meta: serde_json::Value = serde_json::from_str(&body).unwrap();
    let event = EventId(event_meta["id"].as_str().unwrap().to_string());
    let state = registry.resolve(&event).unwrap();

    // 2) Select the class for the event.
    control_put(
        &app,
        &format!("/events/{}/classes", event.0),
        &token,
        serde_json::json!({ "ids": [class_id.0] }),
    )
    .await;

    // 3) Set the class membership = the four pilots (this is the round's field).
    control_put(
        &app,
        &format!("/events/{}/classes/{}/membership", event.0, class_id.0),
        &token,
        serde_json::json!({ "pilot_ids": pilots.iter().map(|p| p.0.clone()).collect::<Vec<_>>() }),
    )
    .await;

    // 4) Define a single-round timed_qual round over the class.
    let qual: RoundDef = add_round(
        &app,
        &event,
        &token,
        NewRoundReq {
            label: "Qualifying".into(),
            classes: vec![class_id.clone()],
            format: "timed_qual".into(),
            params: BTreeMap::from([("rounds".into(), "1".into())]),
            win_condition: gridfpv_engine::scoring::WinCondition::BestLap,
            seeding: SeedingRule::FromRoster,
        },
    )
    .await;

    // Make the event active (so the bridge's failover/selection reads resolve cleanly).
    control_put(
        &app,
        "/active-event",
        &token,
        serde_json::json!({ "id": event.0 }),
    )
    .await;

    // --- Drive the qual round's one heat to Score, mock bridge emitting laps. ---
    let qheat = run_one_heat(&app, &state, &event, &token, &qual.id.0, pilots.len(), laps).await;

    // The scheduled heat carried the round + the single class, and its lineup is the
    // class membership (pilot ids mapped to competitor refs, in membership order).
    let events = read_log(&state);
    let (sched_heat, sched_class, sched_lineup) = round_heat(&events, &qual.id.0).unwrap();
    assert_eq!(sched_heat, qheat);
    assert_eq!(sched_class, Some(ClassId(class_id.0.clone())));
    assert_eq!(
        sched_lineup,
        pilots.iter().map(|p| p.0.clone()).collect::<Vec<_>>(),
        "the heat lineup matches the class members in membership order"
    );

    // Race redesign Slice 4a: the IRL heat carries per-pilot channel assignments from the Mock
    // timer's available set (8 nodes, seeded Raceband). Four pilots → R1..R4 in seed order.
    let freqs = events
        .iter()
        .rev()
        .find_map(|e| match e {
            Event::HeatScheduled {
                heat, frequencies, ..
            } if *heat == sched_heat && !frequencies.is_empty() => Some(frequencies.clone()),
            _ => None,
        })
        .expect("the filled heat carries an assigned frequency set");
    assert_eq!(freqs.len(), pilots.len(), "every pilot gets a channel");
    assert_eq!(freqs[0].1, 5658, "top seed gets Raceband R1");
    assert_eq!(freqs[1].1, 5695, "second seed gets Raceband R2");
    // Each assigned competitor matches the lineup, in seed order.
    for (i, p) in pilots.iter().enumerate() {
        assert_eq!(freqs[i].0.0, p.0, "frequency assigned in seed order");
    }

    // --- FillRound again: the 1-round qual is now Complete (acks ok, schedules nothing new). ---
    let before = read_log(&state).len();
    control_ok(
        &app,
        &event,
        &token,
        &Command::FillRound {
            round: qual.id.clone(),
        },
    )
    .await;
    let after = read_log(&state).len();
    assert_eq!(
        before, after,
        "a completed round appends no new heat on a further FillRound"
    );
    // Exactly one heat was ever scheduled for the qual round.
    let qual_heats = read_log(&state)
        .iter()
        .filter(|e| matches!(e, Event::HeatScheduled { round: Some(r), .. } if *r == qual.id))
        .count();
    assert_eq!(qual_heats, 1, "the 1-round qual scheduled exactly one heat");

    // The qual produced a final ranking (best lap first) — assert it ranks the whole field.
    let qual_round = registry.rounds_of(&event).unwrap()[0].clone();
    let ranking = gridfpv_server::round_engine::round_ranking(
        &registry.meta_of(&event).unwrap(),
        &qual_round,
        &events,
    )
    .unwrap();
    assert_eq!(ranking.len(), pilots.len(), "the ranking covers the field");
    assert_eq!(ranking[0].position, 1);

    // --- A second round seeded FromRanking(top 2) of the qual — the bracket carry. ---
    let bracket: RoundDef = add_round(
        &app,
        &event,
        &token,
        NewRoundReq {
            label: "Bracket".into(),
            classes: vec![class_id.clone()],
            format: "single_elim".into(),
            params: BTreeMap::new(),
            win_condition: gridfpv_engine::scoring::WinCondition::FirstToLaps { n: laps },
            seeding: SeedingRule::FromRanking {
                source_round: qual.id.clone(),
                top_n: 2,
            },
        },
    )
    .await;

    // FillRound the bracket: its first heat lines up the top-2 of the qual ranking.
    control_ok(
        &app,
        &event,
        &token,
        &Command::FillRound {
            round: bracket.id.clone(),
        },
    )
    .await;
    let events = read_log(&state);
    let (_bheat, bclass, blineup) =
        round_heat(&events, &bracket.id.0).expect("FillRound scheduled the bracket's first heat");
    assert_eq!(bclass, Some(ClassId(class_id.0.clone())));
    let expected_top2: Vec<String> = ranking
        .iter()
        .take(2)
        .map(|e| e.competitor.0.clone())
        .collect();
    assert_eq!(
        blineup, expected_top2,
        "the bracket's first heat seeds from the qual ranking (top 2)"
    );
}

/// Race redesign Slice 4a: a `FillRound` whose lineup exceeds the timer's node count is rejected
/// (the heat-size cap) and appends no heat, end to end against the Director router.
#[tokio::test]
async fn fill_round_rejects_an_oversized_heat_e2e() {
    let registry = fast_registry(1, 2);

    // Retune the Mock to a 2-node timer (the heat-size cap), keeping Raceband available.
    registry
        .timers()
        .update(
            &TimerId(MOCK_TIMER_ID.to_string()),
            &UpdateTimerRequest {
                node_count: Some(2),
                ..Default::default()
            },
        )
        .unwrap();

    // A class with four pilots — over the 2-node cap.
    let class_id = registry
        .classes()
        .create(&CreateClassRequest {
            name: "Open".into(),
            source: Default::default(),
            reference: None,
            description: None,
        })
        .unwrap()
        .id;
    let mut pilots = Vec::new();
    for cs in ["a", "b", "c", "d"] {
        pilots.push(
            registry
                .pilots()
                .create(&CreatePilotRequest {
                    callsign: cs.into(),
                    ..Default::default()
                })
                .unwrap()
                .id,
        );
    }
    let token = registry.tokens().issue_rd_token();
    let app = build_app(registry.clone(), &no_assets());

    let (status, body) = call(
        &app,
        "POST",
        "/events",
        Some(&token),
        Some(serde_json::json!({ "name": "Oversized" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create event: {body}");
    let event = EventId(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string(),
    );
    let state = registry.resolve(&event).unwrap();

    control_put(
        &app,
        &format!("/events/{}/classes", event.0),
        &token,
        serde_json::json!({ "ids": [class_id.0] }),
    )
    .await;
    control_put(
        &app,
        &format!("/events/{}/classes/{}/membership", event.0, class_id.0),
        &token,
        serde_json::json!({ "pilot_ids": pilots.iter().map(|p| p.0.clone()).collect::<Vec<_>>() }),
    )
    .await;
    let round: RoundDef = add_round(
        &app,
        &event,
        &token,
        NewRoundReq {
            label: "Qualifying".into(),
            classes: vec![class_id.clone()],
            format: "timed_qual".into(),
            params: BTreeMap::from([("rounds".into(), "1".into())]),
            win_condition: gridfpv_engine::scoring::WinCondition::BestLap,
            seeding: SeedingRule::FromRoster,
        },
    )
    .await;

    // FillRound: the 4-pilot field exceeds the 2-node cap → a rejected command, nothing appended.
    let before = read_log(&state).len();
    let (http, body) = call(
        &app,
        "POST",
        &format!("/events/{}/control", event.0),
        Some(&token),
        Some(serde_json::to_value(Command::FillRound { round: round.id }).unwrap()),
    )
    .await;
    assert_eq!(http, StatusCode::OK, "control HTTP: {body}");
    let ack: CommandAck = serde_json::from_str(&body).unwrap();
    assert!(!ack.ok, "an oversized heat must be rejected: {ack:?}");
    let after = read_log(&state).len();
    assert_eq!(before, after, "a rejected FillRound appends no heat");
}

/// A `PUT` JSON request asserted ok (used for class selection / membership / active-event).
async fn control_put(app: &axum::Router, uri: &str, token: &str, body: serde_json::Value) {
    let (status, resp) = call(app, "PUT", uri, Some(token), Some(body)).await;
    assert_eq!(status, StatusCode::OK, "PUT {uri} failed: {resp}");
}

/// `POST …/rounds` asserted ok, returning the created [`RoundDef`].
async fn add_round(app: &axum::Router, event: &EventId, token: &str, req: NewRoundReq) -> RoundDef {
    let (status, body) = call(
        app,
        "POST",
        &format!("/events/{}/rounds", event.0),
        Some(token),
        Some(serde_json::to_value(&req).unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "add round failed: {body}");
    serde_json::from_str(&body).unwrap()
}
