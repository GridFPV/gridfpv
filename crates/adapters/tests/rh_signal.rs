//! Emulated-signal RotorHazard integration test (#27).
//!
//! Unlike `rh_live.rs` (which injects bare laps via `simulate_lap`), this drives a
//! dockerized RotorHazard from **emulated node-output streams** (`mock_data_*.csv`):
//! per-tick RSSI + crossing flags + incrementing lap ids that make RH record laps
//! through its real pipeline. It then verifies the full chain — emulated signal →
//! RH detection/recording → Socket.IO → our adapter → projection — produces correct
//! multi-node laps and signal context.
//!
//! Local-only class (needs Docker). Run via `cargo xtask live` or:
//!
//! ```sh
//! cargo test -p gridfpv-adapters --features live --test rh_signal -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use gridfpv_adapters::rotorhazard::RotorHazardAdapter;
use gridfpv_adapters::rotorhazard::transport::RotorHazardConnection;
use gridfpv_events::Event;
use gridfpv_projection::{LapList, lap_list};
use gridfpv_testkit::{NodeCsv, RhContainer, node_csv};

const PORT: u16 = 5031;
const TICK: &str = "0.1";

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

/// Passes for a given node-seat competitor in the event stream.
fn passes_for<'a>(events: &'a [Event], competitor: &str) -> Vec<&'a gridfpv_events::Pass> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Pass(p) if p.competitor.0 == competitor => Some(p),
            _ => None,
        })
        .collect()
}

fn laps_for<'a>(
    laps: &'a LapList,
    competitor: &str,
) -> Option<&'a gridfpv_projection::CompetitorLaps> {
    laps.competitors
        .iter()
        .find(|c| c.competitor.competitor.0 == competitor)
}

#[test]
#[ignore = "requires Docker (spins up dockerized RotorHazard with emulated signals)"]
fn emulated_signal_multi_node_race() {
    // node 0: fast laps, strong signal; node 1: slower laps, weaker signal;
    // node 2: no CSV mounted => silent (no passes at all).
    let csvs = vec![
        (
            0usize,
            node_csv(&NodeCsv {
                ticks_per_lap: 6,
                peak_rssi: 150,
                baseline_rssi: 70,
            }),
        ),
        (
            1usize,
            node_csv(&NodeCsv {
                ticks_per_lap: 10,
                peak_rssi: 120,
                baseline_rssi: 60,
            }),
        ),
    ];

    let rh = RhContainer::start(PORT, TICK, &csvs);
    let conn = RotorHazardConnection::connect(rh.url(), RotorHazardAdapter::new())
        .expect("connect to RotorHazard");

    let mut events: Vec<Event> = Vec::new();
    std::thread::sleep(Duration::from_secs(2));

    // Clean state, then start a race; the mock CSVs drive the laps (no simulate_lap).
    conn.stop_race().ok();
    conn.discard_laps().expect("discard_laps");
    std::thread::sleep(Duration::from_secs(2));
    let _ = conn.events();

    conn.stage_race().expect("stage_race");
    assert!(
        wait_until(&conn, &mut events, Duration::from_secs(20), |evs| {
            evs.iter()
                .any(|e| matches!(e, Event::SessionStarted { .. }))
        }),
        "race never reached RACING"
    );

    // Let the emulated signals produce laps on both nodes.
    let both_have_laps = wait_until(&conn, &mut events, Duration::from_secs(25), |evs| {
        let l = lap_list(evs);
        laps_for(&l, "node-0").map(|c| c.lap_count()).unwrap_or(0) >= 1
            && laps_for(&l, "node-1").map(|c| c.lap_count()).unwrap_or(0) >= 1
    });

    conn.stop_race().ok();
    std::thread::sleep(Duration::from_millis(800));
    events.extend(conn.events());
    conn.disconnect().ok();

    assert!(
        both_have_laps,
        "both nodes should complete laps from emulated signals"
    );

    // --- structure ---
    let laps = lap_list(&events);
    let competitors: Vec<&str> = laps
        .competitors
        .iter()
        .map(|c| c.competitor.competitor.0.as_str())
        .collect();
    assert!(competitors.contains(&"node-0"), "node-0 present");
    assert!(competitors.contains(&"node-1"), "node-1 present");
    assert!(
        !competitors.contains(&"node-2"),
        "node-2 had no CSV and must be silent, got {competitors:?}"
    );

    let node0 = laps_for(&laps, "node-0").unwrap();
    let node1 = laps_for(&laps, "node-1").unwrap();
    assert!(node0.lap_count() >= 1 && node1.lap_count() >= 1);
    assert!(
        node0.laps.iter().all(|l| l.duration_micros > 0),
        "lap durations must be positive"
    );

    // --- dedup: lap numbers strictly increasing (a re-sent snapshot must not dup) ---
    for node in ["node-0", "node-1"] {
        let nums: Vec<u32> = laps_for(&laps, node)
            .unwrap()
            .laps
            .iter()
            .map(|l| l.number)
            .collect();
        let mut sorted = nums.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            nums, sorted,
            "{node} lap numbers must be strictly increasing/unique: {nums:?}"
        );
    }

    // --- signal context: emulated peak RSSI flows through node_data into passes ---
    let n0_passes = passes_for(&events, "node-0");
    assert!(
        n0_passes.iter().any(|p| p
            .signal
            .and_then(|s| s.rssi_peak)
            .is_some_and(|r| r >= 140.0)),
        "node-0 passes should carry strong signal context (~150)"
    );
    let n1_passes = passes_for(&events, "node-1");
    assert!(
        n1_passes.iter().any(|p| p
            .signal
            .and_then(|s| s.rssi_peak)
            .is_some_and(|r| (100.0..140.0).contains(&r))),
        "node-1 passes should carry weaker signal context (~120)"
    );

    println!(
        "emulated-signal race: node-0 {} laps, node-1 {} laps; n0 signalled passes ok",
        node0.lap_count(),
        node1.lap_count()
    );
}
