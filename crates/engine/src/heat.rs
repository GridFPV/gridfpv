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
//! Armed      --> Scheduled  : abort
//! Running    --> Scheduled  : abort
//! Running    --> Scheduled  : restart
//! Armed      --> Scheduled  : restart
//! Unofficial --> Scheduled  : restart
//! Unofficial --> Scheduled  : discard & re-run
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
    /// Abandon before finalizing — always resets the heat to `Scheduled`, from any
    /// abortable state (Staged/Armed/Running), so the RD re-Stages it.
    Abort,
    /// Restart a committed heat — always resets it to `Scheduled`, from any committed
    /// state (Armed/Running/Unofficial), so the RD re-Stages it.
    Restart,
    /// Discard a raced heat for a re-run — throw the result away (Unofficial or Final →
    /// Scheduled).
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

        // Off-ramps. Abort is legal from Staged/Armed/Running; it always resets the
        // heat to `Scheduled` (see `next_state`), so the RD re-Stages it.
        (S::Staged | S::Armed | S::Running, C::Abort) => HeatTransition::Aborted,
        // Restart applies to any committed heat short of finalized (Armed/Running/
        // Unofficial); it always resets the heat to `Scheduled` (see `next_state`), so
        // the RD re-Stages it for a clean re-run.
        (S::Armed | S::Running | S::Unofficial, C::Restart) => HeatTransition::Restarted,
        // Revert re-opens a finalized result for correction (Final → Unofficial).
        (S::Final, C::Revert) => HeatTransition::Reverted,
        // Discard-and-re-run applies to a raced heat (Unofficial or Final) — throw the
        // result away and reset to Scheduled (see `next_state`) for a clean re-run.
        (S::Unofficial | S::Final, C::Discard) => HeatTransition::Discarded,

        // Everything else is illegal in this state.
        _ => return Err(IllegalTransition { state, command }),
    };
    Ok(transition)
}

/// The state a recorded [`HeatTransition`] lands in, given the state it left.
///
/// The forward transitions name their target state directly. The off-ramps land per
/// the diagram:
/// - `Aborted` → `Scheduled` (from any abortable state — Staged/Armed/Running — so the
///   RD re-Stages it).
/// - `Restarted` → `Scheduled` (a committed heat fully reset, so the RD re-Stages it).
/// - `Reverted` → `Unofficial` (a finalized heat re-opened for correction).
/// - `Discarded` → `Scheduled` (a raced heat — Unofficial or Final — queued for re-run).
/// - `Advanced` is terminal for the heat; it stays `Final`.
///
/// `from` is no longer consulted for any transition (every target is now state-
/// independent), but is kept in the signature so the fold reads uniformly. If a
/// transition is replayed from an unexpected state it still resolves to its canonical
/// target; legality is [`apply`]'s job, not this function's.
pub fn next_state(_from: HeatState, transition: HeatTransition) -> HeatState {
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
        // Abort always resets the heat to Scheduled (from any abortable state), so the
        // RD re-Stages it.
        T::Aborted => S::Scheduled,
        // Restart always resets the heat to Scheduled (from any committed state), so the
        // RD re-Stages it — consistent with Abort.
        T::Restarted => S::Scheduled,
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

/// The **protest window** for the provisional → official lifecycle (marshaling Slice 5,
/// marshaling.html §3.3): an optional, OFF-by-default **auto-official timer**.
///
/// A heat sits in [`Unofficial`](HeatState::Unofficial) (provisional, correctable) after the race
/// closes. When a protest window is configured, the runtime **auto-finalizes** it
/// (`Unofficial → Final`) once the window elapses from the race-end instant; the RD can always
/// finalize early (manually) or correct during the window, and [`Revert`](HeatCommand::Revert)
/// re-opens a finalized result. This is **not** a gate that blocks `Finalize` — it is an additive
/// auto-finalize. The default ([`Off`](Self::Off)) is today's behaviour: manual `Finalize` only.
///
/// Derives serde + `ts_rs::TS` so it can be carried as a per-round config
/// ([`RoundDef::protest_window`](../../server/events/struct.RoundDef.html)) and read by the
/// frontend; it mirrors [`GraceWindow`]'s shape (an off variant + a bounded duration).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "bindings/")]
pub enum ProtestWindow {
    /// No auto-official timer (the default): the heat stays `Unofficial` until the RD manually
    /// finalizes it. Today's behaviour, unchanged.
    #[default]
    Off,
    /// Auto-finalize `micros` microseconds after the race-end instant: once the window elapses the
    /// runtime appends the `Finalize` the RD would have pressed. The RD may still finalize early.
    After {
        /// Length of the protest window, in microseconds (server wall clock), counted from the
        /// `Running → Unofficial` race-end instant.
        #[ts(type = "number")]
        micros: i64,
    },
}

/// Whether an `Unofficial` heat's auto-official timer is **due** (marshaling Slice 5): the protest
/// window has elapsed, so the runtime should append the auto `Finalize` (`Unofficial → Final`).
///
/// Pure — like [`consumes_pass`], it reads no clock: the elapsed time is an **input**. The runtime
/// supplies `since_finished_micros` (now − the race-end instant) and emits the `Finalize` itself.
///
/// Inputs:
/// - `state` — the heat's current [`HeatState`]. Only an `Unofficial` heat can auto-finalize.
/// - `window` — the configured [`ProtestWindow`]. [`Off`](ProtestWindow::Off) never fires.
/// - `since_finished_micros` — microseconds elapsed since the race-end instant (server clock).
///   `None` (the race-end instant is unknown / the heat has not finished) is never due.
///
/// Returns `true` iff `state` is `Unofficial`, `window` is [`After`](ProtestWindow::After), and the
/// elapsed time is at least the window (inclusive — exactly at the deadline is due).
pub fn auto_official_due(
    state: HeatState,
    window: ProtestWindow,
    since_finished_micros: Option<i64>,
) -> bool {
    match (state, window) {
        (HeatState::Unofficial, ProtestWindow::After { micros }) => {
            since_finished_micros.is_some_and(|elapsed| elapsed >= micros)
        }
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
            (S::Unofficial, C::Discard, T::Discarded),
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
        // 4 manual forward edges + 2 overrides + 3 aborts + 3 restarts + revert + 2 discards = 15.
        assert_eq!(legal_table().len(), 15);
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
    fn restart_is_legal_from_armed_running_unofficial_landing_scheduled() {
        use HeatCommand as C;
        use HeatState as S;
        use HeatTransition as T;
        for &state in &[S::Armed, S::Running, S::Unofficial] {
            assert_eq!(apply(state, C::Restart), Ok(T::Restarted), "{state:?}");
            assert_eq!(next_state(state, T::Restarted), S::Scheduled, "{state:?}");
        }
        // Not legal before the heat is committed, nor once finalized.
        for &state in &[S::Scheduled, S::Staged, S::Final] {
            assert!(apply(state, C::Restart).is_err(), "{state:?} + Restart");
        }
    }

    #[test]
    fn abort_always_resets_to_scheduled() {
        use HeatState as S;
        use HeatTransition as T;
        // Abort always lands in Scheduled, from any abortable state, so the RD
        // re-Stages the heat.
        assert_eq!(next_state(S::Staged, T::Aborted), S::Scheduled);
        assert_eq!(next_state(S::Armed, T::Aborted), S::Scheduled);
        assert_eq!(next_state(S::Running, T::Aborted), S::Scheduled);
    }

    #[test]
    fn discard_is_legal_from_unofficial_and_final_landing_scheduled() {
        use HeatCommand as C;
        use HeatState as S;
        use HeatTransition as T;
        // A raced heat (Unofficial or Final) can be thrown away; Discarded resets it to
        // Scheduled for a clean re-run (the UI offers Discard in both states).
        for &state in &[S::Unofficial, S::Final] {
            assert_eq!(apply(state, C::Discard), Ok(T::Discarded), "{state:?}");
            assert_eq!(next_state(state, T::Discarded), S::Scheduled, "{state:?}");
        }
        // Illegal before the heat has any result to throw away.
        for &state in &[S::Scheduled, S::Staged, S::Armed, S::Running] {
            assert!(apply(state, C::Discard).is_err(), "{state:?} + Discard");
        }
    }

    #[test]
    fn restart_and_discard_land_correctly() {
        use HeatState as S;
        use HeatTransition as T;
        assert_eq!(next_state(S::Running, T::Restarted), S::Scheduled);
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
        // Stage, arm, run, abort (back to Scheduled), then re-stage, re-arm and run on.
        let events = vec![
            scheduled(),
            changed(HeatTransition::Staged),
            changed(HeatTransition::Armed),
            changed(HeatTransition::Running),
            changed(HeatTransition::Aborted), // Running → Scheduled
            changed(HeatTransition::Staged),
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
        assert_eq!(first, Some(HeatState::Scheduled));
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

    // --- the auto-official timer (marshaling Slice 5) ---------------------------------

    #[test]
    fn protest_window_off_never_auto_finalizes() {
        // OFF = manual Finalize only (today's behaviour): no elapsed value ever makes it due.
        for elapsed in [None, Some(-1), Some(0), Some(1_000_000), Some(i64::MAX)] {
            assert!(
                !auto_official_due(HeatState::Unofficial, ProtestWindow::Off, elapsed),
                "Off must never be due (elapsed {elapsed:?})",
            );
        }
    }

    #[test]
    fn protest_window_after_is_due_once_the_window_elapses() {
        let window = ProtestWindow::After { micros: 2_000_000 };
        // Before the window — not due.
        assert!(!auto_official_due(
            HeatState::Unofficial,
            window,
            Some(1_999_999)
        ));
        // Exactly at the deadline — due (inclusive).
        assert!(auto_official_due(
            HeatState::Unofficial,
            window,
            Some(2_000_000)
        ));
        // Past the deadline — due.
        assert!(auto_official_due(
            HeatState::Unofficial,
            window,
            Some(2_500_000)
        ));
        // An unknown race-end instant is never due (the runtime can't place the deadline yet).
        assert!(!auto_official_due(HeatState::Unofficial, window, None));
    }

    #[test]
    fn auto_official_is_due_only_from_unofficial() {
        let window = ProtestWindow::After { micros: 0 };
        // Even a zero-length window only fires from Unofficial; any other state is never due,
        // so the timer can never skip the provisional phase or re-fire on a finalized heat.
        for &state in &ALL_STATES {
            let due = auto_official_due(state, window, Some(10_000_000));
            assert_eq!(
                due,
                state == HeatState::Unofficial,
                "{state:?} auto-official due should be {}",
                state == HeatState::Unofficial,
            );
        }
    }
}
