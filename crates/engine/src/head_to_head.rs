//! Head-to-Head — the **atomic racing format** (format-model.html, decision D17): split a field into
//! heats of `group_size`, race them once, and rank the field. It is the building block every
//! tournament structure composes — a round-robin drives it all-play-all, a bracket chains it with
//! placement + winners-advance. Head-to-Head itself does **one pass** and **one job**: race + rank.
//!
//! # Scoring
//!
//! - [`Scoring::Placement`] — rank by **finishing position** across the heats (all heat-winners first,
//!   then all runners-up, …), breaking a band by laps. This is what a bracket reads (winners advance).
//! - [`Scoring::Points`] — each finish earns points from a **per-position table** (1st most,
//!   descending; positions past the table earn 0); rank by total points. With no explicit table the
//!   points fall back to the linear `heat_size − position + 1`. This is what a round-robin sums.
//!
//! For a single pass (one heat per pilot) Points and Placement rank alike; the table earns its keep
//! when a structure runs Head-to-Head over many passes and **sums** the points (round-robin).
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use gridfpv_events::CompetitorRef;

use crate::format::{
    CompletedHeat, FormatConfig, FormatRegistry, Generator, GeneratorStep, HeatPlan, RankEntry,
    parse_points_table, position_points, rank_by,
};

/// How a Head-to-Head round turns its heats' finishes into a ranking. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scoring {
    /// Rank by finishing position (band by place, then laps). What a bracket reads.
    Placement,
    /// Finishing-position points from a per-position table (`None` ⇒ linear `k − pos + 1`), summed.
    Points(Option<Vec<u32>>),
}

/// A Head-to-Head racing round over a seeded field. `next` emits one pass of heats (the field split
/// into groups of [`group_size`](Self::group_size)) then completes; `ranking` ranks the field under
/// the configured [`Scoring`].
pub struct HeadToHead {
    /// The field in seed/draw order (the recorded seeding already applied) — the split order and the
    /// final ranking's deterministic tie-break.
    field: Vec<CompetitorRef>,
    /// Pilots per heat (clamped to ≥ 2 — a head-to-head heat needs at least two to be a race).
    group_size: usize,
    /// How the round ranks its heats' finishes.
    scoring: Scoring,
}

impl HeadToHead {
    /// The format name this registers under.
    pub const NAME: &'static str = "head_to_head";

    /// The default group size when unconfigured (head-to-head proper).
    pub const DEFAULT_GROUP_SIZE: usize = 2;

    /// Build over a `field` in seed order with the given group size (clamped to ≥ 2) and scoring.
    pub fn new(field: Vec<CompetitorRef>, group_size: usize, scoring: Scoring) -> Self {
        Self {
            field,
            group_size: group_size.max(2),
            scoring,
        }
    }

    /// The registry constructor: applies the recorded seeding draw, reads `group_size` (default 2)
    /// and the `scoring` param (`points` ⇒ [`Scoring::Points`] reading the `points` per-position
    /// table; anything else / absent ⇒ [`Scoring::Placement`]).
    pub fn from_config(config: &FormatConfig) -> Box<dyn Generator> {
        let field = config.seeding.apply(&config.field);
        let group_size = config.param_usize("group_size", Self::DEFAULT_GROUP_SIZE);
        let scoring = match config.params.get("scoring").map(String::as_str) {
            Some("points") => Scoring::Points(parse_points_table(
                config.params.get("points").map(String::as_str),
            )),
            _ => Scoring::Placement,
        };
        Box::new(Self::new(field, group_size, scoring))
    }

    /// Register this format under [`NAME`](Self::NAME).
    pub fn register(registry: &mut FormatRegistry) {
        registry.register(Self::NAME, Self::from_config);
    }

    /// The heat id for heat `index` (0-based) of this round's single pass.
    fn heat_id(index: usize) -> String {
        format!("h2h-h{index}")
    }

    /// The field split into consecutive heats of `group_size` (the last possibly short).
    fn lay_out(&self) -> Vec<HeatPlan> {
        self.field
            .chunks(self.group_size)
            .enumerate()
            .map(|(index, chunk)| HeatPlan::new(Self::heat_id(index), chunk.to_vec()))
            .collect()
    }
}

impl Generator for HeadToHead {
    fn next(&mut self, completed: &[CompletedHeat]) -> GeneratorStep {
        // A lone (or empty) field has nothing to race. Otherwise emit the single pass of heats and
        // complete once they are all in.
        if self.field.len() <= 1 {
            return GeneratorStep::Complete;
        }
        let heats = self.lay_out();
        let done: BTreeSet<&str> = completed.iter().map(|c| c.heat.0.as_str()).collect();
        if heats.iter().all(|h| done.contains(h.heat.0.as_str())) {
            GeneratorStep::Complete
        } else {
            GeneratorStep::Run(heats)
        }
    }

    fn ranking(&self, completed: &[CompletedHeat]) -> Vec<RankEntry> {
        // Before any heat, the provisional ranking is the seed order.
        if completed.is_empty() {
            return seed_ranking(&self.field);
        }

        match &self.scoring {
            // Points: sum each pilot's per-position points across the heats they flew; more = better,
            // so negate into a smaller-is-better key. Seed every field member at 0 so a no-show still
            // ranks (last).
            Scoring::Points(table) => {
                let mut totals: BTreeMap<CompetitorRef, i64> =
                    self.field.iter().map(|c| (c.clone(), 0)).collect();
                for heat in completed {
                    let heat_size = heat.result.places.len();
                    for place in &heat.result.places {
                        *totals
                            .entry(place.competitor.competitor.clone())
                            .or_insert(0) +=
                            position_points(table.as_deref(), place.position, heat_size);
                    }
                }
                let rows: Vec<(CompetitorRef, i64)> =
                    self.field.iter().map(|c| (c.clone(), -totals[c])).collect();
                rank_by(rows)
            }
            // Placement: band by finishing position (lower = better), breaking a band by laps (more =
            // better → negated). A pilot who hasn't raced sorts last. One heat per pilot in a single
            // pass; if a pilot somehow has several, the best (position, then laps) wins.
            Scoring::Placement => {
                let mut best: BTreeMap<CompetitorRef, (u32, i64)> = BTreeMap::new();
                for heat in completed {
                    for place in &heat.result.places {
                        let key = (place.position, -(place.laps as i64));
                        best.entry(place.competitor.competitor.clone())
                            .and_modify(|k| {
                                if key < *k {
                                    *k = key;
                                }
                            })
                            .or_insert(key);
                    }
                }
                let rows: Vec<(CompetitorRef, (u32, i64))> = self
                    .field
                    .iter()
                    .map(|c| (c.clone(), best.get(c).copied().unwrap_or((u32::MAX, 0))))
                    .collect();
                rank_by(rows)
            }
        }
    }
}

/// A trivial 1, 2, 3, … ranking from the seed order — the provisional ranking before any heat is run.
fn seed_ranking(order: &[CompetitorRef]) -> Vec<RankEntry> {
    order
        .iter()
        .enumerate()
        .map(|(index, competitor)| RankEntry {
            competitor: competitor.clone(),
            position: (index as u32) + 1,
        })
        .collect()
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
    fn heat_ids(step: &GeneratorStep) -> Vec<String> {
        match step {
            GeneratorStep::Run(heats) => heats.iter().map(|h| h.heat.0.clone()).collect(),
            GeneratorStep::Complete => panic!("expected Run, got Complete"),
        }
    }

    #[test]
    fn splits_the_field_into_heats_of_group_size() {
        let mut g = HeadToHead::new(field(&["A", "B", "C", "D"]), 2, Scoring::Placement);
        assert_eq!(heat_ids(&g.next(&[])), vec!["h2h-h0", "h2h-h1"]);
        // 4-up groups: one heat.
        let mut g4 = HeadToHead::new(field(&["A", "B", "C", "D"]), 4, Scoring::Placement);
        assert_eq!(heat_ids(&g4.next(&[])), vec!["h2h-h0"]);
    }

    #[test]
    fn completes_once_its_single_pass_is_in() {
        let mut g = HeadToHead::new(field(&["A", "B", "C", "D"]), 2, Scoring::Placement);
        let done = vec![
            CompletedHeat::new("h2h-h0", result(&[("A", 1, 5), ("B", 2, 4)])),
            CompletedHeat::new("h2h-h1", result(&[("C", 1, 5), ("D", 2, 4)])),
        ];
        assert_eq!(g.next(&done), GeneratorStep::Complete);
    }

    #[test]
    fn placement_bands_winners_first_then_by_laps() {
        let g = HeadToHead::new(field(&["A", "B", "C", "D"]), 2, Scoring::Placement);
        // Heat 0: A wins (5 laps), B 2nd (4). Heat 1: C wins (6 laps), D 2nd (3).
        let done = vec![
            CompletedHeat::new("h2h-h0", result(&[("A", 1, 5), ("B", 2, 4)])),
            CompletedHeat::new("h2h-h1", result(&[("C", 1, 6), ("D", 2, 3)])),
        ];
        // Winners band first, ordered by laps (C 6 > A 5); then runners-up (B 4 > D 3).
        assert_eq!(names(&g.ranking(&done)), vec!["C", "A", "B", "D"]);
        assert_eq!(g.ranking(&done)[0].position, 1);
    }

    #[test]
    fn points_use_a_custom_table_and_sum() {
        // A custom table 10/6/3/1; one pass of two head-to-head heats.
        let g = HeadToHead::new(
            field(&["A", "B", "C", "D"]),
            2,
            Scoring::Points(Some(vec![10, 6, 3, 1])),
        );
        let done = vec![
            CompletedHeat::new("h2h-h0", result(&[("A", 1, 5), ("B", 2, 4)])),
            CompletedHeat::new("h2h-h1", result(&[("C", 1, 6), ("D", 2, 3)])),
        ];
        // Winners A, C earn 10 (tie → ref order A, C); runners-up B, D earn 6 (B, D).
        assert_eq!(names(&g.ranking(&done)), vec!["A", "C", "B", "D"]);
        assert_eq!(g.ranking(&done)[0].position, 1);
        assert_eq!(g.ranking(&done)[2].position, 3); // the two winners share position 1
    }

    #[test]
    fn points_fall_back_to_linear_without_a_table() {
        let g = HeadToHead::new(field(&["A", "B", "C"]), 3, Scoring::Points(None));
        // One 3-up heat: A 1st (3 pts), B 2nd (2), C 3rd (1).
        let done = vec![CompletedHeat::new(
            "h2h-h0",
            result(&[("A", 1, 6), ("B", 2, 5), ("C", 3, 4)]),
        )];
        assert_eq!(names(&g.ranking(&done)), vec!["A", "B", "C"]);
    }

    #[test]
    fn provisional_ranking_is_the_seed_order() {
        let g = HeadToHead::new(field(&["A", "B", "C"]), 2, Scoring::Placement);
        assert_eq!(names(&g.ranking(&[])), vec!["A", "B", "C"]);
    }

    #[test]
    fn group_size_clamps_to_two() {
        let g = HeadToHead::new(field(&["A", "B", "C", "D"]), 1, Scoring::Placement);
        assert_eq!(g.group_size, 2);
    }

    #[test]
    fn registry_builds_head_to_head_with_points_table() {
        let mut registry = FormatRegistry::new();
        HeadToHead::register(&mut registry);
        assert_eq!(registry.names(), vec!["head_to_head"]);

        let cfg = FormatConfig::new(field(&["A", "B", "C", "D"]))
            .with_param("group_size", "2")
            .with_param("scoring", "points")
            .with_param("points", "10, 6, 3, 1");
        let mut g = registry.build(HeadToHead::NAME, &cfg).unwrap();
        assert_eq!(heat_ids(&g.next(&[])).len(), 2);
    }
}
