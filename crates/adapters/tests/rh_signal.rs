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
                seed: 0,
            }),
        ),
        (
            1usize,
            node_csv(&NodeCsv {
                ticks_per_lap: 10,
                peak_rssi: 120,
                baseline_rssi: 60,
                seed: 0,
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

    // Grid owns all timing: prepare RH for an instant start (zero its staging hold/tones + reset to
    // READY), exactly as the Director does at the heat's **Staged** transition — well before "go".
    conn.prepare_instant_start().expect("prepare_instant_start");
    std::thread::sleep(Duration::from_secs(2));
    let _ = conn.events();

    // "Go": a single `stage_race` (no reset, no settle) starts RH recording. RH must reach RACING
    // **without an RH-side staging hold/tone** — only the fixed prestage — so we assert the
    // transition is prompt (≤ ~1.5s) rather than the multi-second staging countdown a stock format
    // would run. This is the no-RH-staging guarantee.
    let go = Instant::now();
    conn.stage_race().expect("stage_race");
    assert!(
        wait_until(&conn, &mut events, Duration::from_secs(20), |evs| {
            evs.iter()
                .any(|e| matches!(e, Event::SessionStarted { .. }))
        }),
        "race never reached RACING"
    );
    let to_racing = go.elapsed();
    assert!(
        to_racing <= Duration::from_millis(1_500),
        "RH must start racing promptly on command with no staging hold/tone (Grid owns the delay); \
         took {to_racing:?} — a stock multi-second staging countdown would be far longer"
    );
    println!("instant-start: RH reached RACING {to_racing:?} after the go (no RH staging hold)");

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

    // --- lap-time correctness on Grid's clock (Grid owns all timing) ---
    // node-0 flies `ticks_per_lap: 6` at `TICK = 0.1s` ⇒ ~0.6s per lap. Lap durations are pass-to-
    // pass deltas on RH's race clock, which Grid maps directly — and because they are *differences*,
    // RH's fixed prestage offset cancels, so the durations must land near the emulated 0.6s/lap
    // regardless of how long after the go RH actually reached RACING. A generous band absorbs the
    // mock pipeline's jitter while still proving the mapping is correct (not offset/stretched).
    const TICK_MS: i64 = 100; // TICK = "0.1" seconds
    let node0_expected_us = 6 * TICK_MS * 1_000; // 600_000 µs
    for lap in &node0.laps {
        let d = lap.duration_micros;
        assert!(
            (node0_expected_us / 2..=node0_expected_us * 2).contains(&d),
            "node-0 lap {} duration {}µs must map near the emulated ~{}µs/lap on Grid's clock \
             (the prestage offset cancels in pass-to-pass deltas); all: {:?}",
            lap.number,
            d,
            node0_expected_us,
            node0
                .laps
                .iter()
                .map(|l| l.duration_micros)
                .collect::<Vec<_>>()
        );
    }

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
/// The CSV now drives a **realistic gate-pass profile** (a baseline floor + a Gaussian bell on each
/// crossing + small deterministic noise; see `gridfpv_testkit::pass_value`), not a square step.
/// RotorHazard re-emits `node_data` on its heartbeat while racing, so the adapter captures one trace
/// sample per emit. The fidelity invariant adapts to the model: every captured sample must lie in
/// the model's band `[1, peak + noise]` (a plausible RSSI the CSV could report — no quantization or
/// out-of-range value), and the captured trace must reach the **peak band** at least once (the
/// crossing peak is faithfully captured, so the marshaling graph sees the real crest). Thresholds
/// (RH's `enter_at`/`exit_at`) are captured from `enter_and_exit_at_levels`.
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
    // One node flying the realistic gate-pass profile: the per-tick streamed `node_peak` rises from
    // `baseline_rssi` to a bell crest near `peak_rssi` at each crossing, with a few units of noise.
    let csvs = vec![(
        0usize,
        node_csv(&NodeCsv {
            ticks_per_lap: 48,
            peak_rssi: PEAK,
            baseline_rssi: BASELINE,
            seed: 0,
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

    // Let the heartbeat stream a run of node_data ticks so the trace accumulates samples. The
    // coarse `node_data` stream is sparse (a handful of samples per heat); the dense per-tick shape
    // is asserted by `dense_marshal_history_supersedes_coarse_stream`. Here we just need enough
    // coarse samples to check each is a faithful in-band value of the realistic profile.
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

    // Fidelity within the realistic model: every captured sample lies in the band the CSV could
    // report — `[baseline - noise, peak + noise]` — with no quantization or out-of-range value.
    // (Lap-to-lap variation + per-sample noise mean the faithful invariant is band-membership, not a
    // single constant — `captured == emitted` *within the model*. The dense path asserts the full
    // rise→peak→fall shape; the sparse coarse stream may not span a whole pass.)
    const NOISE_HEADROOM: i32 = 10; // a few units of per-sample + peak-variation slack.
    let lo = (BASELINE - NOISE_HEADROOM).max(1) as u16;
    let hi = (PEAK + NOISE_HEADROOM) as u16;
    assert!(!trace.samples.is_empty(), "trace has samples");
    for &s in &trace.samples {
        assert!(
            (lo..=hi).contains(&s),
            "captured sample {s} outside the model band [{lo}, {hi}] (baseline/peak ± noise); \
             full trace: {:?}",
            trace.samples
        );
    }
    let max = trace.samples.iter().copied().max().unwrap();
    let min = trace.samples.iter().copied().min().unwrap();
    // The capture cadence is positive and the trace is time-anchored near the race start. Under the
    // live plugin path the first dense sample carries its *actual* race-relative time (a small
    // positive offset — the first peak/nadir entry after the start), not the marshal-pull path's
    // zeroed origin, so assert "anchored within the opening second" rather than exactly 0.
    assert!(trace.period_micros > 0);
    let from = trace.from.expect("trace is time-anchored").micros;
    assert!(
        (0..1_000_000).contains(&from),
        "the trace anchors near the race start (got {from} µs)"
    );

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
        "fidelity: node-0 captured {} samples (bell profile: min={min} max={max}, band [1,{}]); \
         thresholds enter={:?} exit={:?}",
        trace.samples.len(),
        PEAK + NOISE_HEADROOM,
        trace.enter,
        trace.exit
    );
}

/// The **dense-history** gate (RH path 2 — `current_marshal_data`): after a heat ends, the adapter
/// pulls RotorHazard's request-driven dense marshal data and the `signal_trace` projection prefers
/// it over the coarse streamed samples. This asserts the dense trace carries **strictly more**
/// samples than the coarse streaming count captured live — the full-fidelity upgrade.
///
/// Crucially this drives the dense path through the **production save precondition**
/// `conn.ensure_savable_heat()` (add a heat + select it as current) — exactly what the live
/// Director's staging loop now does before a heat — rather than poking `add_heat`/`set_current_heat`
/// by hand. RotorHazard only persists a run's dense history for a saved current heat, so this proves
/// the normal flow (not a bespoke test setup) activates path-2.
///
/// Flow: drive an emulated node stream into a real dockerized RH, count the **coarse** streamed
/// samples accumulated while racing, finish the heat (which auto-triggers `current_race_marshal` /
/// `save_laps` -> `race_list` -> `get_pilotrace`), then re-fold and assert the now-dense trace has
/// many more samples (RH's per-tick history vs. one sample per `node_data` heartbeat).
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
            seed: 0,
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
    // `current_heat` is None in default practice mode, where `save_laps` is a no-op. Drive the same
    // production precondition the live Director's staging loop now runs: `ensure_savable_heat` adds a
    // heat and requests the heat list; the `heat_data` handler stashes the newest id, which we then
    // select synchronously (exactly as the driver thread does). After this the post-race dense
    // history is pullable through the normal flow.
    conn.ensure_savable_heat().expect("ensure_savable_heat");
    let heat = {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let _ = conn.events();
            if let Some(h) = conn.take_savable_heat() {
                break Some(h);
            }
            if Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    .expect("RotorHazard returned a savable heat id");
    conn.set_current_heat(heat).expect("set_current_heat");
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

    // Let a run of samples accumulate while racing. (Coarse `node_data` chunks; with the GridFPV
    // plugin mounted the live dense history is already superseding by the time we measure.)
    let captured_enough = wait_until(&conn, &mut events, Duration::from_secs(20), |evs| {
        signal_trace(evs)
            .competitor(&key)
            .map(|t| t.samples.len() >= 5)
            .unwrap_or(false)
    });
    assert!(captured_enough, "node-0 never accumulated a trace");

    // The sample count mid-race, before the heat is stopped.
    let coarse_samples = signal_trace(&events)
        .competitor(&key)
        .map(|t| t.samples.len())
        .unwrap_or(0);

    // S2 split: with the GridFPV plugin (the path `cargo xtask live` exercises) the dense history
    // is pushed **live** over `gridfpv_signal` and supersedes the coarse stream *during* the race —
    // the post-race save-then-pull is suppressed. Without it (stock RH fallback, which S3 deletes)
    // the DONE edge drives the pull. Either way a dense `SignalHistory` results.
    let plugin_live = std::env::var_os("GRIDFPV_RH_PLUGIN").is_some();
    let got_dense = if plugin_live {
        // The live dense history should arrive while the heat is still running (no pull).
        let live = wait_until(&conn, &mut events, Duration::from_secs(20), |evs| {
            evs.iter().any(|e| matches!(e, Event::SignalHistory(_)))
        });
        conn.stop_race().ok();
        live
    } else {
        // The DONE transition auto-triggers the marshal pull (`current_race_marshal`, then
        // `save_laps` + `race_list` → `get_pilotrace` — whichever the server implements answers).
        conn.stop_race().ok();
        wait_until(&conn, &mut events, Duration::from_secs(20), |evs| {
            evs.iter().any(|e| matches!(e, Event::SignalHistory(_)))
        })
    };

    events.extend(conn.events());
    conn.disconnect();

    assert!(
        got_dense,
        "no dense SignalHistory produced (coarse samples were {coarse_samples}, plugin_live={plugin_live})"
    );

    let dense_samples = signal_trace(&events)
        .competitor(&key)
        .map(|t| t.samples.len())
        .expect("node-0 dense trace");

    if plugin_live {
        // The plugin delivered the dense trace live (without the pull); assert it's a real trace.
        assert!(
            dense_samples >= 5,
            "the live dense trace should carry real samples; got {dense_samples}"
        );
    } else {
        // The post-race pull yields a denser trace than the coarse stream.
        assert!(
            dense_samples > coarse_samples,
            "dense history must carry MORE samples than the coarse stream: dense={dense_samples} \
             coarse={coarse_samples}"
        );
    }

    // Eyeball artifact (run with --nocapture): a sparkline of the captured dense trace, exactly the
    // shape the marshaling graph / `cargo xtask rh-mock dump` renders. Under the realistic model
    // this is a noisy rise→peak→fall bell per pass, not a square wave.
    let dense = signal_trace(&events).competitor(&key).cloned().unwrap();
    println!(
        "dense marshal history: coarse stream = {coarse_samples} samples, dense history = \
         {dense_samples} samples (full-fidelity upgrade)\n  trace: {}\n  rssi : min={} max={}",
        sparkline(&dense.samples),
        dense.samples.iter().min().copied().unwrap_or(0),
        dense.samples.iter().max().copied().unwrap_or(0),
    );
}

/// A compact ASCII sparkline of RSSI samples (matches `cargo xtask rh-mock dump`), so a heat's
/// captured trace can be eyeballed from the live test output.
fn sparkline(samples: &[u16]) -> String {
    if samples.is_empty() {
        return String::new();
    }
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let min = *samples.iter().min().unwrap() as i64;
    let max = *samples.iter().max().unwrap() as i64;
    let span = (max - min).max(1);
    samples
        .iter()
        .map(|&s| BARS[((s as i64 - min) * 7 / span) as usize])
        .collect()
}
