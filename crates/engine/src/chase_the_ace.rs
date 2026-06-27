//! Chase-the-Ace final-format generator — a multi-race final where the seeded field races
//! **repeatedly** and the **first pilot to `wins_to_win` race-wins** (default 2, *not* necessarily
//! consecutive) is champion. This is the MultiGP "Chase the Ace" final: the final group races over
//! and over until someone has won twice.
//!
//! # Why it terminates — no cap, fully deterministic (decisions 2026-06-27)
//!
//! Each race produces exactly **one** winner (the scored position-1). To keep every finalist below
//! `N = wins_to_win` wins, each of the `F` finalists may hold at most `N - 1` wins — so at most
//! `F · (N - 1)` races can pass without a champion, and race `F · (N - 1) + 1` *forces* someone to
//! `N`. The series therefore ends from the completed history alone — no artificial race cap, no RNG,
//! no clock (the [`Generator`] determinism contract). For the standard `N = 2`, a 2-up final is
//! decided in at most 3 races, an 8-up group in at most 9.
//!
//! # Composition with a per-race win condition
//!
//! Each individual race is an ordinary heat scored by the round's win condition (Timed — most laps /
//! First-to-N-laps); Chase the Ace only adds the "first to `N` race-**wins**" meta on top. So
//! `wins_to_win = 1` is exactly a single-race final. The whole field flies **every** race, in seed
//! order.
//!
//! # Where it sits
//!
//! It is a **final-level** format: the bracket's last level is a `chase_the_ace` round seeded
//! `FromHeatWinners` of the prior level (or `FromRanking`). `next` single-steps — one race per call
//! — so the RD runs a race, finalizes it, and fills the next, until the generator completes.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use gridfpv_events::CompetitorRef;

use crate::format::{
    CompletedHeat, FormatConfig, FormatRegistry, Generator, GeneratorStep, HeatPlan, RankEntry,
    rank_by, result_ranking,
};
use crate::scoring::HeatResult;

/// A Chase-the-Ace final over a seeded field: race the field repeatedly until the first pilot to
/// [`wins_to_win`](Self::wins_to_win) race-wins is champion. See the module docs.
pub struct ChaseTheAce {
    /// The finalists in seed order (the recorded [`SeedingOutcome`](crate::format::SeedingOutcome)
    /// already applied). The whole field flies every race.
    field: Vec<CompetitorRef>,
    /// Race-wins needed to take the final (clamped to ≥ 1; `1` ⇒ a single-race final).
    wins_to_win: usize,
}

impl ChaseTheAce {
    /// The format name this registers under.
    pub const NAME: &'static str = "chase_the_ace";

    /// Build over a `field` in seed order with the given wins target (clamped to a minimum of 1).
    pub fn new(field: Vec<CompetitorRef>, wins_to_win: usize) -> Self {
        Self {
            field,
            wins_to_win: wins_to_win.max(1),
        }
    }

    /// The registry constructor: applies the recorded seeding draw to `field` and reads the optional
    /// `wins_to_win` param (default 2 = the MultiGP standard).
    pub fn from_config(config: &FormatConfig) -> Box<dyn Generator> {
        let field = config.seeding.apply(&config.field);
        let wins_to_win = config.param_usize("wins_to_win", 2);
        Box::new(Self::new(field, wins_to_win))
    }

    /// Register this format under [`NAME`](Self::NAME).
    pub fn register(registry: &mut FormatRegistry) {
        registry.register(Self::NAME, Self::from_config);
    }

    /// The heat id for race `index` (0-based) of this final.
    fn race_id(index: usize) -> String {
        format!("cta-r{index}")
    }

    /// Replay the completed races (our `cta-r{i}` heats, in race order) into the per-finalist win
    /// tally + aggregate finishing position, the champion (first to reach `wins_to_win`), and how
    /// many of our races have come back. The pure core both [`next`](Generator::next) and
    /// [`ranking`](Generator::ranking) build on.
    fn tally(&self, completed: &[CompletedHeat]) -> Tally {
        let by_id: BTreeMap<&str, &HeatResult> = completed
            .iter()
            .map(|c| (c.heat.0.as_str(), &c.result))
            .collect();

        let mut wins: BTreeMap<CompetitorRef, u32> =
            self.field.iter().map(|c| (c.clone(), 0)).collect();
        let mut position_sum: BTreeMap<CompetitorRef, u32> =
            self.field.iter().map(|c| (c.clone(), 0)).collect();
        let mut champion: Option<CompetitorRef> = None;
        let mut races_in = 0usize;

        // Races run strictly in order (the RD runs one, fills the next), so walk `cta-r0, r1, …`
        // until the first one that hasn't come back.
        for index in 0.. {
            let id = Self::race_id(index);
            let Some(result) = by_id.get(id.as_str()).copied() else {
                break;
            };
            races_in += 1;
            let ranking = result_ranking(result);
            for entry in &ranking {
                *position_sum.entry(entry.competitor.clone()).or_insert(0) += entry.position;
            }
            // The race winner is its scored position-1 (the first ranked entry). Tally the win and,
            // the moment a finalist first reaches the target, crown them — the series stops there.
            if let Some(winner) = ranking.first() {
                let w = wins.entry(winner.competitor.clone()).or_insert(0);
                *w += 1;
                if champion.is_none() && (*w as usize) >= self.wins_to_win {
                    champion = Some(winner.competitor.clone());
                }
            }
        }

        Tally {
            wins,
            position_sum,
            champion,
            races_in,
        }
    }
}

/// The outcome of replaying a Chase-the-Ace final's completed races (see [`ChaseTheAce::tally`]).
struct Tally {
    /// Race-wins per finalist.
    wins: BTreeMap<CompetitorRef, u32>,
    /// Sum of finishing positions per finalist across the races run (lower = stronger) — the
    /// deterministic tie-break for ordering the non-champions.
    position_sum: BTreeMap<CompetitorRef, u32>,
    /// The champion (first finalist to reach `wins_to_win`), once one exists.
    champion: Option<CompetitorRef>,
    /// How many of this final's races have come back.
    races_in: usize,
}

impl Generator for ChaseTheAce {
    fn next(&mut self, completed: &[CompletedHeat]) -> GeneratorStep {
        let tally = self.tally(completed);
        // A lone (or empty) finalist field has nothing to race — it is already settled. Otherwise
        // run races until someone reaches the target, then complete.
        if self.field.len() <= 1 || tally.champion.is_some() {
            GeneratorStep::Complete
        } else {
            // The whole field flies the next race, in seed order.
            GeneratorStep::Run(vec![HeatPlan::new(
                Self::race_id(tally.races_in),
                self.field.clone(),
            )])
        }
    }

    fn ranking(&self, completed: &[CompletedHeat]) -> Vec<RankEntry> {
        let tally = self.tally(completed);

        // Before any race, the provisional ranking is the seed order.
        if tally.races_in == 0 {
            return seed_ranking(&self.field);
        }

        // Champion first (most wins — only the champion ever reaches the target, so the most-wins
        // band is a single row once decided), then by wins descending, ties broken by the lower
        // aggregate finishing position, then the competitor ref (via `rank_by`). Keyed so a lower
        // key sorts first: `(target_band, position_sum)` where `target_band = max_wins - wins`.
        let max_wins = tally.wins.values().copied().max().unwrap_or(0);
        let rows: Vec<(CompetitorRef, (u32, u32))> = self
            .field
            .iter()
            .map(|competitor| {
                let wins = tally.wins.get(competitor).copied().unwrap_or(0);
                let position_sum = tally.position_sum.get(competitor).copied().unwrap_or(0);
                (competitor.clone(), (max_wins - wins, position_sum))
            })
            .collect();
        rank_by(rows)
    }
}

/// A trivial 1, 2, 3, … ranking straight from the seed order — the provisional ranking before any
/// race is flown.
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
    /// A head-to-head race result where `winner` beats `loser`.
    fn h2h(winner: &str, loser: &str) -> HeatResult {
        result(&[(winner, 1, 5), (loser, 2, 3)])
    }
    fn names(entries: &[RankEntry]) -> Vec<String> {
        entries.iter().map(|e| e.competitor.0.clone()).collect()
    }
    /// The single race id a `Run` step emits (panics if `Complete` or not exactly one heat).
    fn next_race(step: &GeneratorStep) -> (String, Vec<String>) {
        match step {
            GeneratorStep::Run(heats) => {
                assert_eq!(heats.len(), 1, "Chase the Ace emits one race at a time");
                (
                    heats[0].heat.0.clone(),
                    heats[0].lineup.iter().map(|c| c.0.clone()).collect(),
                )
            }
            GeneratorStep::Complete => panic!("expected Run, got Complete"),
        }
    }

    #[test]
    fn runs_one_race_at_a_time_over_the_whole_field() {
        let mut g = ChaseTheAce::new(field(&["A", "B"]), 2);
        // First race, before any history.
        assert_eq!(
            next_race(&g.next(&[])),
            ("cta-r0".to_string(), vec!["A".into(), "B".into()])
        );
        // After race 0 (A wins, 1-0), it runs race 1 — still no champion.
        let after0 = vec![CompletedHeat::new("cta-r0", h2h("A", "B"))];
        assert_eq!(
            next_race(&g.next(&after0)),
            ("cta-r1".to_string(), vec!["A".into(), "B".into()])
        );
    }

    #[test]
    fn first_to_two_wins_takes_the_final() {
        let mut g = ChaseTheAce::new(field(&["A", "B"]), 2);
        // A wins r0, B wins r1 (1-1), A wins r2 (2-1) → A champion, series complete.
        let history = vec![
            CompletedHeat::new("cta-r0", h2h("A", "B")),
            CompletedHeat::new("cta-r1", h2h("B", "A")),
            CompletedHeat::new("cta-r2", h2h("A", "B")),
        ];
        assert_eq!(g.next(&history), GeneratorStep::Complete);
        let ranking = g.ranking(&history);
        assert_eq!(names(&ranking), vec!["A", "B"]); // champion A, then B
        assert_eq!(ranking[0].position, 1);
        assert_eq!(ranking[1].position, 2);
    }

    #[test]
    fn wins_need_not_be_consecutive() {
        // A, B, A — A's two wins straddle B's, and A is still champion at r2.
        let mut g = ChaseTheAce::new(field(&["A", "B"]), 2);
        let history = vec![
            CompletedHeat::new("cta-r0", h2h("A", "B")),
            CompletedHeat::new("cta-r1", h2h("B", "A")),
            CompletedHeat::new("cta-r2", h2h("A", "B")),
        ];
        assert_eq!(g.next(&history), GeneratorStep::Complete);
        assert_eq!(names(&g.ranking(&history))[0], "A");
    }

    #[test]
    fn keeps_racing_while_no_one_has_two() {
        let mut g = ChaseTheAce::new(field(&["A", "B"]), 2);
        // 1-1 after two races → not complete, runs race 2.
        let history = vec![
            CompletedHeat::new("cta-r0", h2h("A", "B")),
            CompletedHeat::new("cta-r1", h2h("B", "A")),
        ];
        assert_eq!(next_race(&g.next(&history)).0, "cta-r2");
    }

    #[test]
    fn group_final_terminates_by_pigeonhole() {
        // 4-up group, first to 2. Four different winners (all at 1 win) → still racing; the 5th race
        // forces a second win for someone (here A) → champion. Bounded at F·(N-1)+1 = 5 races.
        let mut g = ChaseTheAce::new(field(&["A", "B", "C", "D"]), 2);
        let four = vec![
            CompletedHeat::new(
                "cta-r0",
                result(&[("A", 1, 6), ("B", 2, 5), ("C", 3, 4), ("D", 4, 3)]),
            ),
            CompletedHeat::new(
                "cta-r1",
                result(&[("B", 1, 6), ("A", 2, 5), ("C", 3, 4), ("D", 4, 3)]),
            ),
            CompletedHeat::new(
                "cta-r2",
                result(&[("C", 1, 6), ("A", 2, 5), ("B", 3, 4), ("D", 4, 3)]),
            ),
            CompletedHeat::new(
                "cta-r3",
                result(&[("D", 1, 6), ("A", 2, 5), ("B", 3, 4), ("C", 4, 3)]),
            ),
        ];
        // All on 1 win → not done, a 5th race runs.
        assert_eq!(next_race(&g.next(&four)).0, "cta-r4");
        // A wins r4 → A reaches 2 → champion.
        let mut five = four.clone();
        five.push(CompletedHeat::new(
            "cta-r4",
            result(&[("A", 1, 6), ("B", 2, 5), ("C", 3, 4), ("D", 4, 3)]),
        ));
        assert_eq!(g.next(&five), GeneratorStep::Complete);
        assert_eq!(names(&g.ranking(&five))[0], "A");
    }

    #[test]
    fn ranking_orders_champion_then_by_wins_then_position() {
        // A champion (2 wins); B has 1 win; C, D have 0 but C finished ahead of D overall.
        let mut g = ChaseTheAce::new(field(&["A", "B", "C", "D"]), 2);
        let history = vec![
            CompletedHeat::new(
                "cta-r0",
                result(&[("A", 1, 6), ("C", 2, 5), ("D", 3, 4), ("B", 4, 3)]),
            ),
            CompletedHeat::new(
                "cta-r1",
                result(&[("B", 1, 6), ("A", 2, 5), ("C", 3, 4), ("D", 4, 3)]),
            ),
            CompletedHeat::new(
                "cta-r2",
                result(&[("A", 1, 6), ("C", 2, 5), ("D", 3, 4), ("B", 4, 3)]),
            ),
        ];
        assert_eq!(g.next(&history), GeneratorStep::Complete);
        // A (2 wins) first, B (1 win) second, then C ahead of D (lower position sum).
        assert_eq!(names(&g.ranking(&history)), vec!["A", "B", "C", "D"]);
    }

    #[test]
    fn wins_to_win_one_is_a_single_race_final() {
        let mut g = ChaseTheAce::new(field(&["A", "B"]), 1);
        // One race decides it.
        let history = vec![CompletedHeat::new("cta-r0", h2h("B", "A"))];
        assert_eq!(g.next(&history), GeneratorStep::Complete);
        assert_eq!(names(&g.ranking(&history))[0], "B");
    }

    #[test]
    fn single_finalist_completes_immediately() {
        let mut g = ChaseTheAce::new(field(&["A"]), 2);
        assert_eq!(g.next(&[]), GeneratorStep::Complete);
        assert_eq!(names(&g.ranking(&[])), vec!["A"]);
    }

    #[test]
    fn provisional_ranking_is_the_seed_order() {
        let g = ChaseTheAce::new(field(&["A", "B", "C"]), 2);
        assert_eq!(names(&g.ranking(&[])), vec!["A", "B", "C"]);
    }

    #[test]
    fn next_is_deterministic_for_the_same_history() {
        let mut g1 = ChaseTheAce::new(field(&["A", "B"]), 2);
        let mut g2 = ChaseTheAce::new(field(&["A", "B"]), 2);
        let history = vec![CompletedHeat::new("cta-r0", h2h("A", "B"))];
        assert_eq!(g1.next(&history), g2.next(&history));
    }

    #[test]
    fn registry_builds_chase_the_ace_and_reads_wins_to_win() {
        let mut registry = FormatRegistry::new();
        ChaseTheAce::register(&mut registry);
        assert_eq!(registry.names(), vec!["chase_the_ace"]);

        // Default wins target is 2: 1-1 keeps racing.
        let cfg = FormatConfig::new(field(&["A", "B"]));
        let mut g = registry.build(ChaseTheAce::NAME, &cfg).unwrap();
        let one_one = vec![
            CompletedHeat::new("cta-r0", h2h("A", "B")),
            CompletedHeat::new("cta-r1", h2h("B", "A")),
        ];
        assert_eq!(next_race(&g.next(&one_one)).0, "cta-r2");

        // wins_to_win = 3: 2-1 still keeps racing.
        let cfg3 = FormatConfig::new(field(&["A", "B"])).with_param("wins_to_win", "3");
        let mut g3 = registry.build(ChaseTheAce::NAME, &cfg3).unwrap();
        let two_one = vec![
            CompletedHeat::new("cta-r0", h2h("A", "B")),
            CompletedHeat::new("cta-r1", h2h("A", "B")),
            CompletedHeat::new("cta-r2", h2h("B", "A")),
        ];
        assert_eq!(next_race(&g3.next(&two_one)).0, "cta-r3");
    }
}
