//! Live integration test against dockerized RotorHazard (#25).
//!
//! Drives a real race on a running RotorHazard (the mock-node container in
//! `docker/rotorhazard/`) over Socket.IO and asserts the adapter translates the
//! live stream into correct laps — the "real timing in" half of v0.2's done-when.
//!
//! Gated behind the `live` feature AND `#[ignore]`, so it never runs in the
//! default `cargo test` or the shared CI pipeline. It's a **local** class —
//! the one-command runner brings up dockerized RH, runs this, and tears it down:
//!
//! ```sh
//! cargo xtask live
//! ```
//!
//! Or manually against an already-running container:
//!
//! ```sh
//! docker compose -f docker/rotorhazard/docker-compose.yml up -d --wait
//! cargo test -p gridfpv-adapters --features live --test rh_live -- --ignored --nocapture
//! ```
//!
//! `RH_URL` overrides the server (defaults to `http://localhost:5000`).
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use gridfpv_adapters::rotorhazard::RotorHazardAdapter;
use gridfpv_adapters::rotorhazard::transport::RotorHazardConnection;
use gridfpv_events::Event;
use gridfpv_projection::lap_list;

fn rh_url() -> String {
    std::env::var("RH_URL").unwrap_or_else(|_| "http://localhost:5000".to_string())
}

fn event_kind(e: &Event) -> &'static str {
    match e {
        Event::AdapterConnected { .. } => "AdapterConnected",
        Event::AdapterDisconnected { .. } => "AdapterDisconnected",
        Event::SessionStarted { .. } => "SessionStarted",
        Event::SessionEnded { .. } => "SessionEnded",
        Event::CompetitorSeen { .. } => "CompetitorSeen",
        Event::Pass(_) => "Pass",
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
    let conn = RotorHazardConnection::connect(&rh_url(), RotorHazardAdapter::new())
        .expect("connect to RotorHazard");

    let mut events: Vec<Event> = Vec::new();

    // Let the connection settle and the server register us before driving it.
    std::thread::sleep(Duration::from_secs(2));

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
    conn.disconnect().ok();

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
