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

use gridfpv_engine::event::score_marshaled;
use gridfpv_engine::format::{
    CompletedHeat, FormatConfig, FormatRegistry, GeneratorStep, RankEntry, advance_top_n,
};
use gridfpv_engine::heat::{HeatState, heat_state};
use gridfpv_engine::scoring::HeatResult;
use gridfpv_events::{ClassId, CompetitorRef, Event, HeatId, Pass, RoundId, SourceTime};

use crate::events::{EventMeta, RoundDef, SeedingRule};

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
        }
    }
}

impl std::error::Error for FillError {}

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
                    for pilot in &membership.pilots {
                        let competitor = CompetitorRef(pilot.0.clone());
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

/// Fill a round (race redesign Slice 3a): build its generator from the field + the round's
/// completed heats off the log, and decide the next heat to schedule.
///
/// Pure with respect to the log — it reads but never appends; appending the tagged
/// `HeatScheduled` is the control handler's job. Deterministic given the same `events` +
/// `meta`, exactly like [`Generator::next`](gridfpv_engine::format::Generator::next).
pub fn fill_round(
    meta: &EventMeta,
    round_id: &RoundId,
    events: &[Event],
) -> Result<FillOutcome, FillError> {
    let round = round_of(meta, round_id)?;
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
    use crate::events::{ClassMembership, EventMeta, RoundDef, SeedingRule};
    use crate::scope::{ClassId as ScopeClassId, EventId, PilotId};
    use gridfpv_engine::scoring::WinCondition;
    use gridfpv_events::{AdapterId, GateIndex, HeatTransition, SourceTime};
    use std::collections::BTreeMap;

    const ADAPTER: &str = "mock";

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
            pilots: pilots.iter().map(|p| PilotId((*p).into())).collect(),
        }
    }

    fn qual_round(id: &str, class: &str) -> RoundDef {
        RoundDef {
            id: RoundId(id.into()),
            label: id.into(),
            classes: vec![ScopeClassId(class.into())],
            format: "timed_qual".into(),
            params: BTreeMap::from([("rounds".into(), "1".into())]),
            win_condition: WinCondition::BestLap,
            seeding: SeedingRule::FromRoster,
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
        let outcome = fill_round(&meta, &RoundId("q1".into()), &[]).unwrap();
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
            fill_round(&meta, &RoundId("q1".into()), &[]),
            Err(FillError::EmptyField(_))
        ));
    }

    #[test]
    fn round_completes_after_its_configured_rounds() {
        // A 1-round timed_qual: after one scored heat, the next FillRound is Complete.
        let round = qual_round("q1", "open");
        let meta = meta_with(vec![round], vec![member("open", &["A", "B"])]);

        // The first heat the generator emits is `round-1`.
        let first = fill_round(&meta, &RoundId("q1".into()), &[]).unwrap();
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
        let next = fill_round(&meta, &RoundId("q1".into()), &log).unwrap();
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
        };
        let meta = meta_with(
            vec![qual, bracket],
            vec![member("open", &["A", "B", "C", "D"])],
        );

        // Run the qual heat to Scored: A fastest, then B, C, D.
        let first = fill_round(&meta, &RoundId("q1".into()), &[]).unwrap();
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
        let outcome = fill_round(&meta, &RoundId("b1".into()), &log).unwrap();
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
    fn fill_round_is_deterministic_on_replay() {
        let round = qual_round("q1", "open");
        let meta = meta_with(vec![round], vec![member("open", &["A", "B", "C"])]);
        let once = fill_round(&meta, &RoundId("q1".into()), &[]).unwrap();
        let twice = fill_round(&meta, &RoundId("q1".into()), &[]).unwrap();
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
                let outcome = fill_round(&meta, &RoundId("q1".into()), &log).unwrap();
                outcomes.push(outcome.clone());
                if let FillOutcome::Scheduled { heat, lineup } = outcome {
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
