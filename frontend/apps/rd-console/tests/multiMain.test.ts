import { describe, expect, it } from 'vitest';
import type { CompetitorRef, HeatId, HeatResult, HeatSummary, RankEntry } from '@gridfpv/types';
import {
  buildMultiMainView,
  mainTierIndexOf,
  mainTierName,
  isMultiMainRound
} from '../src/lib/multiMain.js';

/** A scored heat result from `[ref, position]` rows. */
function result(rows: [string, number][]): HeatResult {
  return {
    places: rows.map(([competitor, position]) => ({
      competitor: { adapter: 'rh-1', competitor },
      position,
      laps: 3,
      metric: { BestLapMicros: null },
      best_lap_micros: null
    }))
  };
}

const label = (ref: CompetitorRef): string =>
  ({ p1: 'AceOne', p2: 'Bolt', p3: 'Comet', p4: 'Dash', p5: 'Echo', p6: 'Fox' })[ref] ?? ref;

const heat = (id: string, lineup: string[]): HeatSummary => ({
  heat: id,
  lineup,
  round: 'mm',
  phase: 'Final',
  is_current: false
});

describe('mainTierIndexOf — the main-X tier index', () => {
  it('maps main-A/B/C to 0/1/2', () => {
    expect(mainTierIndexOf('main-A')).toBe(0);
    expect(mainTierIndexOf('main-B')).toBe(1);
    expect(mainTierIndexOf('main-C')).toBe(2);
  });
  it('parses the numeric >26-mains fallback id', () => {
    expect(mainTierIndexOf('main-26')).toBe(26);
  });
  it('returns undefined for a non-multi-main id', () => {
    expect(mainTierIndexOf('rr-r1-h1')).toBeUndefined();
    expect(mainTierIndexOf('cta-r0')).toBeUndefined();
  });
});

describe('isMultiMainRound (re-exported)', () => {
  it('is true only for the multi_main format', () => {
    expect(isMultiMainRound({ format: 'multi_main' } as never)).toBe(true);
    expect(isMultiMainRound({ format: 'round_robin' } as never)).toBe(false);
  });
});

describe('buildMultiMainView — standings in ranking order, tagged with each pilot tier', () => {
  it('orders by ranking and assigns the right tierName per pilot (from the main-X heat)', () => {
    // A-main = p1,p2; B-main = p3,p4 (by their main-X heat lineup).
    const heats = [heat('main-A', ['p1', 'p2']), heat('main-B', ['p3', 'p4'])];
    const ranking: RankEntry[] = [
      { competitor: 'p1', position: 1 },
      { competitor: 'p2', position: 2 },
      { competitor: 'p3', position: 3 },
      { competitor: 'p4', position: 4 }
    ];
    const view = buildMultiMainView(ranking, heats, { label, tierNameOf: mainTierName });
    // Rendered in canonical ranking order, callsigns resolved.
    expect(view.standings.map((r) => [r.label, r.position, r.tierName])).toEqual([
      ['AceOne', 1, 'A-Main'],
      ['Bolt', 2, 'A-Main'],
      ['Comet', 3, 'B-Main'],
      ['Dash', 4, 'B-Main']
    ]);
  });

  it('derives the tier from a scored result when available (not just the lineup)', () => {
    const heats = [heat('main-A', ['p1', 'p2']), heat('main-B', ['p3', 'p4'])];
    const byHeat: Record<HeatId, HeatResult> = {
      'main-A': result([
        ['p2', 1],
        ['p1', 2]
      ])
    };
    const ranking: RankEntry[] = [
      { competitor: 'p2', position: 1 },
      { competitor: 'p1', position: 2 },
      { competitor: 'p3', position: 3 }
    ];
    const view = buildMultiMainView(ranking, heats, {
      label,
      tierNameOf: mainTierName,
      resultByHeat: (id) => byHeat[id]
    });
    expect(view.standings.find((r) => r.label === 'Bolt')?.tierName).toBe('A-Main');
    expect(view.standings.find((r) => r.label === 'Comet')?.tierName).toBe('B-Main');
  });

  it('shows the best (highest) main a bump-ladder pilot reached', () => {
    // p3 raced in main-C (seed) AND bumped into main-B → shows B-Main (the better tier).
    const heats = [
      heat('main-A', ['p1', 'p2']),
      heat('main-B', ['p3', 'p4']),
      heat('main-C', ['p3', 'p5'])
    ];
    const ranking: RankEntry[] = [
      { competitor: 'p1', position: 1 },
      { competitor: 'p3', position: 2 }
    ];
    const view = buildMultiMainView(ranking, heats, { label, tierNameOf: mainTierName });
    expect(view.standings.find((r) => r.label === 'Comet')?.tierName).toBe('B-Main');
  });

  it('falls back to lineup order with tiers when the round has no ranking yet', () => {
    const heats = [heat('main-A', ['p1', 'p2']), heat('main-B', ['p3'])];
    const view = buildMultiMainView([], heats, { label, tierNameOf: mainTierName });
    expect(view.standings.map((r) => [r.label, r.position, r.tierName])).toEqual([
      ['AceOne', 1, 'A-Main'],
      ['Bolt', 2, 'A-Main'],
      ['Comet', 3, 'B-Main']
    ]);
  });
});
