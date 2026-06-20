//! ZippyQ / rolling — the **honesty-forcing** dynamic format (race-engine.html §3).
//!
//! # The dynamic case the whole abstraction is designed against
//!
//! ZippyQ is *not* a precomputed schedule. The Race Director (RD) adds rounds **on
//! demand** and pilots **fly when ready** — there is no fixed sequence of heats laid
//! out in advance (RE §3, the callout). This generator is the production version of
//! [`crate::format::RollingDemo`]: it emits a heat **only** when one has been
//! *requested* and not yet run, and it derives every heat purely from current state +
//! the RD's explicit inputs. "Produce more heats dynamically from current state" is
//! the general shape; a fixed bracket is just the easy case of a generator that
//! happens to emit a predetermined sequence.
//!
//! # The on-demand / ready model (what an RD actually does)
//!
//! Real ZippyQ queues whoever is *ready to fly* into the next slot — often a **subset**
//! of the field, not the whole grid. This generator models that directly:
//!
//! - [`request_round(pilots)`](ZippyQ::request_round) queues a specific set of "ready"
//!   pilots into a pending round (the RD's "you four, you're up next"). Pilots not in
//!   the set simply do not fly that round.
//! - [`request_full_round()`](ZippyQ::request_full_round) is the convenience for "run
//!   the whole field once more" — it queues the entire seeded field.
//! - [`close()`](ZippyQ::close) is the RD signalling *no more rounds* — the session is
//!   over once the already-queued rounds drain.
//!
//! Each queued request becomes **one** [`HeatPlan`]. [`next`](Generator::next) pops the
//! oldest pending request and emits its heat; with nothing pending it returns
//! [`GeneratorStep::Complete`] (the format is done *for now* — the RD may
//! [`request_round`](ZippyQ::request_round) again, and `next` will then emit the new
//! heat). Once [`close`](ZippyQ::close)d and drained, `Complete` is final.
//!
//! # Why this stays deterministic & replayable (RE §6)
//!
//! The on-demand request is an **explicit input** recorded as generator state (the
//! pending queue + the closed flag), never a clock read or a coin flip inside `next`.
//! [`next`](Generator::next) is a pure function of (completed history + that recorded
//! state): the *only* thing that differs between two `next` calls that produce
//! different steps is a recorded `request_round` / `close`. Heat ids are derived from
//! the number of rounds already run, so the same history + the same queue always yield
//! the same heat — every replay identical.
//!
//! # Ranking — the best-flight aggregate (the honesty payoff)
//!
//! [`ranking`](Generator::ranking) scores each pilot on their **best single flight**
//! across *every* round they flew: their highest counted-lap total in any one heat
//! (ties broken deterministically by competitor). This is ZippyQ's "your best run
//! counts" — a pilot can fly again whenever ready and only their strongest flight
//! stands, which is exactly what makes the format honesty-forcing: there is no gaming a
//! fixed schedule, you just have to fly well once. A pilot who has not yet flown sits
//! at zero so the standing is always total over the field.
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, VecDeque};

use gridfpv_events::CompetitorRef;

use crate::format::{
    CompletedHeat, FormatConfig, FormatRegistry, Generator, GeneratorStep, HeatPlan, RankEntry,
    rank_by,
};

/// A ZippyQ / rolling [`Generator`]: rounds added on demand, pilots flying when ready,
/// best-flight aggregation (race-engine.html §3).
///
/// Built over a seeded `field` (the full roster, used to seed the standing and to back
/// [`request_full_round`](Self::request_full_round)); the RD then drives it with
/// [`request_round`](Self::request_round) / [`request_full_round`](Self::request_full_round)
/// and [`close`](Self::close). See the module docs for the model.
pub struct ZippyQ {
    /// The full seeded field, in seed/draw order. Used to seed the ranking (so a pilot
    /// who has not flown still appears) and as the lineup for a *full* round request.
    field: Vec<CompetitorRef>,
    /// Pending rounds the RD has requested but `next` has not yet emitted, oldest
    /// first. Each entry is one round's ready lineup; popped front-to-back by `next`.
    pending: VecDeque<Vec<CompetitorRef>>,
    /// Whether the RD has signalled "no more rounds" via [`close`](Self::close). Once
    /// set, `next` completes the moment the pending queue drains.
    closed: bool,
}

impl ZippyQ {
    /// The format name this registers under.
    pub const NAME: &'static str = "zippyq";

    /// Build over a seeded `field` with nothing queued and the session open. The RD
    /// must [`request_round`](Self::request_round) (or
    /// [`request_full_round`](Self::request_full_round)) before any heat is emitted.
    pub fn new(field: Vec<CompetitorRef>) -> Self {
        Self {
            field,
            pending: VecDeque::new(),
            closed: false,
        }
    }

    /// The registry constructor (RE §3). Reads `field`, applies any recorded `seeding`
    /// draw, and — for convenience so a freshly-built generator can emit immediately —
    /// pre-queues `rounds` **full**-field rounds (default `0`: nothing queued, the RD
    /// drives every round explicitly, which is the truest ZippyQ shape).
    pub fn from_config(config: &FormatConfig) -> Box<dyn Generator> {
        let field = config.seeding.apply(&config.field);
        let mut generator = Self::new(field);
        for _ in 0..config.param_usize("rounds", 0) {
            generator.request_full_round();
        }
        Box::new(generator)
    }

    /// Register this format under [`NAME`](Self::NAME).
    pub fn register(registry: &mut FormatRegistry) {
        registry.register(Self::NAME, Self::from_config);
    }

    /// **The explicit "add a round on demand" input** (RE §3): queue the given *ready*
    /// `pilots` into the next pending round. This is the RD lining up whoever is ready —
    /// typically a **subset** of the field, not the whole grid. The next
    /// [`Generator::next`] (once earlier pending rounds have run) emits this lineup as a
    /// heat. An empty `pilots` queues an empty round, which `next` skips over (a no-op
    /// request); the lineup is otherwise emitted verbatim, in the order given.
    ///
    /// This — together with [`close`](Self::close) — is the *only* thing that makes the
    /// dynamic format produce more heats; there is no clock or RNG inside `next`.
    pub fn request_round(&mut self, pilots: Vec<CompetitorRef>) {
        self.pending.push_back(pilots);
    }

    /// Convenience over [`request_round`](Self::request_round): queue the **whole**
    /// seeded field for one more round (the "everyone flies again" case).
    pub fn request_full_round(&mut self) {
        let field = self.field.clone();
        self.request_round(field);
    }

    /// The RD signal that **no more rounds** will be requested. After this, once the
    /// pending queue drains, [`Generator::next`] returns [`GeneratorStep::Complete`] for
    /// good. Already-queued rounds still run; only *new* demand is closed off. Idempotent.
    pub fn close(&mut self) {
        self.closed = true;
    }

    /// Whether the session has been [`close`](Self::close)d.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// How many rounds are queued but not yet emitted.
    pub fn pending_rounds(&self) -> usize {
        self.pending.len()
    }

    /// Heat id for round `n` (1-based) — derived purely from the rounds already run, so
    /// the same history always names the next heat identically (replay-stable).
    fn round_id(n: usize) -> String {
        format!("round-{n}")
    }
}

impl Generator for ZippyQ {
    fn next(&mut self, completed: &[CompletedHeat]) -> GeneratorStep {
        // Pop pending requests until one has a non-empty lineup (an empty request is a
        // no-op the RD can issue harmlessly). The heat is numbered from the rounds
        // already completed, so `next` derives its id purely from current state.
        while let Some(lineup) = self.pending.pop_front() {
            if lineup.is_empty() {
                continue;
            }
            let round = completed.len() + 1;
            return GeneratorStep::Run(vec![HeatPlan::new(Self::round_id(round), lineup)]);
        }
        // Nothing pending. The format is done *for now* — `Complete` until the RD
        // requests more (the heat loop calls `next` again after a `request_round`). Once
        // `close`d, this `Complete` is final; either way the step is the same value.
        GeneratorStep::Complete
    }

    fn ranking(&self, completed: &[CompletedHeat]) -> Vec<RankEntry> {
        // Best-flight aggregate: each pilot's *highest* counted-lap total in any single
        // round they flew. A pilot is seeded at 0 so the unflown still appear, keeping
        // the standing total over the field (RE §3, §7.4: provisional mid-format, final
        // once `Complete`).
        let mut best: BTreeMap<CompetitorRef, u32> = BTreeMap::new();
        for competitor in &self.field {
            best.entry(competitor.clone()).or_insert(0);
        }
        for heat in completed {
            for place in &heat.result.places {
                let entry = best.entry(place.competitor.competitor.clone()).or_insert(0);
                *entry = (*entry).max(place.laps);
            }
        }
        // Rank key: negate laps so more laps = smaller key = better; `rank_by` adds the
        // competitor ref as the final, total, deterministic tie-break.
        let rows = best
            .into_iter()
            .map(|(competitor, laps)| (competitor, -(laps as i64)))
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

    /// A scored heat from `(name, position, laps)` rows — no passes/scorer needed.
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
                })
                .collect(),
        }
    }

    fn names(entries: &[RankEntry]) -> Vec<String> {
        entries.iter().map(|e| e.competitor.0.clone()).collect()
    }

    /// A `Run` step's single heat plan, unwrapped — panics if `Complete` or not exactly
    /// one heat.
    fn one_heat(step: GeneratorStep) -> HeatPlan {
        match step {
            GeneratorStep::Run(heats) if heats.len() == 1 => heats.into_iter().next().unwrap(),
            other => panic!("expected exactly one heat, got {other:?}"),
        }
    }

    // --- The dynamic contract: nothing happens without an explicit request ------

    #[test]
    fn emits_nothing_until_a_round_is_requested() {
        let mut zq = ZippyQ::new(field(&["A", "B"]));
        // No request → Complete (nothing pending). NOT a precomputed schedule.
        assert_eq!(zq.next(&[]), GeneratorStep::Complete);
    }

    #[test]
    fn a_round_appears_only_after_request_round() {
        let mut zq = ZippyQ::new(field(&["A", "B", "C"]));
        // Before the request: nothing.
        assert_eq!(zq.next(&[]), GeneratorStep::Complete);
        // The RD queues a *subset* of the field who are ready (pilots fly when ready).
        zq.request_round(field(&["A", "C"]));
        let heat = one_heat(zq.next(&[]));
        // The heat is exactly the requested ready subset, in requested order.
        assert_eq!(heat, HeatPlan::new("round-1", field(&["A", "C"])));
    }

    #[test]
    fn rounds_are_added_on_demand_and_numbered_from_current_state() {
        let mut zq = ZippyQ::new(field(&["A", "B"]));
        zq.request_full_round();
        // Round 1 over the whole field.
        assert_eq!(
            one_heat(zq.next(&[])),
            HeatPlan::new("round-1", field(&["A", "B"]))
        );

        // Round 1 comes back; with nothing further requested the format is done *for now*.
        let r1 = CompletedHeat::new("round-1", result(&[("A", 1, 5), ("B", 2, 3)]));
        assert_eq!(zq.next(std::slice::from_ref(&r1)), GeneratorStep::Complete);

        // The RD asks for another round on demand — round 2 is numbered from the one
        // round already run (current state), not from any precomputed plan.
        zq.request_full_round();
        assert_eq!(
            one_heat(zq.next(std::slice::from_ref(&r1))),
            HeatPlan::new("round-2", field(&["A", "B"]))
        );
    }

    /// The honesty check: this is NOT a precomputed schedule. Requesting *different*
    /// pilots / counts yields *different* heats from the same starting state.
    #[test]
    fn not_a_precomputed_schedule_requests_drive_the_heats() {
        // Two generators over the same field, driven with different on-demand requests,
        // diverge — proof the heats come from the requests, not a fixed sequence.
        let mut a = ZippyQ::new(field(&["A", "B", "C", "D"]));
        let mut b = ZippyQ::new(field(&["A", "B", "C", "D"]));

        a.request_round(field(&["A", "B"]));
        b.request_round(field(&["C", "D", "A"]));

        let ha = one_heat(a.next(&[]));
        let hb = one_heat(b.next(&[]));
        assert_ne!(ha.lineup, hb.lineup);
        assert_eq!(ha.lineup, field(&["A", "B"]));
        assert_eq!(hb.lineup, field(&["C", "D", "A"]));

        // And requesting a *second* round on one but not the other diverges the count:
        // `a` still has a round queued, `b` is idle.
        a.request_round(field(&["C"]));
        assert_eq!(a.pending_rounds(), 1);
        assert_eq!(b.pending_rounds(), 0);
        assert_eq!(b.next(&[]), GeneratorStep::Complete);
    }

    #[test]
    fn multiple_pending_rounds_drain_in_request_order() {
        let mut zq = ZippyQ::new(field(&["A", "B", "C"]));
        // Queue two rounds up front; they drain oldest-first, one heat per `next`.
        zq.request_round(field(&["A", "B"]));
        zq.request_round(field(&["B", "C"]));
        assert_eq!(zq.pending_rounds(), 2);

        let h1 = one_heat(zq.next(&[]));
        assert_eq!(h1, HeatPlan::new("round-1", field(&["A", "B"])));

        // First round flown; the second pops next, numbered round-2 from current state.
        let r1 = CompletedHeat::new("round-1", result(&[("A", 1, 4), ("B", 2, 3)]));
        let h2 = one_heat(zq.next(std::slice::from_ref(&r1)));
        assert_eq!(h2, HeatPlan::new("round-2", field(&["B", "C"])));
    }

    #[test]
    fn empty_request_is_a_no_op_skipped_by_next() {
        let mut zq = ZippyQ::new(field(&["A", "B"]));
        // An empty ready set queues nothing meaningful; `next` skips it through to the
        // real round behind it.
        zq.request_round(Vec::new());
        zq.request_round(field(&["A"]));
        assert_eq!(
            one_heat(zq.next(&[])),
            HeatPlan::new("round-1", field(&["A"]))
        );
    }

    // --- close() ------------------------------------------------------------

    #[test]
    fn close_with_no_pending_completes() {
        let mut zq = ZippyQ::new(field(&["A", "B"]));
        assert!(!zq.is_closed());
        zq.close();
        assert!(zq.is_closed());
        assert_eq!(zq.next(&[]), GeneratorStep::Complete);
    }

    #[test]
    fn close_still_drains_already_queued_rounds() {
        let mut zq = ZippyQ::new(field(&["A", "B"]));
        zq.request_full_round();
        zq.close();
        // The already-queued round still runs even though the session is closed.
        assert_eq!(
            one_heat(zq.next(&[])),
            HeatPlan::new("round-1", field(&["A", "B"]))
        );
        // Then, drained + closed → Complete for good.
        let r1 = CompletedHeat::new("round-1", result(&[("A", 1, 5), ("B", 2, 4)]));
        assert_eq!(zq.next(std::slice::from_ref(&r1)), GeneratorStep::Complete);
    }

    // --- Best-flight ranking ------------------------------------------------

    #[test]
    fn ranking_aggregates_best_flight_across_a_pilots_rounds() {
        let zq = ZippyQ::new(field(&["A", "B", "C"]));
        // Round 1: A 5, C 4, B 3.
        let r1 = CompletedHeat::new("round-1", result(&[("A", 1, 5), ("C", 2, 4), ("B", 3, 3)]));
        // Round 2: B surges to 7 (their best flight); A 5 again; C 4.
        let r2 = CompletedHeat::new("round-2", result(&[("B", 1, 7), ("A", 2, 5), ("C", 3, 4)]));
        let completed = vec![r1, r2];

        // Best single flight per pilot: B 7, A 5, C 4 → B, A, C.
        let ranking = zq.ranking(&completed);
        assert_eq!(names(&ranking), vec!["B", "A", "C"]);
        assert_eq!(ranking[0].position, 1);
        assert_eq!(ranking[1].position, 2);
        assert_eq!(ranking[2].position, 3);
    }

    #[test]
    fn ranking_seeds_unflown_pilots_at_zero() {
        let zq = ZippyQ::new(field(&["A", "B", "C"]));
        // Only A and B flew; C never did and so sits last on 0 laps.
        let r1 = CompletedHeat::new("round-1", result(&[("A", 1, 4), ("B", 2, 2)]));
        let ranking = zq.ranking(std::slice::from_ref(&r1));
        assert_eq!(names(&ranking), vec!["A", "B", "C"]);
        assert_eq!(ranking[2].competitor, cref("C"));
    }

    #[test]
    fn ranking_before_any_heat_is_the_seed_order() {
        let zq = ZippyQ::new(field(&["A", "B", "C"]));
        // Nothing flown: every pilot at 0 → tie on laps, broken by competitor ref, all
        // sharing position 1 (a flat provisional standing).
        let ranking = zq.ranking(&[]);
        assert_eq!(names(&ranking), vec!["A", "B", "C"]);
        assert!(ranking.iter().all(|e| e.position == 1));
    }

    // --- Determinism --------------------------------------------------------

    #[test]
    fn next_is_deterministic_for_the_same_state() {
        let mut g1 = ZippyQ::new(field(&["A", "B"]));
        let mut g2 = ZippyQ::new(field(&["A", "B"]));
        g1.request_full_round();
        g2.request_full_round();
        // Same recorded request + same history → identical step.
        assert_eq!(g1.next(&[]), g2.next(&[]));
        // And calling `ranking` twice on the same history is identical.
        let r1 = CompletedHeat::new("round-1", result(&[("A", 1, 5), ("B", 2, 3)]));
        let c = vec![r1];
        assert_eq!(g1.ranking(&c), g1.ranking(&c));
    }

    // --- Registry -----------------------------------------------------------

    #[test]
    fn registry_builds_zippyq() {
        let mut registry = FormatRegistry::new();
        ZippyQ::register(&mut registry);
        assert_eq!(registry.names(), vec!["zippyq"]);

        // Default config queues no rounds (the truest on-demand shape): a freshly-built
        // generator emits nothing until the RD requests a round.
        let cfg = FormatConfig::new(field(&["A", "B"]));
        let mut generator = registry
            .build(ZippyQ::NAME, &cfg)
            .expect("zippyq is registered");
        assert_eq!(generator.next(&[]), GeneratorStep::Complete);

        assert!(registry.build("no-such-format", &cfg).is_none());
    }

    #[test]
    fn registry_rounds_param_pre_queues_full_rounds() {
        let mut registry = FormatRegistry::new();
        ZippyQ::register(&mut registry);
        // `rounds=1` pre-queues one full-field round so the built generator emits at once.
        let cfg = FormatConfig::new(field(&["A", "B"])).with_param("rounds", "1");
        let mut generator = registry.build(ZippyQ::NAME, &cfg).unwrap();
        assert_eq!(
            generator.next(&[]),
            GeneratorStep::Run(vec![HeatPlan::new("round-1", field(&["A", "B"]))])
        );
    }
}
