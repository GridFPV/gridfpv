import { describe, expect, it } from 'vitest';
import type { ClassStanding, CompetitorRef, HeatResult, RankEntry } from '@gridfpv/types';
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

  // #341: the legacy event-level sections (`standings` / `heatResult`) were carried through as-is,
  // so their competitor fields leaked raw refs even though every other view resolved. They must
  // resolve through the same resolver, raw ref kept alongside.
  it('resolves the legacy standings + heat-result sections too (#341)', () => {
    const standings: RankEntry[] = [{ competitor: 'p1', position: 1 }];
    const heatResult: HeatResult = {
      places: [
        {
          competitor: { adapter: 'rh-1', competitor: 'p1' },
          position: 1,
          laps: 3,
          metric: { BestLapMicros: 41_250_000 },
          best_lap_micros: 41_250_000
        }
      ]
    };
    const out = buildResultsExport({ resolveCompetitor, standings, heatResult });

    // The standings rows resolve like every other view (raw ref kept alongside).
    expect(out.standings?.[0].competitor).toBe('AceOne');
    expect(out.standings?.[0].competitor_ref).toBe('p1');
    expect(out.standings?.[0].position).toBe(1);

    // The heat result's placements resolve the CompetitorKey's ref; the payload is preserved.
    expect(out.heatResult?.places[0].competitor).toBe('AceOne');
    expect(out.heatResult?.places[0].competitor_ref).toBe('p1');
    expect(out.heatResult?.places[0].position).toBe(1);
    expect(out.heatResult?.places[0].laps).toBe(3);

    // Nothing in the serialized export shows a bare raw ref as the display field.
    const parsed = JSON.parse(toExportJson(out));
    expect(parsed.standings[0].competitor).toBe('AceOne');
    expect(parsed.heatResult.places[0].competitor).toBe('AceOne');
  });
});
