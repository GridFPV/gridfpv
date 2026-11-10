/** The snapshot helpers behind the Event page and the Run view (`lib/phases.ts`). */
import { describe, expect, it } from 'vitest';
import {
  currentRace,
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
