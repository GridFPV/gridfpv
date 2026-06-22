import { describe, expect, it } from 'vitest';
import type { HeatPhase } from '@gridfpv/types';
import {
  ACTION_ORDER,
  commandForAction,
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

  it('exposes the clock overrides only in their state (SkipCountdown from Armed, ForceEnd from Running)', () => {
    expect(isActionLegal('Armed', 'SkipCountdown')).toBe(true);
    expect(isActionLegal('Running', 'ForceEnd')).toBe(true);
    for (const p of PHASES.filter((p) => p !== 'Armed')) {
      expect(isActionLegal(p, 'SkipCountdown')).toBe(false);
    }
    for (const p of PHASES.filter((p) => p !== 'Running')) {
      expect(isActionLegal(p, 'ForceEnd')).toBe(false);
    }
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
