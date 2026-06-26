//! Multi-class scheduling end-to-end test (#36) — drive the [`Scheduler`] over *real*
//! mock-RH heats for two classes at once.
//!
//! This is the §5.1 "mock-RH e2e" for multi-class scheduling. Where the exact table
//! tests in `schedule::tests` hand-feed scripted generators, this closes the whole loop
//! against a **real dockerized RotorHazard**: two [`TimedQualifying`] classes share the
//! scheduler's timer and frequency pool, and we run the interleaved heats the scheduler
//! hands out through `run_mock_heat` until every class completes —
//!
//! ```text
//!   loop:
//!     sched.next_heat() -> Some(ScheduledHeat { class, heat, lineup, frequencies })
//!       run_mock_heat(lineup) -> score_events(BestLap) -> CompletedHeat
//!       sched.record(class, completed)
//!   ... until next_heat() -> None (all classes complete) ...
//!   assert: both classes completed, every IRL heat got a distinct channel per pilot
//! ```
//!
//! Structural / tolerant, like the other format e2es: the mock interface reads its CSV
//! continuously, so lap *timing* is not controllable. We assert the loop **terminates**
//! (every class drains to `None`), that we ran the expected number of real heats, that
//! both classes' heats actually flew (the round-robin interleaved them, neither
//! starved), and that the engine **allocated a distinct channel per pilot** for every
//! heat (RE §7.3, the engine-allocates half of the split) — never exact µs or laps.
//!
//! For RH the canonical pass `at` is **ms since race start**, so the timed clock's
//! origin is zero (`SourceTime::from_micros(0)`).
//!
//! Local-only class (needs Docker). DISTINCT port 5039 (heat 5032, scoring 5033,
//! marshaling 5034, format 5035, timed-qual 5036, single-elim 5037, zippyq 5038). Run:
//!
//! ```sh
//! cargo test -p gridfpv-engine --features live --test multiclass_live -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

mod common;

use std::collections::BTreeSet;

use common::run_mock_heat;

use gridfpv_engine::format::CompletedHeat;
use gridfpv_engine::schedule::{FrequencyPool, Scheduler};
use gridfpv_engine::scoring::{WinCondition, score_events};
use gridfpv_engine::timed_qual::{QualMetric, TimedQualifying};
use gridfpv_events::{CompetitorRef, SourceTime};
use gridfpv_testkit::{NodeCsv, node_csv, plan_csv, scenarios};

/// DISTINCT port for the multi-class e2e — never clashing with the other engine or
/// adapter live tests (heat 5032 … zippyq 5038, here 5039).
const PORT: u16 = 5039;

/// RH pass `at` is ms-since-race-start, so the qualifying clock starts at zero (the
/// best-lap condition uses absolute pass times directly and ignores the origin anyway).
fn race_start() -> SourceTime {
    SourceTime::from_micros(0)
}

/// Each round is scored under the qualifying best-lap condition — the metric the
/// timed-qual generator aggregates across rounds.
fn condition() -> WinCondition {
    WinCondition::BestLap
}

/// The two-node scenario every heat flies: a continuous fast lapper (node-0) vs a node
/// that DNFs early (node-1). Identical pairing to the other format e2es — node-0 laps
/// throughout the live window so every heat lands at least one qualifying lap.
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
#[ignore = "requires Docker (spins up dockerized RotorHazard and drives the multi-class scheduler over real heats)"]
fn multiclass_scheduler_drives_two_classes_to_completion() {
    // The field both classes run over: the two seat refs the harness produces.
    let field: Vec<CompetitorRef> = vec![
        CompetitorRef("node-0".into()),
        CompetitorRef("node-1".into()),
    ];

    // Each class flies a single round, so the scheduler interleaves exactly two heats
    // (one per class) — enough to prove round-robin interleaving + per-heat allocation
    // while keeping the e2e quick.
    const ROUNDS: usize = 1;

    // Two IRL timed-qual classes sharing the scheduler's timer and Raceband pool.
    let mut sched = Scheduler::new(FrequencyPool::raceband());
    sched.add_class(
        "open",
        Box::new(TimedQualifying::new(
            field.clone(),
            ROUNDS,
            QualMetric::BestLap,
        )),
    );
    sched.add_class(
        "spec",
        Box::new(TimedQualifying::new(
            field.clone(),
            ROUNDS,
            QualMetric::BestLap,
        )),
    );

    // Track which classes actually flew a heat (round-robin must serve both) and that
    // each heat got a distinct channel per pilot.
    let mut classes_seen: BTreeSet<String> = BTreeSet::new();
    let mut heats_run = 0usize;

    while let Some(heat) = sched.next_heat() {
        // The engine allocated a distinct channel to each pilot (RE §7.3 — the
        // engine-allocates half of the ownership split).
        assert_eq!(
            heat.frequencies.len(),
            heat.lineup.len(),
            "every pilot in an IRL heat must be allocated a channel"
        );
        let channels: BTreeSet<u16> = heat.frequencies.iter().map(|(_, f)| f.mhz).collect();
        assert_eq!(
            channels.len(),
            heat.lineup.len(),
            "allocated channels must be distinct within a heat"
        );
        // The first pilot in the lineup gets the pool's first channel (first-fit).
        assert_eq!(
            heat.frequencies[0].0, heat.lineup[0],
            "frequencies pair with the lineup in order"
        );

        eprintln!(
            "multiclass e2e: class {} heat {} on channels {:?}",
            heat.class,
            heat.heat.0,
            heat.frequencies
                .iter()
                .map(|(c, f)| (c.0.as_str(), f.mhz))
                .collect::<Vec<_>>()
        );

        // Fly the real heat, score it, and feed the result back to its class.
        let log = run_mock_heat(PORT, &heat.heat.0, &scenario());
        let result = score_events(&log, condition(), race_start());
        assert!(
            !result.places.is_empty(),
            "heat {} produced no scored competitors",
            heat.heat.0
        );

        classes_seen.insert(heat.class.clone());
        heats_run += 1;
        sched.record(&heat.class, CompletedHeat::new(heat.heat.0, result));
    }

    // The loop terminated only once every class completed.
    assert!(
        sched.is_complete(),
        "the scheduler must report complete once next_heat returns None"
    );
    assert!(
        sched.next_heat().is_none(),
        "next_heat stays None at the terminal state"
    );

    // Both classes flew (the round-robin interleaved them; neither starved), and we ran
    // exactly one real heat per class per round.
    assert_eq!(
        classes_seen,
        BTreeSet::from(["open".to_string(), "spec".to_string()]),
        "both classes should have flown at least one heat"
    );
    assert_eq!(
        heats_run,
        2 * ROUNDS,
        "one real heat per class per round (two classes)"
    );

    // Each class produced a final best-flight ranking covering the field.
    for class in ["open", "spec"] {
        let ranking = sched
            .rankings(class)
            .unwrap_or_else(|| panic!("class {class} should be present"));
        assert_eq!(
            ranking.len(),
            field.len(),
            "class {class}'s final ranking should cover the whole field"
        );
        assert_eq!(
            ranking[0].position, 1,
            "class {class}'s ranking must start at position 1"
        );
        // node-0 laps continuously every heat; node-1 DNFs — so node-0 tops the aggregate.
        assert_eq!(
            ranking[0].competitor,
            CompetitorRef("node-0".into()),
            "the continuously-lapping node should top class {class}'s ranking"
        );
    }

    println!(
        "multiclass e2e: ran {heats_run} heats across {} classes",
        classes_seen.len()
    );
}
