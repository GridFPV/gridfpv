//! Heat-loop end-to-end test (#29) — the engine's mock-RH e2e leg.
//!
//! Drives a full heat against a **real dockerized RotorHazard** via the shared
//! [`common::run_mock_heat`] harness, then asserts — structurally — that the heat
//! ran the whole loop to `Final`, that the recorded transitions appear in canonical
//! order, and that the timer crossings were collected only while the heat was
//! `Running` (the bridge's emission rule). This is the first consumer of the
//! reusable harness; #30 (scoring), #31 (marshaling), #33–37 fold the same log.
//!
//! Local-only class (needs Docker). Run via `cargo xtask live` or:
//!
//! ```sh
//! cargo test -p gridfpv-engine --features live --test heat_live -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

mod common;

use common::{DEFAULT_PORT, run_mock_heat};

use gridfpv_engine::heat::{HeatState, heat_state, next_state};
use gridfpv_events::{Event, HeatId, HeatTransition};
use gridfpv_testkit::{NodeCsv, node_csv};

const HEAT: &str = "q-1";

#[test]
#[ignore = "requires Docker (spins up dockerized RotorHazard and drives a full heat)"]
fn mock_heat_runs_to_scored_with_passes_only_while_live() {
    // Two emulated nodes producing laps; the harness drives the heat loop end to end.
    let scenario = vec![
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

    let log = run_mock_heat(DEFAULT_PORT, HEAT, &scenario);
    let heat = HeatId(HEAT.to_string());

    // --- the heat reached Final (fold the whole log back) ---
    assert_eq!(
        heat_state(&log, &heat),
        Some(HeatState::Final),
        "the heat must fold back to Final"
    );

    // --- the forward-path transitions appear, in canonical order ---
    let transitions: Vec<HeatTransition> = log
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
            HeatTransition::Staged,
            HeatTransition::Armed,
            HeatTransition::Running,
            HeatTransition::Finished,
            HeatTransition::Finalized,
        ],
        "the recorded transitions must be the forward heat loop, in order"
    );

    // --- exactly one HeatScheduled, and it comes first ---
    assert!(
        matches!(log.first(), Some(Event::HeatScheduled { heat: h, .. }) if *h == heat),
        "the log must open with the heat's HeatScheduled"
    );
    assert_eq!(
        log.iter()
            .filter(|e| matches!(e, Event::HeatScheduled { .. }))
            .count(),
        1,
        "the heat is scheduled exactly once"
    );

    // --- passes were collected, and only between Running and Finished ---
    // Replay the log: walk the heat's state and assert every Pass landed while the heat
    // was Running — the bridge's rule (`source.rs` tears down pass emission the moment a
    // heat leaves `Running`, and the grace window holds `Running` rather than reopening a
    // closed heat, #505). The harness interleaves passes only between the Running and
    // Finished transitions, so any leak past Finished fails here.
    let mut state: Option<HeatState> = None;
    let mut pass_count = 0usize;
    for event in &log {
        match event {
            Event::HeatScheduled { heat: h, .. } if *h == heat => {
                state = Some(HeatState::Scheduled);
            }
            Event::HeatStateChanged {
                heat: h,
                transition,
            } if *h == heat => {
                if let Some(current) = state {
                    state = Some(next_state(current, *transition));
                }
            }
            Event::Pass(_) => {
                pass_count += 1;
                let current = state.expect("a pass must follow the HeatScheduled seed");
                assert_eq!(
                    current,
                    HeatState::Running,
                    "passes must be interleaved only while the heat is Running"
                );
            }
            _ => {}
        }
    }
    assert!(
        pass_count >= 1,
        "the harness must have collected at least one timer crossing"
    );

    println!(
        "mock heat e2e: {} transitions, {pass_count} passes, folded to Final",
        transitions.len()
    );
}
