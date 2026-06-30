//! Full-event orchestration — run a whole event end to end over the existing pieces
//! (race-engine.html §3–§5, the v0.3 capstone, #37).
//!
//! Everything an event needs already exists in this crate: the scoring of a heat's
//! passes ([`crate::scoring`]), the format generators that emit and advance heats
//! ([`crate::format::Generator`]), and the marshaling-aware lap projection
//! ([`gridfpv_projection::lap_list_marshaled`]). This module is **pure orchestration**
//! over them: it does not introduce any new race logic, it only wires a qualifying
//! phase into a single-elimination bracket so a full event runs from a field to a
//! single winner.
//!
//! # The shape of a full event (RE §3, §5)
//!
//! 1. **Qualifying.** A [`crate::timed_qual::TimedQualifying`] (or any [`Generator`])
//!    runs its rounds to completion; its final [`Generator::ranking`] is the qualifying
//!    ranking — the seeds, best first.
//! 2. **Seed the bracket.** [`crate::format::advance_top_n`] takes the top `bracket_size`
//!    of that ranking and feeds them, *in qualifying-rank order*, as the seeded field of
//!    a per-level bracket generator (caller-supplied).
//! 3. **Bracket.** The bracket runs to completion; its winner is the survivor at the top
//!    of its final ranking, and that ranking is the final event standings.
//!
//! # How a heat is run — injected, so the same driver serves fixtures and live RH
//!
//! The loop is identical whether the heats come from a hand-authored fixture log or a
//! real dockerized RotorHazard: each [`crate::format::Generator`] step yields
//! [`HeatPlan`]s, the caller's [`RunHeat`] closure turns one plan into a
//! [`CompletedHeat`] (by scoring whatever log that heat produced), and the result feeds
//! back into the generator. Time and RNG never enter here — the generators read neither,
//! and the closure is the only thing that touches the outside world — so a recorded
//! event replays byte-identically (testing-strategy.html §8).
//!
//! # Marshaling folds in through scoring
//!
//! A heat's log may carry marshaling adjudications ([`gridfpv_events::Event::DetectionVoided`],
//! [`gridfpv_events::Event::LapInserted`], [`gridfpv_events::Event::LapAdjusted`]). The
//! raw passes are never mutated (architecture.html §3); instead [`score_marshaled`]
//! scores the **corrected view** of the lap-gate passes built by
//! [`gridfpv_projection::corrected_passes`] — the single home of the void/insert/adjust
//! fold that [`gridfpv_projection::lap_list_marshaled`] also folds through (#39) — so an
//! adjudication in any heat flows straight through into the qualifying ranking and the
//! bracket via the same scorer the un-marshaled path uses.
#![forbid(unsafe_code)]

use gridfpv_events::{Event, SourceTime};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::format::{CompletedHeat, Generator, GeneratorStep, HeatPlan, RankEntry, advance_top_n};
use crate::scoring::{HeatResult, WinCondition, apply_adjudications};

/// Turn one planned heat into its scored result. The single injected dependency the
/// event driver needs: it owns *how* a heat is run (replay a fixture log, drive real
/// RH, …) and returns the [`HeatResult`] the generator consumes. Pure orchestration
/// lives in [`run_format`]; everything time/IO-shaped is behind this closure.
pub trait RunHeat {
    /// Run `plan` and return its scored result.
    fn run(&mut self, plan: &HeatPlan) -> HeatResult;
}

impl<F: FnMut(&HeatPlan) -> HeatResult> RunHeat for F {
    fn run(&mut self, plan: &HeatPlan) -> HeatResult {
        self(plan)
    }
}

/// Drive any [`Generator`] to completion, running each emitted heat through `run`, and
/// return the completed-heat history together with the generator's final ranking.
///
/// This is the heat loop (RE §5) as a pure fold: `next → run → advance → next` until the
/// generator returns [`GeneratorStep::Complete`]. `max_heats` guards against a
/// misbehaving generator that never completes (a real format always converges; the cap
/// only turns a logic bug into a panic rather than a hang).
pub fn run_format(
    generator: &mut dyn Generator,
    run: &mut dyn RunHeat,
    max_heats: usize,
) -> (Vec<CompletedHeat>, Vec<RankEntry>) {
    let mut completed: Vec<CompletedHeat> = Vec::new();
    while let GeneratorStep::Run(plans) = generator.next(&completed) {
        for plan in &plans {
            assert!(
                completed.len() < max_heats,
                "format ran more than {max_heats} heats without completing"
            );
            let result = run.run(plan);
            completed.push(CompletedHeat::new(plan.heat.0.clone(), result));
        }
    }
    let ranking = generator.ranking(&completed);
    (completed, ranking)
}

/// The result of running a whole event: the qualifying ranking that seeded the bracket,
/// the bracket's final standings, and the single winner at the top of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct EventOutcome {
    /// The qualifying phase's final ranking (best seed first) — what seeds the bracket.
    pub qualifying: Vec<RankEntry>,
    /// The completed qualifying heats, in run order.
    pub qualifying_heats: Vec<CompletedHeat>,
    /// The seeded field that entered the bracket: the top `bracket_size` of `qualifying`.
    pub bracket_seeds: Vec<gridfpv_events::CompetitorRef>,
    /// The bracket's final ranking (winner first) — the event standings.
    pub bracket: Vec<RankEntry>,
    /// The completed bracket heats, in run order.
    pub bracket_heats: Vec<CompletedHeat>,
}

impl EventOutcome {
    /// The event winner: the competitor at the top of the bracket ranking.
    pub fn winner(&self) -> Option<&gridfpv_events::CompetitorRef> {
        self.bracket.first().map(|e| &e.competitor)
    }
}

/// Run a full event: drive `qualifying` to completion, take the top `bracket_size` of
/// its ranking into the bracket, run the bracket **level by level** to a single winner, and
/// return both rankings.
///
/// The qualifying generator and a per-**level** bracket generator factory are supplied by the
/// caller (so the qualifying win condition / metric and the bracket's `heat_size` are the
/// caller's choice); the same `run` closure scores every heat in both phases. `bracket_size`
/// clamps to the qualifying field, so a short field simply takes everyone into the bracket.
///
/// # Level-per-round (decisions D13, #217)
///
/// A single-elimination bracket is now **one round per level**, not one generator for the whole
/// bracket: `make_level` builds a fresh per-level [`Generator`] seeded with that level's field,
/// emits exactly that level's heats, and completes. This driver chains the levels — the **first**
/// level is seeded from the quali top-N, and each **next** level is seeded from the prior level's
/// advancers (its ranking, winners first — the same `FromHeatWinners` carry the live engine does
/// round-to-round) — until a single competitor remains. The returned `bracket` ranking is the
/// **final level's** placement (winner first), `bracket_heats` are every level's heats in order.
///
/// Pure orchestration over the existing pieces — deterministic given a deterministic `run`.
pub fn run_event(
    qualifying: &mut dyn Generator,
    mut make_level: impl FnMut(Vec<gridfpv_events::CompetitorRef>) -> Box<dyn Generator>,
    bracket_size: usize,
    run: &mut dyn RunHeat,
    max_heats: usize,
) -> EventOutcome {
    // Phase 1 — qualifying to a ranking.
    let (qualifying_heats, qualifying) = run_format(qualifying, run, max_heats);

    // Seed the first bracket level from the top of the qualifying ranking, in rank order.
    let bracket_seeds = advance_top_n(&qualifying, bracket_size);

    // Phase 2 — the bracket, level by level. Each level is its own generator seeded from the
    // previous level's advancers (winners in heat order); the chain ends when a level produces
    // a single survivor. `bracket` holds the latest level's ranking (the final standings).
    let mut bracket_heats: Vec<CompletedHeat> = Vec::new();
    let mut level_seeds = bracket_seeds.clone();
    let mut bracket: Vec<RankEntry> = qualifying_seed_ranking(&level_seeds);

    let mut level = 0usize;
    while level_seeds.len() > 1 {
        assert!(
            level < max_heats,
            "bracket ran more than {max_heats} levels without resolving a winner"
        );
        level += 1;

        // Run this one level. Each level reuses the generator's own heat ids (`se-h0`, …), so
        // scope them per level (`l{level}-…`) before scoring — mirroring how the live engine
        // scopes heat ids per round — so a fixture keyed by heat id never collides across levels.
        let mut level_gen = make_level(level_seeds.clone());
        let (level_heats, level_ranking) = run_level(level_gen.as_mut(), run, max_heats, level);
        bracket_heats.extend(level_heats);
        bracket = level_ranking;

        // The level's advancers (its ranking ahead of the eliminated) seed the next level. A
        // single-elim level eliminates exactly the competitors tied at the worst position, so
        // the advancers are everyone above that band — preserving winners-first heat order.
        let advancers = level_advancers(&bracket);
        // Guard against a degenerate level that fails to shrink the field (would loop forever).
        if advancers.len() >= level_seeds.len() {
            break;
        }
        level_seeds = advancers;
    }

    EventOutcome {
        qualifying,
        qualifying_heats,
        bracket_seeds,
        bracket,
        bracket_heats,
    }
}

/// Drive one bracket **level** to completion, scoping each emitted heat id with the level number
/// (`l{level}-{id}`) so a fixture keyed by heat id never confuses two levels that reuse the same
/// per-level ids — the engine analogue of the server scoping a round's heat ids per round.
fn run_level(
    generator: &mut dyn Generator,
    run: &mut dyn RunHeat,
    max_heats: usize,
    level: usize,
) -> (Vec<CompletedHeat>, Vec<RankEntry>) {
    let mut completed: Vec<CompletedHeat> = Vec::new();
    while let GeneratorStep::Run(plans) = generator.next(&completed) {
        for plan in &plans {
            assert!(
                completed.len() < max_heats,
                "level ran more than {max_heats} heats without completing"
            );
            // Scope the heat id with the level so the fixture / log can tell levels apart, but
            // hand the generator back its OWN id (it keyed `next` on the unscoped ids).
            let scoped = HeatPlan::new(format!("l{level}-{}", plan.heat.0), plan.lineup.clone());
            let result = run.run(&scoped);
            completed.push(CompletedHeat::new(plan.heat.0.clone(), result));
        }
    }
    let ranking = generator.ranking(&completed);
    (completed, ranking)
}

/// A trivial 1, 2, 3, … ranking from a seed order — the bracket standings before any level has
/// run (the seeds in qualifying-rank order), so a degenerate (≤1-seed) bracket still reports a
/// ranking.
fn qualifying_seed_ranking(seeds: &[gridfpv_events::CompetitorRef]) -> Vec<RankEntry> {
    seeds
        .iter()
        .enumerate()
        .map(|(i, competitor)| RankEntry {
            competitor: competitor.clone(),
            position: i as u32 + 1,
        })
        .collect()
}

/// The competitors **advancing** out of a single-elim level — everyone the level did *not*
/// eliminate. A level's [`Generator::ranking`] lists the advancers first (winners, in heat order,
/// at distinct positions) and the eliminated last, all **tied at the worst position** (each heat's
/// losers share the single bottom band). So the advancers are exactly the entries whose `position`
/// is strictly better than that worst band — which preserves the winners-first heat order the next
/// level seeds from. A degenerate level whose ranking is all one band advances no one (the loop
/// guards against that).
fn level_advancers(ranking: &[RankEntry]) -> Vec<gridfpv_events::CompetitorRef> {
    let Some(worst) = ranking.iter().map(|e| e.position).max() else {
        return Vec::new();
    };
    ranking
        .iter()
        .filter(|e| e.position < worst)
        .map(|e| e.competitor.clone())
        .collect()
}

// --- Marshaling-aware scoring ----------------------------------------------

/// Score a heat's event log under `condition`, **folding in any marshaling
/// adjudications** the log carries (#31, #37).
///
/// Scoring proper ([`score`]) consumes raw lap-gate [`Pass`]es; marshaling corrections
/// are appended events that must be folded into a *corrected view* of those passes
/// before scoring — never by mutating the raw passes (architecture.html §3).
///
/// The void/insert/adjust fold lives in **one** place — [`gridfpv_projection::corrected_passes`]
/// — and this scorer simply consumes its output (#39): rather than re-implement the same
/// last-writer-wins-by-offset / "void the void" resolution here, we hand the log's
/// positional `(offset, event)` pairs to the projection's fold (the storage layer assigns
/// these same dense append offsets) and score the corrected pass stream it returns. A log
/// with no adjudications yields the raw pass stream unchanged, so this agrees with
/// [`crate::scoring::score_events`] byte-for-byte on a clean log, and stays in lock-step
/// with [`gridfpv_projection::lap_list_marshaled`] by construction (both fold via the
/// single source of truth).
///
/// On top of the marshaling fold, this also applies the heat's **adjudications**
/// ([`gridfpv_events::Event::PenaltyApplied`] / [`gridfpv_events::Event::HeatVoided`], #13):
/// a `Disqualify` sinks a competitor below the field (flagging it), a `TimeAdded` worsens
/// their deciding time, and a `HeatVoided` flags the whole result voided — so a full event
/// run reflects penalties and heat-voids, not just lap corrections.
///
/// `race_start` is the shared race clock for [`WinCondition::Timed`] (ignored by the
/// qualifying / first-to-N conditions), matching [`crate::scoring::score`].
pub fn score_marshaled(
    events: &[Event],
    condition: WinCondition,
    race_start: SourceTime,
) -> HeatResult {
    // The single home of the marshaling fold is `gridfpv_projection::corrected_passes`;
    // tag each event with its positional append offset and fold there, then score the
    // corrected lap-gate passes it returns. The scorer re-groups/re-orders by competitor.
    // `corrected_passes` pairs each surviving pass with the global offset that addresses it
    // — **kept** here so a `LapThrownOut` (whose target is a lap's end-pass offset) excludes
    // the matching lap from the scored count.
    let corrected: Vec<(u64, gridfpv_events::Pass)> =
        gridfpv_projection::corrected_passes(events.iter().enumerate().map(|(i, e)| (i as u64, e)));
    // Penalties / heat-void / throw-outs are a *separate* fold from the marshaling corrections
    // above: apply them on the corrected pass stream so an adjudicated, marshaled heat reflects
    // both (#13). A log with no penalties scores exactly as before.
    apply_adjudications(&corrected, condition, race_start, events)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::scoring::{Metric, score_events};
    use crate::timed_qual::{QualMetric, TimedQualifying};
    use gridfpv_events::{AdapterId, CompetitorRef, GateIndex, LogRef, Pass};

    const ADAPTER: &str = "vd";

    fn cref(name: &str) -> CompetitorRef {
        CompetitorRef(name.into())
    }

    fn field(names: &[&str]) -> Vec<CompetitorRef> {
        names.iter().map(|n| cref(n)).collect()
    }

    fn pass(competitor: &str, at: i64, seq: u64) -> Event {
        Event::Pass(Pass {
            adapter: AdapterId(ADAPTER.into()),
            competitor: cref(competitor),
            at: SourceTime::from_micros(at),
            sequence: Some(seq),
            gate: GateIndex::LAP,
            signal: None,
        })
    }

    fn names(entries: &[RankEntry]) -> Vec<String> {
        entries.iter().map(|e| e.competitor.0.clone()).collect()
    }

    // --- corrected_passes / score_marshaled --------------------------------

    #[test]
    fn corrected_passes_clean_log_equals_raw() {
        // No adjudications: the corrected stream is just the raw lap-gate passes, so
        // score_marshaled agrees with score_events byte-for-byte.
        let log = vec![
            pass("A", 0, 0),
            pass("A", 2_000_000, 1),
            pass("A", 4_000_000, 2),
        ];
        let cond = WinCondition::BestLap;
        let start = SourceTime::from_micros(0);
        assert_eq!(
            score_marshaled(&log, cond, start),
            score_events(&log, cond, start)
        );
    }

    #[test]
    fn detection_voided_changes_the_score() {
        // A's middle pass (offset 1) is a phantom; voiding it removes a lap, shrinking
        // A's lap count and lengthening its remaining lap.
        let log = vec![
            pass("A", 0, 0),                              // offset 0
            pass("A", 2_000_000, 1),                      // offset 1 — phantom
            pass("A", 6_000_000, 2),                      // offset 2
            Event::DetectionVoided { target: LogRef(1) }, // offset 3
        ];
        let start = SourceTime::from_micros(0);
        let cond = WinCondition::Timed {
            window_micros: 60_000_000,
        };
        let raw = score_events(&log, cond, start);
        let marshaled = score_marshaled(&log, cond, start);
        // Raw: 2 laps (0→2, 2→6). Marshaled: 1 lap (0→6) — the void changed the result.
        assert_eq!(raw.places[0].laps, 2);
        assert_eq!(marshaled.places[0].laps, 1);
        assert_ne!(raw, marshaled);
    }

    #[test]
    fn lap_inserted_recovers_a_missed_lap() {
        // A missed lap at 3.0s is inserted; A goes from 1 lap to 2.
        let log = vec![
            pass("A", 0, 0),         // offset 0
            pass("A", 6_000_000, 1), // offset 1
            Event::LapInserted {
                adapter: AdapterId(ADAPTER.into()),
                competitor: cref("A"),
                at: SourceTime::from_micros(3_000_000),
            }, // offset 2
        ];
        let start = SourceTime::from_micros(0);
        let cond = WinCondition::Timed {
            window_micros: 60_000_000,
        };
        assert_eq!(score_events(&log, cond, start).places[0].laps, 1);
        assert_eq!(score_marshaled(&log, cond, start).places[0].laps, 2);
    }

    #[test]
    fn lap_thrown_out_excludes_a_lap_through_the_marshaled_path() {
        // The marshaled scorer (corrected_passes → apply_adjudications) excludes a thrown-out lap
        // by its corrected end-pass offset. A has 3 laps (4 passes at offsets 0..3); throw out the
        // lap ending at offset 2 → 2 counted laps. Proves the offset is preserved end-to-end.
        let clean = vec![
            pass("A", 0, 0),         // offset 0
            pass("A", 3_000_000, 1), // offset 1
            pass("A", 6_000_000, 2), // offset 2
            pass("A", 9_000_000, 3), // offset 3
        ];
        let mut thrown = clean.clone();
        thrown.push(Event::LapThrownOut {
            target: gridfpv_events::LogRef(2),
        }); // offset 4
        let cond = WinCondition::Timed {
            window_micros: 60_000_000,
        };
        let start = SourceTime::from_micros(0);
        // Clean: 3 laps. With the throw-out: 2 counted laps (via the marshaled path).
        assert_eq!(score_marshaled(&clean, cond, start).places[0].laps, 3);
        assert_eq!(score_marshaled(&thrown, cond, start).places[0].laps, 2);
    }

    #[test]
    fn lap_thrown_out_excludes_an_inserted_lap_through_the_marshaled_path() {
        // A throw-out targeting an INSERTED lap (its end_ref is the LapInserted offset) is excluded
        // by the marshaled scorer — the corrected synthetic pass carries that offset.
        let log = vec![
            pass("A", 0, 0),         // offset 0
            pass("A", 6_000_000, 1), // offset 1
            Event::LapInserted {
                adapter: AdapterId(ADAPTER.into()),
                competitor: cref("A"),
                at: SourceTime::from_micros(3_000_000),
            }, // offset 2 — inserts a lap → A has 2 laps
            Event::LapThrownOut {
                target: gridfpv_events::LogRef(2),
            }, // offset 3 — throw out the lap ending at the inserted pass
        ];
        let cond = WinCondition::Timed {
            window_micros: 60_000_000,
        };
        let start = SourceTime::from_micros(0);
        // The insert gives 2 laps; throwing out the inserted lap's end drops it back to 1 counted.
        assert_eq!(score_marshaled(&log, cond, start).places[0].laps, 1);
    }

    // --- run_format / run_event --------------------------------------------

    /// A fixed map from heat id to its scored result — a fixture "run a heat" closure.
    fn fixture(results: Vec<(&str, HeatResult)>) -> impl RunHeat {
        let map: BTreeMap<String, HeatResult> = results
            .into_iter()
            .map(|(h, r)| (h.to_string(), r))
            .collect();
        move |plan: &HeatPlan| {
            map.get(&plan.heat.0)
                .cloned()
                .unwrap_or_else(|| panic!("no fixture result for heat {}", plan.heat.0))
        }
    }

    fn best_lap_result(rows: &[(&str, Option<i64>)]) -> HeatResult {
        use crate::scoring::Placement;
        use gridfpv_projection::CompetitorKey;
        HeatResult {
            places: rows
                .iter()
                .enumerate()
                .map(|(i, (name, micros))| Placement {
                    competitor: CompetitorKey {
                        adapter: AdapterId(ADAPTER.into()),
                        competitor: cref(name),
                    },
                    position: (i as u32) + 1,
                    laps: 0,
                    metric: Metric::BestLapMicros(*micros),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn run_format_drives_qualifying_to_a_ranking() {
        let mut qual = TimedQualifying::new(field(&["A", "B", "C"]), 2, QualMetric::BestLap);
        // Best laps: A 1.6, B 1.8, C 1.7 across two rounds → A, C, B.
        let mut run = fixture(vec![
            (
                "tq-r1-h1",
                best_lap_result(&[
                    ("A", Some(2_000_000)),
                    ("B", Some(1_900_000)),
                    ("C", Some(2_100_000)),
                ]),
            ),
            (
                "tq-r2-h1",
                best_lap_result(&[
                    ("A", Some(1_600_000)),
                    ("B", Some(1_800_000)),
                    ("C", Some(1_700_000)),
                ]),
            ),
        ]);
        let (heats, ranking) = run_format(&mut qual, &mut run, 100);
        assert_eq!(heats.len(), 2);
        assert_eq!(names(&ranking), vec!["A", "C", "B"]);
    }
}
