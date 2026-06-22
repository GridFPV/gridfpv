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

const PHASES: HeatPhase[] = ['Scheduled', 'Staged', 'Armed', 'Running', 'Finished', 'Final'];

describe('transitions: phase → legal actions', () => {
  it('maps each phase to its forward primary action', () => {
    expect(primaryAction('Scheduled')).toBe('Stage');
    expect(primaryAction('Staged')).toBe('Arm');
    expect(primaryAction('Armed')).toBe('Start');
    expect(primaryAction('Running')).toBe('Finish');
    expect(primaryAction('Finished')).toBe('Score');
    expect(primaryAction('Final')).toBe('Advance');
  });

  it('only allows the forward step from Scheduled (no off-ramps before staging)', () => {
    expect(legalActions('Scheduled')).toEqual(['Stage']);
    expect(isActionLegal('Scheduled', 'Abort')).toBe(false);
    expect(isActionLegal('Scheduled', 'Arm')).toBe(false);
  });

  it('allows abort/restart once committed (Staged/Armed/Running) but not from Scheduled', () => {
    for (const p of ['Staged', 'Armed', 'Running'] as HeatPhase[]) {
      expect(isActionLegal(p, 'Abort')).toBe(true);
      expect(isActionLegal(p, 'Restart')).toBe(true);
    }
    expect(isActionLegal('Scheduled', 'Restart')).toBe(false);
  });

  it('allows discard only where there is a result to discard (Finished/Final)', () => {
    expect(isActionLegal('Finished', 'Discard')).toBe(true);
    expect(isActionLegal('Final', 'Discard')).toBe(true);
    expect(isActionLegal('Running', 'Discard')).toBe(false);
  });

  it("disables a phase's non-adjacent forward steps (no skipping)", () => {
    // From Staged you can Arm, not Start/Finish/Score/Advance.
    expect(isActionLegal('Staged', 'Arm')).toBe(true);
    for (const a of ['Start', 'Finish', 'Score', 'Advance'] as HeatAction[]) {
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

  it('marks exactly the three off-ramps destructive', () => {
    expect(isDestructive('Abort')).toBe(true);
    expect(isDestructive('Restart')).toBe(true);
    expect(isDestructive('Discard')).toBe(true);
    for (const a of ['Stage', 'Arm', 'Start', 'Finish', 'Score', 'Advance'] as HeatAction[]) {
      expect(isDestructive(a)).toBe(false);
    }
  });
});

describe('transitions: action → Command', () => {
  it('emits the matching externally-tagged Command variant carrying { heat }', () => {
    // The action name IS the variant tag for every heat-loop action.
    const actions: HeatAction[] = [
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
    for (const action of actions) {
      const cmd = commandForAction(action, 'heat-7');
      expect(cmd).toEqual({ [action]: { heat: 'heat-7' } });
    }
  });
});
