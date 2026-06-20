//! Round-robin end-to-end test (#13) — drive the [`RoundRobin`] generator loop over
//! *real* mock-RH heats.
//!
//! This is the §5.1 "mock-RH e2e" for the round-robin format: rather than hand-write
//! `HeatResult`s (the exact table tests in `round_robin::tests` do that), it closes the
//! whole loop against a **real dockerized RotorHazard** —
//!
//! ```text
//!   generator.next(history) -> Run([HeatPlan, ..])   (one round's partition of heats)
//!        for each HeatPlan:  run_mock_heat -> score_events(Timed) -> CompletedHeat
//!        history += completed
//!   ... repeat until next() -> Complete (after `ROUNDS` rounds) ...
//!   assert: the loop terminated and the final points ranking covers the field
//! ```
//!
//! Each heat is scored under [`WinCondition::Timed`] (most laps in a window); the
//! generator aggregates finishing-position points across every heat a pilot flew.
//!
//! Structural / tolerant, exactly like the other format e2es: the mock interface reads
//! its CSV continuously, so lap *timing* is not controllable. We assert the loop
//! **terminates** with a `Complete` after exactly `ROUNDS` rounds, that we ran one real
//! heat per scheduled plan, and that the final ranking is non-empty, ordered, and tops
//! out with the continuously-lapping node — never exact µs or laps. With a two-node
//! field and `heat_size` 2 each round is a single heat of both nodes; node-0 laps
//! throughout the live window while node-1 DNFs early, so node-0 wins every heat and
//! tops the points table.
//!
//! For RH the canonical pass `at` is **ms since race start**, so the timed clock's
//! origin is zero (`SourceTime::from_micros(0)`).
//!
//! Local-only class (needs Docker). DISTINCT port 5043. Run:
//!
//! ```sh
//! cargo test -p gridfpv-engine --features live --test round_robin_live -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

mod common;

use common::run_mock_heat;

use gridfpv_engine::format::{CompletedHeat, Generator, GeneratorStep, HeatPlan, RankEntry};
use gridfpv_engine::round_robin::{RoundRobin, RrMetric};
use gridfpv_engine::scoring::{WinCondition, score_events};
use gridfpv_events::{CompetitorRef, SourceTime};
use gridfpv_testkit::{NodeCsv, node_csv, plan_csv, scenarios};

/// DISTINCT port for the round-robin e2e — never clashing with the other engine or
/// adapter live tests (which occupy 5030–5040).
const PORT: u16 = 5043;

/// RH pass `at` is ms-since-race-start, so the timed clock starts at zero.
fn race_start() -> SourceTime {
    SourceTime::from_micros(0)
}

/// The win condition each heat is scored under: most laps in a generous window.
fn condition() -> WinCondition {
    WinCondition::Timed {
        window_micros: 60_000_000,
    }
}

/// The two-node scenario every heat flies: a continuous fast lapper (node-0) vs a DNF
/// node (node-1). node-0 laps throughout the live window so it banks the most laps and
/// wins each heat outright.
fn scenario() -> Vec<(usize, String)> {
    vec![
        (
            0usize,
            node_csv(&NodeCsv {
                ticks_per_lap: 2,
                peak_rssi: 180,
                baseline_rssi: 70,
            }),
        ),
        (1usize, plan_csv(&scenarios::dnf(2, 6))),
    ]
}

#[test]
#[ignore = "requires Docker (spins up dockerized RotorHazard and drives the round-robin loop over real heats)"]
fn round_robin_loop_terminates_with_a_points_ranking() {
    // The field the round-robin runs over: the two seat refs the harness produces.
    let field: Vec<CompetitorRef> = vec![
        CompetitorRef("node-0".into()),
        CompetitorRef("node-1".into()),
    ];

    // Two rounds, heat_size 2 — with two nodes each round is a single heat of both, so
    // the loop runs two real heats total while still proving multi-round aggregation.
    const ROUNDS: usize = 2;
    const HEAT_SIZE: usize = 2;

    let mut generator = RoundRobin::new(field.clone(), ROUNDS, HEAT_SIZE, RrMetric::Points);

    // The growing history of scored heats — the generator's only input about the past.
    let mut history: Vec<CompletedHeat> = Vec::new();
    let mut heats_run = 0usize;
    let mut rounds_seen = 0usize;

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
        rounds_seen += 1;

        for plan in plans {
            // Each heat covers a slice of the field; the two-node field makes it both.
            assert!(
                !plan.lineup.is_empty(),
                "heat {} has an empty lineup",
                plan.heat.0
            );
            assert!(
                plan.heat.0.starts_with("rr-r"),
                "heat id should follow the rr-r{{n}}-h{{i}} scheme, got {}",
                plan.heat.0
            );

            let log = run_mock_heat(PORT, &plan.heat.0, &scenario());
            let result = score_events(&log, condition(), race_start());

            assert!(
                !result.places.is_empty(),
                "heat {} produced no scored competitors",
                plan.heat.0
            );

            eprintln!(
                "round-robin e2e: heat {} scored {} competitor(s)",
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

    // The loop terminated via Complete after exactly the configured rounds.
    assert_eq!(
        rounds_seen, ROUNDS,
        "the loop should run exactly ROUNDS rounds"
    );
    assert_eq!(
        heats_run, ROUNDS,
        "two nodes at heat_size 2 → one heat per round → ROUNDS heats total"
    );

    // Calling next again still completes (idempotent terminal state).
    assert_eq!(
        generator.next(&history),
        GeneratorStep::Complete,
        "the generator stays Complete once all rounds are flown"
    );

    // The final points ranking covers the field, is in non-decreasing position order,
    // and starts at position 1 — structural assertions only.
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

    // node-0 laps continuously and wins every heat; node-1 DNFs, so node-0 banks the
    // most points and tops the table. (Structural: which node wins is fixed by the
    // scenario, not by timing.)
    assert_eq!(
        ranking[0].competitor,
        CompetitorRef("node-0".into()),
        "the continuously-lapping node should top the points ranking"
    );

    println!(
        "round-robin e2e: ran {heats_run} heats over {ROUNDS} rounds; final ranking = {:?}",
        ranking
            .iter()
            .map(|e| (e.competitor.0.as_str(), e.position))
            .collect::<Vec<_>>()
    );
}
