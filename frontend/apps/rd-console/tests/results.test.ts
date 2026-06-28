import { describe, expect, it } from 'vitest';
import type { CompletedHeat, EventOutcome } from '@gridfpv/types';
import { bracketFromOutcome, toExportJson } from '../src/lib/results.js';
import { eventOutcome } from './fixtures.js';

/** A completed chase race heat from `[competitor, position]` rows. */
function chaseHeat(id: string, rows: [string, number][]): CompletedHeat {
  return {
    heat: id,
    result: {
      places: rows.map(([competitor, position]) => ({
        competitor: { adapter: 'rh-1', competitor },
        position,
        laps: 3,
        metric: { BestLapMicros: 1 }
      }))
    }
  };
}

describe('bracketFromOutcome', () => {
  it('lays completed bracket heats into rounds (final last), marking winners', () => {
    const bracket = bracketFromOutcome(eventOutcome);
    // 3 heats → semis (2) + final (1).
    expect(bracket.rounds.map((r) => r.name)).toEqual(['Semifinals', 'Final']);
    expect(bracket.rounds[0].matches).toHaveLength(2);
    expect(bracket.rounds[1].matches).toHaveLength(1);

    const final = bracket.rounds[1].matches[0];
    expect(final.heat).toBe('final');
    const winner = final.slots.find((s) => s.winner);
    expect(winner?.competitor).toBe('ALICE');
  });

  it('returns no rounds for an outcome with no bracket heats', () => {
    expect(bracketFromOutcome({ ...eventOutcome, bracket_heats: [] }).rounds).toEqual([]);
  });

  it('collapses cta-r* chase heats into one final match with scores + a champion', () => {
    // A 2-up chase final: A wins r0, B wins r1 (1-1), A wins r2 (2-1) → A champion, B runner-up.
    const outcome: EventOutcome = {
      ...eventOutcome,
      bracket_heats: [
        chaseHeat('cta-r0', [
          ['ALICE', 1],
          ['BOB', 2]
        ]),
        chaseHeat('cta-r1', [
          ['BOB', 1],
          ['ALICE', 2]
        ]),
        chaseHeat('cta-r2', [
          ['ALICE', 1],
          ['BOB', 2]
        ])
      ]
    };
    const bracket = bracketFromOutcome(outcome);
    // The three races collapse into ONE Final round with a single match.
    expect(bracket.rounds.map((r) => r.name)).toEqual(['Final']);
    const match = bracket.rounds[0].matches[0];
    expect(match.note).toBe('Best of 3 · 3 races');
    const alice = match.slots.find((s) => s.competitor === 'ALICE')!;
    const bob = match.slots.find((s) => s.competitor === 'BOB')!;
    expect(alice.score).toBe(2);
    expect(alice.winner).toBe(true);
    expect(bob.score).toBe(1);
    expect(bob.winner).toBe(false);
  });

  it('keeps non-chase heats as elimination levels alongside the collapsed chase final', () => {
    // Two semis heats (single-elim) + a 1-race chase final (wins_to_win inferred = 1).
    const outcome: EventOutcome = {
      ...eventOutcome,
      bracket_heats: [
        chaseHeat('sf-1', [
          ['ALICE', 1],
          ['DANA', 2]
        ]),
        chaseHeat('sf-2', [
          ['BOB', 1],
          ['CARMEN', 2]
        ]),
        chaseHeat('cta-r0', [
          ['ALICE', 1],
          ['BOB', 2]
        ])
      ]
    };
    const bracket = bracketFromOutcome(outcome);
    // The collapsed chase final is appended as the LAST round (one match, the series caption);
    const last = bracket.rounds[bracket.rounds.length - 1];
    expect(last.matches).toHaveLength(1);
    // Single race → inferred best-of-1; ALICE took it.
    expect(last.matches[0].note).toBe('Best of 1 · 1 race');
    expect(last.matches[0].slots.find((s) => s.competitor === 'ALICE')?.winner).toBe(true);
    // …and the two non-chase (single-elim) heats still render as their own per-heat matches before it.
    const earlier = bracket.rounds.slice(0, -1).flatMap((r) => r.matches);
    expect(earlier).toHaveLength(2);
    expect(earlier.every((m) => m.note === undefined)).toBe(true);
  });
});

describe('toExportJson', () => {
  it('serializes typed projection data with bigints as numbers', () => {
    const json = toExportJson({ at: 1_000_000 });
    expect(JSON.parse(json)).toEqual({ at: 1_000_000 });
  });
});
