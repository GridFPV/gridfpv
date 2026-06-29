//! `round_robin` — a rounds-based [`Generator`] that **rotates** the field through a
//! fixed number of heats so pilots face a varied spread of opponents
//! (race-engine.html §3).
//!
//! # What round-robin is here
//!
//! Where [`crate::timed_qual`] flies the *whole* field together every round, a
//! round-robin **partitions** the field into heats of `heat_size` each round and
//! **rotates** the partition across `rounds` rounds, so a pilot meets a *different
//! mix* of opponents from one round to the next. A pilot's standing aggregates their
//! results across every heat they flew — by finishing-position **points** (default) or
//! by **total laps** — into one tie-aware ranking.
//!
//! This is the sibling of `timed_qual`: same "emit a round, score it, feed it back,
//! repeat until `rounds` are done" shape, but the per-round lineup is a *rotated
//! partition* rather than the whole field, and the aggregation **sums** across rounds
//! rather than taking a single best flight.
//!
//! # The rotation schedule (deterministic, balanced)
//!
//! For round `r` (0-based) the field is **cyclically rotated left** by `r` positions
//! and then sliced into consecutive heats of `heat_size`:
//!
//! ```text
//!   offset(r) = r mod field_len
//!   rotated   = field[offset..] ++ field[..offset]
//!   heats     = rotated chunked into runs of `heat_size`
//! ```
//!
//! Each round shifts the chunk boundaries by one pilot, so the partition genuinely
//! re-mixes from one round to the next: a partition repeats only once `r` has advanced
//! a full `heat_size` (the boundaries realign), so for any `heat_size ≥ 2` **every pair
//! of consecutive rounds re-pairs pilots** and a pilot meets a fresh spread of
//! opponents as the rounds progress. The schedule is a pure function of `(r,
//! field_len)` — no clock, no RNG — so it replays identically (RE §6). (With
//! `heat_size = 1` every pilot is alone in their own heat, so the partition is trivially
//! the same every round — there are no opponents to vary.)
//!
//! # Indivisible fields
//!
//! When `field_len` is not a multiple of `heat_size`, the chunking leaves a **short
//! final heat** carrying the remainder (e.g. 7 pilots at `heat_size` 3 → heats of 3,
//! 3, 1). The split is purely positional on the rotated order, so it is deterministic
//! and the short heat lands on different pilots each round as the rotation advances. A
//! `heat_size` of 0 is treated as 1 (every pilot in their own heat) so the format
//! always makes progress.
//!
//! # The aggregation metric (config-selected, points by default)
//!
//! Each heat is *scored* by the format's [`crate::scoring::WinCondition`] (the e2e
//! drives that); the cross-heat **aggregation** ranks pilots by a [`RrMetric`]:
//!
//! - [`RrMetric::Points`] (**default**) — finishing-position points, *lower position =
//!   more points*. By default a pilot in a heat of `k` scores `k - position + 1` points
//!   for that heat (the winner of a `k`-pilot heat gets `k`, last gets `1`); points sum
//!   across all the pilot's heats and **more points = better**. A configurable
//!   per-position **points table** (the `points` param, e.g. a steep MultiGP-style
//!   `25,20,16,…`) overrides the linear default via the shared
//!   [`crate::format::position_points`] — the same editable table every points-based
//!   structure scores with (D17), so a steep curve can reward winning over consistency.
//! - [`RrMetric::TotalLaps`] — the sum of laps banked across all the pilot's heats;
//!   **more laps = better**.
//!
//! Either way the aggregate is ranked into a tie-aware order with [`rank_by`], with the
//! competitor ref as the final, total, deterministic tie-break (RE §6).
//!
//! [`rank_by`]: crate::format::rank_by
#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use gridfpv_events::CompetitorRef;

use crate::format::{
    CompletedHeat, FormatConfig, FormatRegistry, Generator, GeneratorStep, HeatPlan, RankEntry,
    parse_points_table, position_points, rank_by,
};

/// Which aggregate the cross-heat ranking ranks pilots by. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RrMetric {
    /// Finishing-position points summed across heats (more = better). The default.
    #[default]
    Points,
    /// Total laps banked across heats (more = better).
    TotalLaps,
}

impl RrMetric {
    /// Parse a `metric` config value, defaulting to [`RrMetric::Points`] for an absent
    /// or unrecognised value (the documented default).
    fn parse(value: Option<&str>) -> Self {
        match value {
            Some("total-laps") | Some("laps") => RrMetric::TotalLaps,
            // "points", anything else, or absent → the default points standing.
            _ => RrMetric::Points,
        }
    }
}

/// A round-robin generator: over `rounds` rounds the field is partitioned into heats of
/// `heat_size`, the partition **rotating** each round so pilots face a varied spread of
/// opponents, then each pilot's heats are aggregated into one ranking (RE §3).
///
/// Construct with [`RoundRobin::new`] or, via the registry, [`from_config`] under the
/// name [`NAME`](Self::NAME). `next` emits one round's worth of heats at a time until
/// `rounds` rounds are completed, then [`GeneratorStep::Complete`]; `ranking` aggregates
/// across every heat under the configured [`RrMetric`].
///
/// [`from_config`]: RoundRobin::from_config
pub struct RoundRobin {
    /// The field, in seed/draw order (any recorded outcome already applied). The base
    /// order the rotation schedule rotates and the final ranking tie-break.
    field: Vec<CompetitorRef>,
    /// How many rounds the field flies before the format completes.
    rounds: usize,
    /// How many pilots share a heat within a round (1 or more; 0 is treated as 1).
    heat_size: usize,
    /// Which aggregate the cross-heat ranking ranks by.
    metric: RrMetric,
    /// The per-position points table for [`RrMetric::Points`] (`None` ⇒ the linear `k − pos + 1`
    /// default). The same editable table every points-based structure uses (D17).
    points_table: Option<Vec<u32>>,
}

impl RoundRobin {
    /// The format name this registers under.
    pub const NAME: &'static str = "round_robin";

    /// The default number of rounds when `rounds` is not configured.
    pub const DEFAULT_ROUNDS: usize = 3;

    /// The default heat size when `heat_size` is not configured.
    pub const DEFAULT_HEAT_SIZE: usize = 4;

    /// Build over `field` for `rounds` rounds of heats of `heat_size`, aggregated by
    /// `metric`. The field is taken in the given order (apply any
    /// [`crate::format::SeedingOutcome`] before calling, as
    /// [`from_config`](Self::from_config) does).
    pub fn new(
        field: Vec<CompetitorRef>,
        rounds: usize,
        heat_size: usize,
        metric: RrMetric,
    ) -> Self {
        Self {
            field,
            rounds,
            // Never let heat_size be 0, or chunking would loop forever / make no
            // progress; one pilot per heat is the sensible floor.
            heat_size: heat_size.max(1),
            metric,
            points_table: None,
        }
    }

    /// Set the per-position points table used by [`RrMetric::Points`] (builder style, so the many
    /// `new` call-sites stay unchanged). `None` keeps the linear default.
    pub fn with_points(mut self, table: Option<Vec<u32>>) -> Self {
        self.points_table = table;
        self
    }

    /// The registry constructor: applies the recorded `seeding` draw to `field`, reads
    /// `rounds` (default [`DEFAULT_ROUNDS`](Self::DEFAULT_ROUNDS)), `heat_size` (default
    /// [`DEFAULT_HEAT_SIZE`](Self::DEFAULT_HEAT_SIZE)) and the `metric` param (default
    /// [`RrMetric::Points`]).
    pub fn from_config(config: &FormatConfig) -> Box<dyn Generator> {
        let field = config.seeding.apply(&config.field);
        let rounds = config.param_usize("rounds", Self::DEFAULT_ROUNDS);
        let heat_size = config.param_usize("heat_size", Self::DEFAULT_HEAT_SIZE);
        let metric = RrMetric::parse(config.params.get("metric").map(String::as_str));
        let points_table = parse_points_table(config.params.get("points").map(String::as_str));
        Box::new(Self::new(field, rounds, heat_size, metric).with_points(points_table))
    }

    /// Register this format under [`NAME`](Self::NAME).
    pub fn register(registry: &mut FormatRegistry) {
        registry.register(Self::NAME, Self::from_config);
    }

    /// Heat id for heat `i` (1-based) of round `n` (1-based) — `rr-r1-h1`, `rr-r1-h2`,
    /// `rr-r2-h1`, …
    fn heat_id(round: usize, heat: usize) -> String {
        format!("rr-r{round}-h{heat}")
    }

    /// The cyclic rotation offset for round `r` (0-based). See the module docs: rotating
    /// by `r` shifts every chunk boundary by one pilot each round so the partition
    /// re-mixes opponents from round to round instead of repeating.
    fn offset(&self, round_index: usize) -> usize {
        let len = self.field.len();
        if len == 0 {
            return 0;
        }
        round_index % len
    }

    /// The field rotated left by [`offset`](Self::offset) for round `r` (0-based).
    fn rotated(&self, round_index: usize) -> Vec<CompetitorRef> {
        let len = self.field.len();
        if len == 0 {
            return Vec::new();
        }
        let offset = self.offset(round_index);
        let mut out = Vec::with_capacity(len);
        out.extend_from_slice(&self.field[offset..]);
        out.extend_from_slice(&self.field[..offset]);
        out
    }

    /// The heat plans for round `n` (1-based): the field rotated for this round, sliced
    /// into consecutive heats of `heat_size` (the last possibly short).
    fn round_heats(&self, round: usize) -> Vec<HeatPlan> {
        let rotated = self.rotated(round - 1);
        rotated
            .chunks(self.heat_size)
            .enumerate()
            .map(|(heat_index, chunk)| {
                HeatPlan::new(Self::heat_id(round, heat_index + 1), chunk.to_vec())
            })
            .collect()
    }

    /// How many heats one round emits (the same every round — the field partitioned).
    fn heats_per_round(&self) -> usize {
        let len = self.field.len();
        if len == 0 {
            0
        } else {
            len.div_ceil(self.heat_size)
        }
    }
}

impl Generator for RoundRobin {
    fn next(&mut self, completed: &[CompletedHeat]) -> GeneratorStep {
        // Each round emits `heats_per_round` heats; the completed count divided by that
        // is the number of rounds fully run. Emit the next round while any remain,
        // otherwise the format is complete. Pure function of (completed.len(), schedule)
        // — no clock, no RNG.
        let per_round = self.heats_per_round();
        if per_round == 0 {
            // Empty field: nothing to ever run.
            return GeneratorStep::Complete;
        }
        let rounds_run = completed.len() / per_round;
        if rounds_run < self.rounds {
            GeneratorStep::Run(self.round_heats(rounds_run + 1))
        } else {
            GeneratorStep::Complete
        }
    }

    fn ranking(&self, completed: &[CompletedHeat]) -> Vec<RankEntry> {
        // Aggregate each competitor across every heat they flew under the configured
        // metric. Seed every field member at 0 so a no-show still appears in the
        // ranking. The accumulated score is "more is better", so we negate it into a
        // smaller-is-better rank key for `rank_by`.
        let mut totals: BTreeMap<CompetitorRef, i64> = BTreeMap::new();
        // Per competitor, the **fastest single lap** they flew across every heat (the min of each
        // heat's `best_lap_micros`, ignoring heats where they completed no lap). Breaks ties on
        // points/laps: equal score → the faster best lap ranks higher.
        let mut best_lap: BTreeMap<CompetitorRef, i64> = BTreeMap::new();
        for competitor in &self.field {
            totals.entry(competitor.clone()).or_insert(0);
        }

        for heat in completed {
            let heat_size = heat.result.places.len() as i64;
            for place in &heat.result.places {
                let gain = match self.metric {
                    // Finishing-position points from the shared scoring helper: the configured
                    // per-position table, or the linear `k − pos + 1` default. Tied pilots share a
                    // position and so are awarded the same points.
                    RrMetric::Points => position_points(
                        self.points_table.as_deref(),
                        place.position,
                        heat_size as usize,
                    ),
                    // Total laps banked in this heat.
                    RrMetric::TotalLaps => place.laps as i64,
                };
                *totals
                    .entry(place.competitor.competitor.clone())
                    .or_insert(0) += gain;
                // Track the competitor's fastest lap across heats (smaller is better). A heat with
                // no completed lap contributes nothing.
                if let Some(lap) = place.best_lap_micros {
                    best_lap
                        .entry(place.competitor.competitor.clone())
                        .and_modify(|m| *m = (*m).min(lap))
                        .or_insert(lap);
                }
            }
        }

        // Rank key: negate the accumulated score so more points / laps = smaller key = better, then
        // break ties by fastest lap ascending (smaller = better; a pilot with no lap sorts after one
        // with a lap via `i64::MAX`). `rank_by` adds the competitor ref as the final, total
        // tie-break.
        let rows: Vec<(CompetitorRef, (i64, i64))> = totals
            .into_iter()
            .map(|(competitor, score)| {
                let fastest = best_lap.get(&competitor).copied().unwrap_or(i64::MAX);
                (competitor, (-score, fastest))
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
    use std::collections::HashSet;

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

    /// Build a `HeatResult` from `(name, position, laps)` rows.
    fn result(rows: &[(&str, u32, u32)]) -> HeatResult {
        HeatResult {
            places: rows
                .iter()
                .map(|(name, position, laps)| Placement {
                    competitor: CompetitorKey {
                        adapter: AdapterId(ADAPTER.into()),
                        competitor: cref(name),
                    },
                    position: *position,
                    laps: *laps,
                    metric: Metric::LastLapAt(None),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    /// Build a `HeatResult` from `(name, position, laps, best_lap_micros)` rows — the best-lap
    /// variant of [`result`], for exercising the fastest-lap tie-break.
    fn result_bl(rows: &[(&str, u32, u32, Option<i64>)]) -> HeatResult {
        HeatResult {
            places: rows
                .iter()
                .map(|(name, position, laps, best)| Placement {
                    competitor: CompetitorKey {
                        adapter: AdapterId(ADAPTER.into()),
                        competitor: cref(name),
                    },
                    position: *position,
                    laps: *laps,
                    metric: Metric::LastLapAt(None),
                    best_lap_micros: *best,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    /// The set of competitor names a heat plan lines up.
    fn lineup_names(plan: &HeatPlan) -> Vec<String> {
        plan.lineup.iter().map(|c| c.0.clone()).collect()
    }

    /// Extract the `Run` plans from a step, panicking if it completed.
    fn run(step: GeneratorStep) -> Vec<HeatPlan> {
        match step {
            GeneratorStep::Run(plans) => plans,
            GeneratorStep::Complete => panic!("expected Run, got Complete"),
        }
    }

    /// All unordered opponent pairs a pilot shares a heat with, across a round's heats.
    fn pairs(plans: &[HeatPlan]) -> HashSet<(String, String)> {
        let mut out = HashSet::new();
        for plan in plans {
            let pilots = lineup_names(plan);
            for i in 0..pilots.len() {
                for j in (i + 1)..pilots.len() {
                    let (a, b) = (pilots[i].clone(), pilots[j].clone());
                    out.insert(if a < b { (a, b) } else { (b, a) });
                }
            }
        }
        out
    }

    // --- next(): partitions and rotation -------------------------------------

    #[test]
    fn round_one_partitions_the_field_into_heats_of_heat_size() {
        let mut g = RoundRobin::new(field(&["A", "B", "C", "D"]), 3, 2, RrMetric::Points);
        let plans = run(g.next(&[]));
        // 4 pilots / heat_size 2 → two heats.
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].heat.0, "rr-r1-h1");
        assert_eq!(plans[1].heat.0, "rr-r1-h2");
        assert_eq!(lineup_names(&plans[0]), vec!["A", "B"]);
        assert_eq!(lineup_names(&plans[1]), vec!["C", "D"]);
    }

    #[test]
    fn every_round_covers_the_whole_field_exactly_once() {
        let g = RoundRobin::new(
            field(&["A", "B", "C", "D", "E", "F"]),
            4,
            2,
            RrMetric::Points,
        );
        for round in 1..=4 {
            let plans = g.round_heats(round);
            let mut seen: Vec<String> = plans.iter().flat_map(lineup_names).collect();
            seen.sort();
            assert_eq!(
                seen,
                vec!["A", "B", "C", "D", "E", "F"],
                "round {round} must cover the whole field exactly once"
            );
        }
    }

    #[test]
    fn the_partition_rotates_so_pilots_meet_different_opponents() {
        let g = RoundRobin::new(
            field(&["A", "B", "C", "D", "E", "F"]),
            3,
            2,
            RrMetric::Points,
        );
        // Each consecutive round re-pairs pilots (the partition rotates by one pilot).
        let p1 = pairs(&g.round_heats(1));
        let p2 = pairs(&g.round_heats(2));
        let p3 = pairs(&g.round_heats(3));
        assert_ne!(p1, p2, "round 2 must re-pair pilots vs round 1");
        assert_ne!(p2, p3, "round 3 must re-pair pilots vs round 2");
    }

    #[test]
    fn rotation_re_pairs_a_specific_pilot_across_rounds() {
        // A concrete opponent check: A's heat-mates change between round 1 and round 2.
        let g = RoundRobin::new(
            field(&["A", "B", "C", "D", "E", "F"]),
            2,
            3,
            RrMetric::Points,
        );
        let mates = |plans: &[HeatPlan]| -> Vec<String> {
            let heat = plans
                .iter()
                .find(|p| lineup_names(p).contains(&"A".to_string()))
                .unwrap();
            let mut m: Vec<String> = lineup_names(heat)
                .into_iter()
                .filter(|n| n != "A")
                .collect();
            m.sort();
            m
        };
        assert_ne!(
            mates(&g.round_heats(1)),
            mates(&g.round_heats(2)),
            "A should meet a different set of opponents in round 2"
        );
    }

    #[test]
    fn indivisible_field_leaves_a_short_final_heat() {
        // 7 pilots at heat_size 3 → heats of 3, 3, 1 (the remainder in a short heat).
        let mut g = RoundRobin::new(
            field(&["A", "B", "C", "D", "E", "F", "G"]),
            2,
            3,
            RrMetric::Points,
        );
        let plans = run(g.next(&[]));
        assert_eq!(plans.len(), 3);
        assert_eq!(plans[0].lineup.len(), 3);
        assert_eq!(plans[1].lineup.len(), 3);
        assert_eq!(plans[2].lineup.len(), 1, "the remainder rides a short heat");
    }

    #[test]
    fn heat_size_larger_than_field_is_one_heat() {
        let mut g = RoundRobin::new(field(&["A", "B", "C"]), 1, 8, RrMetric::Points);
        let plans = run(g.next(&[]));
        assert_eq!(plans.len(), 1);
        assert_eq!(lineup_names(&plans[0]), vec!["A", "B", "C"]);
    }

    // --- next(): completion ---------------------------------------------------

    #[test]
    fn completes_after_the_configured_rounds() {
        // 4 pilots, heat_size 2 → 2 heats/round, 2 rounds → 4 heats total.
        let mut g = RoundRobin::new(field(&["A", "B", "C", "D"]), 2, 2, RrMetric::Points);
        let mut history: Vec<CompletedHeat> = Vec::new();
        for round in 1..=2 {
            let plans = run(g.next(&history));
            assert_eq!(plans.len(), 2, "round {round} emits two heats");
            for plan in plans {
                history.push(CompletedHeat::new(
                    plan.heat.0.clone(),
                    result(&[(&plan.lineup[0].0, 1, 3), (&plan.lineup[1].0, 2, 2)]),
                ));
            }
        }
        // After both rounds (4 heats), the format completes.
        assert_eq!(g.next(&history), GeneratorStep::Complete);
    }

    #[test]
    fn emits_round_two_after_round_one_completes() {
        let mut g = RoundRobin::new(field(&["A", "B", "C", "D"]), 3, 2, RrMetric::Points);
        // Round 1's two heats come back.
        let r1 = run(g.next(&[]));
        let history: Vec<CompletedHeat> = r1
            .iter()
            .map(|p| {
                CompletedHeat::new(
                    p.heat.0.clone(),
                    result(&[(&p.lineup[0].0, 1, 3), (&p.lineup[1].0, 2, 2)]),
                )
            })
            .collect();
        let r2 = run(g.next(&history));
        assert_eq!(r2[0].heat.0, "rr-r2-h1");
        assert_eq!(r2[1].heat.0, "rr-r2-h2");
    }

    #[test]
    fn empty_field_completes_immediately() {
        let mut g = RoundRobin::new(field(&[]), 3, 2, RrMetric::Points);
        assert_eq!(g.next(&[]), GeneratorStep::Complete);
    }

    // --- next(): determinism --------------------------------------------------

    #[test]
    fn next_is_deterministic_for_the_same_history() {
        let mut g1 = RoundRobin::new(field(&["A", "B", "C", "D"]), 3, 2, RrMetric::Points);
        let mut g2 = RoundRobin::new(field(&["A", "B", "C", "D"]), 3, 2, RrMetric::Points);
        assert_eq!(g1.next(&[]), g2.next(&[]));
    }

    // --- ranking(): points aggregation ---------------------------------------

    #[test]
    fn ranking_by_points_rewards_winning_more_heats() {
        // 4 pilots, heat_size 2, 2 rounds. A wins both its heats; D loses both.
        // Points in a heat of 2: winner 2, loser 1.
        let g = RoundRobin::new(field(&["A", "B", "C", "D"]), 2, 2, RrMetric::Points);
        // Round 1: r1-h1 = A,B (A wins); r1-h2 = C,D (C wins).
        // Round 2 (rotated): use whatever heats, but make A win again and D lose again.
        let completed = vec![
            CompletedHeat::new("rr-r1-h1", result(&[("A", 1, 5), ("B", 2, 4)])),
            CompletedHeat::new("rr-r1-h2", result(&[("C", 1, 5), ("D", 2, 4)])),
            // Round 2 pairings differ, but A still wins and D still loses.
            CompletedHeat::new("rr-r2-h1", result(&[("A", 1, 5), ("D", 2, 4)])),
            CompletedHeat::new("rr-r2-h2", result(&[("C", 1, 5), ("B", 2, 4)])),
        ];
        // Points: A = 2+2 = 4; C = 2+2 = 4; B = 1+1 = 2; D = 1+1 = 2.
        // A and C tie (4), B and D tie (2). Tie-break is competitor ref.
        let ranking = g.ranking(&completed);
        assert_eq!(names(&ranking), vec!["A", "C", "B", "D"]);
        assert_eq!(ranking[0].position, 1);
        assert_eq!(ranking[1].position, 1, "A and C tie on 4 points");
        assert_eq!(ranking[2].position, 3);
        assert_eq!(ranking[3].position, 3, "B and D tie on 2 points");
    }

    #[test]
    fn ranking_a_consistent_winner_tops_the_table() {
        // A wins every heat outright → most points → first.
        let g = RoundRobin::new(field(&["A", "B", "C", "D"]), 2, 2, RrMetric::Points);
        let completed = vec![
            CompletedHeat::new("rr-r1-h1", result(&[("A", 1, 5), ("B", 2, 4)])),
            CompletedHeat::new("rr-r1-h2", result(&[("C", 1, 5), ("D", 2, 4)])),
            CompletedHeat::new("rr-r2-h1", result(&[("A", 1, 5), ("C", 2, 4)])),
            CompletedHeat::new("rr-r2-h2", result(&[("B", 1, 5), ("D", 2, 4)])),
        ];
        // A: 2+2=4; B: 1+2=3; C: 2+1=3; D: 1+1=2 → A, then B/C tie, then D.
        let ranking = g.ranking(&completed);
        assert_eq!(ranking[0].competitor, cref("A"));
        assert_eq!(ranking[0].position, 1);
        assert_eq!(ranking[3].competitor, cref("D"));
        assert_eq!(ranking[3].position, 4);
    }

    /// The same three rounds, scored two ways, to prove the editable per-position points table
    /// actually changes the standings (D17): a steep table rewards wins over consistent seconds, so
    /// it flips the runner-up. P wins twice, R wins once, Q is second all three rounds.
    fn three_round_points_fixture() -> Vec<CompletedHeat> {
        vec![
            CompletedHeat::new("rr-r1-h1", result(&[("P", 1, 9), ("Q", 2, 8), ("R", 3, 7)])),
            CompletedHeat::new("rr-r2-h1", result(&[("P", 1, 9), ("Q", 2, 8), ("R", 3, 7)])),
            CompletedHeat::new("rr-r3-h1", result(&[("R", 1, 9), ("Q", 2, 8), ("P", 3, 7)])),
        ]
    }

    #[test]
    fn points_metric_linear_default_rewards_consistency() {
        // Linear (heat of 3 → 3/2/1): P=7, Q=6, R=5 → Q's three seconds out-point R's single win.
        let g = RoundRobin::new(field(&["P", "Q", "R"]), 3, 3, RrMetric::Points);
        assert_eq!(
            names(&g.ranking(&three_round_points_fixture())),
            vec!["P", "Q", "R"]
        );
    }

    #[test]
    fn points_metric_steep_table_rewards_winning() {
        // Steep table 10/2/1: P=21, R=12, Q=6 → R's win leapfrogs Q's three seconds. The table flips
        // the runner-up versus the linear default above.
        let g = RoundRobin::new(field(&["P", "Q", "R"]), 3, 3, RrMetric::Points)
            .with_points(Some(vec![10, 2, 1]));
        assert_eq!(
            names(&g.ranking(&three_round_points_fixture())),
            vec!["P", "R", "Q"]
        );
    }

    #[test]
    fn config_reads_a_points_table_param() {
        let cfg = FormatConfig::new(field(&["P", "Q", "R"]))
            .with_param("rounds", "3")
            .with_param("heat_size", "3")
            .with_param("points", "10, 2, 1");
        let g = RoundRobin::from_config(&cfg);
        assert_eq!(
            names(&g.ranking(&three_round_points_fixture())),
            vec!["P", "R", "Q"]
        );
    }

    #[test]
    fn ranking_points_handle_heats_of_different_sizes() {
        // A wins a heat of 3 (3 points); B wins a heat of 1 (1 point, alone). A ahead.
        let g = RoundRobin::new(field(&["A", "B", "C", "D"]), 1, 3, RrMetric::Points);
        let completed = vec![
            CompletedHeat::new("rr-r1-h1", result(&[("A", 1, 5), ("C", 2, 4), ("D", 3, 3)])),
            CompletedHeat::new("rr-r1-h2", result(&[("B", 1, 6)])),
        ];
        // Points: A=3, C=2, D=1, B=1. A first; B and D tie on 1.
        let ranking = g.ranking(&completed);
        assert_eq!(ranking[0].competitor, cref("A"));
        assert_eq!(ranking[0].position, 1);
    }

    #[test]
    fn ranking_breaks_equal_points_by_fastest_single_lap() {
        // X and Y finish equal on points across the round-robin (each wins one heat, loses one),
        // but X flew a faster fastest-lap, so X ranks ahead of Y.
        let g = RoundRobin::new(field(&["X", "Y", "Z", "W"]), 2, 2, RrMetric::Points);
        let completed = vec![
            // Round 1: X beats Z; Y beats W. X best lap 2.0s, Y best lap 2.5s.
            CompletedHeat::new(
                "rr-r1-h1",
                result_bl(&[("X", 1, 5, Some(2_000_000)), ("Z", 2, 4, Some(2_400_000))]),
            ),
            CompletedHeat::new(
                "rr-r1-h2",
                result_bl(&[("Y", 1, 5, Some(2_500_000)), ("W", 2, 4, Some(2_600_000))]),
            ),
            // Round 2: X and Y both lose their heats, so each ends on the same points. X still has
            // the faster fastest-lap (2.1s vs Y's 2.5s).
            CompletedHeat::new(
                "rr-r2-h1",
                result_bl(&[("Z", 1, 5, Some(2_300_000)), ("X", 2, 4, Some(2_100_000))]),
            ),
            CompletedHeat::new(
                "rr-r2-h2",
                result_bl(&[("W", 1, 5, Some(2_700_000)), ("Y", 2, 4, Some(2_500_000))]),
            ),
        ];
        // Every pilot ends on 3 points (each won one heat, lost one), so the standing is decided
        // entirely by fastest lap: X 2.0s, Z 2.3s, Y 2.5s, W 2.6s.
        let ranking = g.ranking(&completed);
        assert_eq!(names(&ranking), vec!["X", "Z", "Y", "W"]);
        let x = ranking
            .iter()
            .position(|e| e.competitor == cref("X"))
            .unwrap();
        let y = ranking
            .iter()
            .position(|e| e.competitor == cref("Y"))
            .unwrap();
        assert!(
            x < y,
            "X (faster best lap) must rank ahead of Y on the same points"
        );
        // The fastest-lap tie-break splits the equal-points pilots into distinct positions.
        assert!(ranking[x].position < ranking[y].position);
    }

    #[test]
    fn ranking_fastest_lap_tiebreak_sorts_a_no_lap_pilot_behind_one_with_a_lap() {
        // M and N tie on points; M completed a lap, N never did (no best_lap). M ranks ahead.
        let g = RoundRobin::new(field(&["M", "N"]), 1, 2, RrMetric::Points);
        let completed = vec![CompletedHeat::new(
            "rr-r1-h1",
            // Genuine points tie: both share position 1 (e.g. an all-tied heat), M has a lap.
            result_bl(&[("M", 1, 1, Some(3_000_000)), ("N", 1, 0, None)]),
        )];
        let ranking = g.ranking(&completed);
        assert_eq!(
            ranking[0].competitor,
            cref("M"),
            "the pilot with a lap ranks first"
        );
        assert_eq!(
            ranking[1].competitor,
            cref("N"),
            "the no-lap pilot sorts behind"
        );
    }

    #[test]
    fn ranking_by_total_laps_sums_across_heats() {
        let g = RoundRobin::new(field(&["A", "B", "C", "D"]), 2, 2, RrMetric::TotalLaps);
        let completed = vec![
            CompletedHeat::new("rr-r1-h1", result(&[("A", 1, 5), ("B", 2, 3)])),
            CompletedHeat::new("rr-r1-h2", result(&[("C", 1, 4), ("D", 2, 2)])),
            CompletedHeat::new("rr-r2-h1", result(&[("A", 1, 6), ("C", 2, 4)])),
            CompletedHeat::new("rr-r2-h2", result(&[("B", 1, 5), ("D", 2, 1)])),
        ];
        // Total laps: A=5+6=11; B=3+5=8; C=4+4=8; D=2+1=3.
        // A first, B/C tie on 8, D last.
        let ranking = g.ranking(&completed);
        assert_eq!(ranking[0].competitor, cref("A"));
        assert_eq!(ranking[0].position, 1);
        assert_eq!(ranking[1].position, 2);
        assert_eq!(ranking[2].position, 2, "B and C tie on 8 laps");
        assert_eq!(ranking[3].competitor, cref("D"));
        assert_eq!(ranking[3].position, 4);
    }

    #[test]
    fn ranking_before_any_heat_lists_the_whole_field_tied() {
        // Provisional ranking with no history: every field member appears, all tied at 0.
        let g = RoundRobin::new(field(&["B", "A", "C"]), 3, 2, RrMetric::Points);
        let ranking = g.ranking(&[]);
        assert_eq!(names(&ranking), vec!["A", "B", "C"]);
        assert!(ranking.iter().all(|e| e.position == 1));
    }

    #[test]
    fn ranking_is_deterministic_across_runs() {
        let g = RoundRobin::new(field(&["A", "B", "C", "D"]), 2, 2, RrMetric::Points);
        let completed = vec![
            CompletedHeat::new("rr-r1-h1", result(&[("A", 1, 5), ("B", 2, 4)])),
            CompletedHeat::new("rr-r1-h2", result(&[("C", 1, 5), ("D", 2, 4)])),
        ];
        assert_eq!(g.ranking(&completed), g.ranking(&completed));
    }

    // --- full loop ------------------------------------------------------------

    #[test]
    fn full_two_round_loop_terminates_with_a_points_ranking() {
        let mut g = RoundRobin::new(field(&["A", "B", "C", "D"]), 2, 2, RrMetric::Points);
        let mut history: Vec<CompletedHeat> = Vec::new();
        let mut rounds_seen = 0;

        loop {
            let step = g.next(&history);
            let plans = match step {
                GeneratorStep::Run(plans) => plans,
                GeneratorStep::Complete => break,
            };
            rounds_seen += 1;
            assert!(!plans.is_empty());
            for plan in plans {
                // Front pilot of each heat wins it.
                history.push(CompletedHeat::new(
                    plan.heat.0.clone(),
                    result(&[(&plan.lineup[0].0, 1, 4), (&plan.lineup[1].0, 2, 2)]),
                ));
            }
        }

        assert_eq!(rounds_seen, 2, "exactly two rounds were emitted");
        // Idempotent terminal state.
        assert_eq!(g.next(&history), GeneratorStep::Complete);

        let ranking = g.ranking(&history);
        assert_eq!(ranking.len(), 4, "the final ranking covers the whole field");
        assert_eq!(ranking[0].position, 1);
        for window in ranking.windows(2) {
            assert!(window[0].position <= window[1].position);
        }
    }

    // --- registry -------------------------------------------------------------

    #[test]
    fn registry_builds_round_robin_from_config() {
        let mut registry = FormatRegistry::new();
        RoundRobin::register(&mut registry);
        assert_eq!(registry.names(), vec!["round_robin"]);

        let cfg = FormatConfig::new(field(&["A", "B", "C", "D"]))
            .with_param("rounds", "2")
            .with_param("heat_size", "2");
        let mut g = registry
            .build(RoundRobin::NAME, &cfg)
            .expect("round_robin is registered");
        // Round 1 partitions the field into two heats of two.
        let plans = run(g.next(&[]));
        assert_eq!(plans.len(), 2);
        assert_eq!(lineup_names(&plans[0]), vec!["A", "B"]);
        assert_eq!(lineup_names(&plans[1]), vec!["C", "D"]);
    }

    #[test]
    fn config_defaults_to_points_metric_and_default_sizes() {
        // No params → DEFAULT_ROUNDS, DEFAULT_HEAT_SIZE, Points metric.
        let cfg = FormatConfig::new(field(&["A", "B", "C", "D", "E"]));
        let g = RoundRobin::from_config(&cfg);
        // Ranking with no history lists the whole field (proves the field was taken).
        assert_eq!(g.ranking(&[]).len(), 5);
    }

    #[test]
    fn config_seeding_draw_orders_the_partition() {
        use crate::format::SeedingOutcome;
        // A recorded draw reorders the field; round 1 partitions the drawn order.
        let cfg = FormatConfig::new(field(&["A", "B", "C", "D"]))
            .with_seeding(SeedingOutcome::drawn(field(&["C", "A", "D", "B"])))
            .with_param("rounds", "1")
            .with_param("heat_size", "2");
        let mut g = RoundRobin::from_config(&cfg);
        let plans = run(g.next(&[]));
        assert_eq!(lineup_names(&plans[0]), vec!["C", "A"]);
        assert_eq!(lineup_names(&plans[1]), vec!["D", "B"]);
    }

    #[test]
    fn config_total_laps_metric_parses() {
        let cfg = FormatConfig::new(field(&["A", "B"]))
            .with_param("metric", "total-laps")
            .with_param("rounds", "1")
            .with_param("heat_size", "2");
        let g = RoundRobin::from_config(&cfg);
        let completed = vec![CompletedHeat::new(
            "rr-r1-h1",
            result(&[("B", 1, 9), ("A", 2, 3)]),
        )];
        // By laps, B (9) beats A (3) even though both share the only heat.
        let ranking = g.ranking(&completed);
        assert_eq!(ranking[0].competitor, cref("B"));
    }
}
