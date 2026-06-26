//! Full-event capstone — mock-RH end-to-end (#37, the v0.3 milestone).
//!
//! The live sibling of `full_event`: it drives a **whole event over real dockerized
//! RotorHazard**. A qualifying phase runs its rounds as real mock-RH heats, the scored
//! results aggregate into a qualifying ranking, that ranking seeds a single-elimination
//! bracket, the bracket runs its heats as real mock-RH heats too, and a single winner
//! emerges. This is the §5.1 mock-RH e2e from `docs/testing-strategy.html` stretched
//! across an entire event rather than a single heat.
//!
//! As with the other engine live tests, the mock reads its CSV continuously (lap timing
//! is not controllable), so this is **structural/tolerant**: every heat gives the
//! intended winner a continuously-lapping `node_csv` stream and the other seats a `dnf`
//! stream, and we rely only on "the busy node out-laps the DNF nodes", never on exact
//! µs. The qualifying metric is most-laps and the bracket is scored most-laps-in-window,
//! both of which the busy stream dominates.
//!
//! For RH the canonical pass `at` is **ms since race start**, so the timed clock starts
//! at zero (`race_start = SourceTime::from_micros(0)`).
//!
//! Local-only (needs Docker). DISTINCT port 5040. Run:
//!
//! ```sh
//! cargo test -p gridfpv-engine --features live --test full_event_live -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

mod common;

use common::run_mock_heat;

use gridfpv_engine::event::{run_event, score_marshaled};
use gridfpv_engine::format::HeatPlan;
use gridfpv_engine::scoring::{HeatResult, Metric, Placement, WinCondition};
use gridfpv_engine::single_elim::SingleElim;
use gridfpv_engine::timed_qual::{QualMetric, TimedQualifying};
use gridfpv_events::{AdapterId, CompetitorRef, SourceTime};
use gridfpv_projection::CompetitorKey;
use gridfpv_testkit::{NodeCsv, node_csv, plan_csv, scenarios};

/// DISTINCT port for the full-event e2e (heat e2e 5032, scoring 5033, single-elim 5037).
const PORT: u16 = 5040;

/// A continuously-lapping stream for a heat's intended winner.
fn busy_stream() -> String {
    node_csv(&NodeCsv {
        ticks_per_lap: 2,
        peak_rssi: 180,
        baseline_rssi: 70,
        seed: 0,
    })
}

/// A drop-out stream for the heat's other seats: a couple of early laps, then flat.
fn dnf_stream() -> String {
    plan_csv(&scenarios::dnf(2, 6))
}

/// The seat node index behind a `node-{i}` competitor ref, if it has that shape.
fn node_index(key: &CompetitorKey) -> Option<usize> {
    key.competitor.0.strip_prefix("node-")?.parse().ok()
}

/// Rebuild a placement under the event competitor `as_ref`, preserving position/laps.
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

/// Run one real mock-RH heat for `plan` with `winner` seated busy on node 0 and every
/// other lineup entry on a DNF node, score it most-laps-in-window, and translate the
/// node-seat placements back onto the event's own competitor refs.
///
/// This is the same seat-mapping a real scheduler (#36) performs: a plan's lineup maps
/// onto physical nodes, the heat is scored in node-seat space, and the result is then
/// expressed back in the format's competitor namespace. Scoring goes through
/// [`score_marshaled`] so the full-event path is identical to the core fixture replay
/// (no live heat carries adjudications, so it equals plain scoring here).
fn run_real_heat(plan: &HeatPlan, winner: &CompetitorRef) -> HeatResult {
    // Seat the intended winner first (node 0, busy); everyone else is a DNF node.
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
    let scored = score_marshaled(
        &log,
        WinCondition::Timed {
            window_micros: 10 * 60 * 1_000_000,
        },
        race_start,
    );

    // Translate node-seat placements back onto the event's competitor refs by lineup
    // position (`node-{i}` → `ordered[i]`). A node that produced no live-window passes
    // is absent from `scored`; such competitors are appended behind the finishers so
    // the result stays a total order.
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
                        .unwrap_or_else(|| AdapterId("rotorhazard".into())),
                    competitor: ordered[node].clone(),
                },
                position: next_pos,
                laps: 0,
                metric: Metric::LastLapAt(None),
                ..Default::default()
            });
            next_pos += 1;
        }
    }

    HeatResult {
        places,
        ..Default::default()
    }
}

/// The intended winner of a heat: the highest seed present in the lineup. Seeds here are
/// the numeric refs "1".."4" from the field, so the smallest numeric ref is the top
/// seed. In qualifying every pilot flies together, so the top seed wins the round (and
/// thus qualifies first); in the bracket the higher seed wins each match.
fn intended_winner(plan: &HeatPlan) -> CompetitorRef {
    plan.lineup
        .iter()
        .min_by_key(|c| c.0.parse::<u32>().unwrap_or(u32::MAX))
        .cloned()
        .expect("non-empty lineup")
}

#[test]
#[ignore = "requires Docker (spins up dockerized RotorHazard and drives a full event)"]
fn full_event_runs_to_a_single_winner_over_real_heats() {
    // A four-pilot field "1".."4". Qualifying is a single most-laps round; the top seed
    // gets the busy stream and qualifies first. The bracket then seeds off that ranking
    // and the top seed wins every heat it flies, so it emerges as the event winner.
    let field: Vec<CompetitorRef> = ["1", "2", "3", "4"]
        .iter()
        .map(|n| CompetitorRef(n.to_string()))
        .collect();

    let mut qual = TimedQualifying::new(field, 1, QualMetric::MostLaps);

    // The single injected dependency: run each emitted heat against real RH. The event
    // driver itself (run_event) is the same pure orchestration the core test exercises.
    let mut run = |plan: &HeatPlan| -> HeatResult {
        let winner = intended_winner(plan);
        eprintln!(
            "full-event e2e: heat {} lineup {:?} intended winner {}",
            plan.heat.0,
            plan.lineup.iter().map(|c| &c.0).collect::<Vec<_>>(),
            winner.0
        );
        run_real_heat(plan, &winner)
    };

    let outcome = run_event(
        &mut qual,
        |seeds| Box::new(SingleElim::new(seeds, 2)),
        4,
        &mut run,
        64,
    );

    // Qualifying produced a full ranking over the field.
    assert_eq!(
        outcome.qualifying.len(),
        4,
        "every pilot appears in the qualifying ranking"
    );
    // The busy node ("1") banks the most laps, so it qualifies first.
    assert_eq!(
        outcome.qualifying[0].competitor,
        CompetitorRef("1".into()),
        "the busy node tops qualifying"
    );

    // The bracket ran level-by-level to a single winner — the top seed, busy in every heat.
    // Level-per-round (#217): `bracket` is the FINAL level's standings (the two finalists),
    // winner first; each earlier level is its own round with its own standings.
    assert_eq!(
        outcome.bracket.len(),
        2,
        "the final level's standings are the two finalists"
    );
    assert_eq!(
        outcome.bracket[0].position, 1,
        "there is a single event winner"
    );
    assert_eq!(
        outcome.winner(),
        Some(&CompetitorRef("1".into())),
        "the seed given the busy stream in every heat wins the event"
    );
    assert_eq!(
        outcome.bracket.iter().filter(|e| e.position == 1).count(),
        1,
        "exactly one competitor holds first place"
    );

    eprintln!(
        "full-event e2e: qualifying {:?} → winner {}",
        outcome
            .qualifying
            .iter()
            .map(|e| &e.competitor.0)
            .collect::<Vec<_>>(),
        outcome.winner().unwrap().0
    );
}
