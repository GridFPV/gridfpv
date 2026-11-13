/** The event page's ruling helpers: proposals, positions, the queue and class standings. */
import { describe, expect, it } from 'vitest';
import { buildNames } from '../src/lib/names.js';
import {
  classesToAdd,
  countablePhases,
  hasRanking,
  moveInQueue,
  openProposals,
  positionChoices,
  proposalSentence,
  rosterPrefill,
  standingRows
} from '../src/lib/rulings.js';
import { eventView, group, heat } from './competition.js';

const PILOTS = [
  { id: 'p-1a', callsign: 'Ace', vtx_types: [] },
  { id: 'p-2b', callsign: 'Bee', vtx_types: [] }
];
const placed = (pilot: string, position: number, over = {}) => ({
  pilot,
  position,
  disqualified: false,
  position_set: false,
  flew: false,
  wins: 0,
  points: 0,
  laps: 0,
  ...over
});
const phases = [
  group('finals', 'Finals', undefined, {
    ranking: [placed('p-1a', 1), placed('p-2b', 2, { disqualified: true })]
  }),
  heat('finals/h1', 'Final Heat 1', ['p-1a', 'p-2b'], { parent: 'finals' })
];
const names = buildNames(phases, PILOTS);

describe('proposed disqualifications', () => {
  it('says who, how many, where and the limit, by name', () => {
    expect(
      proposalSentence(names, { phase: 'finals', pilot: 'p-1a', false_starts: 2, limit: 2 })
    ).toBe('Ace has 2 false starts in Finals. The limit is 2.');
    expect(
      proposalSentence(names, { phase: 'finals', pilot: 'p-2b', false_starts: 1, limit: 1 })
    ).toBe('Bee has 1 false start in Finals. The limit is 1.');
  });

  it('leaves out a pilot who is already disqualified from the phase', () => {
    const view = eventView({
      phases,
      proposed_disqualifications: [
        { phase: 'finals', pilot: 'p-1a', false_starts: 2, limit: 2 },
        { phase: 'finals', pilot: 'p-2b', false_starts: 2, limit: 2 }
      ]
    });
    expect(openProposals(view).map((p) => p.pilot)).toEqual(['p-1a']);
  });
});

describe('heat positions', () => {
  it('offers 1 to the size of the ranking', () => {
    expect(positionChoices(phases[0])).toEqual([1, 2]);
    expect(positionChoices(phases[1])).toEqual([]);
  });

  it('shows a ranking once someone flew or a ruling touched it', () => {
    expect(hasRanking(phases[0])).toBe(true);
    expect(hasRanking(phases[1])).toBe(false);
    expect(hasRanking(group('g', 'G', undefined, { ranking: [placed('p-1a', 1)] }))).toBe(false);
    expect(
      hasRanking(group('g', 'G', undefined, { ranking: [placed('p-1a', 1, { flew: true })] }))
    ).toBe(true);
  });
});

describe('the run queue', () => {
  it('moves a heat one place and hands back the whole new order', () => {
    expect(moveInQueue(['a', 'b', 'c'], 1, -1)).toEqual(['b', 'a', 'c']);
    expect(moveInQueue(['a', 'b', 'c'], 1, 1)).toEqual(['a', 'c', 'b']);
  });

  it('leaves the order alone at either end', () => {
    expect(moveInQueue(['a', 'b'], 0, -1)).toEqual(['a', 'b']);
    expect(moveInQueue(['a', 'b'], 1, 1)).toEqual(['a', 'b']);
    expect(moveInQueue(['a', 'b'], 5, -1)).toEqual(['a', 'b']);
  });
});

describe('class standings', () => {
  it('turns every id into a name, and leaves an unplaced pilot without a phase', () => {
    expect(
      standingRows(
        {
          class: 'open',
          pilots: ['p-1a', 'p-2b'],
          counted_phases: ['finals'],
          standings: [
            { pilot: 'p-2b', position: 1, placed_by: 'finals' },
            { pilot: 'p-1a', position: 2 }
          ]
        },
        names
      )
    ).toEqual([
      { position: 1, callsign: 'Bee', placedBy: 'Finals' },
      { position: 2, callsign: 'Ace', placedBy: undefined }
    ]);
  });

  it('offers ranked phases to count, and never an unranked heat', () => {
    const practice = heat('practice', 'Practice', [], {
      format: {
        name: 'Open practice',
        race: { end: 'Open', staging_micros: 0, first_lap: 'HoleShot' },
        completion: 'DirectorDecides'
      }
    });
    expect(countablePhases([...phases, practice]).map((p) => p.def.id)).toEqual([
      'finals',
      'finals/h1'
    ]);
  });

  it('offers the classes that are not in the event and not hidden', () => {
    const classes = [
      { id: 'open', name: 'Open' },
      { id: 'spec', name: 'Spec' },
      { id: 'old', name: 'Old', hidden: true }
    ];
    const rosters = [{ class: 'open', pilots: [], counted_phases: [], standings: [] }];
    expect(classesToAdd(classes, rosters).map((c) => c.id)).toEqual(['spec']);
  });

  it('prefills a picker from a roster, cut to the most it can hold', () => {
    const roster = { class: 'open', pilots: ['a', 'b', 'c'], counted_phases: [], standings: [] };
    expect(rosterPrefill(roster)).toEqual(['a', 'b', 'c']);
    expect(rosterPrefill(roster, 2)).toEqual(['a', 'b']);
  });
});
