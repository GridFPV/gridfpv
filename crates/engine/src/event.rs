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
//!    a [`crate::single_elim::SingleElim`].
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
//! builds the **corrected view** of the lap-gate passes — exactly as
//! [`gridfpv_projection::lap_list_marshaled`] does — and scores *that*, so an
//! adjudication in any heat flows straight through into the qualifying ranking and the
//! bracket via the same scorer the un-marshaled path uses.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use gridfpv_events::{Event, Pass, SourceTime};

use crate::format::{CompletedHeat, Generator, GeneratorStep, HeatPlan, RankEntry, advance_top_n};
use crate::scoring::{HeatResult, WinCondition, score};

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
#[derive(Debug, Clone, PartialEq, Eq)]
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
/// its ranking into `bracket`, drive that to a single winner, and return both rankings.
///
/// The two generators are constructed by the caller (so the qualifying win condition /
/// metric and the bracket's `heat_size` are the caller's choice); the same `run` closure
/// scores every heat in both phases. `bracket_size` clamps to the qualifying field, so a
/// short field simply takes everyone into the bracket. Pure orchestration over the
/// existing pieces — deterministic given a deterministic `run`.
pub fn run_event(
    qualifying: &mut dyn Generator,
    make_bracket: impl FnOnce(Vec<gridfpv_events::CompetitorRef>) -> Box<dyn Generator>,
    bracket_size: usize,
    run: &mut dyn RunHeat,
    max_heats: usize,
) -> EventOutcome {
    // Phase 1 — qualifying to a ranking.
    let (qualifying_heats, qualifying) = run_format(qualifying, run, max_heats);

    // Seed the bracket from the top of the qualifying ranking, in rank order.
    let bracket_seeds = advance_top_n(&qualifying, bracket_size);

    // Phase 2 — the bracket to a single winner.
    let mut bracket_gen = make_bracket(bracket_seeds.clone());
    let (bracket_heats, bracket) = run_format(bracket_gen.as_mut(), run, max_heats);

    EventOutcome {
        qualifying,
        qualifying_heats,
        bracket_seeds,
        bracket,
        bracket_heats,
    }
}

// --- Marshaling-aware scoring ----------------------------------------------

/// Score a heat's event log under `condition`, **folding in any marshaling
/// adjudications** the log carries (#31, #37).
///
/// Scoring proper ([`score`]) consumes raw lap-gate [`Pass`]es; marshaling corrections
/// are appended events that must be folded into a *corrected view* of those passes
/// before scoring — never by mutating the raw passes (architecture.html §3). This builds
/// that corrected pass stream exactly as [`gridfpv_projection::lap_list_marshaled`] does
/// — voiding voided detections, inserting recovered laps, re-timing adjusted ones, with
/// the same last-writer-wins-by-offset and "void the void" resolution — and then scores
/// it. A log with no adjudications produces the raw pass stream unchanged, so this agrees
/// with [`crate::scoring::score_events`] byte-for-byte on a clean log.
///
/// `race_start` is the shared race clock for [`WinCondition::Timed`] (ignored by the
/// qualifying / first-to-N conditions), matching [`score`].
pub fn score_marshaled(
    events: &[Event],
    condition: WinCondition,
    race_start: SourceTime,
) -> HeatResult {
    score(&corrected_passes(events), condition, race_start)
}

/// Build the **corrected** lap-gate pass stream from a heat log: apply every marshaling
/// adjudication by the offset it targets, leaving raw passes untouched, and return the
/// surviving passes (synthetic inserts included) as a fresh `Vec<Pass>`.
///
/// This mirrors [`gridfpv_projection::lap_list_marshaled`]'s fold so the scorer and the
/// lap projection agree on the corrected view; rather than duplicate every rule here it
/// resolves the same three adjudications keyed on the target's positional offset:
///
/// - [`Event::DetectionVoided`] drops the target pass / ruling from the view,
/// - [`Event::LapInserted`] adds a synthetic lap-gate pass,
/// - [`Event::LapAdjusted`] re-times the target pass,
///
/// with last-writer-wins by offset and the "void the void" / "void the adjust" cases
/// resolved exactly as the projection does. The returned passes carry absolute
/// [`SourceTime`]s (a re-time moves a pass; an insert slots in at its `at`) so the
/// timed/first-to-N conditions still see real completion times.
fn corrected_passes(events: &[Event]) -> Vec<Pass> {
    // An entry the fold can target by its append offset: a raw pass, a synthetic insert,
    // or a ruling against another offset.
    enum Entry<'a> {
        RawPass(&'a Pass),
        Inserted(Pass),
        Adjusted { target: u64, at: SourceTime },
        Voided { target: u64 },
    }

    // First pass: index every fold-relevant event by its positional offset (the storage
    // layer assigns these same dense offsets, so positional indexing mirrors the log).
    let mut entries: BTreeMap<u64, Entry<'_>> = BTreeMap::new();
    for (offset, event) in events.iter().enumerate() {
        let offset = offset as u64;
        match event {
            Event::Pass(pass) if pass.gate.is_lap_gate() => {
                entries.insert(offset, Entry::RawPass(pass));
            }
            Event::LapInserted {
                adapter,
                competitor,
                at,
            } => {
                entries.insert(
                    offset,
                    Entry::Inserted(Pass {
                        adapter: adapter.clone(),
                        competitor: competitor.clone(),
                        at: *at,
                        // A synthetic pass carries no source sequence; ordered by `at`.
                        sequence: None,
                        gate: gridfpv_events::GateIndex::LAP,
                        signal: None,
                    }),
                );
            }
            Event::LapAdjusted { target, at } => {
                entries.insert(
                    offset,
                    Entry::Adjusted {
                        target: target.0,
                        at: *at,
                    },
                );
            }
            Event::DetectionVoided { target } => {
                entries.insert(offset, Entry::Voided { target: target.0 });
            }
            // Splits, lifecycle, heat transitions, and heat/result-level rulings never
            // touch the lap-gate pass view.
            _ => {}
        }
    }

    // Resolve rulings in offset order (BTreeMap iterates ascending), last writer winning.
    // `voided[off]` drops an offset from the view; `retime[off]` overrides a pass's `at`.
    let mut voided: BTreeMap<u64, bool> = BTreeMap::new();
    let mut retime: BTreeMap<u64, SourceTime> = BTreeMap::new();
    for entry in entries.values() {
        match entry {
            Entry::RawPass(_) | Entry::Inserted(_) => {}
            Entry::Adjusted { target, at } => {
                // An adjust is the newest ruling on its target: un-void and re-time it.
                voided.insert(*target, false);
                retime.insert(*target, *at);
            }
            Entry::Voided { target } => match entries.get(target) {
                // Voiding an adjust cancels its re-time (target reverts to its raw `at`).
                Some(Entry::Adjusted {
                    target: inner_target,
                    ..
                }) => {
                    retime.remove(inner_target);
                }
                // Voiding a void resurrects the originally-voided target.
                Some(Entry::Voided {
                    target: inner_target,
                }) => {
                    voided.insert(*inner_target, false);
                }
                // Voiding a raw or inserted pass simply drops it.
                _ => {
                    voided.insert(*target, true);
                }
            },
        }
    }

    // Emit the surviving passes (raw + inserted) with any re-time applied, in offset
    // order; the scorer re-groups and re-orders them by competitor itself.
    let mut out: Vec<Pass> = Vec::new();
    for (offset, entry) in entries.iter() {
        if voided.get(offset).copied().unwrap_or(false) {
            continue;
        }
        match entry {
            Entry::RawPass(pass) => {
                let mut p = (*pass).clone();
                if let Some(at) = retime.get(offset) {
                    p.at = *at;
                }
                out.push(p);
            }
            Entry::Inserted(pass) => {
                let mut p = pass.clone();
                if let Some(at) = retime.get(offset) {
                    p.at = *at;
                }
                out.push(p);
            }
            Entry::Adjusted { .. } | Entry::Voided { .. } => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scoring::{Metric, score_events};
    use crate::single_elim::SingleElim;
    use crate::timed_qual::{QualMetric, TimedQualifying};
    use gridfpv_events::{AdapterId, CompetitorRef, GateIndex, LogRef};

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
                })
                .collect(),
        }
    }

    fn h2h(winner: &str, loser: &str) -> HeatResult {
        use crate::scoring::Placement;
        use gridfpv_projection::CompetitorKey;
        HeatResult {
            places: [(winner, 1u32, 5u32), (loser, 2, 3)]
                .iter()
                .map(|(name, position, laps)| Placement {
                    competitor: CompetitorKey {
                        adapter: AdapterId(ADAPTER.into()),
                        competitor: cref(name),
                    },
                    position: *position,
                    laps: *laps,
                    metric: Metric::LastLapAt(None),
                })
                .collect(),
        }
    }

    #[test]
    fn run_format_drives_qualifying_to_a_ranking() {
        let mut qual = TimedQualifying::new(field(&["A", "B", "C"]), 2, QualMetric::BestLap);
        // Best laps: A 1.6, B 1.8, C 1.7 across two rounds → A, C, B.
        let mut run = fixture(vec![
            (
                "round-1",
                best_lap_result(&[
                    ("A", Some(2_000_000)),
                    ("B", Some(1_900_000)),
                    ("C", Some(2_100_000)),
                ]),
            ),
            (
                "round-2",
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

    #[test]
    fn run_event_qualifying_seeds_bracket_to_a_winner() {
        // Four-pilot field: qualifying ranks A,B,C,D; a 4-seed head-to-head bracket
        // where the top seed wins every heat → A wins the event.
        let mut qual = TimedQualifying::new(field(&["A", "B", "C", "D"]), 1, QualMetric::BestLap);
        let mut run = fixture(vec![
            (
                "round-1",
                best_lap_result(&[
                    ("A", Some(1_500_000)),
                    ("B", Some(1_600_000)),
                    ("C", Some(1_700_000)),
                    ("D", Some(1_800_000)),
                ]),
            ),
            // Bracket round 1: A v D, B v C (bracket order A,D,B,C).
            ("se-r1-h0", h2h("A", "D")),
            ("se-r1-h1", h2h("B", "C")),
            // Final: A v B.
            ("se-r2-h0", h2h("A", "B")),
        ]);

        let outcome = run_event(
            &mut qual,
            |seeds| Box::new(SingleElim::new(seeds, 2)),
            4,
            &mut run,
            100,
        );

        assert_eq!(names(&outcome.qualifying), vec!["A", "B", "C", "D"]);
        assert_eq!(outcome.bracket_seeds, field(&["A", "B", "C", "D"]));
        assert_eq!(outcome.winner(), Some(&cref("A")));
        assert_eq!(outcome.bracket[0].position, 1);
        assert_eq!(
            outcome.bracket.iter().filter(|e| e.position == 1).count(),
            1,
            "exactly one event winner"
        );
    }

    #[test]
    fn run_event_is_deterministic_on_replay() {
        let build = || TimedQualifying::new(field(&["A", "B", "C", "D"]), 1, QualMetric::BestLap);
        let make_run = || {
            fixture(vec![
                (
                    "round-1",
                    best_lap_result(&[
                        ("A", Some(1_500_000)),
                        ("B", Some(1_600_000)),
                        ("C", Some(1_700_000)),
                        ("D", Some(1_800_000)),
                    ]),
                ),
                ("se-r1-h0", h2h("A", "D")),
                ("se-r1-h1", h2h("B", "C")),
                ("se-r2-h0", h2h("A", "B")),
            ])
        };

        let mut q1 = build();
        let mut r1 = make_run();
        let first = run_event(
            &mut q1,
            |s| Box::new(SingleElim::new(s, 2)),
            4,
            &mut r1,
            100,
        );

        let mut q2 = build();
        let mut r2 = make_run();
        let second = run_event(
            &mut q2,
            |s| Box::new(SingleElim::new(s, 2)),
            4,
            &mut r2,
            100,
        );

        assert_eq!(first, second, "the full event replays identically");
    }
}
