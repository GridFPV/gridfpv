import { describe, expect, it } from 'vitest';
import type { ClassStanding, CompetitorRef, RankEntry } from '@gridfpv/types';
import { buildResultsExport, toExportJson } from '../src/lib/results.js';

describe('toExportJson', () => {
  it('serializes typed projection data with bigints as numbers', () => {
    const json = toExportJson({ at: 1_000_000 });
    expect(JSON.parse(json)).toEqual({ at: 1_000_000 });
  });
});

describe('buildResultsExport (friendly names, P1-2)', () => {
  // Resolve a couple of refs to callsigns; leave an unknown ref to fall through to its raw handle.
  const resolveCompetitor = (ref: CompetitorRef): string =>
    ({ p1: 'AceOne', 'node-0': 'Raceband R1' })[ref] ?? ref;

  const classStandings: ClassStanding[] = [
    {
      competitor: 'p1',
      position: 1,
      points: 6,
      best_lap_micros: 41_250_000,
      total_laps: 9,
      rounds_entered: 1
    } as ClassStanding
  ];
  const roundRanking: RankEntry[] = [{ competitor: 'node-0', position: 1 } as RankEntry];

  it('bakes callsigns into the competitor field and keeps the raw ref alongside', () => {
    const out = buildResultsExport({
      resolveCompetitor,
      className: 'Open',
      classStandings,
      roundLabel: 'Qualifying',
      roundRanking
    });

    expect(out.class_standings?.class).toBe('Open');
    expect(out.class_standings?.standings[0].competitor).toBe('AceOne');
    expect(out.class_standings?.standings[0].competitor_ref).toBe('p1');
    // The numeric payload is preserved.
    expect(out.class_standings?.standings[0].points).toBe(6);

    expect(out.round_ranking?.round).toBe('Qualifying');
    expect(out.round_ranking?.ranking[0].competitor).toBe('Raceband R1');
    expect(out.round_ranking?.ranking[0].competitor_ref).toBe('node-0');
  });

  it('omits views that are not present', () => {
    const out = buildResultsExport({ resolveCompetitor, roundRanking });
    expect(out.class_standings).toBeUndefined();
    expect(out.round_ranking).toBeDefined();
  });

  it('the serialized JSON carries the friendly name, not the raw ref', () => {
    const out = buildResultsExport({ resolveCompetitor, className: 'Open', classStandings });
    const json = toExportJson(out);
    expect(json).toContain('AceOne');
    // The raw ref is still present (as competitor_ref) for traceability, but the displayed name wins.
    const parsed = JSON.parse(json);
    expect(parsed.class_standings.standings[0].competitor).toBe('AceOne');
  });
});
