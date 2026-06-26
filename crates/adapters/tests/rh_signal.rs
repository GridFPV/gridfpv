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

    // Re-request thresholds AFTER racing starts: the RACING transition clears the adapter's
    // threshold dedup, and the connect-time capture was discarded by the drain above, so re-ask now
    // so the (unchanged-value) `SignalThresholds` are re-emitted and captured into `events`.
    conn.request_thresholds().ok();

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

    // Fidelity: every captured sample is a value the CSV actually reports for this node. The coarse
    // trace samples `node_peak_rssi`, which `node_csv` holds at `peak + 30` on every tick, so a
    // faithful capture reproduces THAT value exactly with no quantization within the u16 range.
    const NODE_PEAK: i32 = PEAK + 30;
    assert!(!trace.samples.is_empty(), "trace has samples");
    for &s in &trace.samples {
        assert_eq!(
            s, NODE_PEAK as u16,
            "captured sample {s} must equal the CSV-reported node peak {NODE_PEAK} (no \
             quantization); full trace: {:?}",
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
        "fidelity: node-0 captured {} samples (all == {NODE_PEAK}); thresholds enter={:?} exit={:?}",
        trace.samples.len(),
        trace.enter,
        trace.exit
    );
}

/// The **dense-history** gate (RH path 2 — `current_marshal_data`): after a heat ends, the adapter
/// pulls RotorHazard's request-driven dense marshal data and the `signal_trace` projection prefers
/// it over the coarse streamed samples. This asserts the dense trace carries **strictly more**
/// samples than the coarse streaming count captured live — the full-fidelity upgrade.
///
/// Flow: drive an emulated node stream into a real dockerized RH, count the **coarse** streamed
/// samples accumulated while racing, finish the heat (which auto-triggers `current_race_marshal`),
/// then re-fold and assert the now-dense trace has many more samples (RH's per-tick history vs. one
/// sample per `node_data` heartbeat). The auto-request fires on the DONE transition; we also call
/// `request_marshal_data()` explicitly as a belt-and-braces re-request in case the heat ended before
/// the live socket drained the DONE.
#[test]
#[ignore = "requires Docker (spins up dockerized RotorHazard with emulated signals)"]
fn dense_marshal_history_supersedes_coarse_stream() {
    const PEAK: i32 = 150;
    const BASELINE: i32 = 70;
    let csvs = vec![(
        0usize,
        node_csv(&NodeCsv {
            ticks_per_lap: 6,
            peak_rssi: PEAK,
            baseline_rssi: BASELINE,
        }),
    )];

    let rh = RhContainer::start(PORT + 2, TICK, &csvs);
    let conn = RotorHazardConnection::connect(rh.url(), RotorHazardAdapter::new())
        .expect("connect to RotorHazard");

    let key = gridfpv_projection::CompetitorKey {
        adapter: gridfpv_events::AdapterId("rotorhazard".into()),
        competitor: gridfpv_events::CompetitorRef("node-0".into()),
    };

    let mut events: Vec<Event> = Vec::new();
    std::thread::sleep(Duration::from_secs(2));
    conn.set_min_lap_time(0).ok();
    conn.stop_race().ok();
    conn.discard_laps().expect("discard_laps");
    // RotorHazard only persists (and thus exposes) a race's dense history for a SAVED heat — its
    // `current_heat` is None in default practice mode, where `save_laps` is a no-op. Create and
    // select a heat so the post-race dense history is pullable (the production staging path selects
    // an RH heat for the same reason).
    conn.add_heat().ok();
    std::thread::sleep(Duration::from_millis(500));
    conn.set_current_heat(1).ok();
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

    // Let a run of coarse streamed samples accumulate.
    let captured_enough = wait_until(&conn, &mut events, Duration::from_secs(20), |evs| {
        signal_trace(evs)
            .competitor(&key)
            .map(|t| t.samples.len() >= 5)
            .unwrap_or(false)
    });
    assert!(captured_enough, "node-0 never accumulated a coarse trace");

    // The coarse streamed sample count, before any dense history is pulled.
    let coarse_samples = signal_trace(&events)
        .competitor(&key)
        .map(|t| t.samples.len())
        .unwrap_or(0);

    // Finish the heat: the DONE transition auto-triggers the heat-end marshal pull (the transport
    // emits `current_race_marshal`, then `save_laps` + a `race_list` request whose ids drive
    // `get_pilotrace` — whichever the server implements answers).
    conn.stop_race().ok();

    // Wait for the dense `SignalHistory` to arrive and supersede the coarse stream. The transport
    // drives the pull automatically off the DONE edge; keep draining while it completes.
    let got_dense = wait_until(&conn, &mut events, Duration::from_secs(20), |evs| {
        evs.iter().any(|e| matches!(e, Event::SignalHistory(_)))
    });

    events.extend(conn.events());
    conn.disconnect();

    assert!(
        got_dense,
        "no SignalHistory pulled after heat end (coarse samples were {coarse_samples})"
    );

    let dense_samples = signal_trace(&events)
        .competitor(&key)
        .map(|t| t.samples.len())
        .expect("node-0 dense trace");

    assert!(
        dense_samples > coarse_samples,
        "dense history must carry MORE samples than the coarse stream: dense={dense_samples} \
         coarse={coarse_samples}"
    );

    println!(
        "dense marshal history: coarse stream = {coarse_samples} samples, dense history = \
         {dense_samples} samples (full-fidelity upgrade)"
    );
}
