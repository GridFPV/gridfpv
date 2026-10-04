/**
 * Unit tests for the RaceGOW helpers (`lib/racegow.ts`): the best-consecutive window the screens
 * slide over served laps, the preset's round shape, the create orchestration's command order, and
 * the submission line. The browser e2e (`e2e/racegow.spec.ts`) proves the whole loop against a
 * real Director; this pins the arithmetic and the composition it relies on.
 */
import { describe, expect, it, vi } from 'vitest';
import type { EventMeta, LapList, RoundStanding } from '@gridfpv/types';
import {
  bestConsecutive,
  createRaceGowRun,
  isRaceGowEvent,
  overlayUrl,
  raceGowRound,
  raceGowRoundRequest,
  runLaps,
  servedBestMicros,
  submissionText,
  RACEGOW_CLASS_ID,
  RACEGOW_PRESET
} from '../src/lib/racegow.js';
import type { Session } from '../src/lib/session.svelte.js';

describe('bestConsecutive', () => {
  it('is undefined with fewer laps than the window', () => {
    expect(bestConsecutive([], 3)).toBeUndefined();
    expect(bestConsecutive([7_000_000, 7_100_000], 3)).toBeUndefined();
  });

  it('finds the fastest 3-lap window and where it starts', () => {
    // Laps 2–4 (indices 1–3) sum to 20.6 s; every other window is slower.
    const laps = [9_000_000, 7_000_000, 6_800_000, 6_800_000, 7_500_000, 7_100_000];
    expect(bestConsecutive(laps)).toEqual({
      start: 1,
      laps: [7_000_000, 6_800_000, 6_800_000],
      totalMicros: 20_600_000
    });
  });

  it('gives a tie to the earliest window (the engine breaks ties by the earlier completion)', () => {
    const laps = [5_000_000, 5_000_000, 5_000_000, 5_000_000];
    expect(bestConsecutive(laps)?.start).toBe(0);
  });

  it('honours a different window size, and refuses a zero one', () => {
    expect(bestConsecutive([3_000_000, 1_000_000, 2_000_000], 1)).toEqual({
      start: 1,
      laps: [1_000_000],
      totalMicros: 1_000_000
    });
    expect(bestConsecutive([1, 2, 3], 0)).toBeUndefined();
  });
});

describe('runLaps / servedBestMicros', () => {
  it('flattens the lap list in competitor order (a solo run has one)', () => {
    const list: LapList = {
      competitors: [
        {
          competitor: { adapter: 'rotorhazard', competitor: 'p1' },
          laps: [
            { number: 1, duration_micros: 7_000_000, at: 1, start_ref: 0, end_ref: 1 },
            { number: 2, duration_micros: 6_900_000, at: 2, start_ref: 1, end_ref: 2 }
          ]
        }
      ]
    };
    expect(runLaps(list).map((l) => l.duration_micros)).toEqual([7_000_000, 6_900_000]);
    expect(runLaps(undefined)).toEqual([]);
  });

  it('reads the served best-consecutive window off the standings, or nothing', () => {
    const rows: RoundStanding[] = [
      {
        competitor: 'p1',
        position: 1,
        best_lap_micros: 6_800_000,
        laps: 6,
        metric: { BestConsecutive: { n: 3, micros: 20_600_000 } }
      }
    ];
    expect(servedBestMicros(rows)).toBe(20_600_000);
    expect(
      servedBestMicros([{ ...rows[0], metric: { BestConsecutive: { n: 3, micros: null } } }])
    ).toBeUndefined();
    expect(servedBestMicros([{ ...rows[0], metric: { BestLap: { micros: 1 } } }])).toBeUndefined();
    expect(servedBestMicros([])).toBeUndefined();
  });
});

describe('the preset shape', () => {
  it('marks and finds a RaceGOW event by its preset', () => {
    const plain = { id: 'e1', name: 'Club night', preset: undefined } as unknown as EventMeta;
    const run = { id: 'e2', name: 'Track 1', preset: RACEGOW_PRESET } as unknown as EventMeta;
    expect(isRaceGowEvent(plain)).toBe(false);
    expect(isRaceGowEvent(run)).toBe(true);
    expect(isRaceGowEvent(undefined)).toBe(false);
  });

  it('scores best 3 consecutive laps, open-ended, on the Whoop class, under a 10-minute ceiling', () => {
    const req = raceGowRoundRequest();
    expect(req.format).toBe('timed_qual');
    expect(req.params).toEqual({ rounds: '0' });
    expect(req.win_condition).toEqual({ BestConsecutive: { n: 3 } });
    expect(req.classes).toEqual([RACEGOW_CLASS_ID]);
    expect(req.seeding).toBe('FromRoster');
    expect(req.channel_mode).toBe('Static');
    // Ranking-only conditions never end a heat, so the Director requires a race time; this one is
    // a ceiling far past a whoop pack, not a race window.
    expect(req.time_limit_secs).toBe(600);
    // A fixed one-second hold: no field of pilots is waiting on a random tone.
    expect(req.start_procedure).toEqual({
      mode: 'randomized-delay',
      min_delay_ms: 1000,
      max_delay_ms: 1000
    });
  });

  it('finds the scoring round by format, not by label', () => {
    const meta = {
      rounds: [
        { id: 'r0', label: 'Warm-up', format: 'open_practice' },
        { id: 'r1', label: 'Renamed by hand', format: 'timed_qual' }
      ]
    } as unknown as EventMeta;
    expect(raceGowRound(meta)?.id).toBe('r1');
    expect(raceGowRound(undefined)).toBeUndefined();
  });
});

describe('createRaceGowRun', () => {
  function fakeSession() {
    const calls: string[] = [];
    const meta = { id: 'e1', name: 'RaceGOW6 Track 1', preset: RACEGOW_PRESET } as EventMeta;
    const session = {
      currentEvent: meta,
      createEventAndEnter: vi.fn(async (name: string, fields?: unknown) => {
        calls.push(`create:${name}:${JSON.stringify(fields)}`);
        return meta;
      }),
      setEventTimers: vi.fn(async (ids: string[]) => (calls.push(`timers:${ids}`), meta)),
      setPrimaryTimer: vi.fn(async (id: string) => (calls.push(`primary:${id}`), meta)),
      setEventClasses: vi.fn(async (ids: string[]) => (calls.push(`classes:${ids}`), meta)),
      setEventRoster: vi.fn(async (ids: string[]) => (calls.push(`roster:${ids}`), meta)),
      setClassMembership: vi.fn(async (cls: string, slots: unknown[]) => {
        calls.push(`members:${cls}:${JSON.stringify(slots)}`);
        return meta;
      }),
      createRound: vi.fn(async (req: { format: string }) => {
        calls.push(`round:${req.format}`);
        return { id: 'r1' };
      })
    };
    return { calls, session: session as unknown as Session, meta };
  }

  it('creates, then configures timer → primary → class → roster → channel slot → round', async () => {
    const { calls, session, meta } = fakeSession();
    const result = await createRaceGowRun(session, {
      track: '  RaceGOW6 Track 1 ',
      pilot: 'p1',
      timer: 'rh-1',
      channel: 5880
    });
    expect(result).toBe(meta);
    expect(calls).toEqual([
      'create:RaceGOW6 Track 1:{"preset":"racegow"}',
      'timers:rh-1',
      'primary:rh-1',
      `classes:${RACEGOW_CLASS_ID}`,
      'roster:p1',
      `members:${RACEGOW_CLASS_ID}:[{"pilot":"p1","channel":5880}]`,
      'round:timed_qual'
    ]);
  });

  it('seats the pilot without a channel when the timer offers none', async () => {
    const { calls, session } = fakeSession();
    await createRaceGowRun(session, { track: 'T', pilot: 'p1', timer: 'mock' });
    expect(calls).toContain(`members:${RACEGOW_CLASS_ID}:[{"pilot":"p1"}]`);
  });

  it('stops at a cancelled token prompt and reports it as undefined', async () => {
    const { calls, session } = fakeSession();
    (session.setEventRoster as unknown as ReturnType<typeof vi.fn>).mockResolvedValueOnce(
      undefined
    );
    const result = await createRaceGowRun(session, { track: 'T', pilot: 'p1', timer: 'rh-1' });
    expect(result).toBeUndefined();
    expect(calls.some((c) => c.startsWith('members:'))).toBe(false);
    expect(calls.some((c) => c.startsWith('round:'))).toBe(false);
  });
});

describe('submissionText / overlayUrl', () => {
  it('writes the time, the three laps, which run and when, and what timed it', () => {
    const text = submissionText({
      track: 'RaceGOW6 Track 1',
      callsign: 'Recon',
      best: { start: 2, laps: [7_012_000, 7_201_000, 7_224_000], totalMicros: 21_437_000 },
      run: 3,
      runs: 5,
      flownAt: new Date(2026, 8, 26, 14, 3),
      timedBy: 'Garage RH (RotorHazard)'
    });
    expect(text).toContain('RaceGOW6 Track 1 — Recon');
    expect(text).toContain('Best 3 consecutive laps: 21.437 s (7.012 / 7.201 / 7.224)');
    expect(text).toContain('Run 3 of 5, laps 3–5');
    expect(text).toContain('Timed by GridFPV on Garage RH (RotorHazard)');
  });

  it('points the overlay at the console origin + path, on the overlay hash', () => {
    expect(overlayUrl({ origin: 'http://192.168.1.20:8080', pathname: '/' })).toBe(
      'http://192.168.1.20:8080/#/overlay/racegow'
    );
  });

  it('carries the light theme in the overlay URL, and leaves the dark default bare', () => {
    const loc = { origin: 'http://192.168.1.20:8080', pathname: '/' };
    expect(overlayUrl(loc, 'light')).toBe('http://192.168.1.20:8080/#/overlay/racegow/light');
    expect(overlayUrl(loc, 'dark')).toBe('http://192.168.1.20:8080/#/overlay/racegow');
  });
});
