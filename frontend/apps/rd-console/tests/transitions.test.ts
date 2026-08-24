import { describe, expect, it } from 'vitest';
import type { HeatPhase } from '@gridfpv/types';
import {
  ACTION_ORDER,
  CEREMONY_ACTIONS,
  actionDescription,
  actionsForKind,
  commandForAction,
  commandsForAction,
  actionLabel,
  isActionLegal,
  isDestructive,
  legalActions,
  primaryAction,
  type HeatAction
} from '../src/lib/transitions.js';

const PHASES: HeatPhase[] = ['Scheduled', 'Staged', 'Armed', 'Running', 'Unofficial', 'Final'];

describe('transitions: phase → legal actions', () => {
  it('maps each phase to its forward primary action (Armed/Running auto-advance, no primary)', () => {
    expect(primaryAction('Scheduled')).toBe('Stage');
    expect(primaryAction('Staged')).toBe('Start');
    // The runtime clock drives Armed → Running and Running → Unofficial — no primary button.
    expect(primaryAction('Armed')).toBe(null);
    expect(primaryAction('Running')).toBe(null);
    expect(primaryAction('Unofficial')).toBe('Finalize');
    expect(primaryAction('Final')).toBe('Advance');
  });

  it('only allows the forward step from Scheduled (no off-ramps before staging)', () => {
    expect(legalActions('Scheduled')).toEqual(['Stage']);
    expect(isActionLegal('Scheduled', 'Abort')).toBe(false);
    expect(isActionLegal('Scheduled', 'Start')).toBe(false);
  });

  it('Stop (ForceEnd) is legal exactly while Running; SkipCountdown is retired from the console', () => {
    // The countdown is seconds long and the field never raced it with a Skip button — Armed
    // exposes only the off-ramps. The ForceEnd command survives on the wire, labeled Stop.
    expect(isActionLegal('Running', 'ForceEnd')).toBe(true);
    expect(actionLabel('ForceEnd')).toBe('Stop');
    for (const p of PHASES) {
      expect(isActionLegal(p, 'SkipCountdown')).toBe(false);
    }
    for (const p of PHASES.filter((p) => p !== 'Running')) {
      expect(isActionLegal(p, 'ForceEnd')).toBe(false);
    }
    expect(legalActions('Armed')).toEqual(['Abort', 'Restart']);
  });

  it('allows abort once committed (Staged/Armed/Running) but not from Scheduled', () => {
    for (const p of ['Staged', 'Armed', 'Running'] as HeatPhase[]) {
      expect(isActionLegal(p, 'Abort')).toBe(true);
    }
    expect(isActionLegal('Scheduled', 'Abort')).toBe(false);
  });

  it('allows restart from Armed/Running/Unofficial but not Scheduled/Staged/Final', () => {
    for (const p of ['Armed', 'Running', 'Unofficial'] as HeatPhase[]) {
      expect(isActionLegal(p, 'Restart')).toBe(true);
    }
    for (const p of ['Scheduled', 'Staged', 'Final'] as HeatPhase[]) {
      expect(isActionLegal(p, 'Restart')).toBe(false);
    }
  });

  it('allows finalize only from Unofficial', () => {
    expect(isActionLegal('Unofficial', 'Finalize')).toBe(true);
    for (const p of PHASES.filter((p) => p !== 'Unofficial')) {
      expect(isActionLegal(p, 'Finalize')).toBe(false);
    }
  });

  it('allows revert only from Final (the re-open off-ramp)', () => {
    expect(isActionLegal('Final', 'Revert')).toBe(true);
    for (const p of PHASES.filter((p) => p !== 'Final')) {
      expect(isActionLegal(p, 'Revert')).toBe(false);
    }
  });

  it('allows discard only where there is a result to discard (Unofficial/Final)', () => {
    expect(isActionLegal('Unofficial', 'Discard')).toBe(true);
    expect(isActionLegal('Final', 'Discard')).toBe(true);
    expect(isActionLegal('Running', 'Discard')).toBe(false);
  });

  it("disables a phase's non-adjacent forward steps (no skipping)", () => {
    // From Staged you can Start, not the later steps/overrides.
    expect(isActionLegal('Staged', 'Start')).toBe(true);
    for (const a of ['SkipCountdown', 'ForceEnd', 'Finalize', 'Advance'] as HeatAction[]) {
      expect(isActionLegal('Staged', a)).toBe(false);
    }
  });

  it('legalActions is always a subset of ACTION_ORDER and in that order', () => {
    for (const p of PHASES) {
      const legal = legalActions(p);
      for (const a of legal) expect(ACTION_ORDER).toContain(a);
      // monotonic index ⇒ preserves ACTION_ORDER
      const idx = legal.map((a) => ACTION_ORDER.indexOf(a));
      expect(idx).toEqual([...idx].sort((x, y) => x - y));
    }
  });

  it('marks exactly the four off-ramps destructive', () => {
    expect(isDestructive('Revert')).toBe(true);
    expect(isDestructive('Abort')).toBe(true);
    expect(isDestructive('Restart')).toBe(true);
    expect(isDestructive('Discard')).toBe(true);
    for (const a of [
      'Stage',
      'Start',
      'SkipCountdown',
      'ForceEnd',
      'Finalize',
      'Advance'
    ] as HeatAction[]) {
      expect(isDestructive(a)).toBe(false);
    }
  });
});

describe('transitions: action → Command', () => {
  it('emits the matching externally-tagged Command variant carrying { heat }', () => {
    // The action name IS the variant tag for every heat-loop action.
    const actions: HeatAction[] = [
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
    for (const action of actions) {
      const cmd = commandForAction(action, 'heat-7');
      expect(cmd).toEqual({ [action]: { heat: 'heat-7' } });
    }
  });
});

// ── Practice: "Run again", never the ceremony verbs (#393) ────────────────────────────────────
//
// An open-practice heat has no result — post-#398 its laps are ordinary logged `Pass` events, but
// the round is excluded from scoring, so `Finalize`/`Advance`/`Revert` ask the RD to adjudicate
// something that does not exist. Practice drops all three and gets one obvious end-of-run action.

describe('transitions: practice drops the ceremony and gets "Run again" (#393)', () => {
  it('never renders Finalize / Advance / Revert for a practice heat, in any phase', () => {
    const rendered = actionsForKind('Practice');
    for (const ceremony of CEREMONY_ACTIONS) {
      expect(rendered).not.toContain(ceremony);
      for (const p of PHASES) expect(isActionLegal(p, ceremony, 'Practice')).toBe(false);
    }
    // The competition heat is untouched — it still renders and uses all three.
    expect(actionsForKind('Competition')).toEqual([...ACTION_ORDER]);
    expect(isActionLegal('Unofficial', 'Finalize', 'Competition')).toBe(true);
    expect(isActionLegal('Final', 'Advance', 'Competition')).toBe(true);
    expect(isActionLegal('Final', 'Revert', 'Competition')).toBe(true);
  });

  it('offers exactly Run again + Discard at the end of a practice run, Run again primary', () => {
    expect(legalActions('Unofficial', 'Practice')).toEqual(['Restart', 'Discard']);
    expect(primaryAction('Unofficial', 'Practice')).toBe('Restart');
    // The label is the whole point: the same command, named for practice rather than adjudication.
    expect(actionLabel('Restart', 'Practice')).toBe('Run again');
    expect(actionLabel('Restart')).toBe('Restart');
    expect(actionDescription('Restart', 'Practice')).toMatch(/run practice again/i);
  });

  it('leaves the forward path to the line identical — only the end of the run changes', () => {
    for (const p of ['Scheduled', 'Staged', 'Armed', 'Running'] as HeatPhase[]) {
      expect(legalActions(p, 'Practice')).toEqual(legalActions(p, 'Competition'));
      expect(primaryAction(p, 'Practice')).toBe(primaryAction(p, 'Competition'));
    }
  });

  it('never strands a practice heat at Final: Run again re-opens it, then resets', () => {
    // Unreachable through the console now (no Finalize button), but an armed protest window
    // auto-finalizes any heat. Restart from Final is illegal in the engine, so the console sends
    // the re-open itself rather than making the RD spell `Revert`.
    expect(legalActions('Final', 'Practice')).toEqual(['Restart', 'Discard']);
    expect(primaryAction('Final', 'Practice')).toBe('Restart');
    expect(commandsForAction('Restart', 'p-1', 'Final', 'Practice')).toEqual([
      { Revert: { heat: 'p-1' } },
      { Restart: { heat: 'p-1' } }
    ]);
  });

  it('is one command everywhere else — commandsForAction wraps commandForAction', () => {
    for (const p of PHASES) {
      for (const action of ACTION_ORDER) {
        for (const kind of ['Competition', 'Practice'] as const) {
          if (kind === 'Practice' && action === 'Restart' && p === 'Final') continue;
          expect(commandsForAction(action, 'heat-7', p, kind)).toEqual([
            commandForAction(action, 'heat-7')
          ]);
        }
      }
    }
  });

  it('defaults to the competition model when no kind is given (every existing caller)', () => {
    for (const p of PHASES) {
      expect(legalActions(p)).toEqual(legalActions(p, 'Competition'));
      expect(primaryAction(p)).toBe(primaryAction(p, 'Competition'));
    }
    expect(actionsForKind()).toEqual([...ACTION_ORDER]);
  });
});
