/**
 * The seat sources (`lib/seats.ts`): what a heat can be timed by, which one a new heat starts
 * on, and the seats each one hands out.
 */
import { describe, expect, it } from 'vitest';
import {
  BENCH_ADAPTER,
  BENCH_SEAT_COUNT,
  canChoose,
  chosenSource,
  defaultSource,
  isBench,
  presetSeats,
  seatSources,
  sourceOptionLabel
} from '../src/lib/seats.js';
import { timerView } from './competition.js';

const GATE = timerView('gate-x1y2', 'Gate', 4);
const OFF = timerView('far-gate-9zz0', 'Far Gate', 0, { connected: false, problem: 'No answer.' });
const WAKING = timerView('new-gate-1abc', 'New Gate', 0);
const DROPPED = timerView('loop-77aa', 'Loop', 2, { connected: false });

describe('seatSources', () => {
  it('is only the bench when there is no timer', () => {
    const sources = seatSources([]);
    expect(sources).toHaveLength(1);
    expect(sources[0]).toMatchObject({ id: BENCH_ADAPTER, name: 'Bench', bench: true });
    expect(sources[0].seats).toHaveLength(BENCH_SEAT_COUNT);
    expect(sources[0].seats[0]).toEqual({ adapter: 'bench', seat: 'seat-1', label: 'Seat 1' });
    expect(sources[0].seats[7]).toEqual({ adapter: 'bench', seat: 'seat-8', label: 'Seat 8' });
  });

  it('lists every timer, connected or not, then the bench', () => {
    const sources = seatSources([GATE, OFF, DROPPED]);
    expect(sources.map((source) => source.name)).toEqual(['Gate', 'Far Gate', 'Loop', 'Bench']);
    expect(sources.map((source) => source.bench)).toEqual([false, false, false, true]);
    expect(sources.map((source) => source.connected)).toEqual([true, false, false, true]);
  });

  it("labels a timer's seats Seat 1, Seat 2, … in the timer's order and keeps its handles", () => {
    const [gate] = seatSources([GATE]);
    expect(gate.seats).toEqual([
      { adapter: 'gate-x1y2', seat: '0', label: 'Seat 1' },
      { adapter: 'gate-x1y2', seat: '1', label: 'Seat 2' },
      { adapter: 'gate-x1y2', seat: '2', label: 'Seat 3' },
      { adapter: 'gate-x1y2', seat: '3', label: 'Seat 4' }
    ]);
  });

  it('lists a timer with no seats but says why it cannot be chosen', () => {
    const [off, waking] = seatSources([OFF, WAKING]);
    expect(canChoose(off)).toBe(false);
    expect(off.unavailable).toBe('not connected');
    expect(canChoose(waking)).toBe(false);
    expect(waking.unavailable).toBe('no seats reported yet');
  });

  it('lets a timer that dropped but has reported seats still be chosen', () => {
    const [dropped] = seatSources([DROPPED]);
    expect(canChoose(dropped)).toBe(true);
    expect(dropped.seats).toHaveLength(2);
  });
});

describe('defaultSource and chosenSource', () => {
  it('starts on the first connected timer with seats', () => {
    expect(defaultSource(seatSources([OFF, DROPPED, GATE])).name).toBe('Gate');
  });

  it('starts on the bench when no timer is connected with seats', () => {
    expect(defaultSource(seatSources([])).bench).toBe(true);
    expect(defaultSource(seatSources([OFF, WAKING, DROPPED])).bench).toBe(true);
  });

  it("uses the director's pick while it can be chosen", () => {
    const sources = seatSources([GATE, DROPPED]);
    expect(chosenSource(sources, BENCH_ADAPTER).bench).toBe(true);
    expect(chosenSource(sources, 'loop-77aa').name).toBe('Loop');
    expect(chosenSource(sources, undefined).name).toBe('Gate');
  });

  it('falls back to the default when the pick is gone or has no seats', () => {
    expect(chosenSource(seatSources([GATE]), 'removed-0000').name).toBe('Gate');
    expect(chosenSource(seatSources([OFF]), 'far-gate-9zz0').bench).toBe(true);
  });
});

describe('what a source hands out', () => {
  it('presetSeats gives the first seats of the chosen source', () => {
    const [gate, bench] = seatSources([GATE]);
    expect(presetSeats(gate, 2)).toEqual([
      { adapter: 'gate-x1y2', seat: '0' },
      { adapter: 'gate-x1y2', seat: '1' }
    ]);
    expect(presetSeats(bench, 1)).toEqual([{ adapter: 'bench', seat: 'seat-1' }]);
    // Never more than the source has.
    expect(presetSeats(gate, 9)).toHaveLength(4);
  });

  it('only the bench is a bench', () => {
    expect(isBench(BENCH_ADAPTER)).toBe(true);
    expect(isBench('gate-x1y2')).toBe(false);
  });

  it('names each source in the list by name, with its seats or why it is not on offer', () => {
    const labels = seatSources([GATE, OFF, WAKING, DROPPED]).map(sourceOptionLabel);
    expect(labels).toEqual([
      'Gate (4 seats)',
      'Far Gate (not connected)',
      'New Gate (no seats reported yet)',
      'Loop (2 seats, not connected)',
      'Bench (no timer: crossings are logged by hand)'
    ]);
    // By name only: no timer id in what a person reads.
    for (const id of ['gate-x1y2', 'far-gate-9zz0', 'new-gate-1abc', 'loop-77aa']) {
      expect(labels.join(' ')).not.toContain(id);
    }
  });
});
