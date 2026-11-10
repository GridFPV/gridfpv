/** The Run view's pure helpers (`lib/run.ts`) and the start tone cue (`lib/startTone.ts`). */
import { describe, expect, it, vi } from 'vitest';
import { buildNames } from '../src/lib/names.js';
import {
  finishOrder,
  lineupSeatRows,
  raceSeatRows,
  raceStateLabel,
  readClock
} from '../src/lib/run.js';
import { StartCue } from '../src/lib/startTone.js';
import { heat, race, raceSeat } from './competition.js';

const S = 1_000_000;
const names = buildNames(
  [heat('q', 'Qualifying', ['p1', 'p2'])],
  [
    { id: 'p1', callsign: 'Ace', vtx_types: [] },
    { id: 'p2', callsign: 'Bee', vtx_types: [] }
  ]
);
const lap = (crossing: number, micros: number, restart = false) => ({
  crossing,
  started_at: 0,
  ended_at: micros,
  micros,
  restart
});

describe('readClock', () => {
  it('shows zero when there is no race', () => {
    expect(readClock(undefined, 5 * S)).toEqual({ label: 'Race time', elapsedMs: 0 });
  });

  it('counts down to the tone while staging, and holds at zero after it', () => {
    const staging = race('q', 1, 'Staging', { tone_at: 10 * S });
    expect(readClock(staging, 7 * S)).toEqual({ label: 'Starts in', remainingMs: 3000 });
    expect(readClock(staging, 11 * S)).toEqual({ label: 'Starts in', remainingMs: 0 });
  });

  it('counts up from the start for a race with no time limit', () => {
    const running = race('q', 1, 'Running', { started_at: 10 * S, end: { FirstToLaps: 3 } });
    expect(readClock(running, 12.5 * S)).toEqual({ label: 'Race time', elapsedMs: 2500 });
  });

  it('shows the time left in a timed race, then the grace window', () => {
    const timed = race('q', 1, 'Running', {
      started_at: 0,
      end: { Timed: { micros: 120 * S, grace_micros: 10 * S } }
    });
    expect(readClock(timed, 100 * S)).toEqual({ label: 'Time left', remainingMs: 20_000 });
    expect(readClock(timed, 124 * S)).toEqual({ label: 'Grace', remainingMs: 6000 });
    expect(readClock(timed, 140 * S)).toEqual({ label: 'Grace', remainingMs: 0 });
  });

  it('stands still at the length of a stopped race', () => {
    const stopped = race('q', 1, 'Stopped', { started_at: 10 * S, stopped_at: 55 * S });
    expect(readClock(stopped, 999 * S)).toEqual({ label: 'Race time', elapsedMs: 45_000 });
  });
});

describe('seat rows', () => {
  it('a race row carries callsign, place, counted laps, last and best', () => {
    const running = race('q', 1, 'Running', {
      seats: [
        raceSeat(0, 'p1', {
          finish: 1,
          hole_shot: { crossing: 1, at: 0 },
          laps: [lap(2, 9 * S), lap(3, 0, true), lap(4, 8 * S)],
          best_lap_micros: 8 * S
        }),
        raceSeat(1)
      ]
    });
    const [first, open] = raceSeatRows(running, names);
    expect(first).toMatchObject({
      seatName: 'Seat 1',
      who: 'Ace',
      place: 1,
      lastLapMicros: 8 * S,
      bestLapMicros: 8 * S,
      onCourse: true
    });
    // The marked restart is not a lap.
    expect(first.laps.map((l) => l.micros)).toEqual([9 * S, 8 * S]);
    expect(open).toMatchObject({ seatName: 'Seat 2', who: 'Open seat', onCourse: false });
    expect(open.place).toBeUndefined();
  });

  it('a lineup row names who is on the seat and has nothing flown', () => {
    const phase = heat('f', 'Final', ['p2', { Waiting: { phase: 'q', position: 1 } }]);
    expect(lineupSeatRows(phase, names)).toMatchObject([
      { seatName: 'Seat 1', who: 'Bee', pilot: 'p2', laps: [] },
      { seatName: 'Seat 2', who: 'Winner of Qualifying', laps: [] }
    ]);
  });

  it('a seat this console does not know is named by its place in the lineup', () => {
    const odd = race('q', 1, 'Running', {
      seats: [{ ...raceSeat(0, 'p1'), adapter: 'oft', seat: 'gate-b' }]
    });
    expect(raceSeatRows(odd, names)[0].seatName).toBe('Seat 1');
  });
});

describe('race words', () => {
  it('names each state, and says why a race was discarded', () => {
    expect(raceStateLabel(race('q', 1, 'Staging'))).toBe('Counting down');
    expect(raceStateLabel(race('q', 1, 'Running'))).toBe('Racing');
    expect(raceStateLabel(race('q', 1, 'Stopped'))).toBe('Stopped');
    expect(raceStateLabel(race('q', 1, 'Saved'))).toBe('Saved');
    expect(raceStateLabel(race('q', 1, 'Discarded'))).toBe('Discarded');
    expect(raceStateLabel(race('q', 1, 'Discarded', { discard_reason: 'FalseStart' }))).toBe(
      'Discarded: false start'
    );
    expect(
      raceStateLabel(race('q', 1, 'Discarded', { discard_reason: 'CrashOrObstruction' }))
    ).toBe('Discarded: crash or obstruction');
  });

  it('finishOrder lists placed seats, first place first', () => {
    const saved = race('q', 1, 'Saved', {
      seats: [raceSeat(0, 'p1', { finish: 2 }), raceSeat(1, 'p2', { finish: 1 }), raceSeat(2)]
    });
    expect(finishOrder(saved).map((seat) => seat.pilot)).toEqual(['p2', 'p1']);
  });
});

describe('StartCue', () => {
  function cue() {
    const cancel = vi.fn();
    const schedule = vi.fn((_ms: number) => cancel);
    return { cue: new StartCue(schedule), schedule, cancel };
  }

  it('schedules the tone once per countdown, however often the snapshot changes', () => {
    const { cue: c, schedule } = cue();
    c.update('counting', 10 * S, 7 * S);
    c.update('counting', 10 * S, 8 * S);
    expect(schedule).toHaveBeenCalledTimes(1);
    expect(schedule).toHaveBeenCalledWith(3000);
  });

  it('lets the tone ring out when the race starts', () => {
    const { cue: c, cancel } = cue();
    c.update('counting', 10 * S, 7 * S);
    c.update('started');
    expect(cancel).not.toHaveBeenCalled();
  });

  it('silences the countdown when the race is called off', () => {
    const { cue: c, cancel } = cue();
    c.update('counting', 10 * S, 7 * S);
    c.update('off');
    expect(cancel).toHaveBeenCalledTimes(1);
  });

  it('schedules again for the next race', () => {
    const { cue: c, schedule } = cue();
    c.update('counting', 10 * S, 7 * S);
    c.update('off');
    c.update('counting', 30 * S, 25 * S);
    expect(schedule).toHaveBeenCalledTimes(2);
    expect(schedule).toHaveBeenLastCalledWith(5000);
  });
});
