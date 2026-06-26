//! Timed-qualifying end-to-end test (#33) — drive the [`TimedQualifying`] generator
//! loop over *real* mock-RH heats.
//!
//! This is the §5.1 "mock-RH e2e" for the timed-qualifying format: rather than
//! hand-write `HeatResult`s (the exact table tests in `timed_qual::tests` do that), it
//! closes the whole loop against a **real dockerized RotorHazard** —
//!
//! ```text
//!   generator.next(history) -> Run([HeatPlan, ..])
//!        for each HeatPlan:  run_mock_heat -> score_events(BestLap) -> CompletedHeat
//!        history += completed
//!   ... repeat until next() -> Complete (after `rounds` rounds) ...
//!   assert: the loop terminated and the final best-flight ranking covers the field
//! ```
//!
//! Each round is scored under the qualifying [`WinCondition::BestLap`] — the same
//! metric the generator aggregates by (best single lap across rounds).
//!
//! Structural / tolerant, exactly like the format e2e: the mock interface reads its CSV
//! continuously, so lap *timing* is not controllable. We assert the loop **terminates**
//! with a `Complete` after exactly `ROUNDS` rounds, that we ran one real heat per round,
//! and that the final ranking is non-empty, ordered, and tops out with the
//! continuously-lapping node — never exact µs or laps. node-0 laps throughout the live
//! window (so it always sets a qualifying lap) while node-1 DNFs early, so node-0's best
//! flight beats node-1's and it must rank first.
//!
//! For RH the canonical pass `at` is **ms since race start**, so the timed clock's
//! origin is zero (`SourceTime::from_micros(0)`).
//!
//! Local-only class (needs Docker). DISTINCT port 5036 (heat 5032, scoring 5033,
//! marshaling 5034, format 5035). Run:
//!
//! ```sh
//! cargo test -p gridfpv-engine --features live --test timed_qual_live -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

mod common;

use common::run_mock_heat;

use gridfpv_engine::format::{CompletedHeat, Generator, GeneratorStep, HeatPlan, RankEntry};
use gridfpv_engine::scoring::{WinCondition, score_events};
use gridfpv_engine::timed_qual::{QualMetric, TimedQualifying};
use gridfpv_events::{CompetitorRef, SourceTime};
use gridfpv_testkit::{NodeCsv, node_csv, plan_csv, scenarios};

/// DISTINCT port for the timed-qual e2e: heat 5032, scoring 5033, marshaling 5034,
/// format 5035, here 5036 — never clashing with the other engine or adapter live tests.
const PORT: u16 = 5036;

/// RH pass `at` is ms-since-race-start, so the qualifying clock starts at zero (the
/// best-lap condition uses absolute pass times directly and ignores the origin anyway).
fn race_start() -> SourceTime {
    SourceTime::from_micros(0)
}

/// The win condition each round is scored under: fastest single lap (qualifying) — the
/// same metric the generator aggregates across rounds.
fn condition() -> WinCondition {
    WinCondition::BestLap
}

/// The two-node scenario every round flies: a continuous fast lapper (node-0) vs a DNF
/// node (node-1). Identical pairing to the format/scoring e2es — node-0 laps throughout
/// the live window so every heat lands at least one qualifying lap.
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
#[ignore = "requires Docker (spins up dockerized RotorHazard and drives the timed-qual loop over real heats)"]
fn timed_qual_loop_terminates_with_a_best_flight_ranking() {
    // The field the qualifying format runs over: the two seat refs the harness produces.
    let field: Vec<CompetitorRef> = vec![
        CompetitorRef("node-0".into()),
        CompetitorRef("node-1".into()),
    ];

    // How many rounds to fly. Kept small (2) so the e2e stays quick while still proving
    // the loop runs multiple real heats and aggregates a best flight across them.
    const ROUNDS: usize = 2;

    // The generator under test: best-lap qualifying over `ROUNDS` rounds.
    let mut generator = TimedQualifying::new(field.clone(), ROUNDS, QualMetric::BestLap);

    // The growing history of scored heats — the generator's only input about the past.
    let mut history: Vec<CompletedHeat> = Vec::new();
    let mut heats_run = 0usize;

    // Drive rounds until the generator declares the format complete.
    loop {
        let step = generator.next(&history);
        let plans: Vec<HeatPlan> = match step {
            GeneratorStep::Run(plans) => plans,
            GeneratorStep::Complete => break,
        };
        assert!(
            !plans.is_empty(),
            "a Run step must carry at least one heat plan"
        );

        for plan in plans {
            // The generator flies the whole field each round.
            assert_eq!(
                plan.lineup, field,
                "the timed-qual generator flies the whole field each round"
            );

            let log = run_mock_heat(PORT, &plan.heat.0, &scenario());
            let result = score_events(&log, condition(), race_start());

            assert!(
                !result.places.is_empty(),
                "heat {} produced no scored competitors",
                plan.heat.0
            );

            eprintln!(
                "timed-qual e2e: heat {} scored {} competitor(s)",
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

    // The loop terminated via Complete, and ran exactly one real heat per round.
    assert_eq!(
        heats_run, ROUNDS,
        "the loop should have run one real heat per configured round"
    );

    // Calling next again still completes (idempotent terminal state).
    assert_eq!(
        generator.next(&history),
        GeneratorStep::Complete,
        "the generator stays Complete once all rounds are flown"
    );

    // The final best-flight ranking covers the field, is in non-decreasing position
    // order, and starts at position 1 — structural assertions only.
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

    // The continuously-lapping node-0 sets a qualifying lap every round; node-1 DNFs, so
    // node-0's best flight tops the aggregate. (Structural: which node wins is fixed by
    // the scenario, not by timing.)
    assert_eq!(
        ranking[0].competitor,
        CompetitorRef("node-0".into()),
        "the continuously-lapping node should top the best-flight ranking"
    );

    println!(
        "timed-qual e2e: ran {heats_run} heats over {ROUNDS} rounds; \
         final ranking = {:?}",
        ranking
            .iter()
            .map(|e| (e.competitor.0.as_str(), e.position))
            .collect::<Vec<_>>()
    );
}
