//! The per-event, **round-driven engine** (race redesign Slice 3a) — the keystone that
//! wires the dormant format generators into a live, RD-driven flow.
//!
//! [`crate::events::RoundDef`] describes *what* a round runs (a format name, its config,
//! its eligible classes, a [`SeedingRule`](crate::events::SeedingRule)); this module turns
//! that definition into a running [`Generator`](gridfpv_engine::format::Generator) and
//! advances it **off the log**, exactly as [`gridfpv_engine::event::run_event`] does for a
//! whole event — but interactively, one [`Command::FillRound`](crate::control::Command)
//! at a time instead of in one wholesale sweep.
//!
//! # The engine is a pure function of the log + meta (RE §6)
//!
//! Nothing here persists engine state. A [`FillRound`](crate::control::Command::FillRound)
//! rebuilds the round's generator from the round's [`RoundDef`] + the event's
//! [`classes_membership`](crate::events::EventMeta::classes_membership) (the field) and
//! the round's **completed heats read back from the log**, then asks the generator what to
//! run next. So the same log + meta always yields the same next heat — the determinism the
//! generator contract demands (RE §6), and the same property
//! [`run_event`](gridfpv_engine::event::run_event) relies on when it replays a recorded
//! event.
//!
//! # How a round advances (mirrors the `run_event` loop)
//!
//! 1. **Build the field.** For a normal round the field is the eligible classes' membership
//!    (union, in roster/seed order), mapped pilot→competitor. For a
//!    [`SeedingRule::FromRanking`](crate::events::SeedingRule::FromRanking) round the field
//!    is the **top-N** of a prior round's ranking — the qualifying→bracket *carry*, reusing
//!    [`advance_top_n`](gridfpv_engine::format::advance_top_n) over that source round's
//!    ranking exactly as [`run_event`](gridfpv_engine::event::run_event)'s phase-2 seeding
//!    does.
//! 2. **Build the generator** for the field via
//!    [`FormatRegistry::build`](gridfpv_engine::format::FormatRegistry::build).
//! 3. **Read the round's completed heats from the log** — every heat tagged
//!    `round == round.id` whose result is final (it reached `Final`), scored under the
//!    round's [`win_condition`](crate::events::RoundDef::win_condition).
//! 4. **`generator.next(&completed)`** → either emit a `HeatScheduled` per plan (tagged
//!    with the round, and the class when the round is single-class) or surface *round
//!    complete*.
//!
//! Because step 3 reads the log, the advance closes through the log: when a heat reaches
//! `Final` (the existing FSM path appends `HeatStateChanged { Finalized }`), the next
//! `FillRound` sees it as a completed heat and the generator advances — including across
//! the bracket carry, where the *source* round's completed heats produce the ranking the
//! bracket seeds from.

use std::collections::{BTreeMap, BTreeSet};

use gridfpv_engine::event::score_marshaled;
use gridfpv_engine::format::{
    CompletedHeat, FormatConfig, FormatRegistry, GeneratorStep, RankEntry, advance_top_n,
};
use gridfpv_engine::heat::{HeatState, heat_state};
use gridfpv_engine::schedule::{Frequency, FrequencyPool, allocate};
use gridfpv_engine::scoring::HeatResult;
use gridfpv_events::{ClassId, CompetitorRef, Event, HeatId, Pass, RoundId, SourceTime};
use gridfpv_projection::lap_list;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::events::{ChannelMode, EventMeta, RoundDef, SeedingRule};
use crate::timers::{Timer, TimerRegistry};

/// The hard guard against a generator that never completes (mirrors
/// [`run_event`](gridfpv_engine::event::run_event)'s `max_heats`): a real format always
/// converges, so a round that has somehow already run this many heats is a logic bug, not
/// a request for another heat. `FillRound` returns it as "complete" rather than emitting an
/// unbounded heat.
const MAX_HEATS_PER_ROUND: usize = 1_000;

/// The outcome of filling a round (race redesign Slice 3a) — the typed answer to a
/// [`Command::FillRound`](crate::control::Command::FillRound).
///
/// Either the generator emitted the next heat (which the handler appends as a tagged
/// [`Event::HeatScheduled`]), or the round is finished — a *typed ok*, **not** an error
/// (an empty round is a legitimate, expected terminal state, not a failure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FillOutcome {
    /// The next heat to schedule for this round: its generated heat id and lineup. The
    /// handler appends a `HeatScheduled` tagged with `round` (and `class` when single-class).
    Scheduled {
        /// The heat id the generator chose.
        heat: HeatId,
        /// The heat lineup, in the generator's seeding order.
        lineup: Vec<CompetitorRef>,
        /// **Static-mode** per-pilot channel assignment (race redesign Slice 7a): `Some` for a
        /// [`ChannelMode::Static`](crate::events::ChannelMode::Static) round (the channel-balanced
        /// builder picks each pilot's *fixed* membership channel), `None` for a
        /// [`PerHeat`](crate::events::ChannelMode::PerHeat) round (the handler then assigns channels
        /// from the timer's pool via [`assign_for_event`], the prior behaviour). The handler still
        /// enforces the node-count cap either way.
        frequencies: Option<Vec<(CompetitorRef, u16)>>,
    },
    /// The round is complete — the generator returned
    /// [`GeneratorStep::Complete`](gridfpv_engine::format::GeneratorStep::Complete). No
    /// heat is appended; the round's final ranking is available via [`round_ranking`].
    Complete,
    /// Every heat the generator wants right now is **already scheduled** and awaiting its
    /// `Finalize`. No new heat is appended and the round is *not* finished — the RD just needs
    /// to drive the outstanding heat before the next one can be drawn. A typed ok, not an
    /// error.
    AlreadyScheduled,
}

/// An error filling a round: the round does not exist, the field is empty, the format is
/// unknown, or a `FromRanking` source round is missing/unscorable. Each maps to a
/// [`ProtocolError`](crate::error::ProtocolError) at the call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FillError {
    /// No [`RoundDef`] with that id in the event's [`rounds`](EventMeta::rounds).
    UnknownRound(String),
    /// The round's field resolved empty (no class membership, or a `FromRanking` source
    /// with no ranking yet) — there is nothing to schedule.
    EmptyField(String),
    /// The round's [`format`](RoundDef::format) is not a known
    /// [`FormatRegistry::standard`] name (should have been caught at add/update; a
    /// defensive check so a stale log can't panic).
    UnknownFormat(String),
    /// A [`SeedingRule::FromRanking`] names a source round (in its `source_rounds`) that does not
    /// exist in this event.
    UnknownSourceRound(String),
    /// A [`ChannelMode::Static`](crate::events::ChannelMode::Static) round has a member with **no
    /// assigned channel** (race redesign Slice 7a): static channel-balanced formation needs every
    /// member to carry a fixed channel. The inner `String` names the pilot missing one.
    MissingChannel(String),
}

impl std::fmt::Display for FillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FillError::UnknownRound(id) => write!(f, "no round with id {id:?} in this event"),
            FillError::EmptyField(id) => {
                write!(
                    f,
                    "round {id:?} has no field to schedule (empty membership)"
                )
            }
            FillError::UnknownFormat(fmt) => write!(f, "round uses unknown format {fmt:?}"),
            FillError::UnknownSourceRound(id) => {
                write!(
                    f,
                    "seeding source round {id:?} does not exist in this event"
                )
            }
            FillError::MissingChannel(pilot) => {
                write!(
                    f,
                    "static-channel round member {pilot:?} has no assigned channel"
                )
            }
        }
    }
}

impl std::error::Error for FillError {}

/// Per-heat **channel assignment** failed (race redesign Slice 4a): the lineup exceeds the timer's
/// node/slot count (the heat-size cap), or there are too few available channels to seat it.
///
/// A typed error the heat-build paths (`FillRound` / `ScheduleHeat`) surface as a `400` with a
/// clear message — a heat that cannot be seated on the timer must not be scheduled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssignError {
    /// The lineup is larger than the timer can physically run: `lineup` pilots, `nodes` slots.
    TooManyForNodes {
        /// Pilots in the heat's lineup.
        lineup: usize,
        /// The timer's node/slot count (the cap).
        nodes: usize,
    },
    /// The lineup fits the node count, but the timer's **available channels** are too few to give
    /// every pilot a distinct channel: `lineup` pilots, `available` channels.
    TooFewChannels {
        /// Pilots in the heat's lineup.
        lineup: usize,
        /// Distinct available channels the timer offered.
        available: usize,
    },
}

impl std::fmt::Display for AssignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssignError::TooManyForNodes { lineup, nodes } => write!(
                f,
                "heat lineup of {lineup} exceeds the timer's {nodes} node(s)"
            ),
            AssignError::TooFewChannels { lineup, available } => write!(
                f,
                "the timer offers only {available} channel(s) for a {lineup}-pilot heat"
            ),
        }
    }
}

impl std::error::Error for AssignError {}

/// Assign **video channels** to a heat's lineup from a timer's available channels (race redesign
/// Slice 4a; IMD-aware selection #209) — the engine's half of the RE §7.3 split (the engine
/// allocates; the adapter applies).
///
/// Given the event's selected `timer` and the heat's `lineup` (in seed order):
///
/// 1. **Heat-size cap.** The lineup must be ≤ the timer's
///    [`node_count`](crate::timers::Timer::node_count); otherwise [`AssignError::TooManyForNodes`].
///    A timer with **no available channels** (a sim/Mock-without-frequencies, an unconfigured
///    timer) assigns **nothing** — an empty allocation — *after* the cap check, so a heat that is
///    simply un-channelled is fine but an oversized one is still rejected.
/// 2. **IMD-aware channel SELECTION (#209 auto-pick).** Rather than first-fitting the pool's first
///    `lineup.len()` channels, pick the **cleanest** size-`lineup.len()` *subset* of the timer's
///    available channels by third-order intermodulation
///    ([`pick_best_imd_set`](gridfpv_engine::imd::pick_best_imd_set)). IMD only matters for the
///    channels flying **simultaneously in this heat**, so the channel *set* is chosen for the
///    heat's lineup size — products landing on/near a used channel cause video breakup, so the
///    subset that keeps the worst product farthest from every used channel is chosen. Too few
///    available channels for the lineup is [`AssignError::TooFewChannels`].
/// 3. **First-fit assignment of the chosen set.** The IMD-best channels (sorted ascending) are
///    laid onto the lineup in seed order via [`allocate`] — top seed gets the lowest chosen
///    channel, etc. — so the per-pilot mapping is deterministic.
///
/// Pure and deterministic: the same lineup + timer config always yields the same per-pilot
/// `(competitor, mhz)` assignment — no clock, no RNG — the determinism `HeatScheduled.frequencies`
/// and the e2e rely on (the fill is replay-deterministic).
pub fn assign_frequencies(
    timer: &Timer,
    lineup: &[CompetitorRef],
) -> Result<Vec<(CompetitorRef, u16)>, AssignError> {
    let nodes = timer.node_count as usize;
    if lineup.len() > nodes {
        return Err(AssignError::TooManyForNodes {
            lineup: lineup.len(),
            nodes,
        });
    }
    // No available channels ⇒ no channel assignment (sim/Mock-without-frequencies, unconfigured).
    // The cap above still applies; only the channel step is skipped.
    if timer.available_channels.is_empty() {
        return Ok(Vec::new());
    }
    // The candidate pool is the available channels, but never more than the timer has nodes for (a
    // node can't run two channels). De-duplicate first so the IMD picker chooses among distinct
    // channels and the TooFewChannels count below is the real distinct supply.
    let mut pool: Vec<u16> = Vec::new();
    for &ch in timer.available_channels.iter().take(nodes) {
        if !pool.contains(&ch) {
            pool.push(ch);
        }
    }
    if lineup.len() > pool.len() {
        return Err(AssignError::TooFewChannels {
            lineup: lineup.len(),
            available: pool.len(),
        });
    }

    // #209 auto-pick: choose the IMD-cleanest size-`lineup.len()` subset of the candidate pool for
    // this heat's *simultaneous* lineup, then first-fit those channels onto the seed-ordered lineup.
    // NOTE(#209): this is the **auto-pick** half only. The remaining halves stay on the roadmap —
    // surfacing the heat's IMD score in the UI and flagging a low-IMD heat for the RD. Channels are
    // now IMD-optimised at fill; the score display + low-IMD flag are not yet wired.
    // `pick_best_imd_set` is deterministic (tie-broken by widest spread then lowest channels), so
    // the assignment is replay-deterministic. A manual per-heat channel override (if any) is applied
    // by the caller, which wins over this auto-pick.
    let best = gridfpv_engine::imd::pick_best_imd_set(&pool, lineup.len());
    let chosen_pool = FrequencyPool::new(best.into_iter().map(Frequency::new));
    match allocate(lineup, &chosen_pool) {
        Ok(assignment) => Ok(assignment
            .into_iter()
            .map(|(competitor, freq)| (competitor, freq.mhz))
            .collect()),
        Err(e) => Err(AssignError::TooFewChannels {
            lineup: e.needed,
            available: e.available,
        }),
    }
}

/// The timer a heat's channels are assigned from for an event (race redesign Slice 4a): the event's
/// **effective primary** timer (its override, else the first selected), resolved in `timers`.
///
/// `None` when the event selects no timer, or the selected timer is not in the registry — the
/// caller then assigns no channels (an un-channelled heat, e.g. a pure-sim event). The effective
/// primary mirrors the source bridge's selection, so the channels a heat is assigned match the
/// timer it will actually fly on.
pub fn assignment_timer(meta: &EventMeta, timers: &TimerRegistry) -> Option<Timer> {
    let primary = meta.effective_primary()?;
    timers.get(&primary)
}

/// Assign channels to `lineup` for `meta`'s event using its effective primary timer (race redesign
/// Slice 4a). When the event has a resolvable timer, delegate to [`assign_frequencies`] (cap +
/// first-fit); when it has none, the heat carries no channels (an empty assignment — a pure-sim
/// event has no node cap beyond the format).
pub fn assign_for_event(
    meta: &EventMeta,
    timers: &TimerRegistry,
    lineup: &[CompetitorRef],
) -> Result<Vec<(CompetitorRef, u16)>, AssignError> {
    match assignment_timer(meta, timers) {
        Some(timer) => assign_frequencies(&timer, lineup),
        None => Ok(Vec::new()),
    }
}

/// Resolve a round by id in the event meta, or [`FillError::UnknownRound`].
fn round_of<'a>(meta: &'a EventMeta, round: &RoundId) -> Result<&'a RoundDef, FillError> {
    meta.rounds
        .iter()
        .find(|r| &r.id == round)
        .ok_or_else(|| FillError::UnknownRound(round.0.clone()))
}

/// Whether a round is **single-class** — exactly one eligible class, so its scheduled heats
/// are tagged with that class. A many/all-class (open / practice) round tags no class.
fn single_class(round: &RoundDef) -> Option<ClassId> {
    match round.classes.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    }
}

/// Build a round's **field** as engine [`CompetitorRef`]s (race redesign Slice 3a).
///
/// - [`SeedingRule::FromRoster`] (the default): the union of the eligible classes'
///   [`classes_membership`](EventMeta::classes_membership), in class-selection then
///   membership (roster/seed) order, de-duplicated so a pilot in two eligible classes
///   appears once. Each pilot id maps straight to a [`CompetitorRef`] of the same string
///   — the handle the lineup carries and the timer emits passes for.
/// - [`SeedingRule::FromRanking`]: the **top-N** of the source rounds' **aggregated** ranking (the
///   qualifying→bracket carry). For a single source round this is exactly that round's ranking; for
///   several (issue #51 multi-select) the rankings are merged **best-per-pilot** by
///   [`aggregate_rankings`] before taking the top-N — exactly the phase-2 seeding
///   [`run_event`](gridfpv_engine::event::run_event) does over the combined field.
/// - [`SeedingRule::FromHeatWinners`]: the source (bracket-level) round's **heat winners**, in heat
///   order — the bracket **advancement** carry (decisions D13, #217). See [`heat_winners`].
fn round_field(
    meta: &EventMeta,
    round: &RoundDef,
    events: &[Event],
) -> Result<Vec<CompetitorRef>, FillError> {
    match &round.seeding {
        SeedingRule::FromRoster => {
            let mut field: Vec<CompetitorRef> = Vec::new();
            for class in &round.classes {
                if let Some(membership) = meta.classes_membership.iter().find(|m| &m.class == class)
                {
                    for slot in &membership.pilots {
                        let competitor = CompetitorRef(slot.pilot.0.clone());
                        if !field.contains(&competitor) {
                            field.push(competitor);
                        }
                    }
                }
            }
            Ok(field)
        }
        SeedingRule::FromRanking {
            source_rounds,
            top_n,
        } => {
            // Compute each source round's ranking (the same provisional-or-final ranking the engine
            // seeds a single-source bracket from), then merge them best-per-pilot into one ranking.
            let mut rankings: Vec<Vec<RankEntry>> = Vec::with_capacity(source_rounds.len());
            for source_id in source_rounds {
                let source = round_of(meta, source_id)
                    .map_err(|_| FillError::UnknownSourceRound(source_id.0.clone()))?;
                rankings.push(round_ranking(meta, source, events)?);
            }
            let merged = aggregate_rankings(&rankings);
            Ok(advance_top_n(&merged, *top_n))
        }
        // Bracket advancement (decisions D13, #217): the field is the source level's **heat
        // winners** — the competitors that advanced out of each heat, in heat order. This is how a
        // single-elimination bracket advances round-to-round under the level-per-round model: the
        // next level is a new round seeded from the prior level's winners.
        SeedingRule::FromHeatWinners { source_round } => {
            let source = round_of(meta, source_round)
                .map_err(|_| FillError::UnknownSourceRound(source_round.0.clone()))?;
            Ok(heat_winners(meta, source, events)?)
        }
        // Open practice (open-practice format): the field is the active **channels**, each node
        // index laid out as a `node-{i}` competitor ref (the timer-seat handle) in the given order.
        // No pilots, no membership — laps are tracked per channel live in memory (not logged).
        SeedingRule::AllChannels { channels } => Ok(channels
            .iter()
            .map(|i| CompetitorRef(format!("node-{i}")))
            .collect()),
    }
}

/// The **heat winners** of a (bracket-level) source round, in heat order — the field a
/// [`SeedingRule::FromHeatWinners`] successor level seeds from (decisions D13, #217).
///
/// A single-elimination *level* is one round whose [`round_ranking`] lists its **advancers**
/// (each heat's winner — the top half — in heat order, then any bye) first, ahead of the
/// **eliminated**, who all share the single worst position (each heat's losers tie at the bottom
/// band). The winners are therefore exactly the ranking entries whose position is **strictly
/// better than that worst band**, taken in ranking (heat) order. This carries forward however many
/// heats the level had — head-to-head advances one per heat, a 4-up heat advances two — so the next
/// level's size follows the bracket rather than a fixed top-N.
///
/// Before the source level is complete its ranking is provisional, so the winners are provisional
/// too (the carry recomputes deterministically as the source level's heats finalize — the same
/// off-the-log property [`FromRanking`](SeedingRule::FromRanking) has). A source round whose
/// ranking is a single band (one competitor, or none finalized yet) advances no one.
fn heat_winners(
    meta: &EventMeta,
    source: &RoundDef,
    events: &[Event],
) -> Result<Vec<CompetitorRef>, FillError> {
    let ranking = round_ranking(meta, source, events)?;
    let Some(worst) = ranking.iter().map(|e| e.position).max() else {
        return Ok(Vec::new());
    };
    Ok(ranking
        .iter()
        .filter(|e| e.position < worst)
        .map(|e| e.competitor.clone())
        .collect())
}

/// Merge several rounds' rankings into **one aggregated ranking, best-per-pilot** (issue #51
/// multi-select seeding).
///
/// The aggregation rule: each pilot's standing in the merged ranking is their **best (lowest)
/// 1-based position across the source rounds they appear in** — e.g. a pilot who placed 3rd in one
/// qualifying round and 1st in another is seeded as a 1. A pilot is included if they ranked in *any*
/// source round; a pilot absent from a round simply does not contribute a position from it. The
/// merged competitors are then ordered by that best position (ascending), ties broken by
/// [`CompetitorRef`] string for a **total, deterministic** order (so the same logs always yield the
/// same seeding), and re-numbered with the same dense, tie-aware "1, 2, 2, 4" convention as a single
/// round's ranking. For a single source round this reproduces that round's ranking unchanged (modulo
/// the deterministic ref tie-break, which a single ranking already satisfies).
///
/// "Best position" — rather than summing points or averaging — keeps the seam **simple and
/// monotonic**: seeding a bracket from multiple qualifiers rewards a pilot's strongest qualifying
/// result, the usual "your best run carries you into the bracket" semantics, without depending on
/// how many rounds each pilot happened to enter.
fn aggregate_rankings(rankings: &[Vec<RankEntry>]) -> Vec<RankEntry> {
    // Best (lowest) position seen per competitor across all source rounds.
    let mut best: BTreeMap<CompetitorRef, u32> = BTreeMap::new();
    for ranking in rankings {
        for entry in ranking {
            best.entry(entry.competitor.clone())
                .and_modify(|p| *p = (*p).min(entry.position))
                .or_insert(entry.position);
        }
    }

    // Order by best position, then by competitor ref — a total, deterministic order. The BTreeMap
    // already yields ref order, so a stable sort by position alone keeps refs as the tie-break.
    let mut rows: Vec<(CompetitorRef, u32)> = best.into_iter().collect();
    rows.sort_by(|(a_ref, a_pos), (b_ref, b_pos)| {
        a_pos.cmp(b_pos).then_with(|| a_ref.0.cmp(&b_ref.0))
    });

    // Re-number with dense, tie-aware positions (1, 2, 2, 4): rows sharing a best position share a
    // merged position, the next distinct row skips past them.
    let mut merged = Vec::with_capacity(rows.len());
    let mut position = 0u32;
    let mut prev_best: Option<u32> = None;
    for (index, (competitor, best_pos)) in rows.into_iter().enumerate() {
        if prev_best != Some(best_pos) {
            position = index as u32 + 1;
            prev_best = Some(best_pos);
        }
        merged.push(RankEntry {
            competitor,
            position,
        });
    }
    merged
}

/// Whether `round` is an **open-practice** round (open-practice format): `format ==
/// "open_practice"` with [`SeedingRule::AllChannels`] seeding (race redesign open-practice Slice 1).
///
/// The source bridge resolves a running heat's round through this so it routes the heat's passes to
/// the in-memory per-channel accumulator (not the log); the field builder lays the channels out as
/// `node-{i}` refs. The format name *and* the seeding are both checked so a mis-tagged round (one or
/// the other but not both) is treated as a normal round, never half-open-practice.
pub fn is_open_practice(round: &RoundDef) -> bool {
    round.format == gridfpv_engine::format::OpenPractice::NAME
        && matches!(round.seeding, SeedingRule::AllChannels { .. })
}

/// The **round-scoped** heat id for an open-practice round (issue #54): `"<round_id>-heat"`.
///
/// The open-practice format generator emits a fixed `"open-practice"` heat id, so two open-practice
/// rounds in one event would both try to auto-create a heat under that one id and the second round
/// would get no distinct heat. Deriving the id from the round id makes each open-practice round's
/// heat unique while staying deterministic (the same round id always yields the same heat id, so the
/// auto-create is idempotent per round).
fn open_practice_heat_id(round_id: &RoundId) -> HeatId {
    HeatId(format!("{}-heat", round_id.0))
}

/// Build a [`FormatConfig`] for a round over `field`: the round's
/// [`params`](RoundDef::params) verbatim, identity seeding (the field is already in seed
/// order — the membership/carry decided it), and no recorded draw.
///
/// For the **qualifying formats** (`timed_qual` / `round_robin`) the cross-round ranking
/// **metric is derived from the round's [`win_condition`](RoundDef::win_condition)** rather
/// than from a separately-stored `metric` param — the qualifying metric *is* the win
/// condition, so the win condition is the single source of truth (Rounds form redesign:
/// qualifying metric is the win condition). The derived `metric` param is injected into the
/// config (overriding any stale stored value), so the generators' existing `from_config`
/// readers see the win-condition-derived metric. A non-qualifying format keeps its params
/// verbatim. See [`qual_metric_for`].
fn format_config(round: &RoundDef, field: Vec<CompetitorRef>) -> FormatConfig {
    let mut config = FormatConfig::new(field);
    config.params = round.params.clone();
    if let Some(metric) = qual_metric_for(&round.format, round.win_condition) {
        config
            .params
            .insert("metric".to_string(), metric.to_string());
    }
    config
}

/// The qualifying-generator **`metric` param derived from a round's win condition** (Rounds form
/// redesign: the qualifying metric *is* the win condition), or `None` for a non-qualifying format
/// (whose params are taken verbatim).
///
/// The win condition is the single source of truth for how the qualifying ranking aggregates:
///
/// - `timed_qual` ([`QualMetric`](gridfpv_engine::timed_qual::QualMetric)):
///   - [`WinCondition::BestLap`] → `"best-lap"` (fastest single lap),
///   - [`WinCondition::BestConsecutive`] → `"best-consecutive"` (fastest N-lap window),
///   - [`WinCondition::Timed`] (Most Laps) → `"most-laps"`,
///   - [`WinCondition::FirstToLaps`] is **not** a qualifying metric → the default `"best-lap"`.
/// - `round_robin` ([`RrMetric`](gridfpv_engine::round_robin::RrMetric)):
///   - [`WinCondition::Timed`] (Most Laps) → `"total-laps"`,
///   - every other condition → the default `"points"` standing.
fn qual_metric_for(
    format: &str,
    win_condition: gridfpv_engine::scoring::WinCondition,
) -> Option<&'static str> {
    use gridfpv_engine::scoring::WinCondition as WC;
    match format {
        "timed_qual" => Some(match win_condition {
            WC::BestConsecutive { .. } => "best-consecutive",
            WC::Timed { .. } => "most-laps",
            // Best lap and the non-qualifying First-to-N both fall to the fast-lap default.
            WC::BestLap | WC::FirstToLaps { .. } => "best-lap",
        }),
        "round_robin" => Some(match win_condition {
            WC::Timed { .. } => "total-laps",
            // Best lap / best-consecutive / first-to-N all rank by the points standing.
            _ => "points",
        }),
        _ => None,
    }
}

/// The completed heats of a round, **read back from the log** and scored under the round's
/// [`win_condition`](RoundDef::win_condition) (race redesign Slice 3a).
///
/// A heat counts as completed when it was scheduled tagged with this round *and* its folded
/// [`HeatState`] is [`Final`](HeatState::Final) (the FSM terminal the `Finalize` command
/// reaches). Each is scored via [`score_marshaled`] over the passes that heat produced (the
/// per-heat pass window, see [`heat_passes`]). The order is the order the heats were first
/// scheduled, which is the order the generator emitted them — so the history fed to
/// [`Generator::next`](gridfpv_engine::format::Generator::next) matches what
/// [`run_format`](gridfpv_engine::event::run_format) accumulated.
pub fn completed_heats(round: &RoundDef, events: &[Event]) -> Vec<CompletedHeat> {
    finalized_heat_ids(round, events)
        .into_iter()
        .map(|heat| {
            let passes = heat_passes(events, &heat);
            let race_start = passes
                .first()
                .map(|p| p.at)
                .unwrap_or_else(|| SourceTime::from_micros(0));
            // Score the corrected/marshaled view of this heat's passes under the round's
            // win condition — the same scorer `run_event` uses, so the ranking the
            // generator sees matches a wholesale run.
            let pass_events: Vec<Event> = passes.into_iter().map(Event::Pass).collect();
            let result: HeatResult = score_marshaled(&pass_events, round.win_condition, race_start);
            // The generator keys `next`/`ranking` on the heat ids it **emitted** (the unscoped
            // `se-h0`, …). A `single_elim` round's logged heats carry the round-scoped id
            // (`{round_id}-se-h0`, so two levels don't collide globally — see `fill_round_per_heat`),
            // so un-scope it back to the generator's id before feeding the history; otherwise the
            // bracket level would see no completed heats and never advance.
            let generator_id = unscope_heat_id(round, &heat);
            CompletedHeat::new(generator_id, result)
        })
        .collect()
}

/// Map a round's **logged** heat id back to the **generator's** id (the one the format emitted).
///
/// Most formats log the generator's id verbatim. A `single_elim` round scopes its per-level heat
/// ids with the round id (`{round_id}-se-h0`) so two bracket levels in one event don't collide in
/// the global heat-state fold (see [`fill_round_per_heat`]); this strips that prefix so the history
/// fed to the generator matches the ids it keyed on. The prefix is stripped only when present, so a
/// legacy/un-scoped id passes through unchanged.
fn unscope_heat_id(round: &RoundDef, heat: &HeatId) -> String {
    if round.format == gridfpv_engine::single_elim::SingleElim::NAME {
        let prefix = format!("{}-", round.id.0);
        if let Some(stripped) = heat.0.strip_prefix(&prefix) {
            return stripped.to_string();
        }
    }
    heat.0.clone()
}

/// The **finalized** heats of a round, in first-scheduled order — the heat ids both
/// [`completed_heats`] (which scores them) and the standings best-lap fold (which laps them) iterate.
///
/// A heat counts when it was scheduled tagged with this round *and* its folded
/// [`HeatState`] is [`Final`](HeatState::Final). The order is the order the heats were first
/// scheduled (repeated schedules of the same id are deduped to their first appearance), matching
/// the generator's emission order.
fn finalized_heat_ids(round: &RoundDef, events: &[Event]) -> Vec<HeatId> {
    let mut tagged: Vec<HeatId> = Vec::new();
    for event in events {
        if let Event::HeatScheduled {
            heat,
            round: Some(r),
            ..
        } = event
        {
            if r == &round.id && !tagged.contains(heat) {
                tagged.push(heat.clone());
            }
        }
    }
    tagged
        .into_iter()
        .filter(|heat| heat_state(events, heat) == Some(HeatState::Final))
        .collect()
}

/// Every competitor's **best (fastest) lap** (µs) across a round's finalized heats, keyed by
/// source-local [`CompetitorRef`] — the standings best-lap source.
///
/// For each finalized heat (the same set [`completed_heats`] scores) this folds [`heat_best_laps`]
/// — the [`lap_list`] minimum lap over the heat's passes — keeping the smallest lap per competitor.
/// Reusing the round's finalized heats + the projection's lap fold means the best lap is read from
/// exactly the laps that decided the round, independent of the round's win condition. A competitor
/// with no completed lap across the round is absent from the map.
fn round_best_laps(round: &RoundDef, events: &[Event]) -> BTreeMap<CompetitorRef, i64> {
    let mut best: BTreeMap<CompetitorRef, i64> = BTreeMap::new();
    for heat in finalized_heat_ids(round, events) {
        let passes = heat_passes(events, &heat);
        for (competitor, lap) in heat_best_laps(&passes) {
            best.entry(competitor)
                .and_modify(|existing| *existing = (*existing).min(lap))
                .or_insert(lap);
        }
    }
    best
}

/// The round's current **ranking** (race redesign Slice 3a): build the round's generator and
/// ask it for the ranking over the round's completed heats — provisional mid-round, final
/// once the round is [`Complete`](FillOutcome::Complete). This is what a `FromRanking`
/// successor round seeds from (the bracket carry).
pub fn round_ranking(
    meta: &EventMeta,
    round: &RoundDef,
    events: &[Event],
) -> Result<Vec<RankEntry>, FillError> {
    let field = round_field(meta, round, events)?;
    let registry = FormatRegistry::standard();
    let generator = registry
        .build(&round.format, &format_config(round, field))
        .ok_or_else(|| FillError::UnknownFormat(round.format.clone()))?;
    let completed = completed_heats(round, events);
    Ok(generator.ranking(&completed))
}

// --- Per-class standings (race redesign Slice 5/6a) -----------------------------------------

/// One pilot's **contribution to a class's standings** (race redesign Slice 5/6a) — the
/// per-pilot / per-class row aggregated across that class's rounds (the season-join shape).
///
/// This is the row the Results UI renders per competitor: their final standing position, the
/// total points they accrued across the class's rounds, and the headline lap metrics (best lap,
/// total counted laps). It is a pure aggregate of the class's scored rounds, so it replays
/// identically off the same log + meta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ClassStanding {
    /// The competitor this standing is for (the pilot's source-local handle).
    pub competitor: CompetitorRef,
    /// 1-based overall standing position; tied competitors share a position with the same
    /// dense, tie-aware "1, 2, 2, 4" convention as [`RankEntry`].
    pub position: u32,
    /// Total **points** across the class's rounds — the sum of each round's per-pilot points,
    /// where a round awards `field_size - round_position + 1` points (a win in an N-pilot round
    /// is worth N, last is worth 1). The headline metric the standings rank on.
    pub points: u32,
    /// The competitor's **best lap** across every heat of the class's rounds, in microseconds,
    /// or `None` when they completed no lap. The qualifying-style tie-break / display metric.
    #[ts(type = "number | null")]
    pub best_lap_micros: Option<i64>,
    /// The **total counted laps** the competitor completed across the class's rounds (each
    /// round's laps under that round's win condition). A display / secondary metric.
    pub total_laps: u32,
    /// How many of the class's rounds this competitor appeared in (was ranked in) — context for
    /// the points total (a pilot who skipped a round has fewer rounds to accrue from).
    pub rounds_entered: u32,
}

/// A class's **standings** (race redesign Slice 5/6a): the ordered per-pilot rows aggregated
/// across the class's rounds, plus the class id they are for.
///
/// The season-join projection the Results screen reads: [`class_standings`] folds the log + meta
/// into one [`ClassStanding`] per competitor that raced the class, best standing first. Pure and
/// deterministic — the same log + meta always yields the same ordered standings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ClassStandings {
    /// The class these standings are for.
    pub class: ClassId,
    /// The per-pilot standings, best first (ties adjacent, sharing a position).
    pub standings: Vec<ClassStanding>,
}

/// The per-competitor running totals accumulated while folding a class's rounds.
#[derive(Default)]
struct StandingAcc {
    points: u32,
    best_lap_micros: Option<i64>,
    total_laps: u32,
    rounds_entered: u32,
}

impl StandingAcc {
    /// Fold one round's contribution for a competitor: their `points` from this round, their
    /// counted `laps`, and their `best_lap` (µs) if any. `best_lap_micros` keeps the smaller.
    fn add_round(&mut self, points: u32, laps: u32, best_lap: Option<i64>) {
        self.points += points;
        self.total_laps += laps;
        self.rounds_entered += 1;
        if let Some(lap) = best_lap {
            self.best_lap_micros = Some(match self.best_lap_micros {
                Some(existing) => existing.min(lap),
                None => lap,
            });
        }
    }
}

/// Fold the log's **standings points adjustments** (marshaling Slice 6) for the heats in
/// `class_heats` into a per-competitor signed delta: every
/// [`Penalty::PointsDeducted`](gridfpv_events::Penalty::PointsDeducted) subtracts and
/// [`PointsAdded`](gridfpv_events::Penalty::PointsAdded) adds, **unless** the
/// [`PenaltyApplied`](Event::PenaltyApplied) that carried it was reversed by a
/// [`RulingReversed`](Event::RulingReversed) targeting its offset.
///
/// Points penalties are **standings-only** (marshaling.html §3.3): they never touch the per-heat
/// lap result (the heat scorer ignores them), so this is the *one* place they land. They are
/// **scoped to the class** by `class_heats` — only a penalty recorded against a heat that belongs
/// to *this* class's rounds counts, so a points deduction in one class's heat does not leak into a
/// pilot's standings in another class they also race. Pure and order-independent — reversals are
/// gathered first, so a reversal preceding or following its penalty drops it the same way,
/// mirroring the heat scorer's adjudication fold. The delta is signed (`i64`) so additions and
/// deductions net out; the caller saturates the final total at zero.
fn points_adjustments(
    events: &[Event],
    class_heats: &BTreeSet<HeatId>,
) -> BTreeMap<CompetitorRef, i64> {
    use gridfpv_events::Penalty;

    // The offsets every `RulingReversed` targets — a points penalty at one of these is dropped.
    let reversed: BTreeSet<u64> = events
        .iter()
        .filter_map(|e| match e {
            Event::RulingReversed { target } => Some(target.0),
            _ => None,
        })
        .collect();

    let mut deltas: BTreeMap<CompetitorRef, i64> = BTreeMap::new();
    for (offset, event) in events.iter().enumerate() {
        if reversed.contains(&(offset as u64)) {
            continue; // this ruling was reversed — it contributes nothing
        }
        if let Event::PenaltyApplied {
            heat,
            competitor,
            penalty,
        } = event
        {
            // Only penalties recorded against this class's heats adjust this class's standings.
            if !class_heats.contains(heat) {
                continue;
            }
            match penalty {
                Penalty::PointsDeducted { points } => {
                    *deltas.entry(competitor.clone()).or_default() -= i64::from(*points);
                }
                Penalty::PointsAdded { points } => {
                    *deltas.entry(competitor.clone()).or_default() += i64::from(*points);
                }
                // DQ / time penalties are per-heat (the heat scorer applies them); not standings.
                Penalty::Disqualify { .. } | Penalty::TimeAdded { .. } => {}
            }
        }
    }
    deltas
}

/// The set of heat ids that belong to `class`'s rounds — the heats whose adjudications scope to
/// this class's standings. A heat belongs when it was scheduled tagged with a round eligible for
/// `class` (the same round set [`class_standings`] aggregates), or tagged directly with `class`.
fn class_heat_ids(meta: &EventMeta, class: &ClassId, events: &[Event]) -> BTreeSet<HeatId> {
    let class_rounds: BTreeSet<&gridfpv_events::RoundId> = rounds_for_class(meta, class)
        .iter()
        .map(|r| &r.id)
        .collect();
    events
        .iter()
        .filter_map(|e| match e {
            Event::HeatScheduled {
                heat,
                class: c,
                round: r,
                ..
            } if c.as_ref() == Some(class)
                || r.as_ref().is_some_and(|r| class_rounds.contains(r)) =>
            {
                Some(heat.clone())
            }
            _ => None,
        })
        .collect()
}

/// The rounds of `meta` whose eligible classes **include** `class` — the class's rounds, in the
/// order they are defined on the event. A round shared by several classes (an open / practice
/// round) contributes to each of its classes' standings.
fn rounds_for_class<'a>(meta: &'a EventMeta, class: &ClassId) -> Vec<&'a RoundDef> {
    // `crate::scope::ClassId` is a re-export of `gridfpv_events::ClassId`, so the round's eligible
    // classes and the queried class are the one type — a direct membership test.
    meta.rounds
        .iter()
        .filter(|r| r.classes.contains(class))
        .collect()
}

/// Every competitor's **best (fastest) lap** (µs) across a heat, keyed by source-local
/// [`CompetitorRef`].
///
/// Derived from the *same* corrected lap view the scorer ranks from — [`lap_list`] over the heat's
/// lap-gate passes (the marshaling-aware [`corrected_passes`](gridfpv_projection::corrected_passes)
/// fold, identical to the no-adjudications case here) — so a standing's best lap comes from exactly
/// the laps that decided the heat, with **no second lap definition**.
///
/// This deliberately does *not* read the placement [`Metric`](gridfpv_engine::scoring::Metric):
/// only a [`BestLap`](gridfpv_engine::scoring::WinCondition::BestLap) heat scores a lap *duration*
/// into its metric; a [`Timed`](gridfpv_engine::scoring::WinCondition::Timed) /
/// [`FirstToLaps`](gridfpv_engine::scoring::WinCondition::FirstToLaps) race scores a completion
/// *time*, which is not a lap duration — so reading the metric would (and did) leave
/// `best_lap_micros` null for every race round even though real per-lap durations exist. Folding
/// the lap list gives the minimum lap regardless of win condition. A competitor with no completed
/// lap is simply absent from the map.
fn heat_best_laps(passes: &[Pass]) -> BTreeMap<CompetitorRef, i64> {
    let pass_events: Vec<Event> = passes.iter().cloned().map(Event::Pass).collect();
    let laps = lap_list(&pass_events);
    laps.competitors
        .into_iter()
        .filter_map(|c| {
            let best = c.best().map(|lap| lap.duration_micros)?;
            Some((c.competitor.competitor, best))
        })
        .collect()
}

/// One competitor's **counted laps** across a heat's [`HeatResult`] (0 when absent).
fn placement_laps(result: &HeatResult, competitor: &CompetitorRef) -> u32 {
    result
        .places
        .iter()
        .find(|p| &p.competitor.competitor == competitor)
        .map(|p| p.laps)
        .unwrap_or(0)
}

/// Aggregate a **class's standings** across its rounds (race redesign Slice 5/6a) — the
/// season-join projection the Results screen reads.
///
/// For every round whose eligible classes include `class`, this:
///
/// 1. Computes that round's **ranking** via [`round_ranking`] (the same provisional-or-final
///    ranking the engine seeds `FromRanking` from), giving each entrant a round position over a
///    field of `field_size`.
/// 2. Awards **points** = `field_size - position + 1` (a win is worth the field size, last is
///    worth 1), and reads each entrant's **laps** / **best lap** off the round's scored heats
///    (each heat scored under the round's [`win_condition`](RoundDef::win_condition) via the same
///    [`completed_heats`] the engine ranks from).
/// 3. Accumulates per competitor: total points, the minimum best lap, total counted laps, and the
///    count of rounds entered.
///
/// The accumulated rows are then ordered **by points (descending)**, ties broken by best lap
/// (faster first, a competitor with no lap last), then by competitor ref for a total, deterministic
/// order. `position` is assigned 1-based and tie-aware (equal points *and* best lap share a
/// position). Pure and deterministic: the same log + meta always yields the same standings.
///
/// A `FromRanking` (bracket) round is included like any other — its ranking is its standings
/// contribution, so a class's brackets feed the season totals alongside its qual rounds. Returns an
/// error if any of the class's rounds is unscorable (an unknown format, a dangling seeding source).
pub fn class_standings(
    meta: &EventMeta,
    class: &ClassId,
    events: &[Event],
) -> Result<ClassStandings, FillError> {
    let mut acc: BTreeMap<CompetitorRef, StandingAcc> = BTreeMap::new();

    for round in rounds_for_class(meta, class) {
        let ranking = round_ranking(meta, round, events)?;
        let field_size = ranking.len() as u32;
        // The round's scored heats — the same view `round_ranking` ranked over — so the laps a
        // standing reports come from exactly the heats that decided the round position.
        let completed = completed_heats(round, events);
        // Best (fastest) lap per competitor, folded from the *same* finalized heats via the lap-list
        // projection — independent of the win condition, so a Timed / FirstToLaps race reports a
        // best lap from its real per-lap durations rather than the null its placement metric carries.
        let best_laps = round_best_laps(round, events);

        for entry in &ranking {
            // Points: a win (position 1) is worth the field size; last is worth 1.
            let points = field_size.saturating_sub(entry.position).saturating_add(1);
            // Laps for this competitor across the round's heats.
            let laps = completed
                .iter()
                .map(|heat| placement_laps(&heat.result, &entry.competitor))
                .sum();
            let best_lap = best_laps.get(&entry.competitor).copied();
            acc.entry(entry.competitor.clone())
                .or_default()
                .add_round(points, laps, best_lap);
        }
    }

    // Apply the marshaling **standings points adjustments** (Slice 6): a `PointsDeducted` /
    // `PointsAdded` penalty shifts a competitor's *season/event* points without touching their
    // per-heat lap result. Scoped to this class's heats so a deduction in one class never leaks
    // into another class the pilot also races. A deduction only applies to a competitor who
    // actually accrued round points (is in `acc`); a points award/deduction for a non-entrant is a
    // no-op here (they have no standings row to adjust). The total saturates at zero.
    let class_heats = class_heat_ids(meta, class, events);
    let adjustments = points_adjustments(events, &class_heats);
    for (competitor, delta) in adjustments {
        if let Some(a) = acc.get_mut(&competitor) {
            a.points = (i64::from(a.points) + delta).max(0) as u32;
        }
    }

    // Order the rows: most points first, then faster best lap (no-lap last), then competitor ref
    // (the BTreeMap already yields ref order, the stable final tie-break).
    let mut rows: Vec<(CompetitorRef, StandingAcc)> = acc.into_iter().collect();
    rows.sort_by(|(a_ref, a), (b_ref, b)| {
        b.points
            .cmp(&a.points)
            .then_with(|| best_lap_order(a.best_lap_micros).cmp(&best_lap_order(b.best_lap_micros)))
            .then_with(|| a_ref.0.cmp(&b_ref.0))
    });

    // Assign dense, tie-aware positions: equal points *and* best lap share a position, the next
    // distinct row skips past them (1, 2, 2, 4).
    let mut standings = Vec::with_capacity(rows.len());
    let mut position = 0u32;
    let mut prev_key: Option<(u32, Option<i64>)> = None;
    for (index, (competitor, a)) in rows.into_iter().enumerate() {
        let key = (a.points, a.best_lap_micros);
        if prev_key != Some(key) {
            position = index as u32 + 1;
            prev_key = Some(key);
        }
        standings.push(ClassStanding {
            competitor,
            position,
            points: a.points,
            best_lap_micros: a.best_lap_micros,
            total_laps: a.total_laps,
            rounds_entered: a.rounds_entered,
        });
    }

    Ok(ClassStandings {
        class: class.clone(),
        standings,
    })
}

/// A sort key that orders a best lap **faster-first**, with "no lap" ranked last: `Some(µs)` keeps
/// its value, `None` becomes `i64::MAX` so a competitor who never completed a lap sinks below every
/// competitor who did.
fn best_lap_order(best: Option<i64>) -> i64 {
    best.unwrap_or(i64::MAX)
}

/// Fill a round (race redesign Slice 3a): build its generator from the field + the round's
/// completed heats off the log, and decide the next heat to schedule.
///
/// Pure with respect to the log — it reads but never appends; appending the tagged
/// `HeatScheduled` is the control handler's job. Deterministic given the same `events` +
/// `meta`, exactly like [`Generator::next`](gridfpv_engine::format::Generator::next).
pub fn fill_round(
    meta: &EventMeta,
    timers: &TimerRegistry,
    round_id: &RoundId,
    events: &[Event],
) -> Result<FillOutcome, FillError> {
    let round = round_of(meta, round_id)?;

    // Mode-aware heat formation (race redesign Slice 7a). A **static** round (time-trial / qual)
    // forms channel-balanced heats off each member's fixed channel; a **per-heat** round (brackets)
    // runs the format generator's heats and lets the handler first-fit channels (the prior path).
    match round.channel_mode {
        ChannelMode::Static => fill_round_static(meta, timers, round, events),
        ChannelMode::PerHeat => fill_round_per_heat(meta, round, round_id, events),
    }
}

/// Fill a **per-heat** (bracket) round (race redesign Slice 7a / the original Slice 3a path): run
/// the format generator and emit the next not-yet-scheduled heat, channels assigned later by the
/// handler's first-fit. Unchanged behaviour — only extracted from [`fill_round`].
fn fill_round_per_heat(
    meta: &EventMeta,
    round: &RoundDef,
    round_id: &RoundId,
    events: &[Event],
) -> Result<FillOutcome, FillError> {
    let field = round_field(meta, round, events)?;
    if field.is_empty() {
        return Err(FillError::EmptyField(round_id.0.clone()));
    }

    let registry = FormatRegistry::standard();
    let mut generator = registry
        .build(&round.format, &format_config(round, field))
        .ok_or_else(|| FillError::UnknownFormat(round.format.clone()))?;

    let completed = completed_heats(round, events);
    if completed.len() >= MAX_HEATS_PER_ROUND {
        return Ok(FillOutcome::Complete);
    }

    match generator.next(&completed) {
        GeneratorStep::Run(plans) => {
            // The interactive flow schedules **one** heat per FillRound (the RD drives each
            // heat to Finalize before asking for the next). A generator that emits several
            // plans at once (a bracket round) still advances one heat at a time: take the
            // first not-yet-scheduled plan. Dedup against already-tagged heats so a repeated
            // FillRound before the prior heat is scored does not double-schedule it.
            // Open practice (issue #54): the format generator emits a fixed `"open-practice"` heat
            // id, which collides when an event has two open-practice rounds — both would claim the
            // same id, so the second round auto-creates no distinct heat. Scope the id to the round
            // so each open-practice round gets its own heat; other formats keep the generator's id
            // verbatim. Rewritten **before** the dedup so `already`/`scheduled_round_heats` (which
            // read the round-scoped id from the log) match it and the auto-create stays idempotent
            // per round.
            // Bracket levels (decisions D13, #217): a `single_elim` round is now **one bracket
            // level**, and its generator numbers heats per level (`se-h0`, …) — so two level
            // rounds in one event (Quarters → Semis → Final) would emit the *same* `se-h0` id and
            // collide in the global heat-state fold. Scope every per-heat bracket id to the round
            // (`{round_id}-{id}`) so each level's heats are unique, exactly as open-practice scopes
            // its single heat. `completed_heats` already filters by round tag, so the scoped id is
            // purely what makes the global heat-state lookup round-unique.
            let open_practice = is_open_practice(round);
            let mut plans = plans;
            if open_practice {
                let scoped = open_practice_heat_id(round_id);
                for plan in &mut plans {
                    plan.heat = scoped.clone();
                }
            } else if round.format == gridfpv_engine::single_elim::SingleElim::NAME {
                for plan in &mut plans {
                    plan.heat = HeatId(format!("{}-{}", round_id.0, plan.heat.0));
                }
            }
            let already: Vec<HeatId> = scheduled_round_heats(events, round_id);
            let next = plans.into_iter().find(|p| !already.contains(&p.heat));
            // Open practice (open-practice format): the heat carries **empty** frequencies — its
            // lineup is the active *channels* themselves (`node-{i}` seats), so there is nothing to
            // allocate. Force `Some(empty)` so the handler appends the logged `HeatScheduled` with no
            // frequencies regardless of the timer's channel pool.
            let open_practice_frequencies = open_practice.then(Vec::new);
            match next {
                Some(plan) => Ok(FillOutcome::Scheduled {
                    heat: plan.heat,
                    lineup: plan.lineup,
                    // Per-heat: the handler assigns channels from the timer pool (first-fit), except
                    // for open practice which carries empty frequencies (the lineup is channels).
                    frequencies: open_practice_frequencies,
                }),
                // Every plan the generator wants this step is already scheduled (the RD
                // re-issued FillRound before scoring the outstanding heat): nothing new to
                // append. Report [`AlreadyScheduled`] — a typed ok the handler answers
                // without appending, distinct from a finished round.
                None => Ok(FillOutcome::AlreadyScheduled),
            }
        }
        GeneratorStep::Complete => Ok(FillOutcome::Complete),
    }
}

/// Fill a **static** (time-trial / qual) round with **channel-balanced** heats (race redesign Slice
/// 7a).
///
/// Static rounds give each member a *fixed* channel at membership; this builds the round's full,
/// deterministic plan of channel-balanced heats — each heat draws pilots on **distinct channels**,
/// **≤ `node_count` pilots** (the node cap is the only per-heat size limit; the channel pool may be
/// larger) — then emits the next not-yet-scheduled one (one per FillRound), or
/// [`Complete`](FillOutcome::Complete) once every planned heat is scheduled. Each emitted heat
/// carries its pilots' assigned channels as `frequencies` (no first-fit).
///
/// A member with no assigned channel is a [`FillError::MissingChannel`]. An empty field is a
/// [`FillError::EmptyField`], as for per-heat.
fn fill_round_static(
    meta: &EventMeta,
    timers: &TimerRegistry,
    round: &RoundDef,
    events: &[Event],
) -> Result<FillOutcome, FillError> {
    // Gather the round's members + their fixed channels (de-duplicated across eligible classes,
    // first occurrence wins — a member in two eligible classes flies once on their channel).
    let members = static_members(meta, round)?;
    if members.is_empty() {
        return Err(FillError::EmptyField(round.id.0.clone()));
    }

    // The node cap is the event's primary timer's node count (the only per-heat size limit); with
    // no resolvable timer, fall back to seating every distinct channel in one heat (a pure-sim
    // event still channel-balances by the distinct-channel rule, just without a node cap).
    let node_cap = assignment_timer(meta, timers)
        .map(|t| t.node_count as usize)
        .filter(|n| *n > 0)
        .unwrap_or(usize::MAX);

    // How many times the whole field flies — the format's round count (e.g. `timed_qual` runs
    // `rounds` rounds). Channel-balanced heats are built per format-round so every member flies
    // each round, across the configured round count.
    let format_rounds = static_round_count(round);

    let plans = channel_balanced_plan(round, &members, node_cap, format_rounds);

    // One heat per FillRound: emit the first plan not already scheduled (dedup like per-heat).
    let already: Vec<HeatId> = scheduled_round_heats(events, &round.id);
    let next = plans.into_iter().find(|(heat, _)| !already.contains(heat));
    match next {
        Some((heat, assignment)) => {
            let lineup = assignment.iter().map(|(c, _)| c.clone()).collect();
            Ok(FillOutcome::Scheduled {
                heat,
                lineup,
                frequencies: Some(assignment),
            })
        }
        // Every planned channel-balanced heat is already scheduled → the static round is complete.
        None => Ok(FillOutcome::Complete),
    }
}

/// The round's members as `(competitor, channel)` for **static** formation (race redesign Slice 7a):
/// each member's fixed channel, de-duplicated across the round's eligible classes (first occurrence
/// wins). A member with no assigned channel is a [`FillError::MissingChannel`].
fn static_members(
    meta: &EventMeta,
    round: &RoundDef,
) -> Result<Vec<(CompetitorRef, u16)>, FillError> {
    let mut out: Vec<(CompetitorRef, u16)> = Vec::new();
    for class in &round.classes {
        let Some(membership) = meta.classes_membership.iter().find(|m| &m.class == class) else {
            continue;
        };
        for slot in &membership.pilots {
            let competitor = CompetitorRef(slot.pilot.0.clone());
            if out.iter().any(|(c, _)| c == &competitor) {
                continue;
            }
            let channel = slot
                .channel
                .ok_or_else(|| FillError::MissingChannel(slot.pilot.0.clone()))?;
            out.push((competitor, channel));
        }
    }
    Ok(out)
}

/// Build the full, deterministic **channel-balanced** heat plan for a static round (race redesign
/// Slice 7a): for each of `format_rounds` rounds, partition the members into heats where every heat
/// has **distinct channels** and **≤ `node_cap` pilots**.
///
/// The builder groups members by channel (preserving membership order within a channel), then draws
/// one pilot per distinct channel into each heat, capped at `node_cap`, until a round's members are
/// exhausted — so two pilots sharing a channel land in *different* heats and every heat is
/// channel-distinct. Heat ids are `"<round-slug>-r<round>-h<heat>"`, stable across replays.
fn channel_balanced_plan(
    round: &RoundDef,
    members: &[(CompetitorRef, u16)],
    node_cap: usize,
    format_rounds: usize,
) -> Vec<(HeatId, Vec<(CompetitorRef, u16)>)> {
    // Group members by channel, preserving order. `groups` is the distinct channels in first-seen
    // order; each holds that channel's pilots as a FIFO queue to draw from.
    let mut channels: Vec<u16> = Vec::new();
    let mut groups: BTreeMap<u16, Vec<CompetitorRef>> = BTreeMap::new();
    for (competitor, channel) in members {
        if !channels.contains(channel) {
            channels.push(*channel);
        }
        groups.entry(*channel).or_default().push(competitor.clone());
    }

    let mut plans: Vec<(HeatId, Vec<(CompetitorRef, u16)>)> = Vec::new();
    for r in 0..format_rounds.max(1) {
        // Per format-round, redraw the same channel queues so every member flies once this round.
        let mut queues: BTreeMap<u16, std::collections::VecDeque<CompetitorRef>> = groups
            .iter()
            .map(|(ch, pilots)| (*ch, pilots.iter().cloned().collect()))
            .collect();
        let mut heat_index = 0usize;
        loop {
            // One heat: draw the next available pilot from each distinct channel, in channel order,
            // up to the node cap. Channels with an empty queue are skipped.
            let mut heat: Vec<(CompetitorRef, u16)> = Vec::new();
            for channel in &channels {
                if heat.len() >= node_cap {
                    break;
                }
                if let Some(queue) = queues.get_mut(channel) {
                    if let Some(pilot) = queue.pop_front() {
                        heat.push((pilot, *channel));
                    }
                }
            }
            if heat.is_empty() {
                break; // this format-round's members are exhausted
            }
            let heat_id = HeatId(format!(
                "{}-r{}-h{}",
                slugify(&round.id.0),
                r + 1,
                heat_index + 1
            ));
            plans.push((heat_id, heat));
            heat_index += 1;
        }
    }
    plans
}

/// The number of times a static round's field flies (its format round count, race redesign Slice
/// 7a): the `rounds` param, defaulting to the qualifying formats' defaults (`timed_qual` 3,
/// `round_robin` 3), else 1. Read off the round's params verbatim.
fn static_round_count(round: &RoundDef) -> usize {
    let default = match round.format.as_str() {
        "timed_qual" | "round_robin" => 3,
        _ => 1,
    };
    round
        .params
        .get("rounds")
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
        .max(1)
}

/// Slugify a round id into a heat-id-safe stem (lowercase alnum, other runs → single `-`).
fn slugify(id: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in id.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "round".to_string()
    } else {
        trimmed.to_string()
    }
}

/// The heat ids scheduled (tagged) for a round so far, in first-scheduled order.
fn scheduled_round_heats(events: &[Event], round_id: &RoundId) -> Vec<HeatId> {
    let mut out: Vec<HeatId> = Vec::new();
    for event in events {
        if let Event::HeatScheduled {
            heat,
            round: Some(r),
            ..
        } = event
        {
            if r == round_id && !out.contains(heat) {
                out.push(heat.clone());
            }
        }
    }
    out
}

/// The single-class tag for a round's scheduled heats, if any — re-exported for the handler
/// so it tags the `HeatScheduled` consistently with [`completed_heats`]'s round filter.
pub fn round_class(meta: &EventMeta, round_id: &RoundId) -> Option<ClassId> {
    meta.rounds
        .iter()
        .find(|r| &r.id == round_id)
        .and_then(single_class)
}

/// The lap-gate **passes a heat produced**, in log order (race redesign Slice 3a).
///
/// The raw log does not tag passes with a heat, so we window them: a pass is attributed to
/// the heat that was **Running** when it was appended. We replay the log in order, tracking
/// the currently-running heat (set on a `Running` transition, cleared on any terminal /
/// off-ramp transition), and collect the passes that land while `heat` is the running one.
/// This is the same "passes are consumed only while the heat is Running" rule the heat FSM
/// states (race-engine.html §2), applied to isolate one heat's passes for scoring.
///
/// Sufficient for the sequential mock-race flow the Slice-3 e2e drives (one heat runs at a
/// time); concurrent multi-class running and a precise per-heat pass log tag are a later
/// refinement (the seam is here, not in the scorer).
fn heat_passes(events: &[Event], heat: &HeatId) -> Vec<Pass> {
    let mut running: Option<HeatId> = None;
    let mut out: Vec<Pass> = Vec::new();
    for event in events {
        match event {
            Event::HeatStateChanged {
                heat: h,
                transition,
            } => {
                use gridfpv_events::HeatTransition as T;
                match transition {
                    T::Running => running = Some(h.clone()),
                    // Any exit from Running (forward to Finished/Finalized or an off-ramp)
                    // closes that heat's pass window.
                    T::Finished | T::Finalized | T::Aborted | T::Restarted | T::Discarded
                        if running.as_ref() == Some(h) =>
                    {
                        running = None;
                    }
                    _ => {}
                }
            }
            Event::Pass(pass) if running.as_ref() == Some(heat) => {
                out.push(pass.clone());
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::StartProcedure;
    use crate::events::{
        ChannelMode, ClassMembership, EventMeta, MemberSlot, RoundDef, SeedingRule,
        default_grace_window, default_staging_timer_secs,
    };
    use crate::scope::{ClassId as ScopeClassId, EventId, PilotId};
    use gridfpv_engine::scoring::WinCondition;
    use gridfpv_events::{AdapterId, GateIndex, HeatTransition, SourceTime};
    use std::collections::BTreeMap;

    const ADAPTER: &str = "mock";

    // --- Channel assignment (race redesign Slice 4a) ---------------------------------------

    use crate::channels::RACEBAND_MHZ;
    use crate::timers::{ChannelCapability, Timer, TimerId, TimerKind, TimerStatus};

    /// A test timer with the given node count + available channels (raw MHz), flexible capability.
    fn timer_with(node_count: u32, available: Vec<u16>) -> Timer {
        Timer {
            id: TimerId("t".into()),
            name: "T".into(),
            kind: TimerKind::Mock { laps: 1, lap_ms: 1 },
            status: TimerStatus::Ready,
            channel_capability: ChannelCapability::Flexible,
            node_count,
            available_channels: available,
        }
    }

    fn lineup(names: &[&str]) -> Vec<CompetitorRef> {
        names.iter().map(|n| CompetitorRef((*n).into())).collect()
    }

    #[test]
    fn assign_picks_the_imd_best_subset_in_seed_order() {
        // #209 auto-pick: an 8-node Raceband timer no longer first-fits R1,R2,R3 (which has a
        // third-order product landing exactly on R3 — IMD score 0). It selects the IMD-cleanest
        // 3-channel subset — [5658, 5732, 5917] (score 74) — and lays it onto the seeds in order
        // (lowest channel → top seed).
        let timer = timer_with(8, RACEBAND_MHZ.to_vec());
        let assignment = assign_frequencies(&timer, &lineup(&["A", "B", "C"])).unwrap();
        assert_eq!(
            assignment,
            vec![
                (CompetitorRef("A".into()), 5658),
                (CompetitorRef("B".into()), 5732),
                (CompetitorRef("C".into()), 5917),
            ]
        );
        // The chosen set is strictly cleaner than the naive first-fit R1,R2,R3.
        let chosen: Vec<u16> = assignment.iter().map(|(_, f)| *f).collect();
        assert!(
            gridfpv_engine::imd::imd_score(&chosen)
                > gridfpv_engine::imd::imd_score(&[5658, 5695, 5732]),
            "IMD-best subset must beat first-fit"
        );
    }

    #[test]
    fn assign_is_deterministic() {
        let timer = timer_with(8, RACEBAND_MHZ.to_vec());
        let l = lineup(&["X", "Y", "Z", "W"]);
        assert_eq!(
            assign_frequencies(&timer, &l).unwrap(),
            assign_frequencies(&timer, &l).unwrap()
        );
    }

    #[test]
    fn assign_with_no_available_channels_is_empty() {
        // A sim/Mock-without-frequencies (no available channels) assigns no channels — but the cap
        // still applies (covered separately).
        let timer = timer_with(8, vec![]);
        let assignment = assign_frequencies(&timer, &lineup(&["A", "B"])).unwrap();
        assert!(assignment.is_empty());
    }

    #[test]
    fn assign_rejects_an_oversized_lineup_at_the_node_cap() {
        // A 4-node timer cannot seat a 5-pilot heat — TooManyForNodes regardless of channel count.
        let timer = timer_with(4, RACEBAND_MHZ.to_vec());
        let err = assign_frequencies(&timer, &lineup(&["A", "B", "C", "D", "E"])).unwrap_err();
        assert_eq!(
            err,
            AssignError::TooManyForNodes {
                lineup: 5,
                nodes: 4
            }
        );
    }

    #[test]
    fn assign_caps_the_pool_to_the_node_count_even_with_more_channels() {
        // 2 nodes but 8 available channels: a 3-pilot lineup is rejected at the node cap first.
        let timer = timer_with(2, RACEBAND_MHZ.to_vec());
        let err = assign_frequencies(&timer, &lineup(&["A", "B", "C"])).unwrap_err();
        assert_eq!(
            err,
            AssignError::TooManyForNodes {
                lineup: 3,
                nodes: 2
            }
        );
        // A 2-pilot lineup fits and gets the first two channels.
        let ok = assign_frequencies(&timer, &lineup(&["A", "B"])).unwrap();
        assert_eq!(ok.len(), 2);
    }

    #[test]
    fn assign_too_few_channels_within_the_node_count() {
        // 8 nodes but only 2 available channels: a 3-pilot heat fits the node cap but runs out of
        // distinct channels.
        let timer = timer_with(8, vec![5658, 5695]);
        let err = assign_frequencies(&timer, &lineup(&["A", "B", "C"])).unwrap_err();
        assert_eq!(
            err,
            AssignError::TooFewChannels {
                lineup: 3,
                available: 2
            }
        );
    }

    #[test]
    fn assign_imd_pick_is_capped_by_node_count_and_replay_deterministic() {
        // #209 auto-pick, capped by nodes + deterministic. A 4-node Raceband timer caps the candidate
        // pool to its first 4 channels (R1..R4); the IMD-best 3-subset of *that* capped pool is
        // [5658, 5695, 5769] (score 37) — chosen over the first-fit R1,R2,R3 (score 0). The node cap
        // bounds the candidate set, exactly as the prior first-fit did.
        let timer = timer_with(4, RACEBAND_MHZ.to_vec());
        let l = lineup(&["A", "B", "C"]);

        let first = assign_frequencies(&timer, &l).unwrap();
        let chosen: Vec<u16> = first.iter().map(|(_, f)| *f).collect();
        assert_eq!(
            chosen,
            vec![5658, 5695, 5769],
            "IMD-best of the node-capped pool"
        );
        assert!(
            gridfpv_engine::imd::imd_score(&chosen)
                > gridfpv_engine::imd::imd_score(&[5658, 5695, 5732]),
            "still beats the first-fit even within the node cap"
        );

        // Fold/fill twice → identical (no clock, no RNG): the assignment replays deterministically.
        let second = assign_frequencies(&timer, &l).unwrap();
        assert_eq!(first, second, "IMD assignment is replay-deterministic");
    }

    /// A timer registry with **no resolvable primary** for the per-heat tests (the meta selects no
    /// timers, so `assignment_timer` resolves `None` and static formation falls back to no node cap).
    fn no_timers() -> TimerRegistry {
        TimerRegistry::new(None, 1, 1).unwrap()
    }

    /// An event meta selecting a primary timer with `node_count` nodes and a Raceband channel pool —
    /// for the static channel-balanced formation tests (race redesign Slice 7a).
    fn meta_with_timer(
        rounds: Vec<RoundDef>,
        membership: Vec<ClassMembership>,
        node_count: u32,
    ) -> (EventMeta, TimerRegistry) {
        let timers = TimerRegistry::new(None, 1, 1).unwrap();
        let timer = timers
            .update(
                &TimerId(crate::timers::MOCK_TIMER_ID.into()),
                &crate::timers::UpdateTimerRequest {
                    node_count: Some(node_count),
                    available_channels: Some(crate::channels::RACEBAND_MHZ.to_vec()),
                    ..Default::default()
                },
            )
            .unwrap();
        let mut meta = meta_with(rounds, membership);
        meta.timers = vec![timer.id.clone()];
        meta.primary_timer = Some(timer.id);
        (meta, timers)
    }

    fn meta_with(rounds: Vec<RoundDef>, membership: Vec<ClassMembership>) -> EventMeta {
        EventMeta {
            id: EventId("e".into()),
            name: "E".into(),
            created_at: 0,
            persistent: false,
            date: None,
            location: None,
            description: None,
            organizer: None,
            timers: vec![],
            primary_timer: None,
            roster: vec![],
            classes: vec![ScopeClassId("open".into())],
            classes_membership: membership,
            rounds,
        }
    }

    fn member(class: &str, pilots: &[&str]) -> ClassMembership {
        ClassMembership {
            class: ScopeClassId(class.into()),
            pilots: pilots
                .iter()
                .map(|p| MemberSlot::new(PilotId((*p).into())))
                .collect(),
        }
    }

    /// A class membership where each pilot carries a fixed channel: `(pilot, channel)` pairs — for
    /// the static channel-balanced formation tests (race redesign Slice 7a).
    fn member_chan(class: &str, pilots: &[(&str, u16)]) -> ClassMembership {
        ClassMembership {
            class: ScopeClassId(class.into()),
            pilots: pilots
                .iter()
                .map(|(p, ch)| MemberSlot {
                    pilot: PilotId((*p).into()),
                    channel: Some(*ch),
                })
                .collect(),
        }
    }

    /// The existing per-heat (bracket-path) qual fixture — explicitly `PerHeat` so the whole-field
    /// single-heat behaviour the Slice-3 tests assert is preserved.
    fn qual_round(id: &str, class: &str) -> RoundDef {
        RoundDef {
            id: RoundId(id.into()),
            label: id.into(),
            classes: vec![ScopeClassId(class.into())],
            format: "timed_qual".into(),
            params: BTreeMap::from([("rounds".into(), "1".into())]),
            win_condition: WinCondition::BestLap,
            seeding: SeedingRule::FromRoster,
            channel_mode: ChannelMode::PerHeat,
            staging_timer_secs: default_staging_timer_secs(),
            start_procedure: StartProcedure::default(),
            grace_window: default_grace_window(),
            protest_window: gridfpv_engine::heat::ProtestWindow::Off,
            time_limit_secs: None,
        }
    }

    fn scheduled(heat: &str, round: &str, class: &str, lineup: &[&str]) -> Event {
        Event::HeatScheduled {
            heat: HeatId(heat.into()),
            lineup: lineup.iter().map(|c| CompetitorRef((*c).into())).collect(),
            class: Some(ClassId(class.into())),
            round: Some(RoundId(round.into())),
            frequencies: vec![],
        }
    }

    fn changed(heat: &str, t: HeatTransition) -> Event {
        Event::HeatStateChanged {
            heat: HeatId(heat.into()),
            transition: t,
        }
    }

    fn pass(c: &str, at: i64, seq: u64) -> Event {
        Event::Pass(Pass {
            adapter: AdapterId(ADAPTER.into()),
            competitor: CompetitorRef(c.into()),
            at: SourceTime::from_micros(at),
            sequence: Some(seq),
            gate: GateIndex::LAP,
            signal: None,
        })
    }

    /// Drive one heat from Running to Final with a set of best-lap passes, returning the
    /// events that span it (schedule is the caller's).
    fn run_heat_events(heat: &str, passes: Vec<Event>) -> Vec<Event> {
        let mut v = vec![
            changed(heat, HeatTransition::Staged),
            changed(heat, HeatTransition::Armed),
            changed(heat, HeatTransition::Running),
        ];
        v.extend(passes);
        v.push(changed(heat, HeatTransition::Finished));
        v.push(changed(heat, HeatTransition::Finalized));
        v
    }

    #[test]
    fn fill_round_builds_field_from_membership_and_emits_tagged_heat() {
        let round = qual_round("q1", "open");
        let meta = meta_with(vec![round], vec![member("open", &["A", "B", "C"])]);
        // Nothing run yet → the generator emits round 1 over the whole class membership.
        let outcome = fill_round(&meta, &no_timers(), &RoundId("q1".into()), &[]).unwrap();
        match outcome {
            FillOutcome::Scheduled { lineup, .. } => {
                assert_eq!(
                    lineup,
                    vec![
                        CompetitorRef("A".into()),
                        CompetitorRef("B".into()),
                        CompetitorRef("C".into())
                    ]
                );
            }
            other => panic!("expected a scheduled heat, got {other:?}"),
        }
    }

    #[test]
    fn fill_round_empty_membership_is_an_error() {
        let round = qual_round("q1", "open");
        let meta = meta_with(vec![round], vec![]);
        assert!(matches!(
            fill_round(&meta, &no_timers(), &RoundId("q1".into()), &[]),
            Err(FillError::EmptyField(_))
        ));
    }

    // --- Qualifying metric is derived from the win condition (Rounds form redesign) --------------

    #[test]
    fn qual_metric_for_timed_qual_maps_each_win_condition() {
        // The qualifying metric IS the win condition: each maps to the matching QualMetric string,
        // and First-to-N (not a qualifying metric) falls to the best-lap default.
        assert_eq!(
            qual_metric_for("timed_qual", WinCondition::BestLap),
            Some("best-lap")
        );
        assert_eq!(
            qual_metric_for("timed_qual", WinCondition::BestConsecutive { n: 3 }),
            Some("best-consecutive")
        );
        assert_eq!(
            qual_metric_for(
                "timed_qual",
                WinCondition::Timed {
                    window_micros: 120_000_000
                }
            ),
            Some("most-laps")
        );
        assert_eq!(
            qual_metric_for("timed_qual", WinCondition::FirstToLaps { n: 5 }),
            Some("best-lap")
        );
    }

    #[test]
    fn qual_metric_for_round_robin_maps_timed_to_total_laps_else_points() {
        // round_robin: Timed (most laps) → total-laps; everything else → the points standing.
        assert_eq!(
            qual_metric_for(
                "round_robin",
                WinCondition::Timed {
                    window_micros: 60_000_000
                }
            ),
            Some("total-laps")
        );
        assert_eq!(
            qual_metric_for("round_robin", WinCondition::BestLap),
            Some("points")
        );
        assert_eq!(
            qual_metric_for("round_robin", WinCondition::BestConsecutive { n: 3 }),
            Some("points")
        );
        assert_eq!(
            qual_metric_for("round_robin", WinCondition::FirstToLaps { n: 4 }),
            Some("points")
        );
    }

    #[test]
    fn qual_metric_for_non_qualifying_format_is_none() {
        // A bracket format keeps its params verbatim — no derived metric.
        assert_eq!(qual_metric_for("single_elim", WinCondition::BestLap), None);
        assert_eq!(
            qual_metric_for("open_practice", WinCondition::BestLap),
            None
        );
    }

    #[test]
    fn format_config_injects_the_win_condition_derived_metric() {
        // The built FormatConfig carries the metric derived from the win condition (the single
        // source of truth), overriding any stale stored `metric` param.
        let mut round = qual_round("q1", "open");
        round.win_condition = WinCondition::Timed {
            window_micros: 90_000_000,
        };
        // A stale stored metric must be overridden by the win-condition-derived one.
        round.params.insert("metric".into(), "best-lap".into());
        let config = format_config(&round, vec![CompetitorRef("A".into())]);
        assert_eq!(
            config.params.get("metric").map(String::as_str),
            Some("most-laps")
        );
    }

    #[test]
    fn round_ranking_ranks_by_the_win_condition_derived_metric() {
        // A timed_qual round scored under Timed (Most Laps): the ranking must rank by most-laps
        // (the win-condition-derived metric), NOT the best-lap default — even with no stored metric.
        let mut round = qual_round("q1", "open");
        round.win_condition = WinCondition::Timed {
            window_micros: 100_000_000,
        };
        let meta = meta_with(vec![round.clone()], vec![member("open", &["A", "B"])]);

        // One heat over A,B where B completes more laps inside the window than A.
        // Passes (lap-gate): A holeshot then 2 more laps (2 counted); B holeshot then 3 more (3).
        let mut events = vec![scheduled("q1-h", "q1", "open", &["A", "B"])];
        let passes = vec![
            pass("A", 0, 0),
            pass("B", 0, 1),
            pass("A", 10_000_000, 2),
            pass("B", 10_000_000, 3),
            pass("A", 20_000_000, 4),
            pass("B", 20_000_000, 5),
            pass("B", 30_000_000, 6),
        ];
        events.extend(run_heat_events("q1-h", passes));

        let ranking = round_ranking(&meta, &round, &events).unwrap();
        // B banked more laps → ranks ahead of A under the most-laps qualifying metric.
        assert_eq!(ranking[0].competitor, CompetitorRef("B".into()));
        assert_eq!(ranking[0].position, 1);
    }

    /// A `RankEntry` for a competitor at a 1-based position (aggregation test helper).
    fn rank(competitor: &str, position: u32) -> RankEntry {
        RankEntry {
            competitor: CompetitorRef(competitor.into()),
            position,
        }
    }

    #[test]
    fn aggregate_rankings_of_a_single_round_is_that_ranking() {
        // One source round → the merged ranking is that round's ranking unchanged.
        let r1 = vec![rank("A", 1), rank("B", 2), rank("C", 3)];
        let merged = aggregate_rankings(std::slice::from_ref(&r1));
        assert_eq!(merged, r1);
    }

    #[test]
    fn aggregate_rankings_takes_each_pilots_best_position_across_rounds() {
        // A placed 1 then 3 → best 1; B placed 2 then 2 → best 2; C placed 3 then 1 → best 1.
        // A and C tie at best-position 1 (dense, tie-aware: 1, 1, then B at 3).
        let r1 = vec![rank("A", 1), rank("B", 2), rank("C", 3)];
        let r2 = vec![rank("C", 1), rank("B", 2), rank("A", 3)];
        let merged = aggregate_rankings(&[r1, r2]);
        assert_eq!(
            merged,
            vec![
                rank("A", 1), // best 1, ref tie-break before C
                rank("C", 1), // best 1
                rank("B", 3), // best 2 → dense position 3 (skips past the two 1s)
            ]
        );
    }

    #[test]
    fn aggregate_rankings_includes_pilots_present_in_only_some_rounds() {
        // D only raced round 2; they still seed from their best (and only) position there.
        let r1 = vec![rank("A", 1), rank("B", 2)];
        let r2 = vec![rank("D", 1), rank("A", 2)];
        let merged = aggregate_rankings(&[r1, r2]);
        // A best 1, D best 1 (tie, ref order A then D), B best 2 → position 3.
        assert_eq!(merged, vec![rank("A", 1), rank("D", 1), rank("B", 3)]);
    }

    #[test]
    fn aggregate_rankings_is_deterministic_regardless_of_round_order() {
        let r1 = vec![rank("A", 1), rank("B", 2), rank("C", 3)];
        let r2 = vec![rank("C", 1), rank("B", 2), rank("A", 3)];
        assert_eq!(
            aggregate_rankings(&[r1.clone(), r2.clone()]),
            aggregate_rankings(&[r2, r1])
        );
    }

    /// An **open-practice** round fixture (open-practice format, Slice 1): `format: "open_practice"`
    /// + `seeding: AllChannels { channels }`, with no eligible classes (it is not a class round).
    fn open_practice_round(id: &str, channels: &[usize]) -> RoundDef {
        RoundDef {
            id: RoundId(id.into()),
            label: id.into(),
            classes: vec![],
            format: "open_practice".into(),
            params: BTreeMap::new(),
            win_condition: WinCondition::BestLap,
            seeding: SeedingRule::AllChannels {
                channels: channels.to_vec(),
            },
            channel_mode: ChannelMode::PerHeat,
            staging_timer_secs: default_staging_timer_secs(),
            start_procedure: StartProcedure::default(),
            grace_window: default_grace_window(),
            protest_window: gridfpv_engine::heat::ProtestWindow::Off,
            time_limit_secs: None,
        }
    }

    #[test]
    fn open_practice_round_emits_one_heat_over_the_channels_with_empty_frequencies() {
        // The active channels (node indices) become `node-{i}` lineup refs; the one open heat
        // carries them with EMPTY frequencies (the lineup *is* the channels — nothing to allocate).
        let round = open_practice_round("op1", &[0, 2, 5]);
        let meta = meta_with(vec![round], vec![]);
        match fill_round(&meta, &no_timers(), &RoundId("op1".into()), &[]).unwrap() {
            FillOutcome::Scheduled {
                heat,
                lineup,
                frequencies,
            } => {
                // Issue #54: the heat id is **round-scoped** (`<round_id>-heat`), not the generator's
                // fixed `"open-practice"`, so two open-practice rounds get distinct heats.
                assert_eq!(heat.0, "op1-heat");
                assert_eq!(
                    lineup,
                    vec![
                        CompetitorRef("node-0".into()),
                        CompetitorRef("node-2".into()),
                        CompetitorRef("node-5".into()),
                    ]
                );
                assert_eq!(
                    frequencies,
                    Some(Vec::new()),
                    "an open-practice heat carries empty frequencies"
                );
            }
            other => panic!("expected a scheduled open-practice heat, got {other:?}"),
        }
    }

    #[test]
    fn open_practice_round_completes_after_its_one_heat() {
        // After the single open heat is scheduled + driven to Final, the next FillRound is Complete
        // (open practice is one heat, ever — no advancement).
        let round = open_practice_round("op1", &[0, 1]);
        let meta = meta_with(vec![round], vec![]);
        let mut log = vec![scheduled("op1-heat", "op1", "open", &["node-0", "node-1"])];
        // The schedule above tags a class for the test helper; re-tag without a class is unnecessary
        // — `finalized_heat_ids` keys on the round, not the class.
        log.extend(run_heat_events("op1-heat", vec![]));
        let next = fill_round(&meta, &no_timers(), &RoundId("op1".into()), &log).unwrap();
        assert_eq!(next, FillOutcome::Complete);
    }

    #[test]
    fn two_open_practice_rounds_yield_distinct_round_scoped_heat_ids() {
        // Issue #54: two open-practice rounds in one event must auto-create **two distinct** heats.
        // Before the fix both claimed the generator's fixed `"open-practice"` id, so the second round
        // got no heat of its own. The id is now derived from the round id.
        let op1 = open_practice_round("op1", &[0, 1]);
        let op2 = open_practice_round("op2", &[2, 3]);
        let meta = meta_with(vec![op1, op2], vec![]);

        let heat1 = match fill_round(&meta, &no_timers(), &RoundId("op1".into()), &[]).unwrap() {
            FillOutcome::Scheduled { heat, .. } => heat,
            other => panic!("expected op1 to schedule a heat, got {other:?}"),
        };
        let heat2 = match fill_round(&meta, &no_timers(), &RoundId("op2".into()), &[]).unwrap() {
            FillOutcome::Scheduled { heat, .. } => heat,
            other => panic!("expected op2 to schedule a heat, got {other:?}"),
        };

        assert_eq!(heat1.0, "op1-heat");
        assert_eq!(heat2.0, "op2-heat");
        assert_ne!(
            heat1, heat2,
            "each open-practice round gets its own heat id"
        );
    }

    #[test]
    fn is_open_practice_recognizes_only_the_open_practice_format_plus_allchannels() {
        // Both the format name AND AllChannels seeding are required.
        assert!(is_open_practice(&open_practice_round("op", &[0])));
        // A normal qual round is not open-practice.
        assert!(!is_open_practice(&qual_round("q", "open")));
        // The format name alone (with FromRoster) is not enough.
        let mut mis = open_practice_round("op", &[0]);
        mis.seeding = SeedingRule::FromRoster;
        assert!(!is_open_practice(&mis));
    }

    #[test]
    fn round_completes_after_its_configured_rounds() {
        // A 1-round timed_qual: after one scored heat, the next FillRound is Complete.
        let round = qual_round("q1", "open");
        let meta = meta_with(vec![round], vec![member("open", &["A", "B"])]);

        // The first heat the generator emits is `round-1`.
        let first = fill_round(&meta, &no_timers(), &RoundId("q1".into()), &[]).unwrap();
        let heat_id = match first {
            FillOutcome::Scheduled { heat, .. } => heat.0,
            other => panic!("expected scheduled, got {other:?}"),
        };

        // Append the tagged schedule + drive it to Final with some passes.
        let mut log = vec![scheduled(&heat_id, "q1", "open", &["A", "B"])];
        log.extend(run_heat_events(
            &heat_id,
            vec![
                pass("A", 0, 0),
                pass("B", 100, 0),
                pass("A", 1_500_000, 1),
                pass("B", 1_700_000, 1),
            ],
        ));

        // Now the round is complete (1 round configured, 1 scored).
        let next = fill_round(&meta, &no_timers(), &RoundId("q1".into()), &log).unwrap();
        assert_eq!(next, FillOutcome::Complete);

        // And the round has a final ranking — A (faster lap) ahead of B.
        let round = &meta.rounds[0];
        let ranking = round_ranking(&meta, round, &log).unwrap();
        assert_eq!(ranking[0].competitor, CompetitorRef("A".into()));
        assert_eq!(ranking[1].competitor, CompetitorRef("B".into()));
    }

    #[test]
    fn bracket_round_seeds_from_a_prior_rounds_ranking() {
        // A qual round, then a single_elim bracket seeded FromRanking(top 2) of the qual.
        let qual = qual_round("q1", "open");
        let bracket = RoundDef {
            id: RoundId("b1".into()),
            label: "Bracket".into(),
            classes: vec![ScopeClassId("open".into())],
            format: "single_elim".into(),
            params: BTreeMap::new(),
            win_condition: WinCondition::FirstToLaps { n: 3 },
            seeding: SeedingRule::FromRanking {
                source_rounds: vec![RoundId("q1".into())],
                top_n: 2,
            },
            channel_mode: ChannelMode::PerHeat,
            staging_timer_secs: default_staging_timer_secs(),
            start_procedure: StartProcedure::default(),
            grace_window: default_grace_window(),
            protest_window: gridfpv_engine::heat::ProtestWindow::Off,
            time_limit_secs: None,
        };
        let meta = meta_with(
            vec![qual, bracket],
            vec![member("open", &["A", "B", "C", "D"])],
        );

        // Run the qual heat to Final: A fastest, then B, C, D.
        let first = fill_round(&meta, &no_timers(), &RoundId("q1".into()), &[]).unwrap();
        let qheat = match first {
            FillOutcome::Scheduled { heat, .. } => heat.0,
            other => panic!("expected scheduled, got {other:?}"),
        };
        let mut log = vec![scheduled(&qheat, "q1", "open", &["A", "B", "C", "D"])];
        log.extend(run_heat_events(
            &qheat,
            vec![
                pass("A", 0, 0),
                pass("B", 10, 0),
                pass("C", 20, 0),
                pass("D", 30, 0),
                pass("A", 1_000_000, 1),
                pass("B", 1_200_000, 1),
                pass("C", 1_400_000, 1),
                pass("D", 1_600_000, 1),
            ],
        ));

        // FillRound the bracket: the field is the top-2 of the qual ranking — A and B.
        let outcome = fill_round(&meta, &no_timers(), &RoundId("b1".into()), &log).unwrap();
        match outcome {
            FillOutcome::Scheduled { lineup, .. } => {
                assert_eq!(
                    lineup,
                    vec![CompetitorRef("A".into()), CompetitorRef("B".into())],
                    "the bracket seeds from the qual ranking (top 2)"
                );
            }
            other => panic!("expected a scheduled bracket heat, got {other:?}"),
        }
    }

    #[test]
    fn bracket_round_seeds_best_per_pilot_across_multiple_source_rounds() {
        // Issue #51: a bracket seeded `FromRanking` from TWO qual rounds. Q1 ranks A,B,C,D;
        // Q2 ranks C,D,A,B. Best-per-pilot positions: A=1 (Q1), C=1 (Q2) → both seed 1; B=2 (Q1),
        // D=2 (Q2) → both seed 2 (well, dense). top_n=2 takes the best two by aggregated rank.
        let q1 = qual_round("q1", "open");
        let q2 = qual_round("q2", "open");
        let bracket = RoundDef {
            id: RoundId("b1".into()),
            label: "Bracket".into(),
            classes: vec![ScopeClassId("open".into())],
            format: "single_elim".into(),
            params: BTreeMap::new(),
            win_condition: WinCondition::FirstToLaps { n: 3 },
            seeding: SeedingRule::FromRanking {
                source_rounds: vec![RoundId("q1".into()), RoundId("q2".into())],
                top_n: 2,
            },
            channel_mode: ChannelMode::PerHeat,
            staging_timer_secs: default_staging_timer_secs(),
            start_procedure: StartProcedure::default(),
            grace_window: default_grace_window(),
            protest_window: gridfpv_engine::heat::ProtestWindow::Off,
            time_limit_secs: None,
        };
        let meta = meta_with(
            vec![q1, q2, bracket],
            vec![member("open", &["A", "B", "C", "D"])],
        );

        // Pre-build the log with each qual round's finalized heat under a DISTINCT heat id (the
        // `timed_qual` generator emits the same `"round-1"` id per round, so we tag explicit ids to
        // keep the two rounds' heats from merging in the heat-state machine — a separate concern from
        // the aggregation under test). `round_ranking` reads each round's finalized heats by the
        // round tag, so the explicit ids drive the per-round ranking exactly as a real run would.
        //
        // Q1 → A fastest, then B, C, D. Q2 → C fastest, then D, A, B (C/D outrank A/B there).
        let mut log = vec![scheduled("q1-heat", "q1", "open", &["A", "B", "C", "D"])];
        log.extend(run_heat_events(
            "q1-heat",
            vec![
                pass("A", 0, 0),
                pass("B", 10, 0),
                pass("C", 20, 0),
                pass("D", 30, 0),
                pass("A", 1_000_000, 1),
                pass("B", 1_200_000, 1),
                pass("C", 1_400_000, 1),
                pass("D", 1_600_000, 1),
            ],
        ));
        log.push(scheduled("q2-heat", "q2", "open", &["A", "B", "C", "D"]));
        log.extend(run_heat_events(
            "q2-heat",
            vec![
                pass("C", 0, 0),
                pass("D", 10, 0),
                pass("A", 20, 0),
                pass("B", 30, 0),
                pass("C", 1_000_000, 1),
                pass("D", 1_200_000, 1),
                pass("A", 1_400_000, 1),
                pass("B", 1_600_000, 1),
            ],
        ));

        // FillRound the bracket: best-per-pilot is A=1 (Q1), C=1 (Q2), B=2 (Q1), D=2 (Q2). top_n=2
        // takes the two best-ranked, ref tie-break A before C among the position-1 pilots.
        let outcome = fill_round(&meta, &no_timers(), &RoundId("b1".into()), &log).unwrap();
        match outcome {
            FillOutcome::Scheduled { lineup, .. } => {
                assert_eq!(
                    lineup,
                    vec![CompetitorRef("A".into()), CompetitorRef("C".into())],
                    "the bracket seeds the best-per-pilot top-2 aggregated across q1 + q2"
                );
            }
            other => panic!("expected a scheduled bracket heat, got {other:?}"),
        }
    }

    // --- Level-per-round single-elim brackets (decisions D13, #217) ------------------------

    /// A `single_elim` bracket-**level** round over `class` with the given `seeding` and id.
    fn bracket_round(id: &str, class: &str, seeding: SeedingRule) -> RoundDef {
        RoundDef {
            id: RoundId(id.into()),
            label: id.into(),
            classes: vec![ScopeClassId(class.into())],
            format: "single_elim".into(),
            params: BTreeMap::new(),
            win_condition: WinCondition::FirstToLaps { n: 3 },
            seeding,
            channel_mode: ChannelMode::PerHeat,
            staging_timer_secs: default_staging_timer_secs(),
            start_procedure: StartProcedure::default(),
            grace_window: default_grace_window(),
            protest_window: gridfpv_engine::heat::ProtestWindow::Off,
            time_limit_secs: None,
        }
    }

    /// A finished bracket heat where `winner` reaches 3 laps and `loser` only 1 — so the round's
    /// `FirstToLaps { n: 3 }` win condition makes `winner` the heat winner. Appends the full
    /// schedule→run→finalize span under `heat_id`, tagged with `round`/`class`.
    fn bracket_heat(
        heat_id: &str,
        round: &str,
        class: &str,
        winner: &str,
        loser: &str,
    ) -> Vec<Event> {
        let mut out = vec![scheduled(heat_id, round, class, &[winner, loser])];
        out.extend(run_heat_events(
            heat_id,
            vec![
                pass(winner, 0, 0),
                pass(loser, 10, 0),
                pass(winner, 1_000_000, 1),
                pass(winner, 2_000_000, 2),
                pass(winner, 3_000_000, 3),
                pass(loser, 1_500_000, 1),
            ],
        ));
        out
    }

    #[test]
    fn advancing_to_a_next_level_seeds_from_the_prior_levels_heat_winners() {
        // The level-per-round flow (#217): quali → level-1 bracket (FromRanking top-4) → level-2
        // bracket (FromHeatWinners of level 1). Advancing creates the next level seeded from the
        // prior level's heat winners, in heat order.
        let qual = qual_round("q1", "open");
        let level1 = bracket_round(
            "l1",
            "open",
            SeedingRule::FromRanking {
                source_rounds: vec![RoundId("q1".into())],
                top_n: 4,
            },
        );
        let level2 = bracket_round(
            "l2",
            "open",
            SeedingRule::FromHeatWinners {
                source_round: RoundId("l1".into()),
            },
        );
        let meta = meta_with(
            vec![qual, level1, level2],
            vec![member("open", &["A", "B", "C", "D"])],
        );

        // Run the quali heat to Final: A fastest, then B, C, D (by best lap).
        let qfill = fill_round(&meta, &no_timers(), &RoundId("q1".into()), &[]).unwrap();
        let qheat = match qfill {
            FillOutcome::Scheduled { heat, .. } => heat.0,
            other => panic!("expected scheduled quali heat, got {other:?}"),
        };
        let mut log = vec![scheduled(&qheat, "q1", "open", &["A", "B", "C", "D"])];
        log.extend(run_heat_events(
            &qheat,
            vec![
                pass("A", 0, 0),
                pass("B", 10, 0),
                pass("C", 20, 0),
                pass("D", 30, 0),
                pass("A", 1_000_000, 1),
                pass("B", 1_200_000, 1),
                pass("C", 1_400_000, 1),
                pass("D", 1_600_000, 1),
            ],
        ));

        // --- Level 1: seeded from the quali top-4 (A,B,C,D). bracket_pairs → A,D,B,C → two heats
        // (A v D), (B v C). FillRound schedules one heat at a time; the ids are round-scoped.
        let l1h0 = match fill_round(&meta, &no_timers(), &RoundId("l1".into()), &log).unwrap() {
            FillOutcome::Scheduled { heat, lineup, .. } => {
                assert_eq!(lineup, lineup_refs(&["A", "D"]), "level-1 heat 0 is A v D");
                heat.0
            }
            other => panic!("expected level-1 heat 0, got {other:?}"),
        };
        assert_eq!(l1h0, "l1-se-h0", "bracket heat ids are scoped to the round");
        // A wins heat 0. Append it, then fill the second heat.
        log.extend(bracket_heat(&l1h0, "l1", "open", "A", "D"));
        let l1h1 = match fill_round(&meta, &no_timers(), &RoundId("l1".into()), &log).unwrap() {
            FillOutcome::Scheduled { heat, lineup, .. } => {
                assert_eq!(lineup, lineup_refs(&["B", "C"]), "level-1 heat 1 is B v C");
                heat.0
            }
            other => panic!("expected level-1 heat 1, got {other:?}"),
        };
        // B wins heat 1. Append it — level 1 is now complete (both heats finalized).
        log.extend(bracket_heat(&l1h1, "l1", "open", "B", "C"));
        assert_eq!(
            fill_round(&meta, &no_timers(), &RoundId("l1".into()), &log).unwrap(),
            FillOutcome::Complete,
            "level 1 completes once both of its heats are in",
        );

        // --- Level 2 (the final): seeded FromHeatWinners of level 1. The winners in heat order are
        // A (heat 0) then B (heat 1), so the final lines up A v B.
        match fill_round(&meta, &no_timers(), &RoundId("l2".into()), &log).unwrap() {
            FillOutcome::Scheduled { heat, lineup, .. } => {
                assert_eq!(
                    lineup,
                    lineup_refs(&["A", "B"]),
                    "the next level seeds from the prior level's heat winners, in heat order"
                );
                assert_eq!(
                    heat.0, "l2-se-h0",
                    "the final's heat id is scoped to level 2"
                );
            }
            other => panic!("expected the level-2 final, got {other:?}"),
        }
    }

    #[test]
    fn a_single_elim_chain_progresses_level_to_level_to_a_final() {
        // An 8-pilot single-elim chain: quarters (FromRanking top-8) → semis (FromHeatWinners of
        // quarters) → final (FromHeatWinners of semis). Each level is its own round; advancement is
        // round-to-round via heat winners. Asserts the chain reaches a single-heat final of the two
        // top seeds.
        let qual = qual_round("q1", "open");
        let quarters = bracket_round(
            "quarters",
            "open",
            SeedingRule::FromRanking {
                source_rounds: vec![RoundId("q1".into())],
                top_n: 8,
            },
        );
        let semis = bracket_round(
            "semis",
            "open",
            SeedingRule::FromHeatWinners {
                source_round: RoundId("quarters".into()),
            },
        );
        let decider = bracket_round(
            "final",
            "open",
            SeedingRule::FromHeatWinners {
                source_round: RoundId("semis".into()),
            },
        );
        let pilots = ["A", "B", "C", "D", "E", "F", "G", "H"];
        let meta = meta_with(
            vec![qual, quarters, semis, decider],
            vec![member("open", &pilots)],
        );

        // Quali: rank A..H in order (descending best lap so A is fastest).
        let qheat = match fill_round(&meta, &no_timers(), &RoundId("q1".into()), &[]).unwrap() {
            FillOutcome::Scheduled { heat, .. } => heat.0,
            other => panic!("expected quali heat, got {other:?}"),
        };
        let mut log = vec![scheduled(&qheat, "q1", "open", &pilots)];
        let qpasses: Vec<Event> = pilots
            .iter()
            .enumerate()
            .flat_map(|(i, p)| {
                vec![
                    pass(p, i as i64, 0),
                    // Later index → slower (larger) lap, so A is fastest, H slowest.
                    pass(p, 1_000_000 + (i as i64) * 100_000, 1),
                ]
            })
            .collect();
        log.extend(run_heat_events(&qheat, qpasses));

        // Helper: fill+run every heat of a bracket level, with the lineup's FIRST entry (the higher
        // seed, laid first by bracket_pairs) winning — then assert the level completes.
        fn run_level(meta: &EventMeta, round: &str, class: &str, log: &mut Vec<Event>) {
            loop {
                match fill_round(meta, &no_timers(), &RoundId(round.into()), log).unwrap() {
                    FillOutcome::Scheduled { heat, lineup, .. } => {
                        let winner = lineup[0].0.clone();
                        let loser = lineup[1].0.clone();
                        log.extend(bracket_heat(&heat.0, round, class, &winner, &loser));
                    }
                    FillOutcome::Complete => break,
                    other => panic!("unexpected fill outcome for {round}: {other:?}"),
                }
            }
        }

        // Quarters: bracket_pairs(A..H) = A,H,B,G,C,F,D,E → 4 heats; the first seed of each wins →
        // A,B,C,D advance.
        run_level(&meta, "quarters", "open", &mut log);
        // Semis: seeded from the quarters' winners A,B,C,D → pairs A,D,B,C → heats (AvD),(BvC) →
        // A,B advance.
        run_level(&meta, "semis", "open", &mut log);

        // Final: seeded from the semis' winners A,B → one heat A v B.
        match fill_round(&meta, &no_timers(), &RoundId("final".into()), &log).unwrap() {
            FillOutcome::Scheduled { lineup, heat, .. } => {
                assert_eq!(
                    lineup,
                    lineup_refs(&["A", "B"]),
                    "the chain reaches a final of the two surviving top seeds",
                );
                assert_eq!(heat.0, "final-se-h0");
            }
            other => panic!("expected the final, got {other:?}"),
        }
    }

    #[test]
    fn from_heat_winners_is_deterministic_on_replay() {
        // The FromHeatWinners carry is a pure function of the log + meta: filling the next level
        // twice over the same log yields the identical seeded lineup (no clock, no RNG).
        let qual = qual_round("q1", "open");
        let level1 = bracket_round(
            "l1",
            "open",
            SeedingRule::FromRanking {
                source_rounds: vec![RoundId("q1".into())],
                top_n: 4,
            },
        );
        let level2 = bracket_round(
            "l2",
            "open",
            SeedingRule::FromHeatWinners {
                source_round: RoundId("l1".into()),
            },
        );
        let meta = meta_with(
            vec![qual, level1, level2],
            vec![member("open", &["A", "B", "C", "D"])],
        );

        let qheat = match fill_round(&meta, &no_timers(), &RoundId("q1".into()), &[]).unwrap() {
            FillOutcome::Scheduled { heat, .. } => heat.0,
            other => panic!("expected quali heat, got {other:?}"),
        };
        let mut log = vec![scheduled(&qheat, "q1", "open", &["A", "B", "C", "D"])];
        log.extend(run_heat_events(
            &qheat,
            vec![
                pass("A", 0, 0),
                pass("B", 10, 0),
                pass("C", 20, 0),
                pass("D", 30, 0),
                pass("A", 1_000_000, 1),
                pass("B", 1_200_000, 1),
                pass("C", 1_400_000, 1),
                pass("D", 1_600_000, 1),
            ],
        ));
        // Level 1: A v D, B v C → A, B win.
        let l1h0 = match fill_round(&meta, &no_timers(), &RoundId("l1".into()), &log).unwrap() {
            FillOutcome::Scheduled { heat, .. } => heat.0,
            other => panic!("expected level-1 heat 0, got {other:?}"),
        };
        log.extend(bracket_heat(&l1h0, "l1", "open", "A", "D"));
        let l1h1 = match fill_round(&meta, &no_timers(), &RoundId("l1".into()), &log).unwrap() {
            FillOutcome::Scheduled { heat, .. } => heat.0,
            other => panic!("expected level-1 heat 1, got {other:?}"),
        };
        log.extend(bracket_heat(&l1h1, "l1", "open", "B", "C"));

        let first = fill_round(&meta, &no_timers(), &RoundId("l2".into()), &log).unwrap();
        let second = fill_round(&meta, &no_timers(), &RoundId("l2".into()), &log).unwrap();
        assert_eq!(
            first, second,
            "the FromHeatWinners carry replays identically"
        );
    }

    /// A lineup of [`CompetitorRef`]s from names — the bracket tests' expected-lineup builder.
    fn lineup_refs(names: &[&str]) -> Vec<CompetitorRef> {
        names.iter().map(|n| CompetitorRef((*n).into())).collect()
    }

    // --- Static channel-balanced formation (race redesign Slice 7a) ------------------------

    /// A `timed_qual` round in **Static** channel mode (one format-round) over `class`.
    fn static_qual_round(id: &str, class: &str) -> RoundDef {
        RoundDef {
            id: RoundId(id.into()),
            label: id.into(),
            classes: vec![ScopeClassId(class.into())],
            format: "timed_qual".into(),
            params: BTreeMap::from([("rounds".into(), "1".into())]),
            win_condition: WinCondition::BestLap,
            seeding: SeedingRule::FromRoster,
            channel_mode: ChannelMode::Static,
            staging_timer_secs: default_staging_timer_secs(),
            start_procedure: StartProcedure::default(),
            grace_window: default_grace_window(),
            protest_window: gridfpv_engine::heat::ProtestWindow::Off,
            time_limit_secs: None,
        }
    }

    #[test]
    fn static_round_forms_channel_balanced_heats_over_node_cap() {
        // 20 members spread across 8 Raceband channels on a 4-node timer (channels > node_count):
        // every heat has ≤ 4 pilots on DISTINCT channels, and every member flies. The channel pool
        // (8) exceeds the node cap (4) — node_count caps only pilots/heat, never the channel count.
        let r1 = RACEBAND_MHZ[0];
        let r2 = RACEBAND_MHZ[1];
        let r3 = RACEBAND_MHZ[2];
        let r4 = RACEBAND_MHZ[3];
        let r5 = RACEBAND_MHZ[4];
        let r6 = RACEBAND_MHZ[5];
        let r7 = RACEBAND_MHZ[6];
        let r8 = RACEBAND_MHZ[7];
        let channels = [r1, r2, r3, r4, r5, r6, r7, r8];
        // 20 pilots, 2-3 per channel.
        let members: Vec<(&str, u16)> =
            (0..20).map(|i| (PILOT_NAMES[i], channels[i % 8])).collect();
        let round = static_qual_round("q1", "open");
        let (meta, timers) = meta_with_timer(
            vec![round],
            vec![member_chan("open", &members)],
            4, // node cap
        );

        // Drive every static heat the round wants, collecting each scheduled heat's assignment.
        let mut log: Vec<Event> = Vec::new();
        let mut heats: Vec<Vec<(CompetitorRef, u16)>> = Vec::new();
        for _ in 0..50 {
            match fill_round(&meta, &timers, &RoundId("q1".into()), &log).unwrap() {
                FillOutcome::Scheduled {
                    heat,
                    lineup,
                    frequencies,
                } => {
                    let freqs = frequencies.expect("static round assigns channels itself");
                    // Each heat is ≤ node cap and channel-distinct.
                    assert!(lineup.len() <= 4, "heat exceeds the 4-node cap: {lineup:?}");
                    let mut seen = std::collections::BTreeSet::new();
                    for (_, ch) in &freqs {
                        assert!(seen.insert(*ch), "duplicate channel {ch} in one heat");
                    }
                    // Lineup matches the assignment competitors.
                    let lineup_set: Vec<&str> = lineup.iter().map(|c| c.0.as_str()).collect();
                    let freq_set: Vec<&str> = freqs.iter().map(|(c, _)| c.0.as_str()).collect();
                    assert_eq!(lineup_set, freq_set);
                    heats.push(freqs.clone());
                    // Schedule + score the heat so the next FillRound advances.
                    let names: Vec<String> = lineup.iter().map(|c| c.0.clone()).collect();
                    log.push(Event::HeatScheduled {
                        heat: heat.clone(),
                        lineup,
                        class: Some(ClassId("open".into())),
                        round: Some(RoundId("q1".into())),
                        frequencies: freqs,
                    });
                    let mut passes = Vec::new();
                    for (i, n) in names.iter().enumerate() {
                        passes.push(pass(n, i as i64, 0));
                        passes.push(pass(n, 1_000_000 + i as i64, 1));
                    }
                    log.extend(run_heat_events(&heat.0, passes));
                }
                FillOutcome::Complete => break,
                other => panic!("unexpected static outcome {other:?}"),
            }
        }

        // Every one of the 20 members flew exactly once across the round.
        let flown: std::collections::BTreeSet<&str> = heats
            .iter()
            .flat_map(|h| h.iter().map(|(c, _)| c.0.as_str()))
            .collect();
        assert_eq!(flown.len(), 20, "every member flies");
        // The channel pool used spans all 8 channels (> the 4-node cap).
        let used_channels: std::collections::BTreeSet<u16> = heats
            .iter()
            .flat_map(|h| h.iter().map(|(_, ch)| *ch))
            .collect();
        assert_eq!(used_channels.len(), 8, "channels span the full 8-wide pool");
    }

    #[test]
    fn static_round_missing_channel_is_a_typed_error() {
        // A Static round with a member lacking a channel is a clear MissingChannel error.
        let round = static_qual_round("q1", "open");
        let (meta, timers) = meta_with_timer(
            vec![round],
            vec![ClassMembership {
                class: ScopeClassId("open".into()),
                pilots: vec![
                    MemberSlot {
                        pilot: PilotId("A".into()),
                        channel: Some(RACEBAND_MHZ[0]),
                    },
                    MemberSlot::new(PilotId("B".into())), // no channel
                ],
            }],
            4,
        );
        assert!(matches!(
            fill_round(&meta, &timers, &RoundId("q1".into()), &[]),
            Err(FillError::MissingChannel(_))
        ));
    }

    #[test]
    fn static_round_is_deterministic_on_replay() {
        let members: Vec<(&str, u16)> = (0..6)
            .map(|i| (PILOT_NAMES[i], RACEBAND_MHZ[i % 3]))
            .collect();
        let round = static_qual_round("q1", "open");
        let (meta, timers) = meta_with_timer(vec![round], vec![member_chan("open", &members)], 4);
        let once = fill_round(&meta, &timers, &RoundId("q1".into()), &[]).unwrap();
        let twice = fill_round(&meta, &timers, &RoundId("q1".into()), &[]).unwrap();
        assert_eq!(once, twice);
    }

    /// Twenty stable pilot callsigns for the static formation tests.
    const PILOT_NAMES: [&str; 20] = [
        "p00", "p01", "p02", "p03", "p04", "p05", "p06", "p07", "p08", "p09", "p10", "p11", "p12",
        "p13", "p14", "p15", "p16", "p17", "p18", "p19",
    ];

    // --- Per-class standings (race redesign Slice 5/6a) ------------------------------------

    /// A complete scored qual heat for a round + class with four pilots A>B>C>D on best lap,
    /// returning the round-tagged schedule plus the run-to-Final events.
    fn scored_qual_heat(heat: &str, round: &str, class: &str, names: &[&str]) -> Vec<Event> {
        let mut log = vec![scheduled(heat, round, class, names)];
        // Holeshot for all, then a distinct lap per pilot so the best-lap order is A<B<C<D.
        let mut passes = Vec::new();
        for (i, n) in names.iter().enumerate() {
            passes.push(pass(n, (i as i64) * 10, 0));
        }
        for (i, n) in names.iter().enumerate() {
            // A lap of 1.0s + i*0.2s — A fastest, D slowest.
            passes.push(pass(n, 1_000_000 + (i as i64) * 200_000, 1));
        }
        log.extend(run_heat_events(heat, passes));
        log
    }

    #[test]
    fn class_standings_aggregate_a_single_round() {
        // One qual round over a four-pilot class: standings rank A>B>C>D, points = field-pos+1.
        let round = qual_round("q1", "open");
        let meta = meta_with(vec![round], vec![member("open", &["A", "B", "C", "D"])]);
        let log = scored_qual_heat("q-1", "q1", "open", &["A", "B", "C", "D"]);

        let standings = class_standings(&meta, &ClassId("open".into()), &log).unwrap();
        let order: Vec<&str> = standings
            .standings
            .iter()
            .map(|s| s.competitor.0.as_str())
            .collect();
        assert_eq!(order, vec!["A", "B", "C", "D"], "best-lap order");
        // 4-pilot field: 1st=4pts, 2nd=3, 3rd=2, 4th=1.
        assert_eq!(standings.standings[0].points, 4);
        assert_eq!(standings.standings[3].points, 1);
        assert_eq!(standings.standings[0].position, 1);
        assert_eq!(standings.standings[3].position, 4);
        // A's best lap is the fastest (1.0s) and they ran the one round.
        assert_eq!(standings.standings[0].best_lap_micros, Some(1_000_000));
        assert_eq!(standings.standings[0].rounds_entered, 1);
        assert_eq!(standings.standings[0].total_laps, 1);
    }

    /// A `PenaltyApplied { PointsDeducted }` for a competitor in a heat.
    fn points_deducted(heat: &str, competitor: &str, points: u32) -> Event {
        Event::PenaltyApplied {
            heat: HeatId(heat.into()),
            competitor: CompetitorRef(competitor.into()),
            penalty: gridfpv_events::Penalty::PointsDeducted { points },
        }
    }

    #[test]
    fn class_standings_apply_points_deduction_to_the_season_total() {
        // Slice 6: a points deduction shifts the *standings* total (not the per-heat lap result).
        // A wins the round (4 pts) but is docked 3 → 1 pt, sinking A below B(3) and C(2). The
        // re-fold is deterministic and order-independent (the deduction is keyed by competitor).
        let round = qual_round("q1", "open");
        let meta = meta_with(vec![round], vec![member("open", &["A", "B", "C", "D"])]);
        let mut log = scored_qual_heat("q-1", "q1", "open", &["A", "B", "C", "D"]);
        log.push(points_deducted("q-1", "A", 3)); // dock A 3 points

        let standings = class_standings(&meta, &ClassId("open".into()), &log).unwrap();
        let a = standings
            .standings
            .iter()
            .find(|s| s.competitor.0 == "A")
            .unwrap();
        assert_eq!(a.points, 1, "4 round points − 3 deducted");
        // B (3 pts) now leads; A drops to 1 pt — tied with D (also 1 pt), but A's faster best lap
        // breaks the tie ahead of D, so the docked A lands at position 3 (B, C, A, D).
        assert_eq!(standings.standings[0].competitor.0, "B");
        assert_eq!(a.position, 3);
        let d = standings
            .standings
            .iter()
            .find(|s| s.competitor.0 == "D")
            .unwrap();
        assert_eq!(d.points, 1);
        assert_eq!(
            d.position, 4,
            "D ties A on points but loses the best-lap tie-break"
        );
        // Deterministic on replay.
        assert_eq!(
            class_standings(&meta, &ClassId("open".into()), &log).unwrap(),
            standings
        );
    }

    #[test]
    fn class_standings_points_deduction_saturates_at_zero_and_reversal_restores() {
        // A huge deduction floors the total at zero (never negative); reversing the deduction
        // (generalized `RulingReversed`) restores the original points — both standings-only.
        let round = qual_round("q1", "open");
        let meta = meta_with(vec![round], vec![member("open", &["A", "B"])]);
        let base = scored_qual_heat("q-1", "q1", "open", &["A", "B"]);

        // Floor at zero: deduct 100 from A (who earned 2 round points).
        let mut floored = base.clone();
        floored.push(points_deducted("q-1", "A", 100));
        let s = class_standings(&meta, &ClassId("open".into()), &floored).unwrap();
        let a = s.standings.iter().find(|x| x.competitor.0 == "A").unwrap();
        assert_eq!(a.points, 0, "saturates at zero, never negative");

        // Reverse the deduction (target its offset, the last event) → A's points restored.
        let mut reversed = floored.clone();
        let deduction_offset = (floored.len() - 1) as u64;
        reversed.push(Event::RulingReversed {
            target: gridfpv_events::LogRef(deduction_offset),
        });
        let s2 = class_standings(&meta, &ClassId("open".into()), &reversed).unwrap();
        let a2 = s2.standings.iter().find(|x| x.competitor.0 == "A").unwrap();
        assert_eq!(a2.points, 2, "reversing the deduction restores the points");
    }

    #[test]
    fn class_standings_points_added_and_deducted_net_out() {
        // PointsAdded and PointsDeducted on the same competitor net together (order-independent).
        let round = qual_round("q1", "open");
        let meta = meta_with(vec![round], vec![member("open", &["A", "B"])]);
        let mut log = scored_qual_heat("q-1", "q1", "open", &["A", "B"]);
        // A earned 2 round points; +3 then −1 nets +2 → 4 total.
        log.push(Event::PenaltyApplied {
            heat: HeatId("q-1".into()),
            competitor: CompetitorRef("A".into()),
            penalty: gridfpv_events::Penalty::PointsAdded { points: 3 },
        });
        log.push(points_deducted("q-1", "A", 1));
        let s = class_standings(&meta, &ClassId("open".into()), &log).unwrap();
        let a = s.standings.iter().find(|x| x.competitor.0 == "A").unwrap();
        assert_eq!(a.points, 4, "2 + 3 − 1");
    }

    #[test]
    fn class_standings_points_deduction_is_scoped_to_the_class_heat() {
        // A pilot "A" races BOTH classes. A points deduction recorded against the OPEN heat must
        // dock A only in the open standings, never leak into the sport standings (Slice 6 fix).
        let open = qual_round("q1", "open");
        let mut sport = qual_round("q2", "sport");
        sport.classes = vec![ScopeClassId("sport".into())];
        let meta = meta_with(
            vec![open, sport],
            vec![member("open", &["A", "B"]), member("sport", &["A", "B"])],
        );
        let mut log = scored_qual_heat("q1-1", "q1", "open", &["A", "B"]);
        log.extend(scored_qual_heat("q2-1", "q2", "sport", &["A", "B"]));
        // Deduct 2 points from A, recorded against the OPEN heat (q1-1).
        log.push(points_deducted("q1-1", "A", 2));

        // Open: A earned 2 round points, docked 2 → 0.
        let open_s = class_standings(&meta, &ClassId("open".into()), &log).unwrap();
        let open_a = open_s
            .standings
            .iter()
            .find(|x| x.competitor.0 == "A")
            .unwrap();
        assert_eq!(open_a.points, 0, "open deduction applies to open");
        // Sport: A's 2 round points are UNTOUCHED — the open-heat deduction must not leak.
        let sport_s = class_standings(&meta, &ClassId("sport".into()), &log).unwrap();
        let sport_a = sport_s
            .standings
            .iter()
            .find(|x| x.competitor.0 == "A")
            .unwrap();
        assert_eq!(
            sport_a.points, 2,
            "open-heat deduction must not leak into sport"
        );
    }

    #[test]
    fn class_standings_aggregate_across_multiple_rounds() {
        // Two qual rounds for the class; points accumulate across both. Same A>B>C>D each round,
        // so A totals 8 points (4+4) and leads, D totals 2 (1+1) and trails.
        let r1 = qual_round("q1", "open");
        let r2 = qual_round("q2", "open");
        let meta = meta_with(vec![r1, r2], vec![member("open", &["A", "B", "C", "D"])]);
        let mut log = scored_qual_heat("q1-1", "q1", "open", &["A", "B", "C", "D"]);
        log.extend(scored_qual_heat(
            "q2-1",
            "q2",
            "open",
            &["A", "B", "C", "D"],
        ));

        let standings = class_standings(&meta, &ClassId("open".into()), &log).unwrap();
        assert_eq!(standings.standings[0].competitor, CompetitorRef("A".into()));
        assert_eq!(standings.standings[0].points, 8, "4 + 4 across two rounds");
        assert_eq!(standings.standings[0].rounds_entered, 2);
        assert_eq!(standings.standings[0].total_laps, 2, "one lap each round");
        assert_eq!(
            standings.standings.last().unwrap().competitor,
            CompetitorRef("D".into())
        );
        assert_eq!(standings.standings.last().unwrap().points, 2);
    }

    #[test]
    fn class_standings_exclude_other_classes_rounds() {
        // Two classes each with their own round; the "open" standings cover only open's pilots.
        let open = qual_round("q1", "open");
        let mut sport = qual_round("q2", "sport");
        sport.classes = vec![ScopeClassId("sport".into())];
        let meta = meta_with(
            vec![open, sport],
            vec![member("open", &["A", "B"]), member("sport", &["X", "Y"])],
        );
        let mut log = scored_qual_heat("q1-1", "q1", "open", &["A", "B"]);
        log.extend(scored_qual_heat("q2-1", "q2", "sport", &["X", "Y"]));

        let standings = class_standings(&meta, &ClassId("open".into()), &log).unwrap();
        let names: Vec<&str> = standings
            .standings
            .iter()
            .map(|s| s.competitor.0.as_str())
            .collect();
        assert_eq!(names, vec!["A", "B"], "only open's pilots, not X/Y");
    }

    #[test]
    fn class_standings_are_deterministic_on_replay() {
        let round = qual_round("q1", "open");
        let meta = meta_with(vec![round], vec![member("open", &["A", "B", "C"])]);
        let log = scored_qual_heat("q-1", "q1", "open", &["A", "B", "C"]);
        let once = class_standings(&meta, &ClassId("open".into()), &log).unwrap();
        let twice = class_standings(&meta, &ClassId("open".into()), &log).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn class_standings_for_a_class_with_no_rounds_are_empty() {
        let round = qual_round("q1", "open");
        let meta = meta_with(vec![round], vec![member("open", &["A", "B"])]);
        // Query a class that has no round at all.
        let standings = class_standings(&meta, &ClassId("nobody".into()), &[]).unwrap();
        assert!(standings.standings.is_empty());
        assert_eq!(standings.class, ClassId("nobody".into()));
    }

    /// A **race** round (a `Timed` win condition) — the bug's reproduction case. Its placements
    /// carry completion *times* (`Metric::LastLapAt`), not lap durations, so the standings best lap
    /// must come from the lap-list projection, not the metric.
    fn race_round(id: &str, class: &str) -> RoundDef {
        RoundDef {
            id: RoundId(id.into()),
            label: id.into(),
            classes: vec![ScopeClassId(class.into())],
            format: "timed_qual".into(),
            params: BTreeMap::from([("rounds".into(), "1".into())]),
            win_condition: WinCondition::Timed {
                window_micros: 60_000_000,
            },
            seeding: SeedingRule::FromRoster,
            channel_mode: ChannelMode::PerHeat,
            staging_timer_secs: default_staging_timer_secs(),
            start_procedure: StartProcedure::default(),
            grace_window: default_grace_window(),
            protest_window: gridfpv_engine::heat::ProtestWindow::Off,
            time_limit_secs: None,
        }
    }

    #[test]
    fn class_standings_compute_best_lap_for_a_race_round() {
        // Regression: a `Timed` race round scores completion *times* into its placement metric, not
        // lap durations — so the old metric-reading best-lap left `best_lap_micros` null even though
        // the heat had real per-pilot laps. The best lap must now be the minimum lap duration each
        // pilot ran, folded from the same lap view `total_laps` is counted from.
        let round = race_round("r1", "open");
        let meta = meta_with(vec![round], vec![member("open", &["A", "B", "C"])]);

        // Holeshot at t=0, then multi-lap runs with a known fastest lap per pilot:
        //   A: laps 2.50s, 2.20s, 2.40s  → best 2.20s
        //   B: laps 2.30s, 2.60s         → best 2.30s
        //   C: never completes a lap (one crossing only) → no best lap.
        let passes = vec![
            pass("A", 0, 0),
            pass("B", 0, 1),
            pass("C", 0, 2),
            pass("A", 2_500_000, 3), // A lap1 2.50s
            pass("B", 2_300_000, 4), // B lap1 2.30s
            pass("A", 4_700_000, 5), // A lap2 2.20s (A's fastest)
            pass("B", 4_900_000, 6), // B lap2 2.60s
            pass("A", 7_100_000, 7), // A lap3 2.40s
        ];
        let mut log = vec![scheduled("r-1", "r1", "open", &["A", "B", "C"])];
        log.extend(run_heat_events("r-1", passes));

        let standings = class_standings(&meta, &ClassId("open".into()), &log).unwrap();
        let by_name = |name: &str| {
            standings
                .standings
                .iter()
                .find(|s| s.competitor.0 == name)
                .unwrap_or_else(|| panic!("{name} missing from standings"))
        };

        // The fix: each pilot's best lap is the minimum of their counted laps — not null.
        assert_eq!(by_name("A").best_lap_micros, Some(2_200_000), "A min lap");
        assert_eq!(by_name("A").total_laps, 3);
        assert_eq!(by_name("B").best_lap_micros, Some(2_300_000), "B min lap");
        assert_eq!(by_name("B").total_laps, 2);
        // A pilot with no completed lap stays null.
        assert_eq!(by_name("C").best_lap_micros, None, "C completed no lap");
        assert_eq!(by_name("C").total_laps, 0);
    }

    #[test]
    fn class_standings_best_lap_is_min_across_a_pilots_race_rounds() {
        // Across two race rounds, the standings best lap is the minimum over both rounds' laps.
        let r1 = race_round("r1", "open");
        let r2 = race_round("r2", "open");
        let meta = meta_with(vec![r1, r2], vec![member("open", &["A", "B"])]);

        // Round 1: A's fastest lap 2.40s. Round 2: A's fastest lap 2.10s → overall best 2.10s.
        let mut log = vec![scheduled("r1-1", "r1", "open", &["A", "B"])];
        log.extend(run_heat_events(
            "r1-1",
            vec![
                pass("A", 0, 0),
                pass("B", 0, 1),
                pass("A", 2_400_000, 2),
                pass("B", 2_700_000, 3),
            ],
        ));
        log.push(scheduled("r2-1", "r2", "open", &["A", "B"]));
        log.extend(run_heat_events(
            "r2-1",
            vec![
                pass("A", 0, 4),
                pass("B", 0, 5),
                pass("A", 2_100_000, 6),
                pass("B", 2_900_000, 7),
            ],
        ));

        let standings = class_standings(&meta, &ClassId("open".into()), &log).unwrap();
        let a = standings
            .standings
            .iter()
            .find(|s| s.competitor.0 == "A")
            .unwrap();
        assert_eq!(a.best_lap_micros, Some(2_100_000), "min across both rounds");
        assert_eq!(a.total_laps, 2, "one lap each round");
    }

    #[test]
    fn fill_round_is_deterministic_on_replay() {
        let round = qual_round("q1", "open");
        let meta = meta_with(vec![round], vec![member("open", &["A", "B", "C"])]);
        let once = fill_round(&meta, &no_timers(), &RoundId("q1".into()), &[]).unwrap();
        let twice = fill_round(&meta, &no_timers(), &RoundId("q1".into()), &[]).unwrap();
        assert_eq!(once, twice);
    }

    /// Mirror of the engine's `run_event_is_deterministic_on_replay` for the FillRound-driven
    /// sequence: a 2-round timed_qual replays the **same** sequence of fill outcomes off the
    /// same log + meta — the determinism the round-driven engine inherits from the generator
    /// contract (RE §6).
    #[test]
    fn fill_round_sequence_is_deterministic_on_replay() {
        // A 2-round qual so the sequence has more than one fill step.
        let mut round = qual_round("q1", "open");
        round.params = BTreeMap::from([("rounds".into(), "2".into())]);
        let meta = meta_with(vec![round], vec![member("open", &["A", "B", "C"])]);

        // Drive the full FillRound sequence over a freshly-built log, recording each outcome.
        let drive = || -> Vec<FillOutcome> {
            let mut log: Vec<Event> = Vec::new();
            let mut outcomes = Vec::new();
            for _ in 0..4 {
                let outcome = fill_round(&meta, &no_timers(), &RoundId("q1".into()), &log).unwrap();
                outcomes.push(outcome.clone());
                if let FillOutcome::Scheduled { heat, lineup, .. } = outcome {
                    let heat_id = heat.0.clone();
                    log.push(Event::HeatScheduled {
                        heat,
                        lineup,
                        class: Some(ClassId("open".into())),
                        round: Some(RoundId("q1".into())),
                        frequencies: vec![],
                    });
                    // Score the heat with deterministic passes (A < B < C on best lap).
                    log.extend(run_heat_events(
                        &heat_id,
                        vec![
                            pass("A", 0, 0),
                            pass("B", 10, 0),
                            pass("C", 20, 0),
                            pass("A", 1_000_000, 1),
                            pass("B", 1_200_000, 1),
                            pass("C", 1_400_000, 1),
                        ],
                    ));
                }
            }
            outcomes
        };

        let first = drive();
        let second = drive();
        assert_eq!(first, second, "the FillRound sequence replays identically");
        // The sequence is: schedule r1, schedule r2, then Complete (and stays Complete).
        assert!(matches!(first[0], FillOutcome::Scheduled { .. }));
        assert!(matches!(first[1], FillOutcome::Scheduled { .. }));
        assert_eq!(first[2], FillOutcome::Complete);
        assert_eq!(first[3], FillOutcome::Complete);
    }

    #[test]
    fn heat_passes_windows_to_the_running_heat() {
        // Two heats run back to back; each heat's passes attribute only to it.
        let mut log = vec![
            scheduled("h1", "q1", "open", &["A"]),
            scheduled("h2", "q1", "open", &["B"]),
        ];
        log.extend(run_heat_events(
            "h1",
            vec![pass("A", 0, 0), pass("A", 1_000, 1)],
        ));
        log.extend(run_heat_events(
            "h2",
            vec![pass("B", 0, 0), pass("B", 2_000, 1)],
        ));

        let h1 = heat_passes(&log, &HeatId("h1".into()));
        let h2 = heat_passes(&log, &HeatId("h2".into()));
        assert_eq!(h1.len(), 2);
        assert!(h1.iter().all(|p| p.competitor == CompetitorRef("A".into())));
        assert_eq!(h2.len(), 2);
        assert!(h2.iter().all(|p| p.competitor == CompetitorRef("B".into())));
    }
}
