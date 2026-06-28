import { describe, expect, it } from 'vitest';
import type { RoundDef, WinCondition } from '@gridfpv/types';
import { advanceRoundLabel, advanceRoundReq, bracketTopNDefault } from '../src/lib/standings.js';

/** The bracket's chosen win condition (one for all heats) — head-to-head default First-to-N. */
const WIN: WinCondition = { FirstToLaps: { n: 3 } };

describe('bracketTopNDefault — largest power-of-two ≤ field size', () => {
  it('returns the field exactly when it is already a power of two', () => {
    expect(bracketTopNDefault(2)).toBe(2);
    expect(bracketTopNDefault(4)).toBe(4);
    expect(bracketTopNDefault(8)).toBe(8);
    expect(bracketTopNDefault(16)).toBe(16);
  });

  it('floors down to the largest power-of-two below an off-size field', () => {
    expect(bracketTopNDefault(3)).toBe(2);
    expect(bracketTopNDefault(6)).toBe(4);
    expect(bracketTopNDefault(9)).toBe(8); // a 9-up qualifier cuts to a clean 8-seed bracket
    expect(bracketTopNDefault(15)).toBe(8);
    expect(bracketTopNDefault(17)).toBe(16);
  });

  it('floors at 1 for a degenerate field of 0 or 1', () => {
    expect(bracketTopNDefault(0)).toBe(1);
    expect(bracketTopNDefault(1)).toBe(1);
  });
});

describe('advanceRoundReq — the seeded single_elim payload', () => {
  const SOURCE: RoundDef = {
    id: 'r1',
    label: 'Qualifying',
    classes: ['c1', 'c2'],
    format: 'timed_qual',
    params: { rounds: '2' },
    win_condition: { Timed: { window_micros: 120_000_000 } },
    seeding: 'FromRoster',
    channel_mode: 'Static',
    staging_timer_secs: 300,
    start_procedure: { mode: 'randomized-delay', min_delay_ms: 2000, max_delay_ms: 5000 },
    grace_window: { Duration: { micros: 3000000 } },
    protest_window: 'Off'
  };

  it('builds a single_elim round seeded FromRanking, carrying classes + the bracket win condition', () => {
    const req = advanceRoundReq(SOURCE, 8, 'Qualifying — Bracket', WIN);
    expect(req).toEqual({
      label: 'Qualifying — Bracket',
      classes: ['c1', 'c2'],
      format: 'single_elim',
      params: {},
      // The bracket carries its OWN win condition (set in the builder), not the source's.
      win_condition: { FirstToLaps: { n: 3 } },
      seeding: { FromRanking: { source_rounds: ['r1'], top_n: 8 } }
    });
  });

  it('clamps a non-integer / sub-1 top_n to at least 1', () => {
    expect(advanceRoundReq(SOURCE, 0, 'x', WIN).seeding).toEqual({
      FromRanking: { source_rounds: ['r1'], top_n: 1 }
    });
    expect(advanceRoundReq(SOURCE, 4.7, 'x', WIN).seeding).toEqual({
      FromRanking: { source_rounds: ['r1'], top_n: 5 }
    });
  });

  it('does not mutate the source round classes array', () => {
    const req = advanceRoundReq(SOURCE, 8, 'x', WIN);
    expect(req.classes).not.toBe(SOURCE.classes);
    expect(req.classes).toEqual(SOURCE.classes);
  });

  it('proposes a sensible default bracket label', () => {
    expect(advanceRoundLabel(SOURCE)).toBe('Qualifying — Bracket');
  });
});
