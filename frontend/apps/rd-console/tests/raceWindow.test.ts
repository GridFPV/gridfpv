import { describe, expect, it } from 'vitest';
import type { RoundDef } from '@gridfpv/types';
import { fixedEndWindowMicros } from '../src/lib/raceWindow.js';

/**
 * The shared fixed-race-end derivation — the single gate for the countdown clocks (HUD + header)
 * and the end-of-race tones. Pins #504: a round's `time_limit_secs` fixes the end for EVERY
 * format, not just open practice — a Time Trial stores its race duration there (Best-of-N only
 * ranks, the limit is what ends the heat), and the backend's time-limit auto-end has always been
 * format-blind. Before the fix a time trial ran with no countdown, pips or buzzer while the
 * server ended it on schedule anyway.
 */
describe('fixedEndWindowMicros', () => {
  const base: RoundDef = {
    id: 'r1',
    label: 'R1',
    classes: ['c1'],
    format: 'timed_qual',
    params: {},
    win_condition: 'BestLap',
    seeding: 'FromRoster',
    channel_mode: 'Static',
    staging_timer_secs: 300,
    start_procedure: { mode: 'randomized-delay', min_delay_ms: 2000, max_delay_ms: 5000 },
    grace_window: { Duration: { micros: 30_000_000 } },
    protest_window: 'Off'
  };

  it('a Time Trial with a race time has a fixed end — the time limit, whatever the format (#504)', () => {
    // The console stores a Best-of-N round's "Race time (seconds)" as `time_limit_secs`.
    const tt: RoundDef = { ...base, time_limit_secs: 120 };
    expect(fixedEndWindowMicros(tt)).toBe(120_000_000);
  });

  it('a practice with a duration keeps its fixed end; without one it has none', () => {
    const practice: RoundDef = {
      ...base,
      format: 'open_practice',
      seeding: { ActiveNodes: { nodes: [0, 1] } },
      time_limit_secs: 1800
    };
    expect(fixedEndWindowMicros(practice)).toBe(1_800_000_000);
    expect(fixedEndWindowMicros({ ...practice, time_limit_secs: undefined })).toBeUndefined();
  });

  it('a Timed round fixes the end on its window; the time limit takes precedence when both exist', () => {
    const timed: RoundDef = {
      ...base,
      format: 'head_to_head',
      channel_mode: 'PerHeat',
      win_condition: { Timed: { window_micros: 90_000_000 } }
    };
    expect(fixedEndWindowMicros(timed)).toBe(90_000_000);
    // Both set (raw-API reachable): the driver's unconditional time-limit branch is checked
    // first, so the derivation mirrors that precedence.
    expect(fixedEndWindowMicros({ ...timed, time_limit_secs: 60 })).toBe(60_000_000);
  });

  it('no limit and no Timed window ⇒ no fixed end (first-to-N; a limitless practice)', () => {
    const firstTo: RoundDef = {
      ...base,
      format: 'head_to_head',
      channel_mode: 'PerHeat',
      win_condition: { FirstToLaps: { n: 3 } }
    };
    expect(fixedEndWindowMicros(firstTo)).toBeUndefined();
    expect(fixedEndWindowMicros(undefined)).toBeUndefined();
  });
});
