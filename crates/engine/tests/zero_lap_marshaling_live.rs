//! Zero-lap marshaling end-to-end test (#388) — the pilot the timer never detected.
//!
//! Drives a full heat against a **real dockerized RotorHazard** where one seated node is
//! *intentionally silent*: its emulated node stream carries a flat baseline and **never
//! crosses**, so RotorHazard records not a single pass for it. That is the field failure this
//! test locks down — a mis-tuned gate or a weak VTX meant the timer missed every crossing, and
//! the pilot's whole race has to be reconstructed by hand from the RSSI trace.
//!
//! Before the fix the lap list was derived *purely from observed passes*, so a competitor with
//! no passes was absent from the projection entirely — and the Marshaling page, which keys its
//! rows off the lap list, had nothing to render. Zero laps is precisely when marshaling matters
//! most, and it was the one case that could not be marshaled.
//!
//! The test asserts, over a real timer's log:
//!
//! 1. the silent competitor **is in the lap list**, with zero laps (seeded from the heat's
//!    lineup, not from detections);
//! 2. its **signal trace is non-empty** — the RSSI streamed all along, so the evidence the RD
//!    reconstructs from was always there;
//! 3. two appended [`Event::LapInserted`] rulings turn that into a real recovered lap, and
//!    the **scored result** picks it up (the competitor goes from absent to placed).
//!
//! Assertions are **structural** (presence, counts, sample counts) — never exact µs — because
//! the mock interface reads its CSV continuously, so lap timing is not controllable (see the
//! harness docs and testing-strategy §5.1).
//!
//! Local-only class (needs Docker). Run via:
//!
//! ```sh
//! cargo test -p gridfpv-engine --features live --test zero_lap_marshaling_live -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

mod common;

use common::run_mock_heat_with_signal_until;

use gridfpv_engine::event::score_marshaled;
use gridfpv_engine::scoring::WinCondition;
use gridfpv_events::{AdapterId, CompetitorRef, Event, SourceTime};
use gridfpv_projection::{CompetitorKey, lap_list, signal_trace};
use gridfpv_testkit::{NodeCsv, NodePlan, node_csv, plan_csv};

/// DISTINCT port for the zero-lap marshaling e2e (heat 5032, scoring 5033, marshaling 5034,
/// format 5035, timed-qual 5036, zippyq 5038, multiclass 5039; 5040–5043 are the full-event /
/// server / app e2es), so it never clashes with another live harness.
const PORT: u16 = 5044;
const HEAT: &str = "q-zero-lap";

/// The node that flies; the node that is armed-but-never-crosses.
const FLYING: &str = "node-0";
const SILENT: &str = "node-1";

/// The competitor's lap-list entry, if the projection has one at all.
fn entry<'a>(
    list: &'a gridfpv_projection::LapList,
    competitor: &str,
) -> Option<&'a gridfpv_projection::CompetitorLaps> {
    list.competitors
        .iter()
        .find(|c| c.competitor.competitor.0 == competitor)
}

#[test]
#[ignore = "requires Docker (spins up dockerized RotorHazard and drives a full heat)"]
fn a_competitor_the_timer_never_detected_is_marshalable() {
    // node-0 flies rapid laps; node-1 is seated with a flat, lap-less stream — present (RSSI
    // flows every heartbeat) but never crossing. `NodePlan { laps: vec![], .. }` is exactly the
    // testkit's "armed but grounded" node: RotorHazard reads its mock data and reports its RSSI,
    // and records no pass for it.
    //
    // The flying node uses the LOOPING `node_csv` (not a finite plan): RH reads the mock CSV
    // continuously from container start, decoupled from race start, so a finite plan's crossings
    // are all consumed during the harness's several-second settle/reset and the race itself sees
    // a flat, already-exhausted stream. `ticks_per_lap: 20` (~2s laps) matches the cadence
    // `marshaling_live` settled on: the old `2` (0.2s laps) made the pass count race the drain
    // window, so this test's "the flying node completed a lap" assertion passed or failed on
    // luck — it failed in the 2026-08-24 matrix run with exactly one pass (0 completed laps).
    // Holding the race open for MIN_PASSES crossings is what makes it deterministic.
    let scenario = vec![
        (
            0usize,
            node_csv(&NodeCsv {
                ticks_per_lap: 20,
                peak_rssi: 150,
                baseline_rssi: 70,
                seed: 0,
            }),
        ),
        (
            1usize,
            plan_csv(&NodePlan {
                laps: vec![],
                baseline_rssi: 70,
                seed: 1,
                ..NodePlan::default()
            }),
        ),
    ];

    // Three crossings on the flying node ⇒ at least two completed laps, so the mixed-heat
    // assertion below cannot be decided by the drain window.
    const MIN_PASSES: usize = 3;
    let log = run_mock_heat_with_signal_until(PORT, HEAT, &scenario, MIN_PASSES);

    // Sanity: the timer really did record nothing for the silent node. If RH detected a phantom
    // crossing on the flat stream the scenario is not exercising the bug, so fail loudly.
    let silent_passes = log
        .iter()
        .filter(|e| matches!(e, Event::Pass(p) if p.competitor.0 == SILENT))
        .count();
    assert_eq!(
        silent_passes, 0,
        "the silent node must record NO passes — that is the whole scenario"
    );

    let laps = lap_list(&log);

    // (1) The zero-lap competitor is in the lap list, seeded from the heat's LINEUP. This is the
    //     regression: before #388 it was absent entirely and could not be marshaled.
    let silent = entry(&laps, SILENT).unwrap_or_else(|| {
        panic!(
            "the silent competitor must appear in the lap list; got {:?}",
            laps.competitors
                .iter()
                .map(|c| c.competitor.competitor.0.as_str())
                .collect::<Vec<_>>()
        )
    });
    assert_eq!(
        silent.lap_count(),
        0,
        "it recorded no crossings, so it has no laps"
    );
    assert!(silent.voided.is_empty(), "nothing was voided");
    let flying = entry(&laps, FLYING).expect("the flying competitor is in the lap list");
    assert!(
        flying.lap_count() >= 1,
        "the flying node must have completed laps for this to be a mixed heat"
    );

    // (2) Its signal trace is right there, keyed to the SAME `CompetitorKey` the lap-list entry
    //     carries — so the Marshaling page can render the evidence and the RD can read the
    //     missed crossings off it. RSSI flows per seated node regardless of detections.
    let trace = signal_trace(&log);
    let silent_trace = trace
        .competitor(&silent.competitor)
        .unwrap_or_else(|| panic!("the silent competitor must have a captured trace"));
    assert!(
        !silent_trace.samples.is_empty(),
        "the trace must carry real RSSI samples — it is the only evidence of this pilot's race"
    );
    println!(
        "silent {} : {} laps, {} captured RSSI samples",
        SILENT,
        silent.lap_count(),
        silent_trace.samples.len()
    );

    // (3) The recovery: the RD reads two crossings off the trace and inserts them. Anchor them on
    //     the flying node's real passes so both instants fall inside the race window; times are
    //     never asserted, only the structure the inserts produce.
    let first = log
        .iter()
        .find_map(|e| match e {
            Event::Pass(p) if p.competitor.0 == FLYING => Some(p.at),
            _ => None,
        })
        .expect("the flying node produced a real pass to anchor on");
    // Two crossings two seconds apart — one recovered lap. Exact µs are never asserted; the
    // spacing only has to be positive and deterministic.
    let second = SourceTime::from_micros(first.micros + 2_000_000);

    let adapter: AdapterId = silent.competitor.adapter.clone();
    let mut recovered = log.clone();
    for at in [first, second] {
        recovered.push(Event::LapInserted {
            adapter: adapter.clone(),
            competitor: CompetitorRef(SILENT.into()),
            at,
            heat: None,
        });
    }

    // The lap projection folds the rulings into one recovered lap for a competitor that had none.
    let after = gridfpv_projection::lap_list_marshaled(
        recovered.iter().enumerate().map(|(i, e)| (i as u64, e)),
    );
    let silent_after = entry(&after, SILENT).expect("still present after the inserts");
    assert_eq!(
        silent_after.lap_count(),
        1,
        "two inserted crossings are one recovered lap"
    );
    assert_eq!(
        silent_after.laps[0].duration_micros, 2_000_000,
        "the recovered lap spans the two inserted crossings"
    );

    // …and the SCORER picks it up: absent from the result before, placed after. The scorer folds
    // the same `corrected_passes` the lap list does, so the two can never disagree.
    let key = CompetitorKey {
        adapter,
        competitor: CompetitorRef(SILENT.into()),
    };
    let start = SourceTime::from_micros(first.micros);
    let before_result = score_marshaled(&log, WinCondition::BestLap, start);
    assert!(
        !before_result.places.iter().any(|p| p.competitor == key),
        "with no detections the silent competitor is not scored"
    );
    let after_result = score_marshaled(&recovered, WinCondition::BestLap, start);
    let placed = after_result
        .places
        .iter()
        .find(|p| p.competitor == key)
        .expect("the recovered competitor must now be scored");
    assert_eq!(placed.laps, 1, "the marshaled lap counts");
    println!(
        "recovered {} : placed P{} with {} lap(s), best {:?}µs",
        SILENT, placed.position, placed.laps, placed.best_lap_micros
    );
}
