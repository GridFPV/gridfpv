/**
 * The overlay's pure helpers (`lib/overlay.ts`): which race it shows and for how long, the
 * word for a race's state, the best run of N laps in a row, the rows, the clock, and the URL.
 */
import { describe, expect, it } from 'vitest';
import type { Format, LapView } from '@gridfpv/types';
import { buildNames } from '../src/lib/names.js';
import {
  OVERLAY_CATALOG,
  RESULT_LINGER_MICROS,
  bestLapRun,
  bestRunInHeat,
  consecutiveLaps,
  inRun,
  lastResult,
  overlayRows,
  overlayStateWord,
  overlaySubject,
  overlayUrl
} from '../src/lib/overlay.js';
import { readClock } from '../src/lib/run.js';
import { eventView, FIRST_TO_3, heat, race, raceSeat, timerView } from './competition.js';

const S = 1_000_000;
const lap = (
  crossing: number,
  seconds: number,
  kind: 'lap' | 'restart' | 'timer' = 'lap'
): LapView => ({
  crossing,
  started_at: 0,
  ended_at: 0,
  micros: kind === 'lap' ? seconds * S : 0,
  restart: kind === 'restart',
  timer_restarted: kind === 'timer'
});
const BEST_3: Format = {
  name: 'Best 3 consecutive',
  race: { end: 'Open', staging_micros: 5 * S, first_lap: 'HoleShot' },
  completion: 'DirectorDecides',
  ranking: { sort: [{ BestConsecutive: 3 }] }
};

describe('overlayUrl', () => {
  const loc = { origin: 'http://192.168.1.20:8080', pathname: '/' };

  it('is the console address with the overlay hash, light text by default', () => {
    expect(overlayUrl(loc, 'race')).toBe('http://192.168.1.20:8080/#/overlay/race');
    expect(overlayUrl(loc, 'race', 'dark')).toBe('http://192.168.1.20:8080/#/overlay/race');
  });

  it('carries the light panel in the hash', () => {
    expect(overlayUrl(loc, 'race', 'light')).toBe('http://192.168.1.20:8080/#/overlay/race/light');
  });

  it('keeps a path the console is served under', () => {
    expect(overlayUrl({ origin: 'http://grid.local', pathname: '/console/' }, 'race')).toBe(
      'http://grid.local/console/#/overlay/race'
    );
  });

  it('the catalogue lists the race overlay with words for the card', () => {
    expect(OVERLAY_CATALOG.map((info) => info.name)).toEqual(['race']);
    expect(OVERLAY_CATALOG[0].description.length).toBeGreaterThan(20);
  });
});

describe('which race the overlay shows', () => {
  const phases = [heat('q1', 'Heat 1', ['p1']), heat('q2', 'Heat 2', ['p2'])];

  it('shows nothing before a snapshot, and nothing for an event with no races', () => {
    expect(overlaySubject(undefined, 10 * S)).toBeUndefined();
    expect(overlaySubject(eventView({ phases }), 10 * S)).toBeUndefined();
  });

  it('shows the current race in every state it can be in', () => {
    for (const state of ['Staging', 'Running', 'Stopped'] as const) {
      const now = race('q2', 1, state);
      const view = eventView({
        phases,
        races: [race('q1', 1, 'Saved', { stopped_at: 1 * S }), now],
        current_race: now.race
      });
      expect(overlaySubject(view, 500 * S)).toEqual({ race: now, current: true });
    }
  });

  it('the last result is the newest saved race of the heat raced last', () => {
    const first = race('q1', 1, 'Saved');
    const second = race('q2', 1, 'Saved');
    const third = race('q2', 2, 'Saved');
    expect(lastResult(eventView({ phases, races: [first, second, third] }))).toBe(third);
    // A discarded race is passed over for the saved one before it in the same heat.
    const discarded = race('q2', 3, 'Discarded');
    expect(lastResult(eventView({ phases, races: [first, second, third, discarded] }))).toBe(third);
    // The heat raced last has nothing saved: an older heat's result is not brought back.
    expect(
      lastResult(eventView({ phases, races: [first, race('q2', 1, 'Discarded')] }))
    ).toBeUndefined();
    expect(lastResult(eventView({ phases }))).toBeUndefined();
  });

  it('keeps the result up for a minute after it saw the race decided, then goes quiet', () => {
    const saved = race('q1', 1, 'Saved', { started_at: 10 * S, stopped_at: 40 * S });
    const view = eventView({ phases, races: [saved] });
    // Saved long after it stopped: the minute counts from when the overlay saw it decided.
    const decidedAt = 900 * S;
    expect(overlaySubject(view, decidedAt + 1, decidedAt)).toEqual({ race: saved, current: false });
    expect(overlaySubject(view, decidedAt + RESULT_LINGER_MICROS - 1, decidedAt)).toEqual({
      race: saved,
      current: false
    });
    expect(overlaySubject(view, decidedAt + RESULT_LINGER_MICROS, decidedAt)).toBeUndefined();
  });

  it('an overlay opened later counts from when the race stopped', () => {
    const saved = race('q1', 1, 'Saved', { started_at: 10 * S, stopped_at: 40 * S });
    const view = eventView({ phases, races: [saved] });
    expect(overlaySubject(view, 45 * S)).toEqual({ race: saved, current: false });
    expect(overlaySubject(view, 40 * S + RESULT_LINGER_MICROS)).toBeUndefined();
    // A result with no stop time and nothing seen is not shown at all.
    const bare = eventView({ phases, races: [race('q1', 1, 'Saved')] });
    expect(overlaySubject(bare, 45 * S)).toBeUndefined();
  });

  it('has one word for each state', () => {
    expect(overlayStateWord(race('q1', 1, 'Staging'))).toBe('Starting');
    expect(overlayStateWord(race('q1', 1, 'Running'))).toBe('Live');
    expect(overlayStateWord(race('q1', 1, 'Stopped'))).toBe('Finished');
    expect(overlayStateWord(race('q1', 1, 'Saved'))).toBe('Result');
  });
});

describe('the overlay clock', () => {
  it('counts down to the tone, up while running, down for a timed race, and stands still after', () => {
    expect(readClock(race('q', 1, 'Staging', { tone_at: 5 * S }), 2 * S)).toEqual({
      label: 'Starts in',
      remainingMs: 3000
    });
    expect(readClock(race('q', 1, 'Running', { started_at: 5 * S, end: 'Open' }), 12 * S)).toEqual({
      label: 'Race time',
      elapsedMs: 7000
    });
    const timed = { Timed: { micros: 120 * S, grace_micros: 10 * S } };
    expect(readClock(race('q', 1, 'Running', { started_at: 5 * S, end: timed }), 25 * S)).toEqual({
      label: 'Time left',
      remainingMs: 100_000
    });
    // A stopped race and the saved result it becomes read the same, whatever the time is.
    for (const state of ['Stopped', 'Saved'] as const) {
      const done = race('q', 1, state, { started_at: 5 * S, stopped_at: 65 * S });
      expect(readClock(done, 999 * S)).toEqual({ label: 'Race time', elapsedMs: 60_000 });
    }
  });
});

describe('best N laps in a row', () => {
  it('reads N from a ranking rule that sorts by best consecutive first', () => {
    expect(consecutiveLaps(BEST_3)).toBe(3);
    expect(consecutiveLaps(FIRST_TO_3)).toBeUndefined();
    expect(consecutiveLaps(undefined)).toBeUndefined();
    expect(consecutiveLaps({ ...BEST_3, ranking: undefined })).toBeUndefined();
    // Only the first measure decides what the column shows.
    expect(
      consecutiveLaps({ ...BEST_3, ranking: { sort: ['Laps', { BestConsecutive: 3 }] } })
    ).toBeUndefined();
    expect(
      consecutiveLaps({ ...BEST_3, ranking: { sort: [{ BestConsecutive: 0 }] } })
    ).toBeUndefined();
  });

  it('finds the fastest window and where it starts', () => {
    const laps = [lap(1, 10), lap(2, 9), lap(3, 8), lap(4, 12)];
    expect(bestLapRun(laps, 3)).toEqual({ start: 0, length: 3, totalMicros: 27 * S });
    expect(bestLapRun(laps, 2)).toEqual({ start: 1, length: 2, totalMicros: 17 * S });
    expect(bestLapRun(laps, 1)).toEqual({ start: 2, length: 1, totalMicros: 8 * S });
  });

  it('gives nothing with too few laps, or a silly N', () => {
    expect(bestLapRun([lap(1, 10), lap(2, 9)], 3)).toBeUndefined();
    expect(bestLapRun([], 1)).toBeUndefined();
    expect(bestLapRun([lap(1, 10)], 0)).toBeUndefined();
    expect(bestLapRun([lap(1, 10)], -2)).toBeUndefined();
    expect(bestLapRun([lap(1, 10)], 1.5)).toBeUndefined();
  });

  it('gives the earlier of two equal runs', () => {
    const laps = [lap(1, 9), lap(2, 9), lap(3, 9), lap(4, 9)];
    expect(bestLapRun(laps, 2)?.start).toBe(0);
  });

  it('never joins laps across a marked restart', () => {
    // 8 + 7 would be the best pair, but a restart sits between them.
    const laps = [lap(1, 12), lap(2, 8), lap(3, 0, 'restart'), lap(4, 7), lap(5, 11)];
    expect(bestLapRun(laps, 2)).toEqual({ start: 2, length: 2, totalMicros: 18 * S });
    // Each stretch has only two laps, so there is no run of three at all.
    expect(bestLapRun(laps, 3)).toBeUndefined();
  });

  it('never joins laps across a timer restart', () => {
    const laps = [lap(1, 8), lap(2, 0, 'timer'), lap(3, 7), lap(4, 20), lap(5, 20)];
    expect(bestLapRun(laps, 2)).toEqual({ start: 1, length: 2, totalMicros: 27 * S });
  });

  it('start counts only laps that count, so it lines up with the rows', () => {
    const laps = [lap(1, 30), lap(2, 0, 'restart'), lap(3, 9), lap(4, 8)];
    const run = bestLapRun(laps, 2);
    expect(run).toEqual({ start: 1, length: 2, totalMicros: 17 * S });
    expect([0, 1, 2].map((index) => inRun(index, run))).toEqual([false, true, true]);
    expect(inRun(0, undefined)).toBe(false);
  });

  it("a pilot's best run in a heat reads its saved races and the one on screen, and no others", () => {
    const seat = (laps: LapView[]) => [raceSeat(0, 'p1', { laps })];
    const saved = race('solo', 1, 'Saved', { seats: seat([lap(1, 10), lap(2, 10), lap(3, 10)]) });
    const thrown = race('solo', 2, 'Discarded', { seats: seat([lap(1, 5), lap(2, 5), lap(3, 5)]) });
    const running = race('solo', 3, 'Running', { seats: seat([lap(1, 9), lap(2, 9), lap(3, 9)]) });
    const other = race('else', 1, 'Saved', { seats: seat([lap(1, 1), lap(2, 1), lap(3, 1)]) });
    const view = eventView({ races: [saved, thrown, running, other] });
    expect(bestRunInHeat(view, 'solo', 'p1', 3)).toBe(30 * S);
    expect(bestRunInHeat(view, 'solo', 'p1', 3, running)).toBe(27 * S);
    expect(bestRunInHeat(view, 'solo', 'p2', 3, running)).toBeUndefined();
    // Laps from two races are never joined: no race holds a run of four.
    expect(bestRunInHeat(view, 'solo', 'p1', 4, running)).toBeUndefined();
  });
});

describe('overlayRows', () => {
  const pilots = [
    { id: 'p-1a', callsign: 'Ace', vtx_types: [] },
    { id: 'p-2b', callsign: 'Bee', vtx_types: [] }
  ];
  const names = buildNames([heat('q', 'Qualifying', ['p-1a', 'p-2b'])], pilots, [
    timerView('gate-x1y2', 'Gate', 4)
  ]);

  it('gives one row per seat in lineup order with the numbers so far', () => {
    const running = race('q', 1, 'Running', {
      seats: [
        raceSeat(0, 'p-2b', { finish: 2, laps: [lap(1, 11)], best_lap_micros: 11 * S }),
        raceSeat(1, 'p-1a', {
          finish: 1,
          laps: [lap(2, 9.5), lap(3, 0, 'restart'), lap(4, 8.25)],
          best_lap_micros: 8.25 * S
        })
      ]
    });
    const rows = overlayRows(running, names);
    expect(rows.map((row) => row.name)).toEqual(['Bee', 'Ace']);
    expect(rows[1]).toMatchObject({
      place: 1,
      laps: 2,
      lapMicros: [9.5 * S, 8.25 * S],
      lastLapMicros: 8.25 * S,
      bestLapMicros: 8.25 * S
    });
    expect(rows[0]).toMatchObject({ place: 2, laps: 1, lastLapMicros: 11 * S });
    // No run is worked out unless the heat is ranked on one.
    expect(rows[1].bestRun).toBeUndefined();
    expect(new Set(rows.map((row) => row.key)).size).toBe(2);
  });

  it('a staging race has every seat with nothing flown', () => {
    const staging = race('q', 1, 'Staging', { seats: [raceSeat(0, 'p-1a'), raceSeat(1, 'p-2b')] });
    expect(overlayRows(staging, names)).toMatchObject([
      { name: 'Ace', laps: 0, lapMicros: [] },
      { name: 'Bee', laps: 0, lapMicros: [] }
    ]);
    expect(overlayRows(staging, names)[0].place).toBeUndefined();
    expect(overlayRows(staging, names)[0].lastLapMicros).toBeUndefined();
  });

  it('an open seat is named by its seat, and a pilot gone from the list is not shown by id', () => {
    const open = race('q', 1, 'Running', {
      seats: [
        raceSeat(0, undefined, { adapter: 'gate-x1y2', seat: '2' }),
        raceSeat(1, 'p-9z', { adapter: 'gate-x1y2', seat: '3' })
      ]
    });
    expect(overlayRows(open, names).map((row) => row.name)).toEqual(['Seat 3', 'Unknown pilot']);
  });

  it('works out the best run for a heat ranked on one, never across a restart', () => {
    const running = race('q', 1, 'Running', {
      seats: [
        raceSeat(0, 'p-1a', {
          laps: [lap(1, 12), lap(2, 8), lap(3, 0, 'restart'), lap(4, 7), lap(5, 11), lap(6, 10)]
        }),
        raceSeat(1, 'p-2b', { laps: [lap(7, 9)] })
      ]
    });
    const rows = overlayRows(running, names, 2);
    expect(rows[0].bestRun).toEqual({ start: 2, length: 2, totalMicros: 18 * S });
    expect(rows[1].bestRun).toBeUndefined();
  });
});
