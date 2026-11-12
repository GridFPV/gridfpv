/** The Results page's pure helpers (`lib/results.ts`): which phases to offer, and one table. */
import { describe, expect, it } from 'vitest';
import type { Format, LapView, PilotId, PlacedView } from '@gridfpv/types';
import { buildNames } from '../src/lib/names.js';
import { defaultResult, heatsUnder, resultChoices, resultTable } from '../src/lib/results.js';
import { FIRST_TO_3, eventView, group, heat, race, raceSeat } from './competition.js';

const S = 1_000_000;
const PILOTS = [
  { id: 'p-1a', callsign: 'Ace', vtx_types: [] },
  { id: 'p-2b', callsign: 'Bee', vtx_types: [] }
];

const BEST_2: Format = {
  name: 'Best 2 in a row',
  race: { end: 'Open', staging_micros: 0, first_lap: 'HoleShot' },
  completion: 'DirectorDecides',
  ranking: { sort: [{ BestConsecutive: 2 }] }
};
const PRACTICE: Format = {
  name: 'Practice',
  race: { end: 'Open', staging_micros: 0, first_lap: 'HoleShot' },
  completion: 'DirectorDecides'
};
const rankedBy = (...sort: NonNullable<Format['ranking']>['sort']): Format => ({
  ...PRACTICE,
  name: 'Ranked',
  ranking: { sort }
});

const placed = (pilot: PilotId, position: number, over: Partial<PlacedView> = {}): PlacedView => ({
  pilot,
  position,
  disqualified: false,
  position_set: false,
  flew: true,
  wins: 0,
  points: 0,
  laps: 0,
  ...over
});
const RANKING = [placed('p-1a', 1), placed('p-2b', 2)];

/** Laps of the given lengths in seconds, one after the other. */
const laps = (...seconds: number[]): LapView[] => {
  let at = 0;
  return seconds.map((length, index) => {
    const lap: LapView = {
      crossing: index + 1,
      started_at: at,
      ended_at: at + length * S,
      micros: length * S,
      restart: false,
      timer_restarted: false
    };
    at += length * S;
    return lap;
  });
};

const phases = [
  group('quali', 'Qualifying', undefined, { format: BEST_2, ranking: RANKING }),
  heat('q-1', 'Qualifying Heat 1', ['p-1a', 'p-2b'], {
    parent: 'quali',
    format: BEST_2,
    ranking: RANKING
  }),
  // Not raced yet: it has no ranking.
  heat('q-2', 'Qualifying Heat 2', ['p-1a'], { parent: 'quali', format: BEST_2 }),
  // A holder with nothing of its own to show.
  group('mains', 'Mains'),
  group('mains/a', 'A-Main', 'mains', { format: FIRST_TO_3, ranking: RANKING }),
  heat('mains/a/h1', 'A-Main Race', ['p-1a', 'p-2b'], { parent: 'mains/a', ranking: RANKING }),
  // Practice lists who flew, but it is not ranked.
  heat('practice', 'Practice', ['p-1a'], { format: PRACTICE, ranking: [placed('p-1a', 1)] }),
  // A list of pilots with no format in force is not a result either.
  group('loose', 'Loose', undefined, { ranking: [placed('p-1a', 1)] })
];
const names = buildNames(phases, PILOTS);

describe('resultChoices', () => {
  it('offers every ranked phase with a ranking, in tree order, with the phases above it', () => {
    expect(resultChoices(eventView({ phases }))).toEqual([
      { id: 'quali', label: 'Qualifying', heat: false },
      { id: 'q-1', label: 'Qualifying › Qualifying Heat 1', heat: true },
      { id: 'mains/a', label: 'Mains › A-Main', heat: false },
      { id: 'mains/a/h1', label: 'Mains › A-Main › A-Main Race', heat: true }
    ]);
  });

  it('leaves out a phase with no ranking yet, an unranked one, and one with no format', () => {
    const ids = resultChoices(eventView({ phases })).map((choice) => choice.id);
    expect(ids).not.toContain('q-2');
    expect(ids).not.toContain('mains');
    expect(ids).not.toContain('practice');
    expect(ids).not.toContain('loose');
  });

  it('names a phase whose parent is missing by its own label alone', () => {
    const orphan = heat('orphan', 'Orphan', ['p-1a'], { parent: 'gone', ranking: RANKING });
    expect(resultChoices(eventView({ phases: [orphan] }))).toEqual([
      { id: 'orphan', label: 'Orphan', heat: true }
    ]);
  });

  it('is empty before anything is ranked', () => {
    expect(resultChoices(eventView())).toEqual([]);
    expect(resultChoices(eventView({ phases: [group('mains', 'Mains')] }))).toEqual([]);
  });
});

describe('heatsUnder', () => {
  const view = eventView({ phases });
  const ids = (phase: string) => heatsUnder(view, phase).map((found) => found.def.id);

  it('finds the heats directly beneath a phase', () => {
    expect(ids('quali')).toEqual(['q-1', 'q-2']);
  });

  it('finds the heats at every level beneath a phase', () => {
    expect(ids('mains')).toEqual(['mains/a/h1']);
    expect(ids('mains/a')).toEqual(['mains/a/h1']);
  });

  it('is the heat itself for a heat', () => {
    expect(ids('q-1')).toEqual(['q-1']);
  });

  it('is empty for a phase that holds no heat, and for one that is not there', () => {
    expect(ids('loose')).toEqual([]);
    expect(ids('gone')).toEqual([]);
  });
});

describe('defaultResult', () => {
  const choices = resultChoices(eventView({ phases }));
  const opened = (...races: ReturnType<typeof race>[]) =>
    defaultResult(eventView({ phases, races }), choices);

  it('opens on the phase that holds the heat raced last: it is the fuller table', () => {
    expect(opened(race('mains/a/h1', 1, 'Saved'), race('q-1', 1, 'Saved'))).toBe('quali');
    expect(opened(race('q-1', 1, 'Saved'), race('mains/a/h1', 1, 'Saved'))).toBe('mains/a');
  });

  it('counts only saved races as raced', () => {
    expect(
      opened(
        race('q-1', 1, 'Saved'),
        race('mains/a/h1', 1, 'Discarded'),
        race('mains/a/h1', 2, 'Stopped')
      )
    ).toBe('quali');
  });

  it('opens on the heat itself when the phase that holds it has no result', () => {
    const top = heat('final', 'Final', ['p-1a'], { ranking: RANKING });
    const view = eventView({ phases: [...phases, top], races: [race('final', 1, 'Saved')] });
    expect(defaultResult(view, resultChoices(view))).toBe('final');
  });

  it('opens on the first result when the heat raced last has none to show', () => {
    expect(opened(race('q-1', 1, 'Saved'), race('practice', 1, 'Saved'))).toBe('quali');
  });

  it('opens on the first result before any race is saved', () => {
    expect(opened()).toBe('quali');
    expect(opened(race('mains/a/h1', 1, 'Running'))).toBe('quali');
  });

  it('opens on nothing when there is no result at all', () => {
    expect(defaultResult(eventView(), [])).toBeUndefined();
    expect(
      defaultResult(eventView({ phases, races: [race('q-1', 1, 'Saved')] }), [])
    ).toBeUndefined();
  });
});

describe('resultTable', () => {
  const seats = (ace: number[], bee: number[] = []) => [
    raceSeat(0, 'p-1a', { laps: laps(...ace) }),
    raceSeat(1, 'p-2b', { laps: laps(...bee) })
  ];
  const view = eventView({
    phases,
    races: [
      // Ace: 3+2 or 2+3, so 5 s. Bee has one lap: no run of two.
      race('q-1', 1, 'Saved', { seats: seats([3, 2, 3], [4]) }),
      // Faster, but never saved.
      race('q-1', 2, 'Discarded', { seats: seats([1, 1]) }),
      race('q-2', 1, 'Saved', { seats: seats([2, 2]) }),
      race('q-2', 2, 'Stopped', { seats: seats([1, 1]) }),
      // Faster, but in a heat under another phase.
      race('mains/a/h1', 1, 'Saved', { seats: seats([1, 1, 1], [1, 1, 1]) })
    ]
  });
  const phase = (id: string) => {
    const found = phases.find((candidate) => candidate.def.id === id);
    if (!found) throw new Error(`no phase ${id}`);
    return found;
  };

  it("lists the pilots in the Director's order, by callsign, with the numbers behind each place", () => {
    const table = resultTable(view, phase('quali'), names);
    expect(table.rows.map((row) => [row.position, row.callsign])).toEqual([
      [1, 'Ace'],
      [2, 'Bee']
    ]);
    expect(table.rows[0].placed).toBe(RANKING[0]);
    expect(table.rows[1].placed).toBe(RANKING[1]);
  });

  it("gives each pilot's fastest run of N laps in a row from the saved races under the phase", () => {
    const table = resultTable(view, phase('quali'), names);
    expect(table.runLength).toBe(2);
    expect(table.rows[0].bestRunMicros).toBe(4 * S);
    // One lap is not a run of two.
    expect(table.rows[1].bestRunMicros).toBeUndefined();
  });

  it("keeps a heat's own table to that heat's races", () => {
    const table = resultTable(view, phase('q-1'), names);
    expect(table.runLength).toBe(2);
    expect(table.rows[0].bestRunMicros).toBe(5 * S);
  });

  it('has no run column, and no run times, for a phase not ranked on laps in a row', () => {
    const table = resultTable(view, phase('mains/a'), names);
    expect(table.runLength).toBeUndefined();
    expect(table.rows.map((row) => row.bestRunMicros)).toEqual([undefined, undefined]);
    expect(table.rows.map((row) => row.callsign)).toEqual(['Ace', 'Bee']);
  });

  it('shows wins or points only when the phase is ranked on them', () => {
    const tableFor = (format: Format | undefined) =>
      resultTable(view, { ...phase('mains/a'), format }, names);
    expect(tableFor(rankedBy('Wins', 'LastFinish'))).toMatchObject({
      showWins: true,
      showPoints: false
    });
    expect(tableFor(rankedBy('Points', 'BestFinish'))).toMatchObject({
      showWins: false,
      showPoints: true
    });
    // Whichever place in the rule the measure has.
    expect(tableFor(rankedBy('Laps', 'Points', 'Wins'))).toMatchObject({
      showWins: true,
      showPoints: true
    });
    expect(tableFor(FIRST_TO_3)).toMatchObject({ showWins: false, showPoints: false });
    expect(resultTable(view, phase('quali'), names)).toMatchObject({
      showWins: false,
      showPoints: false
    });
  });

  it('still lists the pilots of a phase with no ranking rule or no format, with no extra columns', () => {
    for (const format of [PRACTICE, undefined]) {
      const table = resultTable(view, { ...phase('quali'), format }, names);
      expect(table).toMatchObject({ runLength: undefined, showWins: false, showPoints: false });
      expect(table.rows.map((row) => row.callsign)).toEqual(['Ace', 'Bee']);
    }
  });

  it('keeps pilots tied on a position in the order the Director gave', () => {
    const tied = [placed('p-2b', 1), placed('p-1a', 1)];
    const table = resultTable(view, { ...phase('mains/a'), ranking: tied }, names);
    expect(table.rows.map((row) => [row.position, row.callsign])).toEqual([
      [1, 'Bee'],
      [1, 'Ace']
    ]);
  });

  it('names a pilot who is no longer in the directory in words, never by id', () => {
    const table = resultTable(view, { ...phase('mains/a'), ranking: [placed('p-9z', 1)] }, names);
    expect(table.rows[0].callsign).toBe('Unknown pilot');
  });

  it('has no rows for a phase nobody is ranked in', () => {
    expect(resultTable(view, phase('q-2'), names).rows).toEqual([]);
  });
});
