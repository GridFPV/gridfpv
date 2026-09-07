//! Marshaling end-to-end test (#31) — the projection's marshaling fold over a real
//! mock-RH heat log.
//!
//! Drives a full heat against a **real dockerized RotorHazard** via the shared
//! [`common::run_mock_heat_until`] harness — held open for a stated number of crossings, so
//! the heat's detection count is the scenario's choice rather than a timing race — tags the
//! returned canonical log with append offsets (index == offset), then exercises the
//! marshaling-aware lap projection
//! ([`gridfpv_projection::lap_list_marshaled`]) against a real timer's passes:
//!
//! - picks a real [`Event::Pass`] and appends a [`Event::DetectionVoided`] of it,
//!   asserting the marshaled lap list loses exactly that detection versus the
//!   un-marshaled [`gridfpv_projection::lap_list`];
//! - appends a [`Event::LapInserted`] for a real competitor, asserting the marshaled
//!   lap list gains a detection for that competitor.
//!
//! Assertions are **structural** (detection counts per competitor, presence of the
//! correction) — never exact µs — because the mock interface reads its CSV
//! continuously, so lap timing is not controllable (see the harness docs and
//! testing-strategy §5.1). The raw passes themselves are never mutated: the
//! corrections are *appended* events the projection folds over a byte-identical log.
//!
//! Local-only class (needs Docker). Run via:
//!
//! ```sh
//! cargo test -p gridfpv-engine --features live --test marshaling_live -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

mod common;

use common::run_mock_heat_until;

use gridfpv_events::{Event, LogRef, SourceTime};
use gridfpv_projection::{CompetitorKey, corrected_passes, lap_list, lap_list_marshaled};
use gridfpv_testkit::{NodeCsv, node_csv};

/// Distinct port for the marshaling e2e (heat e2e uses 5032, adapters 5030/5031) so
/// the harnesses can in principle coexist; `cargo xtask live` still runs them serially.
const PORT: u16 = 5034;
const HEAT: &str = "q-marshal";

/// How many real crossings this heat is driven for. The harness holds the race open until
/// they have all landed, so the log's detection count is this number — not whatever the
/// stop-and-drain window happened to catch (see [`common::run_mock_heat_until`]).
const PASSES: usize = 4;

/// Total surviving detections (lap-gate passes) in the corrected view of `log`.
///
/// Counted off [`corrected_passes`] — the *same* fold the lap list is built from — and NOT
/// off `laps.len() + 1` per competitor, which is what this test used to do. That old metric
/// assumed "a competitor is in the list iff it has at least one corrected pass", and that has
/// not been true for some time: an entry outlives its detections. A void leaves the removal
/// record behind (so the RD can un-void it) and a lineup seat is seeded from `HeatScheduled`
/// (#388), either of which keeps a competitor present with an empty lap chain. `laps + 1` then
/// counts a phantom detection for that emptied entry — so at one real pass it read 1 both
/// before and after the void, and "a void removes exactly one detection" failed 1 != 0.
/// Counting corrected passes is exact at every count, the 1 -> 0 edge included.
fn detection_count(log: &[Event]) -> usize {
    corrected_passes(tagged(log)).len()
}

/// Tag a log with its append offsets: the storage layer assigns dense offsets in
/// append order, so the slice index is the offset.
fn tagged(log: &[Event]) -> Vec<(u64, &Event)> {
    log.iter().enumerate().map(|(i, e)| (i as u64, e)).collect()
}

#[test]
#[ignore = "requires Docker (spins up dockerized RotorHazard and drives a full heat)"]
fn marshaling_folds_corrections_over_a_real_heat_log() {
    // One node at a ~2s lap cadence (`ticks_per_lap: 20` over the 0.1s tick), driven for
    // exactly [`PASSES`] crossings. The cadence is deliberately much LONGER than the harness's
    // ~1s stop-and-drain window: the race closes within one 250ms poll of the PASSES-th
    // crossing and the next one is ~2s away, so the drain adds none and the heat yields the
    // stated number of detections run after run. (It used to run `ticks_per_lap: 2` and stop on
    // the first crossing, which yielded anywhere from 1 to a handful — and the assertions below
    // then held or failed depending on which.) Timing is still never asserted, only structure.
    let scenario = vec![(
        0usize,
        node_csv(&NodeCsv {
            ticks_per_lap: 20,
            peak_rssi: 150,
            baseline_rssi: 70,
            seed: 0,
        }),
    )];

    let log = run_mock_heat_until(PORT, HEAT, &scenario, PASSES);

    // The un-marshaled baseline over the real log.
    let baseline = lap_list(&log);
    let base_detections = detection_count(&log);
    assert_eq!(
        base_detections, PASSES,
        "the harness drives the heat for exactly {PASSES} crossings; got {base_detections}"
    );

    // Folding the offset-tagged log with no adjudications must equal `lap_list`.
    assert_eq!(
        lap_list_marshaled(tagged(&log)),
        baseline,
        "marshaling fold with no rulings must match the un-marshaled lap list"
    );

    // --- DetectionVoided: pick a real Pass, append a void of it, assert it's gone ---
    let (void_offset, voided_pass) = log
        .iter()
        .enumerate()
        .find_map(|(i, e)| match e {
            Event::Pass(p) if p.gate.is_lap_gate() => Some((i as u64, p.clone())),
            _ => None,
        })
        .expect("the real heat produced at least one lap-gate pass");

    let mut voided_log = log.clone();
    voided_log.push(Event::DetectionVoided {
        target: LogRef(void_offset),
    });
    let voided_list = lap_list_marshaled(tagged(&voided_log));
    assert_eq!(
        detection_count(&voided_log),
        base_detections - 1,
        "voiding one real pass must remove exactly one detection"
    );

    // The competitor is still THERE, one detection lighter, carrying the removal record — a
    // void empties a lap chain, it never makes a competitor vanish. That is what lets the RD
    // see (and un-void) the ruling they just made, and it holds all the way down to the last
    // detection: the entry survives with zero laps. (Locked as a pure fold in
    // `gridfpv_projection`'s `voiding_a_competitors_only_pass_leaves_it_present_with_zero_laps`.)
    let voided_key = CompetitorKey {
        adapter: voided_pass.adapter.clone(),
        competitor: voided_pass.competitor.clone(),
    };
    let voided_entry = voided_list
        .competitor(&voided_key)
        .expect("voiding a detection must not drop the competitor from the lap list");
    assert_eq!(
        voided_entry.voided.len(),
        1,
        "the void is recorded on the competitor's removal record: {voided_entry:?}"
    );
    assert_eq!(
        voided_entry.laps.len(),
        base_detections - 2,
        "K surviving detections are K-1 laps"
    );

    // The raw passes are appended-to, never mutated: every original event still
    // serialises byte-for-byte the same in the extended log.
    for (orig, ext) in log.iter().zip(voided_log.iter()) {
        assert_eq!(
            serde_json::to_string(orig).unwrap(),
            serde_json::to_string(ext).unwrap(),
            "appending a correction must not mutate any prior raw event"
        );
    }

    // --- LapInserted: recover a missed lap for that real competitor, assert +1 ---
    let competitor = voided_pass.competitor.clone();
    let adapter = voided_pass.adapter.clone();
    let mut inserted_log = log.clone();
    inserted_log.push(Event::LapInserted {
        adapter: adapter.clone(),
        competitor: competitor.clone(),
        // A timestamp on the source clock; structural-only, exact µs irrelevant.
        at: SourceTime::from_micros(voided_pass.at.micros + 1),
        // Untagged: this fold is over the heat's own log, so there is no other heat to
        // route it away from (the field is additive — a real console insert carries the
        // marshaled heat so a later heat cannot absorb it). Positional attribution is
        // exactly what this test asserts: the insertion lands on the competitor's own key.
        heat: None,
    });
    let inserted_list = lap_list_marshaled(tagged(&inserted_log));
    assert_eq!(
        detection_count(&inserted_log),
        base_detections + 1,
        "inserting a recovered lap must add exactly one detection"
    );

    // The inserted detection lands on the real competitor's key.
    let key = CompetitorKey {
        adapter,
        competitor,
    };
    let base_laps = baseline.competitor(&key).map(|c| c.laps.len()).unwrap_or(0);
    let inserted_laps = inserted_list
        .competitor(&key)
        .map(|c| c.laps.len())
        .unwrap_or(0);
    assert_eq!(
        inserted_laps,
        base_laps + 1,
        "the inserted lap must show up on the real competitor's lap list"
    );

    println!(
        "marshaling e2e: baseline {base_detections} detections; void -> {}, insert -> {}",
        detection_count(&voided_log),
        detection_count(&inserted_log),
    );
}
