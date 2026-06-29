//! Single-elimination end-to-end test (#34) — a real bracket over real mock-RH heats.
//!
//! Drives a small single-elimination bracket (4 seeds → 2 semis → 1 final) where every
//! heat is a **real** dockerized RotorHazard heat run through the shared
//! [`common::run_mock_heat`] harness. Each round, the [`SingleElim`] generator emits the
//! [`HeatPlan`]s; for each plan we map its two competitors onto two RotorHazard nodes —
//! the **intended winner** gets a continuously-lapping `node_csv` stream, the opponent a
//! `dnf` plan that drops out early — run the heat, score it with
//! [`score_events`], translate the node placements back onto the bracket's competitor
//! refs, and feed the [`CompletedHeat`] back into the generator. We assert the bracket
//! advances round by round and a single winner emerges (the top seed, which is given the
//! busy stream in every heat it flies).
//!
//! As with the scoring e2e, the mock reads its CSV continuously (lap timing is not
//! controllable), so this is **structural**: we rely only on "the busier node out-laps
//! the DNF node", never on exact lap times. The harness guarantees at least one crossing
//! per heat and the busy node supplies plenty.
//!
//! For RH the canonical pass `at` is **ms since race start**, so the timed clock starts
//! at zero (`race_start = SourceTime::from_micros(0)`).
//!
//! Local-only class (needs Docker). DISTINCT port 5037 (heat e2e 5032, scoring 5033).
//! Run:
//!
//! ```sh
//! cargo test -p gridfpv-engine --features live --test single_elim_live -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

mod common;

use common::run_mock_heat;

use gridfpv_engine::format::{CompletedHeat, Generator, GeneratorStep, HeatPlan};
use gridfpv_engine::scoring::Placement;
use gridfpv_engine::scoring::{HeatResult, WinCondition, score_events};
use gridfpv_engine::single_elim::SingleElim;
use gridfpv_events::{CompetitorRef, SourceTime};
use gridfpv_projection::CompetitorKey;
use gridfpv_testkit::{NodeCsv, node_csv, plan_csv, scenarios};

/// DISTINCT port for the single-elim e2e (heat e2e 5032, scoring 5033, adapters
/// 5030/5031).
const PORT: u16 = 5037;

/// A continuously-lapping stream for the heat's intended winner: `node_csv` increments
/// `lap_id` for the whole file and loops at EOF, so crossings keep coming throughout the
/// live window no matter when the race actually starts.
fn busy_stream() -> String {
    node_csv(&NodeCsv {
        ticks_per_lap: 2,
        peak_rssi: 180,
        baseline_rssi: 70,
        seed: 0,
    })
}

/// A drop-out stream for the heat's intended loser: a couple of early laps, then a flat
/// `lap_id` for the rest of the file. It records far fewer laps than the busy stream.
fn dnf_stream() -> String {
    plan_csv(&scenarios::dnf(2, 6))
}

/// Run one bracket heat against real RotorHazard and return its scored [`HeatResult`]
/// **expressed in the bracket's own competitor refs**.
///
/// `winner` is the lineup entry intended to win; it is seated on node 0 with the busy
/// stream, every other lineup entry on a later node with a DNF stream. After scoring the
/// real heat (whose placements are keyed by `node-{i}` seat refs), we translate each
/// placement back to the bracket competitor at the same lineup position, so the result
/// the generator consumes is in *its* namespace — exactly what a real scheduler (#36)
/// would do when it maps a plan's lineup onto seats.
fn run_bracket_heat(plan: &HeatPlan, winner: &CompetitorRef) -> CompletedHeat {
    // Seat the intended winner first (node 0) so it gets the busy stream; everyone else
    // is a DNF node. The lineup order otherwise carries over to node order.
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
    // Most-laps over a wide window: the busy node banks strictly more laps than the
    // DNF node, so it places first. A 10-minute window is generously past the heat.
    let scored = score_events(
        &log,
        WinCondition::Timed {
            window_micros: 10 * 60 * 1_000_000,
        },
        race_start,
    );

    // Translate node-seat placements back onto the bracket's competitor refs by lineup
    // position (`node-{i}` → `ordered[i]`). A node that produced no live-window passes is
    // absent from `scored`; such competitors are appended after the present ones, keeping
    // a total order so the loser still places behind the busy winner.
    let mut places: Vec<Placement> = Vec::new();
    let mut seen: Vec<bool> = vec![false; ordered.len()];
    // First, the placements that appeared, in their finishing order, remapped.
    for place in &scored.places {
        if let Some(node) = node_index(&place.competitor) {
            if node < ordered.len() {
                seen[node] = true;
                places.push(remap(place, ordered[node]));
            }
        }
    }
    // Then any lineup entry that produced no passes, parked behind the finishers.
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
        best_lap_micros: place.best_lap_micros,
        disqualified: place.disqualified,
    }
}

#[test]
#[ignore = "requires Docker (spins up dockerized RotorHazard and drives full heats)"]
fn four_seed_bracket_runs_to_a_single_winner_over_real_heats() {
    let field: Vec<CompetitorRef> = ["1", "2", "3", "4"]
        .iter()
        .map(|n| CompetitorRef(n.to_string()))
        .collect();
    // Head-to-head bracket: round 1 (two semis) → round 2 (final). The top seed is the
    // intended winner of every heat it flies, so it should emerge as the bracket winner.
    let intended_winner = CompetitorRef("1".into());
    let mut generator = SingleElim::new(field, 2);

    let mut completed: Vec<CompletedHeat> = Vec::new();
    let mut rounds = 0;
    while let GeneratorStep::Run(heats) = generator.next(&completed) {
        rounds += 1;
        assert!(rounds < 8, "bracket should converge well within 8 rounds");
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
                "single-elim e2e: heat {} lineup {:?} intended winner {}",
                plan.heat.0,
                plan.lineup.iter().map(|c| &c.0).collect::<Vec<_>>(),
                winner.0
            );
            completed.push(run_bracket_heat(plan, &winner));
        }
    }

    // The bracket completed: exactly one winner sits at the top of the final ranking.
    let ranking = generator.ranking(&completed);
    assert_eq!(ranking.len(), 4, "every seed should appear in the ranking");
    assert_eq!(ranking[0].position, 1, "there is a single bracket winner");
    assert_eq!(
        ranking[0].competitor, intended_winner,
        "the seed given the busy stream in every heat wins the bracket"
    );
    assert!(
        ranking.iter().filter(|e| e.position == 1).count() == 1,
        "exactly one competitor holds first place"
    );
    eprintln!(
        "single-elim e2e: winner {} after {} rounds",
        ranking[0].competitor.0, rounds
    );
}
