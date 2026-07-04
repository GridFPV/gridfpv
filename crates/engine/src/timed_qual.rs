//! `timed_qual` — a timed-qualifying [`Generator`] (race-engine.html §3, §4, §7.1).
//!
//! # What qualifying is here
//!
//! Qualifying "runs rounds and aggregates flights into a ranking" (RE §3): the seeded
//! field flies a fixed number of **rounds** (rotations), and a pilot's standing is
//! their **best flight** across every heat they flew — *not* an average, and *not* a
//! sum. This is the standard FPV qualifying rule: your single best run is the one that
//! counts, so one great flight carries you regardless of a scrappy round.
//!
//! Each rotation **chunks the seeded field into heats of `heat_size`** (#326): a 64-pilot
//! time trial with `heat_size = 4` flies 16 heats per rotation, not one impossible 64-up
//! heat. A field at or under `heat_size` is a single whole-field heat (the small-field
//! case). The ranking aggregates each pilot's best across *all* their heats, so chunking
//! changes only how heats map to rotations, never the best-flight rule.
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
//! `next` and `ranking` read no clock and no RNG: `next` is a pure function of which of
//! the rotation's chunk heats are already in the completed history vs the configured
//! `rounds`, and `ranking` a pure function of the completed history plus the seeded
//! field. The seed order (after any recorded [`SeedingOutcome`]) is the chunk order and
//! the final tie-break, and heat ids (`tq-r{rotation}-h{heat}`) are derived purely from
//! the rotation + chunk index, so the same history always yields the same heats and the
//! same ranking — a recorded session replays identically.
//!
//! # Heat formation: generator vs. static channel-balancing
//!
//! This generator's `next` is the **per-heat** formation path: it chunks the field into
//! consecutive heats of `heat_size`. Production time-trial rounds default to
//! [`ChannelMode::Static`](crate::format) channel formation, which the *server*
//! (`round_engine::fill_round_static`) lays out instead — channel-balanced heats off each
//! pilot's fixed channel, capped at the timer's node count. Either way the heats feed the
//! **same** `ranking` here (it aggregates by competitor, independent of heat ids), so the
//! best-flight standing is identical regardless of which formation built the heats.
//!
//! [`rank_by`]: crate::format::rank_by
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

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
    ///
    /// `best_lap_micros` is the placement's **condition-independent** best lap: the
    /// [`BestLap`](QualMetric::BestLap) qualifier ranks on it regardless of which win
    /// condition scored the heat. Before this, a round scored under a non-BestLap
    /// condition (e.g. FirstToLaps carrying `Metric::ReachedAt`) yielded `None` for
    /// every pilot — the whole field tied at 1 and "ranked" alphabetically, and any
    /// `FromRanking` bracket seeded from that order.
    fn round_key(self, placement_metric: Metric, laps: u32, best_lap_micros: Option<i64>) -> Option<i64> {
        match (self, placement_metric) {
            // Best-lap qualifying: the matched metric when the round was scored under
            // BestLap; otherwise the condition-independent `Placement.best_lap_micros`
            // (which exists precisely so cross-heat rankings survive the mismatch).
            (QualMetric::BestLap, Metric::BestLapMicros(micros)) => micros.or(best_lap_micros),
            (QualMetric::BestLap, _) => best_lap_micros,
            (QualMetric::BestConsecutive, Metric::BestConsecutiveMicros(sum)) => sum,
            // Most-laps: ignore the placement metric, rank on the counted laps.
            (QualMetric::MostLaps, _) => Some(-(laps as i64)),
            // Metric/condition mismatch with no condition-independent equivalent (a
            // best-consecutive qualifier over a round scored under another condition):
            // "no qualifying value" — the pilot ranks in the no-value band rather than
            // corrupting the aggregate.
            _ => None,
        }
    }
}

/// A timed-qualifying generator: the seeded field flies `rounds` rounds, ranked by each
/// pilot's **best flight** under the configured [`QualMetric`] (RE §3, §7.1).
///
/// Construct with [`TimedQualifying::new`] / [`with_heat_size`](Self::with_heat_size) or, via the
/// registry, [`from_config`] under the name [`NAME`](Self::NAME). `next` emits a rotation's heats
/// (the field chunked into heats of `heat_size`) until `rounds` rotations are completed, then
/// [`GeneratorStep::Complete`]; `ranking` aggregates best-flight across every heat.
///
/// [`from_config`]: TimedQualifying::from_config
pub struct TimedQualifying {
    /// The field flying each rotation, in seed/draw order (recorded outcome already
    /// applied). Chunked into heats of `heat_size`; also the final ranking tie-break.
    field: Vec<CompetitorRef>,
    /// How many rounds (rotations) the field flies before the format completes. **`0` is
    /// open-ended**: the generator keeps emitting the next rotation on demand forever (the RD ends
    /// it by not requesting more) — "Heats per pilot = 0".
    rounds: usize,
    /// Pilots per heat: each rotation chunks the field into consecutive heats of this size (the
    /// last possibly short). Clamped to ≥ 1. A field at or under `heat_size` is one whole-field heat.
    heat_size: usize,
    /// Which round metric the cross-round aggregation ranks by.
    metric: QualMetric,
}

impl TimedQualifying {
    /// The format name this registers under.
    pub const NAME: &'static str = "timed_qual";

    /// The default number of qualifying rounds when `rounds` is not configured.
    pub const DEFAULT_ROUNDS: usize = 3;

    /// The default pilots-per-heat when `heat_size` is not configured (a standard 4-up heat).
    pub const DEFAULT_HEAT_SIZE: usize = 4;

    /// The most qualifying rotations a round accepts (`rounds = 0` stays the open-ended
    /// sentinel). The static fill path materializes the round's full heat plan per fill, so an
    /// unchecked param (`rounds=999999999` over the raw API) would allocate unbounded memory.
    /// 100 heats per pilot is far beyond any real qualifying day; a request past it clamps.
    pub const MAX_ROUNDS: usize = 100;

    /// Build over `field` for `rounds` rotations, ranked by `metric`, chunking each rotation into
    /// [`DEFAULT_HEAT_SIZE`](Self::DEFAULT_HEAT_SIZE) heats. The field is taken in the given order
    /// (apply any [`crate::format::SeedingOutcome`] before calling, as
    /// [`from_config`](Self::from_config) does). For a custom heat size use
    /// [`with_heat_size`](Self::with_heat_size).
    pub fn new(field: Vec<CompetitorRef>, rounds: usize, metric: QualMetric) -> Self {
        Self::with_heat_size(field, rounds, Self::DEFAULT_HEAT_SIZE, metric)
    }

    /// Build over `field` for `rounds` rotations, chunking each rotation into heats of `heat_size`
    /// (clamped to ≥ 1), ranked by `metric`.
    pub fn with_heat_size(
        field: Vec<CompetitorRef>,
        rounds: usize,
        heat_size: usize,
        metric: QualMetric,
    ) -> Self {
        Self {
            field,
            // 0 = open-ended (generate the next heat on demand); a positive count clamps to
            // MAX_ROUNDS so the materialized plan is bounded.
            rounds: if rounds == 0 {
                0
            } else {
                rounds.min(Self::MAX_ROUNDS)
            },
            heat_size: heat_size.max(1),
            metric,
        }
    }

    /// The registry constructor: applies the recorded `seeding` draw to `field`, reads
    /// `rounds` (default [`DEFAULT_ROUNDS`](Self::DEFAULT_ROUNDS)), `heat_size` (default
    /// [`DEFAULT_HEAT_SIZE`](Self::DEFAULT_HEAT_SIZE)) and the `metric` param (default
    /// [`QualMetric::BestLap`]).
    ///
    /// `heat_size` is **not** declared in the offered format schema: production time-trial rounds
    /// form heats in [`ChannelMode::Static`](crate::format) (the server sizes heats by the timer's
    /// node count, not this param), so surfacing a heat-size knob there would mislead. The param is
    /// still read here so a `PerHeat` round can carry one, and so the generator chunks sensibly.
    pub fn from_config(config: &FormatConfig) -> Box<dyn Generator> {
        let field = config.seeding.apply(&config.field);
        let rounds = config.param_usize("rounds", Self::DEFAULT_ROUNDS);
        let heat_size = config.param_usize("heat_size", Self::DEFAULT_HEAT_SIZE);
        let metric = QualMetric::parse(config.params.get("metric").map(String::as_str));
        Box::new(Self::with_heat_size(field, rounds, heat_size, metric))
    }

    /// Register this format under [`NAME`](Self::NAME).
    pub fn register(registry: &mut FormatRegistry) {
        registry.register(Self::NAME, Self::from_config);
    }

    /// Heat id for chunk `heat` (1-based) of rotation `rotation` (1-based) —
    /// `tq-r1-h1`, `tq-r1-h2`, `tq-r2-h1`, … Unique + stable across rotations and replays.
    fn heat_id(rotation: usize, heat: usize) -> String {
        format!("tq-r{rotation}-h{heat}")
    }

    /// How many heats one rotation chunks the field into (`ceil(field / heat_size)`, ≥ 1).
    fn chunks_per_rotation(&self) -> usize {
        self.field.len().div_ceil(self.heat_size).max(1)
    }

    /// One rotation's heats: the field split into consecutive chunks of `heat_size`, each a heat
    /// id'd `tq-r{rotation}-h{n}` (the last chunk possibly short).
    fn rotation_heats(&self, rotation: usize) -> Vec<HeatPlan> {
        self.field
            .chunks(self.heat_size)
            .enumerate()
            .map(|(index, chunk)| HeatPlan::new(Self::heat_id(rotation, index + 1), chunk.to_vec()))
            .collect()
    }
}

impl Generator for TimedQualifying {
    fn next(&mut self, completed: &[CompletedHeat]) -> GeneratorStep {
        // Emit the lowest rotation (1-based) whose chunk heats are not yet all completed, as the
        // field chunked into heats of `heat_size`. The driver schedules one heat per fill (taking
        // the first not-yet-scheduled plan) and only finalizes the whole rotation before its heats
        // appear in `completed`, so a rotation stays current until every one of its chunks is in.
        // Pure function of (which chunk ids are in `completed`, rounds, heat_size) — no clock, no
        // RNG; ids derive purely from rotation + chunk index. `rounds == 0` is **open-ended**:
        // every rotation is emitted on demand, never Complete (the RD ends it by not asking more).
        if self.field.is_empty() {
            return GeneratorStep::Complete;
        }
        let chunks = self.chunks_per_rotation();
        let done: BTreeSet<&str> = completed.iter().map(|c| c.heat.0.as_str()).collect();
        let mut rotation = 1usize;
        while (1..=chunks).all(|heat| done.contains(Self::heat_id(rotation, heat).as_str())) {
            rotation += 1;
        }
        if self.rounds != 0 && rotation > self.rounds {
            GeneratorStep::Complete
        } else {
            GeneratorStep::Run(self.rotation_heats(rotation))
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
                // A **disqualified** competitor has no qualifying value for that heat: a DQ voids
                // the lap result that decided their placement metric, so it must not let them
                // out-rank clean pilots on a now-defunct lap time (#226 scored DQs into the heat
                // result; this stops the metric aggregation from ranking on a DQ'd pilot's value).
                // Their *other* (clean) heats still count; DQ'd in every heat → no value → ranked
                // last (band 1), the same place a no-show lands.
                let key = if place.disqualified {
                    None
                } else {
                    self.metric
                        .round_key(place.metric, place.laps, place.best_lap_micros)
                };
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
        // A small field (≤ heat_size) is one whole-field heat, id'd tq-r1-h1.
        let mut g = TimedQualifying::new(field(&["A", "B", "C"]), 3, QualMetric::BestLap);
        assert_eq!(
            g.next(&[]),
            GeneratorStep::Run(vec![HeatPlan::new("tq-r1-h1", field(&["A", "B", "C"]))])
        );
    }

    #[test]
    fn emits_each_subsequent_rotation_numbered_from_history() {
        let mut g = TimedQualifying::new(field(&["A", "B"]), 3, QualMetric::BestLap);
        // Rotation 1's heat completed → emit rotation 2.
        let r1 = CompletedHeat::new("tq-r1-h1", best_lap_round(&[("A", Some(2_000_000))]));
        assert_eq!(
            g.next(std::slice::from_ref(&r1)),
            GeneratorStep::Run(vec![HeatPlan::new("tq-r2-h1", field(&["A", "B"]))])
        );
        // Two rotations completed → emit rotation 3 (the last).
        let r2 = CompletedHeat::new("tq-r2-h1", best_lap_round(&[("A", Some(1_900_000))]));
        assert_eq!(
            g.next(&[r1, r2]),
            GeneratorStep::Run(vec![HeatPlan::new("tq-r3-h1", field(&["A", "B"]))])
        );
    }

    #[test]
    fn completes_after_the_configured_rounds() {
        let mut g = TimedQualifying::new(field(&["A", "B"]), 2, QualMetric::BestLap);
        let completed = vec![
            CompletedHeat::new("tq-r1-h1", best_lap_round(&[("A", Some(2_000_000))])),
            CompletedHeat::new("tq-r2-h1", best_lap_round(&[("A", Some(1_900_000))])),
        ];
        assert_eq!(g.next(&completed), GeneratorStep::Complete);
    }

    #[test]
    fn chunks_a_large_field_into_heats_of_heat_size() {
        // 8 pilots, heat_size 4, one rotation → two heats of 4 (consecutive seed-order chunks).
        let mut g = TimedQualifying::with_heat_size(
            field(&["A", "B", "C", "D", "E", "F", "G", "H"]),
            1,
            4,
            QualMetric::BestLap,
        );
        assert_eq!(
            g.next(&[]),
            GeneratorStep::Run(vec![
                HeatPlan::new("tq-r1-h1", field(&["A", "B", "C", "D"])),
                HeatPlan::new("tq-r1-h2", field(&["E", "F", "G", "H"])),
            ])
        );
    }

    #[test]
    fn chunked_rotation_advances_only_once_all_its_heats_are_in() {
        // 8 pilots / heat_size 4 → 2 heats per rotation. The rotation stays current until BOTH its
        // heats are completed; only then does rotation 2 appear. Heat ids are unique across both.
        let mut g = TimedQualifying::with_heat_size(
            field(&["A", "B", "C", "D", "E", "F", "G", "H"]),
            2,
            4,
            QualMetric::BestLap,
        );
        // Only the first chunk of rotation 1 is in → still rotation 1 (so the driver can pick h2).
        let r1h1 = CompletedHeat::new("tq-r1-h1", best_lap_round(&[("A", Some(2_000_000))]));
        assert_eq!(
            g.next(std::slice::from_ref(&r1h1)),
            GeneratorStep::Run(vec![
                HeatPlan::new("tq-r1-h1", field(&["A", "B", "C", "D"])),
                HeatPlan::new("tq-r1-h2", field(&["E", "F", "G", "H"])),
            ])
        );
        // Both chunks of rotation 1 in → rotation 2 (a fresh pair of unique heat ids).
        let r1h2 = CompletedHeat::new("tq-r1-h2", best_lap_round(&[("E", Some(2_000_000))]));
        assert_eq!(
            g.next(&[r1h1, r1h2]),
            GeneratorStep::Run(vec![
                HeatPlan::new("tq-r2-h1", field(&["A", "B", "C", "D"])),
                HeatPlan::new("tq-r2-h2", field(&["E", "F", "G", "H"])),
            ])
        );
    }

    #[test]
    fn chunked_ranking_aggregates_each_pilots_best_across_their_heats() {
        // 8 pilots, heat_size 4, two rotations. Each pilot flies their chunk twice; the ranking
        // takes their best single lap across both heats — chunking must not break the aggregate.
        let g = TimedQualifying::with_heat_size(
            field(&["A", "B", "C", "D", "E", "F", "G", "H"]),
            2,
            4,
            QualMetric::BestLap,
        );
        let completed = vec![
            // Rotation 1.
            CompletedHeat::new(
                "tq-r1-h1",
                best_lap_round(&[
                    ("A", Some(2_000_000)),
                    ("B", Some(2_100_000)),
                    ("C", Some(2_200_000)),
                    ("D", Some(2_300_000)),
                ]),
            ),
            CompletedHeat::new(
                "tq-r1-h2",
                best_lap_round(&[
                    ("E", Some(2_400_000)),
                    ("F", Some(2_500_000)),
                    ("G", Some(2_600_000)),
                    ("H", Some(2_700_000)),
                ]),
            ),
            // Rotation 2: A improves to the fastest lap overall; H improves but stays slowest.
            CompletedHeat::new(
                "tq-r2-h1",
                best_lap_round(&[
                    ("A", Some(1_500_000)),
                    ("B", Some(2_050_000)),
                    ("C", Some(2_150_000)),
                    ("D", Some(2_250_000)),
                ]),
            ),
            CompletedHeat::new(
                "tq-r2-h2",
                best_lap_round(&[
                    ("E", Some(2_350_000)),
                    ("F", Some(2_450_000)),
                    ("G", Some(2_550_000)),
                    ("H", Some(2_650_000)),
                ]),
            ),
        ];
        // Best single lap per pilot across both heats → A 1.5, B 2.05, C 2.15, D 2.25, E 2.35,
        // F 2.45, G 2.55, H 2.65: a strict A..H order.
        let ranking = g.ranking(&completed);
        assert_eq!(
            names(&ranking),
            vec!["A", "B", "C", "D", "E", "F", "G", "H"]
        );
        assert_eq!(ranking[0].position, 1);
        assert_eq!(ranking[7].position, 8);
    }

    #[test]
    fn zero_rounds_is_open_ended_and_never_completes() {
        // "Heats per pilot = 0": the generator keeps emitting the next rotation forever.
        let mut g = TimedQualifying::new(field(&["A", "B"]), 0, QualMetric::BestLap);
        let mut completed: Vec<CompletedHeat> = Vec::new();
        for r in 1..=20 {
            assert_eq!(
                g.next(&completed),
                GeneratorStep::Run(vec![HeatPlan::new(
                    format!("tq-r{r}-h1"),
                    field(&["A", "B"])
                )]),
                "open-ended (rounds=0) must keep emitting rotation {r}, never Complete"
            );
            completed.push(CompletedHeat::new(
                format!("tq-r{r}-h1"),
                best_lap_round(&[("A", Some(2_000_000))]),
            ));
        }
    }

    #[test]
    fn rounds_clamp_to_max_rounds_but_zero_stays_open_ended() {
        // An unchecked `rounds=999999` (raw API) clamps to MAX_ROUNDS: the round plans at most
        // MAX_ROUNDS rotations, then completes — no unbounded materialized plan.
        let mut g =
            TimedQualifying::with_heat_size(field(&["A", "B"]), 999_999, 4, QualMetric::BestLap);
        assert_eq!(g.rounds, TimedQualifying::MAX_ROUNDS);
        let completed: Vec<CompletedHeat> = (1..=TimedQualifying::MAX_ROUNDS)
            .map(|r| {
                CompletedHeat::new(
                    format!("tq-r{r}-h1"),
                    best_lap_round(&[("A", Some(2_000_000))]),
                )
            })
            .collect();
        assert_eq!(
            g.next(&completed),
            GeneratorStep::Complete,
            "the clamped round completes after MAX_ROUNDS rotations"
        );

        // `rounds = 0` stays the open-ended sentinel — NOT clamped into a finite plan: it
        // keeps emitting the next rotation even past the cap.
        let mut open =
            TimedQualifying::with_heat_size(field(&["A", "B"]), 0, 4, QualMetric::BestLap);
        assert_eq!(open.rounds, 0);
        assert_eq!(
            open.next(&completed),
            GeneratorStep::Run(vec![HeatPlan::new(
                format!("tq-r{}-h1", TimedQualifying::MAX_ROUNDS + 1),
                field(&["A", "B"])
            )]),
            "rounds=0 keeps emitting past the clamp"
        );
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
    fn ranking_best_lap_survives_a_win_condition_mismatch_via_placement_best_lap() {
        // A best-lap qualifier over a round scored under a DIFFERENT win condition (e.g.
        // FirstToLaps → Metric::ReachedAt): the metric field carries no best lap, but the
        // placement's condition-independent `best_lap_micros` does. Before the fallback the
        // whole field yielded `None` → everyone tied at 1, ordered alphabetically — and any
        // FromRanking bracket seeded from that order.
        let g = TimedQualifying::new(field(&["A", "B", "C"]), 1, QualMetric::BestLap);
        let places = [("B", 1_400_000), ("C", 1_600_000), ("A", 1_900_000)]
            .iter()
            .enumerate()
            .map(|(i, (name, best))| Placement {
                best_lap_micros: Some(*best),
                ..placement(
                    name,
                    (i as u32) + 1,
                    3,
                    Metric::ReachedAt(Some(gridfpv_events::SourceTime { micros: 9_000_000 }))
                )
            })
            .collect();
        let ranking = g.ranking(&[CompletedHeat::new(
            "round-1",
            HeatResult {
                places,
                ..Default::default()
            },
        )]);
        // Ranked by best lap — B, C, A — with distinct positions, not an alphabetical tie.
        assert_eq!(names(&ranking), vec!["B", "C", "A"]);
        assert_eq!(
            ranking.iter().map(|r| r.position).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
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
            assert_eq!(plans[0].heat.0, format!("tq-r{}-h1", i + 1));
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
        // Rotation 1 over the whole (small) field.
        assert_eq!(
            g.next(&[]),
            GeneratorStep::Run(vec![HeatPlan::new("tq-r1-h1", field(&["A", "B", "C"]))])
        );
        // And it completes after exactly the configured 2 rotations.
        let completed = vec![
            CompletedHeat::new("tq-r1-h1", best_lap_round(&[("A", Some(2_000_000))])),
            CompletedHeat::new("tq-r2-h1", best_lap_round(&[("A", Some(1_900_000))])),
        ];
        assert_eq!(g.next(&completed), GeneratorStep::Complete);
    }

    #[test]
    fn config_defaults_to_three_best_lap_rounds() {
        // No params → DEFAULT_ROUNDS (3) and the BestLap metric.
        let cfg = FormatConfig::new(field(&["A", "B"]));
        let mut g = TimedQualifying::from_config(&cfg);
        let two_done = vec![
            CompletedHeat::new("tq-r1-h1", best_lap_round(&[("A", Some(2_000_000))])),
            CompletedHeat::new("tq-r2-h1", best_lap_round(&[("A", Some(1_900_000))])),
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
            GeneratorStep::Run(vec![HeatPlan::new(
                "tq-r1-h1",
                field(&["C", "A", "D", "B"])
            )])
        );
    }

    #[test]
    fn config_reads_the_heat_size_param_and_chunks() {
        // heat_size=2 over a field of 5 → 3 heats per rotation (2, 2, 1) — the param is honoured.
        let cfg = FormatConfig::new(field(&["A", "B", "C", "D", "E"]))
            .with_param("rounds", "1")
            .with_param("heat_size", "2");
        let mut g = TimedQualifying::from_config(&cfg);
        assert_eq!(
            g.next(&[]),
            GeneratorStep::Run(vec![
                HeatPlan::new("tq-r1-h1", field(&["A", "B"])),
                HeatPlan::new("tq-r1-h2", field(&["C", "D"])),
                HeatPlan::new("tq-r1-h3", field(&["E"])),
            ])
        );
    }

    // --- ranking(): a disqualified pilot is excluded from the metric aggregate ------------------

    #[test]
    fn ranking_disqualified_pilot_does_not_outrank_clean_pilots() {
        // B sets the fastest lap on track but is DISQUALIFIED in that heat: the DQ voids their
        // qualifying value, so they must NOT out-rank A on the lap time. With no other heat, B has
        // no value at all → ranked last, behind clean A.
        let g = TimedQualifying::new(field(&["A", "B"]), 1, QualMetric::BestLap);
        let mut round = best_lap_round(&[("A", Some(2_000_000)), ("B", Some(1_500_000))]);
        // Flag B's placement disqualified (as the adjudicated scorer does for a DQ penalty).
        for place in &mut round.places {
            if place.competitor.competitor == cref("B") {
                place.disqualified = true;
            }
        }
        let ranking = g.ranking(&[CompletedHeat::new("tq-r1-h1", round)]);
        // A (clean, 2.0s) ahead of B (DQ'd, no qualifying value) despite B's faster on-track lap.
        assert_eq!(names(&ranking), vec!["A", "B"]);
        assert_eq!(ranking[0].competitor, cref("A"));
        assert_eq!(ranking[0].position, 1);
        assert_eq!(ranking[1].position, 2);
    }

    #[test]
    fn ranking_dq_in_one_heat_still_counts_a_clean_heat() {
        // A pilot DQ'd in one rotation but clean in another keeps the clean heat's value.
        // B is DQ'd in rotation 1 (where they were fastest) but flies clean in rotation 2 at 1.9s,
        // which still beats A's best (2.0s) — the clean heat is not discarded with the DQ'd one.
        let g = TimedQualifying::new(field(&["A", "B"]), 2, QualMetric::BestLap);
        let mut r1 = best_lap_round(&[("A", Some(2_000_000)), ("B", Some(1_400_000))]);
        for place in &mut r1.places {
            if place.competitor.competitor == cref("B") {
                place.disqualified = true;
            }
        }
        let r2 = best_lap_round(&[("A", Some(2_100_000)), ("B", Some(1_900_000))]);
        let ranking = g.ranking(&[
            CompletedHeat::new("tq-r1-h1", r1),
            CompletedHeat::new("tq-r2-h1", r2),
        ]);
        // B's clean rotation-2 lap (1.9s) counts and beats A's best (2.0s); the DQ'd 1.4s is voided.
        assert_eq!(names(&ranking), vec!["B", "A"]);
        assert_eq!(ranking[0].competitor, cref("B"));
    }
}
