//! Double-elimination bracket generator (#13) — a [`Generator`] that seeds a field
//! into a **double**-elimination bracket: a loss drops a competitor from the winners
//! bracket into the losers bracket, and a second loss eliminates them. The last two
//! standing (the undefeated winners champion and the once-defeated losers champion)
//! meet in the grand final.
//!
//! # The format (race-engine.html §3, §5)
//!
//! Double elimination is the natural extension of [`crate::single_elim`]: the same
//! fixed-but-state-driven shape (RE §3) — the whole bracket is determined by the
//! seeded field, yet every round is derived from the results so far. `next` reads the
//! completed heats, takes each head-to-head heat's **winner** (the top of the
//! result), and lays out the next round across **both** brackets. It reads no clock or
//! RNG: any seeding draw is resolved once and injected as a [`SeedingOutcome`] at
//! construction (RE §6), so the bracket replays identically.
//!
//! This generator runs **head-to-head (2-pilot) heats** for correctness: every match
//! is a clean win/loss, which is what makes "drop the loser, eliminate on a second
//! loss" unambiguous. Larger-heat variants (advance the top half, drop the bottom
//! half into losers) are a future extension and are intentionally out of scope here.
//!
//! # Bracket bookkeeping
//!
//! 1. **Seed the field.** The config carries the field in seed order (best first); a
//!    recorded [`SeedingOutcome`] is applied first (identity if none). The first
//!    winners round pairs strong-vs-weak via [`bracket_pairs`] (1 v N, 2 v N-1, …),
//!    the standard seeding that keeps top seeds apart.
//! 2. **Winners bracket (WB).** A plain single-elimination knockout. The **winner** of
//!    each WB heat advances within the WB; the **loser** drops into the losers bracket
//!    (LB). The WB runs until one undefeated competitor remains — the WB champion.
//! 3. **Losers bracket (LB).** Fed by the WB's droppers. It alternates two kinds of
//!    rounds, the standard double-elim layout:
//!    - a **minor** round pairs the current LB survivors against each other, then
//!    - a **major** round pairs those survivors against the newest batch of WB
//!      droppers.
//!
//!    The loser of *any* LB heat is eliminated (their second loss). The LB runs until
//!    one once-defeated competitor remains — the LB champion.
//! 4. **Grand final.** WB champion (0 losses) vs LB champion (1 loss).
//!
//! # Grand-final bracket reset (documented choice)
//!
//! A purist double-elimination grand final is *true*: because the LB champion has one
//! loss and the WB champion has none, if the LB champion wins the first grand-final
//! heat both competitors then have one loss, so a **second** decider ("the reset") is
//! played. If the WB champion wins the first heat, the LB champion is eliminated on
//! their second loss and no reset is needed.
//!
//! This generator implements the **bracket reset**, gated by the `bracket_reset`
//! config param (default **on**, `"1"`; set `"0"` for a single-heat "winner-takes-it"
//! grand final). The choice is fully deterministic: the reset heat is emitted **iff**
//! the LB champion won the first grand-final heat *and* reset is enabled — a pure
//! function of the completed history, never a clock or a flag mutated mid-run.
//!
//! # Byes (odd / short fields)
//!
//! When a round's competitor list is odd, the **trailing** competitor (the middle seed
//! in the first WB round, by [`bracket_pairs`]; the last survivor otherwise) takes a
//! **bye** and advances for free, deterministically. The same rule applies in the LB,
//! so an odd field still converges.
//!
//! # Ranking
//!
//! [`Generator::ranking`] is the bracket placement: the **champion** first, the
//! **grand-final loser** second, then everyone else ordered by **how far they got** —
//! a competitor eliminated in a later round (WB or LB) outranks one knocked out
//! earlier. Within an elimination round, finishers keep the in-heat order, with the
//! competitor ref as the final deterministic tie-break. Provisional before the bracket
//! completes, final after.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use gridfpv_events::CompetitorRef;

use crate::format::{
    CompletedHeat, FormatConfig, FormatRegistry, Generator, GeneratorStep, HeatPlan, RankEntry,
    bracket_pairs, rank_by, result_ranking,
};
use crate::scoring::HeatResult;

/// A double-elimination bracket over a seeded field (head-to-head heats).
///
/// Constructed with the seeded field (draw order already applied) and whether the
/// grand final allows a [bracket reset](self#grand-final-bracket-reset-documented-choice);
/// `next` drives the rounds and `ranking` reports the bracket placement. See the module
/// docs for the seeding / bye / WB-LB / grand-final rules.
pub struct DoubleElim {
    /// The field in seed/draw order (the recorded [`SeedingOutcome`] already applied).
    field: Vec<CompetitorRef>,
    /// Whether a losers-champion win in grand-final heat 1 forces a decider (reset).
    bracket_reset: bool,
}

impl DoubleElim {
    /// The format name this registers under.
    pub const NAME: &'static str = "double_elim";

    /// Build over a `field` in seed order, with the grand-final bracket reset enabled
    /// or disabled.
    pub fn new(field: Vec<CompetitorRef>, bracket_reset: bool) -> Self {
        Self {
            field,
            bracket_reset,
        }
    }

    /// The registry constructor: applies the recorded `seeding` draw to `field` and
    /// reads the optional `bracket_reset` param (default on — any value other than
    /// `"0"`/`"false"` keeps it enabled).
    pub fn from_config(config: &FormatConfig) -> Box<dyn Generator> {
        let field = config.seeding.apply(&config.field);
        let bracket_reset = !matches!(
            config.params.get("bracket_reset").map(String::as_str),
            Some("0") | Some("false") | Some("off") | Some("no")
        );
        Box::new(Self::new(field, bracket_reset))
    }

    /// Register this format under [`NAME`](Self::NAME).
    pub fn register(registry: &mut FormatRegistry) {
        registry.register(Self::NAME, Self::from_config);
    }

    // --- Heat-id scheme -----------------------------------------------------
    //
    // Winners: `de-w{round}-h{index}`  (round, heat index both 0-based-ish: round
    // 1-based, heat 0-based, mirroring single_elim's `se-r{round}-h{index}`).
    // Losers:  `de-l{round}-h{index}`.
    // Grand final: `de-gf` (first heat) and `de-gf-reset` (the decider).

    fn winners_id(round: usize, index: usize) -> String {
        format!("de-w{round}-h{index}")
    }

    fn losers_id(round: usize, index: usize) -> String {
        format!("de-l{round}-h{index}")
    }

    const GRAND_FINAL: &'static str = "de-gf";
    const GRAND_FINAL_RESET: &'static str = "de-gf-reset";

    /// Pair `order` into head-to-head heats with the given id builder, returning the
    /// heat plans **and** the bye (a single trailing competitor advances for free).
    ///
    /// Deterministic: chunks are taken left-to-right; a trailing single is the bye.
    fn pair_round(
        round: usize,
        order: &[CompetitorRef],
        id: impl Fn(usize, usize) -> String,
    ) -> (Vec<HeatPlan>, Option<CompetitorRef>) {
        let mut heats = Vec::new();
        let mut bye = None;
        let mut index = 0usize;
        let mut chunks = order.chunks(2);
        for chunk in &mut chunks {
            if chunk.len() == 2 {
                heats.push(HeatPlan::new(id(round, index), chunk.to_vec()));
                index += 1;
            } else {
                bye = Some(chunk[0].clone());
            }
        }
        (heats, bye)
    }

    /// Replay the whole bracket from the completed history. This is the pure core both
    /// [`next`](Generator::next) and [`ranking`](Generator::ranking) build on: it walks
    /// the winners bracket and losers bracket round by round in lock-step, matching
    /// each round's emitted heats against `completed` by heat id, advancing winners and
    /// dropping losers, and only progressing past a round once every one of its heats
    /// has come back.
    fn replay(&self, completed: &[CompletedHeat]) -> Replay {
        let by_id: BTreeMap<&str, &HeatResult> = completed
            .iter()
            .map(|c| (c.heat.0.as_str(), &c.result))
            .collect();

        // The winner / loser of a completed heat, looked up by id. `None` if the heat
        // has not come back yet.
        let outcome = |heat_id: &str| -> Option<(CompetitorRef, CompetitorRef)> {
            let result = by_id.get(heat_id)?;
            let ranking = result_ranking(result);
            // Head-to-head: top is the winner, next is the loser.
            let winner = ranking.first()?.competitor.clone();
            let loser = ranking.get(1)?.competitor.clone();
            Some((winner, loser))
        };

        let mut pending: Vec<HeatPlan> = Vec::new();
        // Per round (earliest first) the competitors that round eliminated, in heat
        // order — the second-loss losers, used to band the ranking.
        let mut eliminated_by_round: Vec<Vec<CompetitorRef>> = Vec::new();

        // --- Winners bracket: advance like a single-elim, collecting droppers. ---
        //
        // `wb_drops[r]` is the batch of competitors who lost in winners round `r`
        // (1-based), in heat order — the feed into the losers bracket.
        let mut wb_round = 1usize;
        let mut wb_order = bracket_pairs(&self.field);
        let mut wb_drops: Vec<Vec<CompetitorRef>> = vec![Vec::new()]; // index 0 unused
        let wb_champion: Option<CompetitorRef> = loop {
            if wb_order.len() <= 1 {
                break wb_order.first().cloned();
            }
            let (heats, bye) = Self::pair_round(wb_round, &wb_order, Self::winners_id);
            let mut next_order = Vec::new();
            let mut drops = Vec::new();
            let mut complete = true;
            for heat in &heats {
                match outcome(&heat.heat.0) {
                    Some((winner, loser)) => {
                        next_order.push(winner);
                        drops.push(loser);
                    }
                    None => {
                        complete = false;
                        pending.push(heat.clone());
                    }
                }
            }
            if !complete {
                // This WB round is still running; stop advancing the WB. We still let
                // the LB run on the droppers already produced by earlier WB rounds.
                wb_drops.push(Vec::new());
                break None;
            }
            if let Some(b) = bye {
                next_order.push(b);
            }
            wb_drops.push(drops);
            wb_order = next_order;
            wb_round += 1;
        };
        let wb_settled = wb_champion.is_some();

        // --- Losers bracket: alternate minor (LB-vs-LB) and major (LB-vs-WB-drop). ---
        //
        // LB round numbering is its own sequence (1-based). The standard layout:
        //   L1 (minor): pair the round-1 WB droppers against each other.
        //   L2 (major): the L1 survivors meet the round-2 WB droppers.
        //   L3 (minor): pair the L2 survivors against each other.
        //   L4 (major): the L3 survivors meet the round-3 WB droppers.
        //   … until one LB survivor remains (the LB champion).
        //
        // We only lay out an LB round once its inputs exist: a major round needs that
        // WB round's droppers to have been produced (which requires the WB to have run
        // that far). If the inputs are not ready, the LB waits — exactly mirroring how
        // a real schedule cannot run a losers heat before its feeders finish.
        let mut lb_round = 1usize;
        let mut lb_survivors: Vec<CompetitorRef> = Vec::new();
        // Which WB drop-batch the next *major* round will consume (round 2 first).
        let mut next_drop_round = 2usize;
        // The LB heat-id round counter is independent of the minor/major distinction;
        // it just increments per LB round we lay out.
        let lb_champion: Option<CompetitorRef> = loop {
            // Inject the first batch of droppers as the LB seed before round 1.
            if lb_round == 1 {
                lb_survivors = wb_drops.get(1).cloned().unwrap_or_default();
            }

            let is_major = lb_round % 2 == 0;
            if is_major {
                // Major round: bring in the next WB drop batch. It must be available —
                // i.e. that WB round must have fully completed. If not, the LB stalls
                // here until the WB produces it.
                let drop_ready = wb_settled || next_drop_round < wb_drops.len();
                let incoming = wb_drops.get(next_drop_round).cloned();
                match incoming {
                    Some(drops) if drop_ready => {
                        // Interleave survivors with incoming droppers for the pairing.
                        // Standard practice cross-seeds them; we keep it deterministic
                        // and simple: survivors first (in their order), then droppers.
                        lb_survivors.extend(drops);
                        next_drop_round += 1;
                    }
                    _ => {
                        // The feeding WB round has not finished; the LB cannot run this
                        // major round yet. Stop here (no LB champion settled).
                        break None;
                    }
                }
            }

            if lb_survivors.len() <= 1 {
                // One (or zero) LB competitor left. If the WB is settled this is the LB
                // champion; otherwise the LB is just waiting on more droppers.
                if wb_settled && next_drop_round >= wb_drops.len() {
                    break lb_survivors.first().cloned();
                }
                // More droppers are still coming (a future major round). Advance the
                // round counter so the next major round pulls them in.
                if !is_major {
                    lb_round += 1;
                    continue;
                }
                break None;
            }

            let (heats, bye) = Self::pair_round(lb_round, &lb_survivors, Self::losers_id);
            let mut next_survivors = Vec::new();
            let mut eliminated = Vec::new();
            let mut complete = true;
            for heat in &heats {
                match outcome(&heat.heat.0) {
                    Some((winner, loser)) => {
                        next_survivors.push(winner);
                        eliminated.push(loser);
                    }
                    None => {
                        complete = false;
                        pending.push(heat.clone());
                    }
                }
            }
            if !complete {
                break None;
            }
            if let Some(b) = bye {
                next_survivors.push(b);
            }
            if !eliminated.is_empty() {
                eliminated_by_round.push(eliminated);
            }
            lb_survivors = next_survivors;
            lb_round += 1;
        };

        // --- Grand final ----------------------------------------------------
        //
        // Only meaningful once BOTH champions are known. The WB champion has 0 losses,
        // the LB champion 1. First heat `de-gf`; an optional reset `de-gf-reset`.
        let mut champion: Option<CompetitorRef> = None;
        let mut runner_up: Option<CompetitorRef> = None;
        if let (Some(wb_champ), Some(lb_champ)) = (wb_champion.clone(), lb_champion.clone()) {
            match outcome(Self::GRAND_FINAL) {
                None => {
                    // Grand final not yet run: emit it. WB champ seeded first.
                    pending.push(HeatPlan::new(
                        Self::GRAND_FINAL,
                        vec![wb_champ.clone(), lb_champ.clone()],
                    ));
                }
                Some((gf_winner, gf_loser)) => {
                    if gf_winner == wb_champ {
                        // WB champ won → LB champ takes their 2nd loss. Done, no reset.
                        champion = Some(wb_champ.clone());
                        runner_up = Some(gf_loser);
                    } else {
                        // LB champ won heat 1 → both now have one loss.
                        if self.bracket_reset {
                            // Reset: a decider settles it.
                            match outcome(Self::GRAND_FINAL_RESET) {
                                None => {
                                    pending.push(HeatPlan::new(
                                        Self::GRAND_FINAL_RESET,
                                        vec![wb_champ.clone(), lb_champ.clone()],
                                    ));
                                }
                                Some((reset_winner, reset_loser)) => {
                                    champion = Some(reset_winner);
                                    runner_up = Some(reset_loser);
                                }
                            }
                        } else {
                            // No reset: the single grand final stands.
                            champion = Some(gf_winner);
                            runner_up = Some(gf_loser);
                        }
                    }
                }
            }
        }

        Replay {
            pending,
            champion,
            runner_up,
            wb_champion,
            lb_champion,
            lb_survivors,
            eliminated_by_round,
        }
    }
}

/// The outcome of replaying the completed history (see [`DoubleElim::replay`]).
struct Replay {
    /// Heats awaiting a result, across both brackets and the grand final. Empty once
    /// the bracket is complete.
    pending: Vec<HeatPlan>,
    /// The overall champion, once the grand final has resolved.
    champion: Option<CompetitorRef>,
    /// The grand-final loser (overall runner-up), once it has resolved.
    runner_up: Option<CompetitorRef>,
    /// The winners-bracket champion, once the WB has settled (0 losses).
    wb_champion: Option<CompetitorRef>,
    /// The losers-bracket champion, once the LB has settled (1 loss).
    lb_champion: Option<CompetitorRef>,
    /// The competitors still alive in the losers bracket mid-run (for provisional
    /// ranking); collapses to the LB champion once it settles.
    lb_survivors: Vec<CompetitorRef>,
    /// Per LB round (earliest first), the competitors that round eliminated (their
    /// second loss), in heat order.
    eliminated_by_round: Vec<Vec<CompetitorRef>>,
}

impl Generator for DoubleElim {
    fn next(&mut self, completed: &[CompletedHeat]) -> GeneratorStep {
        let replay = self.replay(completed);
        if replay.pending.is_empty() && replay.champion.is_some() {
            GeneratorStep::Complete
        } else if replay.pending.is_empty() {
            // Defensive: no pending heats but no champion yet. With a well-formed
            // history this does not happen (a stalled bracket always has a pending
            // heat or a champion); treat it as complete so the loop terminates.
            GeneratorStep::Complete
        } else {
            GeneratorStep::Run(replay.pending)
        }
    }

    fn ranking(&self, completed: &[CompletedHeat]) -> Vec<RankEntry> {
        let replay = self.replay(completed);

        // Bands, best first:
        //   0: champion (once known) — alone at the top.
        //   1: grand-final loser (runner-up).
        //   then: still-alive competitors not yet placed (WB/LB champions mid-run,
        //         LB survivors), in a "furthest along" band,
        //   then: each LB elimination round, latest first (further = higher),
        //   finally: everyone who never appeared (e.g. a never-run field) by seed.
        let mut rows: Vec<(CompetitorRef, (u32, u32))> = Vec::new();
        let mut placed: Vec<CompetitorRef> = Vec::new();
        let mut band = 0u32;

        let push = |rows: &mut Vec<(CompetitorRef, (u32, u32))>,
                    placed: &mut Vec<CompetitorRef>,
                    competitor: &CompetitorRef,
                    band: u32,
                    within: u32| {
            if !placed.contains(competitor) {
                placed.push(competitor.clone());
                rows.push((competitor.clone(), (band, within)));
            }
        };

        if let Some(champ) = &replay.champion {
            push(&mut rows, &mut placed, champ, band, 0);
        }
        band += 1;
        if let Some(ru) = &replay.runner_up {
            push(&mut rows, &mut placed, ru, band, 0);
        }
        band += 1;

        // Still-alive-but-unplaced competitors (mid-run): the WB champion waiting on
        // the LB, the LB champion waiting on the grand final, the LB survivors. They
        // outrank everyone already eliminated.
        if let Some(wb) = &replay.wb_champion {
            push(&mut rows, &mut placed, wb, band, 0);
        }
        if let Some(lb) = &replay.lb_champion {
            push(&mut rows, &mut placed, lb, band, 1);
        }
        for (i, survivor) in replay.lb_survivors.iter().enumerate() {
            push(&mut rows, &mut placed, survivor, band, 2 + i as u32);
        }
        band += 1;

        // Eliminated in the losers bracket, latest round first (advanced furthest).
        for eliminated in replay.eliminated_by_round.iter().rev() {
            for (i, competitor) in eliminated.iter().enumerate() {
                push(&mut rows, &mut placed, competitor, band, i as u32);
            }
            band += 1;
        }

        // Any field member never seen anywhere (e.g. before any heat / tiny field):
        // keep them in seed order behind everyone, so the ranking is always total.
        for (i, competitor) in self.field.iter().enumerate() {
            push(&mut rows, &mut placed, competitor, band, i as u32);
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

    /// The (heat-id, lineup) pairs of a `Run` step (panics if `Complete`).
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

    /// Just the heat ids of a `Run` step.
    fn heat_ids(step: &GeneratorStep) -> Vec<String> {
        lineups(step).into_iter().map(|(id, _)| id).collect()
    }

    /// Drive a generator to completion, resolving every emitted heat with `pick`
    /// (given a heat id + its lineup, return the winner). Returns the full completed
    /// history. Panics if it does not converge within a generous round budget.
    fn drive(
        g: &mut DoubleElim,
        pick: impl Fn(&str, &[CompetitorRef]) -> CompetitorRef,
    ) -> Vec<CompletedHeat> {
        let mut completed: Vec<CompletedHeat> = Vec::new();
        let mut rounds = 0;
        while let GeneratorStep::Run(heats) = g.next(&completed) {
            rounds += 1;
            assert!(rounds < 32, "bracket should converge well within 32 rounds");
            assert!(!heats.is_empty(), "a Run step must carry at least one heat");
            for plan in &heats {
                let winner = pick(&plan.heat.0, &plan.lineup);
                let loser = plan
                    .lineup
                    .iter()
                    .find(|c| **c != winner)
                    .cloned()
                    .expect("head-to-head heat has a loser");
                completed.push(CompletedHeat::new(
                    plan.heat.0.clone(),
                    h2h(&winner.0, &loser.0),
                ));
            }
        }
        completed
    }

    /// A `pick` where the lower numeric seed (smaller ref) always wins — i.e. a
    /// perfectly chalk bracket.
    fn chalk(_id: &str, lineup: &[CompetitorRef]) -> CompetitorRef {
        lineup
            .iter()
            .min_by_key(|c| c.0.parse::<u32>().unwrap_or(u32::MAX))
            .cloned()
            .expect("non-empty lineup")
    }

    // --- Round-1 layout -----------------------------------------------------

    #[test]
    fn winners_round_one_pairs_strong_vs_weak_head_to_head() {
        let mut g = DoubleElim::new(field(&["1", "2", "3", "4"]), true);
        // bracket order 1,4,2,3 → WB heats (1 v 4), (2 v 3). No LB/GF yet.
        assert_eq!(
            lineups(&g.next(&[])),
            vec![
                ("de-w1-h0".to_string(), vec!["1".into(), "4".into()]),
                ("de-w1-h1".to_string(), vec!["2".into(), "3".into()]),
            ]
        );
    }

    // --- Loser drops to losers bracket --------------------------------------

    #[test]
    fn winners_loser_drops_into_the_losers_bracket() {
        let mut g = DoubleElim::new(field(&["1", "2", "3", "4"]), true);
        let _ = g.next(&[]);
        // WB round 1: 1 beats 4, 2 beats 3. Droppers: 4, 3.
        let completed = vec![
            CompletedHeat::new("de-w1-h0", h2h("1", "4")),
            CompletedHeat::new("de-w1-h1", h2h("2", "3")),
        ];
        // Next emits the WB final (1 v 2) AND the LB round-1 minor (4 v 3).
        let ids = heat_ids(&g.next(&completed));
        assert!(ids.contains(&"de-w2-h0".to_string()), "WB final emitted");
        assert!(
            ids.contains(&"de-l1-h0".to_string()),
            "LB round-1 minor emitted from the WB droppers"
        );
        // The LB heat pairs the two WB-round-1 losers.
        let lb = lineups(&g.next(&completed))
            .into_iter()
            .find(|(id, _)| id == "de-l1-h0")
            .unwrap();
        assert_eq!(lb.1, vec!["4".to_string(), "3".to_string()]);
    }

    // --- A second loss eliminates -------------------------------------------

    #[test]
    fn a_second_loss_eliminates_in_the_losers_bracket() {
        let mut g = DoubleElim::new(field(&["1", "2", "3", "4"]), true);
        // WB r1: 1>4, 2>3. WB final & LB minor open.
        let mut completed = vec![
            CompletedHeat::new("de-w1-h0", h2h("1", "4")),
            CompletedHeat::new("de-w1-h1", h2h("2", "3")),
        ];
        let _ = g.next(&completed);
        // WB final: 1 beats 2 → 2 drops to LB. LB minor: 4 beats 3 → 3 is eliminated
        // (its 2nd loss).
        completed.push(CompletedHeat::new("de-w2-h0", h2h("1", "2")));
        completed.push(CompletedHeat::new("de-l1-h0", h2h("4", "3")));
        // Now the LB major pairs the LB survivor (4) with the WB-final dropper (2).
        let ids = heat_ids(&g.next(&completed));
        assert!(
            ids.contains(&"de-l2-h0".to_string()),
            "LB major round emitted, got {ids:?}"
        );
        let lb_major = lineups(&g.next(&completed))
            .into_iter()
            .find(|(id, _)| id == "de-l2-h0")
            .unwrap();
        // Survivor 4 vs WB-final loser 2.
        assert_eq!(lb_major.1, vec!["4".to_string(), "2".to_string()]);
    }

    // --- A once-beaten lower seed can still win via the losers bracket -------

    #[test]
    fn lower_seed_loses_once_then_wins_through_losers_to_champion() {
        let mut g = DoubleElim::new(field(&["1", "2", "3", "4"]), true);
        // Seed 2 will lose in the WB but storm back through the LB and win it all.
        // WB r1: 1>4, 2>3.
        let mut completed = vec![
            CompletedHeat::new("de-w1-h0", h2h("1", "4")),
            CompletedHeat::new("de-w1-h1", h2h("2", "3")),
        ];
        let _ = g.next(&completed);
        // WB final: 1 beats 2 (2's first loss → drops to LB). LB minor: 4 beats 3.
        completed.push(CompletedHeat::new("de-w2-h0", h2h("1", "2")));
        completed.push(CompletedHeat::new("de-l1-h0", h2h("4", "3")));
        let _ = g.next(&completed);
        // LB major: 2 beats 4 (4 eliminated) → 2 is the LB champion.
        completed.push(CompletedHeat::new("de-l2-h0", h2h("2", "4")));
        // Grand final: WB champ 1 vs LB champ 2.
        let gf = lineups(&g.next(&completed));
        assert_eq!(
            gf,
            vec![("de-gf".to_string(), vec!["1".into(), "2".into()])]
        );
        // 2 wins the grand final heat 1 → both have one loss → reset decider.
        completed.push(CompletedHeat::new("de-gf", h2h("2", "1")));
        let reset = lineups(&g.next(&completed));
        assert_eq!(
            reset,
            vec![("de-gf-reset".to_string(), vec!["1".into(), "2".into()])]
        );
        // 2 wins the reset → 2 is champion despite an early WB loss.
        completed.push(CompletedHeat::new("de-gf-reset", h2h("2", "1")));
        assert_eq!(g.next(&completed), GeneratorStep::Complete);

        let ranking = g.ranking(&completed);
        assert_eq!(ranking[0].competitor, cref("2"));
        assert_eq!(ranking[0].position, 1);
        assert_eq!(ranking[1].competitor, cref("1"));
        assert_eq!(ranking[1].position, 2);
    }

    // --- Grand final: WB champ wins heat 1, no reset ------------------------

    #[test]
    fn grand_final_wb_champ_win_needs_no_reset() {
        let mut g = DoubleElim::new(field(&["1", "2", "3", "4"]), true);
        let completed = drive(&mut g, chalk);
        // Chalk: seed 1 wins every heat. No reset heat should ever be emitted.
        assert!(
            !completed.iter().any(|c| c.heat.0 == "de-gf-reset"),
            "WB champ winning GF heat 1 needs no reset"
        );
        assert!(
            completed.iter().any(|c| c.heat.0 == "de-gf"),
            "a grand final was played"
        );
        let ranking = g.ranking(&completed);
        assert_eq!(ranking[0].competitor, cref("1"));
        assert_eq!(ranking[0].position, 1);
    }

    // --- bracket_reset = false: single grand final --------------------------

    #[test]
    fn no_reset_config_plays_a_single_grand_final() {
        let mut g = DoubleElim::new(field(&["1", "2", "3", "4"]), false);
        // WB r1.
        let mut completed = vec![
            CompletedHeat::new("de-w1-h0", h2h("1", "4")),
            CompletedHeat::new("de-w1-h1", h2h("2", "3")),
        ];
        let _ = g.next(&completed);
        completed.push(CompletedHeat::new("de-w2-h0", h2h("1", "2")));
        completed.push(CompletedHeat::new("de-l1-h0", h2h("4", "3")));
        let _ = g.next(&completed);
        completed.push(CompletedHeat::new("de-l2-h0", h2h("2", "4")));
        let _ = g.next(&completed);
        // LB champ 2 wins the GF heat — with reset OFF that is the title, no decider.
        completed.push(CompletedHeat::new("de-gf", h2h("2", "1")));
        assert_eq!(g.next(&completed), GeneratorStep::Complete);
        let ranking = g.ranking(&completed);
        assert_eq!(ranking[0].competitor, cref("2"));
        assert_eq!(ranking[1].competitor, cref("1"));
    }

    // --- Byes (odd field) ---------------------------------------------------

    #[test]
    fn odd_field_gives_the_middle_seed_a_winners_bye() {
        // 3 seeds: bracket order 1,3,2 → WB heat (1 v 3), bye for 2.
        let mut g = DoubleElim::new(field(&["1", "2", "3"]), true);
        assert_eq!(
            lineups(&g.next(&[])),
            vec![("de-w1-h0".to_string(), vec!["1".into(), "3".into()])]
        );
        // 1 beats 3. WB final: 1 v 2 (2 had the bye). 3 dropped alone into the LB —
        // with a single LB competitor there is no LB heat yet (it waits for more).
        let completed = vec![CompletedHeat::new("de-w1-h0", h2h("1", "3"))];
        let ids = heat_ids(&g.next(&completed));
        assert!(ids.contains(&"de-w2-h0".to_string()));
        let wb_final = lineups(&g.next(&completed))
            .into_iter()
            .find(|(id, _)| id == "de-w2-h0")
            .unwrap();
        assert_eq!(wb_final.1, vec!["1".to_string(), "2".to_string()]);
    }

    #[test]
    fn three_seed_bracket_runs_to_a_champion() {
        let mut g = DoubleElim::new(field(&["1", "2", "3"]), true);
        let completed = drive(&mut g, chalk);
        let ranking = g.ranking(&completed);
        assert_eq!(ranking.len(), 3, "every seed appears in the ranking");
        assert_eq!(ranking[0].competitor, cref("1"));
        assert_eq!(ranking[0].position, 1);
        assert_eq!(
            ranking.iter().filter(|e| e.position == 1).count(),
            1,
            "exactly one champion"
        );
    }

    // --- Full small brackets to a champion ----------------------------------

    #[test]
    fn full_four_seed_chalk_bracket_runs_to_top_seed() {
        let mut g = DoubleElim::new(field(&["1", "2", "3", "4"]), true);
        let completed = drive(&mut g, chalk);
        let ranking = g.ranking(&completed);
        assert_eq!(ranking.len(), 4);
        assert_eq!(ranking[0].competitor, cref("1"));
        assert_eq!(ranking[0].position, 1);
        assert_eq!(
            ranking.iter().filter(|e| e.position == 1).count(),
            1,
            "exactly one champion"
        );
        // The runner-up is the grand-final loser; everyone appears exactly once.
        let mut seen: Vec<String> = names(&ranking);
        seen.sort();
        assert_eq!(seen, vec!["1", "2", "3", "4"]);
    }

    #[test]
    fn full_eight_seed_chalk_bracket_runs_to_top_seed() {
        let mut g = DoubleElim::new(field(&["1", "2", "3", "4", "5", "6", "7", "8"]), true);
        let completed = drive(&mut g, chalk);
        let ranking = g.ranking(&completed);
        assert_eq!(ranking.len(), 8, "every seed appears once");
        assert_eq!(ranking[0].competitor, cref("1"));
        assert_eq!(ranking[0].position, 1);
        assert_eq!(
            ranking.iter().filter(|e| e.position == 1).count(),
            1,
            "exactly one champion"
        );
        let mut seen: Vec<String> = names(&ranking);
        seen.sort();
        assert_eq!(seen, vec!["1", "2", "3", "4", "5", "6", "7", "8"]);
    }

    #[test]
    fn eight_seed_upset_champion_comes_through_losers() {
        // Seed 1 wins the WB until the grand final, but seed 2 (who lost to 1 in the
        // WB) comes back through the LB and beats 1 twice in the GF (reset) to win.
        let mut g = DoubleElim::new(field(&["1", "2", "3", "4", "5", "6", "7", "8"]), true);
        // pick: seed 1 wins everything EXCEPT the two grand-final heats, which seed 2
        // wins; otherwise lower seed wins. This makes 2 the comeback champion.
        let pick = |id: &str, lineup: &[CompetitorRef]| -> CompetitorRef {
            if id == "de-gf" || id == "de-gf-reset" {
                // Whoever is seed "2" in this heat wins; both GF heats are 1 v 2.
                if lineup.contains(&cref("2")) {
                    return cref("2");
                }
            }
            chalk(id, lineup)
        };
        let completed = drive(&mut g, pick);
        let ranking = g.ranking(&completed);
        assert_eq!(ranking[0].competitor, cref("2"), "comeback champion");
        assert_eq!(ranking[0].position, 1);
        assert_eq!(ranking[1].competitor, cref("1"), "GF loser is runner-up");
        assert_eq!(ranking[1].position, 2);
    }

    // --- Determinism --------------------------------------------------------

    #[test]
    fn next_is_deterministic_for_the_same_history() {
        let mut g1 = DoubleElim::new(field(&["1", "2", "3", "4"]), true);
        let mut g2 = DoubleElim::new(field(&["1", "2", "3", "4"]), true);
        assert_eq!(g1.next(&[]), g2.next(&[]));
        let completed = vec![
            CompletedHeat::new("de-w1-h0", h2h("1", "4")),
            CompletedHeat::new("de-w1-h1", h2h("2", "3")),
        ];
        assert_eq!(g1.next(&completed), g2.next(&completed));
    }

    #[test]
    fn seeding_draw_reorders_the_bracket_deterministically() {
        let cfg = FormatConfig::new(field(&["1", "2", "3", "4"]))
            .with_seeding(SeedingOutcome::drawn(field(&["3", "1", "4", "2"])));
        let mut g1 = DoubleElim::from_config(&cfg);
        let mut g2 = DoubleElim::from_config(&cfg);
        let s1 = g1.next(&[]);
        assert_eq!(s1, g2.next(&[]));
        // Drawn order 3,1,4,2 → bracket order 3,2,1,4 → WB heats (3 v 2), (1 v 4).
        assert_eq!(
            lineups(&s1),
            vec![
                ("de-w1-h0".to_string(), vec!["3".into(), "2".into()]),
                ("de-w1-h1".to_string(), vec!["1".into(), "4".into()]),
            ]
        );
    }

    // --- Completion edge case -----------------------------------------------

    #[test]
    fn single_competitor_field_completes_immediately() {
        let mut g = DoubleElim::new(field(&["1"]), true);
        assert_eq!(g.next(&[]), GeneratorStep::Complete);
        assert_eq!(names(&g.ranking(&[])), vec!["1"]);
    }

    // --- Provisional ranking ------------------------------------------------

    #[test]
    fn provisional_ranking_before_any_heat_is_seed_order() {
        let g = DoubleElim::new(field(&["1", "2", "3", "4"]), true);
        // Before any heat the field is all "still alive"; ranking falls back to seed
        // order so it is total and stable.
        assert_eq!(names(&g.ranking(&[])), vec!["1", "2", "3", "4"]);
    }

    // --- Registry -----------------------------------------------------------

    #[test]
    fn registry_builds_double_elim() {
        let mut registry = FormatRegistry::new();
        DoubleElim::register(&mut registry);
        assert_eq!(registry.names(), vec!["double_elim"]);

        let cfg = FormatConfig::new(field(&["1", "2", "3", "4"]));
        let mut g = registry
            .build(DoubleElim::NAME, &cfg)
            .expect("double_elim is registered");
        assert_eq!(
            lineups(&g.next(&[])),
            vec![
                ("de-w1-h0".to_string(), vec!["1".into(), "4".into()]),
                ("de-w1-h1".to_string(), vec!["2".into(), "3".into()]),
            ]
        );
    }

    #[test]
    fn registry_reads_bracket_reset_param_off() {
        let mut registry = FormatRegistry::new();
        DoubleElim::register(&mut registry);
        let cfg = FormatConfig::new(field(&["1", "2", "3", "4"])).with_param("bracket_reset", "0");
        let mut g = registry.build(DoubleElim::NAME, &cfg).unwrap();
        // Drive with the LB champ winning the GF; with reset off, no decider is played.
        let mut completed = vec![
            CompletedHeat::new("de-w1-h0", h2h("1", "4")),
            CompletedHeat::new("de-w1-h1", h2h("2", "3")),
        ];
        let _ = g.next(&completed);
        completed.push(CompletedHeat::new("de-w2-h0", h2h("1", "2")));
        completed.push(CompletedHeat::new("de-l1-h0", h2h("4", "3")));
        let _ = g.next(&completed);
        completed.push(CompletedHeat::new("de-l2-h0", h2h("2", "4")));
        let _ = g.next(&completed);
        completed.push(CompletedHeat::new("de-gf", h2h("2", "1")));
        assert_eq!(g.next(&completed), GeneratorStep::Complete);
    }
}
