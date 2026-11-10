/**
 * The event page's competition part: the phase tree, and the dialogs that add a heat and a
 * bracket. Rendered against a stand-in live store.
 */
import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { BracketRequest, Command, EventMeta, EventView, Pilot } from '@gridfpv/types';
import EventPage from '../src/screens/EventPage.svelte';
import { EventLive, type LiveSession } from '../src/lib/eventLive.svelte.js';
import { formatByKey } from '../src/lib/formats.js';
import { eventView, group, heat, race, timerView } from './competition.js';

const EVENT: EventMeta = { id: 'evt-77', name: 'Club Night', created_at: 0, persistent: true };
const PILOTS: Pilot[] = [
  { id: 'p-1a', callsign: 'Ace', vtx_types: [] },
  { id: 'p-2b', callsign: 'Bee', vtx_types: [] },
  { id: 'p-3c', callsign: 'Cat', vtx_types: [] }
];

async function open(view: EventView, over: Partial<LiveSession> = {}) {
  const session: LiveSession = {
    getEventState: vi.fn(async () => view),
    subscribeChanges: vi.fn(() => () => {}),
    sendCommand: vi.fn(async () => ({ cursor: view.cursor })),
    createBracket: vi.fn(async () => ({ cursor: view.cursor })),
    postPass: vi.fn(async () => ({ cursor: view.cursor })),
    listPilots: vi.fn(async () => PILOTS),
    listTimers: vi.fn(async () => []),
    ...over
  };
  const live = new EventLive(session, EVENT.id);
  await live.reload();
  await live.reloadPilots();
  await live.reloadTimers();
  const onrun = vi.fn();
  render(EventPage, { event: EVENT, live, onhome: vi.fn(), onevents: vi.fn(), onrun });
  return { session, onrun };
}

describe('EventPage phases', () => {
  it('says so when there is nothing to race yet', async () => {
    await open(eventView());
    expect(
      screen.getByText('Nothing to race yet. Add a heat or a bracket to start.')
    ).toBeVisible();
    expect(screen.queryByText(/being rebuilt/)).toBeNull();
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
    const rows = within(screen.getByRole('list', { name: 'Phases' }))
      .getAllByRole('listitem')
      .filter((item) => item.classList.contains('phase'));
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
});

describe('Add a heat', () => {
  it('sends PhaseDefined with the pilots in seat order and the chosen format', async () => {
    const { session } = await open(eventView());
    await fireEvent.click(screen.getByRole('button', { name: '+ Add heat' }));
    const form = await screen.findByRole('form', { name: 'Add a heat' });
    await fireEvent.input(within(form).getByLabelText('Heat name'), {
      target: { value: 'Qualifying Heat 1' }
    });
    // Bee first: Bee gets Seat 1.
    await fireEvent.click(within(form).getByRole('button', { name: 'Bee' }));
    await fireEvent.click(within(form).getByRole('button', { name: 'Ace' }));
    await fireEvent.change(within(form).getByLabelText('Format'), {
      target: { value: 'first-to-3' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add heat' }));

    await waitFor(() => expect(session.sendCommand).toHaveBeenCalled());
    const command = vi.mocked(session.sendCommand).mock.calls[0][1] as Extract<
      Command,
      { Record: unknown }
    >;
    const fact = command.Record as { PhaseDefined: { def: Record<string, unknown> } };
    const { id, ...def } = fact.PhaseDefined.def;
    expect(id).toMatch(/^qualifying-heat-1-[0-9a-z]{4}$/);
    expect(def).toEqual({
      label: 'Qualifying Heat 1',
      kind: {
        Heat: {
          lineup: [
            { adapter: 'bench', seat: 'seat-1', source: { Pilot: 'p-2b' } },
            { adapter: 'bench', seat: 'seat-2', source: { Pilot: 'p-1a' } }
          ]
        }
      },
      format: formatByKey('first-to-3')
    });
    // It closes once the Director took it.
    await waitFor(() => expect(screen.queryByRole('form', { name: 'Add a heat' })).toBeNull());
  });

  it('asks for a name and a pilot before sending anything', async () => {
    const { session } = await open(eventView());
    await fireEvent.click(screen.getByRole('button', { name: '+ Add heat' }));
    await screen.findByRole('form', { name: 'Add a heat' });
    await fireEvent.click(screen.getByRole('button', { name: 'Add heat' }));
    expect(await screen.findByText('Give the heat a name.')).toBeVisible();
    expect(session.sendCommand).not.toHaveBeenCalled();
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
      format: formatByKey('first-to-3')
    });
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
  const optionTexts = (select: HTMLSelectElement) =>
    [...select.options].map((option) => option.textContent?.trim());

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
    const command = vi.mocked(session.sendCommand).mock.calls[0][1] as {
      Record: { PhaseDefined: { def: { kind: unknown } } };
    };
    expect(command.Record.PhaseDefined.def.kind).toEqual({
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
    const command = vi.mocked(session.sendCommand).mock.calls[0][1] as {
      Record: { PhaseDefined: { def: { kind: unknown } } };
    };
    expect(command.Record.PhaseDefined.def.kind).toEqual({
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

  it("names a heat's seats from its timer in the phase list, even when it is not connected", async () => {
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
