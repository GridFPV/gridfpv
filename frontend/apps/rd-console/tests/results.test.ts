import { describe, expect, it } from 'vitest';
import { bracketFromOutcome, toExportJson } from '../src/lib/results.js';
import { eventOutcome } from './fixtures.js';

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
});

describe('toExportJson', () => {
  it('serializes typed projection data with bigints as numbers', () => {
    const json = toExportJson({ at: 1_000_000 });
    expect(JSON.parse(json)).toEqual({ at: 1_000_000 });
  });
});
