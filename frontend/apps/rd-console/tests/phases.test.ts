/** The snapshot helpers behind the Event page and the Run view (`lib/phases.ts`). */
import { describe, expect, it } from 'vitest';
import type { Format } from '@gridfpv/types';
import {
  currentRace,
  defaultHeat,
  groupOfHeat,
  heatGroups,
  holderChoices,
  isBye,
  isHeat,
  mainName,
  mainsNeeded,
  newPhaseId,
  phaseStatus,
  phaseSummaries,
  phaseTree,
  pilotsInLineup,
  racesOfHeat,
  shownSummaries,
  slugify,
  TOP_LEVEL_HEATS
} from '../src/lib/phases.js';
import { TWO_MINUTE_QUALI as TWO_MINUTE, eventView, group, heat, race } from './competition.js';

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

  it('calls a phase ready when any heat beneath it can be flown, whatever its field', () => {
    // Finals the way a director builds them by hand: the Director says the phase is
    // "waiting" because the final's seats are not decided, but the semi-finals can fly.
    const finals = group('f', 'Finals', undefined, { waiting: true });
    const semis = group('f/s', 'Semis', 'f');
    const last = group('f/l', 'Last', 'f', { waiting: true });
    const phases = (semiDone: boolean) => [
      finals,
      semis,
      heat('f/s/1', 'Semi 1', ['a'], { parent: 'f/s', complete: semiDone }),
      last,
      heat('f/l/1', 'Final', [], { parent: 'f/l', waiting: true })
    ];
    const open = eventView({ phases: phases(false) });
    expect(phaseStatus(finals, open)).toBe('ready');
    expect(phaseStatus(semis, open)).toBe('ready');
    // Nothing under the last round can fly yet.
    expect(phaseStatus(last, open)).toBe('waiting');
    // With the semi flown and the final still waiting, nothing is left to fly right now.
    expect(phaseStatus(finals, eventView({ phases: phases(true) }))).toBe('waiting');
  });

  it('does not count a locked heat as one to fly', () => {
    const holder = group('g', 'Group');
    const view = eventView({
      phases: [holder, heat('g/1', 'Heat', ['a'], { parent: 'g', locked: true })]
    });
    expect(phaseStatus(holder, view)).toBe('waiting');
  });

  it('with no heats beneath it, a phase is as the snapshot says', () => {
    const view = eventView();
    expect(phaseStatus(group('g', 'Group'), view)).toBe('ready');
    expect(phaseStatus(group('g', 'Group', undefined, { waiting: true }), view)).toBe('waiting');
  });
});

describe('a bracket in the tree', () => {
  // The Director keeps a bracket's rounds latest first.
  const bracket = (hint: string | undefined) => {
    const top = group('b', 'Bracket');
    top.def.display_hint = hint;
    return [
      group('q', 'Qualifying'),
      top,
      group('b/final', 'Final', 'b'),
      heat('b/final/1', 'Final', [], { parent: 'b/final' }),
      group('b/r2', 'Round 2', 'b'),
      heat('b/r2/1', 'Round 2 Heat 1', [], { parent: 'b/r2' }),
      heat('b/r2/2', 'Round 2 Heat 2', [], { parent: 'b/r2' }),
      group('b/r1', 'Round 1', 'b'),
      heat('b/r1/1', 'Round 1 Heat 1', [], { parent: 'b/r1' })
    ];
  };
  const order = (hint: string | undefined) =>
    phaseTree(bracket(hint)).map((row) => row.phase.def.id);

  it('lays its rounds out in the order they are flown, first round first', () => {
    for (const hint of ['single-elimination', 'double-elimination']) {
      expect(order(hint)).toEqual([
        'q',
        'b',
        'b/r1',
        'b/r1/1',
        'b/r2',
        // The heats inside a round keep their order.
        'b/r2/1',
        'b/r2/2',
        'b/final',
        'b/final/1'
      ]);
    }
  });

  it('leaves any other phase in the order it was added', () => {
    for (const hint of [undefined, 'points-table']) {
      expect(order(hint).slice(2, 4)).toEqual(['b/final', 'b/final/1']);
    }
  });

  it('lays mains out bottom main first, the order they are flown', () => {
    // The Director keeps the mains best first, each one a bracket of its own.
    const mains = group('m', 'Mains');
    mains.def.display_hint = 'mains';
    const a = group('m/a', 'A-Main', 'm');
    const b = group('m/b', 'B-Main', 'm');
    a.def.display_hint = b.def.display_hint = 'double-elimination';
    const tree = phaseTree([
      mains,
      a,
      group('m/a/final', 'Final', 'm/a'),
      group('m/a/r1', 'Round 1', 'm/a'),
      b,
      group('m/b/final', 'Final', 'm/b')
    ]);
    expect(tree.map((row) => row.phase.def.id)).toEqual([
      'm',
      'm/b',
      'm/b/final',
      'm/a',
      'm/a/r1',
      'm/a/final'
    ]);
  });

  it('gives the phase list and the pickers the same order', () => {
    const phases = bracket('double-elimination');
    expect(
      phaseSummaries(eventView({ phases }))
        .filter((phase) => phase.parent === 'b')
        .map((phase) => phase.name)
    ).toEqual(['Round 1', 'Round 2', 'Final']);
    expect(heatGroups(phases).map((entry) => entry.label)).toEqual([
      'Bracket › Round 1',
      'Bracket › Round 2',
      'Bracket › Final'
    ]);
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

describe('defaultHeat and the heat on the timer', () => {
  const three = (over: Parameters<typeof eventView>[0] = {}) =>
    eventView({
      phases: [heat('a', 'A', []), heat('b', 'B', []), heat('c', 'C', []), group('g', 'G')],
      queue: ['a', 'b', 'c'],
      ...over
    });

  it('is the heat the Director has on the timer, ahead of the run order', () => {
    expect(defaultHeat(three({ on_timer: 'c' }))).toBe('c');
    // Even when that heat is done and no longer in the run order.
    expect(defaultHeat(three({ on_timer: 'c', queue: ['a'] }))).toBe('c');
  });

  it('is the heat of a race that is on, whatever else is said', () => {
    const on = race('b', 1, 'Running');
    expect(defaultHeat(three({ on_timer: 'c', races: [on], current_race: on.race }))).toBe('b');
  });

  it('falls back to the run order when the heat on the timer is gone or is not a heat', () => {
    expect(defaultHeat(three({ on_timer: 'gone' }))).toBe('a');
    expect(defaultHeat(three({ on_timer: 'g' }))).toBe('a');
    expect(defaultHeat(three())).toBe('a');
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

describe('phaseTree for one phase', () => {
  const phases = [
    group('quali', 'Qualifying'),
    heat('q-1', 'Q1', [], { parent: 'quali' }),
    group('finals', 'Finals'),
    group('finals/w', 'Winners', 'finals'),
    heat('finals/w/h1', 'W1', [], { parent: 'finals/w' }),
    heat('finals/f', 'Final', [], { parent: 'finals' }),
    heat('solo', 'Solo', [])
  ];
  const ids = (root: string) => phaseTree(phases, root).map((row) => [row.phase.def.id, row.depth]);

  it('is that phase at the top with everything beneath it, and nothing beside it', () => {
    expect(ids('finals')).toEqual([
      ['finals', 0],
      ['finals/w', 1],
      ['finals/w/h1', 2],
      ['finals/f', 1]
    ]);
    expect(ids('quali')).toEqual([
      ['quali', 0],
      ['q-1', 1]
    ]);
  });

  it('starts the depth again from a phase in the middle of the tree', () => {
    expect(ids('finals/w')).toEqual([
      ['finals/w', 0],
      ['finals/w/h1', 1]
    ]);
  });

  it('a heat is itself alone, and a phase that is not there is nothing', () => {
    expect(ids('solo')).toEqual([['solo', 0]]);
    expect(ids('finals/f')).toEqual([['finals/f', 0]]);
    expect(ids('gone')).toEqual([]);
  });

  it('the last phase of the event takes everything to the end, and no more', () => {
    const tail = [group('a', 'A'), group('b', 'B'), heat('b1', 'B1', [], { parent: 'b' })];
    expect(phaseTree(tail, 'b').map((row) => row.phase.def.id)).toEqual(['b', 'b1']);
    expect(phaseTree(tail, 'a').map((row) => row.phase.def.id)).toEqual(['a']);
  });
});

describe('phaseSummaries', () => {
  it('is empty for an event with no phases, or with only heats on their own', () => {
    expect(phaseSummaries(eventView())).toEqual([]);
    expect(phaseSummaries(eventView({ phases: [heat('h', 'Heat', [])] }))).toEqual([]);
  });

  it('lists every phase that holds something, a phase inside another right after it, each with its heats counted', () => {
    const view = eventView({
      phases: [
        group('quali', 'Qualifying', undefined, { format: TWO_MINUTE }),
        heat('q-1', 'Q1', [], { parent: 'quali', complete: true }),
        heat('q-2', 'Q2', [], { parent: 'quali' }),
        heat('q-3', 'Q3', [], { parent: 'quali' }),
        heat('solo', 'Solo', []),
        group('finals', 'Finals'),
        group('finals/w', 'Winners', 'finals'),
        heat('finals/w/h1', 'W1', [], { parent: 'finals/w' }),
        heat('finals/f', 'Final', [], { parent: 'finals', waiting: true }),
        group('empty', 'Empty')
      ],
      // A saved race counts as flown even before the heat is complete. A thrown-out one
      // does not, and neither does a race still on.
      races: [
        race('q-2', 1, 'Saved'),
        race('q-3', 1, 'Discarded'),
        race('finals/w/h1', 1, 'Running')
      ]
    });
    const found = phaseSummaries(view);
    expect(found.map((phase) => [phase.name, phase.heats, phase.flown])).toEqual([
      ['Qualifying', 3, 2],
      ['Finals', 2, 0],
      ['Winners', 1, 0],
      ['Empty', 0, 0]
    ]);
    expect(found[0].format).toBe(TWO_MINUTE.name);
    expect(found[1].format).toBeUndefined();
    // How deep each sits, what holds it, and how many phases are directly inside.
    expect(found.map((phase) => [phase.depth, phase.parent, phase.inside])).toEqual([
      [0, undefined, 0],
      [0, undefined, 1],
      [1, 'finals', 0],
      [0, undefined, 0]
    ]);
  });

  it('lays a phase whose parent is gone at the top, with no parent', () => {
    const found = phaseSummaries(eventView({ phases: [group('lost', 'Lost', 'gone')] }));
    expect(found.map((phase) => [phase.depth, phase.parent])).toEqual([[0, undefined]]);
  });

  it('shows a phase inside another only while every phase above it is unfolded', () => {
    const found = phaseSummaries(
      eventView({
        phases: [
          group('a', 'A'),
          group('a/b', 'B', 'a'),
          group('a/b/c', 'C', 'a/b'),
          group('d', 'D')
        ]
      })
    );
    const shown = (...open: string[]) =>
      shownSummaries(found, new Set(open)).map((phase) => phase.name);
    expect(shown()).toEqual(['A', 'D']);
    expect(shown('a')).toEqual(['A', 'B', 'D']);
    expect(shown('a', 'a/b')).toEqual(['A', 'B', 'C', 'D']);
    // B is unfolded but A is not, so nothing under A shows.
    expect(shown('a/b')).toEqual(['A', 'D']);
  });

  it('says where each phase stands', () => {
    const on = race('q-1', 1, 'Running');
    const view = eventView({
      phases: [
        group('quali', 'Qualifying'),
        heat('q-1', 'Q1', [], { parent: 'quali' }),
        group('done', 'Done', undefined, { complete: true }),
        group('shut', 'Shut', undefined, { locked: true }),
        group('later', 'Later', undefined, { waiting: true })
      ],
      races: [on],
      current_race: on.race
    });
    expect(phaseSummaries(view).map((phase) => phase.status)).toEqual([
      'racing',
      'complete',
      'locked',
      'waiting'
    ]);
  });

  it('says whether pilots sign up for it, and whether that is open', () => {
    const rules = { seats: [], open_heats: 1, rest_heats: 0, open: true };
    const sheet = (open: boolean) => ({ rules: { ...rules, open }, heats: [], pilots: [] });
    const view = eventView({
      phases: [
        group('a', 'A', undefined, { signup: sheet(true) }),
        group('b', 'B', undefined, { signup: sheet(false) }),
        group('c', 'C')
      ]
    });
    expect(phaseSummaries(view).map((phase) => phase.signup)).toEqual([
      'open',
      'closed',
      undefined
    ]);
  });

  it('names the heat on the timer on the phase that holds it, at any depth, and on no other', () => {
    const phases = [
      group('quali', 'Qualifying'),
      heat('q-1', 'Q1', [], { parent: 'quali' }),
      group('finals', 'Finals'),
      group('finals/w', 'Winners', 'finals'),
      heat('finals/w/h1', 'W1', [], { parent: 'finals/w' }),
      heat('solo', 'Solo', [])
    ];
    const at = (onTimer?: string) =>
      phaseSummaries(eventView({ phases, on_timer: onTimer })).map((phase) => phase.onTimer);
    // Qualifying, Finals, and Winners inside Finals.
    expect(at('finals/w/h1')).toEqual([undefined, 'finals/w/h1', 'finals/w/h1']);
    expect(at('q-1')).toEqual(['q-1', undefined, undefined]);
    // A heat on its own, a heat that is gone, and nothing at all.
    expect(at('solo')).toEqual([undefined, undefined, undefined]);
    expect(at('gone')).toEqual([undefined, undefined, undefined]);
    expect(at()).toEqual([undefined, undefined, undefined]);
  });

  it('flags a phase where a pilot waits on a decision, until they are disqualified', () => {
    const phases = (disqualified: boolean) => [
      group('quali', 'Qualifying'),
      heat('q-1', 'Q1', [], {
        parent: 'quali',
        ranking: [
          {
            pilot: 'p-1',
            position: 1,
            disqualified,
            position_set: false,
            flew: true,
            wins: 0,
            points: 0,
            laps: 1
          }
        ]
      }),
      group('finals', 'Finals')
    ];
    const proposal = { phase: 'q-1', pilot: 'p-1', false_starts: 2, limit: 2 };
    const flags = (disqualified: boolean) =>
      phaseSummaries(
        eventView({ phases: phases(disqualified), proposed_disqualifications: [proposal] })
      ).map((phase) => phase.needsDecision);
    expect(flags(false)).toEqual([true, false]);
    expect(flags(true)).toEqual([false, false]);
  });
});

describe('how many mains a field takes', () => {
  it('matches the Director: the top main is full, each main below adds the pilots who do not bump', () => {
    expect(mainsNeeded(64, 16, 4)).toBe(5);
    expect(mainsNeeded(96, 16, 2)).toBe(7);
    expect(mainsNeeded(8, 6, 2)).toBe(2);
    expect(mainsNeeded(16, 16, 4)).toBe(1);
    expect(mainsNeeded(3, 16, 2)).toBe(1);
    expect(mainsNeeded(17, 16, 0)).toBe(2);
  });

  it('names them as the Director does', () => {
    expect([0, 1, 25, 26].map(mainName)).toEqual(['A-Main', 'B-Main', 'Z-Main', 'Main 27']);
  });
});
