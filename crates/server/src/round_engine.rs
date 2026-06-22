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
//!    `round == round.id` whose result is final (it reached `Scored`), scored under the
//!    round's [`win_condition`](crate::events::RoundDef::win_condition).
//! 4. **`generator.next(&completed)`** → either emit a `HeatScheduled` per plan (tagged
//!    with the round, and the class when the round is single-class) or surface *round
//!    complete*.
//!
//! Because step 3 reads the log, the advance closes through the log: when a heat reaches
//! `Score` (the existing FSM path appends `HeatStateChanged { Scored }`), the next
//! `FillRound` sees it as a completed heat and the generator advances — including across
//! the bracket carry, where the *source* round's completed heats produce the ranking the
//! bracket seeds from.

use std::collections::BTreeMap;

use gridfpv_engine::event::score_marshaled;
use gridfpv_engine::format::{
    CompletedHeat, FormatConfig, FormatRegistry, GeneratorStep, RankEntry, advance_top_n,
};
use gridfpv_engine::heat::{HeatState, heat_state};
use gridfpv_engine::schedule::{Frequency, FrequencyPool, allocate};
use gridfpv_engine::scoring::{HeatResult, Metric};
use gridfpv_events::{ClassId, CompetitorRef, Event, HeatId, Pass, RoundId, SourceTime};
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
    /// `Score`. No new heat is appended and the round is *not* finished — the RD just needs
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
    /// A [`SeedingRule::FromRanking`] names a `source_round` that does not exist in this
    /// event.
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
/// Slice 4a) — the engine's half of the RE §7.3 split (the engine allocates; the adapter applies).
///
/// Given the event's selected `timer` and the heat's `lineup` (in seed order):
///
/// 1. **Heat-size cap.** The lineup must be ≤ the timer's
///    [`node_count`](crate::timers::Timer::node_count); otherwise [`AssignError::TooManyForNodes`].
///    A timer with **no available channels** (a sim/Mock-without-frequencies, an unconfigured
///    timer) assigns **nothing** — an empty allocation — *after* the cap check, so a heat that is
///    simply un-channelled is fine but an oversized one is still rejected.
/// 2. **First-fit allocation.** The available channels (raw MHz, in preference order, capped to the
///    node count) form a [`FrequencyPool`]; [`allocate`] hands each pilot the first free channel in
///    seed order (top seed → first channel). Too few channels for the lineup is
///    [`AssignError::TooFewChannels`].
///
/// Pure and deterministic: the same lineup + timer config always yields the same per-pilot
/// `(competitor, mhz)` assignment — the determinism `HeatScheduled.frequencies` and the e2e rely on.
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
    // The pool is the available channels, in preference order, but never more than the timer has
    // nodes for (a node can't run two channels). `FrequencyPool::new` de-duplicates.
    let pool = FrequencyPool::new(
        timer
            .available_channels
            .iter()
            .take(nodes)
            .copied()
            .map(Frequency::new),
    );
    match allocate(lineup, &pool) {
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
/// - [`SeedingRule::FromRanking`]: the **top-N** of the source round's ranking (the
///   qualifying→bracket carry), reusing [`advance_top_n`] over the ranking
///   [`round_ranking`] computes from the source round's completed heats — exactly the
///   phase-2 seeding [`run_event`](gridfpv_engine::event::run_event) does.
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
            source_round,
            top_n,
        } => {
            let source = round_of(meta, source_round)
                .map_err(|_| FillError::UnknownSourceRound(source_round.0.clone()))?;
            let ranking = round_ranking(meta, source, events)?;
            Ok(advance_top_n(&ranking, *top_n))
        }
    }
}

/// Build a [`FormatConfig`] for a round over `field`: the round's
/// [`params`](RoundDef::params) verbatim, identity seeding (the field is already in seed
/// order — the membership/carry decided it), and no recorded draw.
fn format_config(round: &RoundDef, field: Vec<CompetitorRef>) -> FormatConfig {
    let mut config = FormatConfig::new(field);
    config.params = round.params.clone();
    config
}

/// The completed heats of a round, **read back from the log** and scored under the round's
/// [`win_condition`](RoundDef::win_condition) (race redesign Slice 3a).
///
/// A heat counts as completed when it was scheduled tagged with this round *and* its folded
/// [`HeatState`] is [`Scored`](HeatState::Scored) (the FSM terminal the `Score` command
/// reaches). Each is scored via [`score_marshaled`] over the passes that heat produced (the
/// per-heat pass window, see [`heat_passes`]). The order is the order the heats were first
/// scheduled, which is the order the generator emitted them — so the history fed to
/// [`Generator::next`](gridfpv_engine::format::Generator::next) matches what
/// [`run_format`](gridfpv_engine::event::run_format) accumulated.
pub fn completed_heats(round: &RoundDef, events: &[Event]) -> Vec<CompletedHeat> {
    // The heats tagged with this round, in first-scheduled order (dedup repeated schedules
    // of the same id — the latest lineup wins for the window scan, but order is by first
    // appearance, matching the generator's emission order).
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
        .filter(|heat| heat_state(events, heat) == Some(HeatState::Scored))
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
            CompletedHeat::new(heat.0, result)
        })
        .collect()
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

/// One competitor's **best lap** (µs) across a heat's [`HeatResult`], read off the scored
/// [`Metric`] each placement carries. Only [`Metric::BestLapMicros`] is a lap duration; the other
/// metrics are completion *times*, not durations, so they contribute no lap metric (the lap totals
/// still aggregate). `None` when the competitor has no placement or no lap.
fn placement_best_lap(result: &HeatResult, competitor: &CompetitorRef) -> Option<i64> {
    result
        .places
        .iter()
        .find(|p| &p.competitor.competitor == competitor)
        .and_then(|p| match p.metric {
            Metric::BestLapMicros(lap) => lap,
            _ => None,
        })
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
        // The round's scored heats — the same view `round_ranking` ranked over — so the laps /
        // best-lap a standing reports come from exactly the heats that decided the round position.
        let completed = completed_heats(round, events);

        for entry in &ranking {
            // Points: a win (position 1) is worth the field size; last is worth 1.
            let points = field_size.saturating_sub(entry.position).saturating_add(1);
            // Laps / best lap for this competitor across the round's heats.
            let mut laps = 0u32;
            let mut best_lap: Option<i64> = None;
            for heat in &completed {
                laps += placement_laps(&heat.result, &entry.competitor);
                if let Some(lap) = placement_best_lap(&heat.result, &entry.competitor) {
                    best_lap = Some(match best_lap {
                        Some(existing) => existing.min(lap),
                        None => lap,
                    });
                }
            }
            acc.entry(entry.competitor.clone())
                .or_default()
                .add_round(points, laps, best_lap);
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
            // heat to Score before asking for the next). A generator that emits several
            // plans at once (a bracket round) still advances one heat at a time: take the
            // first not-yet-scheduled plan. Dedup against already-tagged heats so a repeated
            // FillRound before the prior heat is scored does not double-schedule it.
            let already: Vec<HeatId> = scheduled_round_heats(events, round_id);
            let next = plans.into_iter().find(|p| !already.contains(&p.heat));
            match next {
                Some(plan) => Ok(FillOutcome::Scheduled {
                    heat: plan.heat,
                    lineup: plan.lineup,
                    // Per-heat: the handler assigns channels from the timer pool (first-fit).
                    frequencies: None,
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
                    // Any exit from Running (forward to Finished/Scored or an off-ramp)
                    // closes that heat's pass window.
                    T::Finished | T::Scored | T::Aborted | T::Restarted | T::Discarded
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
    use crate::events::{
        ChannelMode, ClassMembership, EventMeta, MemberSlot, RoundDef, SeedingRule,
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
    fn assign_is_first_fit_in_seed_order() {
        // An 8-node Raceband timer assigns R1, R2, R3 to the top three seeds in order.
        let timer = timer_with(8, RACEBAND_MHZ.to_vec());
        let assignment = assign_frequencies(&timer, &lineup(&["A", "B", "C"])).unwrap();
        assert_eq!(
            assignment,
            vec![
                (CompetitorRef("A".into()), 5658),
                (CompetitorRef("B".into()), 5695),
                (CompetitorRef("C".into()), 5732),
            ]
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

    /// Drive one heat from Running to Scored with a set of best-lap passes, returning the
    /// events that span it (schedule is the caller's).
    fn run_heat_events(heat: &str, passes: Vec<Event>) -> Vec<Event> {
        let mut v = vec![
            changed(heat, HeatTransition::Staged),
            changed(heat, HeatTransition::Armed),
            changed(heat, HeatTransition::Running),
        ];
        v.extend(passes);
        v.push(changed(heat, HeatTransition::Finished));
        v.push(changed(heat, HeatTransition::Scored));
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

        // Append the tagged schedule + drive it to Scored with some passes.
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
                source_round: RoundId("q1".into()),
                top_n: 2,
            },
            channel_mode: ChannelMode::PerHeat,
        };
        let meta = meta_with(
            vec![qual, bracket],
            vec![member("open", &["A", "B", "C", "D"])],
        );

        // Run the qual heat to Scored: A fastest, then B, C, D.
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
    /// returning the round-tagged schedule plus the run-to-Scored events.
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
