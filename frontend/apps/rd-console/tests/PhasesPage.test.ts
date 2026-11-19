/**
 * The Phases page: the empty state, "Up next", the phase tree, and the dialogs that add a
 * heat, a phase and a bracket. Rendered against a stand-in live store. The rulings made in
 * the tree are in `PhaseRulings.test.ts`.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type {
  BracketRequest,
  Class,
  Command,
  EventView,
  MainsRequest,
  RoundRobinRequest,
  PhaseDef,
  Pilot
} from '@gridfpv/types';
import PhasePage from '../src/screens/PhasePage.svelte';
import PhasesPage from '../src/screens/PhasesPage.svelte';
import { unfolded } from '../src/lib/phaseFold.svelte.js';
import type { LiveSession } from '../src/lib/eventLive.svelte.js';
import {
  eventView,
  FIRST_TO_3,
  group,
  heat,
  OPEN_PRACTICE,
  race,
  SAVED_FORMATS,
  timerView,
  TWO_MINUTE_QUALI
} from './competition.js';
import { GATE, SIGNUP_PILOTS, signupEvent, signupSheet } from './signupFixtures.js';
import { openLive } from './support.js';

const PILOTS: Pilot[] = [
  { id: 'p-1a', callsign: 'Ace', vtx_types: [] },
  { id: 'p-2b', callsign: 'Bee', vtx_types: [] },
  { id: 'p-3c', callsign: 'Cat', vtx_types: [] }
];
const CLASSES: Class[] = [
  { id: 'cls-open', name: 'Open' },
  { id: 'cls-spec', name: 'Spec' }
];

async function open(view: EventView, over: Partial<LiveSession> = {}) {
  const { session, live } = await openLive(view, {
    listPilots: vi.fn(async () => PILOTS),
    listClasses: vi.fn(async () => CLASSES),
    ...over
  });
  const onrun = vi.fn();
  render(PhasesPage, { live, onrun });
  return { session, onrun };
}

/** One phase's own page, which is where its heats, rulings and sign-up are. */
async function openPhase(view: EventView, phase: string, over: Partial<LiveSession> = {}) {
  const { session, live } = await openLive(view, {
    listPilots: vi.fn(async () => PILOTS),
    listClasses: vi.fn(async () => CLASSES),
    ...over
  });
  const onrun = vi.fn();
  render(PhasePage, { live, phase, onrun });
  return { session, onrun };
}

/** A phase to put heats in: a heat is always in one. It sets no format of its own. */
const RACES = group('races', 'Races');

const sent = (session: LiveSession): Command[] =>
  vi.mocked(session.sendCommand).mock.calls.map((call) => call[1]);

/** The phase a `PhaseDefined` command defines, with its random id checked and taken off. */
function definedPhase(command: Command, slug: string): Omit<PhaseDef, 'id'> {
  const { id, ...def } = (command as { Record: { PhaseDefined: { def: PhaseDef } } }).Record
    .PhaseDefined.def;
  expect(id).toMatch(new RegExp(`^${slug}-[0-9a-z]{4}$`));
  return def;
}

const optionTexts = (select: HTMLElement) =>
  [...(select as HTMLSelectElement).options].map((option) => option.textContent?.trim());

const phaseRows = (): HTMLElement[] =>
  within(screen.getByRole('list', { name: /^(Phases|Heats of .+)$/ }))
    .getAllByRole('listitem')
    .filter((item) => item.classList.contains('phase'));

/** Every raw handle in these fixtures. None may be on the page. */
function expectNoRawIds() {
  const text = document.body.textContent ?? '';
  for (const raw of ['finals', 'q-1', 'q-2', '#1', 'bench', 'seat-1', 'p-1a', 'p-2b', 'fmt-']) {
    expect(text).not.toContain(raw);
  }
}

describe('PhasesPage', () => {
  it('says so when there is nothing to race yet, and offers a phase and the quick practice phase', async () => {
    await open(eventView());
    expect(screen.getByTestId('no-phases')).toHaveTextContent(/^Nothing to race yet\./);
    expect(screen.queryByRole('list', { name: 'Phases' })).toBeNull();
    expect(screen.queryByRole('heading', { name: 'Up next' })).toBeNull();
    // A heat is always in a phase, so there is no "+ Add heat" at the top: a phase comes
    // first, and the quick way to one is offered right here.
    expect(screen.queryByRole('button', { name: '+ Add heat' })).toBeNull();
    for (const name of ['+ Add phase', 'Add a practice phase']) {
      expect(screen.getByRole('button', { name })).toBeEnabled();
    }
    expect(screen.getByRole('link', { name: 'Help for Phases' })).toHaveAttribute(
      'href',
      '#/help/phases'
    );
  });

  it('says so when the event cannot be read', async () => {
    await open(eventView(), {
      getEventState: vi.fn(async () => Promise.reject(new Error('The Director is not answering.')))
    });
    expect(screen.getByRole('alert')).toHaveTextContent('The Director is not answering.');
    expect(screen.queryByTestId('no-phases')).toBeNull();
  });

  it("shows a phase's own heats with lineups by name, and the phases inside it as lines to open", async () => {
    const running = race('finals/w1/h1', 1, 'Running');
    const view = eventView({
      phases: [
        group('finals', 'Finals'),
        group('finals/w1', 'Winners Round 1', 'finals'),
        heat('finals/w1/h1', 'Winners Round 1 Heat 1', ['p-1a', 'p-2b'], { parent: 'finals/w1' }),
        heat(
          'finals/final',
          'Final',
          [
            { Waiting: { phase: 'finals/w1/h1', position: 1 } },
            { Waiting: { phase: 'finals/w1/h1', position: 2 } }
          ],
          { parent: 'finals', waiting: true }
        ),
        heat('quali-1', 'Qualifying', ['p-3c'], { complete: true })
      ],
      races: [running],
      current_race: running.race
    });
    await openPhase(view, 'finals');
    // Only the heats directly in Finals. The phase is not a row, and neither is the round
    // inside it or that round's heat.
    let rows = phaseRows();
    expect(rows.map((row) => row.querySelector('.label')?.textContent)).toEqual(['Final']);
    expect(rows[0]).toHaveTextContent('Waiting');
    expect(screen.queryByRole('list', { name: 'Lineup of Qualifying' })).toBeNull();
    expect(screen.queryByRole('list', { name: 'Lineup of Winners Round 1 Heat 1' })).toBeNull();
    const final = screen.getByRole('list', { name: 'Lineup of Final' });
    expect(final).toHaveTextContent('Winner of Winners Round 1 Heat 1');
    expect(final).toHaveTextContent('2nd of Winners Round 1 Heat 1');
    // A waiting heat cannot be run.
    expect(screen.queryByRole('button', { name: 'Run Final' })).toBeNull();

    // The round inside is a line in its own list, and it opens the round's page.
    const inside = within(screen.getByRole('list', { name: 'Phases in Finals' }));
    const line = inside.getByRole('link', { name: 'Open Winners Round 1' });
    expect(line).toHaveAttribute('href', '#/event/phases/finals%2Fw1');
    expect(line).toHaveTextContent('Racing');
    expect(line).toHaveTextContent('1 heat · 0 flown');
    expect(inside.getAllByRole('listitem')).toHaveLength(1);

    // The round's page has its heat, and a way back to Finals.
    cleanup();
    const { onrun } = await openPhase(view, 'finals/w1');
    rows = phaseRows();
    expect(rows.map((row) => row.querySelector('.label')?.textContent)).toEqual([
      'Winners Round 1 Heat 1'
    ]);
    expect(screen.queryByRole('list', { name: /^Phases in / })).toBeNull();
    const lineup = screen.getByRole('list', { name: 'Lineup of Winners Round 1 Heat 1' });
    expect(lineup).toHaveTextContent('Seat 1');
    expect(lineup).toHaveTextContent('Ace');
    expect(lineup).toHaveTextContent('Bee');
    expect(screen.getByTestId('back-to-parent')).toHaveAttribute('href', '#/event/phases/finals');
    expect(screen.getByTestId('back-to-parent')).toHaveTextContent('Finals');
    expect(screen.getByTestId('back-to-phases')).toHaveAttribute('href', '#/event/phases');
    await fireEvent.click(screen.getByRole('button', { name: 'Run Winners Round 1 Heat 1' }));
    expect(onrun).toHaveBeenCalledWith('finals/w1/h1');

    const text = document.body.textContent ?? '';
    for (const raw of ['finals/', 'quali-1', '#1', 'bench', 'seat-1', 'p-1a']) {
      expect(text).not.toContain(raw);
    }
  });

  it('names the format in force on each heat, and says when it comes from the phase above', async () => {
    const quali = group('quali', 'Qualifying', undefined, { format: TWO_MINUTE_QUALI });
    quali.def.format = TWO_MINUTE_QUALI;
    const own = heat('q-1', 'Qualifying Heat 1', ['p-1a'], { parent: 'quali', format: FIRST_TO_3 });
    own.def.format = FIRST_TO_3;
    const follows = heat('q-2', 'Qualifying Heat 2', ['p-2b'], {
      parent: 'quali',
      format: TWO_MINUTE_QUALI
    });
    const view = eventView({
      phases: [quali, own, follows, group('finals', 'Finals', undefined, {})]
    });
    await openPhase(view, 'quali');
    const format = (row: HTMLElement) => row.querySelector('.format');
    const [ownRow, followsRow] = phaseRows();
    // The phase's own format is under the page title, not on a row.
    expect(screen.getByTestId('phase-where')).toHaveTextContent('2 minute qualifying');
    expect(format(ownRow)).toHaveTextContent('First to 3 laps');
    expect(format(ownRow)).not.toHaveAttribute('title');
    expect(format(followsRow)).toHaveTextContent('2 minute qualifying');
    expect(format(followsRow)).toHaveAttribute('title', 'From the phase above');
    // A phase with no format in force names none, and one with no heats says so.
    cleanup();
    await openPhase(view, 'finals');
    expect(screen.getByTestId('phase-where')).toHaveTextContent(/^\s*Ready\s*$/);
    expect(screen.getByTestId('no-heats')).toHaveTextContent('No heats in this phase yet.');
    expect(screen.queryAllByRole('listitem')).toHaveLength(0);
  });

  it("names a heat's seats from its timer in the phase list, even when it is not connected", async () => {
    const OFF = timerView('far-gate-9zz0', 'Far Gate', 0, {
      connected: false,
      problem: 'Nothing answered at 10.0.0.9:7270.'
    });
    const onGate = heat('h-1', 'Heat 1', ['p-1a', 'p-2b']);
    onGate.lineup = [
      { adapter: 'far-gate-9zz0', seat: '2', who: { Pilot: 'p-1a' } },
      { adapter: 'far-gate-9zz0', seat: '0', who: { Pilot: 'p-2b' } }
    ];
    await open(eventView({ phases: [onGate] }), { listTimers: vi.fn(async () => [OFF]) });
    const items = within(screen.getByRole('list', { name: 'Lineup of Heat 1' })).getAllByRole(
      'listitem'
    );
    expect(items.map((item) => item.textContent?.replace(/\s+/g, ' ').trim())).toEqual([
      'Seat 3 Ace',
      'Seat 1 Bee'
    ]);
    expect(document.body.textContent).not.toContain('far-gate-9zz0');
  });
});

describe('Up next', () => {
  const view = eventView({
    phases: [
      heat('q-1', 'Qualifying Heat 1', ['p-1a', 'p-2b']),
      heat('q-2', 'Qualifying Heat 2', ['p-3c', 'Open']),
      heat('q-3', 'Qualifying Heat 3', ['p-2b'], { complete: true })
    ],
    queue: ['q-1', 'q-2']
  });

  it('lists the queue in order with each lineup by callsign, and says the order is a suggestion', async () => {
    const { onrun } = await open(view);
    expect(screen.getByText(/The order is a suggestion\./)).toBeVisible();
    const items = within(screen.getByRole('list', { name: 'Up next' })).getAllByRole('listitem');
    expect(items).toHaveLength(2);
    expect(items[0]).toHaveTextContent('Qualifying Heat 1');
    expect(items[0]).toHaveTextContent('Ace, Bee');
    expect(items[1]).toHaveTextContent('Cat, Seat 2');
    await fireEvent.click(screen.getByRole('button', { name: 'Run Qualifying Heat 2 next' }));
    expect(onrun).toHaveBeenCalledWith('q-2');
    expectNoRawIds();
  });

  it('says which phase a heat is in when its name does not', async () => {
    await open(
      eventView({
        phases: [
          group('sport', 'Sport'),
          group('sport/semis', 'Semis', 'sport'),
          heat('sport/semis/1', 'Semi 1', ['p-1a'], { parent: 'sport/semis' }),
          group('practice', 'Practice'),
          heat('practice/1', 'Practice Heat 1', ['p-2b'], { parent: 'practice' })
        ],
        queue: ['sport/semis/1', 'practice/1']
      })
    );
    const items = within(screen.getByRole('list', { name: 'Up next' })).getAllByRole('listitem');
    expect(items[0].querySelector('.within')).toHaveTextContent('Sport › Semis');
    // "Practice Heat 1" already says Practice.
    expect(items[1].querySelector('.within')).toBeNull();
    expect(document.body.textContent).not.toContain('sport/');
  });

  it('moving a heat sends the whole new order', async () => {
    const { session } = await open(view);
    expect(screen.getByRole('button', { name: 'Move Qualifying Heat 1 up' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Move Qualifying Heat 2 down' })).toBeDisabled();
    await fireEvent.click(screen.getByRole('button', { name: 'Move Qualifying Heat 2 up' }));
    await waitFor(() =>
      expect(sent(session)).toEqual([{ Record: { QueueSet: { heats: ['q-2', 'q-1'] } } }])
    );
  });

  it('leaves the card out when no heat is ready', async () => {
    await open(eventView({ phases: view.phases, queue: [] }));
    expect(screen.queryByRole('heading', { name: 'Up next' })).toBeNull();
    expect(screen.queryByRole('list', { name: 'Up next' })).toBeNull();
    // The tree is still there.
    expect(screen.getByRole('list', { name: 'Phases' })).toBeVisible();
  });
});

describe('Add a heat', () => {
  it('sends PhaseDefined with the pilots in seat order and the chosen saved format', async () => {
    const { session } = await open(eventView({ phases: [RACES] }));
    await fireEvent.click(screen.getByRole('button', { name: 'Add a heat under Races' }));
    const form = await screen.findByRole('form', { name: 'Add a heat' });
    await fireEvent.input(within(form).getByLabelText('Heat name'), {
      target: { value: 'Qualifying Heat 1' }
    });
    // Bee first: Bee gets Seat 1.
    await fireEvent.click(within(form).getByRole('button', { name: 'Bee' }));
    await fireEvent.click(within(form).getByRole('button', { name: 'Ace' }));
    // The saved formats are offered by name, and the one picked is read out in words.
    expect(optionTexts(within(form).getByLabelText('Format'))).toEqual([
      'Open practice',
      'First to 3 laps',
      '2 minute qualifying'
    ]);
    await fireEvent.change(within(form).getByLabelText('Format'), {
      target: { value: 'fmt-first3-7q1x' }
    });
    expect(within(form).getByTestId('format-reads')).toHaveTextContent(
      'First to 3 laps · Ranked by finishing order · One race'
    );
    expect(form.textContent).not.toContain('fmt-');
    // It is in the phase it was added from, and "the top of the event" is not a choice.
    expect(optionTexts(within(form).getByLabelText('Under'))).toEqual(['Races']);
    expect(within(form).getByLabelText('Under')).toHaveValue('races');
    await fireEvent.click(screen.getByRole('button', { name: 'Add heat' }));

    await waitFor(() => expect(session.sendCommand).toHaveBeenCalled());
    expect(definedPhase(sent(session)[0], 'qualifying-heat-1')).toEqual({
      label: 'Qualifying Heat 1',
      kind: {
        Heat: {
          lineup: [
            { adapter: 'bench', seat: 'seat-1', source: { Pilot: 'p-2b' } },
            { adapter: 'bench', seat: 'seat-2', source: { Pilot: 'p-1a' } }
          ]
        }
      },
      parent: 'races',
      // A copy of the saved format, with no handle of the saved one.
      format: FIRST_TO_3
    });
    // It closes once the Director took it.
    await waitFor(() => expect(screen.queryByRole('form', { name: 'Add a heat' })).toBeNull());
  });

  it('starts on the first saved format when its phase sets none', async () => {
    const { session } = await open(eventView({ phases: [RACES] }));
    await fireEvent.click(screen.getByRole('button', { name: 'Add a heat under Races' }));
    const form = await screen.findByRole('form', { name: 'Add a heat' });
    expect(within(form).getByLabelText('Format')).toHaveValue('fmt-practice-9k2d');
    await fireEvent.input(within(form).getByLabelText('Heat name'), { target: { value: 'Fly' } });
    await fireEvent.click(within(form).getByRole('button', { name: 'Ace' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Add heat' }));
    await waitFor(() => expect(session.sendCommand).toHaveBeenCalled());
    expect(definedPhase(sent(session)[0], 'fly').format).toEqual(OPEN_PRACTICE);
  });

  it('asks for a name and a pilot before sending anything', async () => {
    const { session } = await open(eventView({ phases: [RACES] }));
    await fireEvent.click(screen.getByRole('button', { name: 'Add a heat under Races' }));
    const form = await screen.findByRole('form', { name: 'Add a heat' });
    await fireEvent.click(screen.getByRole('button', { name: 'Add heat' }));
    expect(await screen.findByText('Give the heat a name.')).toBeVisible();
    await fireEvent.input(within(form).getByLabelText('Heat name'), { target: { value: 'H1' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add heat' }));
    expect(await within(form).findByRole('alert')).toHaveTextContent('Choose at least one pilot.');
    expect(session.sendCommand).not.toHaveBeenCalled();
  });

  it('asks for a format when none is saved and no phase gives one', async () => {
    const { session } = await open(eventView({ phases: [RACES] }), {
      listFormats: vi.fn(async () => [])
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add a heat under Races' }));
    const form = await screen.findByRole('form', { name: 'Add a heat' });
    expect(within(form).getByTestId('format-reads')).toHaveTextContent('No saved format fits.');
    expect(within(form).getByRole('link', { name: 'Formats' })).toHaveAttribute(
      'href',
      '#/formats'
    );
    await fireEvent.input(within(form).getByLabelText('Heat name'), { target: { value: 'H1' } });
    await fireEvent.click(within(form).getByRole('button', { name: 'Ace' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Add heat' }));
    expect(await within(form).findByRole('alert')).toHaveTextContent(/^Pick a format\./);
    expect(session.sendCommand).not.toHaveBeenCalled();
  });

  it("shows the Director's sentence in the dialog when it says no, and keeps what was typed", async () => {
    const SAID = 'Ace is already flying in another heat.';
    await open(eventView({ phases: [RACES] }), {
      sendCommand: vi.fn(async () =>
        Promise.reject(Object.assign(new Error(SAID), { status: 409, code: 'Refused' }))
      )
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add a heat under Races' }));
    const form = await screen.findByRole('form', { name: 'Add a heat' });
    await fireEvent.input(within(form).getByLabelText('Heat name'), { target: { value: 'H1' } });
    await fireEvent.click(within(form).getByRole('button', { name: 'Ace' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Add heat' }));
    expect(await within(form).findByRole('alert')).toHaveTextContent(SAID);
    expect(within(form).getByLabelText('Heat name')).toHaveValue('H1');
  });

  it('reads the saved formats again when the dialog opens', async () => {
    const { session } = await open(eventView({ phases: [RACES] }));
    const before = vi.mocked(session.listFormats).mock.calls.length;
    await fireEvent.click(screen.getByRole('button', { name: 'Add a heat under Races' }));
    await screen.findByRole('form', { name: 'Add a heat' });
    await waitFor(() =>
      expect(vi.mocked(session.listFormats).mock.calls.length).toBeGreaterThan(before)
    );
  });
});

describe('Under a phase', () => {
  const quali = () => {
    const phase = group('quali', 'Qualifying', undefined, { format: TWO_MINUTE_QUALI });
    phase.def.format = TWO_MINUTE_QUALI;
    return phase;
  };
  const phases = () => [
    quali(),
    group('mains', 'Mains'),
    group('mains/a', 'A-Main', 'mains', { locked: true })
  ];

  it('+ Heat on a phase opens the dialog under it, following its format', async () => {
    const { session } = await open(eventView({ phases: phases() }));
    // A locked phase takes no new heat.
    expect(screen.queryByRole('button', { name: 'Add a heat under A-Main' })).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Add a heat under Qualifying' }));
    const form = await screen.findByRole('form', { name: 'Add a heat' });
    const under = within(form).getByLabelText('Under') as HTMLSelectElement;
    // Every phase it could go in, and never "the top of the event".
    expect(optionTexts(under)).toEqual(['Qualifying', 'Mains', 'Mains › A-Main (locked)']);
    expect(under.options[2].disabled).toBe(true);
    expect(under).toHaveValue('quali');
    // The first choice is the phase's own format, and it is the one picked.
    const format = within(form).getByLabelText('Format') as HTMLSelectElement;
    expect(optionTexts(format)[0]).toBe("The phase's: 2 minute qualifying");
    expect(format).toHaveValue('');
    expect(within(form).getByTestId('format-reads')).toHaveTextContent(
      "The heat follows its phase's format."
    );

    await fireEvent.input(within(form).getByLabelText('Heat name'), {
      target: { value: 'Qualifying Heat 1' }
    });
    await fireEvent.click(within(form).getByRole('button', { name: 'Ace' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Add heat' }));
    await waitFor(() => expect(session.sendCommand).toHaveBeenCalled());
    // Under the phase, and with no format of its own.
    expect(definedPhase(sent(session)[0], 'qualifying-heat-1')).toEqual({
      label: 'Qualifying Heat 1',
      parent: 'quali',
      kind: {
        Heat: { lineup: [{ adapter: 'bench', seat: 'seat-1', source: { Pilot: 'p-1a' } }] }
      }
    });
  });

  it('a heat opened from one phase can be put in another, and never at the top', async () => {
    const { session } = await open(eventView({ phases: phases() }));
    // There is no way to add a heat that is in no phase.
    expect(screen.queryByRole('button', { name: '+ Add heat' })).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Add a heat under Qualifying' }));
    const form = await screen.findByRole('form', { name: 'Add a heat' });
    expect(within(form).getByLabelText('Under')).toHaveValue('quali');
    await fireEvent.change(within(form).getByLabelText('Under'), { target: { value: 'mains' } });
    // Mains sets no format, so a saved one is picked.
    await fireEvent.change(within(form).getByLabelText('Format'), {
      target: { value: 'fmt-first3-7q1x' }
    });
    await fireEvent.input(within(form).getByLabelText('Heat name'), { target: { value: 'B' } });
    await fireEvent.click(within(form).getByRole('button', { name: 'Ace' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Add heat' }));
    await waitFor(() => expect(session.sendCommand).toHaveBeenCalled());
    const def = definedPhase(sent(session)[0], 'b');
    expect(def.parent).toBe('mains');
    expect(def.format).toEqual(FIRST_TO_3);
  });

  it('on an empty event, "Add a practice phase" makes the phase and goes on to its first heat', async () => {
    const made: PhaseDef[] = [];
    const first = eventView();
    let now = first;
    const { session } = await open(first, {
      getEventState: vi.fn(async () => now),
      sendCommand: vi.fn(async (_id, command) => {
        const def = (command as { Record: { PhaseDefined: { def: PhaseDef } } }).Record.PhaseDefined
          .def;
        made.push(def);
        now = eventView({ phases: [...now.phases, { ...group(def.id, def.label), def }] });
        return { cursor: made.length + 1 };
      })
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add a practice phase' }));
    // The phase: called Practice, with the saved format that ranks nobody.
    await waitFor(() => expect(made).toHaveLength(1));
    expect(made[0].label).toBe('Practice');
    expect(made[0].id).toMatch(/^practice-[0-9a-z]{4}$/);
    expect(made[0].parent).toBeUndefined();
    expect(made[0].kind).toEqual({ Group: { combine: 'Stack' } });
    expect(made[0].format).toEqual(OPEN_PRACTICE);
    // Then straight on to its first heat, already under it.
    const form = await screen.findByRole('form', { name: 'Add a heat' });
    expect(within(form).getByLabelText('Under')).toHaveValue(made[0].id);
    await fireEvent.input(within(form).getByLabelText('Heat name'), {
      target: { value: 'Heat 1' }
    });
    await fireEvent.click(within(form).getByRole('button', { name: 'Ace' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Add heat' }));
    await waitFor(() => expect(made).toHaveLength(2));
    expect(made[1].parent).toBe(made[0].id);
    expect(vi.mocked(session.sendCommand)).toHaveBeenCalledTimes(2);
    // No id reaches the page.
    expect(document.body.textContent).not.toMatch(/practice-[0-9a-z]{4}/);
  });

  it('with no unranked format saved, the practice phase sets none and the heat picks its own', async () => {
    const made: PhaseDef[] = [];
    await open(eventView(), {
      listFormats: vi.fn(async () => SAVED_FORMATS.filter((saved) => saved.format.ranking)),
      sendCommand: vi.fn(async (_id, command) => {
        made.push(
          (command as { Record: { PhaseDefined: { def: PhaseDef } } }).Record.PhaseDefined.def
        );
        return { cursor: 2 };
      })
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add a practice phase' }));
    await waitFor(() => expect(made).toHaveLength(1));
    expect('format' in made[0]).toBe(false);
  });

  it("shows the Director's words when the practice phase is refused, and opens no dialog", async () => {
    await open(eventView(), {
      sendCommand: vi.fn(async () => Promise.reject(new Error('The event is closed.')))
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add a practice phase' }));
    expect(await screen.findByText('The event is closed.')).toBeVisible();
    expect(screen.queryByRole('form', { name: 'Add a heat' })).toBeNull();
  });
});

/** Open Add phase and pick a kind: the generators are choices in the one dialog. */
async function addPhaseOf(kind: 'empty' | 'bracket' | 'mains' | 'round-robin') {
  await fireEvent.click(screen.getByRole('button', { name: '+ Add phase' }));
  const form = await screen.findByRole('form', { name: 'Add a phase' });
  await fireEvent.change(within(form).getByLabelText('Kind'), { target: { value: kind } });
  return form;
}

describe('Add a phase', () => {
  async function dialog() {
    await fireEvent.click(screen.getByRole('button', { name: '+ Add phase' }));
    return screen.findByRole('form', { name: 'Add a phase' });
  }

  it('asks for a name before sending anything', async () => {
    const { session } = await open(eventView());
    await dialog();
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));
    expect(await screen.findByText('Give the phase a name.')).toBeVisible();
    expect(session.sendCommand).not.toHaveBeenCalled();
  });

  it('with no format it stacks its heats and ranks nobody', async () => {
    const { session } = await open(eventView());
    const form = await dialog();
    expect(optionTexts(within(form).getByLabelText('Format'))[0]).toBe('None of its own');
    expect(within(form).getByTestId('format-reads')).toHaveTextContent(
      'Each heat under it is then given its own format.'
    );
    // No class is in the event and no phase holds phases, so neither is asked about.
    expect(within(form).queryByLabelText('For the class')).toBeNull();
    expect(within(form).queryByLabelText('Under')).toBeNull();
    await fireEvent.input(within(form).getByLabelText('Phase name'), {
      target: { value: 'Practice' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));
    await waitFor(() => expect(session.sendCommand).toHaveBeenCalled());
    expect(definedPhase(sent(session)[0], 'practice')).toEqual({
      label: 'Practice',
      kind: { Group: { combine: 'Stack' } }
    });
    await waitFor(() => expect(screen.queryByRole('form', { name: 'Add a phase' })).toBeNull());
  });

  it("with a ranked format, for a class: it ranks by the format's rule over the class's roster", async () => {
    const view = eventView({
      rosters: [
        { class: 'cls-open', pilots: ['p-1a'], counted_phases: [], phases: [], standings: [] }
      ]
    });
    const { session } = await open(view);
    const form = await dialog();
    expect(optionTexts(within(form).getByLabelText('For the class'))).toEqual([
      'Whoever flies in its heats',
      'Open'
    ]);
    await fireEvent.input(within(form).getByLabelText('Phase name'), {
      target: { value: 'Qualifying' }
    });
    await fireEvent.change(within(form).getByLabelText('For the class'), {
      target: { value: 'cls-open' }
    });
    await fireEvent.change(within(form).getByLabelText('Format'), {
      target: { value: 'fmt-quali-4m8z' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));
    await waitFor(() => expect(session.sendCommand).toHaveBeenCalled());
    expect(definedPhase(sent(session)[0], 'qualifying')).toEqual({
      label: 'Qualifying',
      kind: {
        Group: {
          field: [{ Class: 'cls-open' }],
          combine: { Measures: { sort: [{ BestConsecutive: 3 }] } }
        }
      },
      format: TWO_MINUTE_QUALI
    });
    expect(form.textContent).not.toContain('cls-');
  });

  it('an unranked format of its own still stacks', async () => {
    const { session } = await open(eventView());
    const form = await dialog();
    await fireEvent.input(within(form).getByLabelText('Phase name'), {
      target: { value: 'Practice' }
    });
    await fireEvent.change(within(form).getByLabelText('Format'), {
      target: { value: 'fmt-practice-9k2d' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));
    await waitFor(() => expect(session.sendCommand).toHaveBeenCalled());
    expect(definedPhase(sent(session)[0], 'practice')).toEqual({
      label: 'Practice',
      kind: { Group: { combine: 'Stack' } },
      format: OPEN_PRACTICE
    });
  });

  it('under a phase with a format it follows that format and ranks by its rule', async () => {
    const quali = group('quali', 'Qualifying', undefined, { format: TWO_MINUTE_QUALI });
    quali.def.format = TWO_MINUTE_QUALI;
    const { session } = await open(eventView({ phases: [quali] }));
    const form = await dialog();
    await fireEvent.input(within(form).getByLabelText('Phase name'), {
      target: { value: 'Round 1' }
    });
    await fireEvent.change(within(form).getByLabelText('Under'), { target: { value: 'quali' } });
    expect(optionTexts(within(form).getByLabelText('Format'))[0]).toBe(
      "The phase above's: 2 minute qualifying"
    );
    expect(within(form).getByTestId('format-reads')).toHaveTextContent(
      'Its heats follow the format of the phase above.'
    );
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));
    await waitFor(() => expect(session.sendCommand).toHaveBeenCalled());
    expect(definedPhase(sent(session)[0], 'round-1')).toEqual({
      label: 'Round 1',
      parent: 'quali',
      kind: { Group: { combine: { Measures: { sort: [{ BestConsecutive: 3 }] } } } }
    });
  });

  it("shows the Director's sentence in the dialog when it says no", async () => {
    const SAID = 'A phase called Practice is already in the event.';
    await open(eventView(), {
      sendCommand: vi.fn(async () =>
        Promise.reject(Object.assign(new Error(SAID), { status: 409, code: 'Refused' }))
      )
    });
    const form = await dialog();
    await fireEvent.input(within(form).getByLabelText('Phase name'), {
      target: { value: 'Practice' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));
    expect(await within(form).findByRole('alert')).toHaveTextContent(SAID);
    expect(screen.getByRole('form', { name: 'Add a phase' })).toBeVisible();
  });
});

describe('Add a bracket', () => {
  async function fill() {
    const form = await addPhaseOf('bracket');
    await fireEvent.input(within(form).getByLabelText('Phase name'), {
      target: { value: 'Finals' }
    });
    for (const callsign of ['Cat', 'Ace', 'Bee']) {
      await fireEvent.click(within(form).getByRole('button', { name: callsign }));
    }
    return form;
  }

  it('sends the bracket with seeds in the order picked and one seat per heat place', async () => {
    const { session } = await open(eventView());
    const form = await fill();
    await fireEvent.change(within(form).getByLabelText('Pilots in each heat'), {
      target: { value: '2' }
    });
    await fireEvent.change(within(form).getByLabelText('Elimination'), {
      target: { value: 'Single' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));

    await waitFor(() => expect(session.createBracket).toHaveBeenCalled());
    const { id, ...request } = vi.mocked(session.createBracket).mock.calls[0][1] as BracketRequest;
    expect(id).toMatch(/^finals-[0-9a-z]{4}$/);
    expect(request).toEqual({
      label: 'Finals',
      field: [{ Imported: ['p-3c', 'p-1a', 'p-2b'] }],
      entrants: [{ Pilot: 'p-3c' }, { Pilot: 'p-1a' }, { Pilot: 'p-2b' }],
      seats: [
        { adapter: 'bench', seat: 'seat-1' },
        { adapter: 'bench', seat: 'seat-2' }
      ],
      // A heat of two can move one pilot on at most, so the 2 it started with is pulled back.
      advance: 1,
      elimination: 'Single',
      // Not the first saved format (practice ranks nobody): the first that races to a lap count.
      format: FIRST_TO_3
    });
  });

  it('offers only the saved formats that rank a heat', async () => {
    await open(eventView());
    const form = await addPhaseOf('bracket');
    const format = within(form).getByLabelText('Format');
    expect(optionTexts(format)).toEqual(['First to 3 laps', '2 minute qualifying']);
    expect(format).toHaveValue('fmt-first3-7q1x');
  });

  it('asks for a ranked format when none is saved, and sends nothing', async () => {
    const { session } = await open(eventView(), {
      listFormats: vi.fn(async () => [SAVED_FORMATS[0]])
    });
    const form = await fill();
    expect(within(form).getByTestId('format-reads')).toHaveTextContent('No saved format fits.');
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));
    expect(await within(form).findByRole('alert')).toHaveTextContent(
      'Pick a format that ranks the heat. It decides who moves on.'
    );
    expect(session.createBracket).not.toHaveBeenCalled();
  });

  it('can go under a phase', async () => {
    const { session } = await open(eventView({ phases: [group('mains', 'Mains')] }));
    const form = await fill();
    await fireEvent.change(within(form).getByLabelText('Under'), { target: { value: 'mains' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));
    await waitFor(() => expect(session.createBracket).toHaveBeenCalled());
    expect(vi.mocked(session.createBracket).mock.calls[0][1].parent).toBe('mains');
  });

  it("shows the Director's sentence in the dialog when it says no, and stays open", async () => {
    const SAID = '3 cannot advance from a heat of 4: it must be 1 to 2';
    await open(eventView(), {
      createBracket: vi.fn(async () =>
        Promise.reject(Object.assign(new Error(SAID), { status: 400, code: 'BadRequest' }))
      )
    });
    const form = await fill();
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));
    expect(await within(form).findByRole('alert')).toHaveTextContent(SAID);
    expect(screen.getByRole('form', { name: 'Add a phase' })).toBeVisible();
  });
});

describe('Add a multi-main', () => {
  const seeded = () =>
    eventView({
      phases: [
        group('quali', 'Qualifying', undefined, { field: ['p-1a', 'p-2b', 'p-3c'] }),
        group('fun', 'Fun Flying')
      ]
    });

  it('is a kind of phase, with no button of its own on the page', async () => {
    await open(seeded());
    expect(screen.queryByRole('button', { name: /bracket|main/i })).toBeNull();
    const form = await addPhaseOf('empty');
    expect(optionTexts(within(form).getByLabelText('Kind'))).toEqual([
      'Empty',
      'Bracket',
      'Multi-Main',
      'Round Robin'
    ]);
  });

  it('keeps the name and the phase above when the kind is changed', async () => {
    await open(seeded());
    const form = await addPhaseOf('empty');
    await fireEvent.input(within(form).getByLabelText('Phase name'), {
      target: { value: 'Whoop Mains' }
    });
    await fireEvent.change(within(form).getByLabelText('Under'), { target: { value: 'fun' } });
    await fireEvent.change(within(form).getByLabelText('Kind'), { target: { value: 'mains' } });
    expect(within(form).getByLabelText('Phase name')).toHaveValue('Whoop Mains');
    expect(within(form).getByLabelText('Under')).toHaveValue('fun');
    expect(within(form).getByLabelText('Seeded from')).toBeVisible();
  });

  it('starts from the phase that ranks pilots and says how many mains the numbers make', async () => {
    await open(seeded());
    const form = await addPhaseOf('mains');
    expect(within(form).getByLabelText('Seeded from')).toHaveValue('quali');
    expect(within(form).getByLabelText('Pilots')).toHaveValue(3);
    expect(within(form).getByTestId('mains-count')).toHaveTextContent(
      'This makes 1 main, the A-Main.'
    );
    await fireEvent.input(within(form).getByLabelText('Pilots'), { target: { value: '64' } });
    await fireEvent.input(within(form).getByLabelText('How many bump up'), {
      target: { value: '4' }
    });
    expect(within(form).getByTestId('mains-count')).toHaveTextContent(
      'This makes 5 mains, A-Main to E-Main. The E-Main is flown first.'
    );
    expectNoRawIds();
  });

  it('starts from a finished phase before one still being flown', async () => {
    await open(
      eventView({
        phases: [
          group('practice', 'Practice', undefined, { field: ['p-1a', 'p-2b', 'p-3c'] }),
          group('quali', 'Qualifying', undefined, { field: ['p-1a', 'p-2b'], complete: true })
        ]
      })
    );
    const form = await addPhaseOf('mains');
    expect(within(form).getByLabelText('Seeded from')).toHaveValue('quali');
    expect(within(form).getByLabelText('Pilots')).toHaveValue(2);
  });

  it('sends the mains: the seeding phase, the numbers, the heat shape and the top final', async () => {
    const { session } = await open(seeded());
    const form = await addPhaseOf('mains');
    await fireEvent.input(within(form).getByLabelText('Phase name'), {
      target: { value: 'Mains' }
    });
    await fireEvent.input(within(form).getByLabelText('Pilots'), { target: { value: '20' } });
    await fireEvent.input(within(form).getByLabelText('Pilots in each main'), {
      target: { value: '8' }
    });
    await fireEvent.change(within(form).getByLabelText('Pilots in each heat'), {
      target: { value: '2' }
    });
    await fireEvent.change(within(form).getByLabelText('A-Main final'), {
      target: { value: SAVED_FORMATS[2].id }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));

    await waitFor(() => expect(session.createMains).toHaveBeenCalled());
    const { id, ...request } = vi.mocked(session.createMains).mock.calls[0][1] as MainsRequest;
    expect(id).toMatch(/^mains-[0-9a-z]{4}$/);
    expect(request).toEqual({
      label: 'Mains',
      seeded_from: 'quali',
      pilots: 20,
      per_main: 8,
      bump: 2,
      seats: [
        { adapter: 'bench', seat: 'seat-1' },
        { adapter: 'bench', seat: 'seat-2' }
      ],
      advance: 1,
      elimination: 'Double',
      format: FIRST_TO_3,
      top_final_format: SAVED_FORMATS[2].format
    });
    expect(session.createBracket).not.toHaveBeenCalled();
  });

  it('says what is wrong with the numbers, and sends nothing', async () => {
    const { session } = await open(seeded());
    const form = await addPhaseOf('mains');
    await fireEvent.input(within(form).getByLabelText('Phase name'), {
      target: { value: 'Mains' }
    });
    await fireEvent.input(within(form).getByLabelText('Pilots in each main'), {
      target: { value: '2' }
    });
    expect(within(form).getByTestId('mains-count')).toHaveTextContent(
      'Fewer pilots must bump up than start in a main.'
    );
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));
    expect(await within(form).findByRole('alert')).toHaveTextContent(
      'Fewer pilots must bump up than start in a main.'
    );
    expect(session.createMains).not.toHaveBeenCalled();
  });

  it("shows the Director's sentence when it says no, and stays open", async () => {
    const SAID = '20 pilots do not fit: 2 mains hold 14';
    await open(seeded(), {
      createMains: vi.fn(async () =>
        Promise.reject(Object.assign(new Error(SAID), { status: 400, code: 'BadRequest' }))
      )
    });
    const form = await addPhaseOf('mains');
    await fireEvent.input(within(form).getByLabelText('Phase name'), {
      target: { value: 'Mains' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));
    expect(await within(form).findByRole('alert')).toHaveTextContent(SAID);
  });
});

describe('Add a round robin', () => {
  async function fill() {
    const form = await addPhaseOf('round-robin');
    await fireEvent.input(within(form).getByLabelText('Phase name'), {
      target: { value: 'Robin' }
    });
    for (const callsign of ['Cat', 'Ace', 'Bee']) {
      await fireEvent.click(within(form).getByRole('button', { name: callsign }));
    }
    return form;
  }

  it('asks nothing about elimination: nobody goes out of a round robin', async () => {
    await open(eventView());
    const form = await addPhaseOf('round-robin');
    expect(within(form).queryByLabelText('Elimination')).toBeNull();
    expect(within(form).queryByLabelText('How many move on')).toBeNull();
    expect(within(form).getByLabelText('Pilots in each heat')).toBeVisible();
    expect(within(form).getByLabelText('Rounds')).toHaveValue(3);
  });

  it('offers points for the heat size and says how many heats it makes', async () => {
    await open(eventView());
    const form = await fill();
    expect(within(form).getByLabelText('Points by place')).toHaveValue('4, 3, 2, 1');
    expect(within(form).getByTestId('round-robin-count')).toHaveTextContent(
      'This makes 3 rounds of 1 heat, 3 heats in all. Each pilot keeps the same seat in every round.'
    );
    await fireEvent.change(within(form).getByLabelText('Pilots in each heat'), {
      target: { value: '2' }
    });
    expect(within(form).getByLabelText('Points by place')).toHaveValue('2, 1');
    expect(within(form).getByTestId('round-robin-count')).toHaveTextContent(
      'This makes 3 rounds of 2 heats, 6 heats in all.'
    );
    expectNoRawIds();
  });

  it('sends the pilots in seed order, the rounds, and the points on a copy of the format', async () => {
    const { session } = await open(eventView());
    const form = await fill();
    await fireEvent.change(within(form).getByLabelText('Pilots in each heat'), {
      target: { value: '2' }
    });
    await fireEvent.input(within(form).getByLabelText('Rounds'), { target: { value: '5' } });
    await fireEvent.input(within(form).getByLabelText('Points by place'), {
      target: { value: '10, 6' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));

    await waitFor(() => expect(session.createRoundRobin).toHaveBeenCalled());
    const { id, ...request } = vi.mocked(session.createRoundRobin).mock
      .calls[0][1] as RoundRobinRequest;
    expect(id).toMatch(/^robin-[0-9a-z]{4}$/);
    expect(request).toEqual({
      label: 'Robin',
      field: [{ Imported: ['p-3c', 'p-1a', 'p-2b'] }],
      entrants: [{ Pilot: 'p-3c' }, { Pilot: 'p-1a' }, { Pilot: 'p-2b' }],
      seats: [
        { adapter: 'bench', seat: 'seat-1' },
        { adapter: 'bench', seat: 'seat-2' }
      ],
      rounds: 5,
      format: { ...FIRST_TO_3, points: [10, 6] }
    });
    // The saved format itself is untouched.
    expect(FIRST_TO_3.points ?? []).toEqual([]);
    expect(session.createBracket).not.toHaveBeenCalled();
  });

  it('keeps points the director typed when the heat size changes', async () => {
    await open(eventView());
    const form = await fill();
    await fireEvent.input(within(form).getByLabelText('Points by place'), {
      target: { value: '5, 3, 1' }
    });
    await fireEvent.change(within(form).getByLabelText('Pilots in each heat'), {
      target: { value: '2' }
    });
    expect(within(form).getByLabelText('Points by place')).toHaveValue('5, 3, 1');
  });

  it.each([
    ['Rounds', '0', 'Say how many rounds, one at least.'],
    [
      'Points by place',
      'ten, six',
      'List the points for each place, first place first, like 4, 3, 2, 1.'
    ]
  ])('refuses a bad %s and sends nothing', async (label, value, says) => {
    const { session } = await open(eventView());
    const form = await fill();
    await fireEvent.input(within(form).getByLabelText(label), { target: { value } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));
    expect(await within(form).findByRole('alert')).toHaveTextContent(says);
    expect(session.createRoundRobin).not.toHaveBeenCalled();
  });

  it('needs two pilots, and can go under a phase', async () => {
    const { session } = await open(eventView({ phases: [group('day', 'Day One')] }));
    const form = await addPhaseOf('round-robin');
    await fireEvent.input(within(form).getByLabelText('Phase name'), {
      target: { value: 'Robin' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));
    expect(await within(form).findByRole('alert')).toHaveTextContent('Choose at least two pilots.');
    for (const callsign of ['Ace', 'Bee']) {
      await fireEvent.click(within(form).getByRole('button', { name: callsign }));
    }
    await fireEvent.change(within(form).getByLabelText('Under'), { target: { value: 'day' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));
    await waitFor(() => expect(session.createRoundRobin).toHaveBeenCalled());
    expect(vi.mocked(session.createRoundRobin).mock.calls[0][1].parent).toBe('day');
  });

  it("shows the Director's sentence when it says no, and stays open", async () => {
    const SAID = 'a round robin needs at least one round';
    await open(eventView(), {
      createRoundRobin: vi.fn(async () =>
        Promise.reject(Object.assign(new Error(SAID), { status: 400, code: 'BadRequest' }))
      )
    });
    const form = await fill();
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));
    expect(await within(form).findByRole('alert')).toHaveTextContent(SAID);
    expect(screen.getByRole('form', { name: 'Add a phase' })).toBeVisible();
  });
});

describe('Timed by', () => {
  const GATE = timerView('gate-x1y2', 'Gate', 2);
  const OFF = timerView('far-gate-9zz0', 'Far Gate', 0, {
    connected: false,
    problem: 'Nothing answered at 10.0.0.9:7270.'
  });
  const timedBy = (form: HTMLElement) =>
    within(form).getByLabelText('Timed by') as HTMLSelectElement;

  it('offers only the Bench, already chosen, when no timer is added', async () => {
    await open(eventView({ phases: [RACES] }));
    await fireEvent.click(screen.getByRole('button', { name: 'Add a heat under Races' }));
    const form = await screen.findByRole('form', { name: 'Add a heat' });
    expect(optionTexts(timedBy(form))).toEqual(['Bench (no timer: crossings are logged by hand)']);
    expect(timedBy(form).value).toBe('bench');
    expect(within(form).getByText(/Up to 8\./)).toBeVisible();
  });

  it('starts on the first connected timer and puts the lineup on its seats', async () => {
    const { session } = await open(eventView({ phases: [RACES] }), {
      listTimers: vi.fn(async () => [OFF, GATE])
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add a heat under Races' }));
    const form = await screen.findByRole('form', { name: 'Add a heat' });
    const select = timedBy(form);
    expect(optionTexts(select)).toEqual([
      'Far Gate (not connected)',
      'Gate (2 seats)',
      'Bench (no timer: crossings are logged by hand)'
    ]);
    expect(select.value).toBe('gate-x1y2');
    // A timer with no seats is listed but cannot be chosen.
    expect(select.options[0].disabled).toBe(true);
    expect(select.options[1].disabled).toBe(false);
    // By name only: no timer id is read by a person.
    expect(form.textContent).not.toContain('gate-x1y2');
    expect(form.textContent).not.toContain('far-gate-9zz0');
    // The seat count follows the timer: two seats, so a third pilot cannot be added.
    expect(within(form).getByText(/Up to 2\./)).toBeVisible();

    await fireEvent.input(within(form).getByLabelText('Heat name'), { target: { value: 'H1' } });
    await fireEvent.click(within(form).getByRole('button', { name: 'Bee' }));
    await fireEvent.click(within(form).getByRole('button', { name: 'Ace' }));
    expect(within(form).getByRole('button', { name: 'Cat' })).toBeDisabled();
    const chosen = within(form).getByRole('list', { name: 'Chosen pilots' });
    expect(chosen).toHaveTextContent('Seat 1');
    expect(chosen).toHaveTextContent('Seat 2');
    await fireEvent.click(screen.getByRole('button', { name: 'Add heat' }));

    await waitFor(() => expect(session.sendCommand).toHaveBeenCalled());
    expect(definedPhase(sent(session)[0], 'h1').kind).toEqual({
      Heat: {
        lineup: [
          { adapter: 'gate-x1y2', seat: '0', source: { Pilot: 'p-2b' } },
          { adapter: 'gate-x1y2', seat: '1', source: { Pilot: 'p-1a' } }
        ]
      }
    });
  });

  it('lets the director pick the Bench instead, and the seats follow', async () => {
    const { session } = await open(eventView({ phases: [RACES] }), {
      listTimers: vi.fn(async () => [GATE])
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add a heat under Races' }));
    const form = await screen.findByRole('form', { name: 'Add a heat' });
    await fireEvent.change(timedBy(form), { target: { value: 'bench' } });
    expect(within(form).getByText(/Up to 8\./)).toBeVisible();
    await fireEvent.input(within(form).getByLabelText('Heat name'), { target: { value: 'H1' } });
    await fireEvent.click(within(form).getByRole('button', { name: 'Ace' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Add heat' }));
    await waitFor(() => expect(session.sendCommand).toHaveBeenCalled());
    expect(definedPhase(sent(session)[0], 'h1').kind).toEqual({
      Heat: { lineup: [{ adapter: 'bench', seat: 'seat-1', source: { Pilot: 'p-1a' } }] }
    });
  });

  it("a bracket's heat size cannot pass the timer's seat count, and its seats are the timer's", async () => {
    const { session } = await open(eventView(), {
      listTimers: vi.fn(async () => [timerView('gate-x1y2', 'Gate', 3)])
    });
    const form = await addPhaseOf('bracket');
    expect(timedBy(form).value).toBe('gate-x1y2');
    const sizes = within(form).getByLabelText('Pilots in each heat') as HTMLSelectElement;
    expect([...sizes.options].map((option) => option.value)).toEqual(['2', '3']);
    // It started on 4, which this timer cannot seat, so it is pulled back to 3.
    await waitFor(() => expect(sizes.value).toBe('3'));

    await fireEvent.input(within(form).getByLabelText('Phase name'), {
      target: { value: 'Finals' }
    });
    for (const callsign of ['Cat', 'Ace', 'Bee']) {
      await fireEvent.click(within(form).getByRole('button', { name: callsign }));
    }
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));
    await waitFor(() => expect(session.createBracket).toHaveBeenCalled());
    const request = vi.mocked(session.createBracket).mock.calls[0][1] as BracketRequest;
    expect(request.seats).toEqual([
      { adapter: 'gate-x1y2', seat: '0' },
      { adapter: 'gate-x1y2', seat: '1' },
      { adapter: 'gate-x1y2', seat: '2' }
    ]);
  });
});

describe('starting from a class', () => {
  const view = () =>
    eventView({
      phases: [group('finals', 'Finals')],
      rosters: [
        {
          class: 'cls-open',
          pilots: ['p-1a', 'p-2b'],
          counted_phases: [],
          phases: [],
          standings: []
        }
      ]
    });

  it('a new heat can start from a class roster, and the pilots can still be changed', async () => {
    const { session } = await open(view());
    await fireEvent.click(screen.getByRole('button', { name: 'Add a heat under Finals' }));
    const form = await screen.findByRole('form', { name: 'Add a heat' });
    await fireEvent.input(within(form).getByLabelText('Heat name'), {
      target: { value: 'Open 1' }
    });
    expect(optionTexts(within(form).getByLabelText('Start from a class'))).toEqual([
      'Pick pilots one by one',
      'Open'
    ]);
    await fireEvent.change(within(form).getByLabelText('Start from a class'), {
      target: { value: 'cls-open' }
    });
    const chosen = within(form).getByRole('list', { name: 'Chosen pilots' });
    expect(within(chosen).getAllByRole('listitem')).toHaveLength(2);
    await fireEvent.click(within(form).getByRole('button', { name: 'Cat' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Add heat' }));
    await waitFor(() => expect(session.sendCommand).toHaveBeenCalled());
    const kind = definedPhase(sent(session)[0], 'open-1').kind as {
      Heat: { lineup: { source: unknown }[] };
    };
    expect(kind.Heat.lineup.map((seat) => seat.source)).toEqual([
      { Pilot: 'p-1a' },
      { Pilot: 'p-2b' },
      { Pilot: 'p-3c' }
    ]);
  });

  it('a new bracket made for a class is seeded from its roster and carries the class', async () => {
    const createBracket = vi.fn<LiveSession['createBracket']>(async () => ({ cursor: 1 }));
    await open(view(), { createBracket });
    const form = await addPhaseOf('bracket');
    await fireEvent.input(within(form).getByLabelText('Phase name'), {
      target: { value: 'Open Finals' }
    });
    expect(optionTexts(within(form).getByLabelText('Class'))).toEqual([
      'No class: pick pilots one by one',
      'Open'
    ]);
    await fireEvent.change(within(form).getByLabelText('Class'), {
      target: { value: 'cls-open' }
    });
    // The roster is a start: a pilot is added by hand, and the class is still the phase's.
    await fireEvent.click(within(form).getByRole('button', { name: 'Cat' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));
    await waitFor(() => expect(createBracket).toHaveBeenCalled());
    const request = createBracket.mock.calls[0][1];
    expect(request.class).toBe('cls-open');
    expect(request.field).toEqual([{ Imported: ['p-1a', 'p-2b', 'p-3c'] }]);
  });

  it('a new bracket with no class picked carries none, so it counts toward no class', async () => {
    const createBracket = vi.fn<LiveSession['createBracket']>(async () => ({ cursor: 1 }));
    await open(view(), { createBracket });
    const form = await addPhaseOf('bracket');
    await fireEvent.input(within(form).getByLabelText('Phase name'), {
      target: { value: 'Fun Finals' }
    });
    // Picked and then un-picked: the pilots stay, the class goes.
    await fireEvent.change(within(form).getByLabelText('Class'), {
      target: { value: 'cls-open' }
    });
    await fireEvent.change(within(form).getByLabelText('Class'), { target: { value: '' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));
    await waitFor(() => expect(createBracket).toHaveBeenCalled());
    const request = createBracket.mock.calls[0][1];
    expect('class' in request).toBe(false);
    expect(request.field).toEqual([{ Imported: ['p-1a', 'p-2b'] }]);
  });

  it('a new round robin made for a class carries the class too', async () => {
    const createRoundRobin = vi.fn<LiveSession['createRoundRobin']>(async () => ({ cursor: 1 }));
    await open(view(), { createRoundRobin });
    const form = await addPhaseOf('round-robin');
    await fireEvent.input(within(form).getByLabelText('Phase name'), {
      target: { value: 'Open Robin' }
    });
    await fireEvent.change(within(form).getByLabelText('Class'), {
      target: { value: 'cls-open' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));
    await waitFor(() => expect(createRoundRobin).toHaveBeenCalled());
    expect(createRoundRobin.mock.calls[0][1].class).toBe('cls-open');
  });
});

describe("Pilot sign-up on a phase's page", () => {
  it('offers Sign-up on a phase that holds heats, and not on a heat or a locked phase', async () => {
    const view = eventView({
      phases: [
        group('quali', 'Qualifying'),
        group('mains', 'Mains', undefined, { locked: true }),
        heat('h1', 'Practice Heat', [])
      ]
    });
    await openPhase(view, 'quali');
    expect(
      screen.getByRole('button', { name: 'Set up pilot sign-up for Qualifying' })
    ).toBeVisible();
    cleanup();
    await openPhase(view, 'mains');
    expect(screen.queryByRole('button', { name: /^Set up pilot sign-up/ })).toBeNull();
    cleanup();
    await openPhase(view, 'h1');
    expect(screen.queryByRole('button', { name: /^Set up pilot sign-up/ })).toBeNull();
    // With no sign-up anywhere there is no sheet on the page.
    expect(screen.queryByRole('table', { name: /^Heats of/ })).toBeNull();
  });

  it('turns sign-up on from the dialog, with one command', async () => {
    const { session } = await openPhase(
      eventView({ phases: [group('quali', 'Qualifying')] }),
      'quali',
      {
        listTimers: vi.fn(async () => [GATE])
      }
    );
    await fireEvent.click(
      screen.getByRole('button', { name: 'Set up pilot sign-up for Qualifying' })
    );
    const dialog = screen.getByRole('dialog');
    expect(within(dialog).getByRole('heading')).toHaveTextContent('Pilot sign-up: Qualifying');
    await fireEvent.submit(within(dialog).getByRole('form', { name: 'Sign-up rules' }));
    await waitFor(() => expect(sent(session)).toHaveLength(1));
    expect(sent(session)[0]).toEqual({
      Signup: {
        Set: {
          phase: 'quali',
          rules: {
            seats: [
              { adapter: 'gate', seat: '0' },
              { adapter: 'gate', seat: '1' },
              { adapter: 'gate', seat: '2' }
            ],
            open_heats: 3,
            rest_heats: 0,
            open: false
          }
        }
      }
    });
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  });

  it('shows the sheet of a phase that has sign-up, and no second Sign-up button for it', async () => {
    await openPhase(signupEvent(signupSheet()), 'quali', {
      listPilots: vi.fn(async () => SIGNUP_PILOTS),
      listTimers: vi.fn(async () => [GATE])
    });
    const sheet = screen.getByRole('table', { name: 'Heats of Qualifying' });
    expect(within(sheet).getAllByRole('row')[1]).toHaveTextContent('Ace');
    expect(
      screen.queryByRole('button', { name: 'Set up pilot sign-up for Qualifying' })
    ).toBeNull();
    // The rules are reached from the sheet, with what is in force filled in.
    await fireEvent.click(screen.getByRole('button', { name: 'Rules' }));
    const dialog = screen.getByRole('dialog');
    expect(within(dialog).getByRole('button', { name: 'Save' })).toBeVisible();
    expect(within(dialog).getByRole('button', { name: 'Turn sign-up off' })).toBeVisible();
  });

  it("sends the sheet's changes through the event, as the director", async () => {
    const { session } = await openPhase(signupEvent(signupSheet()), 'quali', {
      listPilots: vi.fn(async () => SIGNUP_PILOTS),
      listTimers: vi.fn(async () => [GATE])
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Close sign-up' }));
    await waitFor(() => expect(sent(session)).toHaveLength(1));
    expect(sent(session)[0]).toMatchObject({
      Signup: { Set: { phase: 'quali', rules: { open: false } } }
    });
  });
});

describe('the Phases page as a list', () => {
  const lines = () =>
    within(screen.getByRole('list', { name: 'Phases of the event' })).getAllByRole('link');
  /** The titles of the cards on the page, top to bottom. */
  const cards = () =>
    [...document.querySelectorAll('.gf-card-title')].map((title) => title.textContent);

  function day(over: Partial<EventView> = {}) {
    return eventView({
      phases: [
        group('quali', 'Qualifying', undefined, { format: TWO_MINUTE_QUALI }),
        heat('q-1', 'Qualifying Heat 1', ['p-1a'], { parent: 'quali', complete: true }),
        heat('q-2', 'Qualifying Heat 2', ['p-2b'], { parent: 'quali' }),
        group('mains', 'Mains'),
        heat('a-main', 'A-Main', ['p-1a', 'p-2b'], { parent: 'mains', waiting: true }),
        heat('solo', 'Shootout', ['p-3c'])
      ],
      queue: ['q-2', 'solo'],
      ...over
    });
  }

  it('has one line for each phase that holds heats, in order, with where it stands', async () => {
    await open(day());
    expect(lines().map((line) => line.getAttribute('aria-label'))).toEqual([
      'Open Qualifying',
      'Open Mains'
    ]);
    expect(lines()[0]).toHaveTextContent('Qualifying');
    expect(lines()[0]).toHaveTextContent('Ready');
    expect(lines()[0]).toHaveTextContent('2 minute qualifying');
    expect(within(lines()[0]).getByTestId('phase-heats')).toHaveTextContent('2 heats · 1 flown');
    expect(within(lines()[1]).getByTestId('phase-heats')).toHaveTextContent('1 heat · 0 flown');
    expectNoRawIds();
  });

  it("each line opens that phase's own page", async () => {
    await open(day());
    expect(lines().map((line) => line.getAttribute('href'))).toEqual([
      '#/event/phases/quali',
      '#/event/phases/mains'
    ]);
  });

  it('keeps the heats of a phase off the list: they are on its page', async () => {
    await open(day());
    expect(screen.queryByRole('list', { name: 'Lineup of Qualifying Heat 1' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Run Qualifying Heat 2' })).toBeNull();
    expect(screen.queryByText('Ranking')).toBeNull();
    // No sign-up sheet either, however many phases have one.
    expect(screen.queryByRole('table', { name: /^Heats of/ })).toBeNull();
  });

  it('puts the phases first, then the heat order, then the heats on their own', async () => {
    await open(day());
    expect(cards()).toEqual(['Phases', 'Up next', 'Heats on their own']);
  });

  it('shows a heat that stands alone in full, since there is nothing to open', async () => {
    const { onrun } = await open(day());
    expect(screen.getByRole('list', { name: 'Lineup of Shootout' })).toHaveTextContent('Cat');
    await fireEvent.click(screen.getByRole('button', { name: 'Run Shootout' }));
    expect(onrun).toHaveBeenCalledWith('solo');
  });

  it('an event of heats only has no Phases card, and calls them just Heats', async () => {
    await open(eventView({ phases: [heat('solo', 'Shootout', ['p-3c'])], queue: ['solo'] }));
    expect(cards()).toEqual(['Up next', 'Heats']);
    expect(screen.queryByRole('list', { name: 'Phases of the event' })).toBeNull();
  });

  it('an event of phases only has no card for heats on their own', async () => {
    await open(eventView({ phases: [group('quali', 'Qualifying')] }));
    expect(cards()).toEqual(['Phases']);
    expect(within(lines()[0]).getByTestId('phase-heats')).toHaveTextContent('No heats yet');
  });

  it("marks the phase that holds the heat on the timer, by the heat's name", async () => {
    await open(day({ on_timer: 'q-2' }));
    expect(lines()[0]).toHaveTextContent('On the timer: Qualifying Heat 2');
    expect(lines()[1]).not.toHaveTextContent('On the timer');
  });

  it('marks a phase that is racing, one with sign-up, and one waiting on a decision', async () => {
    const on = race('q-2', 1, 'Running');
    const rules = { seats: [], open_heats: 1, rest_heats: 0, open: false };
    const view = day({ races: [on], current_race: on.race, on_timer: 'q-2' });
    view.phases[3] = { ...view.phases[3], signup: { rules, heats: [], pilots: [] } };
    view.proposed_disqualifications = [
      { phase: 'a-main', pilot: 'p-1a', false_starts: 2, limit: 2 }
    ];
    await open(view);
    expect(lines()[0]).toHaveTextContent('Racing');
    expect(lines()[1]).toHaveTextContent('Sign-up closed');
    expect(lines()[1]).toHaveTextContent('Needs a decision');
    expect(lines()[0]).not.toHaveTextContent('Needs a decision');
    expect(lines()[0]).not.toHaveTextContent('Sign-up');
  });

  it('adds a heat under a phase from its line, without opening the phase', async () => {
    await open(day());
    await fireEvent.click(screen.getByRole('button', { name: 'Add a heat under Mains' }));
    const dialog = screen.getByRole('dialog');
    expect(within(dialog).getByLabelText('Under')).toHaveValue('mains');
  });

  it('a phase name is text, whatever is in it', async () => {
    await open(eventView({ phases: [group('x', '<img src=x onerror=alert(1)> & "Finals"')] }));
    expect(lines()[0].querySelector('.name')?.textContent).toBe(
      '<img src=x onerror=alert(1)> & "Finals"'
    );
    expect(document.querySelector('img')).toBeNull();
  });
});

describe('phases inside a phase, on the list', () => {
  beforeEach(() => unfolded.clear());

  const view = () =>
    eventView({
      phases: [
        group('quali', 'Qualifying'),
        heat('q-1', 'Qualifying Heat 1', ['p-1a'], { parent: 'quali' }),
        group('finals', 'Finals', undefined, { format: FIRST_TO_3 }),
        group('finals/semis', 'Semis', 'finals'),
        heat('finals/semis/1', 'Semi 1', ['p-1a'], { parent: 'finals/semis' }),
        heat('finals/semis/2', 'Semi 2', ['p-2b'], { parent: 'finals/semis' }),
        group('finals/last', 'Last round', 'finals'),
        group('finals/last/deep', 'Deep', 'finals/last'),
        heat('finals/last/deep/1', 'Deep Heat', ['p-1a'], { parent: 'finals/last/deep' })
      ]
    });
  const lines = () =>
    within(screen.getByRole('list', { name: 'Phases of the event' }))
      .getAllByRole('link')
      .map((link) => link.getAttribute('aria-label'));

  it('starts folded: only the phases at the top, and a count of what is inside', async () => {
    await open(view());
    expect(lines()).toEqual(['Open Qualifying', 'Open Finals']);
    expect(screen.getByRole('link', { name: 'Open Finals' })).toHaveTextContent('2 phases inside');
    expect(screen.getByRole('link', { name: 'Open Finals' })).toHaveTextContent(
      '3 heats · 0 flown'
    );
    // Only a phase that holds phases can be unfolded.
    expect(screen.getByRole('button', { name: 'Show the phases in Finals' })).toHaveAttribute(
      'aria-expanded',
      'false'
    );
    expect(screen.queryByRole('button', { name: /the phases in Qualifying/ })).toBeNull();
    expect(screen.getByRole('link', { name: 'Open Qualifying' })).not.toHaveTextContent('inside');
  });

  it('unfolds one level at a time, each line opening that phase, and folds again', async () => {
    await open(view());
    await fireEvent.click(screen.getByRole('button', { name: 'Show the phases in Finals' }));
    expect(lines()).toEqual(['Open Qualifying', 'Open Finals', 'Open Semis', 'Open Last round']);
    const semis = screen.getByRole('link', { name: 'Open Semis' });
    expect(semis).toHaveAttribute('href', '#/event/phases/finals%2Fsemis');
    expect(semis).toHaveTextContent('2 heats · 0 flown');
    expect(semis.closest('li')).toHaveAttribute('data-depth', '1');
    expect(screen.getByRole('link', { name: 'Open Last round' })).toHaveTextContent(
      '1 phase inside'
    );

    await fireEvent.click(screen.getByRole('button', { name: 'Show the phases in Last round' }));
    expect(lines()).toContain('Open Deep');
    expect(screen.getByRole('link', { name: 'Open Deep' }).closest('li')).toHaveAttribute(
      'data-depth',
      '2'
    );

    // Folding Finals hides everything under it, however deep.
    const hide = screen.getByRole('button', { name: 'Hide the phases in Finals' });
    expect(hide).toHaveAttribute('aria-expanded', 'true');
    await fireEvent.click(hide);
    expect(lines()).toEqual(['Open Qualifying', 'Open Finals']);
    expectNoRawIds();
  });

  it('stays unfolded when the page is left and opened again', async () => {
    await open(view());
    await fireEvent.click(screen.getByRole('button', { name: 'Show the phases in Finals' }));
    cleanup();
    await open(view());
    expect(lines()).toContain('Open Semis');
  });

  it('adds a heat under a phase inside another from its line', async () => {
    await open(view());
    await fireEvent.click(screen.getByRole('button', { name: 'Show the phases in Finals' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Add a heat under Semis' }));
    expect(within(screen.getByRole('dialog')).getByLabelText('Under')).toHaveValue('finals/semis');
  });
});

describe("a phase's own page", () => {
  const view = () =>
    eventView({
      phases: [
        group('quali', 'Qualifying', undefined, { format: TWO_MINUTE_QUALI }),
        heat('q-1', 'Qualifying Heat 1', ['p-1a'], { parent: 'quali' }),
        group('mains', 'Mains'),
        heat('a-main', 'A-Main', ['p-2b'], { parent: 'mains' })
      ],
      queue: ['q-1', 'a-main']
    });

  it('is titled with the phase, says where it stands, and links back to the list', async () => {
    await openPhase(view(), 'quali');
    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Qualifying');
    expect(screen.getByText('2 minute qualifying', { selector: '.muted' })).toBeVisible();
    expect(screen.getByTestId('back-to-phases')).toHaveAttribute('href', '#/event/phases');
    expectNoRawIds();
  });

  it("shows that phase's heats and none of another phase's", async () => {
    await openPhase(view(), 'quali');
    expect(screen.getByRole('list', { name: 'Lineup of Qualifying Heat 1' })).toBeVisible();
    expect(screen.queryByRole('list', { name: 'Lineup of A-Main' })).toBeNull();
    // The phase is the page's title. It is not a row among its heats.
    expect(phaseRows().map((row) => row.querySelector('.label')?.textContent)).toEqual([
      'Qualifying Heat 1'
    ]);
    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Qualifying');
    expect(screen.queryByTestId('back-to-parent')).toBeNull();
  });

  it('has no run order: that is on the list', async () => {
    await openPhase(view(), 'quali');
    expect(screen.queryByText('Up next')).toBeNull();
  });

  it('adds a phase inside this one, with Under already set to it', async () => {
    const { session } = await openPhase(view(), 'mains');
    await fireEvent.click(screen.getByRole('button', { name: 'Add a phase inside Mains' }));
    const form = await screen.findByRole('form', { name: 'Add a phase' });
    expect(within(form).getByLabelText('Under')).toHaveValue('mains');
    await fireEvent.input(within(form).getByLabelText('Phase name'), {
      target: { value: 'Consolation' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add phase' }));
    await waitFor(() => expect(session.sendCommand).toHaveBeenCalled());
    expect(definedPhase(sent(session)[0], 'consolation').parent).toBe('mains');
  });

  it('offers no Add phase on a heat or on a locked phase', async () => {
    const locked = eventView({
      phases: [group('mains', 'Mains', undefined, { locked: true }), heat('h1', 'Solo', [])]
    });
    await openPhase(locked, 'mains');
    expect(screen.queryByRole('button', { name: /^Add a phase inside/ })).toBeNull();
    cleanup();
    await openPhase(locked, 'h1');
    expect(screen.queryByRole('button', { name: /^Add a phase inside/ })).toBeNull();
  });

  it('runs a heat and adds one under the phase from here', async () => {
    const { onrun } = await openPhase(view(), 'quali');
    await fireEvent.click(screen.getByRole('button', { name: 'Run Qualifying Heat 1' }));
    expect(onrun).toHaveBeenCalledWith('q-1');
    await fireEvent.click(screen.getByRole('button', { name: 'Add a heat under Qualifying' }));
    expect(within(screen.getByRole('dialog')).getByLabelText('Under')).toHaveValue('quali');
  });

  it('says so for a phase that is not in the event, and still links back', async () => {
    await openPhase(view(), 'removed-long-ago');
    expect(screen.getByTestId('no-such-phase')).toHaveTextContent(
      'This phase is not in the event.'
    );
    expect(screen.getAllByRole('link', { name: '← All phases' })[0]).toHaveAttribute(
      'href',
      '#/event/phases'
    );
    expect(document.body.textContent).not.toContain('removed-long-ago');
  });

  it('when the phase is removed while the page is open, says so in place of the heats', async () => {
    const start = view();
    const { live } = await openLive(start, { listPilots: vi.fn(async () => PILOTS) });
    render(PhasePage, { live, phase: 'mains', onrun: vi.fn() });
    expect(screen.getByRole('list', { name: 'Lineup of A-Main' })).toBeVisible();
    live.view = { ...start, cursor: 2, phases: start.phases.slice(0, 2), queue: ['q-1'] };
    expect(await screen.findByTestId('no-such-phase')).toBeVisible();
  });

  it('shows only the proposals about this phase', async () => {
    const withProposal = view();
    withProposal.proposed_disqualifications = [
      { phase: 'a-main', pilot: 'p-2b', false_starts: 2, limit: 2 }
    ];
    await openPhase(withProposal, 'quali');
    expect(screen.queryByRole('button', { name: /^Disqualify Bee/ })).toBeNull();
    cleanup();
    await openPhase(withProposal, 'mains');
    expect(screen.getByRole('button', { name: 'Disqualify Bee from A-Main' })).toBeVisible();
  });

  it('a heat that stands alone has a page too', async () => {
    await openPhase(eventView({ phases: [heat('solo', 'Shootout', ['p-3c'])] }), 'solo');
    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Shootout');
    expect(screen.getByRole('list', { name: 'Lineup of Shootout' })).toHaveTextContent('Cat');
  });
});
