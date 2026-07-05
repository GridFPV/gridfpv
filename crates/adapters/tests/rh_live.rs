//! Live integration test against dockerized RotorHazard (#25).
//!
//! Drives a real race on a running RotorHazard (the mock-node container in
//! `docker/rotorhazard/`) over Socket.IO and asserts the adapter translates the
//! live stream into correct laps — the "real timing in" half of v0.2's done-when.
//!
//! Gated behind the `live` feature AND `#[ignore]`, so it never runs in the
//! default `cargo test` or the shared CI pipeline. It's a **local** class that
//! manages its own disposable RotorHazard container (Docker required). Run via:
//!
//! ```sh
//! cargo xtask live
//! # or just this test:
//! cargo test -p gridfpv-adapters --features live --test rh_live -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use gridfpv_adapters::rotorhazard::RotorHazardAdapter;
use gridfpv_adapters::rotorhazard::transport::RotorHazardConnection;
use gridfpv_events::Event;
use gridfpv_projection::lap_list;
use gridfpv_testkit::RhContainer;

/// Port for this test's disposable RotorHazard (distinct from rh_signal's).
const PORT: u16 = 5030;

fn event_kind(e: &Event) -> &'static str {
    match e {
        Event::AdapterConnected { .. } => "AdapterConnected",
        Event::AdapterDisconnected { .. } => "AdapterDisconnected",
        Event::SessionStarted { .. } => "SessionStarted",
        Event::SessionEnded { .. } => "SessionEnded",
        Event::CompetitorSeen { .. } => "CompetitorSeen",
        Event::CompetitorRegistered { .. } => "CompetitorRegistered",
        Event::Pass(_) => "Pass",
        Event::SignalChunk(_) => "SignalChunk",
        Event::SignalThresholds(_) => "SignalThresholds",
        Event::SignalHistory(_) => "SignalHistory",
        Event::HeatScheduled { .. } => "HeatScheduled",
        Event::HeatStateChanged { .. } => "HeatStateChanged",
        Event::CurrentHeatSelected { .. } => "CurrentHeatSelected",
        Event::HeatStarting { .. } => "HeatStarting",
        Event::HeatFinalizing { .. } => "HeatFinalizing",
        Event::DetectionVoided { .. } => "DetectionVoided",
        Event::LapInserted { .. } => "LapInserted",
        Event::LapAdjusted { .. } => "LapAdjusted",
        Event::LapSplit { .. } => "LapSplit",
        Event::LapThrownOut { .. } => "LapThrownOut",
        Event::HeatVoided { .. } => "HeatVoided",
        Event::PenaltyApplied { .. } => "PenaltyApplied",
        Event::ProtestFiled { .. } => "ProtestFiled",
        Event::ProtestResolved { .. } => "ProtestResolved",
        Event::RulingReversed { .. } => "RulingReversed",
        Event::RoundFieldDrawn { .. } => "RoundFieldDrawn",
    }
}

/// Drain events until `pred` is satisfied or `timeout` elapses; returns whether it
/// was satisfied. Accumulates everything drained into `sink`.
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

#[test]
#[ignore = "requires a running dockerized RotorHazard (docker/rotorhazard/)"]
fn live_rotorhazard_race_translates_to_laps() {
    // Disposable RotorHazard with mock nodes (no CSVs); driven via `simulate_lap`.
    let rh = RhContainer::start(PORT, "0.5", &[]);
    let conn = RotorHazardConnection::connect(rh.url(), RotorHazardAdapter::new())
        .expect("connect to RotorHazard");

    let mut events: Vec<Event> = Vec::new();

    // Let the connection settle and the server register us before driving it.
    std::thread::sleep(Duration::from_secs(2));

    // Disable RH's lap minimum for the sim: our rapid `simulate_lap` injections are far closer
    // together than RH's 10s default `MIN_LAP_TIME`, which otherwise logs "Pass record under lap
    // minimum (10)" for every short lap. `0` lets every sim lap record cleanly.
    conn.set_min_lap_time(0).ok();

    // Reset to a clean READY state (a prior DONE race blocks staging), then ignore
    // any events that reset produced — we only assert on the fresh race below.
    conn.stop_race().ok();
    conn.discard_laps().expect("emit discard_laps");
    std::thread::sleep(Duration::from_secs(2));
    let _ = conn.events();

    // Stage the race and wait for it to actually start (RACING -> SessionStarted).
    conn.stage_race().expect("emit stage_race");
    let started = wait_until(&conn, &mut events, Duration::from_secs(20), |evs| {
        evs.iter()
            .any(|e| matches!(e, Event::SessionStarted { .. }))
    });
    if !started {
        eprintln!(
            "debug: {} events so far: {:?}",
            events.len(),
            events.iter().map(event_kind).collect::<Vec<_>>()
        );
    }
    assert!(started, "race never reached RACING (no SessionStarted)");

    // Three crossings on node 0 (=> 2 laps) and one on node 1 (=> 0 laps).
    for node in [0u64, 0, 1, 0] {
        conn.simulate_lap(node).expect("emit simulate_lap");
        std::thread::sleep(Duration::from_millis(1200));
        events.extend(conn.events());
    }

    // Wait until node-0 has accumulated at least two passes (one full lap).
    let got_laps = wait_until(&conn, &mut events, Duration::from_secs(10), |evs| {
        lap_list(evs)
            .competitors
            .iter()
            .any(|c| c.competitor.competitor.0 == "node-0" && c.lap_count() >= 1)
    });

    conn.stop_race().expect("emit stop_race");
    std::thread::sleep(Duration::from_millis(800));
    events.extend(conn.events());

    // Second heat on the SAME persistent connection/adapter (#105 cross-heat regression).
    // RotorHazard resets lap_number to 0 at each race start; without resetting the per-lap
    // dedup on the RACING transition, heat 2's laps would collide with heat 1's and be
    // suppressed (zero laps ingested past the first heat). Reset, re-stage, and assert
    // heat 2 produces its own fresh laps.
    conn.discard_laps().expect("emit discard_laps (heat 2)");
    std::thread::sleep(Duration::from_secs(2));
    let _ = conn.events();
    let mut heat2: Vec<Event> = Vec::new();

    conn.stage_race().expect("emit stage_race (heat 2)");
    let started2 = wait_until(&conn, &mut heat2, Duration::from_secs(20), |evs| {
        evs.iter()
            .any(|e| matches!(e, Event::SessionStarted { .. }))
    });
    assert!(started2, "heat 2 never reached RACING (no SessionStarted)");

    for node in [0u64, 0, 1, 0] {
        conn.simulate_lap(node).expect("emit simulate_lap (heat 2)");
        std::thread::sleep(Duration::from_millis(1200));
        heat2.extend(conn.events());
    }
    let got_laps2 = wait_until(&conn, &mut heat2, Duration::from_secs(10), |evs| {
        lap_list(evs)
            .competitors
            .iter()
            .any(|c| c.competitor.competitor.0 == "node-0" && c.lap_count() >= 1)
    });

    conn.stop_race().expect("emit stop_race (heat 2)");
    std::thread::sleep(Duration::from_millis(800));
    heat2.extend(conn.events());
    conn.disconnect();

    assert!(
        got_laps2,
        "heat 2 ingested no completed lap for node-0 \
         (cross-heat dedup regression: lap_number reset collided with heat 1)"
    );
    let laps2 = lap_list(&heat2);
    let node0_h2 = laps2
        .competitors
        .iter()
        .find(|c| c.competitor.competitor.0 == "node-0")
        .expect("node-0 present in heat 2 lap list");
    assert!(
        node0_h2.lap_count() >= 1,
        "expected >= 1 lap for node-0 in heat 2, got {}",
        node0_h2.lap_count()
    );

    // The live stream produced lifecycle + competitor sightings + real laps.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::SessionStarted { .. })),
        "expected a SessionStarted"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::CompetitorSeen { .. })),
        "expected at least one CompetitorSeen"
    );
    assert!(got_laps, "node-0 never produced a completed lap");

    let laps = lap_list(&events);
    let node0 = laps
        .competitors
        .iter()
        .find(|c| c.competitor.competitor.0 == "node-0")
        .expect("node-0 present in lap list");
    assert!(
        node0.lap_count() >= 1,
        "expected >= 1 lap for node-0, got {}",
        node0.lap_count()
    );
    assert!(
        node0.laps.iter().all(|l| l.duration_micros > 0),
        "lap durations must be positive (source-clock derived)"
    );
    println!(
        "live RH: node-0 completed {} lap(s): {:?}",
        node0.lap_count(),
        node0.laps
    );
}

/// Mid-race reconnect must not double-count (#105). Drives a real race, accumulates laps, then
/// **disconnects mid-race and reconnects reusing the same adapter** (the persisted-adapter fix:
/// `disconnect` returns the adapter, the next `connect` takes it). RotorHazard re-sends the full
/// in-progress `current_laps` snapshot on the new socket; because the carried adapter's dedup
/// already holds those laps, the lap count after the reconnect must equal the count before it (no
/// duplicates). Pre-fix, a fresh adapter on reconnect re-emitted every in-progress lap and the lap
/// projection (no sequence dedup) turned them into duplicate laps.
#[test]
#[ignore = "requires a running dockerized RotorHazard (docker/rotorhazard/)"]
fn mid_race_reconnect_does_not_double_count_laps() {
    let rh = RhContainer::start(PORT + 1, "0.5", &[]);
    let conn = RotorHazardConnection::connect(rh.url(), RotorHazardAdapter::new())
        .expect("connect to RotorHazard");

    let mut events: Vec<Event> = Vec::new();
    std::thread::sleep(Duration::from_secs(2));

    // Disable RH's lap minimum for the sim (see the first test) so the short `simulate_lap`
    // crossings below don't trip "Pass record under lap minimum (10)".
    conn.set_min_lap_time(0).ok();

    conn.stop_race().ok();
    conn.discard_laps().expect("emit discard_laps");
    std::thread::sleep(Duration::from_secs(2));
    let _ = conn.events();

    conn.stage_race().expect("emit stage_race");
    assert!(
        wait_until(&conn, &mut events, Duration::from_secs(20), |evs| {
            evs.iter()
                .any(|e| matches!(e, Event::SessionStarted { .. }))
        }),
        "race never reached RACING (no SessionStarted)"
    );

    // Drive several crossings on node 0 so there are in-progress laps to replay.
    for node in [0u64, 0, 0, 0] {
        conn.simulate_lap(node).expect("emit simulate_lap");
        std::thread::sleep(Duration::from_millis(1200));
        events.extend(conn.events());
    }
    assert!(
        wait_until(&conn, &mut events, Duration::from_secs(10), |evs| {
            lap_list(evs)
                .competitors
                .iter()
                .any(|c| c.competitor.competitor.0 == "node-0" && c.lap_count() >= 1)
        }),
        "node-0 never produced a completed lap before the reconnect"
    );
    let laps_before = lap_list(&events)
        .competitors
        .iter()
        .find(|c| c.competitor.competitor.0 == "node-0")
        .map(|c| c.lap_count())
        .unwrap_or(0);
    assert!(laps_before >= 1, "need at least one lap before reconnect");

    // Simulate a mid-race drop+reconnect: recover the adapter from `disconnect` and feed it into a
    // fresh socket. The race keeps RUNNING server-side, so RH replays the in-progress snapshot.
    let adapter = conn.disconnect();
    std::thread::sleep(Duration::from_millis(500));
    let conn = RotorHazardConnection::connect(rh.url(), adapter)
        .expect("reconnect to RotorHazard reusing the persisted adapter");

    // Drain the replayed snapshot the reconnect triggers; give it time to arrive.
    let mut after: Vec<Event> = Vec::new();
    wait_until(&conn, &mut after, Duration::from_secs(5), |_| false);
    events.extend(after);

    conn.stop_race().ok();
    std::thread::sleep(Duration::from_millis(800));
    events.extend(conn.events());
    conn.disconnect();

    let laps_after = lap_list(&events)
        .competitors
        .iter()
        .find(|c| c.competitor.competitor.0 == "node-0")
        .map(|c| c.lap_count())
        .unwrap_or(0);
    assert_eq!(
        laps_after, laps_before,
        "mid-race reconnect double-counted laps: {laps_before} before, {laps_after} after \
         (the replayed in-progress snapshot was not deduped — the persisted-adapter fix regressed)"
    );
    println!("live RH reconnect: node-0 stable at {laps_after} lap(s) across the reconnect");
}
