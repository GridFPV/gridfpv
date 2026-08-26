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

use gridfpv_engine::format::{
    CompletedHeat, FormatConfig, FormatRegistry, GeneratorStep, RankEntry, advance_range,
    advance_top_n,
};
use gridfpv_engine::heat::{HeatState, heat_state};
use gridfpv_engine::schedule::{Frequency, FrequencyPool, allocate};
use gridfpv_engine::scoring::{HeatResult, Metric, WinCondition};
use gridfpv_events::{ClassId, CompetitorRef, Event, HeatId, RoundId};
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
        /// The round's **field draw to record** (freeze-at-fill, #334): `Some` exactly when
        /// this is a carry-seeded round's FIRST scheduled heat and no draw is recorded yet.
        /// The handler appends the [`Event::RoundFieldDrawn`] *before* the `HeatScheduled`,
        /// freezing the resolution every later read replays.
        field_draw: Option<Vec<CompetitorRef>>,
    },
    /// The round is complete — the generator returned
    /// [`GeneratorStep::Complete`](gridfpv_engine::format::GeneratorStep::Complete). No
    /// heat is appended; the round's final ranking is available via [`round_ranking`].
    Complete,
    /// The round's format **refuses this field** and can never draw a heat for it as configured
    /// (#394) — Head-to-Head handed a single pilot is the case that prompted this. No heat is
    /// appended and the round is *not* finished: nothing has raced and nothing can until the RD
    /// changes something (add a pilot, or pick a format that fits the field).
    ///
    /// A typed ok, not an error: the round is legally configured and the refusal is the
    /// generator behaving correctly. What makes it a *distinct* outcome from
    /// [`Complete`](FillOutcome::Complete) is that "everything raced" and "nothing can race" are
    /// opposite states that were previously reported with the same word — see
    /// [`gridfpv_engine::preconditions`].
    Blocked {
        /// The RD-facing reason, from
        /// [`FieldShortfall`](gridfpv_engine::preconditions::FieldShortfall) — names the format,
        /// its requirement, the round's actual field, and a format that would fit. Carries no
        /// round/heat/pilot id; the caller frames it with the round's friendly label.
        reason: String,
    },
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
    /// Seeding resolution recursed past [`MAX_SEEDING_DEPTH`](crate::events::MAX_SEEDING_DEPTH) —
    /// either a [`Combine`](crate::events::SeedingRule::Combine) nested too deeply, or a cross-round
    /// seeding **cycle** (e.g. round A seeds `FromRanking` B while B seeds `FromRanking` A). A guard
    /// that turns an otherwise-unbounded recursion / stack overflow into a typed `400`.
    SeedingTooDeep,
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
            FillError::SeedingTooDeep => {
                write!(
                    f,
                    "seeding nesting too deep (a Combine nested too far, or a cross-round seeding cycle)"
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
/// 1. **Heat-size cap.** The lineup must fit the timer's **enabled node set** (#412) — its
///    [`seat_capacity`](crate::timers::Timer::seat_capacity), which is how many nodes the RD has
///    left switched on, not the timer's width; otherwise [`AssignError::TooManyForNodes`].
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
    // #412: the cap is the size of the **enabled** node set. On a 4-node timer with node index 2
    // disabled that is 3 — and the three seats a heat lands on are 0, 1 and 3, not 0, 1 and 2. This
    // function only sizes and allocates channels; the seat *indices* are walked (in this same
    // enabled order) where a heat is applied to the timer.
    let nodes = timer.seat_capacity();
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

/// Build a round's **field** as engine [`CompetitorRef`]s (race redesign Slice 3a) — the round's
/// eligible classes + its [`SeedingRule`] resolved by [`resolve_seeding`] at depth 0. See
/// [`resolve_seeding`] for the per-variant semantics (roster / ranking top-N / ranking slice /
/// heat-winners / combine / channels) and the recursion-depth guard.
fn round_field(
    meta: &EventMeta,
    round: &RoundDef,
    events: &[Event],
) -> Result<Vec<CompetitorRef>, FillError> {
    round_field_at(meta, round, events, 0)
}

/// Depth-carrying [`round_field`]: resolve the round's seeding at recursion `depth`.
///
/// The public [`round_field`] enters at depth 0; the seeding resolver threads `depth` through every
/// cross-round / `Combine` hop so a too-deep `Combine` or a cross-round seeding **cycle** is caught
/// as [`FillError::SeedingTooDeep`] rather than overflowing the stack (see [`resolve_seeding`]).
fn round_field_at(
    meta: &EventMeta,
    round: &RoundDef,
    events: &[Event],
    depth: usize,
) -> Result<Vec<CompetitorRef>, FillError> {
    // FREEZE-AT-FILL (#334): a carry seeding's field is drawn ONCE — at the round's first
    // fill — and recorded in the log; every later read replays the recorded draw. Live
    // re-resolution let an adjudication on the SOURCE round, landing after this round had
    // already raced, silently rewrite who this round's field "was" (raced results vanished
    // from its ranking). Before the first fill there is no draw and resolution stays live —
    // a build-ahead round keeps tracking its source until it actually fills.
    if seeding_freezes(&round.seeding) {
        if let Some(field) = recorded_field(events, &round.id) {
            return Ok(field);
        }
    }
    resolve_seeding(meta, &round.classes, &round.seeding, events, depth)
}

/// Whether a [`SeedingRule`]'s resolution is **frozen at first fill** (issue #334).
///
/// The carry seedings — those derived from another round's outcome — freeze: their meaning is
/// "the standings *as advanced*", a draw the RD saw and raced. Roster-derived seedings stay
/// live so a late entrant added to the class mid-round still joins the field/ranking.
fn seeding_freezes(seeding: &SeedingRule) -> bool {
    !matches!(
        seeding,
        SeedingRule::FromRoster | SeedingRule::AllChannels { .. }
    )
}

/// The round's **recorded field draw**, if one was frozen at fill ([`Event::RoundFieldDrawn`]).
/// Last one wins (a round refilled after a `Discard`-style reset re-records).
fn recorded_field(events: &[Event], round_id: &RoundId) -> Option<Vec<CompetitorRef>> {
    events.iter().rev().find_map(|event| match event {
        Event::RoundFieldDrawn { round, field } if round == round_id => Some(field.clone()),
        _ => None,
    })
}

/// Resolve a [`SeedingRule`] to a round's **field** as engine [`CompetitorRef`]s (race redesign
/// Slice 3a; multi-main `FromRankingRange` / `Combine`).
///
/// - [`SeedingRule::FromRoster`] (the default): the union of the eligible `classes`'
///   [`classes_membership`](EventMeta::classes_membership), in class-selection then membership
///   (roster/seed) order, de-duplicated so a pilot in two eligible classes appears once.
/// - [`SeedingRule::FromRanking`]: the **top-N** of the source rounds' best-per-pilot
///   [`aggregate_rankings`] (the qualifying→bracket carry, issue #51 multi-select).
/// - [`SeedingRule::FromRankingRange`]: a **slice** (`skip` / `take`) of that same merged ranking —
///   the multi-main / consolation carry (e.g. a C-main = qual seeds 13–20).
/// - [`SeedingRule::FromHeatWinners`]: the source (bracket-level) round's **heat winners**, in heat
///   order — the bracket advancement carry (decisions D13, #217).
/// - [`SeedingRule::Combine`]: the **union** of each sub-rule's resolved field, concatenated in
///   order and de-duplicated keeping each competitor's first occurrence — the multi-main composition
///   primitive. Each sub-rule resolves against the same `classes`.
///
/// `depth` bounds recursion: it is incremented across each cross-round (`round_ranking` →
/// `resolve_seeding`) hop and each `Combine` level, and past
/// [`MAX_SEEDING_DEPTH`](crate::events::MAX_SEEDING_DEPTH) yields [`FillError::SeedingTooDeep`].
fn resolve_seeding(
    meta: &EventMeta,
    classes: &[ClassId],
    seeding: &SeedingRule,
    events: &[Event],
    depth: usize,
) -> Result<Vec<CompetitorRef>, FillError> {
    if depth > crate::events::MAX_SEEDING_DEPTH {
        return Err(FillError::SeedingTooDeep);
    }
    match seeding {
        SeedingRule::FromRoster => {
            let mut field: Vec<CompetitorRef> = Vec::new();
            for class in classes {
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
            let merged = merged_source_ranking(meta, source_rounds, events, depth)?;
            Ok(advance_top_n(&merged, *top_n))
        }
        // The multi-main / consolation carry: the same merged best-per-pilot ranking as
        // `FromRanking`, but seeded from the window `skip+1 ..= skip+take` rather than the top-N.
        SeedingRule::FromRankingRange {
            source_rounds,
            skip,
            take,
        } => {
            let merged = merged_source_ranking(meta, source_rounds, events, depth)?;
            Ok(advance_range(&merged, *skip, *take))
        }
        // Bracket advancement (decisions D13, #217): the field is the source level's **heat
        // winners** — the competitors that advanced out of each heat, in heat order. This is how a
        // single-elimination bracket advances round-to-round under the level-per-round model: the
        // next level is a new round seeded from the prior level's winners.
        SeedingRule::FromHeatWinners { source_round } => {
            let source = round_of(meta, source_round)
                .map_err(|_| FillError::UnknownSourceRound(source_round.0.clone()))?;
            heat_winners(meta, source, events, depth)
        }
        // The multi-main composition primitive: resolve each sub-rule against the same `classes`,
        // concatenate the fields in order, and de-duplicate keeping each competitor's **first**
        // occurrence — so a competitor two sub-sources both name is seeded once, at the earlier
        // source's position. Each sub-rule resolves one level deeper (bounding the nesting).
        SeedingRule::Combine { sources } => {
            let mut field: Vec<CompetitorRef> = Vec::new();
            for sub in sources {
                for competitor in resolve_seeding(meta, classes, sub, events, depth + 1)? {
                    if !field.contains(&competitor) {
                        field.push(competitor);
                    }
                }
            }
            Ok(field)
        }
        // Open practice (open-practice format): the field is the active **channels**, each node
        // index laid out as a `node-{i}` competitor ref (the timer-seat handle) in the given order.
        // No pilots, no membership — the field is the channels themselves, so laps land on
        // `node-{i}` seats rather than pilots. They ARE logged like any other format (D5 reversed
        // 2026-08-24, #398); practice is excluded from scoring, not from the log.
        SeedingRule::AllChannels { channels } => Ok(channels
            .iter()
            .map(|i| CompetitorRef(format!("node-{i}")))
            .collect()),
    }
}

/// The **merged best-per-pilot ranking** across `source_rounds` — the shared front of the
/// `FromRanking` / `FromRankingRange` carries. Each source round's provisional-or-final ranking is
/// computed (one level deeper, so the depth guard bounds cross-round cycles) then merged via
/// [`aggregate_rankings`]. A source id not in this event is [`FillError::UnknownSourceRound`].
fn merged_source_ranking(
    meta: &EventMeta,
    source_rounds: &[RoundId],
    events: &[Event],
    depth: usize,
) -> Result<Vec<RankEntry>, FillError> {
    let mut rankings: Vec<Vec<RankEntry>> = Vec::with_capacity(source_rounds.len());
    for source_id in source_rounds {
        let source = round_of(meta, source_id)
            .map_err(|_| FillError::UnknownSourceRound(source_id.0.clone()))?;
        rankings.push(round_ranking_at(meta, source, events, depth + 1)?);
    }
    Ok(aggregate_rankings(&rankings))
}

/// The **heat winners** of a (bracket-level) source round, in heat order — the field a
/// [`SeedingRule::FromHeatWinners`] successor level seeds from (decisions D13, #217).
///
/// The winners are the source format's **advancing set** — [`Generator::advancers`], each heat's
/// top `advance` finishers in heat order, then any byes. This carries forward however many heats the
/// level had — head-to-head advances one per heat, a 4-up heat advances two — so the next level's
/// size follows the bracket rather than a fixed top-N. Using the generator's advancers (rather than
/// a ranking-position filter) is what makes a **4-up** level carry correctly: its two heat losers
/// rank at *distinct* in-heat positions (3rd, 4th), so they do not share one worst band and a
/// "better than the worst position" filter would wrongly keep the 3rd-place losers too.
///
/// Before the source level is complete its advancers are provisional (the carry recomputes
/// deterministically as the source level's heats finalize — the same off-the-log property
/// [`FromRanking`](SeedingRule::FromRanking) has). A source round that advances no one (one
/// competitor, or none finalized yet) yields an empty field.
fn heat_winners(
    meta: &EventMeta,
    source: &RoundDef,
    events: &[Event],
    depth: usize,
) -> Result<Vec<CompetitorRef>, FillError> {
    // The carry is the source format's **advancing set** — `Generator::advancers`, not a
    // ranking-position heuristic. `HeadToHead` overrides it to return each heat's winner(s) in
    // heat order (the default position-`< worst` filter wrongly kept the better-placed losers,
    // since a level's eliminated pilots do not share one worst band — the losing-semifinalist
    // bug). Built from the same field + log as `round_ranking_at`, one depth deeper (the
    // seeding-cycle guard).
    let field = round_field_at(meta, source, events, depth + 1)?;
    let registry = FormatRegistry::standard();
    let generator = registry
        .build(&source.format, &format_config(source, field))
        .ok_or_else(|| FillError::UnknownFormat(source.format.clone()))?;
    let completed = completed_heats(source, events);
    Ok(generator.advancers(&completed))
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
/// Used by `add_round`'s auto-fill and by `fill_round`; the field builder lays the channels out as
/// `node-{i}` refs. The format name *and* the seeding are both checked so a mis-tagged round (one or
/// the other but not both) is treated as a normal round, never half-open-practice.
///
/// **The source bridge no longer calls this.** It used to, to route a practice heat's passes into an
/// in-memory accumulator instead of the log — that path was deleted when D5 was reversed
/// (2026-08-24, #398) and practice became an ordinary logged format. The scoring exclusion that
/// replaced it lives in `open_practice::excluded_from_scoring`, which keys on the **format alone**
/// and is deliberately more inclusive than this predicate, so a half-tagged round still cannot be
/// scored even though it is not treated as practice here.
pub fn is_open_practice(round: &RoundDef) -> bool {
    round.format == gridfpv_engine::format::OpenPractice::NAME
        && matches!(round.seeding, SeedingRule::AllChannels { .. })
}

/// Whether `round` is an **open-ended `Static` round** (release-hardening P1-8): a
/// [`ChannelMode::Static`] round whose `rounds` param is `0` (an unbounded "heats per pilot").
///
/// Such a round's [`fill_round_static`] generator **never** reports `Complete` — it manufactures
/// the next heat on demand forever — so a `FillMode::All` batch fill would loop to the defensive cap
/// (logging a spurious "generator bug" warning) instead of converging. The handler uses this to
/// reject `FillMode::All` for the round and steer the RD to single-step fills instead.
pub fn is_open_ended_static(round: &RoundDef) -> bool {
    round.channel_mode == ChannelMode::Static && static_round_count(round) == 0
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
/// - `round_robin` (`RrMetric`, a carved-out format kept only as an inert string arm):
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
/// reaches). Each is scored over its **full adjudicated event window** (passes *and* every
/// marshaling adjudication — DQ / time / throw-out / void / lap-edit) via the one shared
/// [`score_heat_window`](crate::app::score_heat_window) the per-heat result projection uses, so
/// the standings can never disagree with the heat page on an adjudicated heat (#226). The order
/// is the order the heats were first scheduled, which is the order the generator emitted them —
/// so the history fed to [`Generator::next`](gridfpv_engine::format::Generator::next) matches
/// what [`run_format`](gridfpv_engine::event::run_format) accumulated.
pub fn completed_heats(round: &RoundDef, events: &[Event]) -> Vec<CompletedHeat> {
    finalized_heat_ids(round, events)
        .into_iter()
        .map(|heat| {
            // Score over the heat's FULL adjudicated window with PRESERVED global offsets — the
            // SAME path the per-heat result projection (`app.rs` `HeatProjection::Result`) uses, so
            // an adjudication that moves the heat page moves the standings too (#226). The previous
            // pass-only `score_marshaled` discarded every adjudication, leaving the raw on-track
            // score here while the heat page showed the corrected one — the split-brain this closes.
            let result = crate::app::score_heat_window(
                events,
                &heat,
                round.win_condition,
                crate::app::min_lap_micros_of(Some(round)),
            );
            // The generator keys `next`/`ranking` on the heat ids it **emitted**; the log carries
            // the round-scoped id, so strip the scope back off before handing history to the
            // generator (and to every ranking consumer keyed on generator ids).
            let generator_id = unscope_heat_id(round, &heat);
            CompletedHeat::new(generator_id, result)
        })
        // A VOIDED heat's result counts for NOTHING downstream: the RD's "Void heat" (false
        // start, timer glitch) must drop the heat from round ranking, standings, class points,
        // heat winners, and any dependent round's seeding — exactly as if it had not been
        // finalized. (The heat page still shows the voided result, flagged.) Because the void
        // is folded from the adjudicated window, a RulingReversed on the void brings the heat
        // back. Note the round then reads as incomplete until the heat is re-run or the void
        // reversed — that is honest, not a bug.
        .filter(|completed| !completed.result.voided)
        .collect()
}

/// A generator heat id scoped to its round — the id actually logged in `HeatScheduled`.
///
/// Generator ids are only unique within a round, so the log carries `{round}-{generator-id}`
/// (deterministic: the same round + plan always yields the same logged id).
fn scoped_heat_id(round_id: &RoundId, generator_id: &HeatId) -> HeatId {
    HeatId(format!("{}-{}", round_id.0, generator_id.0))
}

/// Map a round's **logged** heat id back to the **generator's** id (the one the format emitted).
///
/// Strips this round's `{round}-` scope prefix. A logged id without the prefix passes through
/// verbatim — that covers events persisted before scoping (their in-flight rounds keep
/// matching their generators' raw ids) and the manually-scheduled heats a round never
/// generated.
fn unscope_heat_id(round: &RoundDef, heat: &HeatId) -> String {
    heat.0
        .strip_prefix(&format!("{}-", round.id.0))
        .map(str::to_owned)
        .unwrap_or_else(|| heat.0.clone())
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
/// For each of the round's [`completed_heats`] this keeps the smallest
/// [`Placement::best_lap_micros`](gridfpv_engine::scoring::Placement::best_lap_micros) per
/// competitor — the fastest single lap the **adjudicated** scoring computed (thrown-out / voided
/// laps already excluded, inserted / adjusted laps already included), *independent* of the round's
/// win condition. Sourcing it from the same adjudicated heat result the standings rank over (rather
/// than the raw lap list) means a thrown-out lap no longer counts as a pilot's best (#226). A
/// competitor with no completed lap across the round is absent from the map.
fn round_best_laps(round: &RoundDef, events: &[Event]) -> BTreeMap<CompetitorRef, i64> {
    let mut best: BTreeMap<CompetitorRef, i64> = BTreeMap::new();
    for heat in completed_heats(round, events) {
        for place in &heat.result.places {
            // A DISQUALIFIED placement contributes nothing: the DQ voids the heat's laps for
            // that pilot, so a lap flown in it must not stand as their best (#339 — the ranking
            // already excludes the DQ'd placement, #331; the displayed metrics must match).
            // The pilot's other, clean heats still count.
            if place.disqualified {
                continue;
            }
            if let Some(lap) = place.best_lap_micros {
                best.entry(place.competitor.competitor.clone())
                    .and_modify(|existing| *existing = (*existing).min(lap))
                    .or_insert(lap);
            }
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
    round_ranking_at(meta, round, events, 0)
}

/// Depth-carrying [`round_ranking`]: build the round's ranking with the seeding recursion `depth`
/// threaded through, so a `FromRanking` / `FromRankingRange` / `FromHeatWinners` source — itself a
/// cross-round seeding hop into [`round_field_at`] — stays bounded by the depth guard (a cross-round
/// seeding cycle returns [`FillError::SeedingTooDeep`] instead of overflowing the stack).
fn round_ranking_at(
    meta: &EventMeta,
    round: &RoundDef,
    events: &[Event],
    depth: usize,
) -> Result<Vec<RankEntry>, FillError> {
    let field = round_field_at(meta, round, events, depth)?;
    let registry = FormatRegistry::standard();
    let generator = registry
        .build(&round.format, &format_config(round, field))
        .ok_or_else(|| FillError::UnknownFormat(round.format.clone()))?;
    let completed = completed_heats(round, events);
    Ok(generator.ranking(&completed))
}

// --- Per-round standings (time-trial / qual display) ----------------------------------------

/// The **win-condition metric** a round's standings are ranked by — the tagged mirror of the
/// round's [`win_condition`](RoundDef::win_condition), carrying the value the ranking is *by*.
///
/// This is the headline number the Rounds stage shows next to each pilot: the fastest single lap
/// ([`BestLap`](RoundMetric::BestLap)), the fastest N-consecutive-lap window
/// ([`BestConsecutive`](RoundMetric::BestConsecutive)), or the most laps banked
/// ([`MostLaps`](RoundMetric::MostLaps)) — mapped from the win condition exactly as
/// [`qual_metric_for`] derives the qualifying metric. The lap-time variants carry `None` for a
/// pilot who set no qualifying value (no lap / fewer than `n` laps), so a no-show still renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum RoundMetric {
    /// [`WinCondition::BestLap`] (and the non-qualifying [`WinCondition::FirstToLaps`]): the pilot's
    /// fastest single lap across the round, in microseconds, or `None` if they completed no lap.
    BestLap {
        /// Fastest single lap (µs), or `None` for no completed lap.
        #[ts(type = "number | null")]
        micros: Option<i64>,
    },
    /// [`WinCondition::BestConsecutive`]: the pilot's smallest sum of `n` consecutive laps across
    /// the round, in microseconds, or `None` if they never completed `n` consecutive laps.
    BestConsecutive {
        /// How many consecutive laps the window spans.
        n: u32,
        /// Smallest consecutive-window sum (µs), or `None` if fewer than `n` laps.
        #[ts(type = "number | null")]
        micros: Option<i64>,
    },
    /// [`WinCondition::Timed`] (Most Laps): the most laps the pilot banked in any single heat.
    MostLaps {
        /// Most laps in a heat (0 if the pilot completed no lap).
        laps: u32,
    },
}

/// One pilot's **per-round standing** for the Rounds stage's time-trial (timed_qual) display.
///
/// Built by [`round_standings`] for a single round: each pilot's [`position`](RoundStanding::position)
/// (exactly the [`round_ranking`] order, so the standings and the ranking never disagree), their
/// **best single lap** ([`best_lap_micros`](RoundStanding::best_lap_micros), *always* computed so the
/// UI can show a Best-lap column regardless of win condition), their **most laps in a heat**
/// ([`laps`](RoundStanding::laps)), and the win-condition [`metric`](RoundStanding::metric) the
/// ranking is by. Pure + deterministic — the same log + meta always yields the same standings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct RoundStanding {
    /// The competitor this standing is for (the pilot's source-local handle).
    pub competitor: CompetitorRef,
    /// 1-based position; tied competitors share a position with the same dense, tie-aware
    /// "1, 2, 2, 4" convention as [`RankEntry`] — and exactly matches [`round_ranking`].
    pub position: u32,
    /// The pilot's **best single lap** across the round's heats, in microseconds, or `None` when
    /// they completed no lap. *Always* computed (independent of the win condition) so a Best-lap
    /// column can sit alongside the ranking metric.
    #[ts(type = "number | null")]
    pub best_lap_micros: Option<i64>,
    /// The most laps the pilot banked in any single heat (under the round's win condition) — the
    /// most-laps metric value and lap-count context. `0` for a pilot who completed no lap.
    pub laps: u32,
    /// The win-condition metric the ranking is *by* (the tagged mirror of the round's win
    /// condition). `best_lap_micros` is always present alongside this.
    pub metric: RoundMetric,
}

/// A round's **standings** for the time-trial (timed_qual) display (designed for it, sensible for
/// any format): one [`RoundStanding`] per pilot, in [`round_ranking`] order.
///
/// For each pilot it computes, across the round's [`completed_heats`] (scored under the round's
/// [`win_condition`](RoundDef::win_condition)):
///
/// - **`best_lap_micros`** — their fastest single lap, folded from the same finalized heats via the
///   lap-list projection ([`round_best_laps`]), *independent* of the win condition (so a most-laps
///   round still shows a real best lap). Always present.
/// - **`laps`** — the most laps they banked in any single heat (the win-condition lap count).
/// - **`metric`** — the value the ranking is by, built from the win condition like
///   [`qual_metric_for`]: [`BestConsecutive`](WinCondition::BestConsecutive) → the smallest
///   consecutive-window sum across their heats; [`Timed`](WinCondition::Timed) → most laps;
///   [`BestLap`](WinCondition::BestLap) / [`FirstToLaps`](WinCondition::FirstToLaps) → the best lap.
/// - **`position`** — reused straight from [`round_ranking`] (`generator.ranking(completed)`), so the
///   standings positions are byte-for-byte the ranking's, including the whole-field seeding (a no-show
///   still appears, ranked last, with a null metric).
///
/// Pure and deterministic — the same log + meta always yields the same standings.
pub fn round_standings(
    meta: &EventMeta,
    round: &RoundDef,
    events: &[Event],
) -> Result<Vec<RoundStanding>, FillError> {
    // Positions come straight from the ranking, so standings + ranking can never disagree — and the
    // whole-field seeding (no-shows ranked last) is inherited for free.
    let ranking = round_ranking(meta, round, events)?;
    // The round's scored heats (the same view the ranking ranked over) for the win-condition metric
    // + lap counts, and the lap-list best single lap per pilot (win-condition-independent).
    let completed = completed_heats(round, events);
    let best_laps = round_best_laps(round, events);

    // Per-pilot aggregates across the round's heats: most laps in a heat, and the smallest
    // best-consecutive window sum (when the round is scored under BestConsecutive).
    let mut most_laps: BTreeMap<CompetitorRef, u32> = BTreeMap::new();
    let mut best_consec: BTreeMap<CompetitorRef, i64> = BTreeMap::new();
    for heat in &completed {
        for place in &heat.result.places {
            // A DISQUALIFIED placement contributes NO metric to the standings row (#339): the
            // ranking already excludes it (#331), so the row must not keep surfacing the DQ'd
            // heat's lap count / consecutive window next to a position that ignored them.
            // The pilot's other, clean heats still aggregate.
            if place.disqualified {
                continue;
            }
            let competitor = place.competitor.competitor.clone();
            most_laps
                .entry(competitor.clone())
                .and_modify(|m| *m = (*m).max(place.laps))
                .or_insert(place.laps);
            if let Metric::BestConsecutiveMicros(Some(sum)) = place.metric {
                best_consec
                    .entry(competitor)
                    .and_modify(|s| *s = (*s).min(sum))
                    .or_insert(sum);
            }
        }
    }

    Ok(ranking
        .into_iter()
        .map(|entry| {
            let competitor = entry.competitor;
            let best_lap_micros = best_laps.get(&competitor).copied();
            let laps = most_laps.get(&competitor).copied().unwrap_or(0);
            let metric = match round.win_condition {
                WinCondition::BestConsecutive { n } => RoundMetric::BestConsecutive {
                    n,
                    micros: best_consec.get(&competitor).copied(),
                },
                WinCondition::Timed { .. } => RoundMetric::MostLaps { laps },
                // Best lap and the non-qualifying First-to-N both display the best single lap.
                WinCondition::BestLap | WinCondition::FirstToLaps { .. } => RoundMetric::BestLap {
                    micros: best_lap_micros,
                },
            };
            RoundStanding {
                competitor,
                position: entry.position,
                best_lap_micros,
                laps,
                metric,
            }
        })
        .collect())
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

/// One competitor's **counted laps** across a heat's [`HeatResult`] (0 when absent).
fn placement_laps(result: &HeatResult, competitor: &CompetitorRef) -> u32 {
    result
        .places
        .iter()
        .find(|p| &p.competitor.competitor == competitor)
        // A DISQUALIFIED placement banks no laps — the DQ voided the result those laps
        // decided (#339; `round_standings` and `round_best_laps` already skip them, and the
        // class total must agree with the round rows it aggregates).
        .filter(|p| !p.disqualified)
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
        // The round's scored heats — the same view `round_ranking` ranked over — so the laps a
        // standing reports come from exactly the heats that decided the round position.
        let completed = completed_heats(round, events);
        // An UNRACED round contributes nothing (user-approved policy): with zero completed
        // heats its "ranking" seeds the whole field tied at 1, so folding it handed every
        // member `field_size` free points and a phantom rounds_entered — defining the finals
        // rounds up front visibly shifted the standings after qualifying alone (and unevenly,
        // for rounds whose seeded field differs).
        if completed.is_empty() {
            continue;
        }
        let ranking = round_ranking(meta, round, events)?;
        let field_size = ranking.len() as u32;
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
    let outcome = match round.channel_mode {
        ChannelMode::Static => fill_round_static(meta, timers, round, events)?,
        ChannelMode::PerHeat => fill_round_per_heat(meta, round, round_id, events)?,
    };
    Ok(diagnose_complete(meta, round, round_id, events, outcome))
}

/// Tell a `Complete` that means "everything raced" apart from one that means "this format
/// refuses this field" (#394), promoting the latter to [`FillOutcome::Blocked`] with its reason.
///
/// A generator has only `Run`/`Complete` to answer with, so a refusal arrives here indelibly
/// stamped "complete". The one thing that separates the two, from outside the generator, is
/// **history**: a round that genuinely completed scheduled heats along the way. A round that
/// reports complete having **never scheduled a heat** did not finish — it never started, and if
/// its format also declares a field precondition this round fails, that precondition is the
/// reason. Both conditions are required: "no heats yet" alone is also how a legitimately empty
/// format behaves, and a declared shortfall alone would mis-explain a round that raced its heats
/// before a pilot was dropped from the field.
///
/// One insertion point for both channel modes, deliberately — a refusal must not be reportable
/// on one path and generic on the other.
fn diagnose_complete(
    meta: &EventMeta,
    round: &RoundDef,
    round_id: &RoundId,
    events: &[Event],
    outcome: FillOutcome,
) -> FillOutcome {
    if outcome != FillOutcome::Complete || !scheduled_round_heats(events, round_id).is_empty() {
        return outcome;
    }
    // The field is resolved (not assumed) so the count in the message is the one this round
    // would actually race. An unresolvable field is a `FillError` on its own terms, already
    // reported by the fill above — never silently reinterpreted as a shortfall here.
    let Ok(field) = round_field(meta, round, events) else {
        return outcome;
    };
    match gridfpv_engine::preconditions::field_shortfall(&round.format, field.len()) {
        Some(shortfall) => FillOutcome::Blocked {
            reason: shortfall.to_string(),
        },
        None => outcome,
    }
}

/// A round's **materialized heat plan**: the heat id the round logs, its lineup, and the channel
/// assignment when the formation path already chose one.
///
/// One shape for both formation paths (per-heat generator / static channel-balanced) so the fill
/// (which emits the next un-scheduled plan) and the **re-materialization** of an edited round's
/// already-scheduled heats (#387) read the same plan, never two drifting derivations of it.
#[derive(Debug, Clone)]
struct HeatPlan {
    /// The heat id **as logged** — round-scoped (`{round}-{generator-id}`, or `{round}-heat` for
    /// open practice).
    heat: HeatId,
    /// The generator's RAW (pre-scoping) id, when it differs from [`heat`](Self::heat): a round
    /// filled before id scoping existed logged that form, so a match must recognize both or an
    /// upgrade mid-round would double-schedule the round's remaining heats.
    raw: Option<HeatId>,
    /// The heat lineup, in the plan's seeding order.
    lineup: Vec<CompetitorRef>,
    /// `Some` when the formation path itself chose the channels — static (each member's fixed
    /// membership channel) or open practice (empty: the lineup *is* the channels). `None` for a
    /// per-heat round, whose channels are first-fit from the timer pool by the caller.
    frequencies: Option<Vec<(CompetitorRef, u16)>>,
}

impl HeatPlan {
    /// Whether `heat` (an id as it appears in the log) is this plan's heat — under either the
    /// round-scoped id or the raw generator id a pre-scoping fill logged.
    fn is(&self, heat: &HeatId) -> bool {
        &self.heat == heat || self.raw.as_ref() == Some(heat)
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
    let (plans, field_draw) = per_heat_plans(meta, round, round_id, events)?;
    if plans.is_empty() {
        return Ok(FillOutcome::Complete);
    }
    // The interactive flow schedules **one** heat per FillRound (the RD drives each heat to
    // Finalize before asking for the next). A generator that emits several plans at once (a
    // bracket round) still advances one heat at a time: take the first not-yet-scheduled plan.
    // Dedup against already-tagged heats so a repeated FillRound before the prior heat is scored
    // does not double-schedule it.
    let already: Vec<HeatId> = scheduled_round_heats(events, round_id);
    match plans
        .into_iter()
        .find(|plan| !already.iter().any(|heat| plan.is(heat)))
    {
        Some(plan) => Ok(FillOutcome::Scheduled {
            heat: plan.heat,
            lineup: plan.lineup,
            // Per-heat: the handler assigns channels from the timer pool (first-fit), except for
            // open practice which carries empty frequencies (the lineup is channels).
            frequencies: plan.frequencies,
            field_draw,
        }),
        // Every plan the generator wants this step is already scheduled (the RD re-issued
        // FillRound before scoring the outstanding heat): nothing new to append. Report
        // [`AlreadyScheduled`] — a typed ok the handler answers without appending, distinct from a
        // finished round.
        None => Ok(FillOutcome::AlreadyScheduled),
    }
}

/// The **per-heat** plans the round's generator wants at its current step, plus the round's field
/// draw to record if this is its freeze-at-fill moment (#334).
///
/// Every generator heat id is scoped to the round. A generator's ids are only unique WITHIN its own
/// round (`h2h-h0`, `tq-r1-h0`, …): two rounds of the same format in one event would log colliding
/// `HeatScheduled` ids, and every by-id fold (heat state, windows, live control) would then
/// conflate two different heats — corrupted results. Scoping with the round id
/// (`{round}-{generator-id}`) makes ids globally unique while staying deterministic; the raw id is
/// kept alongside so a round filled before scoping existed still matches its logged heats, and
/// `unscope_heat_id` strips the prefix when history is handed back to the generator. (Open practice
/// keeps its dedicated `{round}-heat` form, issue #54.)
///
/// An empty plan list means the round is finished (the generator reported `Complete`, or the
/// [`MAX_HEATS_PER_ROUND`] guard tripped).
fn per_heat_plans(
    meta: &EventMeta,
    round: &RoundDef,
    round_id: &RoundId,
    events: &[Event],
) -> Result<(Vec<HeatPlan>, Option<Vec<CompetitorRef>>), FillError> {
    let field = round_field(meta, round, events)?;
    if field.is_empty() {
        return Err(FillError::EmptyField(round_id.0.clone()));
    }
    // FREEZE-AT-FILL (#334): a carry seeding records its resolved draw alongside the round's
    // FIRST scheduled heat (the handler appends `RoundFieldDrawn` before the `HeatScheduled`).
    // From then on `round_field` replays the recorded draw — see `round_field_at`.
    let field_draw = (seeding_freezes(&round.seeding)
        && recorded_field(events, round_id).is_none())
    .then(|| field.clone());

    let registry = FormatRegistry::standard();
    let mut generator = registry
        .build(&round.format, &format_config(round, field))
        .ok_or_else(|| FillError::UnknownFormat(round.format.clone()))?;

    let completed = completed_heats(round, events);
    if completed.len() >= MAX_HEATS_PER_ROUND {
        return Ok((Vec::new(), field_draw));
    }

    // Open practice (open-practice format): the heat carries **empty** frequencies — its lineup is
    // the active *channels* themselves (`node-{i}` seats), so there is nothing to allocate. Force
    // `Some(empty)` so the caller logs the `HeatScheduled` with no frequencies regardless of the
    // timer's channel pool.
    let open_practice = is_open_practice(round);
    let plans = match generator.next(&completed) {
        GeneratorStep::Run(plans) => plans
            .into_iter()
            .map(|plan| {
                let raw = plan.heat;
                let heat = if open_practice {
                    open_practice_heat_id(round_id)
                } else {
                    scoped_heat_id(round_id, &raw)
                };
                HeatPlan {
                    raw: (raw != heat).then_some(raw),
                    heat,
                    lineup: plan.lineup,
                    frequencies: open_practice.then(Vec::new),
                }
            })
            .collect(),
        GeneratorStep::Complete => Vec::new(),
    };
    Ok((plans, field_draw))
}

/// Fill a **static** (time-trial / qual) round with **channel-balanced** heats (race redesign Slice
/// 7a).
///
/// Static rounds give each member a *fixed* channel at membership; [`static_plans`] builds the
/// round's full, deterministic plan of channel-balanced heats and this emits the next
/// not-yet-scheduled one (one per FillRound), or [`Complete`](FillOutcome::Complete) once every
/// planned heat is scheduled. Each emitted heat carries its pilots' assigned channels as
/// `frequencies` (no first-fit).
fn fill_round_static(
    meta: &EventMeta,
    timers: &TimerRegistry,
    round: &RoundDef,
    events: &[Event],
) -> Result<FillOutcome, FillError> {
    let plans = static_plans(meta, timers, round, events)?;
    let already: Vec<HeatId> = scheduled_round_heats(events, &round.id);
    // One heat per FillRound: emit the first plan not already scheduled (dedup like per-heat).
    match plans
        .into_iter()
        .find(|plan| !already.iter().any(|heat| plan.is(heat)))
    {
        Some(plan) => Ok(FillOutcome::Scheduled {
            heat: plan.heat,
            lineup: plan.lineup,
            frequencies: plan.frequencies,
            // Static rounds are validated FromRoster-only, and roster seedings never freeze (late
            // entrants stay welcome) — no draw to record.
            field_draw: None,
        }),
        // Fixed-count: every planned channel-balanced heat is already scheduled → the round is
        // complete. (Open-ended always finds a fresh heat above, so it never lands here.)
        None => Ok(FillOutcome::Complete),
    }
}

/// The **static** (channel-balanced) plan for a round: each heat draws pilots on **distinct
/// channels**, **≤ the timer's enabled node count** (the node cap is the only per-heat size limit;
/// the channel pool may be larger), repeated over the format's round count so every member flies
/// each round.
///
/// A member with no assigned channel is a [`FillError::MissingChannel`]. An empty field is a
/// [`FillError::EmptyField`], as for per-heat.
fn static_plans(
    meta: &EventMeta,
    timers: &TimerRegistry,
    round: &RoundDef,
    events: &[Event],
) -> Result<Vec<HeatPlan>, FillError> {
    // Gather the round's members + their fixed channels (de-duplicated across eligible classes,
    // first occurrence wins — a member in two eligible classes flies once on their channel).
    let members = static_members(meta, round)?;
    if members.is_empty() {
        return Err(FillError::EmptyField(round.id.0.clone()));
    }

    // The node cap is the size of the event's primary timer's **enabled** node set (#412) — the
    // only per-heat size limit; with no resolvable timer, fall back to seating every distinct
    // channel in one heat (a pure-sim event still channel-balances by the distinct-channel rule,
    // just without a node cap).
    let node_cap = assignment_timer(meta, timers)
        .map(|t| t.seat_capacity())
        .filter(|n| *n > 0)
        .unwrap_or(usize::MAX);

    // How many times the whole field flies — the format's round count (e.g. `timed_qual` runs
    // `rounds` rounds). Channel-balanced heats are built per format-round so every member flies
    // each round, across the configured round count. `0` = open-ended (generate the next heat on
    // demand forever; see `static_round_count`).
    let format_rounds = static_round_count(round);

    // For the open-ended case, plan just enough of the (infinite) channel-balanced rotation to yield
    // one not-yet-scheduled heat: one extra format-round beyond what's already been generated. The
    // rotation + ids are deterministic, so this always surfaces the next heat and never completes —
    // the RD ends the round by simply not asking for more.
    let rounds_to_plan = if format_rounds == 0 {
        let per_round = channel_balanced_plan(round, &members, node_cap, 1)
            .len()
            .max(1);
        scheduled_round_heats(events, &round.id).len() / per_round + 1
    } else {
        format_rounds
    };

    Ok(
        channel_balanced_plan(round, &members, node_cap, rounds_to_plan)
            .into_iter()
            .map(|(heat, assignment)| HeatPlan {
                heat,
                raw: None,
                lineup: assignment.iter().map(|(c, _)| c.clone()).collect(),
                frequencies: Some(assignment),
            })
            .collect(),
    )
}

/// One heat **re-materialized** under its round's edited config (#387) — the heat id exactly as
/// already logged, plus the lineup, channel assignment, and label a fresh fill produces for it.
///
/// The caller appends each as a new [`Event::HeatScheduled`] for that heat: every by-id read
/// (lineup, class, round, frequencies, label) takes the heat's **most recent** schedule, so the
/// re-emitted event rewrites the heat in place rather than creating a second one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RematerializedHeat {
    /// The heat to rewrite — its **existing** logged id, so the heat keeps its identity, its
    /// position in the round, and therefore its friendly name.
    pub heat: HeatId,
    /// The freshly-formed lineup, in the plan's seeding order.
    pub lineup: Vec<CompetitorRef>,
    /// The freshly-assigned channels (raw MHz), empty when the round assigns none (open practice,
    /// or an event with no resolvable timer).
    pub frequencies: Vec<(CompetitorRef, u16)>,
    /// The heat's existing custom label, carried through so a rewrite never drops an RD-typed name.
    pub label: Option<String>,
}

/// Re-materialize a round's **still-`Scheduled`** heats against the round's (just-edited) config
/// (#387).
///
/// A scheduled heat bakes in the lineup and frequencies its round's config produced *at fill time*.
/// Editing the round used to leave those baked values untouched, so a heat filled before the edit
/// raced under the old config forever — a practice heat kept the old channel set, a qual heat kept
/// the old channel mode. This recomputes the round's plan under the CURRENT config and hands back
/// the rewrite for each heat that plan still covers.
///
/// **Only `Scheduled` heats are rewritten** (the binding rule): staged / armed / running /
/// unofficial heats are refused the edit outright by
/// [`update_round`](crate::events::EventRegistry::update_round), and a `Final` heat is protected by
/// the raced-freeze on the round's scoring config. Heats whose id the new plan no longer produces
/// (an edit that changes the id scheme, e.g. flipping the channel mode) are **left alone** — the
/// fill dedup is by id, so rewriting them under a different id would either orphan or duplicate
/// them; the RD discards those and re-fills.
///
/// Format-agnostic by construction: it goes through the same [`HeatPlan`] both fill paths use, so
/// open practice, static qualifying, and bracket rounds all re-materialize the same way.
///
/// Returns only the heats whose lineup or channels **actually changed** — a label-only round edit
/// appends nothing.
pub fn rematerialize_round_heats(
    meta: &EventMeta,
    timers: &TimerRegistry,
    round_id: &RoundId,
    events: &[Event],
) -> Vec<RematerializedHeat> {
    let Ok(round) = round_of(meta, round_id) else {
        return Vec::new();
    };
    let targets: Vec<HeatId> = scheduled_round_heats(events, round_id)
        .into_iter()
        .filter(|heat| heat_state(events, heat) == Some(HeatState::Scheduled))
        .collect();
    if targets.is_empty() {
        return Vec::new();
    }

    // A round whose new config cannot even be planned (an empty field, a member with no channel)
    // has nothing to re-materialize — the edit itself still stands, and the RD's next fill surfaces
    // the real error. Never fail the edit on it.
    let plans = match round.channel_mode {
        ChannelMode::Static => static_plans(meta, timers, round, events).unwrap_or_default(),
        ChannelMode::PerHeat => per_heat_plans(meta, round, round_id, events)
            .map(|(plans, _draw)| plans)
            .unwrap_or_default(),
    };

    let mut out = Vec::new();
    for heat in targets {
        let Some(plan) = plans.iter().find(|plan| plan.is(&heat)) else {
            continue;
        };
        let frequencies = match &plan.frequencies {
            Some(freqs) => freqs.clone(),
            // Per-heat: first-fit from the timer's pool, exactly as the fill handler does. An
            // unassignable lineup (over the node cap, too few channels) leaves the heat as it is —
            // a stale heat beats a channel-less one.
            None => match assign_for_event(meta, timers, &plan.lineup) {
                Ok(freqs) => freqs,
                Err(_) => continue,
            },
        };
        let (lineup_now, frequencies_now, label) = logged_schedule(events, &heat);
        if lineup_now == plan.lineup && frequencies_now == frequencies {
            continue;
        }
        out.push(RematerializedHeat {
            heat,
            lineup: plan.lineup.clone(),
            frequencies,
            label,
        });
    }
    out
}

/// The lineup, channel assignment and RD-typed label a heat was **last scheduled** with — the
/// public read of [`logged_schedule`], for a caller that has to describe a heat already in the log
/// rather than one it just drew (the `Advance` ack names the heat it loaded, #401).
pub fn logged_heat_schedule(
    events: &[Event],
    heat: &HeatId,
) -> (
    Vec<CompetitorRef>,
    Vec<(CompetitorRef, u16)>,
    Option<String>,
) {
    logged_schedule(events, heat)
}

/// A heat's currently-effective schedule — `(lineup, frequencies, label)` from its **most recent**
/// [`Event::HeatScheduled`], the same "latest wins" rule the live heat list folds by.
fn logged_schedule(
    events: &[Event],
    heat: &HeatId,
) -> (
    Vec<CompetitorRef>,
    Vec<(CompetitorRef, u16)>,
    Option<String>,
) {
    let mut out = (Vec::new(), Vec::new(), None);
    for event in events {
        if let Event::HeatScheduled {
            heat: h,
            lineup,
            frequencies,
            label,
            ..
        } = event
        {
            if h == heat {
                out = (lineup.clone(), frequencies.clone(), label.clone());
            }
        }
    }
    out
}

/// The heat **loaded on the timer** — the heat live control is showing/driving.
///
/// It is the heat referenced by the last event among `{HeatStateChanged, CurrentHeatSelected}`: a
/// real heat-loop transition (Stage/Start/…) or the RD's explicit "control *this* heat" selection.
/// Deliberately narrower than `LiveRaceState::current_heat`, which additionally falls back to the
/// **first `HeatScheduled`** so the very first heat of a fresh event is controllable before anything
/// has happened. That fallback is a display convenience, not a statement that a heat is loaded —
/// treating it as one would refuse every round edit in a fresh event, including the
/// practice-channel edit #387 exists to make work.
pub fn heat_on_timer(events: &[Event]) -> Option<HeatId> {
    events.iter().rev().find_map(|event| match event {
        Event::HeatStateChanged { heat, .. } | Event::CurrentHeatSelected { heat } => {
            Some(heat.clone())
        }
        _ => None,
    })
}

/// The fixed display name for an open-practice round's single auto-created heat — the server-side
/// twin of the console's `OPEN_PRACTICE_HEAT_NAME` (`frontend/.../lib/heats.ts`).
const OPEN_PRACTICE_HEAT_NAME: &str = "Practice Heat";

/// The **friendly display name** of a heat within its round — the server-side twin of the console's
/// `heatNameById` / `heatDisplayName` (`frontend/apps/rd-console/src/lib/heats.ts`), for the
/// RD-facing messages the server writes (a raw heat id must never reach a user — repo display rule).
///
/// Same convention as the console, in the same order:
/// - a manually-built heat's RD-typed `label` wins;
/// - an **open-practice** round's single heat → "Practice Heat";
/// - a **multi-main** round's heats are tiered mains → "A-Main", "B-Main", …;
/// - every other heat → "‹Round label› Heat N", N being its 1-based position in the round.
pub fn heat_display_name(round: &RoundDef, events: &[Event], heat: &HeatId) -> String {
    if let (_, _, Some(label)) = logged_schedule(events, heat) {
        let label = label.trim().to_string();
        if !label.is_empty() {
            return label;
        }
    }
    if round.format == gridfpv_engine::format::OpenPractice::NAME {
        return OPEN_PRACTICE_HEAT_NAME.to_string();
    }
    let in_round = scheduled_round_heats(events, &round.id);
    let index = in_round.iter().position(|h| h == heat);
    if round.format == "multi_main" {
        // The main's tier is its position in the round (A=first, B=second, …); a not-yet-listed
        // heat is the next main, matching the console.
        return main_tier_name(index.unwrap_or(in_round.len()));
    }
    format!(
        "{} Heat {}",
        round.label,
        index.map_or(in_round.len() + 1, |i| i + 1)
    )
}

/// The tier name for the main at 0-based `index`: 0 → "A-Main", 1 → "B-Main", … matching the
/// engine's `MultiMain` tier labels and the console's `mainTierName`. Past the alphabet
/// (vanishingly unlikely) it falls back to "Main N" so the name stays unique and readable.
fn main_tier_name(index: usize) -> String {
    match u8::try_from(index) {
        Ok(i) if index < 26 => format!("{}-Main", (b'A' + i) as char),
        _ => format!("Main {}", index + 1),
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
///
/// **`0` means open-ended** (the RD set "Heats per pilot" to 0): instead of a fixed number of
/// rounds, the round generates the next heat on demand each fill, indefinitely, until the RD stops
/// — see [`fill_round_static`]. An absent param falls back to the format default (never 0), so
/// open-ended is only ever an explicit choice.
fn static_round_count(round: &RoundDef) -> usize {
    let default = match round.format.as_str() {
        "timed_qual" | "round_robin" => 3,
        _ => 1,
    };
    let rounds: usize = round
        .params
        .get("rounds")
        .and_then(|v| v.parse().ok())
        .unwrap_or(default);
    // 0 stays the explicit open-ended sentinel; a positive count clamps so the materialized
    // full plan is bounded (an unchecked raw-API `rounds=999999999` would otherwise allocate
    // unbounded memory at fill). Mirrors the generator-side clamp.
    if rounds == 0 {
        0
    } else {
        rounds.min(gridfpv_engine::timed_qual::TimedQualifying::MAX_ROUNDS)
    }
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
pub fn scheduled_round_heats(events: &[Event], round_id: &RoundId) -> Vec<HeatId> {
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
    use gridfpv_events::{AdapterId, GateIndex, HeatTransition, LogRef, Pass, Penalty, SourceTime};
    use std::collections::BTreeMap;

    const ADAPTER: &str = "mock";

    // --- Channel assignment (race redesign Slice 4a) ---------------------------------------

    use crate::channels::RACEBAND_MHZ;
    use crate::timers::{ChannelCapability, Timer, TimerId, TimerKind, TimerStatus};

    /// A test timer with the given node count + available channels (raw MHz), flexible capability.
    fn timer_with(node_count: u32, available: Vec<u16>) -> Timer {
        timer_with_disabled(node_count, available, Vec::new())
    }

    /// As [`timer_with`], with `disabled` node indices (0-based) switched off (#412).
    fn timer_with_disabled(node_count: u32, available: Vec<u16>, disabled: Vec<u32>) -> Timer {
        Timer {
            id: TimerId("t".into()),
            name: "T".into(),
            kind: TimerKind::Mock { laps: 1, lap_ms: 1 },
            status: TimerStatus::Ready,
            channel_capability: ChannelCapability::Flexible,
            node_count: Some(node_count),
            reported_nodes: None,
            disabled_nodes: disabled,
            available_channels: available,
            plugin: None,
            manual_connect: false,
            calibration: Vec::new(),
            node_channels: Vec::new(),
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
    fn assign_caps_the_heat_at_the_enabled_node_set_not_the_timers_width() {
        // #412: a 4-node timer with node index 2 disabled seats THREE pilots, and the three seats
        // are 0, 1 and 3 — a set with a hole, not a prefix. A fourth pilot is refused here rather
        // than seated on the dead gate.
        let timer = timer_with_disabled(4, RACEBAND_MHZ.to_vec(), vec![2]);
        assert_eq!(timer.enabled_nodes(), vec![0, 1, 3]);

        let err = assign_frequencies(&timer, &lineup(&["A", "B", "C", "D"])).unwrap_err();
        assert_eq!(
            err,
            AssignError::TooManyForNodes {
                lineup: 4,
                // The cap is the size of the enabled set, not the width — the number the RD sees
                // as "3 nodes usable" rather than "4 nodes".
                nodes: 3
            }
        );

        // Three fit, and each gets a channel.
        let ok = assign_frequencies(&timer, &lineup(&["A", "B", "C"])).unwrap();
        assert_eq!(ok.len(), 3);
        // …and they land on nodes 0, 1 and 3. The channel allocation is by lineup order; the seat
        // mapping is what turns that order into real node indices.
        let seats = timer.seat_nodes(&lineup(&["A", "B", "C"]));
        assert_eq!(
            seats.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            vec![0, 1, 3]
        );
    }

    #[test]
    fn the_channel_pool_is_capped_by_the_enabled_nodes_too() {
        // A disabled node is not offered a channel: the candidate pool a heat's IMD pick is drawn
        // from is `enabled` wide, not `width` wide.
        let timer = timer_with_disabled(4, RACEBAND_MHZ.to_vec(), vec![2]);
        let ok = assign_frequencies(&timer, &lineup(&["A", "B", "C"])).unwrap();
        let chosen: Vec<u16> = ok.iter().map(|(_, mhz)| *mhz).collect();
        // Three distinct channels, all from the first three of the Raceband pool (the pool is
        // `take(3)` — the enabled count — before the IMD pick).
        assert_eq!(chosen.len(), 3);
        for mhz in &chosen {
            assert!(
                RACEBAND_MHZ[..3].contains(mhz),
                "{mhz} came from outside the enabled-node-capped pool"
            );
        }
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
            min_lap_secs: None,
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
            label: None,
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
            heat: None,
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

    #[test]
    fn open_ended_static_round_keeps_generating_the_next_heat() {
        // "Heats per pilot = 0" on a static (time-trial) round: each fill yields the next heat in
        // the continuing channel-balanced rotation, and it never completes.
        let mut round = qual_round("tt", "open");
        round.channel_mode = ChannelMode::Static;
        round.params.insert("rounds".into(), "0".into());
        // 3 pilots on distinct channels, a 2-node timer -> 2 heats per format-round (A&B, then C).
        let (meta, timers) = meta_with_timer(
            vec![round],
            vec![member_chan(
                "open",
                &[("A", 5658), ("B", 5695), ("C", 5760)],
            )],
            2,
        );
        let rid = RoundId("tt".into());

        let mut events: Vec<Event> = Vec::new();
        let mut ids: Vec<String> = Vec::new();
        for _ in 0..5 {
            match fill_round(&meta, &timers, &rid, &events).unwrap() {
                FillOutcome::Scheduled { heat, lineup, .. } => {
                    let names: Vec<&str> = lineup.iter().map(|c| c.0.as_str()).collect();
                    events.push(scheduled(&heat.0, "tt", "open", &names));
                    ids.push(heat.0);
                }
                other => {
                    panic!("open-ended round must always schedule another heat, got {other:?}")
                }
            }
        }
        let unique: std::collections::HashSet<_> = ids.iter().cloned().collect();
        assert_eq!(unique.len(), 5, "every generated heat is distinct: {ids:?}");
        assert!(ids[0].ends_with("-r1-h1"), "first heat is r1-h1: {ids:?}");
        assert!(
            ids.iter().any(|i| i.ends_with("-r2-h1")),
            "the rotation continues into round 2: {ids:?}"
        );
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

    // --- Per-round standings (time-trial / qual display) ----------------------------------------

    /// A scored heat over `pilots`, each flying the given absolute lap-gate pass times (µs), run to
    /// Final and tagged with the round + class. The first time is the holeshot; each later time
    /// completes a lap.
    fn scored_heat(heat: &str, round: &str, class: &str, pilots: &[(&str, &[i64])]) -> Vec<Event> {
        let names: Vec<&str> = pilots.iter().map(|(n, _)| *n).collect();
        let mut log = vec![scheduled(heat, round, class, &names)];
        let mut passes = Vec::new();
        let mut seq = 0u64;
        for (name, times) in pilots {
            for &t in *times {
                passes.push(pass(name, t, seq));
                seq += 1;
            }
        }
        log.extend(run_heat_events(heat, passes));
        log
    }

    /// The `(competitor, position)` pairs of a standings list — to assert parity with the ranking.
    fn positions(standings: &[RoundStanding]) -> Vec<(CompetitorRef, u32)> {
        standings
            .iter()
            .map(|s| (s.competitor.clone(), s.position))
            .collect()
    }

    /// Look up a standing by competitor name.
    fn standing<'a>(standings: &'a [RoundStanding], name: &str) -> &'a RoundStanding {
        standings
            .iter()
            .find(|s| s.competitor.0 == name)
            .unwrap_or_else(|| panic!("no standing for {name}"))
    }

    #[test]
    fn round_standings_best_consecutive_populates_metric_and_best_lap() {
        // A timed_qual round scored under BestConsecutive{3}: each pilot's best lap is always
        // computed, the metric carries the smallest 3-consec window, and positions match the ranking.
        let mut round = qual_round("q1", "open");
        round.win_condition = WinCondition::BestConsecutive { n: 3 };
        let meta = meta_with(vec![round.clone()], vec![member("open", &["A", "B"])]);
        // A: four 1.0s laps → best-3-consec 3.0s, best lap 1.0s.
        // B: four 1.5s laps → best-3-consec 4.5s, best lap 1.5s.
        let log = scored_heat(
            "q-1",
            "q1",
            "open",
            &[
                ("A", &[0, 1_000_000, 2_000_000, 3_000_000, 4_000_000]),
                ("B", &[0, 1_500_000, 3_000_000, 4_500_000, 6_000_000]),
            ],
        );

        let standings = round_standings(&meta, &round, &log).unwrap();
        // Positions are byte-for-byte the ranking's.
        let ranking = round_ranking(&meta, &round, &log).unwrap();
        assert_eq!(
            positions(&standings),
            ranking
                .iter()
                .map(|e| (e.competitor.clone(), e.position))
                .collect::<Vec<_>>()
        );

        let a = standing(&standings, "A");
        assert_eq!(a.position, 1);
        assert_eq!(a.best_lap_micros, Some(1_000_000));
        assert_eq!(
            a.metric,
            RoundMetric::BestConsecutive {
                n: 3,
                micros: Some(3_000_000)
            }
        );
        let b = standing(&standings, "B");
        assert_eq!(b.position, 2);
        assert_eq!(b.best_lap_micros, Some(1_500_000));
        assert_eq!(
            b.metric,
            RoundMetric::BestConsecutive {
                n: 3,
                micros: Some(4_500_000)
            }
        );
    }

    #[test]
    fn round_standings_timed_reports_most_laps_metric() {
        // A Timed round: the metric is MostLaps with the per-pilot lap counts, best lap still set.
        let mut round = qual_round("q1", "open");
        round.win_condition = WinCondition::Timed {
            window_micros: 100_000_000,
        };
        let meta = meta_with(vec![round.clone()], vec![member("open", &["A", "B"])]);
        // Within the 100s window: B completes 3 laps, A completes 2.
        let log = scored_heat(
            "q-1",
            "q1",
            "open",
            &[
                ("A", &[0, 10_000_000, 20_000_000]),
                ("B", &[0, 10_000_000, 20_000_000, 30_000_000]),
            ],
        );

        let standings = round_standings(&meta, &round, &log).unwrap();
        let b = standing(&standings, "B");
        assert_eq!(b.position, 1);
        assert_eq!(b.metric, RoundMetric::MostLaps { laps: 3 });
        assert_eq!(b.laps, 3);
        // Best lap is still folded from the real lap durations, independent of the win condition.
        assert_eq!(b.best_lap_micros, Some(10_000_000));
        let a = standing(&standings, "A");
        assert_eq!(a.metric, RoundMetric::MostLaps { laps: 2 });
        assert_eq!(a.laps, 2);
    }

    #[test]
    fn round_standings_best_lap_metric_equals_best_lap_micros() {
        // A BestLap round: the metric is BestLap and equals best_lap_micros for every pilot.
        let round = qual_round("q1", "open"); // win_condition defaults to BestLap
        let meta = meta_with(vec![round.clone()], vec![member("open", &["A", "B"])]);
        let log = scored_heat(
            "q-1",
            "q1",
            "open",
            &[
                ("A", &[0, 1_000_000, 3_000_000]), // laps 1.0s, 2.0s → best 1.0s
                ("B", &[0, 1_500_000, 3_000_000]), // laps 1.5s, 1.5s → best 1.5s
            ],
        );

        let standings = round_standings(&meta, &round, &log).unwrap();
        let a = standing(&standings, "A");
        assert_eq!(a.position, 1);
        assert_eq!(a.best_lap_micros, Some(1_000_000));
        assert_eq!(
            a.metric,
            RoundMetric::BestLap {
                micros: a.best_lap_micros
            }
        );
        let b = standing(&standings, "B");
        assert_eq!(
            b.metric,
            RoundMetric::BestLap {
                micros: Some(1_500_000)
            }
        );
    }

    #[test]
    fn round_standings_no_lap_pilot_ranks_last_with_null_metric() {
        // Z only crosses the holeshot (no completed lap) → ranks last with a null metric + best lap.
        let round = qual_round("q1", "open"); // BestLap
        let meta = meta_with(vec![round.clone()], vec![member("open", &["A", "Z"])]);
        let log = scored_heat("q-1", "q1", "open", &[("A", &[0, 1_000_000]), ("Z", &[0])]);

        let standings = round_standings(&meta, &round, &log).unwrap();
        let z = standing(&standings, "Z");
        assert_eq!(z.position, 2);
        assert_eq!(z.best_lap_micros, None);
        assert_eq!(z.laps, 0);
        assert_eq!(z.metric, RoundMetric::BestLap { micros: None });
    }

    // --- #226: heat-level adjudications must reach standings / seeding / class results ------
    //
    // Before the fix, `completed_heats` scored each heat over a PASS-ONLY list (every
    // adjudication discarded), so the standings showed the raw on-track result while the heat
    // page (app.rs `HeatProjection::Result`) showed the adjudicated one. These drive an
    // adjudication through each projection and assert it MOVES the result — they fail on the old
    // pass-only path and pass on the shared full-window scorer (`crate::app::score_heat_window`).

    /// A 2-pilot `head_to_head` round (Placement scoring) over `class`, scored under `win`.
    /// Head-to-Head ranks by finishing position, so a DQ / time penalty that reshuffles the
    /// heat's placements reshuffles the round ranking too.
    fn h2h_round(id: &str, class: &str, win: WinCondition) -> RoundDef {
        RoundDef {
            id: RoundId(id.into()),
            label: id.into(),
            classes: vec![ScopeClassId(class.into())],
            format: "head_to_head".into(),
            params: BTreeMap::new(),
            win_condition: win,
            seeding: SeedingRule::FromRoster,
            channel_mode: ChannelMode::PerHeat,
            staging_timer_secs: default_staging_timer_secs(),
            start_procedure: StartProcedure::default(),
            grace_window: default_grace_window(),
            protest_window: gridfpv_engine::heat::ProtestWindow::Off,
            min_lap_secs: None,
            time_limit_secs: None,
        }
    }

    /// A `PenaltyApplied` for a competitor in a heat (DQ / time penalty).
    fn penalty_applied(heat: &str, competitor: &str, penalty: Penalty) -> Event {
        Event::PenaltyApplied {
            heat: HeatId(heat.into()),
            competitor: CompetitorRef(competitor.into()),
            penalty,
        }
    }

    /// The global append offset of a competitor's lap-gate pass at `at` µs — the `LogRef` a
    /// `LapThrownOut` / `RulingReversed` targets (offsets must be PRESERVED, not re-enumerated).
    fn pass_offset(log: &[Event], competitor: &str, at: i64) -> u64 {
        log.iter()
            .position(|e| {
                matches!(e, Event::Pass(p)
                    if p.competitor.0 == competitor && p.at == SourceTime::from_micros(at))
            })
            .expect("pass present in log") as u64
    }

    /// The competitor refs of a ranking, best-first.
    fn ranking_order(ranking: &[RankEntry]) -> Vec<String> {
        ranking.iter().map(|e| e.competitor.0.clone()).collect()
    }

    // --- #394: a refusal must not masquerade as a completion --------------------------------

    /// The bug, end to end at the fill path: a Head-to-Head round with a **single pilot** filled
    /// as `Complete`, which every layer above renders as "the round is complete or awaiting a
    /// score" — on a round where nothing has raced. It now comes back `Blocked`, carrying the
    /// shortfall in words.
    #[test]
    fn one_pilot_head_to_head_fill_is_blocked_and_names_the_shortfall() {
        let round = h2h_round("h2h", "open", WinCondition::FirstToLaps { n: 1 });
        let meta = meta_with(vec![round.clone()], vec![member("open", &["Solo"])]);

        let outcome = fill_round(&meta, &no_timers(), &round.id, &[]).unwrap();
        let FillOutcome::Blocked { reason } = outcome else {
            panic!("a one-pilot head-to-head round must not report Complete, got {outcome:?}");
        };
        // What the RD needs to act: the format, its requirement, their actual field, and the
        // format that would fit a solo pilot.
        assert!(reason.contains("Head-to-Head"), "{reason}");
        assert!(reason.contains("at least 2"), "{reason}");
        assert!(reason.contains("has 1"), "{reason}");
        assert!(reason.contains("timed_qual"), "{reason}");
        // The message is RD-facing: no raw round id leaks into it (repo display rule).
        assert!(
            !reason.contains("h2h"),
            "no raw round id in the reason: {reason}"
        );
    }

    /// The other side of the same coin — the refusal must not swallow a genuine completion. Two
    /// pilots fill normally, and once their heat is scored the round reports `Complete`, not
    /// `Blocked`.
    #[test]
    fn a_two_pilot_head_to_head_round_still_fills_and_then_completes() {
        let round = h2h_round("h2h", "open", WinCondition::FirstToLaps { n: 1 });
        let meta = meta_with(vec![round.clone()], vec![member("open", &["A", "B"])]);

        assert!(
            matches!(
                fill_round(&meta, &no_timers(), &round.id, &[]).unwrap(),
                FillOutcome::Scheduled { .. }
            ),
            "two pilots is a raceable head-to-head field"
        );

        let log = scored_heat(
            "h2h-h2h-h0",
            "h2h",
            "open",
            &[("A", &[0, 1_000_000]), ("B", &[0, 2_000_000])],
        );
        assert_eq!(
            fill_round(&meta, &no_timers(), &round.id, &log).unwrap(),
            FillOutcome::Complete,
            "a round that raced its heats is finished, not blocked"
        );
    }

    /// A round that DID schedule heats is finished, never "blocked", even if its field has since
    /// shrunk below the format's minimum — the shortfall only explains a round that never
    /// started. This is the guard that keeps `Blocked` meaning "nothing has raced and nothing
    /// can" rather than becoming a second, wrong label for a completed round.
    #[test]
    fn a_round_that_already_raced_reports_complete_even_if_its_field_shrank() {
        let round = h2h_round("h2h", "open", WinCondition::FirstToLaps { n: 1 });
        // Membership is down to one pilot, but the log shows the round already scheduled + scored
        // a heat back when it had two.
        let meta = meta_with(vec![round.clone()], vec![member("open", &["A"])]);
        let log = scored_heat(
            "h2h-h2h-h0",
            "h2h",
            "open",
            &[("A", &[0, 1_000_000]), ("B", &[0, 2_000_000])],
        );
        assert_eq!(
            fill_round(&meta, &no_timers(), &round.id, &log).unwrap(),
            FillOutcome::Complete
        );
    }

    #[test]
    fn dq_sinks_the_pilot_in_round_ranking_standings_and_class_standings() {
        // FirstToLaps head-to-head: A reaches lap 1 first (on-track winner), B second. A DQ on A
        // sinks A below B in EVERY projection — not the raw on-track order.
        let round = h2h_round("h2h", "open", WinCondition::FirstToLaps { n: 1 });
        let meta = meta_with(vec![round.clone()], vec![member("open", &["A", "B"])]);
        let mut log = scored_heat(
            "h2h-1",
            "h2h",
            "open",
            &[("A", &[0, 1_000_000]), ("B", &[0, 2_000_000])],
        );
        log.push(penalty_applied(
            "h2h-1",
            "A",
            Penalty::Disqualify { reason: None },
        ));

        // round_ranking: B first, A last (DQ'd) — not A-first by raw reach time.
        let ranking = round_ranking(&meta, &round, &log).unwrap();
        assert_eq!(ranking_order(&ranking), vec!["B", "A"]);

        // round_standings: positions mirror the ranking.
        let standings = round_standings(&meta, &round, &log).unwrap();
        assert_eq!(standing(&standings, "B").position, 1);
        assert_eq!(standing(&standings, "A").position, 2);

        // class_standings: B (round pos 1) outscores the DQ'd A (round pos 2).
        let class = class_standings(&meta, &ClassId("open".into()), &log).unwrap();
        assert_eq!(class.standings[0].competitor.0, "B");
        assert!(
            class.standings[0].points > class.standings[1].points,
            "the DQ'd pilot scores fewer class points"
        );
    }

    #[test]
    fn lap_thrown_out_reorders_ranking_and_drops_the_pilots_best_lap() {
        // timed_qual BestLap: A's fastest lap (1.0s) beats B (2.0s) on track. Throwing out that
        // lap leaves A with only a 3.0s lap — so B now ranks ahead AND A's standings best-lap
        // excludes the thrown-out lap.
        let round = qual_round("q1", "open"); // BestLap
        let meta = meta_with(vec![round.clone()], vec![member("open", &["A", "B"])]);
        let mut log = scored_heat(
            "q-1",
            "q1",
            "open",
            &[("A", &[0, 1_000_000, 4_000_000]), ("B", &[0, 2_000_000])],
        );

        // Sanity: on track A leads on best lap.
        assert_eq!(
            ranking_order(&round_ranking(&meta, &round, &log).unwrap()),
            vec!["A", "B"]
        );

        // Throw out A's 1.0s lap (the lap ENDING at the pass @1.0s).
        let target = pass_offset(&log, "A", 1_000_000);
        log.push(Event::LapThrownOut {
            target: LogRef(target),
        });

        // Ranking flips: A's remaining lap is 3.0s, slower than B's 2.0s.
        assert_eq!(
            ranking_order(&round_ranking(&meta, &round, &log).unwrap()),
            vec!["B", "A"]
        );

        let standings = round_standings(&meta, &round, &log).unwrap();
        assert_eq!(
            standing(&standings, "A").best_lap_micros,
            Some(3_000_000),
            "the thrown-out 1.0s lap is excluded from A's best lap"
        );
        assert_eq!(standing(&standings, "B").best_lap_micros, Some(2_000_000));
        assert_eq!(standing(&standings, "B").position, 1);
        assert_eq!(standing(&standings, "A").position, 2);
    }

    #[test]
    fn time_added_changes_finishing_order_under_first_to_laps() {
        // FirstToLaps head-to-head: A reaches lap 1 at 1.0s, B at 2.0s (A on-track winner). A
        // 3.0s TimeAdded pushes A's deciding reach-time to 4.0s — behind B's 2.0s, flipping it.
        let round = h2h_round("h2h", "open", WinCondition::FirstToLaps { n: 1 });
        let meta = meta_with(vec![round.clone()], vec![member("open", &["A", "B"])]);
        let mut log = scored_heat(
            "h2h-1",
            "h2h",
            "open",
            &[("A", &[0, 1_000_000]), ("B", &[0, 2_000_000])],
        );
        assert_eq!(
            ranking_order(&round_ranking(&meta, &round, &log).unwrap()),
            vec!["A", "B"]
        );

        log.push(penalty_applied(
            "h2h-1",
            "A",
            Penalty::TimeAdded { micros: 3_000_000 },
        ));
        assert_eq!(
            ranking_order(&round_ranking(&meta, &round, &log).unwrap()),
            vec!["B", "A"]
        );
    }

    #[test]
    fn heat_voided_is_excluded_from_the_projections_until_reversed() {
        // The RD's "Void heat" (false start / timer glitch) must count for NOTHING downstream:
        // the voided heat drops out of completed_heats — and with it round ranking, standings,
        // class points, and any dependent seeding — exactly as if it had never finalized. A
        // RulingReversed on the void brings it back.
        let round = qual_round("q1", "open");
        let meta = meta_with(vec![round.clone()], vec![member("open", &["A", "B"])]);
        let mut log = scored_heat(
            "q-1",
            "q1",
            "open",
            &[("A", &[0, 1_000_000]), ("B", &[0, 2_000_000])],
        );
        let void_offset = log.len() as u64;
        log.push(Event::HeatVoided {
            heat: HeatId("q-1".into()),
        });

        // Voided → the heat contributes nothing: no completed heats, no ranked metrics.
        assert!(
            completed_heats(&round, &log).is_empty(),
            "a voided heat is excluded from the round's completed heats"
        );
        let standings = round_standings(&meta, &round, &log).unwrap();
        assert!(
            standings.iter().all(|s| s.best_lap_micros.is_none()),
            "no metric survives from a voided heat"
        );

        // Reversing the void restores the heat (and its results) in full.
        log.push(Event::RulingReversed {
            target: LogRef(void_offset),
        });
        let restored = completed_heats(&round, &log);
        assert_eq!(restored.len(), 1, "reversal brings the heat back");
        assert!(!restored[0].result.voided);
    }

    #[test]
    fn per_heat_result_and_round_standings_agree_on_an_adjudicated_heat() {
        // The split-brain (#226) closed: the per-heat result projection (app.rs
        // `HeatProjection::Result` → `score_heat_window`) and `round_standings` now score the
        // SAME adjudicated window, so they agree on who the DQ sinks.
        let round = h2h_round("h2h", "open", WinCondition::FirstToLaps { n: 1 });
        let meta = meta_with(vec![round.clone()], vec![member("open", &["A", "B"])]);
        let mut log = scored_heat(
            "h2h-1",
            "h2h",
            "open",
            &[("A", &[0, 1_000_000]), ("B", &[0, 2_000_000])],
        );
        log.push(penalty_applied(
            "h2h-1",
            "A",
            Penalty::Disqualify { reason: None },
        ));

        // The per-heat result the heat page shows, via the exact shared helper app.rs uses.
        let heat_result =
            crate::app::score_heat_window(&log, &HeatId("h2h-1".into()), round.win_condition, None);
        let dq_pilot = heat_result
            .places
            .iter()
            .find(|p| p.disqualified)
            .map(|p| p.competitor.competitor.0.clone());
        assert_eq!(dq_pilot.as_deref(), Some("A"), "the heat page DQs A");

        // round_standings, going independently through completed_heats → round_ranking, ranks A
        // last — the SAME pilot the heat page sinks.
        let standings = round_standings(&meta, &round, &log).unwrap();
        let last = standings.iter().max_by_key(|s| s.position).unwrap();
        assert_eq!(last.competitor.0, "A");
    }

    #[test]
    fn dq_heat_contributes_no_metric_or_best_lap_to_the_standings_row() {
        // The #339 asymmetry closed: the ranking excludes a DQ'd placement (#331), so the
        // standings row must not keep surfacing that heat's metric/best-lap next to the
        // position that ignored them. A, DQ'd in their ONLY heat, ranks last AND their row
        // carries no value — exactly like a no-show. B's clean row is untouched.
        let round = qual_round("q1", "open"); // BestLap
        let meta = meta_with(vec![round.clone()], vec![member("open", &["A", "B"])]);
        let mut log = scored_heat(
            "q-1",
            "q1",
            "open",
            &[("A", &[0, 1_000_000]), ("B", &[0, 2_000_000])],
        );
        log.push(penalty_applied(
            "q-1",
            "A",
            Penalty::Disqualify { reason: None },
        ));

        let standings = round_standings(&meta, &round, &log).unwrap();
        let a = standing(&standings, "A");
        assert_eq!(a.position, 2, "the DQ sinks A below B");
        assert_eq!(
            a.best_lap_micros, None,
            "the DQ'd heat's lap is no best lap"
        );
        assert_eq!(a.laps, 0, "the DQ'd heat's laps do not count");
        assert_eq!(a.metric, RoundMetric::BestLap { micros: None });
        let b = standing(&standings, "B");
        assert_eq!(b.position, 1);
        assert_eq!(b.best_lap_micros, Some(2_000_000));
    }

    #[test]
    fn dq_in_one_heat_keeps_the_pilots_clean_heats_in_the_standings() {
        // Only the DQ'd heat is voided for the pilot: A's clean second heat still feeds the
        // row, so their best lap is the CLEAN heat's 3.0s — not the DQ'd heat's faster 1.0s.
        let round = qual_round("q1", "open"); // BestLap
        let meta = meta_with(vec![round.clone()], vec![member("open", &["A", "B"])]);
        let mut log = scored_heat(
            "q-1",
            "q1",
            "open",
            &[("A", &[0, 1_000_000]), ("B", &[0, 2_000_000])],
        );
        log.extend(scored_heat(
            "q-2",
            "q1",
            "open",
            &[("A", &[0, 3_000_000]), ("B", &[0, 2_500_000])],
        ));
        log.push(penalty_applied(
            "q-1",
            "A",
            Penalty::Disqualify { reason: None },
        ));

        let standings = round_standings(&meta, &round, &log).unwrap();
        let a = standing(&standings, "A");
        assert_eq!(
            a.best_lap_micros,
            Some(3_000_000),
            "the clean heat counts; the DQ'd (faster) lap does not"
        );
        assert_eq!(a.laps, 1, "only the clean heat's lap is counted");
        assert_eq!(
            a.metric,
            RoundMetric::BestLap {
                micros: Some(3_000_000)
            }
        );
    }

    #[test]
    fn ruling_reversed_restores_the_original_ranking() {
        // RulingReversed un-applies a DQ at its true global LogRef (offsets PRESERVED, not
        // re-enumerated): a DQ on A sinks A, then reversing that DQ restores the clean order.
        let round = h2h_round("h2h", "open", WinCondition::FirstToLaps { n: 1 });
        let meta = meta_with(vec![round.clone()], vec![member("open", &["A", "B"])]);
        let base = scored_heat(
            "h2h-1",
            "h2h",
            "open",
            &[("A", &[0, 1_000_000]), ("B", &[0, 2_000_000])],
        );
        let clean = ranking_order(&round_ranking(&meta, &round, &base).unwrap());
        assert_eq!(clean, vec!["A", "B"]);

        // Apply a DQ on A — A sinks. Its append offset is the global `LogRef` a reversal targets.
        let mut log = base.clone();
        let dq_offset = log.len() as u64;
        log.push(penalty_applied(
            "h2h-1",
            "A",
            Penalty::Disqualify { reason: None },
        ));
        assert_eq!(
            ranking_order(&round_ranking(&meta, &round, &log).unwrap()),
            vec!["B", "A"]
        );

        // Reverse the DQ at its append offset — the original order is restored. A re-enumerated
        // window would target the wrong offset and fail to restore.
        log.push(Event::RulingReversed {
            target: LogRef(dq_offset),
        });
        assert_eq!(
            ranking_order(&round_ranking(&meta, &round, &log).unwrap()),
            clean
        );
    }

    // --- Mis-windowing regressions: marshaling a NON-LATEST heat routes by tag/target -------
    //
    // `heat_window_offsets` used to attribute every marshaling event *positionally* — to
    // whichever heat was active at that point in the log. Adjudicating a FINISHED heat while a
    // later heat ran was therefore a silent no-op on its target heat AND leaked into the live
    // one. These pin the tag/target routing (app.rs `heat_window_offsets`) and the
    // corrected-pass fold in `score_heat_window`; each fails on the old positional path.

    #[test]
    fn a_restarted_heat_scores_only_its_current_run() {
        // A heat races (run 1), is RESTARTED, and re-races (run 2). The abandoned run's passes
        // — and a ruling made about them before the restart — must not reach the result: before
        // the current-run window rule the scorer folded BOTH runs, and the ghost run's laps
        // out-ranked the real ones (hit live 2026-07-03: a re-raced qualifier scored 39 ghost
        // laps for one pilot AND held two positions at once).
        let round = h2h_round("h2h", "open", WinCondition::FirstToLaps { n: 5 });
        let heat = "h2h-1";
        let mut log = vec![scheduled(heat, "h2h", "open", &["A", "B"])];
        // Run 1: A banks a pile of laps; a DQ on B lands mid-marshaling — all abandoned below.
        log.push(changed(heat, HeatTransition::Staged));
        log.push(changed(heat, HeatTransition::Armed));
        log.push(changed(heat, HeatTransition::Running));
        for (i, t) in [0i64, 1_000_000, 2_000_000, 3_000_000].iter().enumerate() {
            log.push(pass("A", *t, i as u64));
        }
        log.push(changed(heat, HeatTransition::Finished));
        log.push(penalty_applied(
            "h2h-1",
            "B",
            Penalty::Disqualify { reason: None },
        ));
        // The RD abandons the run.
        log.push(changed(heat, HeatTransition::Restarted));
        // Run 2: both fly clean — one lap each, A first.
        log.push(changed(heat, HeatTransition::Staged));
        log.push(changed(heat, HeatTransition::Armed));
        log.push(changed(heat, HeatTransition::Running));
        log.push(pass("A", 10_000_000, 10));
        log.push(pass("A", 11_000_000, 11));
        log.push(pass("B", 10_100_000, 12));
        log.push(pass("B", 12_000_000, 13));
        log.push(changed(heat, HeatTransition::Finished));
        log.push(changed(heat, HeatTransition::Finalized));

        let result =
            crate::app::score_heat_window(&log, &HeatId(heat.into()), round.win_condition, None);
        // Only run 2 scores: one lap each (holeshot + one), NOT run 1's ghost pile.
        let by_ref: std::collections::BTreeMap<&str, u32> = result
            .places
            .iter()
            .map(|p| (p.competitor.competitor.0.as_str(), p.laps))
            .collect();
        assert_eq!(
            by_ref.get("A"),
            Some(&1),
            "A scores run 2's single lap only"
        );
        assert_eq!(
            by_ref.get("B"),
            Some(&1),
            "B scores run 2's single lap only"
        );
        // The abandoned run's DQ does not survive the restart (clean slate).
        assert!(
            result.places.iter().all(|p| !p.disqualified),
            "a pre-restart ruling belongs to the abandoned run"
        );
        // A post-restart ruling DOES apply.
        log.push(penalty_applied(
            "h2h-1",
            "B",
            Penalty::Disqualify { reason: None },
        ));
        let ruled =
            crate::app::score_heat_window(&log, &HeatId(heat.into()), round.win_condition, None);
        assert!(
            ruled
                .places
                .iter()
                .any(|p| p.competitor.competitor.0 == "B" && p.disqualified),
            "a ruling on the CURRENT run applies"
        );
    }

    #[test]
    fn adjudicating_a_non_latest_heat_lands_in_its_own_window() {
        // Two finished heats of one round; the DQ on heat 1's winner is appended AFTER heat 2's
        // whole span (marshaling a finished heat). It must land in heat 1's window — not in
        // whichever heat happened to run last.
        let round = h2h_round("h2h", "open", WinCondition::FirstToLaps { n: 1 });
        let meta = meta_with(
            vec![round.clone()],
            vec![member("open", &["A", "B", "C", "D"])],
        );
        let mut log = scored_heat(
            "h2h-1",
            "h2h",
            "open",
            &[("A", &[0, 1_000_000]), ("B", &[0, 2_000_000])],
        );
        log.extend(scored_heat(
            "h2h-2",
            "h2h",
            "open",
            &[("C", &[0, 1_000_000]), ("D", &[0, 2_000_000])],
        ));
        log.push(penalty_applied(
            "h2h-1",
            "A",
            Penalty::Disqualify { reason: None },
        ));

        // Heat 1's result carries the DQ...
        let heat1 =
            crate::app::score_heat_window(&log, &HeatId("h2h-1".into()), round.win_condition, None);
        let dq: Vec<&str> = heat1
            .places
            .iter()
            .filter(|p| p.disqualified)
            .map(|p| p.competitor.competitor.0.as_str())
            .collect();
        assert_eq!(dq, vec!["A"], "the DQ lands in the heat it names");
        // ...and heat 2 (the later, positionally-active heat) is untouched.
        let heat2 =
            crate::app::score_heat_window(&log, &HeatId("h2h-2".into()), round.win_condition, None);
        assert!(
            heat2.places.iter().all(|p| !p.disqualified),
            "the DQ must not leak into the heat that happened to run last"
        );
        // And the round ranking reflects it: A (heat 1's on-track winner) ranks last.
        assert_eq!(
            ranking_order(&round_ranking(&meta, &round, &log).unwrap()),
            vec!["B", "C", "D", "A"]
        );
    }

    #[test]
    fn marshaling_lap_corrections_reach_the_round_ranking() {
        // timed_qual BestLap: on track B (2.0s) beats A (5.0s). Marshaling then inserts A's
        // missed pass at 2.5s (A best lap → 2.5s) and voids B's only lap-end detection (B → no
        // lap). Ranking and standings must score the CORRECTED pass stream — before the fix
        // `score_heat_window` scored raw passes only, so InsertLap/VoidDetection never reached
        // results.
        let round = qual_round("q1", "open"); // BestLap
        let meta = meta_with(vec![round.clone()], vec![member("open", &["A", "B"])]);
        let mut log = scored_heat(
            "q-1",
            "q1",
            "open",
            &[("A", &[0, 5_000_000]), ("B", &[0, 2_000_000])],
        );
        // Sanity: the raw passes rank B (2.0s) ahead of A (5.0s).
        assert_eq!(
            ranking_order(&round_ranking(&meta, &round, &log).unwrap()),
            vec!["B", "A"]
        );

        let b_lap_end = pass_offset(&log, "B", 2_000_000);
        log.push(Event::LapInserted {
            adapter: AdapterId(ADAPTER.into()),
            competitor: CompetitorRef("A".into()),
            at: SourceTime::from_micros(2_500_000),
            heat: Some(HeatId("q-1".into())),
        });
        log.push(Event::DetectionVoided {
            target: LogRef(b_lap_end),
        });

        // The corrections land: A (2.5s best lap) now leads the lap-less B.
        assert_eq!(
            ranking_order(&round_ranking(&meta, &round, &log).unwrap()),
            vec!["A", "B"]
        );
        let standings = round_standings(&meta, &round, &log).unwrap();
        assert_eq!(
            standing(&standings, "A").best_lap_micros,
            Some(2_500_000),
            "the inserted lap sets A's best lap"
        );
        assert_eq!(
            standing(&standings, "B").best_lap_micros,
            None,
            "voiding B's lap-end detection leaves B lap-less"
        );
    }

    #[test]
    fn protest_on_a_finished_heat_gates_that_heat_not_the_live_one() {
        // Heat 1 is finished; heat 2 is mid-run when the protest against heat 1 is filed. The
        // filing must join heat 1's window (where the audit/gating consumers read it) and stay
        // out of the live heat 2's — positional attribution put it in heat 2's.
        let mut log = scored_heat(
            "h2h-1",
            "h2h",
            "open",
            &[("A", &[0, 1_000_000]), ("B", &[0, 2_000_000])],
        );
        log.push(scheduled("h2h-2", "h2h", "open", &["C", "D"]));
        log.push(changed("h2h-2", HeatTransition::Staged));
        log.push(changed("h2h-2", HeatTransition::Armed));
        log.push(changed("h2h-2", HeatTransition::Running));
        log.push(pass("C", 10_000_000, 8));
        let protest_offset = log.len() as u64;
        log.push(Event::ProtestFiled {
            heat: HeatId("h2h-1".into()),
            competitor: CompetitorRef("A".into()),
            note: "blocking on lap 1".into(),
        });
        log.push(pass("D", 11_000_000, 9)); // heat 2 races on around the filing

        let window1 = crate::app::heat_window_offsets(&log, &HeatId("h2h-1".into()));
        assert!(
            window1
                .iter()
                .any(|(offset, e)| *offset == protest_offset
                    && matches!(e, Event::ProtestFiled { .. })),
            "the protest gates the finished heat it names"
        );
        let window2 = crate::app::heat_window_offsets(&log, &HeatId("h2h-2".into()));
        assert!(
            window2
                .iter()
                .all(|(_, e)| !matches!(e, Event::ProtestFiled { .. })),
            "the live heat is clear of the other heat's protest"
        );
    }

    #[test]
    fn two_rounds_of_the_same_format_never_collide_on_heat_ids() {
        // Two head_to_head rounds over the same class: each generator emits h2h-h0/h2h-h1, so
        // the LOGGED ids must be round-scoped (`{round}-{generator-id}`) — unscoped, the second
        // round's heats would collide with the first's and every by-id fold (heat state,
        // windows, live control) would conflate two different heats.
        let mut ra = h2h_round("ra", "open", WinCondition::FirstToLaps { n: 1 });
        ra.params.insert("group_size".into(), "2".into());
        let mut rb = h2h_round("rb", "open", WinCondition::FirstToLaps { n: 1 });
        rb.params.insert("group_size".into(), "2".into());
        let meta = meta_with(vec![ra, rb], vec![member("open", &["A", "B", "C", "D"])]);

        // Drive both rounds through every fill, appending the tagged schedule + scored run the
        // way the real handler does (mirrors fill_round_sequence_is_deterministic_on_replay).
        let mut log: Vec<Event> = Vec::new();
        let mut ids: Vec<String> = Vec::new();
        for round_id in ["ra", "rb"] {
            for _ in 0..8 {
                match fill_round(&meta, &no_timers(), &RoundId(round_id.into()), &log).unwrap() {
                    FillOutcome::Scheduled { heat, lineup, .. } => {
                        let names: Vec<&str> = lineup.iter().map(|c| c.0.as_str()).collect();
                        log.push(scheduled(&heat.0, round_id, "open", &names));
                        let mut passes = Vec::new();
                        for (i, n) in names.iter().enumerate() {
                            passes.push(pass(n, i as i64, 0));
                            passes.push(pass(n, 1_000_000 + i as i64, 1));
                        }
                        log.extend(run_heat_events(&heat.0, passes));
                        ids.push(heat.0);
                    }
                    FillOutcome::Complete => break,
                    other => panic!("unexpected fill outcome {other:?}"),
                }
            }
        }

        // 4 pilots, 2-up, one rotation → two heats per round, all four ids distinct.
        assert_eq!(ids.len(), 4, "two heats per round scheduled: {ids:?}");
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(
            unique.len(),
            ids.len(),
            "heat ids are globally unique across the two rounds: {ids:?}"
        );
        // Each logged id carries its own round's scope prefix.
        for id in &ids[..2] {
            assert!(id.starts_with("ra-"), "round ra's heat is ra-scoped: {id}");
        }
        for id in &ids[2..] {
            assert!(id.starts_with("rb-"), "round rb's heat is rb-scoped: {id}");
        }
    }

    #[test]
    fn carry_seeding_freezes_at_first_fill_and_survives_source_adjudication() {
        // FREEZE-AT-FILL (#334): once a FromRanking round fills, its field is a RECORDED draw —
        // adjudicating the SOURCE round afterwards must not rewrite who the dependent round's
        // field was (raced results would vanish from its ranking). A sibling round that has NOT
        // yet filled keeps resolving live (build-ahead tracks its source until it draws).
        let qual = qual_round("q1", "open");
        let mut fill_now = h2h_round("dep", "open", WinCondition::FirstToLaps { n: 1 });
        fill_now.seeding = SeedingRule::FromRanking {
            source_rounds: vec![RoundId("q1".into())],
            top_n: 2,
        };
        fill_now.params.insert("group_size".into(), "2".into());
        let mut fill_later = h2h_round("dep2", "open", WinCondition::FirstToLaps { n: 1 });
        fill_later.seeding = SeedingRule::FromRanking {
            source_rounds: vec![RoundId("q1".into())],
            top_n: 2,
        };
        fill_later.params.insert("group_size".into(), "2".into());
        let meta = meta_with(
            vec![qual.clone(), fill_now.clone(), fill_later.clone()],
            vec![member("open", &["A", "B", "C"])],
        );

        // Qualifier: A (1.0s) beats B (2.0s) beats C (3.0s) → top-2 carry = [A, B].
        let mut log = scored_heat(
            "q-1",
            "q1",
            "open",
            &[
                ("A", &[0, 1_000_000]),
                ("B", &[0, 2_000_000]),
                ("C", &[0, 3_000_000]),
            ],
        );

        // Fill `dep` the way the real handler does: record the draw, then the heat.
        let FillOutcome::Scheduled {
            heat,
            lineup,
            field_draw,
            ..
        } = fill_round(&meta, &no_timers(), &fill_now.id, &log).unwrap()
        else {
            panic!("expected a scheduled heat");
        };
        let drawn = field_draw.expect("a carry seeding's first fill records its draw");
        assert_eq!(names_of(&drawn), vec!["A", "B"]);
        log.push(Event::RoundFieldDrawn {
            round: fill_now.id.clone(),
            field: drawn,
        });
        let names: Vec<&str> = lineup.iter().map(|c| c.0.as_str()).collect();
        log.push(scheduled(&heat.0, "dep", "open", &names));

        // NOW the source adjudication lands: DQ A in the qualifier — its live ranking becomes
        // [B, C] and a live top-2 carry would be [B, C].
        log.push(penalty_applied(
            "q-1",
            "A",
            Penalty::Disqualify { reason: None },
        ));

        // The FILLED round's field is frozen at its recorded draw…
        assert_eq!(
            names_of(&round_field(&meta, &fill_now, &log).unwrap()),
            vec!["A", "B"],
            "the filled round keeps the field it actually raced"
        );
        // …its next fill draws from the SAME frozen field (no second RoundFieldDrawn either)…
        match fill_round(&meta, &no_timers(), &fill_now.id, &log).unwrap() {
            FillOutcome::Scheduled { field_draw, .. } => {
                assert!(
                    field_draw.is_none(),
                    "the draw is recorded once, not per heat"
                );
            }
            FillOutcome::Complete | FillOutcome::AlreadyScheduled | FillOutcome::Blocked { .. } => {
            }
        }
        // …while the NOT-yet-filled sibling resolves live and sees the adjudicated carry.
        assert_eq!(
            names_of(&round_field(&meta, &fill_later, &log).unwrap()),
            vec!["B", "C"],
            "an unfilled round keeps tracking its source until it draws"
        );
    }

    #[test]
    fn roster_seeding_stays_live_after_fill() {
        // FromRoster never freezes (#334): a late entrant added to the class after the round
        // filled still joins the round's field (and with it the ranking's no-value tail).
        let round = h2h_round("r1", "open", WinCondition::FirstToLaps { n: 1 });
        let meta_before = meta_with(vec![round.clone()], vec![member("open", &["A", "B"])]);
        let mut log: Vec<Event> = Vec::new();
        match fill_round(&meta_before, &no_timers(), &round.id, &log).unwrap() {
            FillOutcome::Scheduled {
                heat,
                lineup,
                field_draw,
                ..
            } => {
                assert!(field_draw.is_none(), "a roster seeding records no draw");
                let names: Vec<&str> = lineup.iter().map(|c| c.0.as_str()).collect();
                log.push(scheduled(&heat.0, "r1", "open", &names));
            }
            other => panic!("unexpected fill outcome {other:?}"),
        }
        // The late entrant lands in the membership; the round's field follows live.
        let meta_after = meta_with(vec![round.clone()], vec![member("open", &["A", "B", "C"])]);
        assert_eq!(
            names_of(&round_field(&meta_after, &round, &log).unwrap()),
            vec!["A", "B", "C"]
        );
    }

    /// The bare names of a resolved field (freeze-at-fill test helper).
    fn names_of(field: &[CompetitorRef]) -> Vec<&str> {
        field.iter().map(|c| c.0.as_str()).collect()
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
            min_lap_secs: None,
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
                ..
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

        // The first heat the generator emits is the rotation-1 chunk (`tq-r1-h1`).
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

    /// Finalize a heat where `order` is its finishing order (best first): each pilot gets a holeshot
    /// then one lap, with lap times strictly increasing down the order so BestLap ranks them as
    /// listed. Appends the full schedule→run→finalize span under `heat_id`.
    fn finishing_heat(heat_id: &str, round: &str, class: &str, order: &[&str]) -> Vec<Event> {
        let mut out = vec![scheduled(heat_id, round, class, order)];
        let mut passes = Vec::new();
        for (i, c) in order.iter().enumerate() {
            passes.push(pass(c, i as i64, 0)); // holeshot (uncounted)
        }
        for (i, c) in order.iter().enumerate() {
            // Best lap grows with position so the listed order is the finishing order.
            passes.push(pass(c, 1_000_000 + (i as i64) * 100_000, 1));
        }
        out.extend(run_heat_events(heat_id, passes));
        out
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
            min_lap_secs: None,
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
                    ..
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
                        label: None,
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
    fn class_standings_ignore_a_defined_but_unraced_round() {
        // Two rounds are defined up front (qual + finals); only qual has raced. The unraced
        // finals round used to seed the whole field tied at 1 and hand everyone field_size
        // free points + a phantom rounds_entered — shifting the standings before a single
        // finals heat ran.
        let raced = qual_round("q1", "open");
        let unraced = qual_round("q2", "open");
        let meta = meta_with(vec![raced, unraced], vec![member("open", &["A", "B", "C"])]);
        let log = scored_qual_heat("q1-1", "q1", "open", &["A", "B", "C"]);
        let standings = class_standings(&meta, &ClassId("open".into()), &log).unwrap();
        for row in &standings.standings {
            assert_eq!(
                row.rounds_entered, 1,
                "{:?} must count only the RACED round",
                row.competitor
            );
        }
        // Winner points come from the one raced round only (field of 3 -> 3 points, not 6).
        assert_eq!(standings.standings[0].points, 3);
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
            min_lap_secs: None,
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
                        label: None,
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

    // --- Multi-main seeding: FromRankingRange + Combine (depth-guarded) --------------------

    /// A round seeded by an arbitrary `seeding` rule over the `open` class — for the
    /// `FromRankingRange` / `Combine` field tests. Format/win-condition are inert here (only the
    /// field builder is exercised), so `head_to_head` + a sensible win condition keep it valid.
    fn seeded_round(id: &str, seeding: SeedingRule) -> RoundDef {
        RoundDef {
            id: RoundId(id.into()),
            label: id.into(),
            classes: vec![ScopeClassId("open".into())],
            format: "head_to_head".into(),
            params: BTreeMap::new(),
            win_condition: WinCondition::FirstToLaps { n: 1 },
            seeding,
            channel_mode: ChannelMode::PerHeat,
            staging_timer_secs: default_staging_timer_secs(),
            start_procedure: StartProcedure::default(),
            grace_window: default_grace_window(),
            protest_window: gridfpv_engine::heat::ProtestWindow::Off,
            min_lap_secs: None,
            time_limit_secs: None,
        }
    }

    /// A finalized `q1` qual heat ranking the listed pilots best-first (increasing best laps).
    fn qual_ranking_log(order: &[&str]) -> Vec<Event> {
        finishing_heat("q-1", "q1", "open", order)
    }

    #[test]
    fn from_ranking_range_resolves_the_right_slice() {
        // q1 ranks A>B>C>D>E>F>G>H. A FromRankingRange{skip:2, take:3} is the window seeds 3–5.
        let qual = qual_round("q1", "open");
        let rr = seeded_round(
            "rr",
            SeedingRule::FromRankingRange {
                source_rounds: vec![RoundId("q1".into())],
                skip: 2,
                take: 3,
            },
        );
        let meta = meta_with(
            vec![qual, rr.clone()],
            vec![member("open", &["A", "B", "C", "D", "E", "F", "G", "H"])],
        );
        let log = qual_ranking_log(&["A", "B", "C", "D", "E", "F", "G", "H"]);

        let field = round_field(&meta, &rr, &log).unwrap();
        assert_eq!(
            field,
            lineup(&["C", "D", "E"]),
            "FromRankingRange takes the skip..skip+take window of the merged ranking"
        );
    }

    #[test]
    fn combine_concatenates_and_dedupes_keeping_first_occurrence() {
        // q1 ranks A>B>C>D. Combine[ FromRankingRange{skip:2,take:2} (→ C,D),
        // FromRanking{top_n:3} (→ A,B,C) ] concatenates C,D,A,B,C and dedupes first-wins → C,D,A,B.
        // C is named by both sources; it is seeded once, at the *earlier* source's position.
        let qual = qual_round("q1", "open");
        let combined = seeded_round(
            "cmb",
            SeedingRule::Combine {
                sources: vec![
                    SeedingRule::FromRankingRange {
                        source_rounds: vec![RoundId("q1".into())],
                        skip: 2,
                        take: 2,
                    },
                    SeedingRule::FromRanking {
                        source_rounds: vec![RoundId("q1".into())],
                        top_n: 3,
                    },
                ],
            },
        );
        let meta = meta_with(
            vec![qual, combined.clone()],
            vec![member("open", &["A", "B", "C", "D"])],
        );
        let log = qual_ranking_log(&["A", "B", "C", "D"]);

        let field = round_field(&meta, &combined, &log).unwrap();
        assert_eq!(
            field,
            lineup(&["C", "D", "A", "B"]),
            "Combine concatenates in order then dedupes keeping each ref's first occurrence"
        );
    }

    #[test]
    fn combine_over_a_provisional_source_matches_from_ranking_gating() {
        // Gating parity: with q1 NOT yet run (no finalized heat), its ranking is provisional. A
        // Combine wrapping a FromRanking yields exactly what the bare FromRanking does over the same
        // provisional source — the carry is not gated on the source being Final (same as FromRanking).
        let qual = qual_round("q1", "open");
        let bare = seeded_round(
            "bare",
            SeedingRule::FromRanking {
                source_rounds: vec![RoundId("q1".into())],
                top_n: 2,
            },
        );
        let wrapped = seeded_round(
            "wrapped",
            SeedingRule::Combine {
                sources: vec![SeedingRule::FromRanking {
                    source_rounds: vec![RoundId("q1".into())],
                    top_n: 2,
                }],
            },
        );
        let meta = meta_with(
            vec![qual, bare.clone(), wrapped.clone()],
            vec![member("open", &["A", "B", "C", "D"])],
        );
        // Empty log → q1 has no finalized heat → its ranking is the provisional whole-field order.
        let log: Vec<Event> = vec![];

        let bare_field = round_field(&meta, &bare, &log).unwrap();
        let wrapped_field = round_field(&meta, &wrapped, &log).unwrap();
        assert!(
            !bare_field.is_empty(),
            "a provisional source still yields a (provisional) field"
        );
        assert_eq!(
            wrapped_field, bare_field,
            "Combine over a provisional source matches the bare FromRanking carry"
        );
    }

    #[test]
    fn deeply_nested_combine_is_rejected_as_too_deep() {
        // Nest Combine far past MAX_SEEDING_DEPTH; resolving must return SeedingTooDeep, not overflow.
        let mut seeding = SeedingRule::FromRoster;
        for _ in 0..(crate::events::MAX_SEEDING_DEPTH + 2) {
            seeding = SeedingRule::Combine {
                sources: vec![seeding],
            };
        }
        let round = seeded_round("deep", seeding);
        let meta = meta_with(vec![round.clone()], vec![member("open", &["A", "B"])]);

        assert_eq!(
            round_field(&meta, &round, &[]),
            Err(FillError::SeedingTooDeep),
            "an over-deep Combine is rejected, not a stack overflow"
        );
    }

    #[test]
    fn cross_round_seeding_cycle_is_rejected_as_too_deep() {
        // A seeds FromRanking B, B seeds FromRanking A — a mutual cycle. The shared depth thread
        // through round_ranking→resolve_seeding bounds it: SeedingTooDeep instead of a stack overflow.
        let a = seeded_round(
            "ra",
            SeedingRule::FromRanking {
                source_rounds: vec![RoundId("rb".into())],
                top_n: 2,
            },
        );
        let b = seeded_round(
            "rb",
            SeedingRule::FromRanking {
                source_rounds: vec![RoundId("ra".into())],
                top_n: 2,
            },
        );
        let meta = meta_with(
            vec![a.clone(), b.clone()],
            vec![member("open", &["A", "B", "C", "D"])],
        );

        assert_eq!(
            round_field(&meta, &a, &[]),
            Err(FillError::SeedingTooDeep),
            "a 2-round seeding cycle terminates with SeedingTooDeep"
        );
    }

    // --- Re-materializing an edited round's scheduled heats (#387) --------------------------

    /// A `HeatScheduled` carrying frequencies, for the re-materialization fixtures.
    fn scheduled_with(
        heat: &str,
        round: &str,
        lineup: &[&str],
        frequencies: &[(&str, u16)],
    ) -> Event {
        Event::HeatScheduled {
            heat: HeatId(heat.into()),
            lineup: lineup.iter().map(|c| CompetitorRef((*c).into())).collect(),
            class: None,
            round: Some(RoundId(round.into())),
            frequencies: frequencies
                .iter()
                .map(|(c, f)| (CompetitorRef((*c).into()), *f))
                .collect(),
            label: None,
        }
    }

    #[test]
    fn rematerialize_rebuilds_a_scheduled_open_practice_heat_after_a_channel_edit() {
        // #387: the practice heat baked in the channels the round carried at fill time. Editing the
        // round's channels must rebuild that heat's lineup — not leave it stale forever.
        let log = vec![scheduled_with(
            "op1-heat",
            "op1",
            &["node-0", "node-1"],
            &[],
        )];
        // The round now runs a different channel set.
        let edited = open_practice_round("op1", &[2, 3, 4]);
        let meta = meta_with(vec![edited], vec![]);

        let rewrites = rematerialize_round_heats(&meta, &no_timers(), &RoundId("op1".into()), &log);
        assert_eq!(rewrites.len(), 1, "the one scheduled heat is rewritten");
        assert_eq!(rewrites[0].heat, HeatId("op1-heat".into()), "same heat id");
        assert_eq!(
            rewrites[0].lineup,
            lineup(&["node-2", "node-3", "node-4"]),
            "the lineup follows the round's new channels"
        );
        assert!(
            rewrites[0].frequencies.is_empty(),
            "an open-practice heat still carries no frequencies (its lineup IS the channels)"
        );
    }

    #[test]
    fn rematerialize_is_a_no_op_when_the_edit_changes_nothing_material() {
        // The round is re-saved with the SAME channels (a label-only edit, say): nothing to append.
        let log = vec![scheduled_with(
            "op1-heat",
            "op1",
            &["node-0", "node-1"],
            &[],
        )];
        let meta = meta_with(vec![open_practice_round("op1", &[0, 1])], vec![]);
        assert!(
            rematerialize_round_heats(&meta, &no_timers(), &RoundId("op1".into()), &log).is_empty()
        );
    }

    #[test]
    fn rematerialize_rewrites_lineup_and_frequencies_of_a_scheduled_per_heat_round() {
        // Not practice-specific (#387): a per-heat qual round's scheduled heat is rebuilt the same
        // way — new field ⇒ new lineup, and the channels are re-assigned from the timer's pool.
        let round = qual_round("q1", "open");
        let (meta_before, timers) =
            meta_with_timer(vec![round.clone()], vec![member("open", &["A", "B"])], 8);
        let filled = fill_round(&meta_before, &timers, &RoundId("q1".into()), &[]).unwrap();
        let (heat, before_lineup) = match filled {
            FillOutcome::Scheduled { heat, lineup, .. } => (heat, lineup),
            other => panic!("expected a scheduled heat, got {other:?}"),
        };
        let before_freqs = assign_for_event(&meta_before, &timers, &before_lineup).unwrap();
        assert!(
            !before_freqs.is_empty(),
            "the fixture timer assigns channels"
        );
        let log = vec![Event::HeatScheduled {
            heat: heat.clone(),
            lineup: before_lineup.clone(),
            class: Some(ClassId("open".into())),
            round: Some(RoundId("q1".into())),
            frequencies: before_freqs.clone(),
            label: None,
        }];

        // The RD edits the round's class membership out from under the filled heat (a third pilot).
        let (meta_after, timers) =
            meta_with_timer(vec![round], vec![member("open", &["A", "B", "C"])], 8);

        let rewrites = rematerialize_round_heats(&meta_after, &timers, &RoundId("q1".into()), &log);
        assert_eq!(rewrites.len(), 1);
        assert_eq!(rewrites[0].heat, heat, "the heat keeps its id");
        assert_eq!(
            rewrites[0].lineup,
            lineup(&["A", "B", "C"]),
            "the lineup is rebuilt from the round's current field"
        );
        assert_ne!(
            rewrites[0].frequencies, before_freqs,
            "the channel assignment is re-run for the new lineup"
        );
        assert_eq!(
            rewrites[0].frequencies.len(),
            3,
            "every pilot in the rebuilt lineup gets a channel"
        );
    }

    #[test]
    fn rematerialize_leaves_a_raced_heat_alone() {
        // A heat that has left `Scheduled` is never rewritten: it raced under the config it raced
        // under. (The round's channel config is frozen once raced anyway — belt and braces.)
        let mut log = vec![scheduled_with(
            "op1-heat",
            "op1",
            &["node-0", "node-1"],
            &[],
        )];
        log.extend(run_heat_events("op1-heat", vec![]));
        let meta = meta_with(vec![open_practice_round("op1", &[5, 6])], vec![]);
        assert!(
            rematerialize_round_heats(&meta, &no_timers(), &RoundId("op1".into()), &log).is_empty(),
            "a Final heat is left exactly as it raced"
        );
    }

    #[test]
    fn rematerialize_leaves_a_staged_heat_alone() {
        // Staged/armed/running/unofficial are off limits too (`update_round` refuses the edit
        // outright; this is the engine-side half of the same rule).
        let mut log = vec![scheduled_with(
            "op1-heat",
            "op1",
            &["node-0", "node-1"],
            &[],
        )];
        log.push(changed("op1-heat", HeatTransition::Staged));
        let meta = meta_with(vec![open_practice_round("op1", &[5, 6])], vec![]);
        assert!(
            rematerialize_round_heats(&meta, &no_timers(), &RoundId("op1".into()), &log).is_empty()
        );
    }

    #[test]
    fn rematerialize_ignores_heats_of_other_rounds() {
        let log = vec![
            scheduled_with("op1-heat", "op1", &["node-0"], &[]),
            scheduled_with("op2-heat", "op2", &["node-0"], &[]),
        ];
        let meta = meta_with(
            vec![
                open_practice_round("op1", &[7]),
                open_practice_round("op2", &[8]),
            ],
            vec![],
        );
        let rewrites = rematerialize_round_heats(&meta, &no_timers(), &RoundId("op1".into()), &log);
        assert_eq!(rewrites.len(), 1);
        assert_eq!(rewrites[0].heat, HeatId("op1-heat".into()));
    }

    #[test]
    fn heat_on_timer_is_the_last_transition_or_selection_never_the_first_fill() {
        // A freshly-filled heat is NOT "on the timer" — nothing has been staged or selected.
        let log = vec![scheduled_with("op1-heat", "op1", &["node-0"], &[])];
        assert_eq!(heat_on_timer(&log), None);

        // An explicit selection makes it the heat live control is driving …
        let mut log = log;
        log.push(Event::CurrentHeatSelected {
            heat: HeatId("op1-heat".into()),
        });
        assert_eq!(heat_on_timer(&log), Some(HeatId("op1-heat".into())));

        // … and the last transition wins over an earlier selection.
        log.push(scheduled_with("q1-h1", "q1", &["A"], &[]));
        log.push(changed("q1-h1", HeatTransition::Staged));
        assert_eq!(heat_on_timer(&log), Some(HeatId("q1-h1".into())));
    }

    #[test]
    fn heat_display_name_matches_the_console_convention() {
        // "‹Round label› Heat N", numbered by position within the round.
        let mut round = qual_round("q1", "open");
        round.label = "Qualifying".into();
        let log = vec![
            scheduled_with("q1-tq-r1-h1", "q1", &["A"], &[]),
            scheduled_with("q1-tq-r1-h2", "q1", &["B"], &[]),
        ];
        assert_eq!(
            heat_display_name(&round, &log, &HeatId("q1-tq-r1-h2".into())),
            "Qualifying Heat 2"
        );

        // An open-practice round's single heat is named, not numbered.
        let practice = open_practice_round("op1", &[0, 1]);
        let log = vec![scheduled_with("op1-heat", "op1", &["node-0"], &[])];
        assert_eq!(
            heat_display_name(&practice, &log, &HeatId("op1-heat".into())),
            "Practice Heat"
        );
    }

    #[test]
    fn heat_display_name_prefers_a_custom_label() {
        let round = qual_round("q1", "open");
        let log = vec![Event::HeatScheduled {
            heat: HeatId("q1-tq-r1-h1".into()),
            lineup: lineup(&["A"]),
            class: None,
            round: Some(RoundId("q1".into())),
            frequencies: vec![],
            label: Some("  Shootout  ".into()),
        }];
        assert_eq!(
            heat_display_name(&round, &log, &HeatId("q1-tq-r1-h1".into())),
            "Shootout",
            "an RD-typed label wins and is trimmed"
        );
    }
}
