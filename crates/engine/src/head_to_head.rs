//! Head-to-Head — the **atomic racing format** (format-model.html, decision D17): split a field into
//! heats of `group_size`, race them, and rank the field. It is the building block every
//! tournament structure composes — a round-robin drives it all-play-all, a bracket chains it with
//! placement + winners-advance.
//!
//! # One grouping decision per round
//!
//! A Head-to-Head round makes its grouping decision **once**, at fill: the field splits into
//! consecutive groups of `group_size`. With `rotations` = 1 (the default) that single pass is the
//! whole round — everyone races exactly once. With `rotations` = N the **same groups** run back to
//! back N times (MultiGP ProSpec-style points racing: same pilots, same channels, run it again) and
//! the scoring accumulates. What a rotations knob deliberately does NOT do is re-group between
//! heats — the moment opponents change, something has to *decide* the new groups, and that is
//! seeding work, i.e. a tournament **structure** (round-robin all-play-all, a bracket's
//! winners-advance, …), not the atomic round.
//!
//! # Scoring
//!
//! - [`Scoring::Placement`] — rank by **finishing position** (all heat-winners first, then all
//!   runners-up, …), breaking a band by laps. Over several rotations a pilot's **best single
//!   result** counts. This is what a bracket reads (winners advance).
//! - [`Scoring::Points`] — each finish earns points from a **per-position table** (1st most,
//!   descending; positions past the table earn 0), **summed across every heat flown**; rank by
//!   total. With no explicit table the points fall back to the linear `heat_size − position + 1`.
//!   This is what a round-robin sums — and what makes multi-rotation racing meaningful.
//!
//! For a single pass (one heat per pilot) Points and Placement rank alike; the table earns its keep
//! over many passes — a structure driving Head-to-Head repeatedly (round-robin), or this round's
//! own `rotations`.
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

/// A Head-to-Head racing round over a seeded field. `next` emits the round's heats (the field split
/// into groups of [`group_size`](Self::group_size), the same groups repeated
/// [`rotations`](Self::rotations) times) then completes; `ranking` ranks the field under the
/// configured [`Scoring`].
pub struct HeadToHead {
    /// The field in seed/draw order (the recorded seeding already applied) — the split order and the
    /// final ranking's deterministic tie-break.
    field: Vec<CompetitorRef>,
    /// Pilots per heat (clamped to ≥ 2 — a head-to-head heat needs at least two to be a race).
    group_size: usize,
    /// How many times each group races (clamped to ≥ 1). The grouping is drawn once; every rotation
    /// runs the **same** groups — see the module docs for why re-grouping is a structure's job.
    rotations: usize,
    /// How the round ranks its heats' finishes.
    scoring: Scoring,
}

impl HeadToHead {
    /// The format name this registers under.
    pub const NAME: &'static str = "head_to_head";

    /// The default group size when unconfigured (head-to-head proper).
    pub const DEFAULT_GROUP_SIZE: usize = 2;

    /// The default rotations when unconfigured — the classic single pass (everyone races once).
    pub const DEFAULT_ROTATIONS: usize = 1;

    /// The most rotations a round accepts. `lay_out` materializes every rotation's heats up
    /// front, so an unchecked param (`rotations=999999999` over the raw API) would allocate
    /// unbounded memory at fill. 50 back-to-back heats per group is far beyond any real race
    /// day; a request past it clamps here.
    pub const MAX_ROTATIONS: usize = 50;

    /// Build over a `field` in seed order with the given group size (clamped to ≥ 2), rotations
    /// (clamped to ≥ 1), and scoring.
    pub fn new(
        field: Vec<CompetitorRef>,
        group_size: usize,
        rotations: usize,
        scoring: Scoring,
    ) -> Self {
        Self {
            field,
            group_size: group_size.max(2),
            rotations: rotations.clamp(1, Self::MAX_ROTATIONS),
            scoring,
        }
    }

    /// The registry constructor: applies the recorded seeding draw, reads `group_size` (default 2),
    /// `rotations` (default 1) and the `scoring` param (`points` ⇒ [`Scoring::Points`] reading the
    /// `points` per-position table; anything else / absent ⇒ [`Scoring::Placement`]).
    pub fn from_config(config: &FormatConfig) -> Box<dyn Generator> {
        let field = config.seeding.apply(&config.field);
        let group_size = config.param_usize("group_size", Self::DEFAULT_GROUP_SIZE);
        let rotations = config.param_usize("rotations", Self::DEFAULT_ROTATIONS);
        let scoring = match config.params.get("scoring").map(String::as_str) {
            Some("points") => Scoring::Points(parse_points_table(
                config.params.get("points").map(String::as_str),
            )),
            _ => Scoring::Placement,
        };
        Box::new(Self::new(field, group_size, rotations, scoring))
    }

    /// Register this format under [`NAME`](Self::NAME).
    pub fn register(registry: &mut FormatRegistry) {
        registry.register(Self::NAME, Self::from_config);
    }

    /// The heat id for heat `index` (0-based) of rotation `rotation` (0-based). A single-rotation
    /// round keeps the historical `h2h-h{index}` ids (so persisted single-pass rounds keep
    /// resolving); a multi-rotation round scopes them `h2h-r{rotation}-h{index}` (the `tq-r{n}-h{m}`
    /// convention).
    fn heat_id(&self, rotation: usize, index: usize) -> String {
        if self.rotations == 1 {
            format!("h2h-h{index}")
        } else {
            let r = rotation + 1;
            format!("h2h-r{r}-h{index}")
        }
    }

    /// The field split into consecutive heats of `group_size` (the last possibly short), the same
    /// groups repeated for each rotation in order (rotation 1's heats first).
    fn lay_out(&self) -> Vec<HeatPlan> {
        (0..self.rotations)
            .flat_map(|rotation| {
                self.field
                    .chunks(self.group_size)
                    .enumerate()
                    .map(move |(index, chunk)| (rotation, index, chunk.to_vec()))
            })
            .map(|(rotation, index, chunk)| HeatPlan::new(self.heat_id(rotation, index), chunk))
            .collect()
    }
}

impl Generator for HeadToHead {
    /// The **advancing set** a bracket carry (`FromHeatWinners`) reads: each completed heat's
    /// **winner(s)** — in-heat position 1, ties included, a DQ never advances — in heat order,
    /// de-duplicated across rotations (the same groups race again; one advancing slot per pilot).
    ///
    /// This OVERRIDES the default `ranking_advancers` ("everyone strictly better than the worst
    /// ranking position"), which is wrong for a head-to-head level: the Placement ranking breaks
    /// the losers' band by laps, giving losers of different heats *distinct* overall positions —
    /// so the default advanced every loser but the single worst one (a losing semifinalist was
    /// carried into the final). Winners-per-heat is the semantics the seeding doc promises
    /// ("the source round's heat winners, in heat order").
    fn advancers(&self, completed: &[CompletedHeat]) -> Vec<CompetitorRef> {
        let mut seen: BTreeSet<CompetitorRef> = BTreeSet::new();
        let mut advancing = Vec::new();
        for heat in completed {
            for place in &heat.result.places {
                if place.position == 1 && !place.disqualified {
                    let competitor = place.competitor.competitor.clone();
                    if seen.insert(competitor.clone()) {
                        advancing.push(competitor);
                    }
                }
            }
        }
        advancing
    }

    fn next(&mut self, completed: &[CompletedHeat]) -> GeneratorStep {
        // A lone (or empty) field has nothing to race. Otherwise emit every rotation's heats up
        // front (the grouping is fixed, so the whole schedule is known at fill — points racing
        // schedules all heats up front, D17) and complete once they are all in.
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
                    // The linear fallback prices a finish against the round's GROUP size, not the
                    // number of placements the heat happened to produce: a no-show shrinking the
                    // result must not devalue every present pilot's finish (a win in a 4-up group
                    // is worth 4 even if only 2 showed). An explicit table is unaffected (it looks
                    // up by position). A short *trailing* group prices by the same group size, so
                    // equal finishing positions earn equal points across a round's heats.
                    let heat_size = self.group_size;
                    for place in &heat.result.places {
                        // A DISQUALIFIED placement earns nothing: the DQ voids the finish that
                        // decided the position, so it must not pay points (mirrors timed_qual's
                        // DQ exclusion, #331). The pilot's other, clean heats still score.
                        if place.disqualified {
                            continue;
                        }
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
            // better → negated). A pilot who hasn't raced sorts last. Over several rotations (or if
            // a pilot somehow has several heats) the best (position, then laps) counts — placement
            // reads a pilot's best single result; accumulating across heats is Points' job.
            Scoring::Placement => {
                let mut best: BTreeMap<CompetitorRef, (u32, i64)> = BTreeMap::new();
                for heat in completed {
                    for place in &heat.result.places {
                        // A DISQUALIFIED placement is no placement: it must not band the pilot
                        // (a DQ'd "win" is not a win a bracket may advance). DQ'd in every heat
                        // → no key → ranked last, the same place a no-show lands (#331).
                        if place.disqualified {
                            continue;
                        }
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
        let mut g = HeadToHead::new(field(&["A", "B", "C", "D"]), 2, 1, Scoring::Placement);
        assert_eq!(heat_ids(&g.next(&[])), vec!["h2h-h0", "h2h-h1"]);
        // 4-up groups: one heat.
        let mut g4 = HeadToHead::new(field(&["A", "B", "C", "D"]), 4, 1, Scoring::Placement);
        assert_eq!(heat_ids(&g4.next(&[])), vec!["h2h-h0"]);
    }

    #[test]
    fn completes_once_its_single_pass_is_in() {
        let mut g = HeadToHead::new(field(&["A", "B", "C", "D"]), 2, 1, Scoring::Placement);
        let done = vec![
            CompletedHeat::new("h2h-h0", result(&[("A", 1, 5), ("B", 2, 4)])),
            CompletedHeat::new("h2h-h1", result(&[("C", 1, 5), ("D", 2, 4)])),
        ];
        assert_eq!(g.next(&done), GeneratorStep::Complete);
    }

    #[test]
    fn placement_bands_winners_first_then_by_laps() {
        let g = HeadToHead::new(field(&["A", "B", "C", "D"]), 2, 1, Scoring::Placement);
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
            1,
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
        let g = HeadToHead::new(field(&["A", "B", "C"]), 3, 1, Scoring::Points(None));
        // One 3-up heat: A 1st (3 pts), B 2nd (2), C 3rd (1).
        let done = vec![CompletedHeat::new(
            "h2h-h0",
            result(&[("A", 1, 6), ("B", 2, 5), ("C", 3, 4)]),
        )];
        assert_eq!(names(&g.ranking(&done)), vec!["A", "B", "C"]);
    }

    #[test]
    fn provisional_ranking_is_the_seed_order() {
        let g = HeadToHead::new(field(&["A", "B", "C"]), 2, 1, Scoring::Placement);
        assert_eq!(names(&g.ranking(&[])), vec!["A", "B", "C"]);
    }

    #[test]
    fn group_size_clamps_to_two() {
        let g = HeadToHead::new(field(&["A", "B", "C", "D"]), 1, 1, Scoring::Placement);
        assert_eq!(g.group_size, 2);
    }

    #[test]
    fn rotations_repeat_the_same_groups_with_rotation_scoped_ids() {
        // 4 pilots, 2-up, 2 rotations: the SAME two groups run twice, rotation 1's heats first.
        let mut g = HeadToHead::new(field(&["A", "B", "C", "D"]), 2, 2, Scoring::Points(None));
        let step = g.next(&[]);
        assert_eq!(
            heat_ids(&step),
            vec!["h2h-r1-h0", "h2h-r1-h1", "h2h-r2-h0", "h2h-r2-h1"]
        );
        // Every rotation's heat `index` carries the identical lineup (one grouping decision).
        let GeneratorStep::Run(heats) = step else {
            panic!("expected Run")
        };
        assert_eq!(heats[0].lineup, heats[2].lineup);
        assert_eq!(heats[1].lineup, heats[3].lineup);
    }

    #[test]
    fn rotations_clamp_to_one_and_keep_the_single_pass_ids() {
        // rotations = 0 clamps to the classic single pass — historical `h2h-h{N}` ids preserved.
        let mut g = HeadToHead::new(field(&["A", "B", "C", "D"]), 2, 0, Scoring::Placement);
        assert_eq!(heat_ids(&g.next(&[])), vec!["h2h-h0", "h2h-h1"]);
    }

    #[test]
    fn completes_only_after_every_rotation_is_in() {
        let mut g = HeadToHead::new(field(&["A", "B"]), 2, 2, Scoring::Points(None));
        // Rotation 1 done, rotation 2 outstanding → still Run.
        let first = vec![CompletedHeat::new(
            "h2h-r1-h0",
            result(&[("A", 1, 5), ("B", 2, 4)]),
        )];
        assert!(matches!(g.next(&first), GeneratorStep::Run(_)));
        // Both rotations in → Complete.
        let both = vec![
            CompletedHeat::new("h2h-r1-h0", result(&[("A", 1, 5), ("B", 2, 4)])),
            CompletedHeat::new("h2h-r2-h0", result(&[("B", 1, 5), ("A", 2, 4)])),
        ];
        assert_eq!(g.next(&both), GeneratorStep::Complete);
    }

    #[test]
    fn points_accumulate_across_rotations() {
        // Two rotations of the same 2-up groups, linear points (1st = 2, 2nd = 1).
        // A wins both of its heats (4 pts); C and D split theirs (3 pts each); B loses both (2).
        let g = HeadToHead::new(field(&["A", "B", "C", "D"]), 2, 2, Scoring::Points(None));
        let done = vec![
            CompletedHeat::new("h2h-r1-h0", result(&[("A", 1, 5), ("B", 2, 4)])),
            CompletedHeat::new("h2h-r1-h1", result(&[("C", 1, 5), ("D", 2, 4)])),
            CompletedHeat::new("h2h-r2-h0", result(&[("A", 1, 5), ("B", 2, 4)])),
            CompletedHeat::new("h2h-r2-h1", result(&[("D", 1, 5), ("C", 2, 4)])),
        ];
        let ranking = g.ranking(&done);
        assert_eq!(names(&ranking), vec!["A", "C", "D", "B"]);
        assert_eq!(ranking[0].position, 1);
        // C and D tie on 3 points and share position 2.
        assert_eq!(ranking[1].position, 2);
        assert_eq!(ranking[2].position, 2);
        assert_eq!(ranking[3].position, 4);
    }

    #[test]
    fn placement_across_rotations_reads_the_best_single_result() {
        // 2 rotations, placement scoring: B wins rotation 2, so B joins the winners band; the band
        // orders by laps (A's winning 6 > B's winning 5).
        let g = HeadToHead::new(field(&["A", "B"]), 2, 2, Scoring::Placement);
        let done = vec![
            CompletedHeat::new("h2h-r1-h0", result(&[("A", 1, 6), ("B", 2, 5)])),
            CompletedHeat::new("h2h-r2-h0", result(&[("B", 1, 5), ("A", 2, 4)])),
        ];
        assert_eq!(names(&g.ranking(&done)), vec!["A", "B"]);
    }

    /// Mark `name` disqualified in a built result — the adjudicated-DQ fixture (#331).
    fn with_dq(mut result: HeatResult, name: &str) -> HeatResult {
        for place in &mut result.places {
            if place.competitor.competitor.0 == name {
                place.disqualified = true;
            }
        }
        result
    }

    #[test]
    fn points_skip_a_disqualified_placement() {
        // A "wins" the heat but is DQ'd: the voided finish earns NOTHING, so B's earned
        // runner-up point outranks A. Without the skip A would bank the winner's points off a
        // finish the adjudication voided.
        let g = HeadToHead::new(field(&["A", "B"]), 2, 1, Scoring::Points(None));
        let done = vec![CompletedHeat::new(
            "h2h-h0",
            with_dq(result(&[("A", 1, 5), ("B", 2, 4)]), "A"),
        )];
        let ranking = g.ranking(&done);
        assert_eq!(names(&ranking), vec!["B", "A"]);
        assert_eq!(ranking[0].position, 1);
        // A holds 0 points — strictly below B's 1, not tied (a tie would share position 1).
        assert_eq!(ranking[1].position, 2, "the DQ'd win earns zero points");
    }

    #[test]
    fn placement_skips_a_disqualified_placement() {
        // Placement: a DQ'd "win" is no win — A gets no band key and ranks last (the same
        // place a no-show lands), so a bracket reading the ranking can never advance the
        // DQ'd winner.
        let g = HeadToHead::new(field(&["A", "B"]), 2, 1, Scoring::Placement);
        let done = vec![CompletedHeat::new(
            "h2h-h0",
            with_dq(result(&[("A", 1, 5), ("B", 2, 4)]), "A"),
        )];
        assert_eq!(names(&g.ranking(&done)), vec!["B", "A"]);
    }

    #[test]
    fn rotations_clamp_to_max_rotations() {
        // An unchecked `rotations=999` (raw API) clamps to MAX_ROTATIONS: the laid-out
        // schedule is groups × MAX_ROTATIONS heats, not 999 rotations of unbounded memory.
        let mut g = HeadToHead::new(field(&["A", "B", "C", "D"]), 2, 999, Scoring::Points(None));
        assert_eq!(g.rotations, HeadToHead::MAX_ROTATIONS);
        assert_eq!(
            heat_ids(&g.next(&[])).len(),
            2 * HeadToHead::MAX_ROTATIONS,
            "2 groups × the clamped rotation cap"
        );
    }

    #[test]
    fn registry_reads_the_rotations_param() {
        let mut registry = FormatRegistry::new();
        HeadToHead::register(&mut registry);
        let cfg = FormatConfig::new(field(&["A", "B", "C", "D"]))
            .with_param("group_size", "2")
            .with_param("rotations", "3")
            .with_param("scoring", "points");
        let mut g = registry.build(HeadToHead::NAME, &cfg).unwrap();
        // 2 groups × 3 rotations.
        assert_eq!(heat_ids(&g.next(&[])).len(), 6);
    }

    #[test]
    fn points_linear_fallback_prices_by_group_size_not_result_size() {
        // Two 4-up groups; heat 0's C and D no-show (only two placements land in the result).
        // The linear fallback must price by the GROUP size: A's win earns 4 — the same as E's
        // win in the full heat — not `places.len() − pos + 1 = 2`. Before the fix the shrunken
        // result devalued heat 0 wholesale (E would outrank A off an identical win).
        let g = HeadToHead::new(
            field(&["A", "B", "C", "D", "E", "F", "G", "H"]),
            4,
            1,
            Scoring::Points(None),
        );
        let done = vec![
            CompletedHeat::new("h2h-h0", result(&[("A", 1, 5), ("B", 2, 4)])),
            CompletedHeat::new(
                "h2h-h1",
                result(&[("E", 1, 5), ("F", 2, 4), ("G", 3, 3), ("H", 4, 2)]),
            ),
        ];
        let ranking = g.ranking(&done);
        // A and E both earned 4 points and share position 1 (tie → ref order A, E); B and F
        // both earned 3 and share position 3; G (2) and H (1) raced and earned theirs; the
        // no-shows C and D sit on their seeded 0 points, last.
        assert_eq!(
            names(&ranking),
            vec!["A", "E", "B", "F", "G", "H", "C", "D"]
        );
        assert_eq!(ranking[0].position, 1, "A's 2-present win is a full win");
        assert_eq!(ranking[1].position, 1, "E ties A — same finishing position");
        assert_eq!(ranking[2].position, 3);
        assert_eq!(ranking[3].position, 3);
    }

    #[test]
    fn odd_field_lays_out_a_short_trailing_group() {
        // 5 pilots, 2-up: chunks() yields [2, 2, 1] — the trailing group is a SINGLE pilot.
        // This pins the current layout: the 1-pilot heat IS emitted (a solo time-trial-style
        // pass for the odd pilot out), not silently dropped or merged.
        let mut g = HeadToHead::new(field(&["A", "B", "C", "D", "E"]), 2, 1, Scoring::Placement);
        let step = g.next(&[]);
        assert_eq!(heat_ids(&step), vec!["h2h-h0", "h2h-h1", "h2h-h2"]);
        let GeneratorStep::Run(heats) = step else {
            panic!("expected Run")
        };
        assert_eq!(heats[0].lineup, field(&["A", "B"]));
        assert_eq!(heats[1].lineup, field(&["C", "D"]));
        assert_eq!(heats[2].lineup, field(&["E"]), "the odd pilot flies alone");
    }

    #[test]
    fn odd_field_single_pilot_trailing_heat_ranks_under_both_scorings() {
        // The [2, 2, 1] layout raced to completion: the solo pilot's unopposed "win" counts
        // like any other heat win under BOTH scorings (pinning current behavior, not
        // redesigning it — the RD who wants no solo heat picks a different group size).
        let done = vec![
            CompletedHeat::new("h2h-h0", result(&[("A", 1, 5), ("B", 2, 4)])),
            CompletedHeat::new("h2h-h1", result(&[("C", 1, 6), ("D", 2, 3)])),
            CompletedHeat::new("h2h-h2", result(&[("E", 1, 4)])),
        ];
        // Placement: E joins the winners band (position 1 in their heat), ordered within the
        // band by laps — C (6) > A (5) > E (4) — then the runners-up B (4) > D (3).
        let placement =
            HeadToHead::new(field(&["A", "B", "C", "D", "E"]), 2, 1, Scoring::Placement);
        assert_eq!(
            names(&placement.ranking(&done)),
            vec!["C", "A", "E", "B", "D"]
        );
        // Points (linear, group_size 2): every heat winner earns 2 — including E's solo win —
        // so A, C, E tie on 2 points at position 1 (ref order), B and D on 1 point at 4.
        let points = HeadToHead::new(
            field(&["A", "B", "C", "D", "E"]),
            2,
            1,
            Scoring::Points(None),
        );
        let ranking = points.ranking(&done);
        assert_eq!(names(&ranking), vec!["A", "C", "E", "B", "D"]);
        assert_eq!(ranking[0].position, 1);
        assert_eq!(
            ranking[2].position, 1,
            "the solo win pays the same 2 points"
        );
        assert_eq!(ranking[3].position, 4);
    }

    #[test]
    fn odd_field_short_trailing_group_scores_under_both_scorings() {
        // 6 pilots, 4-up: chunks() yields [4, 2] — a short (but real) trailing group.
        let done = vec![
            CompletedHeat::new(
                "h2h-h0",
                result(&[("A", 1, 6), ("B", 2, 5), ("C", 3, 4), ("D", 4, 3)]),
            ),
            CompletedHeat::new("h2h-h1", result(&[("E", 1, 5), ("F", 2, 4)])),
        ];
        let laid_out =
            |scoring| HeadToHead::new(field(&["A", "B", "C", "D", "E", "F"]), 4, 1, scoring);
        let mut g = laid_out(Scoring::Placement);
        let step = g.next(&[]);
        assert_eq!(heat_ids(&step), vec!["h2h-h0", "h2h-h1"]);
        let GeneratorStep::Run(heats) = step else {
            panic!("expected Run")
        };
        assert_eq!(
            heats[1].lineup,
            field(&["E", "F"]),
            "the trailing group is the leftover 2"
        );
        // Placement: winners band A (6 laps) > E (5), then the position-2 band B (5) > F (4),
        // then C, D — the short group's finishes band exactly like the full group's.
        assert_eq!(names(&g.ranking(&done)), vec!["A", "E", "B", "F", "C", "D"]);
        // Points (linear, group_size 4): E's win in the short group earns the full 4 — equal
        // finishing positions earn equal points across the round's heats — so A and E tie at
        // position 1, B and F (3 points each) at 3, then C (2) and D (1).
        let ranking = laid_out(Scoring::Points(None)).ranking(&done);
        assert_eq!(names(&ranking), vec!["A", "E", "B", "F", "C", "D"]);
        assert_eq!(ranking[0].position, 1);
        assert_eq!(
            ranking[1].position, 1,
            "the short-group win pays the full group_size"
        );
        assert_eq!(ranking[2].position, 3);
        assert_eq!(ranking[3].position, 3);
        assert_eq!(ranking[4].position, 5);
        assert_eq!(ranking[5].position, 6);
    }

    #[test]
    fn advancers_carries_only_each_heats_winner_never_a_loser() {
        // The verified bracket-carry bug: a 4-pilot 2-up level — A beats B (5 v 4 laps),
        // C beats D (6 v 3). The Placement ranking breaks the losers' band by laps (B 3rd,
        // D 4th), so the default position-<worst rule advanced [C, A, B] — the losing
        // semifinalist B was seeded into the final. The override must carry exactly the
        // heat winners, in heat order: [A, C].
        let g = HeadToHead::new(field(&["A", "B", "C", "D"]), 2, 1, Scoring::Placement);
        let completed = vec![
            CompletedHeat::new("h2h-r1-h1", result(&[("A", 1, 5), ("B", 2, 4)])),
            CompletedHeat::new("h2h-r1-h2", result(&[("C", 1, 6), ("D", 2, 3)])),
        ];
        assert_eq!(g.advancers(&completed), field(&["A", "C"]));
    }

    #[test]
    fn advancers_dedupes_across_rotations_and_skips_a_dq_winner() {
        // Two rotations of the same group: A wins both — one advancing slot, not two. A
        // heat whose position-1 row is disqualified advances nobody from that heat.
        let g = HeadToHead::new(field(&["A", "B", "C", "D"]), 2, 2, Scoring::Placement);
        let dq = |rows: &[(&str, u32, u32)]| {
            let mut r = result(rows);
            r.places[0].disqualified = true;
            r
        };
        let completed = vec![
            CompletedHeat::new("h2h-r1-h1", result(&[("A", 1, 5), ("B", 2, 4)])),
            CompletedHeat::new("h2h-r1-h2", dq(&[("C", 1, 6), ("D", 2, 3)])),
            CompletedHeat::new("h2h-r2-h1", result(&[("A", 1, 5), ("B", 2, 4)])),
        ];
        assert_eq!(g.advancers(&completed), field(&["A"]));
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
