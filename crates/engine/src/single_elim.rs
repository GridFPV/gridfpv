//! Single-elimination bracket generator (#34) — a [`Generator`] that seeds a field
//! into a knockout bracket and advances winners round by round until one remains.
//!
//! # The format (race-engine.html §3, §5)
//!
//! A single-elimination bracket is the archetypal **fixed-but-state-driven** format
//! (RE §3): the whole bracket is determined by the seeded field, yet every round is
//! still derived from the results so far — `next` reads the completed heats, takes
//! each heat's winner(s), and lays out the next round (RE §5, "winners advance toward
//! a final"). It never reads a clock or an RNG: any seeding draw is resolved once and
//! injected as a [`SeedingOutcome`] at construction (RE §6), so the bracket replays
//! identically.
//!
//! # Seeding & round layout
//!
//! 1. **Seed the field.** The config carries the field in seed order (best first); a
//!    recorded [`SeedingOutcome`] is applied first (identity if none), giving the
//!    draw order the bracket actually uses.
//! 2. **Pair strong-vs-weak.** [`bracket_pairs`] reorders the seeds `[1, 8, 2, 7, …]`
//!    so consecutive entries are match-ups (1 v 8, 2 v 7, …) — the standard bracket
//!    seeding that keeps top seeds apart until late rounds.
//! 3. **Chunk into heats.** The bracket order is split into heats of `heat_size`
//!    competitors (default **2**, i.e. head-to-head; set `heat_size=4` for 4-up
//!    heats). Each heat advances its **top half** (`heat_size / 2`, at least one):
//!    head-to-head advances the winner; a 4-up heat advances its top two.
//!
//! # Byes (odd / short fields)
//!
//! When the bracket order does not divide evenly into full heats, the **trailing**
//! competitors form a short final chunk. A chunk that ends up with a *single*
//! competitor is a **bye**: that competitor advances to the next round without flying
//! a heat, deterministically. Because [`bracket_pairs`] places an odd field's middle
//! seed last, the bye naturally falls to that seed (e.g. a 5-field head-to-head lays
//! out `1 v 5`, `2 v 4`, and `3` byes). A short chunk that still has two or more
//! competitors is run as a smaller heat (e.g. a 6-field 4-up round runs a 4-up heat
//! and a 2-up heat).
//!
//! # Ranking
//!
//! [`Generator::ranking`] is the **bracket placement**: the eventual winner first, the
//! runner-up (whoever lost the final) second, then everyone else ordered by **how far
//! they advanced** — competitors eliminated in a later round outrank those knocked out
//! earlier. Within a round, finishers are ordered by the [`HeatResult`] that knocked
//! them out (better in-heat position first), with the competitor ref as the final
//! deterministic tie-break. Provisional before the bracket completes, final after.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use gridfpv_events::CompetitorRef;

use crate::format::{
    CompletedHeat, FormatConfig, FormatRegistry, Generator, GeneratorStep, HeatPlan, RankEntry,
    bracket_pairs, rank_by, result_ranking,
};
use crate::scoring::HeatResult;

/// A single-elimination bracket over a seeded field.
///
/// Constructed with the seeded field (draw order already applied) and a `heat_size`;
/// `next` drives the rounds and `ranking` reports the bracket placement. See the
/// module docs for the seeding / bye / advancement rules.
pub struct SingleElim {
    /// The field in seed/draw order (the recorded [`SeedingOutcome`] already applied).
    field: Vec<CompetitorRef>,
    /// Competitors per heat (default 2 = head-to-head). Each heat advances its top
    /// half (`heat_size / 2`, at least one).
    heat_size: usize,
}

impl SingleElim {
    /// The format name this registers under.
    pub const NAME: &'static str = "single_elim";

    /// Build over a `field` in seed order with the given `heat_size` (clamped to a
    /// minimum of 2 — a heat needs at least two competitors to eliminate anyone).
    pub fn new(field: Vec<CompetitorRef>, heat_size: usize) -> Self {
        Self {
            field,
            heat_size: heat_size.max(2),
        }
    }

    /// The registry constructor: applies the recorded `seeding` draw to `field` and
    /// reads the optional `heat_size` param (default 2 = head-to-head).
    pub fn from_config(config: &FormatConfig) -> Box<dyn Generator> {
        let field = config.seeding.apply(&config.field);
        let heat_size = config.param_usize("heat_size", 2);
        Box::new(Self::new(field, heat_size))
    }

    /// Register this format under [`NAME`](Self::NAME).
    pub fn register(registry: &mut FormatRegistry) {
        registry.register(Self::NAME, Self::from_config);
    }

    /// How many competitors advance out of a heat of `n` competitors: the top half,
    /// at least one. (Head-to-head → 1; 4-up → 2; a short 3-up chunk → 1.)
    fn advance_count(&self, n: usize) -> usize {
        (self.heat_size / 2).min(n.saturating_sub(1)).max(1).min(n)
    }

    /// The heat id for heat `index` (0-based) of round `round` (1-based).
    fn heat_id(round: usize, index: usize) -> String {
        format!("se-r{round}-h{index}")
    }

    /// Lay `order` (a round's bracket order) into heats of `heat_size`, returning the
    /// heat plans **and** the byes (single-competitor chunks that advance for free).
    ///
    /// A trailing chunk with two or more competitors is a (possibly short) heat; a
    /// trailing chunk of exactly one is a bye. The split is deterministic: chunks are
    /// taken left-to-right in `order`.
    fn lay_out_round(
        &self,
        round: usize,
        order: &[CompetitorRef],
    ) -> (Vec<HeatPlan>, Vec<CompetitorRef>) {
        let mut heats = Vec::new();
        let mut byes = Vec::new();
        for (index, chunk) in order.chunks(self.heat_size).enumerate() {
            if chunk.len() == 1 {
                byes.push(chunk[0].clone());
            } else {
                heats.push(HeatPlan::new(Self::heat_id(round, index), chunk.to_vec()));
            }
        }
        (heats, byes)
    }

    /// Replay the bracket from the completed history, returning, for each round that
    /// has *fully* completed, the competitors advancing out of it (in bracket order),
    /// plus the bracket order of the round currently awaiting heats (if any).
    ///
    /// This is the pure core both [`next`](Generator::next) and
    /// [`ranking`](Generator::ranking) build on: it walks round by round, matching each
    /// round's emitted heats against `completed` by heat id, and only advances to the
    /// next round once every heat of the current one has come back.
    fn replay(&self, completed: &[CompletedHeat]) -> Replay {
        let by_id: BTreeMap<&str, &HeatResult> = completed
            .iter()
            .map(|c| (c.heat.0.as_str(), &c.result))
            .collect();

        let mut round = 1usize;
        let mut order = bracket_pairs(&self.field);
        let mut eliminated_by_round: Vec<Vec<RankEntry>> = Vec::new();

        loop {
            // A single survivor (or empty field) ends the bracket — no more heats.
            if order.len() <= 1 {
                return Replay {
                    pending: None,
                    survivors: order,
                    eliminated_by_round,
                };
            }

            let (heats, byes) = self.lay_out_round(round, &order);

            // Are all of this round's heats complete? If any is missing, this round is
            // the one we are waiting on — return it as pending.
            let results: Option<Vec<&HeatResult>> = heats
                .iter()
                .map(|h| by_id.get(h.heat.0.as_str()).copied())
                .collect();
            let Some(results) = results else {
                return Replay {
                    pending: Some((round, heats)),
                    survivors: order,
                    eliminated_by_round,
                };
            };

            // Round complete: advance each heat's top half, in heat order, then the
            // byes; record who this round eliminated (heat losers, in heat order).
            let mut next_order = Vec::new();
            let mut eliminated = Vec::new();
            for (heat, result) in heats.iter().zip(results) {
                let ranking = result_ranking(result);
                let advance = self.advance_count(heat.lineup.len());
                for (i, entry) in ranking.iter().enumerate() {
                    if i < advance {
                        next_order.push(entry.competitor.clone());
                    } else {
                        eliminated.push(entry.clone());
                    }
                }
            }
            next_order.extend(byes);
            eliminated_by_round.push(eliminated);

            order = next_order;
            round += 1;
        }
    }
}

/// The outcome of replaying the completed history (see [`SingleElim::replay`]).
struct Replay {
    /// The round awaiting heats and the heats it needs run, if the bracket is still
    /// in progress; `None` once one survivor remains.
    pending: Option<(usize, Vec<HeatPlan>)>,
    /// The competitors still in the bracket, in bracket order — one entry once the
    /// bracket is complete (the winner).
    survivors: Vec<CompetitorRef>,
    /// Per round (earliest first), the competitors that round eliminated, each with
    /// the in-heat [`RankEntry`] that knocked them out.
    eliminated_by_round: Vec<Vec<RankEntry>>,
}

impl Generator for SingleElim {
    fn next(&mut self, completed: &[CompletedHeat]) -> GeneratorStep {
        match self.replay(completed).pending {
            Some((_round, heats)) => GeneratorStep::Run(heats),
            None => GeneratorStep::Complete,
        }
    }

    fn ranking(&self, completed: &[CompletedHeat]) -> Vec<RankEntry> {
        let replay = self.replay(completed);

        // Bands, best first: the survivors still in (the winner once complete, or the
        // current field mid-bracket), then each round's eliminated set from the latest
        // round (advanced furthest) back to the first (knocked out earliest).
        //
        // Rows carry a (band, in_band_key) sort key; `rank_by` turns equal-band rows
        // into shared positions and the competitor ref is the final tie-break.
        let mut rows: Vec<(CompetitorRef, (u32, u32))> = Vec::new();
        let mut band = 0u32;

        // Survivors: still in the bracket. With a single survivor this is the winner
        // alone at band 0; mid-bracket it is the live field in bracket order.
        for (i, competitor) in replay.survivors.iter().enumerate() {
            rows.push((competitor.clone(), (band, i as u32)));
        }
        band += 1;

        // Eliminated, latest round first (they advanced furthest, so rank higher).
        for eliminated in replay.eliminated_by_round.iter().rev() {
            for entry in eliminated {
                rows.push((entry.competitor.clone(), (band, entry.position)));
            }
            band += 1;
        }

        rank_by(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::SeedingOutcome;
    use crate::scoring::{Metric, Placement};
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

    /// A head-to-head result where `winner` beats `loser`.
    fn h2h(winner: &str, loser: &str) -> HeatResult {
        result(&[(winner, 1, 5), (loser, 2, 3)])
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

    // --- Round-1 layout -----------------------------------------------------

    #[test]
    fn round_one_pairs_strong_vs_weak_head_to_head() {
        let mut g = SingleElim::new(field(&["1", "2", "3", "4"]), 2);
        // bracket order 1,4,2,3 → heats (1 v 4), (2 v 3).
        assert_eq!(
            lineups(&g.next(&[])),
            vec![
                ("se-r1-h0".to_string(), vec!["1".into(), "4".into()]),
                ("se-r1-h1".to_string(), vec!["2".into(), "3".into()]),
            ]
        );
    }

    #[test]
    fn round_one_odd_field_gives_the_middle_seed_a_bye() {
        // 5 seeds: bracket order 1,5,2,4,3 → heats (1 v 5), (2 v 4), and 3 byes.
        let mut g = SingleElim::new(field(&["1", "2", "3", "4", "5"]), 2);
        assert_eq!(
            lineups(&g.next(&[])),
            vec![
                ("se-r1-h0".to_string(), vec!["1".into(), "5".into()]),
                ("se-r1-h1".to_string(), vec!["2".into(), "4".into()]),
            ]
        );
    }

    #[test]
    fn four_up_heats_chunk_the_bracket_order() {
        // 8 seeds, 4-up: bracket order 1,8,2,7,3,6,4,5 → two 4-up heats.
        let mut g = SingleElim::new(field(&["1", "2", "3", "4", "5", "6", "7", "8"]), 4);
        assert_eq!(
            lineups(&g.next(&[])),
            vec![
                (
                    "se-r1-h0".to_string(),
                    vec!["1".into(), "8".into(), "2".into(), "7".into()]
                ),
                (
                    "se-r1-h1".to_string(),
                    vec!["3".into(), "6".into(), "4".into(), "5".into()]
                ),
            ]
        );
    }

    // --- Advancement --------------------------------------------------------

    #[test]
    fn winners_advance_to_the_next_round() {
        let mut g = SingleElim::new(field(&["1", "2", "3", "4"]), 2);
        // Round 1: seed 1 beats 4, seed 2 beats 3.
        let r1 = vec![
            CompletedHeat::new("se-r1-h0", h2h("1", "4")),
            CompletedHeat::new("se-r1-h1", h2h("2", "3")),
        ];
        // The final lines up the two winners in heat order: 1 v 2.
        assert_eq!(
            lineups(&g.next(&r1)),
            vec![("se-r2-h0".to_string(), vec!["1".into(), "2".into()])]
        );
    }

    #[test]
    fn bye_competitor_advances_without_a_heat() {
        // 3 seeds: round 1 is (1 v 3) plus a bye for seed 2.
        let mut g = SingleElim::new(field(&["1", "2", "3"]), 2);
        assert_eq!(
            lineups(&g.next(&[])),
            vec![("se-r1-h0".to_string(), vec!["1".into(), "3".into()])]
        );
        // Seed 1 wins its heat; seed 2 had the bye → final is 1 v 2.
        let r1 = vec![CompletedHeat::new("se-r1-h0", h2h("1", "3"))];
        assert_eq!(
            lineups(&g.next(&r1)),
            vec![("se-r2-h0".to_string(), vec!["1".into(), "2".into()])]
        );
    }

    #[test]
    fn four_up_advances_top_two_of_each_heat() {
        let mut g = SingleElim::new(field(&["1", "2", "3", "4", "5", "6", "7", "8"]), 4);
        let _ = g.next(&[]);
        // Heat h0 (1,8,2,7): order 2,1,8,7 → 2,1 advance. Heat h1 (3,6,4,5): order
        // 4,5,3,6 → 4,5 advance. Next round (4-up) is one heat of those four.
        let r1 = vec![
            CompletedHeat::new(
                "se-r1-h0",
                result(&[("2", 1, 6), ("1", 2, 5), ("8", 3, 4), ("7", 4, 3)]),
            ),
            CompletedHeat::new(
                "se-r1-h1",
                result(&[("4", 1, 6), ("5", 2, 5), ("3", 3, 4), ("6", 4, 3)]),
            ),
        ];
        assert_eq!(
            lineups(&g.next(&r1)),
            vec![(
                "se-r2-h0".to_string(),
                vec!["2".into(), "1".into(), "4".into(), "5".into()]
            )]
        );
    }

    // --- Completion ---------------------------------------------------------

    #[test]
    fn bracket_completes_when_one_remains() {
        let mut g = SingleElim::new(field(&["1", "2", "3", "4"]), 2);
        let completed = vec![
            CompletedHeat::new("se-r1-h0", h2h("1", "4")),
            CompletedHeat::new("se-r1-h1", h2h("2", "3")),
            // Final: seed 2 beats seed 1.
            CompletedHeat::new("se-r2-h0", h2h("2", "1")),
        ];
        assert_eq!(g.next(&completed), GeneratorStep::Complete);
    }

    #[test]
    fn single_competitor_field_completes_immediately() {
        let mut g = SingleElim::new(field(&["1"]), 2);
        assert_eq!(g.next(&[]), GeneratorStep::Complete);
        assert_eq!(names(&g.ranking(&[])), vec!["1"]);
    }

    // --- Final ranking ------------------------------------------------------

    #[test]
    fn final_ranking_is_winner_runner_up_then_by_round_eliminated() {
        let mut g = SingleElim::new(field(&["1", "2", "3", "4"]), 2);
        let completed = vec![
            // Round 1: 1 beats 4, 2 beats 3.
            CompletedHeat::new("se-r1-h0", h2h("1", "4")),
            CompletedHeat::new("se-r1-h1", h2h("2", "3")),
            // Final: 2 beats 1.
            CompletedHeat::new("se-r2-h0", h2h("2", "1")),
        ];
        assert_eq!(g.next(&completed), GeneratorStep::Complete);

        let ranking = g.ranking(&completed);
        // Winner 2, runner-up 1 (lost the final), then the round-1 losers (3, 4)
        // ordered by competitor ref as the final tie-break.
        assert_eq!(names(&ranking), vec!["2", "1", "3", "4"]);
        assert_eq!(ranking[0].position, 1);
        assert_eq!(ranking[1].position, 2);
        assert_eq!(ranking[2].position, 3);
        assert_eq!(ranking[3].position, 3); // 3 and 4 share the round-1 elimination band
    }

    #[test]
    fn provisional_ranking_tracks_state() {
        let g = SingleElim::new(field(&["1", "2", "3", "4"]), 2);
        // Before any heat: bracket order is the provisional ranking.
        assert_eq!(names(&g.ranking(&[])), vec!["1", "4", "2", "3"]);
        // After round 1: the two winners lead (still in), then the two losers.
        let r1 = vec![
            CompletedHeat::new("se-r1-h0", h2h("1", "4")),
            CompletedHeat::new("se-r1-h1", h2h("2", "3")),
        ];
        let ranking = g.ranking(&r1);
        // Survivors 1, 2 lead (band 0, bracket order); losers 3, 4 share the next band.
        assert_eq!(&names(&ranking)[..2], &["1".to_string(), "2".to_string()]);
        assert_eq!(ranking[0].position, 1);
        assert_eq!(ranking[1].position, 2);
        assert_eq!(ranking[2].position, 3);
        assert_eq!(ranking[3].position, 3);
    }

    // --- Determinism --------------------------------------------------------

    #[test]
    fn next_is_deterministic_for_the_same_history() {
        let mut g1 = SingleElim::new(field(&["1", "2", "3", "4"]), 2);
        let mut g2 = SingleElim::new(field(&["1", "2", "3", "4"]), 2);
        assert_eq!(g1.next(&[]), g2.next(&[]));
    }

    #[test]
    fn seeding_draw_reorders_the_bracket_deterministically() {
        let cfg = FormatConfig::new(field(&["1", "2", "3", "4"]))
            .with_seeding(SeedingOutcome::drawn(field(&["3", "1", "4", "2"])));
        let mut g1 = SingleElim::from_config(&cfg);
        let mut g2 = SingleElim::from_config(&cfg);
        let s1 = g1.next(&[]);
        assert_eq!(s1, g2.next(&[]));
        // Drawn order 3,1,4,2 → bracket order 3,2,1,4 → heats (3 v 2), (1 v 4).
        assert_eq!(
            lineups(&s1),
            vec![
                ("se-r1-h0".to_string(), vec!["3".into(), "2".into()]),
                ("se-r1-h1".to_string(), vec!["1".into(), "4".into()]),
            ]
        );
    }

    // --- Registry -----------------------------------------------------------

    #[test]
    fn registry_builds_single_elim() {
        let mut registry = FormatRegistry::new();
        SingleElim::register(&mut registry);
        assert_eq!(registry.names(), vec!["single_elim"]);

        let cfg = FormatConfig::new(field(&["1", "2", "3", "4"]));
        let mut g = registry
            .build(SingleElim::NAME, &cfg)
            .expect("single_elim is registered");
        assert_eq!(
            lineups(&g.next(&[])),
            vec![
                ("se-r1-h0".to_string(), vec!["1".into(), "4".into()]),
                ("se-r1-h1".to_string(), vec!["2".into(), "3".into()]),
            ]
        );
    }

    #[test]
    fn registry_reads_the_heat_size_param() {
        let mut registry = FormatRegistry::new();
        SingleElim::register(&mut registry);
        let cfg = FormatConfig::new(field(&["1", "2", "3", "4", "5", "6", "7", "8"]))
            .with_param("heat_size", "4");
        let mut g = registry.build(SingleElim::NAME, &cfg).unwrap();
        let step = g.next(&[]);
        // 4-up: two heats of four.
        assert_eq!(lineups(&step).len(), 2);
        assert_eq!(lineups(&step)[0].1.len(), 4);
    }

    // --- Larger bracket end-to-end (table) ----------------------------------

    #[test]
    fn full_eight_seed_bracket_runs_to_a_single_winner() {
        let mut g = SingleElim::new(field(&["1", "2", "3", "4", "5", "6", "7", "8"]), 2);
        // Round 1: bracket 1,8,2,7,3,6,4,5 → heats (1v8),(2v7),(3v6),(4v5).
        let r1 = vec![
            CompletedHeat::new("se-r1-h0", h2h("1", "8")),
            CompletedHeat::new("se-r1-h1", h2h("2", "7")),
            CompletedHeat::new("se-r1-h2", h2h("3", "6")),
            CompletedHeat::new("se-r1-h3", h2h("4", "5")),
        ];
        // Round 2 (semis): winners 1,2,3,4 → heats (1v2),(3v4).
        assert_eq!(
            lineups(&g.next(&r1)),
            vec![
                ("se-r2-h0".to_string(), vec!["1".into(), "2".into()]),
                ("se-r2-h1".to_string(), vec!["3".into(), "4".into()]),
            ]
        );
        let mut completed = r1;
        completed.push(CompletedHeat::new("se-r2-h0", h2h("1", "2")));
        completed.push(CompletedHeat::new("se-r2-h1", h2h("4", "3")));
        // Round 3 (final): 1 v 4.
        assert_eq!(
            lineups(&g.next(&completed)),
            vec![("se-r3-h0".to_string(), vec!["1".into(), "4".into()])]
        );
        completed.push(CompletedHeat::new("se-r3-h0", h2h("1", "4")));
        assert_eq!(g.next(&completed), GeneratorStep::Complete);

        // Final ranking: winner 1, runner-up 4, then the semi losers (2,3), then the
        // round-1 losers (5,6,7,8), each band sharing a position.
        let ranking = g.ranking(&completed);
        assert_eq!(ranking[0].competitor, cref("1"));
        assert_eq!(ranking[0].position, 1);
        assert_eq!(ranking[1].competitor, cref("4"));
        assert_eq!(ranking[1].position, 2);
        // Semi losers 2 and 3 share position 3.
        let semi: Vec<&str> = ranking[2..4]
            .iter()
            .map(|e| e.competitor.0.as_str())
            .collect();
        assert_eq!(semi, vec!["2", "3"]);
        assert_eq!(ranking[2].position, 3);
        assert_eq!(ranking[3].position, 3);
        // Round-1 losers 5,6,7,8 share position 5.
        assert_eq!(ranking[4].position, 5);
        assert_eq!(ranking[7].position, 5);
        assert_eq!(ranking.len(), 8);
    }
}
