/**
 * The one resolver from ids to names (`lib/names.ts`). A raw id reaching the screen is a
 * bug, so the fallbacks are pinned here too: an id nobody knows becomes plain words.
 */
import { describe, expect, it } from 'vitest';
import type { PhaseView, Pilot } from '@gridfpv/types';
import { buildNames, ordinal, placeOf, UNKNOWN_PHASE, UNKNOWN_PILOT } from '../src/lib/names.js';
import { BENCH_ADAPTER, UNKNOWN_TIMER } from '../src/lib/seats.js';
import { heat, group, timerView } from './competition.js';

const PILOTS: Pilot[] = [
  { id: 'p-1a', callsign: 'Ace', vtx_types: [] },
  { id: 'p-2b', callsign: 'Bee', vtx_types: [] }
];

const PHASES: PhaseView[] = [
  group('finals', 'Finals'),
  group('finals/winners-1', 'Winners Round 1', 'finals'),
  heat('finals/winners-1/heat-2', 'Winners Round 1 Heat 2', ['p-1a', 'p-2b'], {
    parent: 'finals/winners-1'
  })
];

describe('ordinal', () => {
  it('ends each number the way it is said', () => {
    expect([1, 2, 3, 4, 11, 12, 13, 21, 22, 23, 101, 111].map(ordinal)).toEqual([
      '1st',
      '2nd',
      '3rd',
      '4th',
      '11th',
      '12th',
      '13th',
      '21st',
      '22nd',
      '23rd',
      '101st',
      '111th'
    ]);
  });
});

describe('placeOf', () => {
  it('calls first place the winner and the rest by their place', () => {
    expect(placeOf(1, 'Heat 1')).toBe('Winner of Heat 1');
    expect(placeOf(2, 'Heat 1')).toBe('2nd of Heat 1');
    expect(placeOf(5, 'Heat 1')).toBe('5th of Heat 1');
  });
});

describe('buildNames', () => {
  const names = buildNames(PHASES, PILOTS);

  it('names a phase by its own label, which already reads in full', () => {
    expect(names.phase('finals')).toBe('Finals');
    expect(names.phase('finals/winners-1/heat-2')).toBe('Winners Round 1 Heat 2');
  });

  it('names a pilot by callsign', () => {
    expect(names.pilot('p-2b')).toBe('Bee');
  });

  it('never falls back to the raw id', () => {
    expect(names.phase('finals/gone')).toBe(UNKNOWN_PHASE);
    expect(names.pilot('p-zz')).toBe(UNKNOWN_PILOT);
  });

  it('says who is on a seat', () => {
    expect(names.who({ Pilot: 'p-1a' })).toBe('Ace');
    expect(names.who('Open')).toBe('Open seat');
    expect(names.who('Nobody')).toBe('Empty');
  });

  it('says where a seat that is still waiting will be filled from', () => {
    const phase = 'finals/winners-1/heat-2';
    expect(names.who({ Waiting: { phase, position: 1 } })).toBe('Winner of Winners Round 1 Heat 2');
    expect(names.who({ Waiting: { phase, position: 2 } })).toBe('2nd of Winners Round 1 Heat 2');
    expect(names.who({ Waiting: { phase, position: 4 } })).toBe('4th of Winners Round 1 Heat 2');
    expect(names.who({ Waiting: { phase: 'nope', position: 1 } })).toBe(
      `Winner of ${UNKNOWN_PHASE}`
    );
  });
});

describe('buildNames: seats and timers', () => {
  const TIMERS = [
    timerView('gate-x1y2', 'Gate', 4),
    // Switched off since the Director started: not connected, and no seats reported.
    timerView('far-gate-9zz0', 'Far Gate', 0, { connected: false, problem: 'No answer.' }),
    // Was connected, then dropped: the seats it reported are still known.
    timerView('loop-77aa', 'Loop', 2, { connected: false, problem: 'The connection dropped.' })
  ];
  const names = buildNames(PHASES, PILOTS, TIMERS);

  it('names a timer by the name its director gave it, connected or not', () => {
    expect(names.timer('gate-x1y2')).toBe('Gate');
    expect(names.timer('far-gate-9zz0')).toBe('Far Gate');
    expect(names.timer('loop-77aa')).toBe('Loop');
  });

  it('calls the bench "Bench" and a timer nobody knows "Unknown timer"', () => {
    expect(names.timer(BENCH_ADAPTER)).toBe('Bench');
    expect(names.timer('gone-0000')).toBe(UNKNOWN_TIMER);
    expect(UNKNOWN_TIMER).toBe('Unknown timer');
  });

  it("numbers a timer's seat by its place in the seats the timer reported", () => {
    expect(names.seat('gate-x1y2', '0')).toBe('Seat 1');
    expect(names.seat('gate-x1y2', '3')).toBe('Seat 4');
    // The timer's own order wins over the place in a lineup.
    expect(names.seat('gate-x1y2', '2', 0)).toBe('Seat 3');
  });

  it('still numbers the seats of a timer that is not connected', () => {
    expect(names.seat('loop-77aa', '1')).toBe('Seat 2');
    // Nothing reported yet: the seat's own number says which it is.
    expect(names.seat('far-gate-9zz0', '2')).toBe('Seat 3');
  });

  it('numbers the bench seats', () => {
    expect(names.seat(BENCH_ADAPTER, 'seat-1')).toBe('Seat 1');
    expect(names.seat(BENCH_ADAPTER, 'seat-8')).toBe('Seat 8');
  });

  it('never shows a raw seat handle or timer id', () => {
    // A removed timer: named by the place in the lineup when there is one.
    expect(names.seat('gone-0000', 'gate-b', 1)).toBe('Seat 2');
    expect(names.seat('gone-0000', 'gate-b')).toBe('Seat');
    expect(names.seat(BENCH_ADAPTER, 'seat-99')).toBe('Seat');
  });

  it('works with no timer list at all, as before the first read', () => {
    const bare = buildNames(PHASES, PILOTS);
    expect(bare.seat(BENCH_ADAPTER, 'seat-2')).toBe('Seat 2');
    expect(bare.timer('gate-x1y2')).toBe(UNKNOWN_TIMER);
  });
});
