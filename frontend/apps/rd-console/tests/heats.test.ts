import { describe, expect, it } from 'vitest';
import type { HeatSummary, RoundDef } from '@gridfpv/types';
import {
  heatDisplayName,
  heatNameById,
  REMOVED_HEAT_NAME,
  isMultiMainRound,
  isOpenPracticeRound,
  mainTierName,
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

  it('names the one open-practice heat the fixed practice name', () => {
    const r = round({ format: 'open_practice', label: 'Open Practice' });
    expect(isOpenPracticeRound(r)).toBe(true);
    expect(heatDisplayName(r, heat('p-1'), [heat('p-1')])).toBe(OPEN_PRACTICE_HEAT_NAME);
  });

  it('numbers practice heats only once the RD has added a second by hand', () => {
    // The round's own fill emits one practice heat, ever — so the plain name is right for it. But
    // "Add heat" can seat more, and two rows both reading "Practice Heat" name nothing at all.
    const r = round({ format: 'open_practice', label: 'Open Practice' });
    const list = [heat('p-1'), heat('p-2')];
    expect(heatDisplayName(r, list[0], list)).toBe(`${OPEN_PRACTICE_HEAT_NAME} 1`);
    expect(heatDisplayName(r, list[1], list)).toBe(`${OPEN_PRACTICE_HEAT_NAME} 2`);
    // An RD-typed label still wins over the derived name, as it does everywhere.
    expect(heatDisplayName(r, { ...list[1], label: 'Whoop session' }, list)).toBe('Whoop session');
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

  it('#418: a heat the event no longer serves is NAMED as removed, never rendered as its id', () => {
    // Removing a round takes its unstarted heats with it. The live `current_heat` can still be
    // pointing at one (the RD had it loaded in Live control), and the raw id must not surface.
    expect(heatNameById('unknown', [heat('h-a')], [round()])).toBe(REMOVED_HEAT_NAME);
    expect(heatNameById('unknown', [heat('h-a')], [round()])).not.toContain('unknown');
  });

  it('falls back to the bare id when the heat carries no resolvable round (sim/free-text)', () => {
    const untagged: HeatSummary = { ...heat('q-1'), round: undefined };
    expect(heatNameById('q-1', [untagged], [round()])).toBe('q-1');
  });

  it('prefers a custom label even when the heat has no resolvable round', () => {
    const labelled: HeatSummary = { ...heat('q-1'), round: undefined, label: 'Featured Heat' };
    expect(heatNameById('q-1', [labelled], [round()])).toBe('Featured Heat');
  });
});

describe('heats — a custom label overrides the derived auto-name (build-heat)', () => {
  it('heatDisplayName returns the custom label over the "<Round> Heat N" convention', () => {
    const r = round();
    const labelled: HeatSummary = { ...heat('h-a'), label: 'Featured Heat' };
    expect(heatDisplayName(r, labelled, [labelled])).toBe('Featured Heat');
  });

  it('heatDisplayName returns the custom label over the multi-main tier name', () => {
    const r = round({ format: 'multi_main' });
    const labelled: HeatSummary = { ...heat('main-A'), label: 'Grand Final' };
    expect(heatDisplayName(r, labelled, [labelled])).toBe('Grand Final');
  });

  it('falls back to the derived name when the label is absent or blank', () => {
    const r = round();
    const list = [heat('h-a')];
    expect(heatDisplayName(r, { ...list[0], label: undefined }, list)).toBe('Qualifying R1 Heat 1');
    expect(heatDisplayName(r, { ...list[0], label: '   ' }, list)).toBe('Qualifying R1 Heat 1');
  });

  it('heatNameById resolves a labelled heat to its custom label', () => {
    const r = round();
    const labelled: HeatSummary = { ...heat('h-a'), label: 'Featured Heat' };
    expect(heatNameById('h-a', [labelled], [r])).toBe('Featured Heat');
  });
});

describe('heats — multi-main tier naming (#219)', () => {
  const mainsRound = (over: Partial<RoundDef> = {}): RoundDef =>
    round({ format: 'multi_main', label: 'Mains', ...over });

  it('reports a multi-main round as such (and not open-practice)', () => {
    expect(isMultiMainRound(mainsRound())).toBe(true);
    expect(isOpenPracticeRound(mainsRound())).toBe(false);
    expect(isMultiMainRound(round())).toBe(false);
  });

  it('maps a main index to its tier name (A-Main, B-Main, …)', () => {
    expect(mainTierName(0)).toBe('A-Main');
    expect(mainTierName(1)).toBe('B-Main');
    expect(mainTierName(2)).toBe('C-Main');
    expect(mainTierName(25)).toBe('Z-Main');
    // Past the alphabet falls back to a numbered main rather than running off the letters.
    expect(mainTierName(26)).toBe('Main 27');
  });

  it('names each multi-main heat by its tier (position in the round)', () => {
    const r = mainsRound();
    const list = [heat('main-A'), heat('main-B'), heat('main-C')];
    expect(heatDisplayName(r, list[0], list)).toBe('A-Main');
    expect(heatDisplayName(r, list[1], list)).toBe('B-Main');
    expect(heatDisplayName(r, list[2], list)).toBe('C-Main');
  });

  it('names a not-yet-listed multi-main heat as the next tier', () => {
    const r = mainsRound();
    const list = [heat('main-A')];
    expect(heatDisplayName(r, heat('main-B'), list)).toBe('B-Main');
  });

  it('resolves a multi-main heat id to its tier name via heatNameById', () => {
    const r = mainsRound();
    const list = [heat('main-A'), heat('main-B')];
    expect(heatNameById('main-B', list, [r])).toBe('B-Main');
  });
});
