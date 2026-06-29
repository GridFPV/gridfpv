import { describe, expect, it } from 'vitest';
import type { CompetitorRef, HeatId, HeatResult, HeatSummary, RankEntry } from '@gridfpv/types';
import {
  buildRoundRobinView,
  groupRoundRobinRotations,
  parseRoundRobinPoints,
  roundRobinStandings,
  rotationNumberOf
} from '../src/lib/roundRobin.js';

/** A scored heat result from `[ref, position, bestLapMicros?]` rows. */
function result(rows: [string, number, number?][]): HeatResult {
  return {
    places: rows.map(([competitor, position, bestLap]) => ({
      competitor: { adapter: 'rh-1', competitor },
      position,
      laps: 3,
      metric: { BestLapMicros: bestLap ?? null }
    }))
  };
}

const label = (ref: CompetitorRef): string =>
  ({ p1: 'AceOne', p2: 'Bolt', p3: 'Comet', p4: 'Dash' })[ref] ?? ref;

const heat = (id: string, lineup: string[]): HeatSummary => ({
  heat: id,
  lineup,
  round: 'rr',
  phase: 'Final',
  is_current: false
});

describe('rotationNumberOf — the rr-r{N} rotation number', () => {
  it('extracts the rotation number from a rr-r{N}-h{M} id', () => {
    expect(rotationNumberOf('rr-r1-h1')).toBe(1);
    expect(rotationNumberOf('rr-r2-h3')).toBe(2);
    expect(rotationNumberOf('rr-r10-h2')).toBe(10);
  });
  it('returns undefined for a non-round-robin id', () => {
    expect(rotationNumberOf('r1-h-abc')).toBeUndefined();
    expect(rotationNumberOf('cta-r0')).toBeUndefined();
  });
});

describe('parseRoundRobinPoints', () => {
  it('parses a CSV table and falls back to the default when blank', () => {
    expect(parseRoundRobinPoints('10,6,4')).toEqual([10, 6, 4]);
    expect(parseRoundRobinPoints('10, 6, 4')).toEqual([10, 6, 4]);
    expect(parseRoundRobinPoints(undefined)).toEqual([10, 6, 4, 3, 2, 1]);
    expect(parseRoundRobinPoints('')).toEqual([10, 6, 4, 3, 2, 1]);
  });
});

describe('groupRoundRobinRotations — group heats by rr-r{N} in order', () => {
  it('groups heats into rotations ascending, heats sorted within each rotation', () => {
    // Deliberately out of order to prove the grouping sorts.
    const heats = [
      heat('rr-r2-h1', ['p1', 'p3']),
      heat('rr-r1-h2', ['p3', 'p4']),
      heat('rr-r1-h1', ['p1', 'p2'])
    ];
    const rotations = groupRoundRobinRotations(
      heats,
      (h) => `Heat ${h.heat}`,
      label,
      () => undefined
    );
    expect(rotations.map((r) => r.name)).toEqual(['Rotation 1', 'Rotation 2']);
    // Within Rotation 1, h1 precedes h2.
    expect(rotations[0].heats.map((h) => h.heat)).toEqual(['rr-r1-h1', 'rr-r1-h2']);
    expect(rotations[1].heats.map((h) => h.heat)).toEqual(['rr-r2-h1']);
  });

  it('orders entries by finishing position when the heat is scored, else lineup order', () => {
    const heats = [heat('rr-r1-h1', ['p1', 'p2'])];
    const scored = result([
      ['p2', 1],
      ['p1', 2]
    ]);
    const rotations = groupRoundRobinRotations(
      heats,
      () => 'Heat',
      label,
      (id) => (id === 'rr-r1-h1' ? scored : undefined)
    );
    // Finishing order: Bolt (1st) then AceOne (2nd) — not lineup order.
    expect(rotations[0].heats[0].entries.map((e) => e.label)).toEqual(['Bolt', 'AceOne']);
    expect(rotations[0].heats[0].entries.map((e) => e.position)).toEqual([1, 2]);

    // An unscored heat keeps lineup order with no positions.
    const unscored = groupRoundRobinRotations(
      heats,
      () => 'Heat',
      label,
      () => undefined
    );
    expect(unscored[0].heats[0].entries.map((e) => e.label)).toEqual(['AceOne', 'Bolt']);
    expect(unscored[0].heats[0].entries.map((e) => e.position)).toEqual([undefined, undefined]);
  });
});

describe('roundRobinStandings — points aggregation + best lap, ordered by ranking', () => {
  const POINTS = [10, 6, 4, 3, 2, 1];

  it('sums the points table per finish; Best lap comes from the bestLapOf source, not the metric', () => {
    // Two rotations: p1 wins both (10+10=20), p2 2nd both (6+6=12), etc.
    const heats = [heat('rr-r1-h1', ['p1', 'p2', 'p3']), heat('rr-r2-h1', ['p1', 'p2', 'p3'])];
    const byHeat: Record<string, HeatResult> = {
      'rr-r1-h1': result([
        ['p1', 1],
        ['p2', 2],
        ['p3', 3]
      ]),
      'rr-r2-h1': result([
        ['p1', 1],
        ['p2', 2],
        ['p3', 3]
      ])
    };
    const ranking: RankEntry[] = [
      { competitor: 'p1', position: 1 },
      { competitor: 'p2', position: 2 },
      { competitor: 'p3', position: 3 }
    ];
    // Best lap comes from the dedicated standings source (the heat-result metric here is the
    // win-condition metric, not a best lap). p3 has none → null → renders as "—".
    const bestLap: Record<string, number | null> = {
      p1: 28_000_000,
      p2: 31_000_000,
      p3: null
    };
    const rows = roundRobinStandings(
      ranking,
      heats,
      POINTS,
      label,
      (id) => byHeat[id],
      (ref) => bestLap[ref] ?? null
    );
    // Rendered in the canonical ranking order, with summed points.
    expect(rows.map((r) => [r.label, r.position, r.points])).toEqual([
      ['AceOne', 1, 20],
      ['Bolt', 2, 12],
      ['Comet', 3, 8]
    ]);
    // Best lap is whatever the bestLapOf source returns; a null source → undefined (renders "—").
    expect(rows[0].bestLapMicros).toBe(28_000_000);
    expect(rows[1].bestLapMicros).toBe(31_000_000);
    expect(rows[2].bestLapMicros).toBeUndefined();
  });

  it('positions past the points table score 0; a pilot with no scored finish totals 0', () => {
    const heats = [heat('rr-r1-h1', ['p1', 'p2'])];
    const byHeat: Record<string, HeatResult> = {
      'rr-r1-h1': result([
        ['p1', 1],
        ['p2', 7] // beyond the 6-entry table → 0
      ])
    };
    const ranking: RankEntry[] = [
      { competitor: 'p1', position: 1 },
      { competitor: 'p2', position: 2 }
    ];
    const rows = roundRobinStandings(
      ranking,
      heats,
      POINTS,
      label,
      (id) => byHeat[id],
      () => null
    );
    expect(rows.find((r) => r.label === 'Bolt')?.points).toBe(0);
  });

  it('falls back to lineup order with zero points when the round has no ranking yet', () => {
    const heats = [heat('rr-r1-h1', ['p2', 'p1'])];
    const rows = roundRobinStandings(
      [],
      heats,
      POINTS,
      label,
      () => undefined,
      () => null
    );
    expect(rows.map((r) => [r.label, r.position, r.points])).toEqual([
      ['Bolt', 1, 0],
      ['AceOne', 2, 0]
    ]);
  });
});

describe('buildRoundRobinView — standings + rotation grid together', () => {
  it('assembles both halves from the ranking + heats + results', () => {
    const heats = [heat('rr-r1-h1', ['p1', 'p2']), heat('rr-r2-h1', ['p1', 'p2'])];
    const byHeat: Record<HeatId, HeatResult> = {
      'rr-r1-h1': result([
        ['p1', 1],
        ['p2', 2]
      ])
    };
    const ranking: RankEntry[] = [
      { competitor: 'p1', position: 1 },
      { competitor: 'p2', position: 2 }
    ];
    const view = buildRoundRobinView(ranking, heats, {
      pointsTable: [10, 6, 4, 3, 2, 1],
      label,
      heatName: (h) => `RR ${h.heat}`,
      resultByHeat: (id) => byHeat[id],
      bestLapOf: (ref) => (ref === 'p1' ? 27_500_000 : null)
    });
    expect(view.standings.map((r) => r.label)).toEqual(['AceOne', 'Bolt']);
    // Best lap flows through from the bestLapOf source.
    expect(view.standings[0].bestLapMicros).toBe(27_500_000);
    expect(view.standings[1].bestLapMicros).toBeUndefined();
    expect(view.rotations.map((r) => r.name)).toEqual(['Rotation 1', 'Rotation 2']);
    expect(view.rotations[0].heats[0].name).toBe('RR rr-r1-h1');
  });
});
