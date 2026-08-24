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
 *
 * The model has a **second axis** since #393: a heat's {@link HeatKind}. An open-practice heat
 * walks the same phases and fires the same commands, but it produces no result — so it drops the
 * result-ceremony verbs and spells `Restart` **"Run again"** (see {@link HeatKind}). That is the
 * whole of the practice special case: presentation, decided here, with no branch below the console.
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

/**
 * Which lifecycle a heat's actions follow — the second axis of the model (#393).
 *
 * `Competition` is every scored format: a run produces a result and the RD adjudicates it
 * (`Finalize` makes it official, `Revert` re-opens it, `Advance` moves on).
 *
 * `Practice` is an **open-practice** heat, which produces no result at all. Since #398 its laps are
 * ordinary logged `Pass` events like anyone else's, but the round is excluded from results,
 * standings and rankings — so there is nothing to make official. Offering the ceremony verbs there
 * asks the RD to adjudicate something that does not exist, and `Finalize` in particular is a trap:
 * it strands the heat at `Final`, which the engine will not `Restart` from. A practice heat
 * therefore renders **none** of the three ({@link actionsForKind}) and gets one obvious action at
 * the end of a run instead — `Restart`, labelled **"Run again"**.
 */
export type HeatKind = 'Competition' | 'Practice';

/** Actions that destroy or rewind progress — the console confirms these (§5). */
export const DESTRUCTIVE_ACTIONS: ReadonlySet<HeatAction> = new Set<HeatAction>([
  'Revert',
  'Abort',
  'Restart',
  'Discard'
]);

/**
 * The **result-ceremony** verbs: the ones that only mean anything once a run has produced a result
 * to adjudicate. A {@link HeatKind} of `Practice` never renders them at all — see
 * {@link actionsForKind} and {@link HeatKind}.
 */
export const CEREMONY_ACTIONS: ReadonlySet<HeatAction> = new Set<HeatAction>([
  'Finalize',
  'Advance',
  'Revert'
]);

/**
 * The **runtime-clock overrides** (heat-lifecycle Slice 2): the manual escape hatches that force a
 * transition the runtime clock normally drives on its own. `SkipCountdown` forces `Armed → Running`
 * (skip the start hold); `ForceEnd` forces `Running → Unofficial` (call the race now). They are
 * *secondary* to the forward path — the RD waits for the clock by default — so the console styles
 * them as clearly-labelled "override" buttons, not the obvious next step.
 */
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
 * The forward "primary" action for a **practice** heat (#393).
 *
 * Identical up to the end of the run; from there the obvious next step is not to adjudicate but to
 * **go again**, so `Restart` (rendered "Run again") is the primary at `Unofficial` — and at `Final`
 * too, for a practice heat that an armed protest window auto-finalized, so the RD is never stranded
 * somewhere the console offers no way forward.
 */
const PRACTICE_PRIMARY_BY_PHASE: Record<HeatPhase, HeatAction | null> = {
  Scheduled: 'Stage',
  Staged: 'Start',
  Armed: null,
  Running: null,
  Unofficial: 'Restart',
  Final: 'Restart'
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
  // Armed/Running auto-advance via the runtime clock. Armed exposes only the off-ramps: the
  // countdown is seconds long and racing it with a Skip button was never used in the field —
  // "I usually just hit start anyway". Running keeps one plain Stop (the ForceEnd command).
  Armed: new Set<HeatAction>(['Abort', 'Restart']),
  Running: new Set<HeatAction>(['ForceEnd', 'Abort', 'Restart']),
  Unofficial: new Set<HeatAction>(['Finalize', 'Restart', 'Discard']),
  Final: new Set<HeatAction>(['Advance', 'Revert', 'Discard'])
};

/**
 * Which actions are legal in each phase for a **practice** heat (#393).
 *
 * The forward path and the off-ramps are the competition set minus every
 * {@link CEREMONY_ACTIONS} verb: a practice run has no result, so there is nothing to finalize,
 * re-open, or advance past. What is left at the end of a run is `Restart` — the "Run again" the RD
 * actually wants — and `Discard`.
 *
 * `Final` is the one entry that is *more* permissive than the competition table. A practice heat
 * cannot reach it through this console any more (there is no `Finalize` button to press), but an
 * armed protest window auto-finalizes any heat, practice included, and an older session may have
 * finalized one by hand. A heat parked there with only `Discard` on offer would be worse than the
 * workflow this issue replaced — so "Run again" is offered there too, and
 * {@link commandsForAction} re-opens the heat before resetting it (the engine restarts a committed
 * heat only up to `Unofficial`).
 */
const PRACTICE_LEGAL_BY_PHASE: Record<HeatPhase, ReadonlySet<HeatAction>> = {
  Scheduled: new Set<HeatAction>(['Stage']),
  Staged: new Set<HeatAction>(['Start', 'Abort']),
  Armed: new Set<HeatAction>(['Abort', 'Restart']),
  Running: new Set<HeatAction>(['ForceEnd', 'Abort', 'Restart']),
  Unofficial: new Set<HeatAction>(['Restart', 'Discard']),
  Final: new Set<HeatAction>(['Restart', 'Discard'])
};

/** The legality table `kind` reads. */
function legalTable(kind: HeatKind): Record<HeatPhase, ReadonlySet<HeatAction>> {
  return kind === 'Practice' ? PRACTICE_LEGAL_BY_PHASE : LEGAL_BY_PHASE;
}

/** The display order actions render in (forward steps first, then overrides, then off-ramps). */
export const ACTION_ORDER: readonly HeatAction[] = [
  'Stage',
  'Start',
  'ForceEnd',
  'Finalize',
  'Advance',
  'Revert',
  'Abort',
  'Restart',
  'Discard'
];

/**
 * Every action the console **renders** for `kind`, in {@link ACTION_ORDER}.
 *
 * The controls row draws each of these once and disables the ones illegal in the current phase, so
 * this is the set an RD can ever see. A practice heat drops the {@link CEREMONY_ACTIONS} entirely —
 * not merely disabled, absent: a greyed-out `Finalize` still reads as "the thing I am supposed to
 * do eventually", which is exactly the misdirection #393 is about.
 */
export function actionsForKind(kind: HeatKind = 'Competition'): HeatAction[] {
  if (kind !== 'Practice') return [...ACTION_ORDER];
  return ACTION_ORDER.filter((a) => !CEREMONY_ACTIONS.has(a));
}

/** Is `action` legal to fire while a `kind` heat is in `phase`? */
export function isActionLegal(
  phase: HeatPhase,
  action: HeatAction,
  kind: HeatKind = 'Competition'
): boolean {
  return legalTable(kind)[phase].has(action);
}

/** Every action legal in `phase` for `kind`, in {@link ACTION_ORDER}. */
export function legalActions(phase: HeatPhase, kind: HeatKind = 'Competition'): HeatAction[] {
  return actionsForKind(kind).filter((a) => isActionLegal(phase, a, kind));
}

/** The single forward "primary" action for `phase`, or `null` at no obvious step. */
export function primaryAction(phase: HeatPhase, kind: HeatKind = 'Competition'): HeatAction | null {
  return kind === 'Practice' ? PRACTICE_PRIMARY_BY_PHASE[phase] : PRIMARY_BY_PHASE[phase];
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

/**
 * The command(s) an action fires, **in order** — what the console actually sends.
 *
 * Every action is one command ({@link commandForAction}) with a single exception: practice's
 * "Run again" from `Final`. The engine restarts a committed heat only up to `Unofficial`
 * (`heat.rs`: `Armed | Running | Unofficial` + `Restart` → `Restarted`), so a practice heat that an
 * armed protest window auto-finalized has to be re-opened first. The console spells that as one
 * button and sends the two commands itself, rather than making the RD reach for `Revert` — the
 * adjudication verb this format has no use for.
 */
export function commandsForAction(
  action: HeatAction,
  heat: HeatId,
  phase: HeatPhase,
  kind: HeatKind = 'Competition'
): Command[] {
  if (kind === 'Practice' && action === 'Restart' && phase === 'Final') {
    return [{ Revert: { heat } }, { Restart: { heat } }];
  }
  return [commandForAction(action, heat)];
}

/** A short human label for an action button. */
export function actionLabel(action: HeatAction, kind: HeatKind = 'Competition'): string {
  // Practice re-runs constantly and adjudicates nothing, so its `Restart` is named for what the RD
  // is doing — going again — not for throwing out a contested run (#393). Same command, same reset.
  if (kind === 'Practice' && action === 'Restart') return 'Run again';
  switch (action) {
    // The command stays ForceEnd on the wire; to the RD it is simply the race's Stop button.
    case 'ForceEnd':
      return 'Stop';
    case 'SkipCountdown':
      return 'Skip countdown';
    default:
      return action;
  }
}

/** A one-line description of what an action does, for tooltips / confirms. */
export function actionDescription(action: HeatAction, kind: HeatKind = 'Competition'): string {
  if (kind === 'Practice') {
    switch (action) {
      case 'Restart':
        return 'Clear the board and run practice again — the heat re-stages from the top.';
      case 'Discard':
        return 'End this practice session — the heat is thrown away, laps and all.';
      default:
        break;
    }
  }
  switch (action) {
    case 'Stage':
      return 'Call pilots to the line and stage the heat.';
    case 'Start':
      return 'Start the heat — arm it and run the start procedure. The race auto-starts after the countdown.';
    case 'SkipCountdown':
      return 'Skip the countdown — start the race now.';
    case 'ForceEnd':
      return 'Stop the race now. Pilots land.';
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
