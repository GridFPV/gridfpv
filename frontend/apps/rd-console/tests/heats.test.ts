import { describe, expect, it } from 'vitest';
import type { HeatSummary, RoundDef } from '@gridfpv/types';
import {
  heatDisplayName,
  heatNameById,
  isOpenPracticeRound,
  OPEN_PRACTICE_HEAT_NAME
} from '../src/lib/heats.js';

const round = (over: Partial<RoundDef> = {}): RoundDef =>
  ({
    id: 'r1',
    label: 'Qualifying R1',
    classes: ['c1'],
    format: 'timed_qual',
    params: {},
    win_condition: 'BestLap',
    seeding: 'FromRoster',
    channel_mode: 'Static',
    staging_timer_secs: 300,
    start_procedure: { mode: 'randomized-delay', min_delay_ms: 2000, max_delay_ms: 5000 },
    grace_window: { Duration: { micros: 3_000_000 } },
    ...over
  }) as RoundDef;

const heat = (id: string): HeatSummary => ({
  heat: id,
  lineup: [],
  round: 'r1',
  class: 'c1',
  frequencies: [],
  phase: 'Scheduled',
  is_current: false
});

describe('heats — shared heat-name helper', () => {
  it('names a non-practice heat "<Round> Heat <N>" by its 1-based position in the round', () => {
    const r = round();
    const list = [heat('h-a'), heat('h-b'), heat('h-c')];
    expect(heatDisplayName(r, list[0], list)).toBe('Qualifying R1 Heat 1');
    expect(heatDisplayName(r, list[1], list)).toBe('Qualifying R1 Heat 2');
    expect(heatDisplayName(r, list[2], list)).toBe('Qualifying R1 Heat 3');
  });

  it('names a heat not yet in the list as the next position', () => {
    const r = round();
    const list = [heat('h-a')];
    expect(heatDisplayName(r, heat('h-new'), list)).toBe('Qualifying R1 Heat 2');
  });

  it('names every open-practice heat the fixed practice name', () => {
    const r = round({ format: 'open_practice', label: 'Open Practice' });
    expect(isOpenPracticeRound(r)).toBe(true);
    expect(heatDisplayName(r, heat('p-1'), [heat('p-1')])).toBe(OPEN_PRACTICE_HEAT_NAME);
  });

  it('reports a non-practice round as not open-practice', () => {
    expect(isOpenPracticeRound(round())).toBe(false);
  });
});

describe('heats — heatNameById (by-id resolution for Live control)', () => {
  it('resolves a heat id to its "<Round> Heat N" name via the heats list + rounds', () => {
    const r = round();
    const list = [heat('h-a'), heat('h-b')];
    expect(heatNameById('h-b', list, [r])).toBe('Qualifying R1 Heat 2');
  });

  it('resolves an open-practice heat id to the fixed practice name', () => {
    const r = round({ format: 'open_practice', label: 'Open Practice' });
    expect(heatNameById('p-1', [heat('p-1')], [r])).toBe(OPEN_PRACTICE_HEAT_NAME);
  });

  it('falls back to the bare id when the heat is not in the list', () => {
    expect(heatNameById('unknown', [heat('h-a')], [round()])).toBe('unknown');
  });

  it('falls back to the bare id when the heat carries no resolvable round (sim/free-text)', () => {
    const untagged: HeatSummary = { ...heat('q-1'), round: undefined };
    expect(heatNameById('q-1', [untagged], [round()])).toBe('q-1');
  });
});
