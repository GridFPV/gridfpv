/**
 * The helpers behind the spectator's results picker: phase first, then overall or a heat.
 */
import { describe, expect, it } from 'vitest';
import type { EventView } from '@gridfpv/types';
import { buildNames } from '../src/lib/names.js';
import { findRace, heatChoices, resultPhases } from '../src/lib/results.js';
import { TWO_MINUTE_QUALI, eventView, group, heat, placed, race } from './competition.js';

const ranked = { format: TWO_MINUTE_QUALI, ranking: [placed('p-a', 1)] };

function event(): EventView {
  return eventView({
    phases: [
      group('quali', 'Qualifying', undefined, ranked),
      heat('q1', 'Qualifying Heat 1', ['p-a'], { parent: 'quali', ...ranked }),
      heat('q2', 'Qualifying Heat 2', ['p-a'], { parent: 'quali', ...ranked }),
      // A group with no ranking of its own: its heats stand for themselves.
      group('fun', 'Fun runs'),
      heat('f1', 'Fun Heat', ['p-a'], { parent: 'fun', ...ranked }),
      heat('solo', 'Shootout', ['p-a'], ranked),
      heat('later', 'Not flown', ['p-a'])
    ],
    races: [
      race('q2', 1, 'Saved'),
      race('q1', 1, 'Discarded'),
      race('q1', 2, 'Saved'),
      race('q2', 2, 'Saved'),
      race('solo', 1, 'Stopped'),
      race('f1', 1, 'Saved')
    ]
  });
}
const names = (view: EventView) => buildNames(view.phases, [], []);

describe('resultPhases', () => {
  it('leaves out a heat whose phase has results of its own, and keeps the rest', () => {
    expect(resultPhases(event()).map((choice) => choice.label)).toEqual([
      'Qualifying',
      'Fun runs › Fun Heat',
      'Shootout'
    ]);
  });

  it('is empty for an event with no results', () => {
    expect(resultPhases(eventView({ phases: [heat('h', 'Heat', ['p-a'])] }))).toEqual([]);
  });
});

describe('heatChoices', () => {
  it('lists the heats with a saved race, in the order they were first raced', () => {
    const view = event();
    const heats = heatChoices(view, 'quali', names(view));
    expect(heats.map((entry) => entry.label)).toEqual(['Qualifying Heat 2', 'Qualifying Heat 1']);
  });

  it("keeps each race's number from its heat, counting the ones thrown out", () => {
    const view = event();
    const heats = heatChoices(view, 'quali', names(view));
    expect(heats.map((entry) => entry.races.map((r) => r.number))).toEqual([[1, 2], [2]]);
    expect(heats[1].races[0].name).toBe('Qualifying Heat 1 race 2');
  });

  it('a heat picked as the phase lists itself, and one with nothing saved lists nothing', () => {
    const view = event();
    expect(heatChoices(view, 'f1', names(view)).map((entry) => entry.id)).toEqual(['f1']);
    expect(heatChoices(view, 'solo', names(view))).toEqual([]);
    expect(heatChoices(view, 'later', names(view))).toEqual([]);
    expect(heatChoices(view, 'no-such-phase', names(view))).toEqual([]);
  });
});

describe('findRace', () => {
  it('finds a race by its id with the name and number every page uses', () => {
    const view = event();
    const found = findRace(view, view.races[2].race, names(view));
    expect(found?.name).toBe('Qualifying Heat 1 race 2');
    expect(found?.number).toBe(2);
    expect(found?.heat).toBe('q1');
  });

  it('is undefined for a race that is not there', () => {
    const view = event();
    expect(findRace(view, 'nope#1', names(view))).toBeUndefined();
  });
});
