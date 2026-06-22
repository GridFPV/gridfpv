//! Heat-loop state machine (#29) — the one reusable FSM that runs every heat, in
//! every phase (race-engine.html §2).
//!
//! ```text
//! [*] --> Scheduled
//! Scheduled  --> Staged     : stage
//! Staged     --> Armed      : start          (runs the start procedure)
//! Armed      --> Running    : (auto) countdown elapsed  /  SkipCountdown (override)
//! Running    --> Unofficial : (auto) win condition + grace  /  ForceEnd (override)
//! Unofficial --> Final      : finalize
//! Final      --> Unofficial : revert
//! Final      --> [*]        : advance
//! Staged     --> Scheduled  : abort
//! Armed      --> Staged     : abort
//! Running    --> Staged     : abort / restart
//! Armed      --> Staged     : restart
//! Unofficial --> Staged     : restart
//! Final      --> Scheduled  : discard & re-run
//! ```
//!
//! # The command collapse + the runtime clock (heat-lifecycle redesign, Slice 2)
//!
//! The two middle forward steps — `Armed → Running` and `Running → Unofficial` — are no
//! longer **manual** commands. They are **runtime-driven**: the Director's clock appends the
//! transition itself when the start countdown elapses (after a logged, deterministic delay) and
//! when the round's win condition + grace window are met. So the manual command set drops the old
//! `Start`/`Finish` (the engine *transitions* `Running`/`Finished` they produced still exist —
//! the runtime records them). The old `Arm` command is renamed **`Start`** (the RD presses
//! "Start", which arms the heat and runs the start procedure).
//!
//! For the race-day cases where the clock can't be trusted (a stuck countdown, a race that must be
//! called now), two **manual overrides** remain, legal only in the right state:
//! [`SkipCountdown`](HeatCommand::SkipCountdown) forces `Armed → Running` (skip the countdown) and
//! [`ForceEnd`](HeatCommand::ForceEnd) forces `Running → Unofficial` now. They record the same
//! `Running`/`Finished` transitions the auto-path does.
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
use serde::{Deserialize, Serialize};

/// The states of the heat loop (race-engine.html §2). `Scheduled` is the entry
/// state a [`Event::HeatScheduled`] creates; `Final` is reached when the result is
/// finalized (via the `Finalize` command); `advance` leaves the machine (terminal for
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
    /// The race closed — time elapsed or all landed — but the result is not yet
    /// finalized (the grace window for late crossings is still open).
    Unofficial,
    /// The result is finalized.
    Final,
}

/// The imperative commands of live race control (race-engine.html §2). A command is
/// validated against the current [`HeatState`] by [`apply`] and, on success, records
/// the corresponding [`HeatTransition`]. Commands are kept distinct from the recorded
/// transitions so the off-ramps stay legible:
///
/// | command         | records                       |
/// |-----------------|-------------------------------|
/// | `Stage`         | [`HeatTransition::Staged`]    |
/// | `Start`         | [`HeatTransition::Armed`]     |
/// | `SkipCountdown` | [`HeatTransition::Running`]   |
/// | `ForceEnd`      | [`HeatTransition::Finished`]  |
/// | `Finalize`      | [`HeatTransition::Finalized`] |
/// | `Advance`       | [`HeatTransition::Advanced`]  |
/// | `Revert`        | [`HeatTransition::Reverted`]  |
/// | `Abort`         | [`HeatTransition::Aborted`]   |
/// | `Restart`       | [`HeatTransition::Restarted`] |
/// | `Discard`       | [`HeatTransition::Discarded`] |
///
/// The `Armed → Running` and `Running → Unofficial` transitions are normally appended by the
/// **runtime clock**, not an RD command (the command collapse, see the module docs).
/// [`SkipCountdown`](Self::SkipCountdown) / [`ForceEnd`](Self::ForceEnd) are the manual overrides
/// that record those same transitions when the clock must be bypassed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeatCommand {
    /// Begin the countdown (Scheduled → Staged).
    Stage,
    /// Start the heat (Staged → Armed) — opens the gate to detections and **runs the start
    /// procedure**. The runtime then auto-advances to `Running` after the logged start delay.
    /// (Renamed from the former `Arm` in the command collapse.)
    Start,
    /// **Override:** force `Armed → Running` immediately, skipping the start countdown — the
    /// race-day escape hatch when the clock's auto-start can't be trusted. Records the same
    /// [`HeatTransition::Running`] the runtime's auto-start would.
    SkipCountdown,
    /// **Override:** force `Running → Unofficial` now — call the race when the completion clock
    /// must be bypassed. Records the same [`HeatTransition::Finished`] the runtime's auto-complete
    /// would.
    ForceEnd,
    /// Finalize the result (Unofficial → Final).
    Finalize,
    /// Hand results to the format generator (Final → terminal).
    Advance,
    /// Re-open a finalized result for correction (Final → Unofficial).
    Revert,
    /// Abandon before finalizing — the target depends on the from-state
    /// (Staged → Scheduled, Armed → Staged, Running → Staged).
    Abort,
    /// Restart a committed heat from staging (Armed/Running/Unofficial → Staged).
    Restart,
    /// Discard a finalized heat for a re-run (Final → Scheduled).
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
        // Forward path. `Start` arms the heat (the runtime then auto-advances to Running after the
        // logged start delay); the manual Armed→Running / Running→Unofficial steps are gone — they
        // are runtime-appended, with the overrides below as the only manual route.
        (S::Scheduled, C::Stage) => HeatTransition::Staged,
        (S::Staged, C::Start) => HeatTransition::Armed,
        (S::Unofficial, C::Finalize) => HeatTransition::Finalized,
        (S::Final, C::Advance) => HeatTransition::Advanced,

        // Overrides for when the runtime clock can't be trusted (race-day escape hatches). They
        // record exactly the transitions the auto-path would; legal only in the matching state.
        (S::Armed, C::SkipCountdown) => HeatTransition::Running,
        (S::Running, C::ForceEnd) => HeatTransition::Finished,

        // Off-ramps. Abort is legal from Staged/Armed/Running (it backs up a
        // state); the landing state is resolved by `next_state`.
        (S::Staged | S::Armed | S::Running, C::Abort) => HeatTransition::Aborted,
        // Restart applies to any committed heat short of finalized (Armed/Running/
        // Unofficial), back to staging for a clean re-run.
        (S::Armed | S::Running | S::Unofficial, C::Restart) => HeatTransition::Restarted,
        // Revert re-opens a finalized result for correction (Final → Unofficial).
        (S::Final, C::Revert) => HeatTransition::Reverted,
        // Discard-and-re-run applies only to a finalized heat.
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
/// - `Restarted` → `Staged` (a committed heat back to staging).
/// - `Reverted` → `Unofficial` (a finalized heat re-opened for correction).
/// - `Discarded` → `Scheduled` (a finalized heat queued for re-run).
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
        T::Finished => S::Unofficial,
        T::Finalized => S::Final,
        // Advance hands off to the format generator; the heat itself stays Final
        // (terminal). The state machine for this heat ends here.
        T::Advanced => S::Final,
        // Revert re-opens a finalized result back to Unofficial for correction.
        T::Reverted => S::Unofficial,
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
/// §2): "late crossings still count until the heat is finalized; the window is
/// configurable, default until finalized".
///
/// Derives serde + `ts_rs::TS` so it can be carried as a per-round config
/// ([`RoundDef::grace_window`](../../server/events/struct.RoundDef.html)) and read by the
/// frontend. The runtime completion clock (heat-lifecycle Slice 2) reads this to decide how long
/// to hold the heat in `Running` for trailing pilots after the win condition is met, before
/// appending the auto `Running → Unofficial`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "bindings/")]
pub enum GraceWindow {
    /// Late crossings count for the whole `Unofficial` phase, until the heat is
    /// `Final`. The default for [`consumes_pass`] (an open window); a *completion-clock* round
    /// config instead defaults to a bounded [`Duration`](Self::Duration) so the auto-transition
    /// actually fires.
    #[default]
    UntilScored,
    /// Late crossings count only for `micros` microseconds after the heat finished;
    /// crossings later than that are not consumed even if the heat is still
    /// `Unofficial`.
    Duration {
        /// Length of the grace window, in microseconds on the source clock.
        #[ts(type = "number")]
        micros: i64,
    },
}

/// Whether a pass should be consumed by this heat (race-engine.html §2).
///
/// The rule: **passes are consumed only while the heat is `Running`, plus the grace
/// window after it is `Unofficial`** — by default until the heat is `Final`.
///
/// Inputs:
/// - `state` — the heat's current [`HeatState`].
/// - `grace` — the configured [`GraceWindow`].
/// - `since_finished_micros` — microseconds elapsed since the heat finished, on the
///   source clock (`pass_time - finished_time`). Only consulted when `state` is
///   `Unofficial` and `grace` is [`GraceWindow::Duration`]. Pass `None` when the heat
///   has not finished (the value is irrelevant there); a negative value (a pass at or
///   before the finish instant) is always within the window.
///
/// Behaviour by state:
/// - `Running` → `true` (the heat is live).
/// - `Unofficial` → `true` iff still within the grace window:
///   - [`GraceWindow::UntilScored`]: always `true` (the whole `Unofficial` phase).
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
        HeatState::Unofficial => match grace {
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
        HeatState::Unofficial,
        HeatState::Final,
    ];

    const ALL_COMMANDS: [HeatCommand; 10] = [
        HeatCommand::Stage,
        HeatCommand::Start,
        HeatCommand::SkipCountdown,
        HeatCommand::ForceEnd,
        HeatCommand::Finalize,
        HeatCommand::Advance,
        HeatCommand::Revert,
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
            // forward path (the Armed→Running / Running→Unofficial steps are runtime-appended,
            // not manual commands — see the overrides below)
            (S::Scheduled, C::Stage, T::Staged),
            (S::Staged, C::Start, T::Armed),
            (S::Unofficial, C::Finalize, T::Finalized),
            (S::Final, C::Advance, T::Advanced),
            // overrides (manual route for the runtime-driven transitions)
            (S::Armed, C::SkipCountdown, T::Running),
            (S::Running, C::ForceEnd, T::Finished),
            // off-ramps
            (S::Staged, C::Abort, T::Aborted),
            (S::Armed, C::Abort, T::Aborted),
            (S::Running, C::Abort, T::Aborted),
            (S::Armed, C::Restart, T::Restarted),
            (S::Running, C::Restart, T::Restarted),
            (S::Unofficial, C::Restart, T::Restarted),
            (S::Final, C::Revert, T::Reverted),
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
        // 4 manual forward edges + 2 overrides + 3 aborts + 3 restarts + revert + discard = 14.
        assert_eq!(legal_table().len(), 14);
    }

    #[test]
    fn overrides_are_legal_only_in_their_state() {
        use HeatCommand as C;
        use HeatState as S;
        use HeatTransition as T;
        // SkipCountdown forces Armed → Running; ForceEnd forces Running → Unofficial.
        assert_eq!(apply(S::Armed, C::SkipCountdown), Ok(T::Running));
        assert_eq!(apply(S::Running, C::ForceEnd), Ok(T::Finished));
        // Each is illegal in every other state.
        for &state in &ALL_STATES {
            if state != S::Armed {
                assert!(apply(state, C::SkipCountdown).is_err(), "{state:?} + Skip");
            }
            if state != S::Running {
                assert!(apply(state, C::ForceEnd).is_err(), "{state:?} + ForceEnd");
            }
        }
    }

    #[test]
    fn forward_transitions_land_on_their_named_states() {
        use HeatState as S;
        use HeatTransition as T;
        assert_eq!(next_state(S::Scheduled, T::Staged), S::Staged);
        assert_eq!(next_state(S::Staged, T::Armed), S::Armed);
        assert_eq!(next_state(S::Armed, T::Running), S::Running);
        assert_eq!(next_state(S::Running, T::Finished), S::Unofficial);
        assert_eq!(next_state(S::Unofficial, T::Finalized), S::Final);
        // advance is terminal: the heat stays Final.
        assert_eq!(next_state(S::Final, T::Advanced), S::Final);
    }

    #[test]
    fn finalize_is_legal_only_from_unofficial() {
        use HeatCommand as C;
        use HeatState as S;
        use HeatTransition as T;
        assert_eq!(apply(S::Unofficial, C::Finalize), Ok(T::Finalized));
        for &state in &ALL_STATES {
            if state != S::Unofficial {
                assert!(apply(state, C::Finalize).is_err(), "{state:?} + Finalize");
            }
        }
    }

    #[test]
    fn revert_is_legal_only_from_final_and_lands_unofficial() {
        use HeatCommand as C;
        use HeatState as S;
        use HeatTransition as T;
        // Legal from Final, recording Reverted, landing back at Unofficial.
        assert_eq!(apply(S::Final, C::Revert), Ok(T::Reverted));
        assert_eq!(next_state(S::Final, T::Reverted), S::Unofficial);
        // Illegal everywhere else.
        for &state in &ALL_STATES {
            if state != S::Final {
                assert!(apply(state, C::Revert).is_err(), "{state:?} + Revert");
            }
        }
    }

    #[test]
    fn restart_is_legal_from_armed_running_unofficial_landing_staged() {
        use HeatCommand as C;
        use HeatState as S;
        use HeatTransition as T;
        for &state in &[S::Armed, S::Running, S::Unofficial] {
            assert_eq!(apply(state, C::Restart), Ok(T::Restarted), "{state:?}");
            assert_eq!(next_state(state, T::Restarted), S::Staged, "{state:?}");
        }
        // Not legal before the heat is committed, nor once finalized.
        for &state in &[S::Scheduled, S::Staged, S::Final] {
            assert!(apply(state, C::Restart).is_err(), "{state:?} + Restart");
        }
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
    fn revert_round_trips_unofficial_final_unofficial() {
        // Finalize then Revert returns a finalized heat to Unofficial, and it can be
        // finalized again — a full correction loop.
        let mut state = HeatState::Unofficial;
        let t = apply(state, HeatCommand::Finalize).expect("Unofficial → Final");
        state = next_state(state, t);
        assert_eq!(state, HeatState::Final);
        let t = apply(state, HeatCommand::Revert).expect("Final → Unofficial");
        state = next_state(state, t);
        assert_eq!(state, HeatState::Unofficial);
        let t = apply(state, HeatCommand::Finalize).expect("Unofficial → Final again");
        state = next_state(state, t);
        assert_eq!(state, HeatState::Final);
    }

    #[test]
    fn apply_then_next_state_round_trips_the_forward_path() {
        // Drive the whole forward path command-by-command, checking each landing.
        let mut state = HeatState::Scheduled;
        let path = [
            (HeatCommand::Stage, HeatState::Staged),
            (HeatCommand::Start, HeatState::Armed),
            // The Armed→Running / Running→Unofficial steps are runtime-driven; the overrides
            // record the same transitions, so the forward path is driven by them here.
            (HeatCommand::SkipCountdown, HeatState::Running),
            (HeatCommand::ForceEnd, HeatState::Unofficial),
            (HeatCommand::Finalize, HeatState::Final),
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
            changed(HeatTransition::Finalized),
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
            changed(HeatTransition::Finalized),
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
            HeatState::Unofficial,
            GraceWindow::UntilScored,
            None
        ));
        // Default is UntilScored.
        assert_eq!(GraceWindow::default(), GraceWindow::UntilScored);
        assert!(consumes_pass(
            HeatState::Unofficial,
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
        assert!(consumes_pass(HeatState::Unofficial, grace, Some(1_500_000)));
        // Exactly at the boundary — consumed (inclusive).
        assert!(consumes_pass(HeatState::Unofficial, grace, Some(2_000_000)));
        // Past the window — not consumed.
        assert!(!consumes_pass(
            HeatState::Unofficial,
            grace,
            Some(2_000_001)
        ));
        // A pass at/before the finish instant — within the window.
        assert!(consumes_pass(HeatState::Unofficial, grace, Some(-5)));
        // Elapsed unknown — treated as within the window.
        assert!(consumes_pass(HeatState::Unofficial, grace, None));
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
