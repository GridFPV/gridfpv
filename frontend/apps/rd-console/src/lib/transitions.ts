/**
 * The heat-loop transition model for live race control (#54).
 *
 * The heat loop is a linear forward path with off-ramps (race-engine.html §2,
 * protocol.html §1):
 *
 *     Scheduled → Staged → Armed → Running → Finished → Final → (Advanced)
 *
 * `Command` exposes one variant per forward step (`Stage`/`Arm`/`Start`/`Finish`/
 * `Score`/`Advance`) and three off-ramps (`Abort`/`Restart`/`Discard`). Each carries
 * the same `{ heat }` payload and requests the matching `HeatTransition`; the engine
 * validates legality against the heat's *current* state — but the console disables
 * illegal actions up front so the RD never fires a command that can only fail
 * (clients.html §5: "reversible mistakes", progressive disclosure).
 *
 * The phase the projection reports is `HeatPhase` (the folded view). This module is
 * the single source of truth for "given this phase, which actions are legal, and what
 * `Command` does each emit" — built once here, unit-tested exhaustively, and consumed
 * by the live-control screen so the buttons and the tests agree by construction.
 */

import type { Command, HeatId, HeatPhase } from '@gridfpv/types';

/**
 * The console-facing name of a heat-loop action. Mirrors the forward
 * `Command`/`HeatTransition` steps plus the three off-ramps. (`Start` enters
 * `Running`; `Finish` enters the projected `Finished` phase; `Score` enters `Final`.)
 */
export type HeatAction =
  | 'Stage'
  | 'Arm'
  | 'Start'
  | 'Finish'
  | 'Score'
  | 'Advance'
  | 'Abort'
  | 'Restart'
  | 'Discard';

/** Actions that destroy or rewind progress — the console confirms these (§5). */
export const DESTRUCTIVE_ACTIONS: ReadonlySet<HeatAction> = new Set<HeatAction>([
  'Abort',
  'Restart',
  'Discard'
]);

/** The forward "primary" action for each phase (the obvious next step), if any. */
const PRIMARY_BY_PHASE: Record<HeatPhase, HeatAction | null> = {
  Scheduled: 'Stage',
  Staged: 'Arm',
  Armed: 'Start',
  Running: 'Finish',
  Finished: 'Score',
  Final: 'Advance'
};

/**
 * Which actions are legal in each phase.
 *
 * Forward steps follow the linear path. The off-ramps are available where they make
 * sense:
 *   • `Abort` — bail out of a heat that has been committed to but not yet scored
 *     (Staged/Armed/Running): stop it where it is.
 *   • `Restart` — re-run from the top once committed (Staged/Armed/Running/Finished):
 *     a bad start, a crash before the window, a contested run.
 *   • `Discard` — throw the heat away entirely once it has results to throw away
 *     (Finished/Final): it should never have counted.
 *
 * The engine is the final authority (it re-validates), so this errs toward the RD's
 * mental model rather than encoding every edge; an over-permissive entry simply
 * yields a `CommandAck` error the screen surfaces.
 */
const LEGAL_BY_PHASE: Record<HeatPhase, ReadonlySet<HeatAction>> = {
  Scheduled: new Set<HeatAction>(['Stage']),
  Staged: new Set<HeatAction>(['Arm', 'Abort', 'Restart']),
  Armed: new Set<HeatAction>(['Start', 'Abort', 'Restart']),
  Running: new Set<HeatAction>(['Finish', 'Abort', 'Restart']),
  Finished: new Set<HeatAction>(['Score', 'Restart', 'Discard']),
  Final: new Set<HeatAction>(['Advance', 'Discard'])
};

/** The display order actions render in (forward steps first, then off-ramps). */
export const ACTION_ORDER: readonly HeatAction[] = [
  'Stage',
  'Arm',
  'Start',
  'Finish',
  'Score',
  'Advance',
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
    case 'Arm':
      return { Arm: { heat } };
    case 'Start':
      return { Start: { heat } };
    case 'Finish':
      return { Finish: { heat } };
    case 'Score':
      return { Score: { heat } };
    case 'Advance':
      return { Advance: { heat } };
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
    case 'Arm':
      return 'Arm the heat — pilots ready, timer armed.';
    case 'Start':
      return 'Start the race. The clock begins.';
    case 'Finish':
      return 'End the race window. Pilots land.';
    case 'Score':
      return 'Score the heat and lock in the result.';
    case 'Advance':
      return 'Advance to the next heat.';
    case 'Abort':
      return 'Abort this heat where it is (no result).';
    case 'Restart':
      return 'Throw out this run and re-stage from the top.';
    case 'Discard':
      return 'Discard this heat entirely — it will not count.';
  }
}
