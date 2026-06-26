//! ZippyQ / rolling end-to-end test (#35) — the dynamic generator driven over *real*
//! mock-RH heats.
//!
//! Drives a **rolling session** against a dockerized RotorHazard via the shared
//! [`common::run_mock_heat`] harness: the RD requests a couple of rounds **on demand**,
//! each is run as a real heat, scored through [`gridfpv_engine::scoring::score_events`]
//! into a [`CompletedHeat`], and fed back into the [`ZippyQ`] generator. Finally the RD
//! [`close`](ZippyQ::close)s the session and the generator
//! [`Complete`](gridfpv_engine::format::GeneratorStep::Complete)s. We then assert the
//! best-flight standing is total over the field.
//!
//! This is the §3 honesty check end to end: the heats appear *only* because the RD
//! requested them (no precomputed schedule), and the standing aggregates each pilot's
//! best single flight. The mock reads its CSV continuously, so lap *timing* is
//! approximate; assertions are **structural** (a round appears per request, termination
//! after close, a best-flight ranking covering the field), never exact µs.
//!
//! For RH the canonical pass `at` is **ms since race start**, so the timed clock starts
//! at zero: `race_start = SourceTime::from_micros(0)`.
//!
//! Local-only class (needs Docker). DISTINCT port 5038. Run:
//!
//! ```sh
//! cargo test -p gridfpv-engine --features live --test zippyq_live -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

mod common;

use common::run_mock_heat;

use gridfpv_engine::format::{CompletedHeat, Generator, GeneratorStep, HeatPlan};
use gridfpv_engine::scoring::{WinCondition, score_events};
use gridfpv_engine::zippyq::ZippyQ;
use gridfpv_events::{CompetitorRef, SourceTime};
use gridfpv_testkit::{NodeCsv, node_csv};

/// DISTINCT port for the ZippyQ e2e (heat e2e 5032, scoring 5033, adapters 5030/5031).
const PORT: u16 = 5038;

/// The seat refs the harness lines up are `node-{i}`; the rolling field is these two.
fn field() -> Vec<CompetitorRef> {
    vec![
        CompetitorRef("node-0".into()),
        CompetitorRef("node-1".into()),
    ]
}

/// A continuously-lapping node CSV at the given cadence (smaller `ticks_per_lap` =
/// faster). Continuous because the mock reads its CSV decoupled from race start — only
/// a stream that keeps lapping reliably lands passes in the brief live window.
fn lapper(ticks_per_lap: usize) -> String {
    node_csv(&NodeCsv {
        ticks_per_lap,
        peak_rssi: 180,
        baseline_rssi: 70,
        seed: 0,
    })
}

/// Run one rolling round: both nodes fly the same continuous scenario; returns the
/// scored [`CompletedHeat`] keyed to `plan.heat`.
fn run_round(plan: &HeatPlan) -> CompletedHeat {
    // Both seats lap continuously so each round reliably banks crossings in the live
    // window; the structural assertions never depend on exact lap counts.
    let scenario = vec![(0usize, lapper(2)), (1usize, lapper(2))];
    let log = run_mock_heat(PORT, &plan.heat.0, &scenario);
    // RH pass `at` is ms-since-race-start, so the timed clock starts at zero. A wide
    // window so the whole heat counts (structure, not the cutoff boundary).
    let result = score_events(
        &log,
        WinCondition::Timed {
            window_micros: 10 * 60 * 1_000_000,
        },
        SourceTime::from_micros(0),
    );
    CompletedHeat::new(plan.heat.0.clone(), result)
}

#[test]
#[ignore = "requires Docker (spins up dockerized RotorHazard and drives rolling heats)"]
fn rolling_session_runs_rounds_on_demand_then_completes() {
    let mut zq = ZippyQ::new(field());

    // Nothing requested yet → the generator emits nothing (NOT a precomputed schedule).
    assert_eq!(
        zq.next(&[]),
        GeneratorStep::Complete,
        "with no round requested, ZippyQ must not emit a heat"
    );

    // The growing history of completed heats fed back to the generator each step.
    let mut completed: Vec<CompletedHeat> = Vec::new();

    // --- Round 1, on demand: the RD queues the whole field as ready ---
    zq.request_full_round();
    let step = zq.next(&completed);
    let plan1 = match step {
        GeneratorStep::Run(heats) if heats.len() == 1 => heats.into_iter().next().unwrap(),
        other => panic!("expected one heat for the first requested round, got {other:?}"),
    };
    assert_eq!(
        plan1.heat.0, "round-1",
        "first round is numbered from state"
    );
    let c1 = run_round(&plan1);
    assert!(
        !c1.result.places.is_empty(),
        "round 1 must have scored at least one competitor"
    );
    completed.push(c1);

    // Round 1 drained, nothing further requested → done *for now* (the RD may ask again).
    assert_eq!(
        zq.next(&completed),
        GeneratorStep::Complete,
        "with round 1 run and nothing pending, ZippyQ completes for now"
    );

    // --- Round 2, on demand: requested only now — proves heats follow requests ---
    zq.request_full_round();
    let step = zq.next(&completed);
    let plan2 = match step {
        GeneratorStep::Run(heats) if heats.len() == 1 => heats.into_iter().next().unwrap(),
        other => panic!("expected one heat for the second requested round, got {other:?}"),
    };
    assert_eq!(
        plan2.heat.0, "round-2",
        "second round is numbered from current state (one round already run)"
    );
    let c2 = run_round(&plan2);
    completed.push(c2);

    // --- Close the session: no more rounds → terminate ---
    zq.close();
    assert!(zq.is_closed());
    assert_eq!(
        zq.next(&completed),
        GeneratorStep::Complete,
        "closed with nothing pending, ZippyQ terminates for good"
    );

    // --- Best-flight standing is total over the field ---
    let ranking = zq.ranking(&completed);
    assert_eq!(
        ranking.len(),
        field().len(),
        "the best-flight ranking covers every pilot in the field"
    );
    // Positions are 1-based and start at 1 (best first).
    assert!(
        ranking.iter().any(|e| e.position == 1),
        "the standing has a leader at position 1"
    );
    // Every ranked pilot is a field member (no phantom entries).
    for entry in &ranking {
        assert!(
            field().contains(&entry.competitor),
            "ranked pilot {:?} is a field member",
            entry.competitor
        );
    }

    eprintln!(
        "zippyq e2e: ran {} rounds on demand; standing = {:?}",
        completed.len(),
        ranking
            .iter()
            .map(|e| (e.competitor.0.clone(), e.position))
            .collect::<Vec<_>>()
    );
}
