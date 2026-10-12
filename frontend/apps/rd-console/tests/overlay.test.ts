/**
 * Unit tests for the overlay helpers (`lib/overlay.ts`): the best-consecutive window the race
 * overlay slides over served laps, how it finds a heat's round and scoring window, the per-pilot
 * served figures, the frame order, and the OBS URL. The browser e2e (`e2e/overlay.spec.ts`) proves
 * the page against a real Director; this pins the arithmetic it relies on.
 */
import { describe, expect, it } from 'vitest';
import type { EventMeta, HeatSummary, LapList, RoundStanding } from '@gridfpv/types';
import {
  bestConsecutive,
  competitorLaps,
  consecutiveWindow,
  heatRound,
  overlayOrder,
  overlayUrl,
  servedRoundBestMicros,
  OVERLAY_CATALOG
} from '../src/lib/overlay.js';

describe('bestConsecutive', () => {
  it('is undefined with fewer laps than the window', () => {
    expect(bestConsecutive([], 3)).toBeUndefined();
    expect(bestConsecutive([7_000_000, 7_100_000], 3)).toBeUndefined();
  });

  it('finds the fastest 3-lap window and where it starts', () => {
    // Laps 2–4 (indices 1–3) sum to 20.6 s; every other window is slower.
    const laps = [9_000_000, 7_000_000, 6_800_000, 6_800_000, 7_500_000, 7_100_000];
    expect(bestConsecutive(laps, 3)).toEqual({
      start: 1,
      laps: [7_000_000, 6_800_000, 6_800_000],
      totalMicros: 20_600_000
    });
  });

  it('gives a tie to the earliest window (the engine breaks ties by the earlier completion)', () => {
    const laps = [7_000_000, 7_000_000, 7_000_000, 7_000_000];
    expect(bestConsecutive(laps, 3)?.start).toBe(0);
  });

  it('honours a different window size, and refuses a zero one', () => {
    const laps = [8_000_000, 6_000_000, 7_000_000];
    expect(bestConsecutive(laps, 1)).toEqual({
      start: 1,
      laps: [6_000_000],
      totalMicros: 6_000_000
    });
    expect(bestConsecutive(laps, 2)).toEqual({
      start: 1,
      laps: [6_000_000, 7_000_000],
      totalMicros: 13_000_000
    });
    expect(bestConsecutive(laps, 0)).toBeUndefined();
  });
});

describe('consecutiveWindow / heatRound', () => {
  const EVENT = {
    id: 'e',
    name: 'Club night',
    created_at: 0,
    persistent: true,
    timers: [],
    roster: [],
    classes: [],
    rounds: [
      {
        id: 'r-tt',
        label: 'Time trial',
        format: 'timed_qual',
        win_condition: { BestConsecutive: { n: 3 } }
      },
      { id: 'r-race', label: 'Race', format: 'race', win_condition: { FirstToLaps: { n: 4 } } }
    ]
  } as unknown as EventMeta;
  const HEATS = [
    {
      heat: 'h-1',
      name: 'Time trial Heat 1',
      lineup: ['p1'],
      round: 'r-tt',
      phase: 'Final',
      is_current: false
    },
    {
      heat: 'h-2',
      name: 'Race Heat 1',
      lineup: ['p1', 'p2'],
      round: 'r-race',
      phase: 'Running',
      is_current: true
    },
    { heat: 'free', name: 'free', lineup: ['p1'], phase: 'Scheduled', is_current: false }
  ] as HeatSummary[];

  it('reads the window only off a BestConsecutive condition', () => {
    expect(consecutiveWindow({ BestConsecutive: { n: 3 } })).toBe(3);
    expect(consecutiveWindow('BestLap')).toBeUndefined();
    expect(consecutiveWindow({ Timed: { window_micros: 120_000_000 } })).toBeUndefined();
    expect(consecutiveWindow({ FirstToLaps: { n: 4 } })).toBeUndefined();
    expect(consecutiveWindow(undefined)).toBeUndefined();
  });

  it('finds a heat’s round through its round tag, and nothing for an untagged heat', () => {
    expect(heatRound(EVENT, HEATS, 'h-1')?.id).toBe('r-tt');
    expect(heatRound(EVENT, HEATS, 'h-2')?.id).toBe('r-race');
    expect(heatRound(EVENT, HEATS, 'free')).toBeUndefined();
    expect(heatRound(EVENT, HEATS, 'gone')).toBeUndefined();
    expect(heatRound(undefined, HEATS, 'h-1')).toBeUndefined();
  });
});

describe('competitorLaps / servedRoundBestMicros / overlayOrder', () => {
  const LAPS: LapList = {
    competitors: [
      {
        competitor: { adapter: 'sim', competitor: 'p2' },
        laps: [{ number: 1, duration_micros: 8_000_000, at: 1, start_ref: 0, end_ref: 1 }]
      },
      {
        competitor: { adapter: 'sim', competitor: 'p1' },
        laps: [
          { number: 1, duration_micros: 7_000_000, at: 1, start_ref: 0, end_ref: 1 },
          { number: 2, duration_micros: 6_500_000, at: 2, start_ref: 1, end_ref: 2 }
        ]
      }
    ]
  };

  it('picks one competitor’s served laps, in order, and nothing for a stranger', () => {
    expect(competitorLaps(LAPS, 'p1').map((l) => l.duration_micros)).toEqual([
      7_000_000, 6_500_000
    ]);
    expect(competitorLaps(LAPS, 'p2')).toHaveLength(1);
    expect(competitorLaps(LAPS, 'p9')).toEqual([]);
    expect(competitorLaps(undefined, 'p1')).toEqual([]);
  });

  it('reads the served round figure per pilot: a best window, a best lap, or nothing for laps', () => {
    const standings: RoundStanding[] = [
      {
        competitor: 'p1',
        position: 1,
        best_lap_micros: 6_500_000,
        laps: 8,
        metric: { BestConsecutive: { n: 3, micros: 20_500_000 } }
      },
      {
        competitor: 'p2',
        position: 2,
        best_lap_micros: null,
        laps: 1,
        metric: { BestConsecutive: { n: 3, micros: null } }
      },
      {
        competitor: 'p3',
        position: 3,
        best_lap_micros: 7_100_000,
        laps: 3,
        metric: { BestLap: { micros: 7_100_000 } }
      },
      {
        competitor: 'p4',
        position: 4,
        best_lap_micros: null,
        laps: 5,
        metric: { MostLaps: { laps: 5 } }
      }
    ];
    expect(servedRoundBestMicros(standings, 'p1')).toBe(20_500_000);
    expect(servedRoundBestMicros(standings, 'p2')).toBeUndefined();
    expect(servedRoundBestMicros(standings, 'p3')).toBe(7_100_000);
    expect(servedRoundBestMicros(standings, 'p4')).toBeUndefined();
    expect(servedRoundBestMicros(standings, 'p9')).toBeUndefined();
  });

  it('orders by the first source that names a pilot, appending the rest, without duplicates', () => {
    expect(overlayOrder([['p2', 'p1'], ['p1', 'p3'], undefined, ['p4', 'p2']])).toEqual([
      'p2',
      'p1',
      'p3',
      'p4'
    ]);
    expect(overlayOrder([undefined, undefined])).toEqual([]);
  });
});

describe('overlayUrl / the catalogue', () => {
  it('points the overlay at the console origin + path, on the overlay hash', () => {
    expect(overlayUrl({ origin: 'http://192.168.1.50:8080', pathname: '/' }, 'race')).toBe(
      'http://192.168.1.50:8080/#/overlay/race'
    );
    expect(overlayUrl({ origin: 'http://127.0.0.1:8080', pathname: '/console/' }, 'race')).toBe(
      'http://127.0.0.1:8080/console/#/overlay/race'
    );
  });

  it('carries the light theme in the URL, and leaves the dark default bare', () => {
    const loc = { origin: 'http://127.0.0.1:8080', pathname: '/' };
    expect(overlayUrl(loc, 'race', 'light')).toBe('http://127.0.0.1:8080/#/overlay/race/light');
    expect(overlayUrl(loc, 'race', 'dark')).toBe('http://127.0.0.1:8080/#/overlay/race');
  });

  it('lists the race timer as the one overlay built so far', () => {
    expect(OVERLAY_CATALOG.map((o) => o.name)).toEqual(['race']);
    expect(OVERLAY_CATALOG[0].title).toBe('Race timer');
  });
});
