//! Format / generator end-to-end test (#32) — drive the [`Generator`] loop over
//! *real* mock-RH heats.
//!
//! This is the §5.1 "mock-RH e2e" for the format interface: rather than hand-write
//! `HeatResult`s (the exact table tests in `format::tests` do that), it closes the
//! whole loop against a **real dockerized RotorHazard** —
//!
//! ```text
//!   generator.next(history) -> Run([HeatPlan, ..])
//!        for each HeatPlan:  run_mock_heat -> score_events -> CompletedHeat
//!        history += completed
//!   ... repeat until next() -> Complete ...
//!   assert: the loop terminated and the final ranking covers the field
//! ```
//!
//! The dynamic [`RollingDemo`] is the generator under test, because it is the
//! honesty-forcing case (race-engine.html §3): the test acts as the RD, requesting a
//! round, draining the emitted heat against real hardware, feeding the scored result
//! back, then requesting one more — so the heats are produced *from current state*,
//! never a precomputed schedule.
//!
//! Structural / tolerant, exactly like the scoring e2e: the mock interface reads its
//! CSV continuously, so lap *timing* is not controllable. We assert the loop
//! **terminates** with a `Complete`, that we ran as many heats as rounds requested,
//! and that the final ranking is non-empty and ordered — never exact µs or laps.
//!
//! For RH the canonical pass `at` is **ms since race start**, so the timed clock's
//! origin is zero (`SourceTime::from_micros(0)`).
//!
//! Local-only class (needs Docker). DISTINCT port 5035 (heat 5032, scoring 5033,
//! marshaling 5034). Run:
//!
//! ```sh
//! cargo test -p gridfpv-engine --features live --test format_live -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

mod common;

use common::run_mock_heat;

use gridfpv_engine::format::{
    CompletedHeat, Generator, GeneratorStep, HeatPlan, RankEntry, RollingDemo,
};
use gridfpv_engine::scoring::{WinCondition, score_events};
use gridfpv_events::{CompetitorRef, SourceTime};
use gridfpv_testkit::{NodeCsv, node_csv, plan_csv, scenarios};

/// DISTINCT port for the format e2e: heat 5032, scoring 5033, marshaling 5034, here
/// 5035, so it never clashes with the other engine or adapter live tests.
const PORT: u16 = 5035;

/// RH pass `at` is ms-since-race-start, so the timed clock starts at zero. A wide
/// window so the whole heat counts — we assert structure, not the cutoff.
fn race_start() -> SourceTime {
    SourceTime::from_micros(0)
}

/// The win condition the format scores each heat under: most laps in a generous
/// window (10 min, well past any mock heat).
fn condition() -> WinCondition {
    WinCondition::Timed {
        window_micros: 10 * 60 * 1_000_000,
    }
}

/// The two-node scenario every round flies: a continuous fast lapper (node-0) vs a
/// DNF node (node-1). Identical to the scoring e2e's timing-robust pairing — node-0
/// laps throughout the live window so every heat lands at least one crossing.
fn scenario() -> Vec<(usize, String)> {
    vec![
        (
            0usize,
            node_csv(&NodeCsv {
                ticks_per_lap: 2,
                peak_rssi: 180,
                baseline_rssi: 70,
                seed: 0,
            }),
        ),
        (1usize, plan_csv(&scenarios::dnf(2, 6))),
    ]
}

#[test]
#[ignore = "requires Docker (spins up dockerized RotorHazard and drives the format loop over real heats)"]
fn rolling_generator_loop_terminates_with_a_final_ranking() {
    // The field the rolling format runs over: the two seat refs the harness produces.
    let field: Vec<CompetitorRef> = vec![
        CompetitorRef("node-0".into()),
        CompetitorRef("node-1".into()),
    ];

    // The dynamic generator under test. We (acting as the RD) drive it on demand.
    let mut generator = RollingDemo::new(field.clone());

    // How many rounds the RD will request in total. Kept small (2) so the e2e stays
    // quick while still proving the loop runs multiple real heats and aggregates.
    const ROUNDS: usize = 2;

    // The growing history of scored heats — the generator's only input about the past.
    let mut history: Vec<CompletedHeat> = Vec::new();
    let mut heats_run = 0usize;

    for round in 0..ROUNDS {
        // RD action: explicitly request another round (the only thing that makes the
        // dynamic generator emit a heat — no clock, no RNG inside `next`).
        generator.request_round();

        // Ask the generator what to run next from the current state.
        let step = generator.next(&history);
        let plans: Vec<HeatPlan> = match step {
            GeneratorStep::Run(plans) => plans,
            GeneratorStep::Complete => {
                panic!("generator declared Complete while a round ({round}) was still pending")
            }
        };
        assert!(
            !plans.is_empty(),
            "a Run step must carry at least one heat plan"
        );

        // Drain every emitted heat plan against a real mock-RH heat, score it, and fold
        // the scored result back into the history as a CompletedHeat.
        for plan in plans {
            // The generator chose the lineup; over the mock it is always the field.
            assert_eq!(
                plan.lineup, field,
                "the rolling generator flies the whole field each round"
            );

            let log = run_mock_heat(PORT, &plan.heat.0, &scenario());
            let result = score_events(&log, condition(), race_start());

            // At least one crossing must have landed (the harness guarantees it).
            assert!(
                !result.places.is_empty(),
                "heat {} produced no scored competitors",
                plan.heat.0
            );

            eprintln!(
                "format e2e: heat {} scored {} competitor(s)",
                plan.heat.0,
                result.places.len()
            );

            history.push(CompletedHeat {
                heat: plan.heat.clone(),
                result,
            });
            heats_run += 1;
        }
    }

    // No further rounds requested: the generator must now declare the format complete.
    assert_eq!(
        generator.next(&history),
        GeneratorStep::Complete,
        "with no rounds pending the generator must complete"
    );

    // We ran exactly as many real heats as rounds requested.
    assert_eq!(
        heats_run, ROUNDS,
        "the loop should have run one real heat per requested round"
    );

    // The final ranking covers the field, is in non-decreasing position order, and the
    // top entry is position 1 — structural assertions only.
    let ranking: Vec<RankEntry> = generator.ranking(&history);
    assert_eq!(
        ranking.len(),
        field.len(),
        "the final ranking should cover the whole field"
    );
    assert_eq!(
        ranking[0].position, 1,
        "the ranking must start at position 1"
    );
    for window in ranking.windows(2) {
        assert!(
            window[0].position <= window[1].position,
            "ranking must be in non-decreasing position order"
        );
    }

    // The continuously-lapping node-0 out-laps the DNF node-1, so it must top the
    // best-flight aggregate. (Structural: which node wins is determined by the
    // scenario, not by timing.)
    assert_eq!(
        ranking[0].competitor,
        CompetitorRef("node-0".into()),
        "the continuously-lapping node should top the aggregate ranking"
    );

    println!(
        "format e2e: ran {heats_run} heats over {ROUNDS} requested rounds; \
         final ranking = {:?}",
        ranking
            .iter()
            .map(|e| (e.competitor.0.as_str(), e.position))
            .collect::<Vec<_>>()
    );
}
