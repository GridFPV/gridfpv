/** The Marshal page's pure helpers: the to-do list, race names, lap rows and times. */
import { describe, expect, it } from 'vitest';
import type { LapView, ProtestView, RaceSeatView } from '@gridfpv/types';
import {
  canRule,
  countedLaps,
  formatOffset,
  lapRows,
  lockedBy,
  marshalCount,
  otherRaces,
  parseOffset,
  protestStateLabel,
  raceName,
  raceNumber,
  signalForSeat,
  timeBase,
  todoList,
  traceMarks,
  uncountedLabel
} from '../src/lib/marshal.js';
import { buildNames } from '../src/lib/names.js';
import { eventView, group, heat, race, raceSeat } from './competition.js';

const S = 1_000_000;
const PILOTS = [
  { id: 'p-1a', callsign: 'Ace', vtx_types: [] },
  { id: 'p-2b', callsign: 'Bee', vtx_types: [] }
];
const phases = [
  group('quali', 'Qualifying'),
  heat('q-1', 'Qualifying Heat 1', ['p-1a', 'p-2b'], { parent: 'quali' }),
  heat('q-2', 'Qualifying Heat 2', ['p-1a'], { parent: 'quali' })
];
const names = buildNames(phases, PILOTS);

const lap = (crossing: number, from: number, to: number, restart = false): LapView => ({
  crossing,
  started_at: from,
  ended_at: to,
  micros: to - from,
  restart
});
const protest = (id: string, raceId: string, over: Partial<ProtestView> = {}): ProtestView => ({
  protest: id,
  race: raceId,
  ...over
});

describe('race names', () => {
  const view = eventView({
    phases,
    races: [race('q-1', 1, 'Discarded'), race('q-1', 2, 'Saved'), race('q-2', 1, 'Saved')]
  });

  it('numbers a race by its place in its heat, discarded races included', () => {
    expect(raceNumber(view, view.races[1])).toBe(2);
    expect(raceNumber(view, view.races[2])).toBe(1);
  });

  it('names a race by its heat label and number, never its id', () => {
    expect(raceName(view, names, view.races[1])).toBe('Qualifying Heat 1 · race 2');
  });
});

describe('the to-do list', () => {
  const view = eventView({
    phases,
    races: [
      race('q-1', 1, 'Saved', { needs_marshal: true, note: 'stray lap?' }),
      race('q-1', 2, 'Saved'),
      race('q-1', 3, 'Discarded', { needs_marshal: true }),
      race('q-2', 1, 'Saved'),
      race('q-2', 2, 'Stopped')
    ],
    protests: [
      protest('pr-1', 'q-2#1'),
      protest('pr-2', 'q-2#1'),
      protest('pr-3', 'q-1#2', { outcome: 'Denied' })
    ]
  });

  it('lists flagged saved races, races with an open protest, and stopped races, with why', () => {
    expect(todoList(view, names)).toEqual([
      {
        race: 'q-1#1',
        heat: 'q-1',
        name: 'Qualifying Heat 1 · race 1',
        reasons: ['Needs a look.'],
        note: 'stray lap?'
      },
      {
        race: 'q-2#1',
        heat: 'q-2',
        name: 'Qualifying Heat 2 · race 1',
        reasons: ['2 open protests.'],
        note: undefined
      },
      {
        race: 'q-2#2',
        heat: 'q-2',
        name: 'Qualifying Heat 2 · race 2',
        reasons: ['Stopped. Not saved or discarded yet.'],
        note: undefined
      }
    ]);
  });

  it('gives both reasons for a flagged race with one open protest', () => {
    const both = eventView({
      phases,
      races: [race('q-1', 1, 'Saved', { needs_marshal: true })],
      protests: [protest('pr-1', 'q-1#1')]
    });
    expect(todoList(both, names)[0].reasons).toEqual(['Needs a look.', '1 open protest.']);
  });

  it('counts flagged and protested races for the badge, not stopped ones', () => {
    expect(marshalCount(view)).toBe(2);
    expect(marshalCount(eventView())).toBe(0);
  });

  it('groups every other race by heat, in the event order', () => {
    expect(otherRaces(view, names)).toEqual([
      {
        heat: 'q-1',
        label: 'Qualifying Heat 1',
        races: [
          { race: 'q-1#2', name: 'Race 2', state: 'Saved' },
          { race: 'q-1#3', name: 'Race 3', state: 'Discarded' }
        ]
      }
    ]);
  });
});

describe('locks and rulings', () => {
  it('finds the phase that carries the lock, at the heat or above it', () => {
    const locked = [
      group('quali', 'Qualifying', undefined, { locked: true, locked_here: true }),
      heat('q-1', 'Qualifying Heat 1', [], { parent: 'quali', locked: true })
    ];
    expect(lockedBy(locked, 'q-1')).toBe('quali');
    expect(lockedBy(phases, 'q-1')).toBeUndefined();
    expect(lockedBy(phases, 'gone')).toBeUndefined();
  });

  it('allows rulings on a stopped or saved race that is not locked', () => {
    expect(canRule(race('q-1', 1, 'Saved'))).toBe(true);
    expect(canRule(race('q-1', 1, 'Stopped'))).toBe(true);
    expect(canRule(race('q-1', 1, 'Saved', { locked: true }))).toBe(false);
    expect(canRule(race('q-1', 1, 'Running'))).toBe(false);
    expect(canRule(race('q-1', 1, 'Discarded'))).toBe(false);
  });
});

describe('words', () => {
  it('says why a crossing does not count', () => {
    expect(uncountedLabel('Removed')).toBe('Removed by a marshal.');
    expect(uncountedLabel('Removed', 'FalseDetection')).toBe(
      'Removed by a marshal: false detection.'
    );
    expect(uncountedLabel('UnderMinLap')).toContain('minimum lap time');
    expect(uncountedLabel('BeforeStart')).toBe('Before the start.');
  });

  it('names a protest by its state', () => {
    expect(protestStateLabel(protest('a', 'r'))).toBe('Open');
    expect(protestStateLabel(protest('a', 'r', { outcome: 'Upheld' }))).toBe('Upheld');
  });
});

describe('lap rows', () => {
  const seat: RaceSeatView = raceSeat(0, 'p-1a', {
    hole_shot: { crossing: 10, at: 1 * S },
    laps: [lap(11, 1 * S, 4 * S), lap(12, 4 * S, 6 * S, true), lap(13, 6 * S, 8 * S)],
    best_lap_micros: 2 * S,
    uncounted: [{ crossing: 14, at: 8.2 * S, why: 'UnderMinLap' }]
  });

  it('lists the hole shot, numbers the laps, and gives a restart no number and no time', () => {
    expect(lapRows(seat)).toEqual([
      { kind: 'holeshot', crossing: 10, at: 1 * S, best: false },
      { kind: 'lap', crossing: 11, at: 4 * S, number: 1, micros: 3 * S, best: false },
      { kind: 'restart', crossing: 12, at: 6 * S, best: false },
      { kind: 'lap', crossing: 13, at: 8 * S, number: 2, micros: 2 * S, best: true }
    ]);
    expect(countedLaps(seat)).toBe(2);
  });

  it('marks only one lap as best when two tie', () => {
    const tied = raceSeat(0, 'p-1a', {
      laps: [lap(1, 0, S), lap(2, S, 2 * S)],
      best_lap_micros: S
    });
    expect(lapRows(tied).map((row) => row.best)).toEqual([true, false]);
  });

  it('marks the start and every crossing for the trace, in time order', () => {
    expect(traceMarks(seat, 0).map((mark) => [mark.kind, mark.text, mark.crossing])).toEqual([
      ['start', 'Start', undefined],
      ['holeshot', 'HS', 10],
      ['lap', '1', 11],
      ['restart', 'R', 12],
      ['lap', '2', 13],
      ['uncounted', '×', 14]
    ]);
    expect(traceMarks(seat)[0].kind).toBe('holeshot');
  });
});

describe('times', () => {
  const seat = raceSeat(0, 'p-1a', {
    hole_shot: { crossing: 10, at: 105 * S },
    laps: [lap(11, 105 * S, 108 * S)]
  });
  const signal = { adapter: 'gate-x1', seat: '0', start_at: 100 * S, chunks: [], thresholds: [] };

  it("counts from the race's start on the timer when the Director measured it", () => {
    const onGate = { ...seat, adapter: 'gate-x1', seat: '0' };
    expect(timeBase(race('q-1', 1, 'Saved'), onGate, signal)).toEqual({
      at: 100 * S,
      words: 'into the race'
    });
    expect(signalForSeat([signal], onGate)).toBe(signal);
    expect(signalForSeat([signal], seat)).toBeUndefined();
  });

  it("counts a bench seat from the race's own start time", () => {
    expect(timeBase(race('q-1', 1, 'Saved', { started_at: 90 * S }), seat)).toEqual({
      at: 90 * S,
      words: 'into the race'
    });
  });

  it('falls back to the first crossing, and to nothing when the seat has none', () => {
    const onGate = { ...seat, adapter: 'gate-x1' };
    expect(timeBase(race('q-1', 1, 'Saved', { started_at: 90 * S }), onGate)).toEqual({
      at: 105 * S,
      words: 'after the first crossing'
    });
    expect(
      timeBase(race('q-1', 1, 'Saved'), raceSeat(0, 'p-1a', { adapter: 'gate-x1' }))
    ).toBeUndefined();
  });

  it('writes a time as seconds from the base and reads it back exactly', () => {
    expect(formatOffset(112_345_678, 100 * S)).toBe('12.346');
    expect(formatOffset(99.5 * S, 100 * S)).toBe('-0.500');
    expect(parseOffset('12.345678', 100 * S)).toBe(112_345_678);
    expect(parseOffset(' 3,5 ', 0)).toBe(3_500_000);
    expect(parseOffset('7', 10)).toBe(7_000_010);
    expect(parseOffset('', 0)).toBeUndefined();
    expect(parseOffset('-1', 0)).toBeUndefined();
    expect(parseOffset('abc', 0)).toBeUndefined();
  });
});
