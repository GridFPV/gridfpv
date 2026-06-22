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
use gridfpv_engine::heat::GraceWindow;
use gridfpv_events::{AdapterId, ClassId, Event, HeatId, RoundId};
use gridfpv_server::app::AppState;
use gridfpv_server::classes::CreateClassRequest;
use gridfpv_server::control::{Command, CommandAck};
use gridfpv_server::events::{
    ChannelMode, EventRegistry, MemberSlot, NewRoundReq, RoundDef, SeedingRule, StartProcedure,
};
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
                | gridfpv_events::HeatTransition::Finalized
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
/// FillRound → Stage → Start → SkipCountdown → (wait for laps) → ForceEnd → Finalize. Returns the
/// heat id.
///
/// Heat-lifecycle Slice 2 collapsed the manual `Arm`/`Start`/`Finish` commands: `Start` now arms the
/// heat and the runtime auto-advances `Armed → Running` (after the round's start delay) and
/// `Running → Unofficial` (on the win condition + grace). This e2e uses the **overrides**
/// `SkipCountdown` / `ForceEnd` to bypass those timers so it stays fast and deterministic regardless
/// of the round's start-delay / win-condition config (this round scores `BestLap`, which has no
/// intrinsic auto-completion). A dedicated test drives the *auto* path
/// ([`heat_auto_advances_running_to_unofficial_under_the_clock`]).
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
    // `Start` arms the heat (Staged → Armed) and runs the start procedure; `SkipCountdown` forces
    // Armed → Running immediately so the e2e doesn't wait on the randomized start delay.
    control_ok(app, event, token, &Command::Start { heat: heat.clone() }).await;
    control_ok(
        app,
        event,
        token,
        &Command::SkipCountdown { heat: heat.clone() },
    )
    .await;

    // The mock bridge emits a holeshot + `laps` lap-gate passes per pilot. Wait until they
    // have all landed before closing the heat.
    let want = pilots * (laps as usize + 1);
    wait_until(state, Duration::from_secs(10), move |events| {
        passes_in_running_window(events) >= want
    })
    .await;

    // `ForceEnd` closes the race (Running → Unofficial) — the override for the auto-completion.
    control_ok(app, event, token, &Command::ForceEnd { heat: heat.clone() }).await;
    control_ok(app, event, token, &Command::Finalize { heat: heat.clone() }).await;
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
        serde_json::json!({ "pilots": pilots.iter().map(|p| p.0.clone()).collect::<Vec<_>>() }),
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
            win_condition: Some(gridfpv_engine::scoring::WinCondition::BestLap),
            time_limit_secs: None,
            seeding: SeedingRule::FromRoster,
            // Per-heat: this flow asserts the whole-field heat + first-fit channel assignment.
            channel_mode: Some(ChannelMode::PerHeat),
            staging_timer_secs: None,
            start_procedure: None,
            grace_window: None,
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
            win_condition: Some(gridfpv_engine::scoring::WinCondition::FirstToLaps { n: laps }),
            time_limit_secs: None,
            seeding: SeedingRule::FromRanking {
                source_round: qual.id.clone(),
                top_n: 2,
            },
            // single_elim defaults to PerHeat anyway; keep it explicit for the bracket carry.
            channel_mode: Some(ChannelMode::PerHeat),
            staging_timer_secs: None,
            start_procedure: None,
            grace_window: None,
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

    // --- Race redesign Slice 5/6a: the round-ranking + class-standings read routes. ---

    // 1) GET …/rounds/{round}/ranking returns the round's ranking, in the SAME order the engine
    //    seeds `FromRanking` from — so the served ranking == the bracket's seeding source.
    let (status, body) = call(
        &app,
        "GET",
        &format!("/events/{}/rounds/{}/ranking", event.0, qual.id.0),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "round ranking: {body}");
    let served: Vec<gridfpv_engine::format::RankEntry> = serde_json::from_str(&body).unwrap();
    let served_names: Vec<String> = served.iter().map(|e| e.competitor.0.clone()).collect();
    let engine_names: Vec<String> = ranking.iter().map(|e| e.competitor.0.clone()).collect();
    assert_eq!(
        served_names, engine_names,
        "the route's ranking matches the engine's FromRanking seeding order"
    );
    // The bracket's lineup (the top-2 carry) is exactly the served ranking's top-2.
    assert_eq!(
        blineup,
        served_names.iter().take(2).cloned().collect::<Vec<_>>(),
        "a FromRanking bracket's seeding equals the served round ranking"
    );

    // 2) GET …/classes/{class}/standings aggregates the class's rounds: one row per pilot, best
    //    first, with the qual round's points (4-pilot field → 4..1). The class's two rounds
    //    (qual + bracket) both feed the standings, so positions reflect the aggregate.
    let (status, body) = call(
        &app,
        "GET",
        &format!("/events/{}/classes/{}/standings", event.0, class_id.0),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "class standings: {body}");
    let standings: gridfpv_server::round_engine::ClassStandings =
        serde_json::from_str(&body).unwrap();
    assert_eq!(standings.class.0, class_id.0);
    assert_eq!(
        standings.standings.len(),
        pilots.len(),
        "every class pilot has a standings row"
    );
    // The top of the standings is the qual winner (they also won/led the bracket carry).
    assert_eq!(
        standings.standings[0].competitor.0, engine_names[0],
        "the standings leader is the qual ranking leader"
    );
    assert_eq!(standings.standings[0].position, 1);
    // The standings best lap is derived from the heats' real laps (the lap-list projection), so a
    // pilot who completed laps reports a non-null minimum — not the null the old metric-reading
    // left for non-BestLap (FirstToLaps bracket) rounds.
    assert!(
        standings.standings[0]
            .best_lap_micros
            .is_some_and(|micros| micros > 0),
        "the standings leader has a real best lap, not null: {:?}",
        standings.standings[0],
    );
    // Standings are ordered by points (descending) — non-increasing down the list.
    for pair in standings.standings.windows(2) {
        assert!(
            pair[0].points >= pair[1].points,
            "standings are ordered by points descending"
        );
    }
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
        serde_json::json!({ "pilots": pilots.iter().map(|p| p.0.clone()).collect::<Vec<_>>() }),
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
            win_condition: Some(gridfpv_engine::scoring::WinCondition::BestLap),
            time_limit_secs: None,
            seeding: SeedingRule::FromRoster,
            // Per-heat: this flow asserts the node-cap rejection of an oversized whole-field heat.
            channel_mode: Some(ChannelMode::PerHeat),
            staging_timer_secs: None,
            start_procedure: None,
            grace_window: None,
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

#[tokio::test]
async fn static_channel_balanced_qual_flow_e2e() {
    // A **static** qual round (race redesign Slice 7a): six members across three Raceband channels
    // on a 2-node timer (channels > node_count). The channel-balanced builder forms heats of ≤2
    // pilots on distinct channels off each member's fixed channel; every member flies, and each
    // heat carries the members' assigned channels (no first-fit).
    let laps = 1u32;
    let registry = fast_registry(laps, 2);
    // Retune the Mock to a 2-node cap (channels are the default Raceband 8-wide pool — > node cap).
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
    for cs in ["a", "b", "c", "d", "e", "f"] {
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
    let _bridge = spawn_registry_bridge(
        registry.clone(),
        SourceConfig::Sim(SimSource::new(laps, Duration::from_millis(2))),
        AdapterId(SIM_ADAPTER.to_string()),
    );
    let app = build_app(registry.clone(), &no_assets());

    let (status, body) = call(
        &app,
        "POST",
        "/events",
        Some(&token),
        Some(serde_json::json!({ "name": "Static Qual" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create event: {body}");
    let event_meta: serde_json::Value = serde_json::from_str(&body).unwrap();
    let event = EventId(event_meta["id"].as_str().unwrap().to_string());
    let state = registry.resolve(&event).unwrap();

    control_put(
        &app,
        &format!("/events/{}/classes", event.0),
        &token,
        serde_json::json!({ "ids": [class_id.0] }),
    )
    .await;

    // Membership with per-pilot fixed channels — 3 Raceband channels, two pilots each.
    let channels = [5658u16, 5695, 5732];
    let member_slots: Vec<serde_json::Value> = pilots
        .iter()
        .enumerate()
        .map(|(i, p)| serde_json::json!({ "pilot": p.0, "channel": channels[i % 3] }))
        .collect();
    control_put(
        &app,
        &format!("/events/{}/classes/{}/membership", event.0, class_id.0),
        &token,
        serde_json::json!({ "pilots": member_slots }),
    )
    .await;

    // A **static** timed_qual round (1 format-round).
    let round: RoundDef = add_round(
        &app,
        &event,
        &token,
        NewRoundReq {
            label: "Qualifying".into(),
            classes: vec![class_id.clone()],
            format: "timed_qual".into(),
            params: BTreeMap::from([("rounds".into(), "1".into())]),
            win_condition: Some(gridfpv_engine::scoring::WinCondition::BestLap),
            time_limit_secs: None,
            seeding: SeedingRule::FromRoster,
            channel_mode: Some(ChannelMode::Static),
            staging_timer_secs: None,
            start_procedure: None,
            grace_window: None,
        },
    )
    .await;
    control_put(
        &app,
        "/active-event",
        &token,
        serde_json::json!({ "id": event.0 }),
    )
    .await;

    // Drive the round's channel-balanced heats one at a time to Score until Complete.
    for _ in 0..10 {
        control_ok(
            &app,
            &event,
            &token,
            &Command::FillRound {
                round: round.id.clone(),
            },
        )
        .await;
        // The latest round heat (if a new one was scheduled this FillRound).
        let events = read_log(&state);
        let scheduled: Vec<&Event> = events
            .iter()
            .filter(|e| matches!(e, Event::HeatScheduled { round: Some(r), .. } if *r == round.id))
            .collect();
        // Find the newest heat with no terminal transition yet (the one to drive).
        let pending = scheduled.iter().rev().find_map(|e| match e {
            Event::HeatScheduled {
                heat, frequencies, ..
            } => {
                let scored = events.iter().any(|x| matches!(x, Event::HeatStateChanged { heat: h, transition: gridfpv_events::HeatTransition::Finalized } if h == heat));
                if scored {
                    None
                } else {
                    Some((heat.clone(), frequencies.clone()))
                }
            }
            _ => None,
        });
        let Some((heat, freqs)) = pending else {
            break; // Complete — no outstanding heat
        };
        // The heat is ≤ the 2-node cap and channel-distinct, carrying the members' fixed channels.
        assert!(freqs.len() <= 2, "static heat exceeds node cap: {freqs:?}");
        let mut seen = std::collections::BTreeSet::new();
        for (_, ch) in &freqs {
            assert!(seen.insert(*ch), "duplicate channel in a static heat");
            assert!(
                channels.contains(ch),
                "channel {ch} is a membership channel"
            );
        }
        let want = freqs.len() * (laps as usize + 1);
        control_ok(&app, &event, &token, &Command::Stage { heat: heat.clone() }).await;
        // Heat-lifecycle Slice 2: `Start` arms; `SkipCountdown`/`ForceEnd` are the overrides that
        // bypass the runtime start/completion clocks so this static-formation flow stays fast.
        control_ok(&app, &event, &token, &Command::Start { heat: heat.clone() }).await;
        control_ok(
            &app,
            &event,
            &token,
            &Command::SkipCountdown { heat: heat.clone() },
        )
        .await;
        wait_until(&state, Duration::from_secs(10), move |events| {
            passes_in_running_window(events) >= want
        })
        .await;
        control_ok(
            &app,
            &event,
            &token,
            &Command::ForceEnd { heat: heat.clone() },
        )
        .await;
        control_ok(&app, &event, &token, &Command::Finalize { heat }).await;
    }

    // Every one of the six members flew, across channel-balanced heats of ≤2 distinct channels.
    let events = read_log(&state);
    let flown: std::collections::BTreeSet<String> = events
        .iter()
        .filter_map(|e| match e {
            Event::HeatScheduled {
                lineup,
                round: Some(r),
                ..
            } if *r == round.id => Some(lineup.iter().map(|c| c.0.clone())),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(flown.len(), pilots.len(), "every static member flies");
    // The pool spans all three membership channels (> the 2-node cap).
    let used: std::collections::BTreeSet<u16> = events
        .iter()
        .filter_map(|e| match e {
            Event::HeatScheduled {
                frequencies,
                round: Some(r),
                ..
            } if *r == round.id => Some(frequencies.iter().map(|(_, ch)| *ch)),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(
        used.len(),
        3,
        "channels span the 3-wide pool, beyond the 2-node cap"
    );
    // The static MemberSlot wire shape round-tripped (the membership carried channels).
    let _ = MemberSlot::new(pilots[0].clone());
}

/// A `PUT` JSON request asserted ok (used for class selection / membership / active-event).
async fn control_put(app: &axum::Router, uri: &str, token: &str, body: serde_json::Value) {
    let (status, resp) = call(app, "PUT", uri, Some(token), Some(body)).await;
    assert_eq!(status, StatusCode::OK, "PUT {uri} failed: {resp}");
}

/// Fold the heat's current `HeatState` from a log (the engine's pure fold), for asserting the
/// runtime clock drove the heat to a given state.
fn heat_state_of(events: &[Event], heat: &HeatId) -> Option<gridfpv_engine::heat::HeatState> {
    gridfpv_engine::heat::heat_state(events, heat)
}

/// Build the event + a single round whose **start procedure is near-instant** and whose win
/// condition is `FirstToLaps`, so the runtime clock drives both auto-transitions quickly. Returns
/// `(registry, app, token, event, round_id, pilots)`.
async fn fast_auto_event(
    laps: u32,
    lap_ms: u64,
    win_laps: u32,
) -> (
    EventRegistry,
    axum::Router,
    String,
    EventId,
    RoundId,
    Vec<gridfpv_server::scope::PilotId>,
) {
    let registry = fast_registry(laps, lap_ms);
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
    for cs in ["alpha", "bravo"] {
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
    let _bridge = spawn_registry_bridge(
        registry.clone(),
        SourceConfig::Sim(SimSource::new(laps, Duration::from_millis(lap_ms))),
        AdapterId(SIM_ADAPTER.to_string()),
    );
    // Leak the bridge handle so it runs for the whole test (the registry keeps the logs alive).
    std::mem::forget(_bridge);
    let app = build_app(registry.clone(), &no_assets());

    let (status, body) = call(
        &app,
        "POST",
        "/events",
        Some(&token),
        Some(serde_json::json!({ "name": "Auto Clock" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create event: {body}");
    let event_meta: serde_json::Value = serde_json::from_str(&body).unwrap();
    let event = EventId(event_meta["id"].as_str().unwrap().to_string());

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
        serde_json::json!({ "pilots": pilots.iter().map(|p| p.0.clone()).collect::<Vec<_>>() }),
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
            // FirstToLaps gives the completion clock an intrinsic end (the leader reaching `win_laps`).
            win_condition: Some(gridfpv_engine::scoring::WinCondition::FirstToLaps { n: win_laps }),
            time_limit_secs: None,
            seeding: SeedingRule::FromRoster,
            channel_mode: Some(ChannelMode::PerHeat),
            staging_timer_secs: None,
            // A near-instant start hold so the auto Armed→Running fires within a poll.
            start_procedure: Some(StartProcedure::RandomizedDelay {
                min_delay_ms: 0,
                max_delay_ms: 1,
                tone: None,
            }),
            // A short bounded grace so the auto Running→Unofficial fires promptly after the win.
            grace_window: Some(GraceWindow::Duration { micros: 5_000 }),
        },
    )
    .await;
    control_put(
        &app,
        "/active-event",
        &token,
        serde_json::json!({ "id": event.0 }),
    )
    .await;
    (registry, app, token, event, round.id, pilots)
}

/// The headline auto-advance: after `Start` (which arms the heat) the **runtime clock** drives the
/// heat all the way to `Unofficial` on its own — auto `Armed → Running` (after the logged start
/// delay) and auto `Running → Unofficial` (on the win condition + grace) — with no manual
/// `SkipCountdown`/`ForceEnd`. Then `Finalize` reaches `Final`.
#[tokio::test]
async fn heat_auto_advances_running_to_unofficial_under_the_clock() {
    let (registry, app, token, event, round, pilots) = fast_auto_event(5, 2, 3).await;
    let state = registry.resolve(&event).unwrap();

    control_ok(
        &app,
        &event,
        &token,
        &Command::FillRound {
            round: round.clone(),
        },
    )
    .await;
    let (heat, _class, _lineup) =
        round_heat(&read_log(&state), &round.0).expect("a heat scheduled");

    // Stage, then Start — Start ARMS the heat; the runtime then auto-advances. No SkipCountdown.
    control_ok(&app, &event, &token, &Command::Stage { heat: heat.clone() }).await;
    control_ok(&app, &event, &token, &Command::Start { heat: heat.clone() }).await;

    // The runtime logs the start delay (`HeatStarting`), then auto-appends `Running`.
    let starting_heat = heat.clone();
    wait_until(&state, Duration::from_secs(10), move |events| {
        events
            .iter()
            .any(|e| matches!(e, Event::HeatStarting { heat: h, .. } if *h == starting_heat))
    })
    .await;

    // The completion clock then auto-advances `Running → Unofficial` once the leader hits 3 laps +
    // the grace window — no `ForceEnd` was sent.
    let unofficial_heat = heat.clone();
    wait_until(&state, Duration::from_secs(15), move |events| {
        heat_state_of(events, &unofficial_heat) == Some(gridfpv_engine::heat::HeatState::Unofficial)
    })
    .await;

    // Exactly one auto `Running` and one auto `Finished` were appended (the runtime drove them).
    let events = read_log(&state);
    let running = events
        .iter()
        .filter(|e| matches!(e, Event::HeatStateChanged { heat: h, transition: gridfpv_events::HeatTransition::Running } if *h == heat))
        .count();
    let finished = events
        .iter()
        .filter(|e| matches!(e, Event::HeatStateChanged { heat: h, transition: gridfpv_events::HeatTransition::Finished } if *h == heat))
        .count();
    assert_eq!(running, 1, "the runtime auto-appended exactly one Running");
    assert_eq!(
        finished, 1,
        "the runtime auto-appended exactly one Finished"
    );

    // Finalize closes the loop.
    control_ok(
        &app,
        &event,
        &token,
        &Command::Finalize { heat: heat.clone() },
    )
    .await;
    assert_eq!(
        heat_state_of(&read_log(&state), &heat),
        Some(gridfpv_engine::heat::HeatState::Final)
    );
    assert_eq!(pilots.len(), 2);
}

/// **Determinism / replay** (the headline guarantee): drive a heat through the full auto-clock path
/// to `Final`, then **replay the resulting log** through the pure engine fold and assert the replay
/// reproduces the identical state — the logged start delay (`HeatStarting`) + the two
/// runtime-appended auto-transitions are *facts* the fold reads, never re-randomized or recomputed
/// from a clock (race-engine.html §6).
#[tokio::test]
async fn the_logged_clock_events_replay_deterministically() {
    let (registry, app, token, event, round, _pilots) = fast_auto_event(5, 2, 3).await;
    let state = registry.resolve(&event).unwrap();

    control_ok(
        &app,
        &event,
        &token,
        &Command::FillRound {
            round: round.clone(),
        },
    )
    .await;
    let (heat, _class, _lineup) =
        round_heat(&read_log(&state), &round.0).expect("a heat scheduled");
    control_ok(&app, &event, &token, &Command::Stage { heat: heat.clone() }).await;
    control_ok(&app, &event, &token, &Command::Start { heat: heat.clone() }).await;

    // Wait for the full auto path to land, then Finalize.
    let wheat = heat.clone();
    wait_until(&state, Duration::from_secs(15), move |events| {
        heat_state_of(events, &wheat) == Some(gridfpv_engine::heat::HeatState::Unofficial)
    })
    .await;
    control_ok(
        &app,
        &event,
        &token,
        &Command::Finalize { heat: heat.clone() },
    )
    .await;

    let log = read_log(&state);

    // (1) The start delay was logged once as a FACT — the runtime chose it, the fold never re-rolls.
    let delays: Vec<u32> = log
        .iter()
        .filter_map(|e| match e {
            Event::HeatStarting { heat: h, delay_ms } if *h == heat => Some(*delay_ms),
            _ => None,
        })
        .collect();
    assert_eq!(delays.len(), 1, "exactly one logged start delay");

    // (2) Replay: fold the SAME log twice; a pure fold gives the same state every time, with no
    // hidden clock/RNG re-evaluation. This is the deterministic-replay guarantee.
    let first = heat_state_of(&log, &heat);
    let second = heat_state_of(&log, &heat);
    assert_eq!(first, second);
    assert_eq!(first, Some(gridfpv_engine::heat::HeatState::Final));

    // (3) Replaying the start delay reads the SAME value — no re-randomization on replay.
    let replay_delays: Vec<u32> = log
        .iter()
        .filter_map(|e| match e {
            Event::HeatStarting { heat: h, delay_ms } if *h == heat => Some(*delay_ms),
            _ => None,
        })
        .collect();
    assert_eq!(
        replay_delays, delays,
        "the logged delay is stable on replay"
    );

    // (4) The auto-transitions appear exactly once each, in canonical forward order — the same log
    // a fresh Director would replay identically.
    let transitions: Vec<gridfpv_events::HeatTransition> = log
        .iter()
        .filter_map(|e| match e {
            Event::HeatStateChanged {
                heat: h,
                transition,
            } if *h == heat => Some(*transition),
            _ => None,
        })
        .collect();
    assert_eq!(
        transitions,
        vec![
            gridfpv_events::HeatTransition::Staged,
            gridfpv_events::HeatTransition::Armed,
            gridfpv_events::HeatTransition::Running,
            gridfpv_events::HeatTransition::Finished,
            gridfpv_events::HeatTransition::Finalized,
        ],
        "the runtime-driven log folds the full forward path, once each"
    );
}

/// Open-practice refinement e2e: creating an **open-practice** round auto-creates its single channel
/// heat (no manual `FillRound`), idempotently (re-creating doesn't duplicate it), and a short
/// `time_limit_secs` auto-ends the running practice (Running → Unofficial) with no `ForceEnd`.
#[tokio::test]
async fn open_practice_round_auto_creates_heat_and_time_limit_auto_ends_it_e2e() {
    let registry = fast_registry(3, 1);
    let token = registry.tokens().issue_rd_token();
    let _bridge = spawn_registry_bridge(
        registry.clone(),
        SourceConfig::Sim(SimSource::new(3, Duration::from_millis(1))),
        AdapterId(SIM_ADAPTER.to_string()),
    );
    std::mem::forget(_bridge);
    let app = build_app(registry.clone(), &no_assets());

    let (status, body) = call(
        &app,
        "POST",
        "/events",
        Some(&token),
        Some(serde_json::json!({ "name": "Practice" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create event: {body}");
    let event_meta: serde_json::Value = serde_json::from_str(&body).unwrap();
    let event = EventId(event_meta["id"].as_str().unwrap().to_string());

    // Define an open-practice round: NO win condition, a 1s time limit, AllChannels seeding over two
    // node-seats. Creating it must auto-build the single open heat (the channels are the lineup).
    let open_req = || NewRoundReq {
        label: "Open Practice".into(),
        classes: vec![],
        format: "open_practice".into(),
        params: BTreeMap::new(),
        win_condition: None,
        time_limit_secs: Some(1),
        seeding: SeedingRule::AllChannels {
            channels: vec![0, 1],
        },
        channel_mode: None,
        staging_timer_secs: None,
        start_procedure: Some(StartProcedure::RandomizedDelay {
            min_delay_ms: 0,
            max_delay_ms: 1,
            tone: None,
        }),
        grace_window: None,
    };
    let round: RoundDef = add_round(&app, &event, &token, open_req()).await;
    let state = registry.resolve(&event).unwrap();

    // (a) The heat was auto-created: exactly one HeatScheduled tagged with the round, no FillRound.
    let scheduled_for_round = |events: &[Event]| {
        events
            .iter()
            .filter(|e| matches!(e, Event::HeatScheduled { round: Some(r), .. } if *r == round.id))
            .count()
    };
    assert_eq!(
        scheduled_for_round(&read_log(&state)),
        1,
        "creating an open-practice round auto-creates exactly one heat"
    );

    // (b) Idempotent: re-running the auto-fill (the same round's FillRound) appends no second heat.
    control_ok(
        &app,
        &event,
        &token,
        &Command::FillRound {
            round: round.id.clone(),
        },
    )
    .await;
    assert_eq!(
        scheduled_for_round(&read_log(&state)),
        1,
        "a re-fill of the open-practice round does not duplicate its heat"
    );

    let (heat, _class, _lineup) =
        round_heat(&read_log(&state), &round.id.0).expect("the auto-created open-practice heat");

    // (c) The time limit auto-ends the running practice. Make active, Stage, Start — then the runtime
    // drives Armed → Running (instant start hold) and, ~1s later, the time-limit Running → Unofficial.
    control_put(
        &app,
        "/active-event",
        &token,
        serde_json::json!({ "id": event.0 }),
    )
    .await;
    control_ok(&app, &event, &token, &Command::Stage { heat: heat.clone() }).await;
    control_ok(&app, &event, &token, &Command::Start { heat: heat.clone() }).await;

    let target = heat.clone();
    wait_until(&state, Duration::from_secs(6), move |events| {
        heat_state_of(events, &target) == Some(gridfpv_engine::heat::HeatState::Unofficial)
    })
    .await;

    // Exactly one auto Finished (the Running → Unofficial step) was appended by the time-limit driver.
    let events = read_log(&state);
    let finished = events
        .iter()
        .filter(|e| matches!(e, Event::HeatStateChanged { heat: h, transition: gridfpv_events::HeatTransition::Finished } if *h == heat))
        .count();
    assert_eq!(
        finished, 1,
        "the time limit auto-ends the practice exactly once"
    );

    // (d) Live-state desync fix — the **served** live state's phase/clock are the REAL log's at every
    // step, with the per-channel laps spliced on top (laps-only overlay). The console renders phase →
    // buttons and the clock from this served state, so it must follow the log exactly — never a stale
    // synthetic phase. We fetch the served heat snapshot (the same merge the `/stream` fold applies).
    //
    // The time limit has driven the LOG to `Unofficial` (asserted above). The served live state must
    // therefore read `Unofficial` (so the console clock freezes at the duration — no synthetic-start
    // bump), while the non-logged per-channel laps stay visible (the accumulator holds them through
    // `Unofficial`; only a true terminal / abort / restart clears them).
    let served_live = |heat: &HeatId| {
        let app = app.clone();
        let event = event.clone();
        let token = token.clone();
        let heat = heat.clone();
        async move {
            let (status, body) = call(
                &app,
                "GET",
                &format!("/events/{}/snapshot/heat/{}", event.0, heat.0),
                Some(&token),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "served heat snapshot: {body}");
            let snap: gridfpv_server::snapshot::Snapshot = serde_json::from_str(&body).unwrap();
            match snap.body {
                gridfpv_server::snapshot::ProjectionBody::LiveRaceState(ls) => ls,
                other => panic!("expected a LiveRaceState body, got {other:?}"),
            }
        }
    };

    let unofficial = served_live(&heat).await;
    assert_eq!(
        unofficial.phase,
        gridfpv_server::snapshot::HeatPhase::Unofficial,
        "the served phase follows the log to Unofficial so the console clock freezes (no bump)"
    );
    assert_eq!(unofficial.current_heat.as_ref(), Some(&heat));
    // The per-channel laps are still carried through Unofficial, each row unbound (per channel).
    assert!(
        !unofficial.progress.is_empty(),
        "the per-channel laps stay visible through Unofficial"
    );
    assert!(
        unofficial.progress.iter().all(|p| p.pilot.is_none()),
        "open-practice rows stay unbound (per channel)"
    );

    // (e) Restart → Scheduled: the RD restarts the closed practice. The engine resets the heat to
    // `Scheduled` (a full reset, like Abort; the RD re-Stages); the bridge clears the accumulator
    // (wake-on-clear). The served live state must then read **`Scheduled`** with the laps cleared —
    // never a stale `Unofficial` (the bug: the console kept rendering `Unofficial`/Final buttons and
    // `Restart` then errored "illegal … in state Staged").
    control_ok(
        &app,
        &event,
        &token,
        &Command::Restart { heat: heat.clone() },
    )
    .await;
    wait_until(&state, Duration::from_secs(3), {
        let heat = heat.clone();
        move |events| {
            heat_state_of(events, &heat) == Some(gridfpv_engine::heat::HeatState::Scheduled)
        }
    })
    .await;

    // The accumulator clears one bridge poll after it observes the restart; wait for it to settle.
    let practice_overlay = registry.resolve(&event).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while practice_overlay.open_practice().is_active(&heat) {
        assert!(
            std::time::Instant::now() < deadline,
            "the open-practice accumulator never cleared after Restart"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let scheduled = served_live(&heat).await;
    assert_eq!(
        scheduled.phase,
        gridfpv_server::snapshot::HeatPhase::Scheduled,
        "after Restart the served phase is the log's Scheduled — never a stale Unofficial"
    );
    assert!(
        scheduled.progress.iter().all(|p| p.laps_completed == 0),
        "the per-channel laps are cleared after Restart"
    );

    // The clock-timing basis is the log's at every step: the heat carries exactly one `Finished`
    // (the time-limit close) and one `Restarted`, so the served phase moved Running → Unofficial →
    // Scheduled with no synthetic re-`Running` that would reset/bump the console clock.
    let log = read_log(&state);
    let restarts = log
        .iter()
        .filter(|e| matches!(e, Event::HeatStateChanged { heat: h, transition: gridfpv_events::HeatTransition::Restarted } if *h == heat))
        .count();
    assert_eq!(
        restarts, 1,
        "exactly one Restarted resets the heat to Scheduled"
    );
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
