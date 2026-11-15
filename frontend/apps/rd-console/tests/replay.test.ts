/**
 * Replay (`lib/replay.ts`): a finished race as it stood at a moment of its recording.
 */
import { describe, expect, it } from 'vitest';
import type { LapView, RaceView } from '@gridfpv/types';
import { LEAD_MICROS, firstFrameAt, raceAt, raceStart, replayAt } from '../src/lib/replay.js';
import { TWO_MINUTE_QUALI, eventView, heat, race, raceSeat } from './competition.js';

const S = 1_000_000;
/** The race's start on the Director's clock. */
const START = 1_000 * S;
/** The same moment on the timer's clock, which counts from when the timer was switched on. */
const TIMER_START = 50 * S;
const HEAT = 'quali-x9z1';

/** A lap from `from` to `to` seconds into the race, on the timer's clock. */
const lap = (crossing: number, from: number, to: number, over: Partial<LapView> = {}): LapView => ({
  crossing,
  started_at: TIMER_START + from * S,
  ended_at: TIMER_START + to * S,
  micros: (to - from) * S,
  restart: false,
  timer_restarted: false,
  ...over
});

/** Two pilots. Ace: hole shot at 1 s, laps ending at 11, 20, 32. Bee: hole shot at 2, laps at 14, 25. */
const finished = (over: Partial<RaceView> = {}): RaceView =>
  race(HEAT, 2, 'Saved', {
    tone_at: START,
    started_at: START,
    stopped_at: START + 40 * S,
    seats: [
      raceSeat(0, 'p-1a', {
        adapter: 'gate-1',
        start_at: TIMER_START,
        hole_shot: { crossing: 10, at: TIMER_START + 1 * S },
        laps: [lap(11, 1, 11), lap(12, 11, 20), lap(13, 20, 32)],
        finish: 2,
        best_lap_micros: 9 * S,
        total_micros: 31 * S
      }),
      raceSeat(1, 'p-2b', {
        adapter: 'gate-1',
        start_at: TIMER_START,
        hole_shot: { crossing: 20, at: TIMER_START + 2 * S },
        laps: [lap(21, 2, 14), lap(22, 14, 25)],
        // The race's own final order, which the laps alone would not give.
        finish: 1,
        best_lap_micros: 11 * S,
        total_micros: 23 * S
      })
    ],
    ...over
  });

const counts = (shown: RaceView) => shown.seats.map((seat) => seat.laps.length);
const places = (shown: RaceView) => shown.seats.map((seat) => seat.finish);

describe('raceAt', () => {
  it('counts down before the start, with nothing flown', () => {
    const shown = raceAt(finished(), START - 3 * S);
    expect(shown.state).toBe('Staging');
    expect(shown.tone_at).toBe(START);
    expect(shown.started_at).toBeUndefined();
    expect(counts(shown)).toEqual([0, 0]);
    expect(shown.seats[0].hole_shot).toBeUndefined();
    expect(shown.seats[0].best_lap_micros).toBeUndefined();
    expect(places(shown)).toEqual([undefined, undefined]);
  });

  it("runs with only the laps that had ended by then, placed by the timer's own clock", () => {
    // 0.5 s in: nobody has crossed yet.
    let shown = raceAt(finished(), START + 0.5 * S);
    expect(shown.state).toBe('Running');
    expect(shown.stopped_at).toBeUndefined();
    expect(shown.seats[0].hole_shot).toBeUndefined();
    // 1.5 s in: Ace is on course, Bee is not.
    shown = raceAt(finished(), START + 1.5 * S);
    expect(shown.seats[0].hole_shot).toBeDefined();
    expect(shown.seats[1].hole_shot).toBeUndefined();
    expect(counts(shown)).toEqual([0, 0]);
    // Exactly as a lap ends, it is there.
    expect(counts(raceAt(finished(), START + 11 * S))).toEqual([1, 0]);
    expect(counts(raceAt(finished(), START + 11 * S - 1))).toEqual([0, 0]);
    shown = raceAt(finished(), START + 21 * S);
    expect(counts(shown)).toEqual([2, 1]);
    expect(shown.seats[0].best_lap_micros).toBe(9 * S);
    expect(shown.seats[1].best_lap_micros).toBe(12 * S);
    expect(shown.seats[0].total_micros).toBeUndefined();
  });

  it('places the pilots on laps, then on who got there first, while it runs', () => {
    // 12 s in: Ace has a lap, Bee none.
    expect(places(raceAt(finished(), START + 12 * S))).toEqual([1, 2]);
    // 15 s in: one lap each. Ace finished the lap first.
    expect(places(raceAt(finished(), START + 15 * S))).toEqual([1, 2]);
    // 26 s in: two laps each. Ace got to two first (20 s against 25 s).
    expect(places(raceAt(finished(), START + 26 * S))).toEqual([1, 2]);
    // An open seat takes no place.
    const open = finished();
    open.seats[0] = { ...open.seats[0], pilot: undefined };
    expect(places(raceAt(open, START + 26 * S))).toEqual([undefined, 1]);
  });

  it('places a heat ranked on best N in a row by the best run so far', () => {
    // 26 s in, on best 2 in a row: Ace 10 + 9 = 19 s, Bee 12 + 11 = 23 s.
    expect(places(raceAt(finished(), START + 26 * S, 2))).toEqual([1, 2]);
    // 15 s in nobody has two laps yet: laps, then time.
    expect(places(raceAt(finished(), START + 15 * S, 2))).toEqual([1, 2]);
    // Bee flies two quick laps: the better run leads, whoever has more laps.
    const quick = finished();
    quick.seats[1] = { ...quick.seats[1], laps: [lap(21, 2, 8), lap(22, 8, 13)] };
    expect(places(raceAt(quick, START + 21 * S, 2))).toEqual([2, 1]);
  });

  it('from the stop on it is the race as it now stands, its own places and all', () => {
    const shown = raceAt(finished(), START + 40 * S);
    expect(shown.state).toBe('Stopped');
    expect(counts(shown)).toEqual([3, 2]);
    expect(places(shown)).toEqual([2, 1]);
    expect(shown.seats[0].total_micros).toBe(31 * S);
    expect(raceAt(finished(), START + 44 * S)).toEqual(shown);
  });

  it('leaves out a marked restart that had not happened yet, and never counts one as a lap', () => {
    const marked = finished();
    marked.seats[0] = {
      ...marked.seats[0],
      laps: [lap(11, 1, 11), lap(12, 11, 20, { restart: true, micros: 0 }), lap(13, 20, 32)]
    };
    const shown = raceAt(marked, START + 21 * S);
    expect(shown.seats[0].laps.map((l) => l.crossing)).toEqual([11, 12]);
    expect(shown.seats[0].best_lap_micros).toBe(10 * S);
    // One counted lap each: Ace's ended first.
    expect(places(shown)).toEqual([1, 2]);
  });

  it("places a hand-timed seat on the Director's clock, which is the clock it was timed on", () => {
    // No start on a timer's clock, and crossings stamped with the Director's own time.
    const hand = (crossing: number, from: number, to: number): LapView => ({
      ...lap(crossing, from, to),
      started_at: START + from * S,
      ended_at: START + to * S
    });
    const bench = finished();
    bench.seats = [
      raceSeat(0, 'p-1a', {
        hole_shot: { crossing: 10, at: START + 3 * S },
        laps: [hand(11, 3, 13), hand(12, 13, 22)]
      })
    ];
    // The hole shot was 3 s in, so lap 1 ends 13 s in, not 10.
    expect(counts(raceAt(bench, START + 12 * S))).toEqual([0]);
    expect(counts(raceAt(bench, START + 13 * S))).toEqual([1]);
    expect(raceAt(bench, START + 2 * S).seats[0].hole_shot).toBeUndefined();
  });

  it("lays laps end to end from the start when a seat's clock cannot be placed", () => {
    // No start on the timer's clock, and times that are not the Director's.
    const lost = finished();
    lost.seats = [raceSeat(0, 'p-1a', { laps: [lap(11, 1, 11), lap(12, 11, 20)] })];
    expect(counts(raceAt(lost, START + 9 * S))).toEqual([0]);
    expect(counts(raceAt(lost, START + 10 * S))).toEqual([1]);
    expect(counts(raceAt(lost, START + 19 * S))).toEqual([2]);
  });

  it('hands back a race that never started as it is', () => {
    const never = race(HEAT, 1, 'Discarded', { seats: [raceSeat(0, 'p-1a')] });
    expect(raceStart(never)).toBeUndefined();
    expect(firstFrameAt(never)).toBeUndefined();
    expect(raceAt(never, START)).toBe(never);
  });
});

describe('firstFrameAt', () => {
  it("is the recording's first frame, or a short lead before the start without one", () => {
    expect(firstFrameAt(finished())).toBe(START - LEAD_MICROS);
    const filmed = finished({
      video: { file: 'a.mkv', first_frame_at: START - 7 * S, clean: true, overlay: 'Missing' }
    });
    expect(firstFrameAt(filmed)).toBe(START - 7 * S);
    // A race started with no countdown has no tone: the start still places it.
    expect(raceStart(finished({ tone_at: undefined }))).toBe(START);
  });
});

describe('replayAt', () => {
  const view = () =>
    eventView({
      phases: [heat(HEAT, 'Qualifying Heat 1', ['p-1a', 'p-2b'], { format: TWO_MINUTE_QUALI })],
      races: [race(HEAT, 1, 'Saved'), finished(), race(HEAT, 3, 'Saved')]
    });

  it('cuts the race back and leaves out the races flown after it', () => {
    const shown = replayAt(view(), `${HEAT}#2`, START + 21 * S);
    expect(shown?.race.state).toBe('Running');
    expect(shown?.view.races.map((r) => r.race)).toEqual([`${HEAT}#1`, `${HEAT}#2`]);
    expect(shown?.view.races[1]).toBe(shown?.race);
  });

  it("uses the heat's own ranking for the running order", () => {
    // Best 3 in a row. Ace flies four 8 s laps (best three: 24 s); Bee three 7 s laps (21 s).
    const ranked = view();
    const whole = ranked.races[1];
    whole.seats[0] = {
      ...whole.seats[0],
      laps: [lap(11, 1, 9), lap(12, 9, 17), lap(13, 17, 25), lap(14, 25, 33)]
    };
    whole.seats[1] = { ...whole.seats[1], laps: [lap(21, 2, 9), lap(22, 9, 16), lap(23, 16, 23)] };
    // 34 s in Ace has more laps, but the heat is ranked on the run, and Bee's is quicker.
    expect(places(replayAt(ranked, `${HEAT}#2`, START + 34 * S)!.race)).toEqual([2, 1]);
    // Without the format it would go on laps.
    expect(places(raceAt(whole, START + 34 * S))).toEqual([1, 2]);
  });

  it('is undefined for a race the event does not have', () => {
    expect(replayAt(view(), 'nope#1', START)).toBeUndefined();
  });
});
