/**
 * Check-in helpers: which race takes check-ins, its open seats, and a pilot's own practice
 * laps. The Director decides which laps belong to which stint; these only shape it.
 */
import { describe, expect, it } from 'vitest';
import type { Pilot, RaceSeatView } from '@gridfpv/types';
import { checkInRace, openSeats, practiceRuns, takesCheckIns } from '../src/lib/checkin.js';
import { buildNames } from '../src/lib/names.js';
import { eventView, heat, lap, race, raceSeat } from './competition.js';

const S = 1_000_000;
const PILOTS: Pilot[] = [
  { id: 'p-1a', callsign: 'Ace', vtx_types: [] },
  { id: 'p-2b', callsign: 'Bee', vtx_types: [] }
];
const phases = [heat('practice', 'Open Practice', ['Open', 'Open', 'p-2b'])];
const names = buildNames(phases, PILOTS);

const stint = (
  pilot: string,
  open: boolean,
  seconds: number[]
): RaceSeatView['stints'][number] => ({
  pilot,
  open,
  laps: seconds.map((s, index) => lap(100 + index, s)),
  best_lap_micros: seconds.length > 0 ? Math.min(...seconds) * S : undefined
});

describe('the race that takes check-ins', () => {
  it('is one being flown that has a seat with no pilot named on it', () => {
    const flying = race('practice', 1, 'Running', {
      seats: [raceSeat(0), raceSeat(1), raceSeat(2, 'p-2b')]
    });
    expect(checkInRace(eventView({ phases, races: [flying] }))).toBe(flying);
    expect(takesCheckIns(race('practice', 1, 'Staging'))).toBe(true);
  });

  it('is not a race that has stopped, nor one where every seat has a pilot', () => {
    for (const state of ['Stopped', 'Saved', 'Discarded'] as const) {
      const over = race('practice', 1, state, { seats: [raceSeat(0)] });
      expect(checkInRace(eventView({ phases, races: [over] }))).toBeUndefined();
    }
    const full = race('practice', 1, 'Running', { seats: [raceSeat(0, 'p-1a')] });
    expect(checkInRace(eventView({ phases, races: [full] }))).toBeUndefined();
    expect(checkInRace(undefined)).toBeUndefined();
  });
});

describe('the open seats of a race', () => {
  it('lists only the seats with no pilot named, each with who has it now, by name', () => {
    const flying = race('practice', 1, 'Running', {
      seats: [raceSeat(0, undefined, { holder: 'p-1a' }), raceSeat(1), raceSeat(2, 'p-2b')]
    });
    expect(openSeats(flying, names)).toEqual([
      { adapter: 'bench', seat: 'seat-1', seatName: 'Seat 1', holder: 'p-1a', holderName: 'Ace' },
      {
        adapter: 'bench',
        seat: 'seat-2',
        seatName: 'Seat 2',
        holder: undefined,
        holderName: undefined
      }
    ]);
  });
});

describe("a pilot's own practice", () => {
  const view = eventView({
    phases,
    races: [
      race('practice', 1, 'Saved', {
        seats: [
          raceSeat(0, undefined, {
            stints: [stint('p-1a', false, [15, 12]), stint('p-2b', false, [20])]
          }),
          raceSeat(1, undefined, { stints: [stint('p-1a', false, [])] })
        ]
      }),
      race('practice', 2, 'Discarded', {
        seats: [raceSeat(0, undefined, { stints: [stint('p-1a', false, [9])] })]
      }),
      race('practice', 3, 'Running', {
        seats: [raceSeat(0, undefined, { holder: 'p-1a', stints: [stint('p-1a', true, [11])] })]
      })
    ]
  });

  it('is every stint they had, newest first, with the best lap marked', () => {
    const runs = practiceRuns(view, names, 'p-1a');
    expect(runs.map((run) => [run.name, run.open, run.laps.map((m) => m / S)])).toEqual([
      ['Open Practice · Seat 1', true, [11]],
      ['Open Practice · Seat 2', false, []],
      ['Open Practice · Seat 1', false, [15, 12]]
    ]);
    expect(runs[2]).toMatchObject({ bestLapMicros: 12 * S, bestLapIndex: 1 });
    expect(new Set(runs.map((run) => run.key)).size).toBe(3);
  });

  it("leaves out another pilot's stints and a race that was thrown out", () => {
    expect(practiceRuns(view, names, 'p-2b').map((run) => run.laps.map((m) => m / S))).toEqual([
      [20]
    ]);
    expect(practiceRuns(undefined, names, 'p-1a')).toEqual([]);
  });

  it('an open stint in a race that has stopped is no longer "flying now"', () => {
    const stopped = eventView({
      phases,
      races: [
        race('practice', 1, 'Stopped', {
          seats: [raceSeat(0, undefined, { holder: 'p-1a', stints: [stint('p-1a', true, [11])] })]
        })
      ]
    });
    expect(practiceRuns(stopped, names, 'p-1a')[0].open).toBe(false);
  });
});
