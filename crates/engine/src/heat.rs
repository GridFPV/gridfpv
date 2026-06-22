//! Heat-loop state machine (#29) — the one reusable FSM that runs every heat, in
//! every phase (race-engine.html §2).
//!
//! ```text
//! [*] --> Scheduled
//! Scheduled --> Staged    : stage
//! Staged    --> Armed     : arm
//! Armed     --> Running   : start
//! Running   --> Finished  : time elapsed / all landed (finish)
//! Finished  --> Final     : score
//! Final     --> [*]       : advance
//! Staged    --> Scheduled : abort
//! Armed     --> Staged    : abort
//! Running   --> Staged    : abort / restart
//! Final     --> Scheduled : discard & re-run
//! ```
//!
//! This module is **pure** (race-engine.html §6): it reads no clock and rolls no
//! dice, so a recorded session always replays identically. Live race control is just
//! [`HeatCommand`]s driven against the current [`HeatState`]; each legal command
//! records a [`HeatTransition`] (the events-crate vocabulary, #28), which is what
//! lands in the append-only log as an [`Event::HeatStateChanged`].
//!
//! The split of responsibilities:
//! - [`HeatCommand`] — the *imperative* (what the RD asks for). Kept distinct from
//!   the recorded transition so the off-ramps (abort/restart/discard) read clearly.
//! - [`HeatTransition`] (events crate) — the *fact* appended to the log.
//! - [`apply`] — validates a command against a state and yields its transition.
//! - [`next_state`] — the state a recorded transition lands in.
//! - [`heat_state`] — folds a heat's events back to its current state.

use std::fmt;

use gridfpv_events::{Event, HeatId, HeatTransition};

/// The states of the heat loop (race-engine.html §2). `Scheduled` is the entry
/// state a [`Event::HeatScheduled`] creates; `Final` is reached when the result is
/// finalized (via the `Score` command); `advance` leaves the machine (terminal for
/// this heat).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeatState {
    /// Created with a lineup, not yet staged (`[*] → Scheduled`).
    Scheduled,
    /// Countdown / IRL frequency assignment underway.
    Staged,
    /// The gate is open to detections.
    Armed,
    /// The race is running; passes are consumed from here (plus the grace window).
    Running,
    /// The race closed — time elapsed or all landed.
    Finished,
    /// The result is finalized.
    Final,
}

/// The imperative commands of live race control (race-engine.html §2). A command is
/// validated against the current [`HeatState`] by [`apply`] and, on success, records
/// the corresponding [`HeatTransition`]. Commands are kept distinct from the recorded
/// transitions so the off-ramps stay legible:
///
/// | command   | records                       |
/// |-----------|-------------------------------|
/// | `Stage`   | [`HeatTransition::Staged`]    |
/// | `Arm`     | [`HeatTransition::Armed`]     |
/// | `Start`   | [`HeatTransition::Running`]   |
/// | `Finish`  | [`HeatTransition::Finished`]  |
/// | `Score`   | [`HeatTransition::Scored`]    |
/// | `Advance` | [`HeatTransition::Advanced`]  |
/// | `Abort`   | [`HeatTransition::Aborted`]   |
/// | `Restart` | [`HeatTransition::Restarted`] |
/// | `Discard` | [`HeatTransition::Discarded`] |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeatCommand {
    /// Begin the countdown (Scheduled → Staged).
    Stage,
    /// Open the gate to detections (Staged → Armed).
    Arm,
    /// Start the race (Armed → Running).
    Start,
    /// Close the race — time elapsed / all landed (Running → Finished).
    Finish,
    /// Finalize the result (Finished → Final).
    Score,
    /// Hand results to the format generator (Final → terminal).
    Advance,
    /// Abandon before scoring — the target depends on the from-state
    /// (Staged → Scheduled, Armed → Staged, Running → Staged).
    Abort,
    /// Restart a running heat from staging (Running → Staged).
    Restart,
    /// Discard a scored heat for a re-run (Final → Scheduled).
    Discard,
}

/// Rejection returned by [`apply`] when a command is illegal in the current state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalTransition {
    /// The state the heat was in.
    pub state: HeatState,
    /// The command that was rejected.
    pub command: HeatCommand,
}

impl fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "illegal heat command {:?} in state {:?}",
            self.command, self.state
        )
    }
}

impl std::error::Error for IllegalTransition {}

/// Validate `command` against `state` per the heat-loop diagram (race-engine.html
/// §2) and, on success, return the [`HeatTransition`] it records.
///
/// On an illegal command, returns [`IllegalTransition`] naming the state and the
/// rejected command. This is the single source of FSM legality: [`next_state`] only
/// describes where a *recorded* transition lands.
pub fn apply(state: HeatState, command: HeatCommand) -> Result<HeatTransition, IllegalTransition> {
    use HeatCommand as C;
    use HeatState as S;

    let transition = match (state, command) {
        // Forward path.
        (S::Scheduled, C::Stage) => HeatTransition::Staged,
        (S::Staged, C::Arm) => HeatTransition::Armed,
        (S::Armed, C::Start) => HeatTransition::Running,
        (S::Running, C::Finish) => HeatTransition::Finished,
        (S::Finished, C::Score) => HeatTransition::Scored,
        (S::Final, C::Advance) => HeatTransition::Advanced,

        // Off-ramps. Abort is legal from Staged/Armed/Running (it backs up a
        // state); the landing state is resolved by `next_state`.
        (S::Staged | S::Armed | S::Running, C::Abort) => HeatTransition::Aborted,
        // Restart applies only to a running heat (back to staging).
        (S::Running, C::Restart) => HeatTransition::Restarted,
        // Discard-and-re-run applies only to a scored heat.
        (S::Final, C::Discard) => HeatTransition::Discarded,

        // Everything else is illegal in this state.
        _ => return Err(IllegalTransition { state, command }),
    };
    Ok(transition)
}

/// The state a recorded [`HeatTransition`] lands in, given the state it left.
///
/// The forward transitions name their target state directly. The off-ramps depend on
/// the from-state per the diagram:
/// - `Aborted` from `Staged` → `Scheduled`; from `Armed`/`Running` → `Staged`.
/// - `Restarted` → `Staged` (a running heat back to staging).
/// - `Discarded` → `Scheduled` (a scored heat queued for re-run).
/// - `Advanced` is terminal for the heat; it stays `Final`.
///
/// `from` is consulted only for `Aborted` (whose target is state-dependent). If a
/// transition is replayed from an unexpected state it still resolves to its canonical
/// target; legality is [`apply`]'s job, not this function's.
pub fn next_state(from: HeatState, transition: HeatTransition) -> HeatState {
    use HeatState as S;
    use HeatTransition as T;

    match transition {
        T::Staged => S::Staged,
        T::Armed => S::Armed,
        T::Running => S::Running,
        T::Finished => S::Finished,
        T::Scored => S::Final,
        // Advance hands off to the format generator; the heat itself stays Final
        // (terminal). The state machine for this heat ends here.
        T::Advanced => S::Final,
        // Abort backs up one state: Staged → Scheduled, Armed/Running → Staged.
        T::Aborted => match from {
            S::Staged => S::Scheduled,
            _ => S::Staged,
        },
        T::Restarted => S::Staged,
        T::Discarded => S::Scheduled,
    }
}

/// Fold a heat's events back to its current [`HeatState`].
///
/// Scans `events` for those concerning `heat`: an [`Event::HeatScheduled`] seeds the
/// state at [`HeatState::Scheduled`]; each [`Event::HeatStateChanged`] advances it via
/// [`next_state`]. Returns `None` if the heat was never scheduled. A second
/// `HeatScheduled` for the same id re-seeds to `Scheduled` (a discard-and-re-run is
/// modelled as a `Discarded` transition, not a re-schedule, but re-seeding keeps the
/// fold robust). Pure and order-preserving, so replaying the same slice twice yields
/// the same state.
pub fn heat_state<'a>(
    events: impl IntoIterator<Item = &'a Event>,
    heat: &HeatId,
) -> Option<HeatState> {
    let mut state: Option<HeatState> = None;
    for event in events {
        match event {
            Event::HeatScheduled { heat: h, .. } if h == heat => {
                state = Some(HeatState::Scheduled);
            }
            Event::HeatStateChanged {
                heat: h,
                transition,
            } if h == heat => {
                if let Some(current) = state {
                    state = Some(next_state(current, *transition));
                }
                // A transition before the heat was scheduled is ignored — there is
                // no state to advance from; legality lives in `apply`.
            }
            _ => {}
        }
    }
    state
}

/// The grace window for late crossings after a heat is finished (race-engine.html
/// §2): "late crossings still count until the heat is scored; the window is
/// configurable, default until scored".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GraceWindow {
    /// Late crossings count for the whole `Finished` phase, until the heat is
    /// `Final`. The default.
    #[default]
    UntilScored,
    /// Late crossings count only for `micros` microseconds after the heat finished;
    /// crossings later than that are not consumed even if the heat is still
    /// `Finished`.
    Duration {
        /// Length of the grace window, in microseconds on the source clock.
        micros: i64,
    },
}

/// Whether a pass should be consumed by this heat (race-engine.html §2).
///
/// The rule: **passes are consumed only while the heat is `Running`, plus the grace
/// window after it `Finished`** — by default until the heat is `Final`.
///
/// Inputs:
/// - `state` — the heat's current [`HeatState`].
/// - `grace` — the configured [`GraceWindow`].
/// - `since_finished_micros` — microseconds elapsed since the heat finished, on the
///   source clock (`pass_time - finished_time`). Only consulted when `state` is
///   `Finished` and `grace` is [`GraceWindow::Duration`]. Pass `None` when the heat
///   has not finished (the value is irrelevant there); a negative value (a pass at or
///   before the finish instant) is always within the window.
///
/// Behaviour by state:
/// - `Running` → `true` (the heat is live).
/// - `Finished` → `true` iff still within the grace window:
///   - [`GraceWindow::UntilScored`]: always `true` (the whole `Finished` phase).
///   - [`GraceWindow::Duration { micros }`]: `true` iff
///     `since_finished_micros <= micros` (a `None` elapsed is treated as within the
///     window, since the caller could not place the pass after finish).
/// - any other state (`Scheduled`, `Staged`, `Armed`, `Final`) → `false`. In
///   particular, once `Final` the window is closed regardless of `grace`.
///
/// Pure: it derives consumption from the supplied values and reads no clock itself.
pub fn consumes_pass(
    state: HeatState,
    grace: GraceWindow,
    since_finished_micros: Option<i64>,
) -> bool {
    match state {
        HeatState::Running => true,
        HeatState::Finished => match grace {
            GraceWindow::UntilScored => true,
            GraceWindow::Duration { micros } => {
                // Within the window when the elapsed time is unknown (caller could
                // not place it after finish) or no greater than the configured span.
                since_finished_micros.is_none_or(|elapsed| elapsed <= micros)
            }
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_events::CompetitorRef;

    const ALL_STATES: [HeatState; 6] = [
        HeatState::Scheduled,
        HeatState::Staged,
        HeatState::Armed,
        HeatState::Running,
        HeatState::Finished,
        HeatState::Final,
    ];

    const ALL_COMMANDS: [HeatCommand; 9] = [
        HeatCommand::Stage,
        HeatCommand::Arm,
        HeatCommand::Start,
        HeatCommand::Finish,
        HeatCommand::Score,
        HeatCommand::Advance,
        HeatCommand::Abort,
        HeatCommand::Restart,
        HeatCommand::Discard,
    ];

    /// The complete legality table: (state, command) → recorded transition. Every
    /// pair *not* listed here must be rejected by `apply`.
    fn legal_table() -> Vec<(HeatState, HeatCommand, HeatTransition)> {
        use HeatCommand as C;
        use HeatState as S;
        use HeatTransition as T;
        vec![
            // forward path
            (S::Scheduled, C::Stage, T::Staged),
            (S::Staged, C::Arm, T::Armed),
            (S::Armed, C::Start, T::Running),
            (S::Running, C::Finish, T::Finished),
            (S::Finished, C::Score, T::Scored),
            (S::Final, C::Advance, T::Advanced),
            // off-ramps
            (S::Staged, C::Abort, T::Aborted),
            (S::Armed, C::Abort, T::Aborted),
            (S::Running, C::Abort, T::Aborted),
            (S::Running, C::Restart, T::Restarted),
            (S::Final, C::Discard, T::Discarded),
        ]
    }

    #[test]
    fn every_legal_transition_is_accepted() {
        for (state, command, expected) in legal_table() {
            assert_eq!(
                apply(state, command),
                Ok(expected),
                "{state:?} + {command:?} should record {expected:?}",
            );
        }
    }

    #[test]
    fn every_illegal_command_is_rejected_with_the_right_error() {
        let legal: Vec<(HeatState, HeatCommand)> =
            legal_table().into_iter().map(|(s, c, _)| (s, c)).collect();

        for &state in &ALL_STATES {
            for &command in &ALL_COMMANDS {
                let is_legal = legal.contains(&(state, command));
                let result = apply(state, command);
                if is_legal {
                    assert!(result.is_ok(), "{state:?} + {command:?} should be legal");
                } else {
                    assert_eq!(
                        result,
                        Err(IllegalTransition { state, command }),
                        "{state:?} + {command:?} should be rejected",
                    );
                }
            }
        }
    }

    #[test]
    fn legal_table_is_exhaustive_over_the_diagram() {
        // 6 forward edges + 3 aborts + restart + discard = 11 legal pairs.
        assert_eq!(legal_table().len(), 11);
    }

    #[test]
    fn forward_transitions_land_on_their_named_states() {
        use HeatState as S;
        use HeatTransition as T;
        assert_eq!(next_state(S::Scheduled, T::Staged), S::Staged);
        assert_eq!(next_state(S::Staged, T::Armed), S::Armed);
        assert_eq!(next_state(S::Armed, T::Running), S::Running);
        assert_eq!(next_state(S::Running, T::Finished), S::Finished);
        assert_eq!(next_state(S::Finished, T::Scored), S::Final);
        // advance is terminal: the heat stays Final.
        assert_eq!(next_state(S::Final, T::Advanced), S::Final);
    }

    #[test]
    fn abort_target_depends_on_the_from_state() {
        use HeatState as S;
        use HeatTransition as T;
        // Staged → Scheduled
        assert_eq!(next_state(S::Staged, T::Aborted), S::Scheduled);
        // Armed → Staged
        assert_eq!(next_state(S::Armed, T::Aborted), S::Staged);
        // Running → Staged
        assert_eq!(next_state(S::Running, T::Aborted), S::Staged);
    }

    #[test]
    fn restart_and_discard_land_correctly() {
        use HeatState as S;
        use HeatTransition as T;
        assert_eq!(next_state(S::Running, T::Restarted), S::Staged);
        assert_eq!(next_state(S::Final, T::Discarded), S::Scheduled);
    }

    #[test]
    fn apply_then_next_state_round_trips_the_forward_path() {
        // Drive the whole forward path command-by-command, checking each landing.
        let mut state = HeatState::Scheduled;
        let path = [
            (HeatCommand::Stage, HeatState::Staged),
            (HeatCommand::Arm, HeatState::Armed),
            (HeatCommand::Start, HeatState::Running),
            (HeatCommand::Finish, HeatState::Finished),
            (HeatCommand::Score, HeatState::Final),
            (HeatCommand::Advance, HeatState::Final),
        ];
        for (command, expected) in path {
            let transition = apply(state, command).expect("legal on forward path");
            state = next_state(state, transition);
            assert_eq!(state, expected, "after {command:?}");
        }
    }

    fn heat() -> HeatId {
        HeatId("q-1".into())
    }

    fn scheduled() -> Event {
        Event::HeatScheduled {
            heat: heat(),
            lineup: vec![
                CompetitorRef("node-0".into()),
                CompetitorRef("node-1".into()),
            ],
            class: None,
            round: None,
            frequencies: vec![],
        }
    }

    fn changed(transition: HeatTransition) -> Event {
        Event::HeatStateChanged {
            heat: heat(),
            transition,
        }
    }

    #[test]
    fn heat_state_returns_none_when_never_scheduled() {
        let events = vec![changed(HeatTransition::Staged)];
        assert_eq!(heat_state(&events, &heat()), None);
    }

    #[test]
    fn heat_state_folds_a_full_run_to_scored() {
        let events = vec![
            scheduled(),
            changed(HeatTransition::Staged),
            changed(HeatTransition::Armed),
            changed(HeatTransition::Running),
            changed(HeatTransition::Finished),
            changed(HeatTransition::Scored),
        ];
        assert_eq!(heat_state(&events, &heat()), Some(HeatState::Final));
    }

    #[test]
    fn heat_state_reconstructs_an_abort_and_re_run() {
        // Stage, arm, run, abort (back to Staged), then re-arm and run on.
        let events = vec![
            scheduled(),
            changed(HeatTransition::Staged),
            changed(HeatTransition::Armed),
            changed(HeatTransition::Running),
            changed(HeatTransition::Aborted), // Running → Staged
            changed(HeatTransition::Armed),
            changed(HeatTransition::Running),
        ];
        assert_eq!(heat_state(&events, &heat()), Some(HeatState::Running));
    }

    #[test]
    fn heat_state_reconstructs_a_discard() {
        let events = vec![
            scheduled(),
            changed(HeatTransition::Staged),
            changed(HeatTransition::Armed),
            changed(HeatTransition::Running),
            changed(HeatTransition::Finished),
            changed(HeatTransition::Scored),
            changed(HeatTransition::Discarded), // Final → Scheduled
        ];
        assert_eq!(heat_state(&events, &heat()), Some(HeatState::Scheduled));
    }

    #[test]
    fn heat_state_ignores_other_heats() {
        let other = HeatId("q-2".into());
        let events = vec![
            scheduled(),
            Event::HeatScheduled {
                heat: other.clone(),
                lineup: vec![],
                class: None,
                round: None,
                frequencies: vec![],
            },
            changed(HeatTransition::Staged),
            Event::HeatStateChanged {
                heat: other.clone(),
                transition: HeatTransition::Staged,
            },
        ];
        assert_eq!(heat_state(&events, &heat()), Some(HeatState::Staged));
        assert_eq!(heat_state(&events, &other), Some(HeatState::Staged));
    }

    #[test]
    fn folding_the_same_events_twice_is_identical() {
        // Determinism (race-engine.html §6): a pure fold gives the same answer every
        // time, with no hidden clock/RNG state between runs.
        let events = vec![
            scheduled(),
            changed(HeatTransition::Staged),
            changed(HeatTransition::Armed),
            changed(HeatTransition::Running),
            changed(HeatTransition::Aborted),
        ];
        let first = heat_state(&events, &heat());
        let second = heat_state(&events, &heat());
        assert_eq!(first, second);
        assert_eq!(first, Some(HeatState::Staged));
    }

    #[test]
    fn grace_running_always_consumes() {
        assert!(consumes_pass(
            HeatState::Running,
            GraceWindow::UntilScored,
            None
        ));
        assert!(consumes_pass(
            HeatState::Running,
            GraceWindow::Duration { micros: 0 },
            Some(1_000_000),
        ));
    }

    #[test]
    fn grace_until_scored_consumes_while_finished() {
        assert!(consumes_pass(
            HeatState::Finished,
            GraceWindow::UntilScored,
            None
        ));
        // Default is UntilScored.
        assert_eq!(GraceWindow::default(), GraceWindow::UntilScored);
        assert!(consumes_pass(
            HeatState::Finished,
            GraceWindow::default(),
            Some(999_999_999),
        ));
    }

    #[test]
    fn grace_closed_once_scored() {
        assert!(!consumes_pass(
            HeatState::Final,
            GraceWindow::UntilScored,
            None
        ));
        assert!(!consumes_pass(
            HeatState::Final,
            GraceWindow::Duration { micros: 1_000_000 },
            Some(0),
        ));
    }

    #[test]
    fn grace_duration_bounds_the_window_after_finished() {
        let grace = GraceWindow::Duration { micros: 2_000_000 };
        // Within the window — consumed.
        assert!(consumes_pass(HeatState::Finished, grace, Some(1_500_000)));
        // Exactly at the boundary — consumed (inclusive).
        assert!(consumes_pass(HeatState::Finished, grace, Some(2_000_000)));
        // Past the window — not consumed.
        assert!(!consumes_pass(HeatState::Finished, grace, Some(2_000_001)));
        // A pass at/before the finish instant — within the window.
        assert!(consumes_pass(HeatState::Finished, grace, Some(-5)));
        // Elapsed unknown — treated as within the window.
        assert!(consumes_pass(HeatState::Finished, grace, None));
    }

    #[test]
    fn grace_never_consumes_before_running() {
        for &state in &[HeatState::Scheduled, HeatState::Staged, HeatState::Armed] {
            assert!(
                !consumes_pass(state, GraceWindow::UntilScored, None),
                "{state:?} must not consume passes",
            );
        }
    }
}
