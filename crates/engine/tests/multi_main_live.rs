//! Multi-tier mains end-to-end test (#13) — real tiered mains over real mock-RH heats.
//!
//! Drives a small multi-main final (6 pilots, `main_size` 3 → an A main and a B main)
//! where every main is a **real** dockerized RotorHazard heat run through the shared
//! [`common::run_mock_heat`] harness. The [`MultiMain`] generator emits both mains in one
//! [`GeneratorStep::Run`]; for each we seat the lineup onto RotorHazard nodes (a busy
//! continuously-lapping stream for the intended in-main winner, DNF streams for the rest),
//! run the heat, score it with [`score_events`], translate the node placements back onto
//! the main's competitor refs, and feed every [`CompletedHeat`] back. We assert the format
//! terminates and the final ranking orders the **whole A main above the whole B main** —
//! the defining property of tiered mains.
//!
//! As with the other format e2es, the mock reads its CSV continuously (lap timing is not
//! controllable), so this is **structural / tolerant**: we rely only on "the busy node
//! out-laps the DNF nodes", never on exact lap times.
//!
//! For RH the canonical pass `at` is **ms since race start**, so the timed clock starts at
//! zero (`race_start = SourceTime::from_micros(0)`).
//!
//! Local-only class (needs Docker). DISTINCT port 5044. Run:
//!
//! ```sh
//! cargo test -p gridfpv-engine --features live --test multi_main_live -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

mod common;

use common::run_mock_heat;

use gridfpv_engine::format::{CompletedHeat, Generator, GeneratorStep, HeatPlan};
use gridfpv_engine::multi_main::MultiMain;
use gridfpv_engine::scoring::Placement;
use gridfpv_engine::scoring::{HeatResult, WinCondition, score_events};
use gridfpv_events::{CompetitorRef, SourceTime};
use gridfpv_projection::CompetitorKey;
use gridfpv_testkit::{NodeCsv, node_csv, plan_csv, scenarios};

/// DISTINCT port for the multi-main e2e (heat e2e 5032, scoring 5033, single-elim 5037).
const PORT: u16 = 5044;

/// A continuously-lapping stream for the main's intended winner.
fn busy_stream() -> String {
    node_csv(&NodeCsv {
        ticks_per_lap: 2,
        peak_rssi: 180,
        baseline_rssi: 70,
        seed: 0,
    })
}

/// A drop-out stream: a couple of early laps then flat — far fewer laps than the busy one.
fn dnf_stream() -> String {
    plan_csv(&scenarios::dnf(2, 6))
}

/// The seat node index behind a `node-{i}` competitor ref, if it has that shape.
fn node_index(key: &CompetitorKey) -> Option<usize> {
    key.competitor.0.strip_prefix("node-")?.parse().ok()
}

/// Rebuild a placement under the main's competitor `as_ref`, preserving position/laps.
fn remap(place: &Placement, as_ref: &CompetitorRef) -> Placement {
    Placement {
        competitor: CompetitorKey {
            adapter: place.competitor.adapter.clone(),
            competitor: as_ref.clone(),
        },
        position: place.position,
        laps: place.laps,
        metric: place.metric,
        disqualified: place.disqualified,
    }
}

/// Run one main against real RotorHazard and return its scored [`HeatResult`] expressed in
/// the main's own competitor refs. `winner` (seated on node 0, busy stream) is intended to
/// win; everyone else is a DNF node. Node placements are translated back by lineup position.
fn run_main_heat(plan: &HeatPlan, winner: &CompetitorRef) -> CompletedHeat {
    let mut ordered: Vec<&CompetitorRef> = Vec::with_capacity(plan.lineup.len());
    ordered.push(winner);
    for c in &plan.lineup {
        if c != winner {
            ordered.push(c);
        }
    }

    let scenario: Vec<(usize, String)> = ordered
        .iter()
        .enumerate()
        .map(|(node, _)| {
            let stream = if node == 0 {
                busy_stream()
            } else {
                dnf_stream()
            };
            (node, stream)
        })
        .collect();

    let log = run_mock_heat(PORT, &plan.heat.0, &scenario);
    let race_start = SourceTime::from_micros(0);
    let scored = score_events(
        &log,
        WinCondition::Timed {
            window_micros: 10 * 60 * 1_000_000,
        },
        race_start,
    );

    let mut places: Vec<Placement> = Vec::new();
    let mut seen: Vec<bool> = vec![false; ordered.len()];
    for place in &scored.places {
        if let Some(node) = node_index(&place.competitor) {
            if node < ordered.len() {
                seen[node] = true;
                places.push(remap(place, ordered[node]));
            }
        }
    }
    let mut next_pos = places.len() as u32 + 1;
    for (node, present) in seen.iter().enumerate() {
        if !present {
            places.push(Placement {
                competitor: CompetitorKey {
                    adapter: scored
                        .places
                        .first()
                        .map(|p| p.competitor.adapter.clone())
                        .unwrap_or_else(|| gridfpv_events::AdapterId("rotorhazard".into())),
                    competitor: ordered[node].clone(),
                },
                position: next_pos,
                laps: 0,
                metric: gridfpv_engine::scoring::Metric::LastLapAt(None),
                ..Default::default()
            });
            next_pos += 1;
        }
    }

    CompletedHeat::new(
        plan.heat.0.clone(),
        HeatResult {
            places,
            ..Default::default()
        },
    )
}

#[test]
#[ignore = "requires Docker (spins up dockerized RotorHazard and drives full heats)"]
fn two_tier_mains_run_over_real_heats_and_rank_a_above_b() {
    // 6 qualifying-ranked pilots, main_size 3 → A main (1,2,3) and B main (4,5,6).
    let field: Vec<CompetitorRef> = ["1", "2", "3", "4", "5", "6"]
        .iter()
        .map(|n| CompetitorRef(n.to_string()))
        .collect();
    let mut generator = MultiMain::new(field.clone(), 3);

    let a_field: Vec<&str> = vec!["1", "2", "3"];
    let b_field: Vec<&str> = vec!["4", "5", "6"];

    let mut completed: Vec<CompletedHeat> = Vec::new();
    let mut steps = 0;
    while let GeneratorStep::Run(heats) = generator.next(&completed) {
        steps += 1;
        assert!(
            steps < 4,
            "tiered mains should converge in a single Run step"
        );
        assert!(!heats.is_empty(), "a Run step must carry at least one heat");
        for plan in &heats {
            // Top seed present in each main takes the busy stream (smallest numeric ref).
            let winner = plan
                .lineup
                .iter()
                .min_by_key(|c| c.0.parse::<u32>().unwrap_or(u32::MAX))
                .cloned()
                .expect("non-empty lineup");
            eprintln!(
                "multi-main e2e: heat {} lineup {:?} intended winner {}",
                plan.heat.0,
                plan.lineup.iter().map(|c| &c.0).collect::<Vec<_>>(),
                winner.0
            );
            completed.push(run_main_heat(plan, &winner));
        }
    }

    // The format terminated; every pilot appears in the final ranking.
    let ranking = generator.ranking(&completed);
    assert_eq!(ranking.len(), 6, "every pilot should appear in the ranking");

    // The defining property: the whole A main outranks the whole B main. The worst
    // A-main position is strictly better (smaller) than the best B-main position.
    let pos = |c: &str| {
        ranking
            .iter()
            .find(|e| e.competitor.0 == c)
            .unwrap_or_else(|| panic!("missing {c} in ranking"))
            .position
    };
    let worst_a = a_field.iter().map(|c| pos(c)).max().unwrap();
    let best_b = b_field.iter().map(|c| pos(c)).min().unwrap();
    assert!(
        worst_a < best_b,
        "every A-main finisher must rank above every B-main finisher \
         (worst A pos {worst_a} vs best B pos {best_b})"
    );

    eprintln!(
        "multi-main e2e: final order {:?}",
        ranking.iter().map(|e| &e.competitor.0).collect::<Vec<_>>()
    );
}
