//! Single-elimination bracket generator (#34, #217) — a [`Generator`] for **one bracket
//! level**: it seeds a field, pairs strong-vs-weak, and runs that level's heats. Advancement
//! to the next level is **round-to-round** (each level is its own round), not intra-round.
//!
//! # Level-per-round (decisions D13, #217)
//!
//! A single-elimination bracket used to be **one round for the whole bracket**, with
//! intra-round advancement walking the bracket tree inside a single generator. That made the
//! bracket round a special, internally-stateful shape unlike every other round. The model is
//! now **one round per bracket level**: Quarters, Semis, Final are three rounds, each seeded
//! from the previous level's heat winners (`SeedingRule::FromHeatWinners` server-side). This
//! generator therefore produces a **single level**:
//!
//! 1. **Seed the field.** The config carries the level's field in seed order (best first); a
//!    recorded [`SeedingOutcome`] is applied first (identity if none), giving the draw order
//!    the level actually uses. For the **first** level this is the quali top-N; for a later
//!    level it is the prior level's heat winners (in heat order).
//! 2. **Pair strong-vs-weak.** [`bracket_pairs`] reorders the seeds `[1, 8, 2, 7, …]` so
//!    consecutive entries are match-ups (1 v 8, 2 v 7, …) — the standard bracket seeding that
//!    keeps top seeds apart. (For a winners-seeded later level whose field is already in heat
//!    order this still produces a deterministic, stable pairing.)
//! 3. **Chunk into heats.** The bracket order is split into heats of `heat_size` competitors
//!    (default **2**, i.e. head-to-head; set `heat_size=4` for 4-up heats). Each heat advances
//!    `advance` competitors (default `heat_size / 2`, at least one): head-to-head advances the
//!    winner; a 4-up heat advances its top two by default, or set `advance=1` to advance only the
//!    winner of each 4-up heat.
//!
//! The generator emits exactly that one level's heats, then declares the level
//! [`Complete`](GeneratorStep::Complete). There is **no** next-level layout inside it; the next
//! level is a separate round seeded from this level's [`Generator::ranking`] / heat winners.
//!
//! # Byes (odd / short fields)
//!
//! When the bracket order does not divide evenly into full heats, the **trailing** competitors
//! form a short final chunk. A chunk that ends up with a *single* competitor is a **bye**: that
//! competitor advances out of the level without flying a heat, deterministically. Because
//! [`bracket_pairs`] places an odd field's middle seed last, the bye naturally falls to that
//! seed (e.g. a 5-field head-to-head lays out `1 v 5`, `2 v 4`, and `3` byes). A short chunk
//! that still has two or more competitors is run as a smaller heat.
//!
//! # Ranking (one level's placement)
//!
//! [`Generator::ranking`] is this **level's** placement: the competitors who **advance** out of
//! the level first — in **heat order** (each heat's top half, then the byes) — then the
//! competitors the level eliminated (heat losers, in heat order), each ordered by the
//! [`HeatResult`] within their heat with the competitor ref as the final deterministic
//! tie-break. The advancing set, in this order, is exactly what the next level seeds from.
//! Provisional (the bracket order) before the level's heats complete, final after.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use gridfpv_events::CompetitorRef;

use crate::format::{
    CompletedHeat, FormatConfig, FormatRegistry, Generator, GeneratorStep, HeatPlan, RankEntry,
    bracket_pairs, rank_by, ranking_advancers, result_ranking,
};
use crate::scoring::HeatResult;

/// A single-elimination bracket **level** over a seeded field.
///
/// Constructed with the seeded field (draw order already applied) and a `heat_size`; `next`
/// emits this one level's heats (then completes) and `ranking` reports the level placement
/// (advancers first, in heat order). See the module docs for the seeding / bye / advancement
/// rules. Advancement to the next level is a separate round (server-side `FromHeatWinners`).
pub struct SingleElim {
    /// The field in seed/draw order (the recorded [`SeedingOutcome`] already applied).
    field: Vec<CompetitorRef>,
    /// Competitors per heat (default 2 = head-to-head). Each heat advances `advance` competitors.
    heat_size: usize,
    /// How many competitors advance out of each heat (at least one). Defaults to the top half
    /// (`heat_size / 2`); the RD can override it (e.g. a 4-up heat advancing only the top 1).
    advance: usize,
}

impl SingleElim {
    /// The format name this registers under.
    pub const NAME: &'static str = "single_elim";

    /// Build over a `field` in seed order with the given `heat_size` (clamped to a minimum of
    /// 2 — a heat needs at least two competitors to eliminate anyone).
    pub fn new(field: Vec<CompetitorRef>, heat_size: usize) -> Self {
        let heat_size = heat_size.max(2);
        Self {
            field,
            heat_size,
            advance: (heat_size / 2).max(1),
        }
    }

    /// Override how many competitors advance out of each heat (clamped to a minimum of 1 — a heat
    /// must advance someone). Defaults to `heat_size / 2`.
    pub fn with_advance(mut self, advance: usize) -> Self {
        self.advance = advance.max(1);
        self
    }

    /// The registry constructor: applies the recorded `seeding` draw to `field`, reads the optional
    /// `heat_size` param (default 2 = head-to-head) and the optional `advance` param (how many
    /// advance per heat, default `heat_size / 2`).
    pub fn from_config(config: &FormatConfig) -> Box<dyn Generator> {
        let field = config.seeding.apply(&config.field);
        let heat_size = config.param_usize("heat_size", 2);
        let advance = config.param_usize("advance", heat_size / 2);
        Box::new(Self::new(field, heat_size).with_advance(advance))
    }

    /// Register this format under [`NAME`](Self::NAME).
    pub fn register(registry: &mut FormatRegistry) {
        registry.register(Self::NAME, Self::from_config);
    }

    /// How many competitors advance out of a heat of `n` competitors: the configured `advance`,
    /// clamped so at least one advances, at least one is eliminated, and never more than `n`.
    /// (Head-to-head → 1; a 4-up heat with `advance=2` → 2, with `advance=1` → 1; a short chunk
    /// shorter than `advance` advances all but one.)
    fn advance_count(&self, n: usize) -> usize {
        self.advance.min(n.saturating_sub(1)).max(1).min(n)
    }

    /// The heat id for heat `index` (0-based) of this level. A single level numbers its heats
    /// `se-h{index}`; round-to-round advancement is expressed by *separate rounds*, each with
    /// its own [`crate::format::HeatPlan`] ids (the server scopes them per round).
    fn heat_id(index: usize) -> String {
        format!("se-h{index}")
    }

    /// Lay the level's bracket `order` into heats of `heat_size`, returning the heat plans
    /// **and** the byes (single-competitor chunks that advance for free).
    ///
    /// A trailing chunk with two or more competitors is a (possibly short) heat; a trailing
    /// chunk of exactly one is a bye. The split is deterministic: chunks are taken
    /// left-to-right in `order`.
    fn lay_out(&self, order: &[CompetitorRef]) -> (Vec<HeatPlan>, Vec<CompetitorRef>) {
        let mut heats = Vec::new();
        let mut byes = Vec::new();
        for (index, chunk) in order.chunks(self.heat_size).enumerate() {
            if chunk.len() == 1 {
                byes.push(chunk[0].clone());
            } else {
                heats.push(HeatPlan::new(Self::heat_id(index), chunk.to_vec()));
            }
        }
        (heats, byes)
    }

    /// Replay the level from the completed history: the heats this level needs run, whether
    /// they are all complete, and — once they are — who advanced (in heat order, then byes)
    /// and who was eliminated (heat losers, in heat order).
    ///
    /// The pure core both [`next`](Generator::next) and [`ranking`](Generator::ranking) build
    /// on. A level with a single competitor (or empty field) has no heats and completes with
    /// that lone survivor as the sole advancer.
    fn replay(&self, completed: &[CompletedHeat]) -> Replay {
        let by_id: BTreeMap<&str, &HeatResult> = completed
            .iter()
            .map(|c| (c.heat.0.as_str(), &c.result))
            .collect();

        let order = bracket_pairs(&self.field);

        // A single competitor (or empty field) is already the level's survivor — no heats.
        if order.len() <= 1 {
            return Replay {
                heats: Vec::new(),
                complete: true,
                advanced: order,
                eliminated: Vec::new(),
            };
        }

        let (heats, byes) = self.lay_out(&order);

        // Are all of this level's heats complete? If any is missing, the level is still
        // running: its advancers/eliminated are not yet known.
        let results: Option<Vec<&HeatResult>> = heats
            .iter()
            .map(|h| by_id.get(h.heat.0.as_str()).copied())
            .collect();
        let Some(results) = results else {
            return Replay {
                heats,
                complete: false,
                advanced: Vec::new(),
                eliminated: Vec::new(),
            };
        };

        // Level complete: advance each heat's top half (in heat order), then the byes; record
        // who the level eliminated (heat losers, in heat order).
        let mut advanced = Vec::new();
        let mut eliminated = Vec::new();
        for (heat, result) in heats.iter().zip(results) {
            let ranking = result_ranking(result);
            let advance = self.advance_count(heat.lineup.len());
            for (i, entry) in ranking.iter().enumerate() {
                if i < advance {
                    advanced.push(entry.competitor.clone());
                } else {
                    eliminated.push(entry.clone());
                }
            }
        }
        advanced.extend(byes);

        Replay {
            heats,
            complete: true,
            advanced,
            eliminated,
        }
    }
}

/// The outcome of replaying one level's completed history (see [`SingleElim::replay`]).
struct Replay {
    /// The heats this level needs run (empty for a single-competitor level).
    heats: Vec<HeatPlan>,
    /// Whether every heat of the level has come back (so advancers / eliminated are known).
    complete: bool,
    /// The competitors advancing out of the level, in heat order then byes — what the next
    /// level seeds from. Empty until the level is complete.
    advanced: Vec<CompetitorRef>,
    /// The competitors the level eliminated (heat losers, in heat order), each with the
    /// in-heat [`RankEntry`] that knocked them out. Empty until the level is complete.
    eliminated: Vec<RankEntry>,
}

impl Generator for SingleElim {
    fn next(&mut self, completed: &[CompletedHeat]) -> GeneratorStep {
        let replay = self.replay(completed);
        // Emit this level's heats until they are all complete, then the level is done — the
        // next level is a separate round, not another step here.
        if !replay.complete && !replay.heats.is_empty() {
            GeneratorStep::Run(replay.heats)
        } else {
            GeneratorStep::Complete
        }
    }

    fn ranking(&self, completed: &[CompletedHeat]) -> Vec<RankEntry> {
        let replay = self.replay(completed);

        // Before the level's heats complete, the provisional ranking is the bracket order.
        if !replay.complete {
            return seed_ranking(&bracket_pairs(&self.field));
        }

        // Bands, best first: the advancers (in heat order, then byes) lead — they are exactly
        // the next level's field, in seed order — then the eliminated (heat losers, in heat
        // order). Rows carry a (band, in_band_key) sort key; `rank_by` turns equal-band rows
        // into shared positions with the competitor ref as the final tie-break.
        let mut rows: Vec<(CompetitorRef, (u32, u32))> = Vec::new();
        for (i, competitor) in replay.advanced.iter().enumerate() {
            rows.push((competitor.clone(), (0, i as u32)));
        }
        for entry in &replay.eliminated {
            rows.push((entry.competitor.clone(), (1, entry.position)));
        }
        rank_by(rows)
    }

    /// The level's **advancing set** — each heat's top `advance` finishers in heat order, then any
    /// byes. Overrides the ranking-derived default ([`ranking_advancers`]), which is wrong for a
    /// 4-up level: a heat advancing two eliminates two at distinct in-heat positions (3rd, 4th), so
    /// the eliminated do **not** share one worst band and the "better than worst" rule would keep
    /// the 3rd-place losers too. The replay's `advanced` is exactly the winners, however many heats
    /// the level had. Provisional (the ranking fallback) before the level completes.
    fn advancers(&self, completed: &[CompletedHeat]) -> Vec<CompetitorRef> {
        let replay = self.replay(completed);
        if replay.complete {
            replay.advanced
        } else {
            ranking_advancers(&self.ranking(completed))
        }
    }
}

/// A trivial 1, 2, 3, … ranking straight from a seed/bracket order — the provisional ranking
/// the level exposes before its heats are flown.
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

    // --- One level's layout -------------------------------------------------

    #[test]
    fn level_pairs_strong_vs_weak_head_to_head() {
        let mut g = SingleElim::new(field(&["1", "2", "3", "4"]), 2);
        // bracket order 1,4,2,3 → heats (1 v 4), (2 v 3). One level only.
        assert_eq!(
            lineups(&g.next(&[])),
            vec![
                ("se-h0".to_string(), vec!["1".into(), "4".into()]),
                ("se-h1".to_string(), vec!["2".into(), "3".into()]),
            ]
        );
    }

    #[test]
    fn level_odd_field_gives_the_middle_seed_a_bye() {
        // 5 seeds: bracket order 1,5,2,4,3 → heats (1 v 5), (2 v 4), and 3 byes.
        let mut g = SingleElim::new(field(&["1", "2", "3", "4", "5"]), 2);
        assert_eq!(
            lineups(&g.next(&[])),
            vec![
                ("se-h0".to_string(), vec!["1".into(), "5".into()]),
                ("se-h1".to_string(), vec!["2".into(), "4".into()]),
            ]
        );
    }

    #[test]
    fn four_up_heats_chunk_the_level() {
        // 8 seeds, 4-up: bracket order 1,8,2,7,3,6,4,5 → two 4-up heats.
        let mut g = SingleElim::new(field(&["1", "2", "3", "4", "5", "6", "7", "8"]), 4);
        assert_eq!(
            lineups(&g.next(&[])),
            vec![
                (
                    "se-h0".to_string(),
                    vec!["1".into(), "8".into(), "2".into(), "7".into()]
                ),
                (
                    "se-h1".to_string(),
                    vec!["3".into(), "6".into(), "4".into(), "5".into()]
                ),
            ]
        );
    }

    // --- Level completes after its heats ------------------------------------

    #[test]
    fn level_completes_once_its_heats_are_in() {
        // A 4-seed level is ONE round of two heats; once both come back the level is done —
        // there is no second round inside this generator (the next level is a new round).
        let mut g = SingleElim::new(field(&["1", "2", "3", "4"]), 2);
        let completed = vec![
            CompletedHeat::new("se-h0", h2h("1", "4")),
            CompletedHeat::new("se-h1", h2h("2", "3")),
        ];
        assert_eq!(g.next(&completed), GeneratorStep::Complete);
    }

    #[test]
    fn single_competitor_level_completes_immediately() {
        // A one-competitor level (a final's lone survivor seeded into a degenerate level) has
        // no heat and completes, ranking that competitor first.
        let mut g = SingleElim::new(field(&["1"]), 2);
        assert_eq!(g.next(&[]), GeneratorStep::Complete);
        assert_eq!(names(&g.ranking(&[])), vec!["1"]);
    }

    // --- Level ranking = advancers first, then eliminated -------------------

    #[test]
    fn level_ranking_is_advancers_in_heat_order_then_losers() {
        let mut g = SingleElim::new(field(&["1", "2", "3", "4"]), 2);
        let completed = vec![
            // Heat 0: 1 beats 4. Heat 1: 2 beats 3.
            CompletedHeat::new("se-h0", h2h("1", "4")),
            CompletedHeat::new("se-h1", h2h("2", "3")),
        ];
        assert_eq!(g.next(&completed), GeneratorStep::Complete);

        let ranking = g.ranking(&completed);
        // Advancers 1, 2 lead (in heat order, band 0 → positions 1, 2); losers 4, 3 share the
        // eliminated band, ordered by ref as the final tie-break (3 before 4).
        assert_eq!(&names(&ranking)[..2], &["1".to_string(), "2".to_string()]);
        assert_eq!(ranking[0].position, 1);
        assert_eq!(ranking[1].position, 2);
        assert_eq!(ranking[2].position, 3);
        assert_eq!(ranking[3].position, 3); // 3 and 4 share the eliminated band
    }

    #[test]
    fn four_up_level_advances_top_two_of_each_heat() {
        let mut g = SingleElim::new(field(&["1", "2", "3", "4", "5", "6", "7", "8"]), 4);
        let _ = g.next(&[]);
        // Heat h0 (1,8,2,7): order 2,1,8,7 → 2,1 advance. Heat h1 (3,6,4,5): order 4,5,3,6 →
        // 4,5 advance. The level's advancers (the next level's field) are 2,1,4,5 in heat order.
        let completed = vec![
            CompletedHeat::new(
                "se-h0",
                result(&[("2", 1, 6), ("1", 2, 5), ("8", 3, 4), ("7", 4, 3)]),
            ),
            CompletedHeat::new(
                "se-h1",
                result(&[("4", 1, 6), ("5", 2, 5), ("3", 3, 4), ("6", 4, 3)]),
            ),
        ];
        assert_eq!(g.next(&completed), GeneratorStep::Complete);
        let ranking = g.ranking(&completed);
        assert_eq!(&names(&ranking)[..4], &["2", "1", "4", "5"]);
        assert_eq!(ranking[0].position, 1);
        assert_eq!(ranking[3].position, 4);
    }

    #[test]
    fn four_up_level_advancers_carry_exactly_the_winners_not_the_better_losers() {
        // The FromHeatWinners carry (`Generator::advancers`) of a 4-up level must be **exactly** the
        // top two of each heat — the four advancers, in heat order — and NOT the 3rd-place losers.
        // The eliminated rank at distinct overall positions (3rd-place losers 5th, 4th-place losers
        // 7th), so they do NOT share one worst band; a ranking `position < worst` filter would keep
        // the two 5th-placed losers too (6 carried, not 4). The override returns the replay's real
        // advancing set, fixing that. Two heats of four advancing two each → exactly 4 carry.
        let mut g = SingleElim::new(field(&["1", "2", "3", "4", "5", "6", "7", "8"]), 4);
        let _ = g.next(&[]);
        let completed = vec![
            CompletedHeat::new(
                "se-h0",
                result(&[("2", 1, 6), ("1", 2, 5), ("8", 3, 4), ("7", 4, 3)]),
            ),
            CompletedHeat::new(
                "se-h1",
                result(&[("4", 1, 6), ("5", 2, 5), ("3", 3, 4), ("6", 4, 3)]),
            ),
        ];
        let advancers: Vec<String> = g
            .advancers(&completed)
            .iter()
            .map(|c| c.0.clone())
            .collect();
        assert_eq!(advancers, vec!["2", "1", "4", "5"]);
        // The default ranking-derived heuristic would have wrongly carried the two 5th-placed losers
        // (8 and 3) too — guard that the fix does not regress to that.
        assert!(!advancers.contains(&"8".to_string()));
        assert!(!advancers.contains(&"3".to_string()));
    }

    #[test]
    fn head_to_head_advancers_match_the_ranking_default() {
        // For a head-to-head level the eliminated DO share one worst band, so the override and the
        // ranking-derived default agree: each heat's single winner carries, in heat order.
        let mut g = SingleElim::new(field(&["1", "2", "3", "4"]), 2);
        let _ = g.next(&[]);
        let completed = vec![
            CompletedHeat::new("se-h0", h2h("1", "4")),
            CompletedHeat::new("se-h1", h2h("2", "3")),
        ];
        let advancers: Vec<String> = g
            .advancers(&completed)
            .iter()
            .map(|c| c.0.clone())
            .collect();
        assert_eq!(advancers, vec!["1", "2"]);
    }

    #[test]
    fn four_up_level_with_advance_one_advances_only_the_winner() {
        // A 4-up heat with `advance=1`: only the heat winner progresses; the other three are
        // eliminated. With 8 seeds + 4-up, two heats each yield one advancer → 2 advance overall.
        let mut g =
            SingleElim::new(field(&["1", "2", "3", "4", "5", "6", "7", "8"]), 4).with_advance(1);
        let _ = g.next(&[]);
        let completed = vec![
            CompletedHeat::new(
                "se-h0",
                result(&[("2", 1, 6), ("1", 2, 5), ("8", 3, 4), ("7", 4, 3)]),
            ),
            CompletedHeat::new(
                "se-h1",
                result(&[("4", 1, 6), ("5", 2, 5), ("3", 3, 4), ("6", 4, 3)]),
            ),
        ];
        assert_eq!(g.next(&completed), GeneratorStep::Complete);
        let ranking = g.ranking(&completed);
        // Only the two heat winners (2, then 4 in heat order) advance — exactly `advance` per heat.
        assert_eq!(&names(&ranking)[..2], &["2", "4"]);
        assert_eq!(ranking[0].position, 1);
        assert_eq!(ranking[1].position, 2);
        // The remaining six are eliminated (band 1) — three per heat.
        assert!(ranking[2].position >= 3);
    }

    #[test]
    fn advance_defaults_to_half_the_heat_size_when_no_param() {
        // No `advance` param → default `heat_size / 2`: a 4-up heat advances its top two.
        let cfg = FormatConfig::new(field(&["1", "2", "3", "4", "5", "6", "7", "8"]))
            .with_param("heat_size", "4");
        let mut g = SingleElim::from_config(&cfg);
        let _ = g.next(&[]);
        let completed = vec![
            CompletedHeat::new(
                "se-h0",
                result(&[("2", 1, 6), ("1", 2, 5), ("8", 3, 4), ("7", 4, 3)]),
            ),
            CompletedHeat::new(
                "se-h1",
                result(&[("4", 1, 6), ("5", 2, 5), ("3", 3, 4), ("6", 4, 3)]),
            ),
        ];
        assert_eq!(g.next(&completed), GeneratorStep::Complete);
        // Default advance (heat_size/2 = 2) → top two of each heat: 2,1,4,5 in heat order.
        assert_eq!(&names(&g.ranking(&completed))[..4], &["2", "1", "4", "5"]);
    }

    #[test]
    fn registry_reads_the_advance_param() {
        let mut registry = FormatRegistry::new();
        SingleElim::register(&mut registry);
        // 4-up heats but advance=1: each heat carries exactly one to the next level.
        let cfg = FormatConfig::new(field(&["1", "2", "3", "4", "5", "6", "7", "8"]))
            .with_param("heat_size", "4")
            .with_param("advance", "1");
        let g = registry.build(SingleElim::NAME, &cfg).unwrap();
        let completed = vec![
            CompletedHeat::new(
                "se-h0",
                result(&[("2", 1, 6), ("1", 2, 5), ("8", 3, 4), ("7", 4, 3)]),
            ),
            CompletedHeat::new(
                "se-h1",
                result(&[("4", 1, 6), ("5", 2, 5), ("3", 3, 4), ("6", 4, 3)]),
            ),
        ];
        // The FromHeatWinners carry = the advancers band: exactly one per heat (2, then 4).
        let advancers: Vec<String> = g
            .ranking(&completed)
            .iter()
            .take(2)
            .map(|e| e.competitor.0.clone())
            .collect();
        assert_eq!(advancers, vec!["2", "4"]);
    }

    #[test]
    fn bye_competitor_advances_without_a_heat() {
        // 3 seeds: the level is (1 v 3) plus a bye for seed 2.
        let mut g = SingleElim::new(field(&["1", "2", "3"]), 2);
        assert_eq!(
            lineups(&g.next(&[])),
            vec![("se-h0".to_string(), vec!["1".into(), "3".into()])]
        );
        // Seed 1 wins; the bye (seed 2) advances. Advancers in heat order then byes: 1, 2.
        let completed = vec![CompletedHeat::new("se-h0", h2h("1", "3"))];
        assert_eq!(g.next(&completed), GeneratorStep::Complete);
        let ranking = g.ranking(&completed);
        assert_eq!(&names(&ranking)[..2], &["1".to_string(), "2".to_string()]);
    }

    // --- Provisional ranking ------------------------------------------------

    #[test]
    fn provisional_ranking_is_the_bracket_order() {
        let g = SingleElim::new(field(&["1", "2", "3", "4"]), 2);
        // Before any heat: bracket order is the provisional ranking.
        assert_eq!(names(&g.ranking(&[])), vec!["1", "4", "2", "3"]);
    }

    // --- Determinism --------------------------------------------------------

    #[test]
    fn next_is_deterministic_for_the_same_history() {
        let mut g1 = SingleElim::new(field(&["1", "2", "3", "4"]), 2);
        let mut g2 = SingleElim::new(field(&["1", "2", "3", "4"]), 2);
        assert_eq!(g1.next(&[]), g2.next(&[]));
    }

    #[test]
    fn seeding_draw_reorders_the_level_deterministically() {
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
                ("se-h0".to_string(), vec!["3".into(), "2".into()]),
                ("se-h1".to_string(), vec!["1".into(), "4".into()]),
            ]
        );
    }

    // --- A single-elim CHAIN: level → level → final (the level-per-round model) ----

    #[test]
    fn a_chain_of_levels_progresses_to_a_final() {
        // 8 seeds. Level 1 (quarters): one SingleElim over the quali top-8. Its advancers seed
        // level 2 (semis); their advancers seed level 3 (the final). Each level is its own
        // generator, seeded from the prior level's advancers (heat winners) — exactly what the
        // server's FromHeatWinners carry does round-to-round.

        // Level 1 — quarters.
        let mut quarters = SingleElim::new(field(&["1", "2", "3", "4", "5", "6", "7", "8"]), 2);
        // bracket 1,8,2,7,3,6,4,5 → heats (1v8),(2v7),(3v6),(4v5).
        assert_eq!(
            lineups(&quarters.next(&[])),
            vec![
                ("se-h0".to_string(), vec!["1".into(), "8".into()]),
                ("se-h1".to_string(), vec!["2".into(), "7".into()]),
                ("se-h2".to_string(), vec!["3".into(), "6".into()]),
                ("se-h3".to_string(), vec!["4".into(), "5".into()]),
            ]
        );
        let q_completed = vec![
            CompletedHeat::new("se-h0", h2h("1", "8")),
            CompletedHeat::new("se-h1", h2h("2", "7")),
            CompletedHeat::new("se-h2", h2h("3", "6")),
            CompletedHeat::new("se-h3", h2h("4", "5")),
        ];
        assert_eq!(quarters.next(&q_completed), GeneratorStep::Complete);
        // The level's advancers (winners in heat order) seed the next level: 1,2,3,4.
        let semis_seeds: Vec<CompetitorRef> = quarters
            .ranking(&q_completed)
            .iter()
            .take(4)
            .map(|e| e.competitor.clone())
            .collect();
        assert_eq!(semis_seeds, field(&["1", "2", "3", "4"]));

        // Level 2 — semis, seeded from the quarters' winners.
        let mut semis = SingleElim::new(semis_seeds, 2);
        // bracket 1,4,2,3 → heats (1v4),(2v3).
        assert_eq!(
            lineups(&semis.next(&[])),
            vec![
                ("se-h0".to_string(), vec!["1".into(), "4".into()]),
                ("se-h1".to_string(), vec!["2".into(), "3".into()]),
            ]
        );
        let s_completed = vec![
            CompletedHeat::new("se-h0", h2h("1", "4")),
            CompletedHeat::new("se-h1", h2h("2", "3")),
        ];
        assert_eq!(semis.next(&s_completed), GeneratorStep::Complete);
        let final_seeds: Vec<CompetitorRef> = semis
            .ranking(&s_completed)
            .iter()
            .take(2)
            .map(|e| e.competitor.clone())
            .collect();
        assert_eq!(final_seeds, field(&["1", "2"]));

        // Level 3 — the final, seeded from the semis' winners.
        let mut decider = SingleElim::new(final_seeds, 2);
        assert_eq!(
            lineups(&decider.next(&[])),
            vec![("se-h0".to_string(), vec!["1".into(), "2".into()])]
        );
        let f_completed = vec![CompletedHeat::new("se-h0", h2h("2", "1"))];
        assert_eq!(decider.next(&f_completed), GeneratorStep::Complete);
        // The final's ranking: winner 2, runner-up 1.
        assert_eq!(names(&decider.ranking(&f_completed)), vec!["2", "1"]);
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
                ("se-h0".to_string(), vec!["1".into(), "4".into()]),
                ("se-h1".to_string(), vec!["2".into(), "3".into()]),
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
}
