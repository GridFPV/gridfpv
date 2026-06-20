//! Multi-tier mains generator (#13) — the MultiGP-standard tiered finals: partition a
//! qualifying-ranked field into an A main, a B main, a C main … and race each as one
//! heat, then concatenate their finishing orders into the overall standing.
//!
//! # The format (race-engine.html §3, §5)
//!
//! Multi-tier mains is the classic club finals format. After qualifying produces a
//! **seeded field** (the seed order *is* the qualifying rank), the field is sliced into
//! fixed-size tiers, best seeds first:
//!
//! - the **A main** is the top `main_size` seeds,
//! - the **B main** the next `main_size`,
//! - the **C main** the next, and so on,
//! - down to a possibly-**short last main** when the field does not divide evenly.
//!
//! Each main is an independent heat (`main-A`, `main-B`, …). The mains do not feed each
//! other — there is no advancement between them in v1 — so the generator emits **all of
//! them at once** as a single [`GeneratorStep::Run`]; the heat loop runs them in any
//! order and feeds every result back. (See "bump-up" below for the variant where a lower
//! main's winner advances up.)
//!
//! # Final ranking — concatenation in tier order
//!
//! The overall standing is each main's finishing order, **concatenated A, then B, then
//! C…**: the *worst* finisher of the A main still ranks above the *best* finisher of the
//! B main, and so on down the tiers. This is the defining property of the format — your
//! main placement is bounded by the tier you qualified into. Positions are dense and
//! tie-aware via [`rank_by`], but because each competitor lands in exactly one tier and
//! the tiers are laid out in strict order, the bands never overlap.
//!
//! # Determinism (race-engine.html §6)
//!
//! Tiering is a pure slice of the seeded field (the recorded [`SeedingOutcome`] applied
//! once at construction); the ranking is a pure projection of the completed mains. No
//! clock, no RNG. Same field + same results → same heats and same standing, every replay.
//!
//! # Future: the bump-up variant
//!
//! A common variant lets the **top finisher(s) of a lower main bump up** into the next
//! main (B's winner earns a slot in A, etc.), which turns the independent mains into a
//! dependent ladder run bottom-up. v1 is deliberately **straight tiered mains** (mains
//! are independent, emitted together). Bump-up is a future option (it would emit the
//! lowest main first and seed each higher main from the one below); the trait surface
//! does not change.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use gridfpv_events::CompetitorRef;

use crate::format::{
    CompletedHeat, FormatConfig, FormatRegistry, Generator, GeneratorStep, HeatPlan, RankEntry,
    rank_by, result_ranking,
};

/// Multi-tier mains over a seeded (qualifying-ranked) field.
///
/// Constructed with the seeded field (the recorded [`SeedingOutcome`] already applied,
/// so the field order *is* the qualifying rank) and a `main_size`. `next` emits the A/B/C
/// … main heats; `ranking` concatenates their results in tier order. See the module docs.
///
/// [`SeedingOutcome`]: crate::format::SeedingOutcome
pub struct MultiMain {
    /// The field in qualifying-rank order (best seed first; recorded draw already applied).
    field: Vec<CompetitorRef>,
    /// Competitors per main (the A/B/C… tier size). Clamped to a minimum of 1.
    main_size: usize,
}

impl MultiMain {
    /// The format name this registers under.
    pub const NAME: &'static str = "multi_main";

    /// The default tier size (a standard 4-up main).
    pub const DEFAULT_MAIN_SIZE: usize = 4;

    /// Build over a `field` in qualifying-rank order with the given `main_size` (clamped
    /// to a minimum of 1 — a main needs at least one competitor).
    pub fn new(field: Vec<CompetitorRef>, main_size: usize) -> Self {
        Self {
            field,
            main_size: main_size.max(1),
        }
    }

    /// The registry constructor: applies the recorded `seeding` draw to `field` and reads
    /// the optional `main_size` param (default [`DEFAULT_MAIN_SIZE`](Self::DEFAULT_MAIN_SIZE)).
    pub fn from_config(config: &FormatConfig) -> Box<dyn Generator> {
        let field = config.seeding.apply(&config.field);
        let main_size = config.param_usize("main_size", Self::DEFAULT_MAIN_SIZE);
        Box::new(Self::new(field, main_size))
    }

    /// Register this format under [`NAME`](Self::NAME).
    pub fn register(registry: &mut FormatRegistry) {
        registry.register(Self::NAME, Self::from_config);
    }

    /// The tier letter for main index `index` (0-based): 0 → `A`, 1 → `B`, … For more
    /// tiers than the alphabet holds (>26 mains, vanishingly unlikely) it falls back to
    /// the raw index so ids stay unique and deterministic.
    fn tier_label(index: usize) -> String {
        if index < 26 {
            ((b'A' + index as u8) as char).to_string()
        } else {
            index.to_string()
        }
    }

    /// The heat id for the main at `index`: `main-A`, `main-B`, …
    fn main_id(index: usize) -> String {
        format!("main-{}", Self::tier_label(index))
    }

    /// The tiers as a list of `(heat_id, lineup)` pairs, best tier first.
    ///
    /// Slices the seeded field into chunks of `main_size`; the final chunk may be **short**
    /// (a short last main) when the field does not divide evenly, and a field of `main_size`
    /// or fewer yields a single A main. Pure and deterministic.
    fn tiers(&self) -> Vec<(String, Vec<CompetitorRef>)> {
        self.field
            .chunks(self.main_size)
            .enumerate()
            .map(|(index, chunk)| (Self::main_id(index), chunk.to_vec()))
            .collect()
    }
}

impl Generator for MultiMain {
    fn next(&mut self, completed: &[CompletedHeat]) -> GeneratorStep {
        let tiers = self.tiers();
        // An empty field has no mains to run — it is complete immediately.
        if tiers.is_empty() {
            return GeneratorStep::Complete;
        }

        // The mains are independent, so emit them all at once. Once every main has a
        // recorded result, the format is complete; until then, (re-)emit the full set —
        // `next` stays a pure function of the completed history.
        let scored: std::collections::BTreeSet<&str> =
            completed.iter().map(|c| c.heat.0.as_str()).collect();
        let all_scored = tiers.iter().all(|(id, _)| scored.contains(id.as_str()));
        if all_scored {
            GeneratorStep::Complete
        } else {
            GeneratorStep::Run(
                tiers
                    .into_iter()
                    .map(|(id, lineup)| HeatPlan::new(id, lineup))
                    .collect(),
            )
        }
    }

    fn ranking(&self, completed: &[CompletedHeat]) -> Vec<RankEntry> {
        let by_id: BTreeMap<&str, &CompletedHeat> =
            completed.iter().map(|c| (c.heat.0.as_str(), c)).collect();

        // Walk the tiers best-first; each tier occupies its own band, so concatenating the
        // bands puts every A-main finisher above every B-main finisher, and so on. A tier
        // whose heat has not come back yet falls back to its seed order (provisional).
        //
        // Rows carry a (band, in_band_key) sort key so `rank_by` shares positions only
        // *within* a tier; the competitor ref is the final deterministic tie-break.
        let mut rows: Vec<(CompetitorRef, (u32, u32))> = Vec::new();
        for (band, (id, lineup)) in self.tiers().into_iter().enumerate() {
            let band = band as u32;
            match by_id.get(id.as_str()) {
                Some(heat) => {
                    for entry in result_ranking(&heat.result) {
                        rows.push((entry.competitor, (band, entry.position)));
                    }
                }
                None => {
                    // Provisional: this main has not been scored — keep its seed order.
                    for (i, competitor) in lineup.into_iter().enumerate() {
                        rows.push((competitor, (band, i as u32 + 1)));
                    }
                }
            }
        }
        rank_by(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::SeedingOutcome;
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

    fn names(entries: &[RankEntry]) -> Vec<String> {
        entries.iter().map(|e| e.competitor.0.clone()).collect()
    }

    /// The lineups of a `Run` step (panics if the step is `Complete`).
    fn lineups(step: &GeneratorStep) -> Vec<(String, Vec<String>)> {
        match step {
            GeneratorStep::Run(heats) => heats
                .iter()
                .map(|h| {
                    (
                        h.heat.0.clone(),
                        h.lineup.iter().map(|c| c.0.clone()).collect(),
                    )
                })
                .collect(),
            GeneratorStep::Complete => panic!("expected Run, got Complete"),
        }
    }

    // --- Tiering ------------------------------------------------------------

    #[test]
    fn ten_seed_field_splits_into_a_b_and_a_short_c_main() {
        // 10 seeds, main_size 4 → A(top 4), B(next 4), C(last 2, short).
        let mut g = MultiMain::new(
            field(&["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"]),
            4,
        );
        assert_eq!(
            lineups(&g.next(&[])),
            vec![
                (
                    "main-A".to_string(),
                    vec!["1".into(), "2".into(), "3".into(), "4".into()]
                ),
                (
                    "main-B".to_string(),
                    vec!["5".into(), "6".into(), "7".into(), "8".into()]
                ),
                ("main-C".to_string(), vec!["9".into(), "10".into()]),
            ]
        );
    }

    #[test]
    fn field_at_or_below_main_size_is_a_single_a_main() {
        let mut g = MultiMain::new(field(&["1", "2", "3"]), 4);
        assert_eq!(
            lineups(&g.next(&[])),
            vec![(
                "main-A".to_string(),
                vec!["1".into(), "2".into(), "3".into()]
            )]
        );
    }

    #[test]
    fn field_dividing_evenly_has_no_short_main() {
        let mut g = MultiMain::new(field(&["1", "2", "3", "4", "5", "6"]), 3);
        let ls = lineups(&g.next(&[]));
        assert_eq!(ls.len(), 2);
        assert_eq!(ls[0].1.len(), 3);
        assert_eq!(ls[1].1.len(), 3);
    }

    // --- All mains emitted together -----------------------------------------

    #[test]
    fn emits_every_main_at_once() {
        let mut g = MultiMain::new(field(&["1", "2", "3", "4", "5", "6", "7", "8"]), 4);
        // A and B emitted in a single Run step (the mains are independent).
        assert_eq!(lineups(&g.next(&[])).len(), 2);
    }

    // --- Completion ---------------------------------------------------------

    #[test]
    fn completes_only_after_every_main_is_scored() {
        let mut g = MultiMain::new(field(&["1", "2", "3", "4", "5", "6"]), 3);
        // Only the A main scored so far → still running (B outstanding).
        let a_only = vec![CompletedHeat::new(
            "main-A",
            result(&[("1", 1, 6), ("2", 2, 5), ("3", 3, 4)]),
        )];
        assert!(matches!(g.next(&a_only), GeneratorStep::Run(_)));

        // Both mains scored → complete.
        let both = vec![
            CompletedHeat::new("main-A", result(&[("1", 1, 6), ("2", 2, 5), ("3", 3, 4)])),
            CompletedHeat::new("main-B", result(&[("4", 1, 6), ("5", 2, 5), ("6", 3, 4)])),
        ];
        assert_eq!(g.next(&both), GeneratorStep::Complete);
    }

    // --- Final ranking: concatenation in tier order -------------------------

    #[test]
    fn final_ranking_concatenates_a_then_b_then_c() {
        let mut g = MultiMain::new(
            field(&["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"]),
            4,
        );
        let completed = vec![
            // A main: finishing order 2, 1, 4, 3.
            CompletedHeat::new(
                "main-A",
                result(&[("2", 1, 8), ("1", 2, 7), ("4", 3, 6), ("3", 4, 5)]),
            ),
            // B main: finishing order 5, 8, 6, 7.
            CompletedHeat::new(
                "main-B",
                result(&[("5", 1, 8), ("8", 2, 7), ("6", 3, 6), ("7", 4, 5)]),
            ),
            // C main (short): finishing order 10, 9.
            CompletedHeat::new("main-C", result(&[("10", 1, 8), ("9", 2, 7)])),
        ];
        assert_eq!(g.next(&completed), GeneratorStep::Complete);

        let ranking = g.ranking(&completed);
        // A main first (in its finishing order), then B, then C — concatenated.
        assert_eq!(
            names(&ranking),
            vec!["2", "1", "4", "3", "5", "8", "6", "7", "10", "9"]
        );
        // Positions are 1..=10: the worst A finisher (pos 4) outranks the best B
        // finisher (pos 5), etc.
        for (i, entry) in ranking.iter().enumerate() {
            assert_eq!(entry.position, i as u32 + 1);
        }
    }

    #[test]
    fn b_main_winner_ranks_below_the_a_mains_last() {
        let g = MultiMain::new(field(&["1", "2", "3", "4", "5", "6", "7", "8"]), 4);
        let completed = vec![
            CompletedHeat::new(
                "main-A",
                result(&[("1", 1, 8), ("2", 2, 7), ("3", 3, 6), ("4", 4, 5)]),
            ),
            CompletedHeat::new(
                "main-B",
                result(&[("5", 1, 8), ("6", 2, 7), ("7", 3, 6), ("8", 4, 5)]),
            ),
        ];
        let ranking = g.ranking(&completed);
        // The A-main's last finisher (4) is 4th overall; the B-main's winner (5) is 5th.
        let pos = |c: &str| {
            ranking
                .iter()
                .find(|e| e.competitor.0 == c)
                .unwrap()
                .position
        };
        assert_eq!(pos("4"), 4);
        assert_eq!(pos("5"), 5);
        assert!(pos("4") < pos("5"));
    }

    // --- Provisional ranking ------------------------------------------------

    #[test]
    fn provisional_ranking_before_any_heat_is_the_seed_order() {
        let g = MultiMain::new(field(&["1", "2", "3", "4", "5", "6"]), 3);
        assert_eq!(names(&g.ranking(&[])), vec!["1", "2", "3", "4", "5", "6"]);
    }

    #[test]
    fn provisional_ranking_uses_scored_mains_and_seed_order_for_the_rest() {
        let g = MultiMain::new(field(&["1", "2", "3", "4", "5", "6"]), 3);
        // Only A scored (re-ordered 3,1,2); B keeps seed order behind it.
        let a_only = vec![CompletedHeat::new(
            "main-A",
            result(&[("3", 1, 8), ("1", 2, 7), ("2", 3, 6)]),
        )];
        assert_eq!(
            names(&g.ranking(&a_only)),
            vec!["3", "1", "2", "4", "5", "6"]
        );
    }

    // --- Determinism --------------------------------------------------------

    #[test]
    fn next_is_deterministic_for_the_same_history() {
        let mut g1 = MultiMain::new(field(&["1", "2", "3", "4", "5"]), 2);
        let mut g2 = MultiMain::new(field(&["1", "2", "3", "4", "5"]), 2);
        assert_eq!(g1.next(&[]), g2.next(&[]));
    }

    #[test]
    fn seeding_draw_reorders_the_tiers_deterministically() {
        // The qualifying rank arrives as a recorded draw; tiering follows that order.
        let cfg = FormatConfig::new(field(&["1", "2", "3", "4"]))
            .with_seeding(SeedingOutcome::drawn(field(&["3", "1", "4", "2"])))
            .with_param("main_size", "2");
        let mut g1 = MultiMain::from_config(&cfg);
        let mut g2 = MultiMain::from_config(&cfg);
        let s1 = g1.next(&[]);
        assert_eq!(s1, g2.next(&[]));
        // Drawn order 3,1,4,2 → A(3,1), B(4,2).
        assert_eq!(
            lineups(&s1),
            vec![
                ("main-A".to_string(), vec!["3".into(), "1".into()]),
                ("main-B".to_string(), vec!["4".into(), "2".into()]),
            ]
        );
    }

    // --- Registry -----------------------------------------------------------

    #[test]
    fn registry_builds_multi_main_with_default_main_size() {
        let mut registry = FormatRegistry::new();
        MultiMain::register(&mut registry);
        assert_eq!(registry.names(), vec!["multi_main"]);

        // Default main_size 4: 5 seeds → A(4), B(1, short).
        let cfg = FormatConfig::new(field(&["1", "2", "3", "4", "5"]));
        let mut g = registry
            .build(MultiMain::NAME, &cfg)
            .expect("multi_main is registered");
        let ls = lineups(&g.next(&[]));
        assert_eq!(ls[0].0, "main-A");
        assert_eq!(ls[0].1.len(), 4);
        assert_eq!(ls[1].0, "main-B");
        assert_eq!(ls[1].1, vec!["5".to_string()]);
    }

    #[test]
    fn registry_reads_the_main_size_param() {
        let mut registry = FormatRegistry::new();
        MultiMain::register(&mut registry);
        let cfg =
            FormatConfig::new(field(&["1", "2", "3", "4", "5", "6"])).with_param("main_size", "3");
        let mut g = registry.build(MultiMain::NAME, &cfg).unwrap();
        let ls = lineups(&g.next(&[]));
        assert_eq!(ls.len(), 2);
        assert_eq!(ls[0].1.len(), 3);
        assert_eq!(ls[1].1.len(), 3);
    }
}
