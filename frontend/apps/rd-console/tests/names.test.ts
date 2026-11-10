/**
 * The one resolver from ids to names (`lib/names.ts`). A raw id reaching the screen is a
 * bug, so the fallbacks are pinned here too: an id nobody knows becomes plain words.
 */
import { describe, expect, it } from 'vitest';
import type { PhaseView, Pilot } from '@gridfpv/types';
import { buildNames, ordinal, placeOf, UNKNOWN_PHASE, UNKNOWN_PILOT } from '../src/lib/names.js';
import { heat, group } from './competition.js';

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
