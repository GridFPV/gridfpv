//! Marshaling end-to-end test (#31) — the projection's marshaling fold over a real
//! mock-RH heat log.
//!
//! Drives a full heat against a **real dockerized RotorHazard** via the shared
//! [`common::run_mock_heat`] harness, tags the returned canonical log with append
//! offsets (index == offset), then exercises the marshaling-aware lap projection
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

use common::run_mock_heat;

use gridfpv_events::{Event, LogRef, SourceTime};
use gridfpv_projection::{CompetitorKey, LapList, lap_list, lap_list_marshaled};
use gridfpv_testkit::{NodeCsv, node_csv};

/// Distinct port for the marshaling e2e (heat e2e uses 5032, adapters 5030/5031) so
/// the harnesses can in principle coexist; `cargo xtask live` still runs them serially.
const PORT: u16 = 5034;
const HEAT: &str = "q-marshal";

/// Total detections (lap-gate passes) the corrected view holds. A competitor is in
/// the list iff it had at least one corrected pass, and `K` passes ⇒ `K - 1` laps,
/// so its detection count is `laps + 1`. Summing this is enough to prove a void
/// removed exactly one detection and an insert added exactly one.
fn detection_count(list: &LapList) -> usize {
    list.competitors.iter().map(|c| c.laps.len() + 1).sum()
}

/// Tag a log with its append offsets: the storage layer assigns dense offsets in
/// append order, so the slice index is the offset.
fn tagged(log: &[Event]) -> Vec<(u64, &Event)> {
    log.iter().enumerate().map(|(i, e)| (i as u64, e)).collect()
}

#[test]
#[ignore = "requires Docker (spins up dockerized RotorHazard and drives a full heat)"]
fn marshaling_folds_corrections_over_a_real_heat_log() {
    // One node producing rapid laps so several real passes land inside the harness's
    // live window (it stops the race shortly after the first pass, draining a final
    // ~800ms). Fast laps (`ticks_per_lap: 2` over a 0.1s tick) give us the >= 2
    // detections a void/insert needs; timing is never asserted, only structure.
    let scenario = vec![(
        0usize,
        node_csv(&NodeCsv {
            ticks_per_lap: 2,
            peak_rssi: 150,
            baseline_rssi: 70,
            seed: 0,
        }),
    )];

    let log = run_mock_heat(PORT, HEAT, &scenario);

    // The un-marshaled baseline over the real log.
    let baseline = lap_list(&log);
    let base_detections = detection_count(&baseline);
    assert!(
        base_detections >= 1,
        "the real heat must produce at least one detection to marshal; got {base_detections}"
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
        detection_count(&voided_list),
        base_detections - 1,
        "voiding one real pass must remove exactly one detection"
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
        // marshaled heat so a later heat cannot absorb it).
        heat: None,
    });
    let inserted_list = lap_list_marshaled(tagged(&inserted_log));
    assert_eq!(
        detection_count(&inserted_list),
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
        detection_count(&voided_list),
        detection_count(&inserted_list),
    );
}
