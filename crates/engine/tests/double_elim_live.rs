//! Double-elimination end-to-end test (#13) — a real bracket over real mock-RH heats.
//!
//! Drives a small double-elimination bracket (4 seeds) where every heat — winners
//! bracket, losers bracket, and grand final — is a **real** dockerized RotorHazard heat
//! run through the shared [`common::run_mock_heat`] harness. Each round the
//! [`DoubleElim`] generator emits the [`HeatPlan`]s; for each plan we map its two
//! competitors onto two RotorHazard nodes — the **intended winner** gets a
//! continuously-lapping `node_csv` stream, the opponent a `dnf` plan that drops out
//! early — run the heat, score it with [`score_events`], translate the node placements
//! back onto the bracket's competitor refs, and feed the [`CompletedHeat`] back into the
//! generator. We assert the bracket advances through both brackets and a single champion
//! emerges (the top seed, given the busy stream in every heat it flies).
//!
//! As with the scoring / single-elim e2e the mock reads its CSV continuously (lap timing
//! is not controllable), so this is **structural**: we rely only on "the busier node
//! out-laps the DNF node", never on exact lap times. The harness guarantees at least one
//! crossing per heat and the busy node supplies plenty.
//!
//! For RH the canonical pass `at` is **ms since race start**, so the timed clock starts
//! at zero (`race_start = SourceTime::from_micros(0)`).
//!
//! Local-only class (needs Docker). DISTINCT port 5042 (heat e2e 5032, scoring 5033,
//! single-elim 5037). Run:
//!
//! ```sh
//! cargo test -p gridfpv-engine --features live --test double_elim_live -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

mod common;

use common::run_mock_heat;

use gridfpv_engine::double_elim::DoubleElim;
use gridfpv_engine::format::{CompletedHeat, Generator, GeneratorStep, HeatPlan};
use gridfpv_engine::scoring::Placement;
use gridfpv_engine::scoring::{HeatResult, WinCondition, score_events};
use gridfpv_events::{CompetitorRef, SourceTime};
use gridfpv_projection::CompetitorKey;
use gridfpv_testkit::{NodeCsv, node_csv, plan_csv, scenarios};

/// DISTINCT port for the double-elim e2e (heat e2e 5032, scoring 5033, single-elim 5037,
/// adapters 5030/5031).
const PORT: u16 = 5042;

/// A continuously-lapping stream for the heat's intended winner.
fn busy_stream() -> String {
    node_csv(&NodeCsv {
        ticks_per_lap: 2,
        peak_rssi: 180,
        baseline_rssi: 70,
        seed: 0,
    })
}

/// A drop-out stream for the heat's intended loser: a couple of early laps then flat.
fn dnf_stream() -> String {
    plan_csv(&scenarios::dnf(2, 6))
}

/// Run one bracket heat against real RotorHazard and return its scored [`HeatResult`]
/// **expressed in the bracket's own competitor refs** (same remapping shape as the
/// single-elim e2e: intended winner on node 0 with the busy stream, others DNF).
fn run_bracket_heat(plan: &HeatPlan, winner: &CompetitorRef) -> CompletedHeat {
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

    // Translate node-seat placements back onto the bracket's competitor refs by lineup
    // position; nodes that produced no live-window passes are parked behind the rest.
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

/// The seat node index behind a `node-{i}` competitor ref, if it has that shape.
fn node_index(key: &CompetitorKey) -> Option<usize> {
    key.competitor.0.strip_prefix("node-")?.parse().ok()
}

/// Rebuild a placement under the bracket competitor `as_ref`, preserving position/laps.
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

#[test]
#[ignore = "requires Docker (spins up dockerized RotorHazard and drives full heats)"]
fn four_seed_double_elim_runs_to_a_single_champion_over_real_heats() {
    let field: Vec<CompetitorRef> = ["1", "2", "3", "4"]
        .iter()
        .map(|n| CompetitorRef(n.to_string()))
        .collect();
    // Head-to-head double elimination. The top seed is the intended winner of every
    // heat it flies (winners, and the grand final), so it should emerge as champion
    // without ever needing a bracket reset.
    let intended_winner = CompetitorRef("1".into());
    let mut generator = DoubleElim::new(field, true);

    let mut completed: Vec<CompletedHeat> = Vec::new();
    let mut rounds = 0;
    while let GeneratorStep::Run(heats) = generator.next(&completed) {
        rounds += 1;
        assert!(rounds < 16, "bracket should converge well within 16 rounds");
        assert!(!heats.is_empty(), "a Run step must carry at least one heat");
        for plan in &heats {
            // The intended winner wins any heat it is in; otherwise the highest seed
            // present (smallest numeric ref) takes the busy stream and wins.
            let winner = if plan.lineup.contains(&intended_winner) {
                intended_winner.clone()
            } else {
                plan.lineup
                    .iter()
                    .min_by_key(|c| c.0.parse::<u32>().unwrap_or(u32::MAX))
                    .cloned()
                    .expect("non-empty lineup")
            };
            eprintln!(
                "double-elim e2e: heat {} lineup {:?} intended winner {}",
                plan.heat.0,
                plan.lineup.iter().map(|c| &c.0).collect::<Vec<_>>(),
                winner.0
            );
            completed.push(run_bracket_heat(plan, &winner));
        }
    }

    // The bracket completed: exactly one champion at the top of the final ranking.
    let ranking = generator.ranking(&completed);
    assert_eq!(ranking.len(), 4, "every seed should appear in the ranking");
    assert_eq!(ranking[0].position, 1, "there is a single champion");
    assert_eq!(
        ranking[0].competitor, intended_winner,
        "the seed given the busy stream in every heat wins the bracket"
    );
    assert_eq!(
        ranking.iter().filter(|e| e.position == 1).count(),
        1,
        "exactly one competitor holds first place"
    );
    // The top seed never loses, so no bracket-reset heat should have been played.
    assert!(
        !completed.iter().any(|c| c.heat.0 == "de-gf-reset"),
        "an undefeated WB champion needs no reset"
    );
    eprintln!(
        "double-elim e2e: champion {} after {} rounds",
        ranking[0].competitor.0, rounds
    );
}
