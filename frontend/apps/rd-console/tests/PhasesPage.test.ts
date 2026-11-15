/**
 * The Phases page: the empty state, "Up next", the phase tree, and the dialogs that add a
 * heat, a phase and a bracket. Rendered against a stand-in live store. The rulings made in
 * the tree are in `PhaseRulings.test.ts`.
 */
import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { BracketRequest, Class, Command, EventView, PhaseDef, Pilot } from '@gridfpv/types';
import PhasesPage from '../src/screens/PhasesPage.svelte';
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
import { openLive } from './support.js';

const PILOTS: Pilot[] = [
  { id: 'p-1a', callsign: 'Ace', vtx_types: [] },
  { id: 'p-2b', callsign: 'Bee', vtx_types: [] },
  { id: 'p-3c', callsign: 'Cat', vtx_types: [] }
];
const CLASSES: Class[] = [
  { id: 'cls-open', name: 'Open', source: 'Custom' },
  { id: 'cls-spec', name: 'Spec', source: 'Custom' }
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
  within(screen.getByRole('list', { name: 'Phases' }))
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
  it('says so when there is nothing to race yet, and still offers the three ways to add', async () => {
    await open(eventView());
    expect(screen.getByTestId('no-phases')).toHaveTextContent(/^Nothing to race yet\./);
    expect(screen.queryByRole('list', { name: 'Phases' })).toBeNull();
    expect(screen.queryByRole('heading', { name: 'Up next' })).toBeNull();
    for (const name of ['+ Add heat', '+ Add phase', '+ Add bracket']) {
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

  it('shows the tree with a status per phase and each heat lineup by name', async () => {
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
    const { onrun } = await open(view);
    const rows = phaseRows();
    expect(rows.map((row) => row.style.getPropertyValue('--depth'))).toEqual([
      '0',
      '1',
      '2',
      '1',
      '0'
    ]);
    expect(rows[0]).toHaveTextContent('Finals');
    expect(rows[0]).toHaveTextContent('Racing');
    expect(rows[3]).toHaveTextContent('Waiting');
    expect(rows[4]).toHaveTextContent('Complete');

    const lineup = screen.getByRole('list', { name: 'Lineup of Winners Round 1 Heat 1' });
    expect(lineup).toHaveTextContent('Seat 1');
    expect(lineup).toHaveTextContent('Ace');
    expect(lineup).toHaveTextContent('Bee');
    const final = screen.getByRole('list', { name: 'Lineup of Final' });
    expect(final).toHaveTextContent('Winner of Winners Round 1 Heat 1');
    expect(final).toHaveTextContent('2nd of Winners Round 1 Heat 1');

    // A waiting heat cannot be run; a racing one can.
    expect(screen.queryByRole('button', { name: 'Run Final' })).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Run Winners Round 1 Heat 1' }));
    expect(onrun).toHaveBeenCalledWith('finals/w1/h1');

    const text = document.body.textContent ?? '';
    for (const raw of ['finals/', 'quali-1', '#1', 'bench', 'seat-1', 'p-1a']) {
      expect(text).not.toContain(raw);
    }
  });

  it('names the format in force on each row, and says when it comes from the phase above', async () => {
    const quali = group('quali', 'Qualifying', undefined, { format: TWO_MINUTE_QUALI });
    quali.def.format = TWO_MINUTE_QUALI;
    const own = heat('q-1', 'Qualifying Heat 1', ['p-1a'], { parent: 'quali', format: FIRST_TO_3 });
    own.def.format = FIRST_TO_3;
    const follows = heat('q-2', 'Qualifying Heat 2', ['p-2b'], {
      parent: 'quali',
      format: TWO_MINUTE_QUALI
    });
    await open(
      eventView({ phases: [quali, own, follows, group('finals', 'Finals', undefined, {})] })
    );
    const format = (row: HTMLElement) => row.querySelector('.format');
    const [qualiRow, ownRow, followsRow, finalsRow] = phaseRows();
    expect(format(qualiRow)).toHaveTextContent('2 minute qualifying');
    expect(format(qualiRow)).not.toHaveAttribute('title');
    expect(format(ownRow)).toHaveTextContent('First to 3 laps');
    expect(format(ownRow)).not.toHaveAttribute('title');
    expect(format(followsRow)).toHaveTextContent('2 minute qualifying');
    expect(format(followsRow)).toHaveAttribute('title', 'From the phase above');
    // A phase with no format in force names none.
    expect(format(finalsRow)).toBeNull();
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
    expect(items[1]).toHaveTextContent('Cat, Open seat');
    await fireEvent.click(screen.getByRole('button', { name: 'Run Qualifying Heat 2 next' }));
    expect(onrun).toHaveBeenCalledWith('q-2');
    expectNoRawIds();
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
    const { session } = await open(eventView());
    await fireEvent.click(screen.getByRole('button', { name: '+ Add heat' }));
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
    // With no phase that holds phases there is nothing to put it under.
    expect(within(form).queryByLabelText('Under')).toBeNull();
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
      // A copy of the saved format, with no handle of the saved one.
      format: FIRST_TO_3
    });
    // It closes once the Director took it.
    await waitFor(() => expect(screen.queryByRole('form', { name: 'Add a heat' })).toBeNull());
  });

  it('starts on the first saved format when the heat stands alone', async () => {
    const { session } = await open(eventView());
    await fireEvent.click(screen.getByRole('button', { name: '+ Add heat' }));
    const form = await screen.findByRole('form', { name: 'Add a heat' });
    expect(within(form).getByLabelText('Format')).toHaveValue('fmt-practice-9k2d');
    await fireEvent.input(within(form).getByLabelText('Heat name'), { target: { value: 'Fly' } });
    await fireEvent.click(within(form).getByRole('button', { name: 'Ace' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Add heat' }));
    await waitFor(() => expect(session.sendCommand).toHaveBeenCalled());
    expect(definedPhase(sent(session)[0], 'fly').format).toEqual(OPEN_PRACTICE);
  });

  it('asks for a name and a pilot before sending anything', async () => {
    const { session } = await open(eventView());
    await fireEvent.click(screen.getByRole('button', { name: '+ Add heat' }));
    const form = await screen.findByRole('form', { name: 'Add a heat' });
    await fireEvent.click(screen.getByRole('button', { name: 'Add heat' }));
    expect(await screen.findByText('Give the heat a name.')).toBeVisible();
    await fireEvent.input(within(form).getByLabelText('Heat name'), { target: { value: 'H1' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add heat' }));
    expect(await within(form).findByRole('alert')).toHaveTextContent('Choose at least one pilot.');
    expect(session.sendCommand).not.toHaveBeenCalled();
  });

  it('asks for a format when none is saved and no phase gives one', async () => {
    const { session } = await open(eventView(), { listFormats: vi.fn(async () => []) });
    await fireEvent.click(screen.getByRole('button', { name: '+ Add heat' }));
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
    await open(eventView(), {
      sendCommand: vi.fn(async () =>
        Promise.reject(Object.assign(new Error(SAID), { status: 409, code: 'Refused' }))
      )
    });
    await fireEvent.click(screen.getByRole('button', { name: '+ Add heat' }));
    const form = await screen.findByRole('form', { name: 'Add a heat' });
    await fireEvent.input(within(form).getByLabelText('Heat name'), { target: { value: 'H1' } });
    await fireEvent.click(within(form).getByRole('button', { name: 'Ace' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Add heat' }));
    expect(await within(form).findByRole('alert')).toHaveTextContent(SAID);
    expect(within(form).getByLabelText('Heat name')).toHaveValue('H1');
  });

  it('reads the saved formats again when the dialog opens', async () => {
    const { session } = await open(eventView());
    const before = vi.mocked(session.listFormats).mock.calls.length;
    await fireEvent.click(screen.getByRole('button', { name: '+ Add heat' }));
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
    expect(optionTexts(under)).toEqual([
      'The top of the event',
      'Qualifying',
      'Mains',
      'Mains › A-Main (locked)'
    ]);
    expect(under.options[3].disabled).toBe(true);
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

  it('+ Add heat starts at the top of the event, and the director can put it under a phase', async () => {
    const { session } = await open(eventView({ phases: phases() }));
    await fireEvent.click(screen.getByRole('button', { name: '+ Add heat' }));
    const form = await screen.findByRole('form', { name: 'Add a heat' });
    expect(within(form).getByLabelText('Under')).toHaveValue('');
    // At the top there is no phase to follow: a saved format is picked.
    expect(within(form).getByLabelText('Format')).toHaveValue('fmt-practice-9k2d');
    await fireEvent.change(within(form).getByLabelText('Under'), { target: { value: 'mains' } });
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
});

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
      rosters: [{ class: 'cls-open', pilots: ['p-1a'], counted_phases: [], standings: [] }]
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
    await fireEvent.click(screen.getByRole('button', { name: '+ Add bracket' }));
    const form = await screen.findByRole('form', { name: 'Add a bracket' });
    await fireEvent.input(within(form).getByLabelText('Bracket name'), {
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
    await fireEvent.click(screen.getByRole('button', { name: 'Add bracket' }));

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
    await fireEvent.click(screen.getByRole('button', { name: '+ Add bracket' }));
    const form = await screen.findByRole('form', { name: 'Add a bracket' });
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
    await fireEvent.click(screen.getByRole('button', { name: 'Add bracket' }));
    expect(await within(form).findByRole('alert')).toHaveTextContent(
      'Pick a format that ranks the heat. It decides who moves on.'
    );
    expect(session.createBracket).not.toHaveBeenCalled();
  });

  it('can go under a phase', async () => {
    const { session } = await open(eventView({ phases: [group('mains', 'Mains')] }));
    const form = await fill();
    await fireEvent.change(within(form).getByLabelText('Under'), { target: { value: 'mains' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add bracket' }));
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
    await fireEvent.click(screen.getByRole('button', { name: 'Add bracket' }));
    expect(await within(form).findByRole('alert')).toHaveTextContent(SAID);
    expect(screen.getByRole('form', { name: 'Add a bracket' })).toBeVisible();
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
    await open(eventView());
    await fireEvent.click(screen.getByRole('button', { name: '+ Add heat' }));
    const form = await screen.findByRole('form', { name: 'Add a heat' });
    expect(optionTexts(timedBy(form))).toEqual(['Bench (no timer: crossings are logged by hand)']);
    expect(timedBy(form).value).toBe('bench');
    expect(within(form).getByText(/Up to 8\./)).toBeVisible();
  });

  it('starts on the first connected timer and puts the lineup on its seats', async () => {
    const { session } = await open(eventView(), { listTimers: vi.fn(async () => [OFF, GATE]) });
    await fireEvent.click(screen.getByRole('button', { name: '+ Add heat' }));
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
    const { session } = await open(eventView(), { listTimers: vi.fn(async () => [GATE]) });
    await fireEvent.click(screen.getByRole('button', { name: '+ Add heat' }));
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
    await fireEvent.click(screen.getByRole('button', { name: '+ Add bracket' }));
    const form = await screen.findByRole('form', { name: 'Add a bracket' });
    expect(timedBy(form).value).toBe('gate-x1y2');
    const sizes = within(form).getByLabelText('Pilots in each heat') as HTMLSelectElement;
    expect([...sizes.options].map((option) => option.value)).toEqual(['2', '3']);
    // It started on 4, which this timer cannot seat, so it is pulled back to 3.
    await waitFor(() => expect(sizes.value).toBe('3'));

    await fireEvent.input(within(form).getByLabelText('Bracket name'), {
      target: { value: 'Finals' }
    });
    for (const callsign of ['Cat', 'Ace', 'Bee']) {
      await fireEvent.click(within(form).getByRole('button', { name: callsign }));
    }
    await fireEvent.click(screen.getByRole('button', { name: 'Add bracket' }));
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
          standings: []
        }
      ]
    });

  it('a new heat can start from a class roster, and the pilots can still be changed', async () => {
    const { session } = await open(view());
    await fireEvent.click(screen.getByRole('button', { name: '+ Add heat' }));
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

  it('a new bracket can start from a class roster as its field', async () => {
    const createBracket = vi.fn<LiveSession['createBracket']>(async () => ({ cursor: 1 }));
    await open(view(), { createBracket });
    await fireEvent.click(screen.getByRole('button', { name: '+ Add bracket' }));
    const form = await screen.findByRole('form', { name: 'Add a bracket' });
    await fireEvent.input(within(form).getByLabelText('Bracket name'), {
      target: { value: 'Open Finals' }
    });
    await fireEvent.change(within(form).getByLabelText('Start from a class'), {
      target: { value: 'cls-open' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add bracket' }));
    await waitFor(() => expect(createBracket).toHaveBeenCalled());
    expect(createBracket.mock.calls[0][1].field).toEqual([{ Imported: ['p-1a', 'p-2b'] }]);
  });
});
