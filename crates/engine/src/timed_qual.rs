//! `timed_qual` — a timed-qualifying [`Generator`] (race-engine.html §3, §4, §7.1).
//!
//! # What qualifying is here
//!
//! Qualifying "runs rounds and aggregates flights into a ranking" (RE §3): the seeded
//! field flies a fixed number of **rounds**, each round being a *flight* (one heat over
//! the whole field for v0.3 — see the splitting note below), and a pilot's standing is
//! their **best flight** across those rounds — *not* an average, and *not* a sum. This
//! is the standard FPV qualifying rule: your single best run is the one that counts, so
//! one great flight carries you regardless of a scrappy round.
//!
//! # The qualifying metric (config-selected, best-lap by default)
//!
//! Each round is *scored* by the format's [`WinCondition`] (the e2e drives that), but
//! the cross-round **aggregation** ranks pilots by a [`QualMetric`] read from that
//! round's [`HeatResult`]:
//!
//! - [`QualMetric::BestLap`] (**default**) — a pilot's best single lap, taken as the
//!   *fastest* (smallest duration) across all their rounds. The classic "fast lap"
//!   qualifier. Pairs with [`WinCondition::BestLap`] scoring.
//! - [`QualMetric::BestConsecutive`] — the fastest consecutive-lap window (e.g. best
//!   3-lap pace), taken as the smallest sum across rounds. Pairs with
//!   [`WinCondition::BestConsecutive`].
//! - [`QualMetric::MostLaps`] — the most laps banked in any single round. The "most
//!   laps in a window" qualifier; pairs with [`WinCondition::Timed`].
//!
//! For lap-time metrics smaller is better and a pilot who never set a qualifying lap
//! ranks last; for most-laps more is better. Either way the aggregate is each pilot's
//! single best round, ranked into a tie-aware order with [`rank_by`].
//!
//! # Determinism (RE §6)
//!
//! `next` and `ranking` read no clock and no RNG: `next` is a pure function of the
//! number of completed rounds vs the configured `rounds`, and `ranking` a pure function
//! of the completed history plus the seeded field. The seed order (after any recorded
//! [`SeedingOutcome`]) is the round lineup and the final tie-break, so the same history
//! always yields the same heats and the same ranking — a recorded session replays
//! identically.
//!
//! # v0.3 scope: one heat per round
//!
//! For v0.3 a round is a single heat over the whole field. Flight grouping /
//! heat-splitting for fields too large for one heat (multiple heats per round, then
//! aggregate every pilot's best flight across *all* their heats) is a later refinement;
//! the aggregation is already written against "best result across the rounds a pilot
//! flew", so splitting only changes how heats map to rounds, not the ranking rule.
//!
//! [`rank_by`]: crate::format::rank_by
#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use gridfpv_events::CompetitorRef;

use crate::format::{
    CompletedHeat, FormatConfig, FormatRegistry, Generator, GeneratorStep, HeatPlan, RankEntry,
    rank_by,
};
use crate::scoring::Metric;

/// Which value of a round's [`HeatResult`] the cross-round aggregation ranks pilots by.
///
/// See the module docs for the qualifying semantics. The discriminants pair with the
/// matching [`crate::scoring::WinCondition`] a round is scored under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QualMetric {
    /// Fastest single lap across a pilot's rounds (smaller µs = better). The default.
    #[default]
    BestLap,
    /// Fastest consecutive-lap window across a pilot's rounds (smaller µs sum = better).
    BestConsecutive,
    /// Most laps banked in any single round (more = better).
    MostLaps,
}

impl QualMetric {
    /// Parse a `metric` config value, defaulting to [`QualMetric::BestLap`] for an
    /// absent or unrecognised value (the documented default).
    fn parse(value: Option<&str>) -> Self {
        match value {
            Some("best-consecutive") => QualMetric::BestConsecutive,
            Some("most-laps") => QualMetric::MostLaps,
            // "best-lap", anything else, or absent → the default fast-lap qualifier.
            _ => QualMetric::BestLap,
        }
    }

    /// This metric's value for one competitor in one round, as a **rank key where
    /// smaller is better**, or `None` if the round yielded no qualifying value for them
    /// (no lap / fewer than `n` laps). Aggregation takes the best (minimum) such key
    /// across a pilot's rounds.
    ///
    /// Lap-time metrics map straight through (µs, smaller better); most-laps negates the
    /// count so "more laps" becomes "smaller key" under the same min-is-best rule.
    fn round_key(self, placement_metric: Metric, laps: u32) -> Option<i64> {
        match (self, placement_metric) {
            (QualMetric::BestLap, Metric::BestLapMicros(micros)) => micros,
            (QualMetric::BestConsecutive, Metric::BestConsecutiveMicros(sum)) => sum,
            // Most-laps: ignore the placement metric, rank on the counted laps.
            (QualMetric::MostLaps, _) => Some(-(laps as i64)),
            // Metric/condition mismatch (a round scored under a different condition than
            // the configured qualifying metric): treat as "no qualifying value", so the
            // pilot ranks last for that round rather than corrupting the aggregate.
            _ => None,
        }
    }
}

/// A timed-qualifying generator: the seeded field flies `rounds` rounds, ranked by each
/// pilot's **best flight** under the configured [`QualMetric`] (RE §3, §7.1).
///
/// Construct with [`TimedQualifying::new`] or, via the registry, [`from_config`] under
/// the name [`NAME`](Self::NAME). `next` emits one heat per round until `rounds` rounds
/// are completed, then [`GeneratorStep::Complete`]; `ranking` aggregates best-flight.
///
/// [`from_config`]: TimedQualifying::from_config
pub struct TimedQualifying {
    /// The field flying each round, in seed/draw order (recorded outcome already
    /// applied). The lineup of every round and the final ranking tie-break.
    field: Vec<CompetitorRef>,
    /// How many rounds the field flies before the format completes. **`0` is open-ended**: the
    /// generator keeps emitting the next round on demand forever (the RD ends it by not requesting
    /// more) — "Heats per pilot = 0".
    rounds: usize,
    /// Which round metric the cross-round aggregation ranks by.
    metric: QualMetric,
}

impl TimedQualifying {
    /// The format name this registers under.
    pub const NAME: &'static str = "timed_qual";

    /// The default number of qualifying rounds when `rounds` is not configured.
    pub const DEFAULT_ROUNDS: usize = 3;

    /// Build over `field` for `rounds` rounds, ranked by `metric`. The field is taken in
    /// the given order (apply any [`crate::format::SeedingOutcome`] before calling, as
    /// [`from_config`](Self::from_config) does).
    pub fn new(field: Vec<CompetitorRef>, rounds: usize, metric: QualMetric) -> Self {
        Self {
            field,
            rounds,
            metric,
        }
    }

    /// The registry constructor: applies the recorded `seeding` draw to `field`, reads
    /// `rounds` (default [`DEFAULT_ROUNDS`](Self::DEFAULT_ROUNDS)) and the `metric` param
    /// (default [`QualMetric::BestLap`]).
    pub fn from_config(config: &FormatConfig) -> Box<dyn Generator> {
        let field = config.seeding.apply(&config.field);
        let rounds = config.param_usize("rounds", Self::DEFAULT_ROUNDS);
        let metric = QualMetric::parse(config.params.get("metric").map(String::as_str));
        Box::new(Self::new(field, rounds, metric))
    }

    /// Register this format under [`NAME`](Self::NAME).
    pub fn register(registry: &mut FormatRegistry) {
        registry.register(Self::NAME, Self::from_config);
    }

    /// Heat id for round `n` (1-based) — `round-1`, `round-2`, …
    fn round_id(n: usize) -> String {
        format!("round-{n}")
    }
}

impl Generator for TimedQualifying {
    fn next(&mut self, completed: &[CompletedHeat]) -> GeneratorStep {
        // One heat per round: the number of completed heats *is* the rounds run. Emit the
        // next round while any remain, otherwise the format is complete. Pure function of
        // (completed.len(), rounds) — no clock, no RNG. `rounds == 0` is **open-ended**: always
        // emit the next round (the RD ends the format by not requesting more).
        let rounds_run = completed.len();
        if self.rounds == 0 || rounds_run < self.rounds {
            let next_round = rounds_run + 1;
            GeneratorStep::Run(vec![HeatPlan::new(
                Self::round_id(next_round),
                self.field.clone(),
            )])
        } else {
            GeneratorStep::Complete
        }
    }

    fn ranking(&self, completed: &[CompletedHeat]) -> Vec<RankEntry> {
        // Aggregate each competitor's BEST round under the configured metric: their best
        // single flight wins, not their average. Seed every field member with "no value"
        // so a no-show still appears (ranked last). `None` = no qualifying value yet.
        let mut best: BTreeMap<CompetitorRef, Option<i64>> = BTreeMap::new();
        for competitor in &self.field {
            best.entry(competitor.clone()).or_insert(None);
        }

        for heat in completed {
            for place in &heat.result.places {
                let key = self.metric.round_key(place.metric, place.laps);
                let slot = best
                    .entry(place.competitor.competitor.clone())
                    .or_insert(None);
                *slot = match (*slot, key) {
                    // Take the better (smaller) of the two round keys.
                    (Some(current), Some(candidate)) => Some(current.min(candidate)),
                    (Some(current), None) => Some(current),
                    (None, candidate) => candidate,
                };
            }
        }

        // Build the rank key: pilots with a qualifying value (band 0) rank ahead of those
        // without (band 1, e.g. never set a lap). Within a band the metric key orders
        // them; rank_by adds the competitor ref as the final, total tie-break.
        let rows: Vec<(CompetitorRef, (u8, i64))> = best
            .into_iter()
            .map(|(competitor, value)| {
                let key = match value {
                    Some(metric) => (0u8, metric),
                    None => (1u8, 0),
                };
                (competitor, key)
            })
            .collect();
        rank_by(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scoring::{HeatResult, Metric, Placement};
    use gridfpv_events::AdapterId;
    use gridfpv_projection::CompetitorKey;

    const ADAPTER: &str = "demo";

    fn cref(name: &str) -> CompetitorRef {
        CompetitorRef(name.into())
    }

    fn field(names: &[&str]) -> Vec<CompetitorRef> {
        names.iter().map(|n| cref(n)).collect()
    }

    fn names(entries: &[RankEntry]) -> Vec<String> {
        entries.iter().map(|e| e.competitor.0.clone()).collect()
    }

    fn placement(name: &str, position: u32, laps: u32, metric: Metric) -> Placement {
        Placement {
            competitor: CompetitorKey {
                adapter: AdapterId(ADAPTER.into()),
                competitor: cref(name),
            },
            position,
            laps,
            metric,
            ..Default::default()
        }
    }

    /// A best-lap-scored round from `(name, fastest_lap_micros)` rows. Positions are
    /// filled by the row order (only the metric matters to the aggregation).
    fn best_lap_round(rows: &[(&str, Option<i64>)]) -> HeatResult {
        HeatResult {
            places: rows
                .iter()
                .enumerate()
                .map(|(i, (name, micros))| {
                    placement(name, (i as u32) + 1, 0, Metric::BestLapMicros(*micros))
                })
                .collect(),
            ..Default::default()
        }
    }

    /// A timed (most-laps) round from `(name, laps)` rows.
    fn timed_round(rows: &[(&str, u32)]) -> HeatResult {
        HeatResult {
            places: rows
                .iter()
                .enumerate()
                .map(|(i, (name, laps))| {
                    placement(name, (i as u32) + 1, *laps, Metric::LastLapAt(None))
                })
                .collect(),
            ..Default::default()
        }
    }

    // --- next(): emits the right rounds from history -------------------------

    #[test]
    fn emits_round_one_over_the_whole_field_first() {
        let mut g = TimedQualifying::new(field(&["A", "B", "C"]), 3, QualMetric::BestLap);
        assert_eq!(
            g.next(&[]),
            GeneratorStep::Run(vec![HeatPlan::new("round-1", field(&["A", "B", "C"]))])
        );
    }

    #[test]
    fn emits_each_subsequent_round_numbered_from_history() {
        let mut g = TimedQualifying::new(field(&["A", "B"]), 3, QualMetric::BestLap);
        // One round completed → emit round 2.
        let r1 = CompletedHeat::new("round-1", best_lap_round(&[("A", Some(2_000_000))]));
        assert_eq!(
            g.next(std::slice::from_ref(&r1)),
            GeneratorStep::Run(vec![HeatPlan::new("round-2", field(&["A", "B"]))])
        );
        // Two rounds completed → emit round 3 (the last).
        let r2 = CompletedHeat::new("round-2", best_lap_round(&[("A", Some(1_900_000))]));
        assert_eq!(
            g.next(&[r1, r2]),
            GeneratorStep::Run(vec![HeatPlan::new("round-3", field(&["A", "B"]))])
        );
    }

    #[test]
    fn completes_after_the_configured_rounds() {
        let mut g = TimedQualifying::new(field(&["A", "B"]), 2, QualMetric::BestLap);
        let completed = vec![
            CompletedHeat::new("round-1", best_lap_round(&[("A", Some(2_000_000))])),
            CompletedHeat::new("round-2", best_lap_round(&[("A", Some(1_900_000))])),
        ];
        assert_eq!(g.next(&completed), GeneratorStep::Complete);
    }

    #[test]
    fn zero_rounds_is_open_ended_and_never_completes() {
        // "Heats per pilot = 0": the generator keeps emitting the next round forever.
        let mut g = TimedQualifying::new(field(&["A", "B"]), 0, QualMetric::BestLap);
        let mut completed: Vec<CompletedHeat> = Vec::new();
        for r in 1..=20 {
            assert_eq!(
                g.next(&completed),
                GeneratorStep::Run(vec![HeatPlan::new(
                    format!("round-{r}"),
                    field(&["A", "B"])
                )]),
                "open-ended (rounds=0) must keep emitting round {r}, never Complete"
            );
            completed.push(CompletedHeat::new(
                format!("round-{r}"),
                best_lap_round(&[("A", Some(2_000_000))]),
            ));
        }
    }

    #[test]
    fn next_is_deterministic_for_the_same_history() {
        let mut g1 = TimedQualifying::new(field(&["A", "B"]), 3, QualMetric::BestLap);
        let mut g2 = TimedQualifying::new(field(&["A", "B"]), 3, QualMetric::BestLap);
        assert_eq!(g1.next(&[]), g2.next(&[]));
    }

    // --- ranking(): best-flight aggregation ----------------------------------

    #[test]
    fn ranking_takes_each_pilots_best_round_not_their_average() {
        // The load-bearing rule: a pilot's BEST single flight counts.
        // A: rounds 2.0s then 1.5s  → best 1.5s.
        // B: a steady 1.7s both rounds → best 1.7s.
        // By average A (1.75) is worse than B (1.7), but by BEST FLIGHT A (1.5s) wins.
        let g = TimedQualifying::new(field(&["A", "B"]), 2, QualMetric::BestLap);
        let r1 = CompletedHeat::new(
            "round-1",
            best_lap_round(&[("A", Some(2_000_000)), ("B", Some(1_700_000))]),
        );
        let r2 = CompletedHeat::new(
            "round-2",
            best_lap_round(&[("A", Some(1_500_000)), ("B", Some(1_700_000))]),
        );
        let ranking = g.ranking(&[r1, r2]);
        assert_eq!(names(&ranking), vec!["A", "B"]);
        assert_eq!(ranking[0].position, 1);
        assert_eq!(ranking[1].position, 2);
    }

    #[test]
    fn ranking_no_lap_pilot_ranks_last() {
        // C never set a qualifying lap in any round → ranks behind everyone who did.
        let g = TimedQualifying::new(field(&["A", "B", "C"]), 2, QualMetric::BestLap);
        let r1 = CompletedHeat::new(
            "round-1",
            best_lap_round(&[("A", Some(2_000_000)), ("B", Some(2_500_000)), ("C", None)]),
        );
        let r2 = CompletedHeat::new(
            "round-2",
            best_lap_round(&[("A", Some(1_800_000)), ("B", Some(2_400_000)), ("C", None)]),
        );
        let ranking = g.ranking(&[r1, r2]);
        assert_eq!(names(&ranking), vec!["A", "B", "C"]);
        assert_eq!(ranking[2].position, 3);
    }

    #[test]
    fn ranking_before_any_round_lists_the_whole_field() {
        // Provisional ranking with no history: every field member appears (all band 1,
        // tie-broken by competitor ref), so the standing is well-formed from the start.
        let g = TimedQualifying::new(field(&["B", "A", "C"]), 3, QualMetric::BestLap);
        let ranking = g.ranking(&[]);
        assert_eq!(names(&ranking), vec!["A", "B", "C"]);
        // All tied at the top band with no value yet → all share position 1.
        assert!(ranking.iter().all(|e| e.position == 1));
    }

    #[test]
    fn ranking_ties_share_a_position_competition_style() {
        // A and B set the identical best lap; C is slower. 1, 1, 3.
        let g = TimedQualifying::new(field(&["A", "B", "C"]), 1, QualMetric::BestLap);
        let r1 = CompletedHeat::new(
            "round-1",
            best_lap_round(&[
                ("A", Some(2_000_000)),
                ("B", Some(2_000_000)),
                ("C", Some(2_500_000)),
            ]),
        );
        let ranking = g.ranking(&[r1]);
        assert_eq!(names(&ranking), vec!["A", "B", "C"]);
        assert_eq!(ranking[0].position, 1);
        assert_eq!(ranking[1].position, 1);
        assert_eq!(ranking[2].position, 3);
    }

    #[test]
    fn ranking_most_laps_metric_takes_best_round_lap_count() {
        // most-laps qualifier: a pilot's best single round's lap count.
        // A: 3 laps then 5 laps → best 5. B: 4 laps then 4 laps → best 4. A wins.
        let g = TimedQualifying::new(field(&["A", "B"]), 2, QualMetric::MostLaps);
        let r1 = CompletedHeat::new("round-1", timed_round(&[("B", 4), ("A", 3)]));
        let r2 = CompletedHeat::new("round-2", timed_round(&[("A", 5), ("B", 4)]));
        let ranking = g.ranking(&[r1, r2]);
        assert_eq!(names(&ranking), vec!["A", "B"]);
        assert_eq!(ranking[0].position, 1);
    }

    #[test]
    fn ranking_best_consecutive_metric_takes_smallest_sum() {
        let g = TimedQualifying::new(field(&["A", "B"]), 2, QualMetric::BestConsecutive);
        let round = |a: Option<i64>, b: Option<i64>| HeatResult {
            places: vec![
                placement("A", 1, 3, Metric::BestConsecutiveMicros(a)),
                placement("B", 2, 3, Metric::BestConsecutiveMicros(b)),
            ],
            ..Default::default()
        };
        let r1 = CompletedHeat::new("round-1", round(Some(6_000_000), Some(5_500_000)));
        // A improves to 5.0s in round 2; B holds at 5.5s. A's best (5.0) beats B's (5.5).
        let r2 = CompletedHeat::new("round-2", round(Some(5_000_000), Some(5_800_000)));
        let ranking = g.ranking(&[r1, r2]);
        assert_eq!(names(&ranking), vec!["A", "B"]);
    }

    #[test]
    fn ranking_is_deterministic_across_runs() {
        let g = TimedQualifying::new(field(&["A", "B", "C"]), 2, QualMetric::BestLap);
        let completed = vec![
            CompletedHeat::new(
                "round-1",
                best_lap_round(&[("A", Some(2_000_000)), ("B", Some(1_900_000)), ("C", None)]),
            ),
            CompletedHeat::new(
                "round-2",
                best_lap_round(&[
                    ("A", Some(1_700_000)),
                    ("B", Some(1_950_000)),
                    ("C", Some(3_000_000)),
                ]),
            ),
        ];
        assert_eq!(g.ranking(&completed), g.ranking(&completed));
    }

    // --- full loop: emit → complete → final ranking --------------------------

    #[test]
    fn full_three_round_loop_terminates_with_an_exact_final_ranking() {
        let mut g = TimedQualifying::new(field(&["A", "B", "C"]), 3, QualMetric::BestLap);
        let mut history: Vec<CompletedHeat> = Vec::new();

        // Best laps per round (µs): the aggregate best is A 1.6, B 1.8, C 1.7 → A, C, B.
        let rounds = [
            [("A", 2_000_000i64), ("B", 1_900_000), ("C", 2_100_000)],
            [("A", 1_600_000), ("B", 1_850_000), ("C", 1_700_000)],
            [("A", 1_800_000), ("B", 1_800_000), ("C", 1_950_000)],
        ];

        for (i, round) in rounds.iter().enumerate() {
            let step = g.next(&history);
            let plans = match step {
                GeneratorStep::Run(plans) => plans,
                GeneratorStep::Complete => panic!("completed early at round {i}"),
            };
            assert_eq!(plans.len(), 1);
            assert_eq!(plans[0].heat.0, format!("round-{}", i + 1));
            let rows: Vec<(&str, Option<i64>)> =
                round.iter().map(|(n, m)| (*n, Some(*m))).collect();
            history.push(CompletedHeat::new(
                plans[0].heat.0.clone(),
                best_lap_round(&rows),
            ));
        }

        // After three rounds the format completes.
        assert_eq!(g.next(&history), GeneratorStep::Complete);

        // Exact final ranking by best flight: A (1.6), C (1.7), B (1.8).
        let ranking = g.ranking(&history);
        assert_eq!(names(&ranking), vec!["A", "C", "B"]);
        assert_eq!(ranking[0].position, 1);
        assert_eq!(ranking[1].position, 2);
        assert_eq!(ranking[2].position, 3);
    }

    // --- registry build from config ------------------------------------------

    #[test]
    fn registry_builds_timed_qual_from_config() {
        let mut registry = FormatRegistry::new();
        TimedQualifying::register(&mut registry);
        assert_eq!(registry.names(), vec!["timed_qual"]);

        let cfg = FormatConfig::new(field(&["A", "B", "C"])).with_param("rounds", "2");
        let mut g = registry
            .build(TimedQualifying::NAME, &cfg)
            .expect("timed_qual is registered");
        // Round 1 over the whole field.
        assert_eq!(
            g.next(&[]),
            GeneratorStep::Run(vec![HeatPlan::new("round-1", field(&["A", "B", "C"]))])
        );
        // And it completes after exactly the configured 2 rounds.
        let completed = vec![
            CompletedHeat::new("round-1", best_lap_round(&[("A", Some(2_000_000))])),
            CompletedHeat::new("round-2", best_lap_round(&[("A", Some(1_900_000))])),
        ];
        assert_eq!(g.next(&completed), GeneratorStep::Complete);
    }

    #[test]
    fn config_defaults_to_three_best_lap_rounds() {
        // No params → DEFAULT_ROUNDS (3) and the BestLap metric.
        let cfg = FormatConfig::new(field(&["A", "B"]));
        let mut g = TimedQualifying::from_config(&cfg);
        let two_done = vec![
            CompletedHeat::new("round-1", best_lap_round(&[("A", Some(2_000_000))])),
            CompletedHeat::new("round-2", best_lap_round(&[("A", Some(1_900_000))])),
        ];
        // Still running after 2 (default is 3).
        assert!(matches!(g.next(&two_done), GeneratorStep::Run(_)));
    }

    #[test]
    fn config_seeding_draw_orders_the_round_lineup() {
        use crate::format::SeedingOutcome;
        // A recorded draw reorders the field; round 1 flies the drawn order.
        let cfg = FormatConfig::new(field(&["A", "B", "C", "D"]))
            .with_seeding(SeedingOutcome::drawn(field(&["C", "A", "D", "B"])))
            .with_param("rounds", "1");
        let mut g = TimedQualifying::from_config(&cfg);
        assert_eq!(
            g.next(&[]),
            GeneratorStep::Run(vec![HeatPlan::new("round-1", field(&["C", "A", "D", "B"]))])
        );
    }
}
