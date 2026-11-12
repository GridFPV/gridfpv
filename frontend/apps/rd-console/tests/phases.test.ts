/** The snapshot helpers behind the Event page and the Run view (`lib/phases.ts`). */
import { describe, expect, it } from 'vitest';
import type { Format } from '@gridfpv/types';
import {
  TOP_LEVEL_HEATS,
  currentRace,
  defaultHeat,
  groupOfHeat,
  heatGroups,
  holderChoices,
  isBye,
  isHeat,
  newPhaseId,
  phaseStatus,
  phaseTree,
  pilotsInLineup,
  racesOfHeat,
  slugify
} from '../src/lib/phases.js';
import { eventView, group, heat, race } from './competition.js';

describe('phaseTree', () => {
  it('puts each child under its parent, keeping the order phases were added in', () => {
    const phases = [
      group('finals', 'Finals'),
      heat('quali', 'Qualifying', ['a']),
      group('finals/w1', 'Winners Round 1', 'finals'),
      heat('finals/w1/h1', 'Winners Round 1 Heat 1', ['a'], { parent: 'finals/w1' }),
      heat('finals/final', 'Final', ['a'], { parent: 'finals' })
    ];
    expect(phaseTree(phases).map((row) => [row.phase.def.id, row.depth])).toEqual([
      ['finals', 0],
      ['finals/w1', 1],
      ['finals/w1/h1', 2],
      ['finals/final', 1],
      ['quali', 0]
    ]);
  });

  it('shows a phase whose parent is missing at the top instead of hiding it', () => {
    const rows = phaseTree([heat('orphan', 'Orphan', ['a'], { parent: 'gone' })]);
    expect(rows.map((row) => [row.phase.def.id, row.depth])).toEqual([['orphan', 0]]);
  });
});

describe('phaseStatus', () => {
  const quali = heat('quali', 'Qualifying', ['a', 'b']);

  it('is ready when nothing stands in the way', () => {
    expect(phaseStatus(quali, eventView({ phases: [quali] }))).toBe('ready');
  });

  it('is waiting, complete or locked as the snapshot says', () => {
    const view = eventView();
    expect(phaseStatus({ ...quali, waiting: true }, view)).toBe('waiting');
    expect(phaseStatus({ ...quali, complete: true }, view)).toBe('complete');
    expect(phaseStatus({ ...quali, complete: true, locked: true }, view)).toBe('locked');
  });

  it('is racing while the undecided race is in the heat, and so is every phase above it', () => {
    const finals = group('finals', 'Finals');
    const final = heat('finals/final', 'Final', ['a', 'b'], { parent: 'finals' });
    const running = race('finals/final', 1, 'Running');
    const view = eventView({
      phases: [finals, final, quali],
      races: [running],
      current_race: running.race
    });
    expect(phaseStatus(final, view)).toBe('racing');
    expect(phaseStatus(finals, view)).toBe('racing');
    expect(phaseStatus(quali, view)).toBe('ready');
  });
});

describe('races and lineups', () => {
  it('currentRace finds the undecided race, or nothing', () => {
    const stopped = race('quali', 2, 'Stopped');
    const view = eventView({ races: [race('quali', 1, 'Saved'), stopped] });
    expect(currentRace(view)).toBeUndefined();
    expect(currentRace({ ...view, current_race: stopped.race })).toBe(stopped);
  });

  it("racesOfHeat keeps one heat's races, oldest first", () => {
    const view = eventView({
      races: [race('a', 1, 'Discarded'), race('b', 1, 'Saved'), race('a', 2, 'Saved')]
    });
    expect(racesOfHeat(view, 'a').map((r) => r.state)).toEqual(['Discarded', 'Saved']);
  });

  it('pilotsInLineup lists the named pilots in seat order', () => {
    const waiting = { Waiting: { phase: 'x', position: 1 } };
    expect(pilotsInLineup(heat('h', 'H', ['a', 'Open', waiting, 'b', 'Nobody']))).toEqual([
      'a',
      'b'
    ]);
  });

  it('isBye is one named pilot and nobody else to fly against', () => {
    expect(isBye(heat('h', 'H', ['a']))).toBe(true);
    expect(isBye(heat('h', 'H', ['a', 'Nobody', 'Nobody']))).toBe(true);
    expect(isBye(heat('h', 'H', ['a', 'b']))).toBe(false);
    expect(isBye(heat('h', 'H', ['a', { Waiting: { phase: 'x', position: 2 } }]))).toBe(false);
    expect(isBye(heat('h', 'H', ['Open']))).toBe(false);
  });

  it('isHeat tells a heat from a holder of phases', () => {
    expect(isHeat(heat('h', 'H', ['a']).def)).toBe(true);
    expect(isHeat(group('g', 'G').def)).toBe(false);
  });
});

describe('heatGroups', () => {
  const phases = [
    group('finals', 'Finals'),
    heat('quali-1', 'Qualifying Heat 1', ['a']),
    group('finals/w1', 'Winners Round 1', 'finals'),
    heat('finals/w1/h1', 'Winners Round 1 Heat 1', ['a'], { parent: 'finals/w1' }),
    heat('finals/final', 'Final', ['a'], { parent: 'finals' }),
    heat('finals/w1/h2', 'Winners Round 1 Heat 2', ['a'], { parent: 'finals/w1' }),
    heat('quali-2', 'Qualifying Heat 2', ['a']),
    group('empty', 'Nothing yet')
  ];
  const shape = (groups: ReturnType<typeof heatGroups>) =>
    groups.map((entry) => [entry.key, entry.label, entry.heats.map((h) => h.def.id)]);

  it('groups heats by the phase that holds them, in tree order, naming the phases above', () => {
    expect(shape(heatGroups(phases))).toEqual([
      ['finals/w1', 'Finals › Winners Round 1', ['finals/w1/h1', 'finals/w1/h2']],
      ['finals', 'Finals', ['finals/final']],
      ['', 'Heats', ['quali-1', 'quali-2']]
    ]);
    expect(TOP_LEVEL_HEATS).toBe('Heats');
  });

  it('hands back the phases themselves, and leaves out a phase that holds no heat', () => {
    const groups = heatGroups(phases);
    expect(groups[1].heats[0]).toBe(phases[4]);
    expect(groups.some((entry) => entry.key === 'empty')).toBe(false);
  });

  it('puts a heat whose parent is missing with the heats at the top, so it is not hidden', () => {
    const orphaned = [
      heat('orphan', 'Orphan', ['a'], { parent: 'gone' }),
      heat('top', 'Top', ['a'])
    ];
    expect(shape(heatGroups(orphaned))).toEqual([['', 'Heats', ['orphan', 'top']]]);
  });

  it('is empty for an event with no heats', () => {
    expect(heatGroups([])).toEqual([]);
    expect(heatGroups([group('finals', 'Finals')])).toEqual([]);
  });

  it('groupOfHeat finds the group a heat is in, by the heat', () => {
    const groups = heatGroups(phases);
    expect(groupOfHeat(groups, 'finals/w1/h2')).toBe(groups[0]);
    expect(groupOfHeat(groups, 'quali-2')).toBe(groups[2]);
    // A phase that holds heats is not a heat, and an unknown id is in no group.
    expect(groupOfHeat(groups, 'finals/w1')).toBeUndefined();
    expect(groupOfHeat(groups, 'gone')).toBeUndefined();
    expect(groupOfHeat([], 'quali-1')).toBeUndefined();
  });
});

describe('holderChoices', () => {
  const TIMED: Format = {
    name: '2 minutes',
    race: {
      end: { Timed: { micros: 120_000_000, grace_micros: 0 } },
      staging_micros: 0,
      first_lap: 'HoleShot'
    },
    completion: { SavedRaces: 1 }
  };

  it('lists every phase that holds phases, in tree order, with the phases above it', () => {
    const phases = [
      group('quali', 'Qualifying'),
      heat('quali/h1', 'Qualifying Heat 1', ['a'], { parent: 'quali' }),
      group('mains', 'Mains', undefined, { format: TIMED }),
      group('mains/a', 'A-Main', 'mains', { locked: true }),
      heat('top', 'Top', ['a']),
      group('quali/r2', 'Round 2', 'quali')
    ];
    expect(holderChoices(phases)).toEqual([
      { id: 'quali', label: 'Qualifying', format: undefined, locked: false },
      { id: 'quali/r2', label: 'Qualifying › Round 2', format: undefined, locked: false },
      { id: 'mains', label: 'Mains', format: TIMED, locked: false },
      { id: 'mains/a', label: 'Mains › A-Main', format: undefined, locked: true }
    ]);
  });

  it('names a phase whose parent is missing by its own label alone', () => {
    expect(holderChoices([group('lost', 'Lost', 'gone')])).toEqual([
      { id: 'lost', label: 'Lost', format: undefined, locked: false }
    ]);
  });

  it('is empty when the event has only heats, or nothing', () => {
    expect(holderChoices([heat('h', 'H', ['a'])])).toEqual([]);
    expect(holderChoices([])).toEqual([]);
  });
});

describe('defaultHeat', () => {
  const phases = [
    group('quali', 'Qualifying'),
    heat('q-1', 'Qualifying Heat 1', ['a'], { parent: 'quali' }),
    heat('q-2', 'Qualifying Heat 2', ['a'], { parent: 'quali' }),
    heat('q-3', 'Qualifying Heat 3', ['a'], { parent: 'quali' })
  ];

  it('is the heat with a race that is not saved or discarded yet, before anything else', () => {
    const stopped = race('q-2', 1, 'Stopped');
    const view = eventView({
      phases,
      races: [stopped, race('q-1', 1, 'Saved')],
      current_race: stopped.race,
      queue: ['q-3']
    });
    expect(defaultHeat(view)).toBe('q-2');
  });

  it('is the first heat in the run queue when no race is undecided', () => {
    const view = eventView({ phases, races: [race('q-1', 1, 'Saved')], queue: ['q-3', 'q-2'] });
    expect(defaultHeat(view)).toBe('q-3');
  });

  it('is the heat raced last when the queue is empty', () => {
    const view = eventView({
      phases,
      races: [race('q-3', 1, 'Saved'), race('q-2', 1, 'Discarded')]
    });
    expect(defaultHeat(view)).toBe('q-2');
  });

  it('is the first heat there is when nothing has raced, skipping phases that hold heats', () => {
    expect(defaultHeat(eventView({ phases }))).toBe('q-1');
  });

  it('is nothing for an event with no heats', () => {
    expect(defaultHeat(eventView())).toBeUndefined();
    expect(defaultHeat(eventView({ phases: [group('quali', 'Qualifying')] }))).toBeUndefined();
  });

  it('falls through to the queue when the current race is not among the races', () => {
    expect(defaultHeat(eventView({ phases, current_race: 'gone#1', queue: ['q-2'] }))).toBe('q-2');
  });
});

describe('phase ids', () => {
  it('slugify keeps letters and digits and joins the rest with dashes', () => {
    expect(slugify('  Friday Quali — Heat #2! ')).toBe('friday-quali-heat-2');
    expect(slugify('Élan')).toBe('elan');
    expect(slugify('!!!')).toBe('');
  });

  it('newPhaseId is the slug plus four random characters', () => {
    expect(newPhaseId('Heat 1', () => 0)).toBe('heat-1-0000');
    expect(newPhaseId('Heat 1', () => 0.9999999)).toBe('heat-1-zzzz');
    expect(newPhaseId('!!!', () => 0)).toBe('phase-0000');
  });

  it('two heats with the same name get different ids', () => {
    expect(newPhaseId('Heat 1')).not.toBe(newPhaseId('Heat 1'));
  });
});
