/**
 * The heat-loop transition model for live race control (#54, heat-lifecycle Slice 2).
 *
 * The heat loop is a linear forward path with off-ramps (race-engine.html §2,
 * protocol.html §1):
 *
 *     Scheduled → Staged → Armed → Running → Unofficial → Final → (Advanced)
 *
 * The manual command set is `Stage`/`Start`/`Finalize`/`Advance` plus the off-ramps
 * (`Revert`/`Abort`/`Restart`/`Discard`). `Start` *arms* the heat (Staged → Armed) and runs the
 * start procedure; the `Armed → Running` and `Running → Unofficial` steps are then driven by the
 * Director's **runtime clock**, not by a button. The two clock **overrides** `SkipCountdown`
 * (force Armed → Running) and `ForceEnd` (force Running → Unofficial) remain for the race-day
 * cases where the clock must be bypassed. Each command carries the same `{ heat }` payload and
 * requests the matching `HeatTransition`; the engine validates legality against the heat's
 * *current* state — but the console disables illegal actions up front so the RD never fires a
 * command that can only fail (clients.html §5: "reversible mistakes", progressive disclosure).
 *
 * The phase the projection reports is `HeatPhase` (the folded view). This module is
 * the single source of truth for "given this phase, which actions are legal, and what
 * `Command` does each emit" — built once here, unit-tested exhaustively, and consumed
 * by the live-control screen so the buttons and the tests agree by construction.
 */

import type { Command, HeatId, HeatPhase } from '@gridfpv/types';

/**
 * The console-facing name of a heat-loop action. Mirrors the manual `Command` steps plus the
 * off-ramps and the runtime-clock overrides. (`Start` arms the heat — the runtime then auto-starts
 * the race; `SkipCountdown` forces the start; `ForceEnd` enters the projected `Unofficial` phase;
 * `Finalize` enters `Final`; `Revert` re-opens a `Final` heat back to `Unofficial`.)
 */
export type HeatAction =
  | 'Stage'
  | 'Start'
  | 'SkipCountdown'
  | 'ForceEnd'
  | 'Finalize'
  | 'Advance'
  | 'Revert'
  | 'Abort'
  | 'Restart'
  | 'Discard';

/** Actions that destroy or rewind progress — the console confirms these (§5). */
export const DESTRUCTIVE_ACTIONS: ReadonlySet<HeatAction> = new Set<HeatAction>([
  'Revert',
  'Abort',
  'Restart',
  'Discard'
]);

/**
 * The forward "primary" action for each phase (the obvious next step), if any.
 *
 * `Armed` and `Running` have **no** primary button: the runtime clock auto-advances them
 * (`Armed → Running` after the start procedure, `Running → Unofficial` on the win condition +
 * grace). The RD waits for the clock; `SkipCountdown`/`ForceEnd` are available as overrides but are
 * not the obvious forward step.
 */
const PRIMARY_BY_PHASE: Record<HeatPhase, HeatAction | null> = {
  Scheduled: 'Stage',
  Staged: 'Start',
  Armed: null,
  Running: null,
  Unofficial: 'Finalize',
  Final: 'Advance'
};

/**
 * Which actions are legal in each phase.
 *
 * Forward steps follow the linear path. The off-ramps are available where they make
 * sense:
 *   • `Revert` — re-open a finalized result for correction (Final → Unofficial): a
 *     scoring fix the RD spotted after locking the heat in.
 *   • `Abort` — bail out of a heat that has been committed to but not yet finalized
 *     (Staged/Armed/Running): stop it where it is.
 *   • `Restart` — re-run from the top once committed (Armed/Running/Unofficial):
 *     a bad start, a crash before the window, a contested run.
 *   • `Discard` — throw the heat away entirely once it has results to throw away
 *     (Unofficial/Final): it should never have counted.
 *
 * The engine is the final authority (it re-validates), so this errs toward the RD's
 * mental model rather than encoding every edge; an over-permissive entry simply
 * yields a `CommandAck` error the screen surfaces.
 */
const LEGAL_BY_PHASE: Record<HeatPhase, ReadonlySet<HeatAction>> = {
  Scheduled: new Set<HeatAction>(['Stage']),
  Staged: new Set<HeatAction>(['Start', 'Abort']),
  // Armed/Running auto-advance via the runtime clock; the overrides are the manual escape hatches.
  Armed: new Set<HeatAction>(['SkipCountdown', 'Abort', 'Restart']),
  Running: new Set<HeatAction>(['ForceEnd', 'Abort', 'Restart']),
  Unofficial: new Set<HeatAction>(['Finalize', 'Restart', 'Discard']),
  Final: new Set<HeatAction>(['Advance', 'Revert', 'Discard'])
};

/** The display order actions render in (forward steps first, then overrides, then off-ramps). */
export const ACTION_ORDER: readonly HeatAction[] = [
  'Stage',
  'Start',
  'SkipCountdown',
  'ForceEnd',
  'Finalize',
  'Advance',
  'Revert',
  'Abort',
  'Restart',
  'Discard'
];

/** Is `action` legal to fire while the heat is in `phase`? */
export function isActionLegal(phase: HeatPhase, action: HeatAction): boolean {
  return LEGAL_BY_PHASE[phase].has(action);
}

/** Every action legal in `phase`, in {@link ACTION_ORDER}. */
export function legalActions(phase: HeatPhase): HeatAction[] {
  return ACTION_ORDER.filter((a) => isActionLegal(phase, a));
}

/** The single forward "primary" action for `phase`, or `null` at no obvious step. */
export function primaryAction(phase: HeatPhase): HeatAction | null {
  return PRIMARY_BY_PHASE[phase];
}

/** Does this action need a confirm before firing (clients.html §5)? */
export function isDestructive(action: HeatAction): boolean {
  return DESTRUCTIVE_ACTIONS.has(action);
}

/**
 * Build the `Command` an action emits for a heat. Every heat-loop action maps to a
 * single externally-tagged `Command` variant carrying `{ heat }` — the action name
 * *is* the variant tag, so this stays a total, mechanical mapping.
 */
export function commandForAction(action: HeatAction, heat: HeatId): Command {
  switch (action) {
    case 'Stage':
      return { Stage: { heat } };
    case 'Start':
      return { Start: { heat } };
    case 'SkipCountdown':
      return { SkipCountdown: { heat } };
    case 'ForceEnd':
      return { ForceEnd: { heat } };
    case 'Finalize':
      return { Finalize: { heat } };
    case 'Advance':
      return { Advance: { heat } };
    case 'Revert':
      return { Revert: { heat } };
    case 'Abort':
      return { Abort: { heat } };
    case 'Restart':
      return { Restart: { heat } };
    case 'Discard':
      return { Discard: { heat } };
  }
}

/** A short human label for an action button. */
export function actionLabel(action: HeatAction): string {
  return action;
}

/** A one-line description of what an action does, for tooltips / confirms. */
export function actionDescription(action: HeatAction): string {
  switch (action) {
    case 'Stage':
      return 'Call pilots to the line and stage the heat.';
    case 'Start':
      return 'Start the heat — arm it and run the start procedure. The race auto-starts after the countdown.';
    case 'SkipCountdown':
      return 'Skip the countdown — start the race now.';
    case 'ForceEnd':
      return 'End the race window now. Pilots land.';
    case 'Finalize':
      return 'Finalize the heat and lock in the result.';
    case 'Advance':
      return 'Advance to the next heat.';
    case 'Revert':
      return 'Re-open this finalized heat to correct its result.';
    case 'Abort':
      return 'Abort this heat where it is (no result).';
    case 'Restart':
      return 'Throw out this run and re-stage from the top.';
    case 'Discard':
      return 'Discard this heat entirely — it will not count.';
  }
}
