//! The format / generator interface — competition structure as a pluggable
//! registry (race-engine.html §3).
//!
//! # A format is a function of state, not a fixed schedule (RE §3)
//!
//! A *format* decides what races a competition runs and in what order: a qualifying
//! format runs rounds and aggregates flights into a ranking; a bracket format emits
//! heats and advances seeds toward a winner; ZippyQ / rolling adds rounds on demand.
//! Rather than precompute a schedule, every format implements **one** contract — the
//! [`Generator`] trait — that, given the field, its config, and the results of the
//! heats run so far, does three things (RE §3):
//!
//! 1. **emit the next heat(s)** to run, or
//! 2. **declare the format complete**, and
//! 3. at any point **expose the current ranking**.
//!
//! That single contract drives the heat loop until a format finishes — `run → result
//! → advance → run` (RE §5). The honesty-forcing case the trait is designed against
//! is the **dynamic** one (ZippyQ): "produce more heats from current state" is the
//! general shape; a fixed bracket is just a generator that happens to emit a
//! predetermined sequence.
//!
//! # Determinism & recorded outcomes (RE §6)
//!
//! The engine is a **pure function of the log**: given the same events it produces
//! the same heats, results, and standings, which is what lets recorded sessions
//! replay. A [`Generator`] therefore reads **no clock and no RNG**. Its [`next`] and
//! [`ranking`] are deterministic given the same completed-heat history plus the
//! generator's own seeded field and config.
//!
//! Anything genuinely non-deterministic — a random seeding draw, a coin-flip
//! tie-break — is resolved **once** and its *outcome* recorded, so replay never
//! re-rolls it (RE §6). A generator receives that outcome as an injected value at
//! construction: a [`SeedingOutcome`] (a recorded permutation). The generator stores
//! it and uses it deterministically; the same outcome always yields the same heats.
//!
//! > Modelling note: the *recording* of a seeding draw as a first-class
//! > `Event` variant is deferred (E5 / #32 follow-up). For now the outcome is a value
//! > a generator is **constructed with** — sufficient for a deterministic, replayable
//! > generator and for the table tests. When the event lands, a generator is rebuilt
//! > from the log by reading that event instead of taking the value directly; the
//! > trait surface does not change.
//!
//! [`next`]: Generator::next
//! [`ranking`]: Generator::ranking
#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use gridfpv_events::{CompetitorRef, HeatId};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::scoring::HeatResult;

/// A heat the format wants run next: an id and the competitors lined up in it.
///
/// The `lineup` order is the **seeding** order the generator chose (e.g. top seed
/// first); downstream scheduling (#36) may assign seats / frequencies from it, but
/// the generator only commits to *who is in the heat and in what seed order*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeatPlan {
    /// The id this heat will carry in the log once scheduled.
    pub heat: HeatId,
    /// The competitors in the heat, in seeding order.
    pub lineup: Vec<CompetitorRef>,
}

impl HeatPlan {
    /// Build a plan from a heat id and a lineup.
    pub fn new(heat: impl Into<String>, lineup: Vec<CompetitorRef>) -> Self {
        Self {
            heat: HeatId(heat.into()),
            lineup,
        }
    }
}

/// A scored heat fed back into the generator: the heat's id and its [`HeatResult`].
///
/// This is the generator's *only* input about what happened — it consumes finished,
/// scored heats (produced by [`crate::scoring::score`]) and never raw passes. The
/// `heat` id ties the result back to the [`HeatPlan`] that produced it, so a
/// generator that emitted several heats can tell which result is which.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CompletedHeat {
    /// Which planned heat this result is for.
    pub heat: HeatId,
    /// The scored result of that heat.
    pub result: HeatResult,
}

impl CompletedHeat {
    /// Pair a heat id with its scored result.
    pub fn new(heat: impl Into<String>, result: HeatResult) -> Self {
        Self {
            heat: HeatId(heat.into()),
            result,
        }
    }
}

/// What a [`Generator`] decided to do given the heats completed so far.
///
/// Either there are more heats to run, or the format is finished. [`GeneratorStep::Run`]
/// always carries at least one [`HeatPlan`] (an empty `Run` is meaningless — a
/// generator with nothing to emit yet but not yet complete is waiting on input it
/// already has, so it either runs heats or completes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeneratorStep {
    /// Run these heats next, then feed their results back via [`Generator::next`].
    Run(Vec<HeatPlan>),
    /// The format is finished; [`Generator::ranking`] is the final ordering.
    Complete,
}

/// One competitor's place in a generator's overall ranking.
///
/// `position` is **1-based and tie-aware** with the same "competition ranking"
/// convention as [`crate::scoring::Placement`]: tied competitors share a `position`
/// and the next distinct entry skips past them (1, 2, 2, 4). Entries are returned in
/// ranking order (best first), with a total, deterministic tie-break so the order is
/// stable across runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct RankEntry {
    /// The competitor this entry ranks.
    pub competitor: CompetitorRef,
    /// 1-based overall position; tied competitors share a position.
    pub position: u32,
}

/// The unified format contract (race-engine.html §3).
///
/// A `Generator` is constructed with its **seeded field** and **config** (and any
/// recorded [`SeedingOutcome`] it needs); it then answers two questions over the
/// growing history of completed heats.
///
/// # Determinism contract (RE §6) — the load-bearing invariant
///
/// Both methods are **pure** with respect to wall-clock time and randomness:
///
/// - They read **no clock and no RNG**. Any randomness was resolved once, recorded,
///   and injected at construction (see [`SeedingOutcome`]).
/// - [`next`](Generator::next) is **deterministic** given the same `completed` history
///   *and* the generator's own seeded field / config / recorded outcomes. Calling it
///   twice with the same history yields the same [`GeneratorStep`]. The history is the
///   full set of heats completed so far (the heat loop accumulates and replays it);
///   `next` is free to be called with a growing slice each round.
/// - [`ranking`](Generator::ranking) likewise depends only on `completed` + the
///   generator's seeded state — it is the **provisional ranking** mid-format and the
///   **final ranking** once [`GeneratorStep::Complete`] is returned (RE §3, §7.4).
///
/// # Dynamic formats request rounds explicitly — never hidden nondeterminism
///
/// A dynamic format (ZippyQ) adds rounds *on demand*. That demand is an **explicit
/// input to the generator**, modelled by the implementor as a field or a method —
/// e.g. an RD "request another round" call that flips a stored flag — *not* as a
/// clock read or a coin flip inside `next`. `next` stays a pure function of (history +
/// generator state); the only thing that changed between two `next` calls that
/// produce different steps is that recorded, explicit state. See [`RollingDemo`] for a
/// worked example.
///
/// [`next`]: Generator::next
pub trait Generator {
    /// Decide what to run next given every heat completed so far.
    ///
    /// `completed` is the full history of scored heats (the heat loop accumulates it
    /// and passes the growing set each round). Returns [`GeneratorStep::Run`] with the
    /// next heats, or [`GeneratorStep::Complete`] when the format is finished.
    ///
    /// Deterministic given the same `completed` + the generator's seeded state.
    fn next(&mut self, completed: &[CompletedHeat]) -> GeneratorStep;

    /// The current overall ranking given every heat completed so far.
    ///
    /// Provisional while the format is running; the **final** ordering once `next`
    /// has returned [`GeneratorStep::Complete`]. Best-placed competitor first, ties
    /// sharing a position (see [`RankEntry`]).
    fn ranking(&self, completed: &[CompletedHeat]) -> Vec<RankEntry>;
}

// --- Advancement & seeding helpers (RE §5) ----------------------------------

/// Advance the **top `n`** competitors of a ranking: the seeds that move up a tier /
/// into the next heat (RE §5, "seeds advancing per the configured top-N advance").
///
/// Returns the first `n` competitors in ranking order (best first). `n` larger than
/// the ranking yields the whole ranking. Because [`RankEntry`] is already in a total,
/// deterministic order, the advancing set is deterministic — including across a tie at
/// the `n` boundary, where the tie's deterministic intra-group order decides who makes
/// the cut. (A format whose rules forbid splitting a tie at the cut line resolves it
/// with a recorded tie-break before calling this; this helper is the mechanical "take
/// the top n of an already-total order".)
pub fn advance_top_n(ranking: &[RankEntry], n: usize) -> Vec<CompetitorRef> {
    ranking
        .iter()
        .take(n)
        .map(|entry| entry.competitor.clone())
        .collect()
}

/// Build the next heat's lineup by **seeding from a ranking**: take the competitors in
/// `order` and lay them into a heat in that order, which *is* the seed order.
///
/// A thin convenience over [`advance_top_n`] for the common "advance the top N into
/// one heat" shape; bracket formats that pair seeds (1 v 8, 2 v 7, …) compose their
/// own lineups from [`advance_top_n`] + the seeding utilities they need.
pub fn seed_heat(heat: impl Into<String>, order: Vec<CompetitorRef>) -> HeatPlan {
    HeatPlan::new(heat, order)
}

/// Standard bracket seeding pairing: pair the strongest seed with the weakest, the
/// 2nd-strongest with the 2nd-weakest, and so on (1 v 8, 2 v 7, 3 v 6, 4 v 5).
///
/// `seeds` is in seed order (best first). Returns the bracket order — `[1, 8, 2, 7,
/// …]` — so consecutive pairs are the match-ups; an odd count leaves the middle seed
/// unpaired (a bye), placed last. A single-elimination generator (#34) uses this to
/// lay out a round from a ranking.
pub fn bracket_pairs(seeds: &[CompetitorRef]) -> Vec<CompetitorRef> {
    let mut out = Vec::with_capacity(seeds.len());
    let (mut lo, mut hi) = (0usize, seeds.len());
    while lo < hi {
        out.push(seeds[lo].clone());
        lo += 1;
        if lo < hi {
            hi -= 1;
            out.push(seeds[hi].clone());
        }
    }
    out
}

// --- Recorded outcomes (RE §6) ----------------------------------------------

/// A **recorded** seeding draw: the resolved outcome of a one-time random ordering,
/// stored so replay is deterministic (race-engine.html §6).
///
/// When a format needs to break initial symmetry randomly (e.g. drawing the starting
/// order of an otherwise-unseeded field), the randomness is rolled **once** and the
/// resulting permutation is recorded here. A [`Generator`] is constructed with this
/// value and applies it deterministically: same outcome → same heats, every replay.
///
/// The outcome is a permutation expressed as the **drawn order of the field**: it
/// lists the competitors in the order the draw placed them. [`apply`](SeedingOutcome::apply)
/// reorders any matching field by it.
///
/// > This is a plain value, not yet an [`gridfpv_events::Event`] — the event variant
/// > that *records* a draw in the log is E5 (#32 follow-up). The shape here is what
/// > that event will carry, so promoting it later does not change the generator API.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SeedingOutcome {
    /// The field in drawn order — the recorded permutation.
    pub drawn_order: Vec<CompetitorRef>,
}

impl SeedingOutcome {
    /// An identity outcome: no draw was made, the field keeps its given order.
    pub fn identity() -> Self {
        Self {
            drawn_order: Vec::new(),
        }
    }

    /// Record a draw as the field laid out in the order it was drawn.
    pub fn drawn(drawn_order: Vec<CompetitorRef>) -> Self {
        Self { drawn_order }
    }

    /// Apply the recorded draw to `field`, returning the field in drawn order.
    ///
    /// Competitors present in [`drawn_order`](SeedingOutcome::drawn_order) come first,
    /// in the drawn order; any competitor in `field` the draw did not mention follows
    /// in `field`'s original relative order (so an extended field still produces a
    /// total, deterministic ordering). An [`identity`](SeedingOutcome::identity)
    /// outcome returns `field` unchanged.
    pub fn apply(&self, field: &[CompetitorRef]) -> Vec<CompetitorRef> {
        if self.drawn_order.is_empty() {
            return field.to_vec();
        }
        let mut out: Vec<CompetitorRef> = self
            .drawn_order
            .iter()
            .filter(|c| field.contains(c))
            .cloned()
            .collect();
        for competitor in field {
            if !self.drawn_order.contains(competitor) {
                out.push(competitor.clone());
            }
        }
        out
    }
}

// --- Format registry (RE §3) ------------------------------------------------

/// A format's construction config: a generic, simple bag of named values.
///
/// Mirrors the adapter registry's "name + config" shape. Concrete formats (#33–#35)
/// define their own typed config and parse it from this; keeping the registry's config
/// stringly-typed here keeps the registry itself format-agnostic. The seeded `field`
/// is carried alongside because every format needs it.
#[derive(Debug, Clone, Default)]
pub struct FormatConfig {
    /// The seeded field the format runs over, in seed order (best seed first), or in
    /// draw order once a [`SeedingOutcome`] has been applied.
    pub field: Vec<CompetitorRef>,
    /// Named scalar parameters (e.g. `"rounds" -> "3"`, `"advance" -> "2"`). A
    /// concrete format reads the keys it understands.
    pub params: BTreeMap<String, String>,
    /// A recorded seeding draw, if the format's construction needs one (RE §6).
    pub seeding: SeedingOutcome,
}

impl FormatConfig {
    /// A config over `field` with no params and no seeding draw.
    pub fn new(field: Vec<CompetitorRef>) -> Self {
        Self {
            field,
            params: BTreeMap::new(),
            seeding: SeedingOutcome::identity(),
        }
    }

    /// Set a named parameter (builder style).
    pub fn with_param(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.params.insert(key.into(), value.into());
        self
    }

    /// Attach a recorded seeding draw (builder style).
    pub fn with_seeding(mut self, seeding: SeedingOutcome) -> Self {
        self.seeding = seeding;
        self
    }

    /// Read a named parameter as a `usize`, falling back to `default` if absent or
    /// unparseable. The simple accessor concrete formats use for their numeric config.
    pub fn param_usize(&self, key: &str, default: usize) -> usize {
        self.params
            .get(key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }
}

/// A constructor that builds a boxed [`Generator`] from a [`FormatConfig`].
pub type FormatCtor = fn(&FormatConfig) -> Box<dyn Generator>;

/// A registry mapping a **format name** to a [`Generator`] constructor — the
/// competition-structure analogue of the adapter registry (RE §3).
///
/// The Director picks a format by name; the registry turns that name + a
/// [`FormatConfig`] into a live `Box<dyn Generator>`. Formats register themselves here
/// (#33 timed-qual, #34 single-elim, #35 ZippyQ each add a `register` entry); the heat
/// loop only ever sees the trait object.
#[derive(Default)]
pub struct FormatRegistry {
    ctors: BTreeMap<String, FormatCtor>,
}

impl FormatRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self {
            ctors: BTreeMap::new(),
        }
    }

    /// Register a format constructor under `name`. A later registration under the same
    /// name replaces the earlier one (last write wins).
    pub fn register(&mut self, name: impl Into<String>, ctor: FormatCtor) {
        self.ctors.insert(name.into(), ctor);
    }

    /// A registry pre-populated with every **production** format the engine ships:
    /// [`timed_qual`](crate::timed_qual::TimedQualifying), [`zippyq`](crate::zippyq::ZippyQ),
    /// [`single_elim`](crate::single_elim::SingleElim),
    /// [`double_elim`](crate::double_elim::DoubleElim),
    /// [`round_robin`](crate::round_robin::RoundRobin), and
    /// [`multi_main`](crate::multi_main::MultiMain).
    ///
    /// This is the single authority for "which format names are valid": the server validates a
    /// round's configured format name against [`names`](Self::names) / [`contains`](Self::contains)
    /// of this registry. The `*-demo` formats in this module are test fixtures and are deliberately
    /// **not** registered here.
    pub fn standard() -> Self {
        let mut registry = Self::new();
        crate::timed_qual::TimedQualifying::register(&mut registry);
        crate::zippyq::ZippyQ::register(&mut registry);
        crate::single_elim::SingleElim::register(&mut registry);
        crate::double_elim::DoubleElim::register(&mut registry);
        crate::round_robin::RoundRobin::register(&mut registry);
        crate::multi_main::MultiMain::register(&mut registry);
        registry
    }

    /// The format names registered, in sorted order.
    pub fn names(&self) -> Vec<&str> {
        self.ctors.keys().map(String::as_str).collect()
    }

    /// Whether a format is registered under `name`.
    pub fn contains(&self, name: &str) -> bool {
        self.ctors.contains_key(name)
    }

    /// Build a generator for the format registered under `name`, or `None` if no such
    /// format is registered.
    pub fn build(&self, name: &str, config: &FormatConfig) -> Option<Box<dyn Generator>> {
        self.ctors.get(name).map(|ctor| ctor(config))
    }
}

// --- Ranking aggregation helper ---------------------------------------------

/// Build a [`RankEntry`] list from `(competitor, rank_key)` rows, smaller key = better.
///
/// The general "turn a scored ordering into a tie-aware ranking" used by generators
/// that aggregate completed heats into an overall standing. Rows are sorted by
/// `(rank_key, competitor)` so the competitor ref is the final, total tie-break;
/// competitors whose `rank_key` is **equal** share a `position`, with the next distinct
/// group skipping past them (1, 2, 2, 4) — mirroring [`crate::scoring`]'s convention.
pub fn rank_by<K: Ord + Clone>(rows: Vec<(CompetitorRef, K)>) -> Vec<RankEntry> {
    let mut rows = rows;
    rows.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));

    let mut out = Vec::with_capacity(rows.len());
    let mut prev_key: Option<K> = None;
    let mut position = 0u32;
    for (index, (competitor, key)) in rows.into_iter().enumerate() {
        if prev_key.as_ref() != Some(&key) {
            position = (index as u32) + 1;
            prev_key = Some(key.clone());
        }
        out.push(RankEntry {
            competitor,
            position,
        });
    }
    out
}

// --- Demo generators (exercise the contract) --------------------------------

/// A trivial **fixed-schedule** demo: a two-round seeded knockout that exercises the
/// whole contract — emit heats, advance top-N, complete, and final ranking, with a
/// recorded seeding draw applied at construction.
///
/// Rules (deliberately tiny, just to drive the trait):
///
/// 1. **Round 1** — all `field` competitors (in *drawn* order, after applying the
///    recorded [`SeedingOutcome`]) fly one heat.
/// 2. **Round 2 (final)** — the top `advance` of round 1 fly one decider heat.
/// 3. **Complete** — final ranking is round 2's order, then the round-1 also-rans
///    behind them.
///
/// It shows the fixed case is "a generator that happens to emit a predetermined
/// sequence" (RE §3): `next` still derives each step from the completed history, it
/// just never adds a round beyond the two its config fixes.
pub struct KnockoutDemo {
    /// The field in seed/draw order (the recorded outcome already applied).
    field: Vec<CompetitorRef>,
    /// How many advance from round 1 to the final.
    advance: usize,
}

impl KnockoutDemo {
    /// The format name this registers under.
    pub const NAME: &'static str = "knockout-demo";

    const ROUND_1: &'static str = "r1";
    const FINAL: &'static str = "final";

    /// Build from a field and an advance count, applying no draw (identity seeding).
    pub fn new(field: Vec<CompetitorRef>, advance: usize) -> Self {
        Self { field, advance }
    }

    /// The registry constructor: reads `field`, the recorded `seeding` draw, and the
    /// `advance` param (default 2).
    pub fn from_config(config: &FormatConfig) -> Box<dyn Generator> {
        let field = config.seeding.apply(&config.field);
        let advance = config.param_usize("advance", 2);
        Box::new(Self { field, advance })
    }

    /// Register this demo format under [`NAME`](Self::NAME).
    pub fn register(registry: &mut FormatRegistry) {
        registry.register(Self::NAME, Self::from_config);
    }

    /// The competitors advancing from round 1, in finishing order.
    fn advancers(&self, r1: &CompletedHeat) -> Vec<CompetitorRef> {
        let ranking = result_ranking(&r1.result);
        advance_top_n(&ranking, self.advance)
    }
}

impl Generator for KnockoutDemo {
    fn next(&mut self, completed: &[CompletedHeat]) -> GeneratorStep {
        match completed.len() {
            // Nothing run yet: emit round 1 with the whole (drawn) field.
            0 => GeneratorStep::Run(vec![HeatPlan::new(Self::ROUND_1, self.field.clone())]),
            // Round 1 done: emit the final with the top `advance`.
            1 => {
                let finalists = self.advancers(&completed[0]);
                GeneratorStep::Run(vec![HeatPlan::new(Self::FINAL, finalists)])
            }
            // Final done: the format is complete.
            _ => GeneratorStep::Complete,
        }
    }

    fn ranking(&self, completed: &[CompletedHeat]) -> Vec<RankEntry> {
        match completed {
            // Before any heat: provisional ranking is the seed order.
            [] => seed_ranking(&self.field),
            // After round 1 only: provisional ranking is round 1's order.
            [r1] => result_ranking(&r1.result),
            // After the final: finalists in final order, then the round-1 also-rans
            // (those who didn't advance) behind them, in their round-1 order.
            [r1, final_heat, ..] => {
                let final_order = result_ranking(&final_heat.result);
                let advanced: Vec<CompetitorRef> = self.advancers(r1);
                let mut rows: Vec<(CompetitorRef, (u8, u32))> = Vec::new();
                for entry in &final_order {
                    rows.push((entry.competitor.clone(), (0, entry.position)));
                }
                for entry in result_ranking(&r1.result) {
                    if !advanced.contains(&entry.competitor) {
                        // Also-rans go in a second band, keeping their round-1 order.
                        rows.push((entry.competitor.clone(), (1, entry.position)));
                    }
                }
                rank_by(rows)
            }
        }
    }
}

/// A **dynamic** demo (the ZippyQ shape, RE §3): rounds are added **on demand**, not
/// precomputed. Each round, the whole field flies one heat; the format keeps going
/// only while the RD has *requested another round*.
///
/// The on-demand request is an **explicit input**, never hidden nondeterminism: the RD
/// calls [`request_round`](Self::request_round) (modelled here as a stored pending-round
/// counter). `next` is then a pure function of (completed history + that counter): it
/// emits a heat while rounds are pending and `Complete` once they run out. Two `next`
/// calls differ only because of that recorded, explicit state — exactly the honesty
/// the dynamic case forces (RE §3). The ranking aggregates **best lap count across all
/// rounds flown so far** (a stand-in for ZippyQ's "best flight" aggregation).
pub struct RollingDemo {
    /// The field flying each round.
    field: Vec<CompetitorRef>,
    /// Rounds the RD has requested but not yet been emitted as heats. Incremented by
    /// [`request_round`](Self::request_round); a round is "consumed" when its heat is
    /// emitted and that heat's result comes back.
    pending_rounds: usize,
}

impl RollingDemo {
    /// The format name this registers under.
    pub const NAME: &'static str = "rolling-demo";

    /// Build over `field` with no rounds yet requested. The RD must
    /// [`request_round`](Self::request_round) before any heat is emitted.
    pub fn new(field: Vec<CompetitorRef>) -> Self {
        Self {
            field,
            pending_rounds: 0,
        }
    }

    /// The registry constructor. An initial `rounds` param pre-requests that many
    /// rounds (default 1) so a freshly-built generator has something to emit; the RD
    /// can still [`request_round`](Self::request_round) for more at any time.
    pub fn from_config(config: &FormatConfig) -> Box<dyn Generator> {
        let field = config.seeding.apply(&config.field);
        let mut generator = Self::new(field);
        generator.pending_rounds = config.param_usize("rounds", 1);
        Box::new(generator)
    }

    /// Register this demo format under [`NAME`](Self::NAME).
    pub fn register(registry: &mut FormatRegistry) {
        registry.register(Self::NAME, Self::from_config);
    }

    /// **The explicit "add a round on demand" input** (RE §3). The RD calls this to
    /// queue another round; the next [`Generator::next`] then emits its heat. This is
    /// the only thing that makes the dynamic format produce more heats — there is no
    /// clock or RNG inside `next`.
    pub fn request_round(&mut self) {
        self.pending_rounds += 1;
    }

    /// Heat id for round `n` (1-based).
    fn round_id(n: usize) -> String {
        format!("round-{n}")
    }
}

impl Generator for RollingDemo {
    fn next(&mut self, completed: &[CompletedHeat]) -> GeneratorStep {
        // Every completed heat consumed one pending round. If rounds are still pending
        // beyond those already run, emit the next one; otherwise the format is done
        // *for now* — `Complete` until the RD requests more (which the heat loop
        // observes by calling `next` again after a `request_round`).
        let rounds_run = completed.len();
        if self.pending_rounds > rounds_run {
            let next_round = rounds_run + 1;
            GeneratorStep::Run(vec![HeatPlan::new(
                Self::round_id(next_round),
                self.field.clone(),
            )])
        } else {
            GeneratorStep::Complete
        }
    }

    fn ranking(&self, completed: &[CompletedHeat]) -> Vec<RankEntry> {
        // Aggregate: each competitor's *best* (max) lap count across the rounds flown.
        // Mirrors ZippyQ's "your best flight counts" — a sensible standing from current
        // state, derived purely from the completed history.
        if completed.is_empty() {
            return seed_ranking(&self.field);
        }
        let mut best: BTreeMap<CompetitorRef, u32> = BTreeMap::new();
        // Seed every field member at 0 so a no-show still appears in the ranking.
        for competitor in &self.field {
            best.entry(competitor.clone()).or_insert(0);
        }
        for heat in completed {
            for place in &heat.result.places {
                let entry = best.entry(place.competitor.competitor.clone()).or_insert(0);
                *entry = (*entry).max(place.laps);
            }
        }
        // Rank key: negate laps so more laps = smaller key = better.
        let rows = best
            .into_iter()
            .map(|(competitor, laps)| (competitor, -(laps as i64)))
            .collect();
        rank_by(rows)
    }
}

// --- Shared ranking adapters ------------------------------------------------

/// Turn a scored [`HeatResult`] into a generator [`RankEntry`] list: drop the scoring
/// detail (laps/metric), keep competitor + position. The result's `places` are already
/// in finishing order with shared positions, so this is a straight projection.
pub fn result_ranking(result: &HeatResult) -> Vec<RankEntry> {
    result
        .places
        .iter()
        .map(|place| RankEntry {
            competitor: place.competitor.competitor.clone(),
            position: place.position,
        })
        .collect()
}

/// A trivial 1, 2, 3, … ranking straight from a seed order — the provisional ranking a
/// generator exposes before any heat has been flown.
fn seed_ranking(field: &[CompetitorRef]) -> Vec<RankEntry> {
    field
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

    /// Build a `HeatResult` from `(name, position, laps)` rows — a hand-written scored
    /// heat for the table tests (no passes, no scorer needed).
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

    // --- advance_top_n ------------------------------------------------------

    #[test]
    fn advance_top_n_takes_the_first_n_in_order() {
        let ranking = seed_ranking(&field(&["A", "B", "C", "D"]));
        let advanced = advance_top_n(&ranking, 2);
        assert_eq!(advanced, field(&["A", "B"]));
    }

    #[test]
    fn advance_top_n_clamps_to_the_field() {
        let ranking = seed_ranking(&field(&["A", "B"]));
        assert_eq!(advance_top_n(&ranking, 5), field(&["A", "B"]));
    }

    // --- bracket_pairs ------------------------------------------------------

    #[test]
    fn bracket_pairs_seeds_strong_against_weak() {
        // 1v8, 2v7, 3v6, 4v5.
        let seeds = field(&["1", "2", "3", "4", "5", "6", "7", "8"]);
        assert_eq!(
            bracket_pairs(&seeds),
            field(&["1", "8", "2", "7", "3", "6", "4", "5"])
        );
    }

    #[test]
    fn bracket_pairs_odd_count_leaves_middle_seed_a_bye_last() {
        let seeds = field(&["1", "2", "3", "4", "5"]);
        // 1v5, 2v4, then 3 alone (the bye), placed last.
        assert_eq!(bracket_pairs(&seeds), field(&["1", "5", "2", "4", "3"]));
    }

    // --- SeedingOutcome (recorded draw) -------------------------------------

    #[test]
    fn seeding_identity_keeps_field_order() {
        let f = field(&["A", "B", "C"]);
        assert_eq!(SeedingOutcome::identity().apply(&f), f);
    }

    #[test]
    fn seeding_applies_recorded_permutation() {
        let f = field(&["A", "B", "C", "D"]);
        let outcome = SeedingOutcome::drawn(field(&["C", "A", "D", "B"]));
        assert_eq!(outcome.apply(&f), field(&["C", "A", "D", "B"]));
    }

    #[test]
    fn seeding_appends_undrawn_field_members_in_order() {
        // The draw only mentions some of the field; the rest follow in field order.
        let f = field(&["A", "B", "C", "D"]);
        let outcome = SeedingOutcome::drawn(field(&["C", "A"]));
        assert_eq!(outcome.apply(&f), field(&["C", "A", "B", "D"]));
    }

    #[test]
    fn recorded_outcome_makes_seeding_deterministic() {
        // Same recorded outcome → same drawn order, every time (replay safety).
        let f = field(&["A", "B", "C", "D"]);
        let outcome = SeedingOutcome::drawn(field(&["D", "C", "B", "A"]));
        let once = outcome.apply(&f);
        let twice = outcome.apply(&f);
        assert_eq!(once, twice);
        assert_eq!(once, field(&["D", "C", "B", "A"]));
    }

    // --- rank_by ------------------------------------------------------------

    #[test]
    fn rank_by_shares_positions_on_ties_competition_style() {
        // A and B tie on key 0, C on key 1: positions 1, 1, 3.
        let rows = vec![(cref("B"), 0u32), (cref("A"), 0u32), (cref("C"), 1u32)];
        let ranked = rank_by(rows);
        // Within the tie, competitor ref is the final tie-break: A before B.
        assert_eq!(names(&ranked), vec!["A", "B", "C"]);
        assert_eq!(ranked[0].position, 1);
        assert_eq!(ranked[1].position, 1);
        assert_eq!(ranked[2].position, 3);
    }

    // --- KnockoutDemo: the fixed-schedule contract --------------------------

    #[test]
    fn knockout_emits_round_one_with_the_whole_field_first() {
        let mut generator = KnockoutDemo::new(field(&["A", "B", "C", "D"]), 2);
        let step = generator.next(&[]);
        assert_eq!(
            step,
            GeneratorStep::Run(vec![HeatPlan::new("r1", field(&["A", "B", "C", "D"]))])
        );
    }

    #[test]
    fn knockout_advances_top_n_into_the_final() {
        let mut generator = KnockoutDemo::new(field(&["A", "B", "C", "D"]), 2);
        // Round 1: C won, A second, B third, D fourth.
        let r1 = CompletedHeat::new(
            "r1",
            result(&[("C", 1, 5), ("A", 2, 4), ("B", 3, 3), ("D", 4, 2)]),
        );
        let step = generator.next(&[r1]);
        // The final lines up the top 2 of round 1 in finishing order: C then A.
        assert_eq!(
            step,
            GeneratorStep::Run(vec![HeatPlan::new("final", field(&["C", "A"]))])
        );
    }

    #[test]
    fn knockout_completes_after_the_final_with_a_full_ranking() {
        let mut generator = KnockoutDemo::new(field(&["A", "B", "C", "D"]), 2);
        let r1 = CompletedHeat::new(
            "r1",
            result(&[("C", 1, 5), ("A", 2, 4), ("B", 3, 3), ("D", 4, 2)]),
        );
        // Final: A beat C.
        let fin = CompletedHeat::new("final", result(&[("A", 1, 6), ("C", 2, 5)]));
        let completed = vec![r1, fin];

        // After both heats, the format is complete.
        assert_eq!(generator.next(&completed), GeneratorStep::Complete);

        // Final ranking: finalists in final order (A, C), then the round-1 also-rans
        // (B, then D) behind them.
        let ranking = generator.ranking(&completed);
        assert_eq!(names(&ranking), vec!["A", "C", "B", "D"]);
        assert_eq!(ranking[0].position, 1);
        assert_eq!(ranking[3].position, 4);
    }

    #[test]
    fn knockout_seeding_draw_is_deterministic_same_outcome_same_heats() {
        // Two generators built from the SAME recorded draw emit identical round-1 heats.
        let cfg = FormatConfig::new(field(&["A", "B", "C", "D"]))
            .with_seeding(SeedingOutcome::drawn(field(&["D", "B", "A", "C"])))
            .with_param("advance", "2");

        let mut g1 = KnockoutDemo::from_config(&cfg);
        let mut g2 = KnockoutDemo::from_config(&cfg);

        let s1 = g1.next(&[]);
        let s2 = g2.next(&[]);
        assert_eq!(s1, s2);
        // Round 1 flies the field in the DRAWN order, not the config order.
        assert_eq!(
            s1,
            GeneratorStep::Run(vec![HeatPlan::new("r1", field(&["D", "B", "A", "C"]))])
        );
    }

    #[test]
    fn knockout_provisional_ranking_tracks_state() {
        let mut generator = KnockoutDemo::new(field(&["A", "B", "C"]), 2);
        // Before any heat: seed order.
        assert_eq!(names(&generator.ranking(&[])), vec!["A", "B", "C"]);
        // After round 1: round-1 order.
        let r1 = CompletedHeat::new("r1", result(&[("B", 1, 4), ("C", 2, 3), ("A", 3, 2)]));
        let _ = generator.next(std::slice::from_ref(&r1));
        assert_eq!(names(&generator.ranking(&[r1])), vec!["B", "C", "A"]);
    }

    // --- RollingDemo: the dynamic contract ----------------------------------

    #[test]
    fn rolling_emits_nothing_until_a_round_is_requested() {
        let mut generator = RollingDemo::new(field(&["A", "B"]));
        // No round requested yet → complete (nothing pending).
        assert_eq!(generator.next(&[]), GeneratorStep::Complete);
    }

    #[test]
    fn rolling_adds_a_round_on_demand_from_current_state() {
        let mut generator = RollingDemo::new(field(&["A", "B"]));
        // The RD explicitly requests a round — the only thing that makes a heat appear.
        generator.request_round();
        assert_eq!(
            generator.next(&[]),
            GeneratorStep::Run(vec![HeatPlan::new("round-1", field(&["A", "B"]))])
        );

        // Round 1 comes back; with nothing further requested, the format is done *for now*.
        let r1 = CompletedHeat::new("round-1", result(&[("A", 1, 5), ("B", 2, 3)]));
        assert_eq!(
            generator.next(std::slice::from_ref(&r1)),
            GeneratorStep::Complete
        );

        // The RD asks for another round on demand — next() now yields round 2 from the
        // current state (a heat numbered from the rounds already run), NOT a precomputed
        // schedule.
        generator.request_round();
        let step = generator.next(std::slice::from_ref(&r1));
        assert_eq!(
            step,
            GeneratorStep::Run(vec![HeatPlan::new("round-2", field(&["A", "B"]))])
        );
    }

    #[test]
    fn rolling_ranking_aggregates_best_lap_count_across_rounds() {
        let mut generator = RollingDemo::new(field(&["A", "B", "C"]));
        generator.request_round();
        generator.request_round();

        // Round 1: A 5 laps, B 3, C 4.
        let r1 = CompletedHeat::new("round-1", result(&[("A", 1, 5), ("C", 2, 4), ("B", 3, 3)]));
        // Round 2: B surges to 7 (their best flight), A 5 again, C 4.
        let r2 = CompletedHeat::new("round-2", result(&[("B", 1, 7), ("A", 2, 5), ("C", 3, 4)]));
        let completed = vec![r1, r2];

        // Best-flight aggregate: B 7, A 5, C 4 → B, A, C.
        let ranking = generator.ranking(&completed);
        assert_eq!(names(&ranking), vec!["B", "A", "C"]);
        assert_eq!(ranking[0].position, 1);
    }

    #[test]
    fn rolling_next_is_deterministic_for_the_same_state() {
        // Same pending-round state + same history → same step, every call (RE §6).
        let mut g1 = RollingDemo::new(field(&["A", "B"]));
        let mut g2 = RollingDemo::new(field(&["A", "B"]));
        g1.request_round();
        g2.request_round();
        assert_eq!(g1.next(&[]), g2.next(&[]));
    }

    // --- FormatRegistry -----------------------------------------------------

    #[test]
    fn registry_builds_registered_formats_and_rejects_unknown() {
        let mut registry = FormatRegistry::new();
        KnockoutDemo::register(&mut registry);
        RollingDemo::register(&mut registry);

        assert_eq!(registry.names(), vec!["knockout-demo", "rolling-demo"]);

        let cfg = FormatConfig::new(field(&["A", "B", "C", "D"])).with_param("advance", "2");
        let mut generator = registry
            .build(KnockoutDemo::NAME, &cfg)
            .expect("knockout-demo is registered");
        // The built generator behaves like a knockout: round 1 over the whole field.
        assert_eq!(
            generator.next(&[]),
            GeneratorStep::Run(vec![HeatPlan::new("r1", field(&["A", "B", "C", "D"]))])
        );

        assert!(registry.build("no-such-format", &cfg).is_none());
    }

    #[test]
    fn standard_registry_holds_every_production_format() {
        let registry = FormatRegistry::standard();
        assert_eq!(
            registry.names(),
            vec![
                "double_elim",
                "multi_main",
                "round_robin",
                "single_elim",
                "timed_qual",
                "zippyq",
            ]
        );
        // The validation surface the server uses.
        assert!(registry.contains("timed_qual"));
        assert!(!registry.contains("knockout-demo"));
        assert!(!registry.contains("no-such-format"));
    }

    #[test]
    fn registry_built_rolling_uses_the_rounds_param() {
        let mut registry = FormatRegistry::new();
        RollingDemo::register(&mut registry);
        // `rounds=1` pre-requests one round, so the built generator emits immediately.
        let cfg = FormatConfig::new(field(&["A", "B"])).with_param("rounds", "1");
        let mut generator = registry.build(RollingDemo::NAME, &cfg).unwrap();
        assert_eq!(
            generator.next(&[]),
            GeneratorStep::Run(vec![HeatPlan::new("round-1", field(&["A", "B"]))])
        );
    }
}
