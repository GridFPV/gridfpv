//! Scoring end-to-end test (#30) — the scorer over a *real* mock-RH heat.
//!
//! Drives a full heat against a **real dockerized RotorHazard** via the shared
//! [`common::run_mock_heat`] harness, then folds the returned log through
//! [`gridfpv_engine::scoring::score_events`] and asserts — structurally — that the
//! competitor who clearly out-laps the other places ahead, under both a timed
//! (most-laps) and a first-to-N condition. The harness's mock interface reads its
//! CSV continuously, so lap *timing* is approximate; we assert structure (relative
//! placement, lap counts), never exact µs.
//!
//! Scenario: a continuous fast lapper (`node_csv`, lapping for the whole file) vs a
//! `dnf` node (a couple of early laps, then silence). The busy node banks strictly
//! more laps, so it must finish ahead under most-laps and reach a modest lap target
//! first. The busy node uses the looping `node_csv` (not the front-loaded `uniform`
//! plan) because the mock reads its CSV continuously and decoupled from race start —
//! only a stream that keeps lapping throughout reliably lands passes in the live
//! window; the `dnf` plan is fine as the under-lapper since it is allowed to drop out.
//!
//! For RH the canonical pass `at` is **milliseconds since race start**, so the timed
//! condition's shared clock starts at zero: `race_start = SourceTime::from_micros(0)`.
//!
//! Local-only class (needs Docker). DISTINCT port 5033 (the heat e2e uses 5032). Run:
//!
//! ```sh
//! cargo test -p gridfpv-engine --features live --test scoring_live -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

mod common;

use common::run_mock_heat;

use gridfpv_engine::scoring::{Metric, WinCondition, score_events};
use gridfpv_events::{AdapterId, CompetitorRef, Event, SourceTime};
use gridfpv_projection::CompetitorKey;
use gridfpv_testkit::{NodeCsv, node_csv, plan_csv, scenarios};

/// DISTINCT port for the scoring e2e, so it never clashes with the heat e2e (5032)
/// or the adapters' live tests (5030/5031).
const PORT: u16 = 5033;
const HEAT: &str = "scoring-q-1";

/// The adapter id the RotorHazard adapter stamps onto its passes.
const RH_ADAPTER: &str = "rotorhazard";

/// Key for `node-{index}` on the RotorHazard adapter (the seat refs the harness
/// builds the lineup from).
fn node_key(index: usize) -> CompetitorKey {
    CompetitorKey {
        adapter: AdapterId(RH_ADAPTER.into()),
        competitor: CompetitorRef(format!("node-{index}")),
    }
}

/// The placement for a node, if it appears in the result at all. A node that
/// produced no passes (a deep DNF in the live window) has no laps and so is absent.
fn placement_of(
    result: &gridfpv_engine::scoring::HeatResult,
    index: usize,
) -> Option<&gridfpv_engine::scoring::Placement> {
    result
        .places
        .iter()
        .find(|p| p.competitor == node_key(index))
}

/// Lap count actually recorded for a node (`0` if it never appears).
fn laps_of(result: &gridfpv_engine::scoring::HeatResult, index: usize) -> u32 {
    placement_of(result, index).map(|p| p.laps).unwrap_or(0)
}

/// Position of a node, asserting it is present (used only for the busy node).
fn position_of(result: &gridfpv_engine::scoring::HeatResult, index: usize) -> u32 {
    placement_of(result, index)
        .map(|p| p.position)
        .unwrap_or_else(|| panic!("no placement for node-{index}"))
}

#[test]
#[ignore = "requires Docker (spins up dockerized RotorHazard and drives a full heat)"]
fn busier_node_places_ahead_under_timed_and_first_to_n() {
    // node-0: a continuous fast lapper — `node_csv` increments `lap_id` for the whole
    // file and loops at EOF, so crossings keep coming *throughout* the live window no
    // matter when (relative to the mock's continuous CSV reader) the race actually
    // starts. This is the timing-robust "busy node" the heat e2e relies on.
    //
    // node-1: a `dnf` plan — a couple of early laps, then a flat `lap_id` for the rest
    // of the file (the pilot dropped out). It records far fewer laps than node-0; it
    // may even contribute none to the *live* window, which is fine — the assertion is
    // structural ("the busier node places ahead"), and the harness only needs one
    // crossing total (node-0 supplies plenty).
    let scenario = vec![
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
    ];

    let log = run_mock_heat(PORT, HEAT, &scenario);

    // RH pass `at` is ms-since-race-start, so the timed clock starts at zero. A wide
    // window so the whole heat counts (we are asserting structure, not the cutoff —
    // the cutoff boundary is exercised by the exact table tests).
    let race_start = SourceTime::from_micros(0);

    // Count the real crossings the mock produced per node (diagnostic + sanity). The
    // shared harness stops the race as soon as the first crossing lands, so the live
    // window is brief: we are guaranteed at least one crossing total, and — because the
    // busy node laps continuously while the DNF node front-loads then goes flat — the
    // busy node out-crosses the DNF node.
    let pass_count = |node: &str| {
        log.iter()
            .filter(|e| matches!(e, Event::Pass(p) if p.competitor == CompetitorRef(node.into())))
            .count()
    };
    let busy_passes = pass_count("node-0");
    let dnf_passes = pass_count("node-1");
    eprintln!("scoring e2e: node-0 passes={busy_passes}, node-1 passes={dnf_passes}");
    assert!(
        busy_passes >= 1,
        "the busy node must have produced at least one crossing"
    );
    assert!(
        busy_passes >= dnf_passes,
        "the busy node ({busy_passes}) must out-cross the DNF node ({dnf_passes})"
    );

    // --- Timed (most laps): the uniform node out-laps the DNF node ---
    let timed = score_events(
        &log,
        WinCondition::Timed {
            window_micros: 10 * 60 * 1_000_000, // 10 min — generously past the heat
        },
        race_start,
    );

    let busy_laps = laps_of(&timed, 0);
    let dnf_laps = laps_of(&timed, 1);
    assert!(
        busy_laps >= dnf_laps,
        "the busy node should record at least as many laps ({busy_laps}) as the DNF node ({dnf_laps})"
    );
    assert_eq!(
        position_of(&timed, 0),
        1,
        "the busier node must place first under most-laps"
    );
    // If the DNF node appears at all (it may contribute no passes to the live window),
    // it must sit behind the busy node.
    if let Some(dnf) = placement_of(&timed, 1) {
        assert!(
            dnf.position > position_of(&timed, 0),
            "the DNF node must place behind the busier node"
        );
    }

    // --- FirstToLaps: a target the busy node reaches and the DNF node cannot ---
    // The DNF node flies at most 2 laps; pick n = 3 so only the busy node can reach it.
    let first_to = score_events(&log, WinCondition::FirstToLaps { n: 3 }, race_start);
    assert_eq!(
        position_of(&first_to, 0),
        1,
        "the busier node reaches the lap target first"
    );
    // The DNF node never reaches 3 laps. If present, its metric records that it did
    // not, and it ranks behind the node that reached the target.
    if let Some(dnf) = placement_of(&first_to, 1) {
        assert_eq!(
            dnf.metric,
            Metric::ReachedAt(None),
            "the DNF node must not have reached the lap target"
        );
        assert!(
            dnf.position > position_of(&first_to, 0),
            "the DNF node ranks after the node that reached the target"
        );
    }

    println!(
        "scoring e2e: busy={busy_laps} laps (pos {}), dnf={dnf_laps} laps",
        position_of(&timed, 0),
    );
}
