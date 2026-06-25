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
use gridfpv_projection::{LapList, lap_list, signal_trace};
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

    // Disable RH's lap minimum for the sim: the mock CSVs drive short laps that would otherwise
    // trip RH's 10s default `MIN_LAP_TIME` ("Pass record under lap minimum (10)"). `0` lets every
    // emulated-signal crossing record cleanly.
    conn.set_min_lap_time(0).ok();

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
    conn.disconnect();

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

/// The **fidelity gate** (marshaling Slice 1): capture the RSSI trace + enter/exit thresholds from a
/// dockerized RotorHazard driven by an emulated node-output stream (`NodeCsv`), and assert the
/// captured trace matches the RSSI values that CSV actually reports — no quantization or dropout
/// within what RotorHazard exposes live.
///
/// The CSV drives a known, stable `peak_rssi` (reported every tick as the node peak) and a
/// `baseline_rssi`. RotorHazard re-emits `node_data` on its heartbeat while racing, so the adapter
/// captures one trace sample per emit; every captured sample must be one of the values the CSV
/// reports (the node peak), proving the capture is faithful to the source. Thresholds (RH's
/// `enter_at`/`exit_at`) are captured from `enter_and_exit_at_levels`.
///
/// NB: this is the value RH *streams* live (one aggregate `node_peak_rssi` per emit), which is the
/// documented fidelity bound — coarser than the detector's internal per-tick history (that lives in
/// the request-driven `current_marshal_data`, which a live translator does not subscribe to). The
/// thing for the maintainer to confirm on real hardware is whether this streamed cadence is dense
/// enough for the evidence layer, or whether the request-driven history must be pulled at heat end.
#[test]
#[ignore = "requires Docker (spins up dockerized RotorHazard with emulated signals)"]
fn captured_trace_matches_emitted_csv_samples() {
    const PEAK: i32 = 150;
    const BASELINE: i32 = 70;
    // One node, fast laps, a known stable peak — `node_csv` reports `peak_rssi` as the node peak on
    // every tick and `node_peak = peak + 30`.
    let csvs = vec![(
        0usize,
        node_csv(&NodeCsv {
            ticks_per_lap: 6,
            peak_rssi: PEAK,
            baseline_rssi: BASELINE,
        }),
    )];

    let rh = RhContainer::start(PORT + 1, TICK, &csvs);
    let conn = RotorHazardConnection::connect(rh.url(), RotorHazardAdapter::new())
        .expect("connect to RotorHazard");

    let mut events: Vec<Event> = Vec::new();
    std::thread::sleep(Duration::from_secs(2));
    conn.set_min_lap_time(0).ok();
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

    // Let the heartbeat stream a run of node_data ticks so the trace accumulates samples.
    let captured_enough = wait_until(&conn, &mut events, Duration::from_secs(20), |evs| {
        signal_trace(evs)
            .competitor(&gridfpv_projection::CompetitorKey {
                adapter: gridfpv_events::AdapterId("rotorhazard".into()),
                competitor: gridfpv_events::CompetitorRef("node-0".into()),
            })
            .map(|t| t.samples.len() >= 5)
            .unwrap_or(false)
    });

    conn.stop_race().ok();
    std::thread::sleep(Duration::from_millis(800));
    events.extend(conn.events());
    conn.disconnect();

    assert!(captured_enough, "node-0 never accumulated a trace");

    let view = signal_trace(&events);
    let trace = view
        .competitor(&gridfpv_projection::CompetitorKey {
            adapter: gridfpv_events::AdapterId("rotorhazard".into()),
            competitor: gridfpv_events::CompetitorRef("node-0".into()),
        })
        .expect("node-0 trace present");

    // Fidelity: every captured sample is a value the CSV actually reports for this node — the node
    // peak (PEAK). RotorHazard's mock node holds `node_peak_rssi` at the CSV's reported peak, so a
    // faithful capture reproduces it exactly with no quantization within the u16 range.
    assert!(!trace.samples.is_empty(), "trace has samples");
    for &s in &trace.samples {
        assert_eq!(
            s, PEAK as u16,
            "captured sample {s} must equal the CSV-reported node peak {PEAK} (no quantization); \
             full trace: {:?}",
            trace.samples
        );
    }
    // The capture cadence is the configured period; the time base anchors at 0 for the heat.
    assert!(trace.period_micros > 0);
    assert_eq!(trace.from, Some(gridfpv_events::SourceTime::from_micros(0)));

    // Thresholds: RotorHazard's enter/exit levels were captured from `enter_and_exit_at_levels`.
    // (The mock profile carries the stock defaults; we assert they were captured, not their exact
    // value, since the profile default can vary by RH build.)
    assert!(
        trace.enter.is_some() && trace.exit.is_some(),
        "enter/exit thresholds must be captured, got enter={:?} exit={:?}",
        trace.enter,
        trace.exit
    );

    println!(
        "fidelity: node-0 captured {} samples (all == {PEAK}); thresholds enter={:?} exit={:?}",
        trace.samples.len(),
        trace.enter,
        trace.exit
    );
}
