/**
 * The event page beyond the phase tree: the Marshal link and its count, the run queue, the
 * rulings on heats and phases (lock, unlock, reopen, set position, disqualify, reinstate, the
 * proposed disqualification), and the classes with their rosters and standings. Rendered
 * against a stand-in live store.
 */
import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { Class, Command, EventMeta, EventView, Pilot } from '@gridfpv/types';
import EventPage from '../src/screens/EventPage.svelte';
import { EventLive, type LiveSession } from '../src/lib/eventLive.svelte.js';
import { eventView, group, heat, race } from './competition.js';

const EVENT: EventMeta = { id: 'evt-77', name: 'Club Night', created_at: 0, persistent: true };
const PILOTS: Pilot[] = [
  { id: 'p-1a', callsign: 'Ace', vtx_types: [] },
  { id: 'p-2b', callsign: 'Bee', vtx_types: [] },
  { id: 'p-3c', callsign: 'Cat', vtx_types: [] }
];
const CLASSES: Class[] = [
  { id: 'cls-open', name: 'Open', source: 'Custom' },
  { id: 'cls-spec', name: 'Spec', source: 'Custom' },
  { id: 'cls-old', name: 'Old', source: 'Custom', hidden: true }
];

const placed = (pilot: string, position: number, over = {}) => ({
  pilot,
  position,
  disqualified: false,
  position_set: false,
  flew: true,
  wins: 0,
  points: 0,
  laps: 3,
  ...over
});

async function open(view: EventView, over: Partial<LiveSession> = {}) {
  const session: LiveSession = {
    getEventState: vi.fn(async () => view),
    subscribeChanges: vi.fn(() => () => {}),
    sendCommand: vi.fn(async () => ({ cursor: view.cursor })),
    createBracket: vi.fn(async () => ({ cursor: view.cursor })),
    postPass: vi.fn(async () => ({ cursor: view.cursor })),
    listPilots: vi.fn(async () => PILOTS),
    listTimers: vi.fn(async () => []),
    listClasses: vi.fn(async () => CLASSES),
    getRaceSignal: vi.fn(async (_id: string, id: string) => ({ race: id, seats: [] })),
    ...over
  };
  const live = new EventLive(session, EVENT.id);
  await live.reload();
  await live.reloadPilots();
  await live.reloadTimers();
  await live.reloadClasses();
  const nav = { onhome: vi.fn(), onevents: vi.fn(), onrun: vi.fn(), onmarshal: vi.fn() };
  render(EventPage, { event: EVENT, live, ...nav });
  return { session, nav };
}

const sent = (session: LiveSession): Command[] =>
  vi.mocked(session.sendCommand).mock.calls.map((call) => call[1]);

/** Every raw handle in these fixtures. None may be on the page. */
function expectNoRawIds() {
  const text = document.body.textContent ?? '';
  for (const raw of ['finals', 'q-1', 'q-2', '#1', 'bench', 'seat-1', 'p-1a', 'p-2b', 'cls-']) {
    expect(text).not.toContain(raw);
  }
}

describe('the Marshal link', () => {
  it('opens the Marshal page, with no count when nothing needs a look', async () => {
    const { nav } = await open(eventView());
    expect(screen.queryByTestId('marshal-count')).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Marshal' }));
    expect(nav.onmarshal).toHaveBeenCalled();
  });

  it('counts the races that need a look or have an open protest', async () => {
    await open(
      eventView({
        phases: [heat('q-1', 'Qualifying Heat 1', ['p-1a'])],
        races: [
          race('q-1', 1, 'Saved', { needs_marshal: true }),
          race('q-1', 2, 'Saved'),
          race('q-1', 3, 'Saved')
        ],
        protests: [{ protest: 'pr-1', race: 'q-1#2' }]
      })
    );
    expect(screen.getByTestId('marshal-count')).toHaveTextContent('2');
    expect(screen.getByRole('button', { name: 'Marshal, 2 races need a look' })).toBeVisible();
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
    const { nav } = await open(view);
    expect(screen.getByText(/The order is a suggestion\./)).toBeVisible();
    const items = within(screen.getByRole('list', { name: 'Up next' })).getAllByRole('listitem');
    expect(items).toHaveLength(2);
    expect(items[0]).toHaveTextContent('Qualifying Heat 1');
    expect(items[0]).toHaveTextContent('Ace, Bee');
    expect(items[1]).toHaveTextContent('Cat, Open seat');
    await fireEvent.click(screen.getByRole('button', { name: 'Run Qualifying Heat 2 next' }));
    expect(nav.onrun).toHaveBeenCalledWith('q-2');
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

  it('says so when no heat is ready', async () => {
    await open(eventView({ phases: view.phases, queue: [] }));
    expect(screen.getByText('No heat is ready to race.')).toBeVisible();
  });
});

describe('heat and phase rulings', () => {
  const phases = (over = {}, top = {}) => [
    group('finals', 'Finals', undefined, {
      ranking: [placed('p-1a', 1), placed('p-2b', 2)],
      ...top
    }),
    heat('q-1', 'Final Heat 1', ['p-1a', 'p-2b'], {
      parent: 'finals',
      complete: true,
      director_done: true,
      ranking: [placed('p-1a', 1, { position_set: true }), placed('p-2b', 2)],
      ...over
    })
  ];

  it('locks a phase, and unlocks only the phase that carries the lock', async () => {
    const first = await open(eventView({ phases: phases() }));
    await fireEvent.click(screen.getByRole('button', { name: 'Lock Finals' }));
    await waitFor(() =>
      expect(sent(first.session)).toEqual([{ Record: { PhaseLocked: { phase: 'finals' } } }])
    );
    document.body.innerHTML = '';

    const second = await open(
      eventView({ phases: phases({ locked: true }, { locked: true, locked_here: true }) })
    );
    // The heat is locked from above: it offers neither Lock nor Unlock, and no ruling.
    expect(screen.queryByRole('button', { name: /Lock Final Heat 1/ })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Unlock Final Heat 1' })).toBeNull();
    expect(screen.queryByRole('button', { name: /Set the position/ })).toBeNull();
    expect(screen.queryByRole('button', { name: /Disqualify/ })).toBeNull();
    expect(screen.queryByRole('button', { name: /Reopen/ })).toBeNull();
    expect(screen.getAllByText(/The results are official/).length).toBeGreaterThan(0);
    await fireEvent.click(screen.getByRole('button', { name: 'Unlock Finals' }));
    await waitFor(() =>
      expect(sent(second.session)).toEqual([{ Record: { PhaseUnlocked: { phase: 'finals' } } }])
    );
  });

  it('reopens a heat the director called done', async () => {
    const { session } = await open(eventView({ phases: phases() }));
    await fireEvent.click(screen.getByRole('button', { name: 'Reopen Final Heat 1' }));
    await waitFor(() =>
      expect(sent(session)).toEqual([{ Record: { HeatReopened: { heat: 'q-1' } } }])
    );
  });

  it('sets a pilot position in a heat with a note, marks hand-set ones, and clears them', async () => {
    const { session } = await open(eventView({ phases: phases() }));
    const ranking = screen.getByRole('list', { name: 'Ranking of Final Heat 1' });
    const rows = within(ranking).getAllByRole('listitem');
    expect(rows[0]).toHaveTextContent('Ace');
    expect(rows[0]).toHaveTextContent('Set by hand');
    expect(rows[1]).not.toHaveTextContent('Set by hand');
    // Positions are set in a heat, never in a phase above it.
    expect(screen.queryByRole('button', { name: /Set the position of Ace in Finals/ })).toBeNull();

    await fireEvent.click(
      screen.getByRole('button', { name: 'Set the position of Bee in Final Heat 1' })
    );
    const form = await screen.findByRole('form', { name: "Set Bee's position" });
    expect(within(form).getByLabelText('Position')).toHaveValue('2');
    await fireEvent.change(within(form).getByLabelText('Position'), { target: { value: '1' } });
    await fireEvent.input(within(form).getByLabelText('Note'), {
      target: { value: 'Video review' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Set position' }));
    await waitFor(() =>
      expect(sent(session)).toEqual([
        {
          Record: {
            PositionSet: { heat: 'q-1', pilot: 'p-2b', position: 1, note: 'Video review' }
          }
        }
      ])
    );
    await waitFor(() =>
      expect(screen.queryByRole('form', { name: "Set Bee's position" })).toBeNull()
    );

    await fireEvent.click(
      screen.getByRole('button', { name: 'Clear the position of Ace in Final Heat 1' })
    );
    await waitFor(() =>
      expect(sent(session)[1]).toEqual({
        Record: { PositionCleared: { heat: 'q-1', pilot: 'p-1a' } }
      })
    );
    expectNoRawIds();
  });

  it('disqualifies from a phase through the dialog: the next pilot takes the place by default', async () => {
    const { session } = await open(eventView({ phases: phases() }));
    await fireEvent.click(screen.getByRole('button', { name: 'Disqualify Bee from Finals' }));
    const form = await screen.findByRole('form', { name: 'Disqualify Bee' });
    expect(within(form).getByLabelText('The next pilot takes their place')).toBeChecked();
    await fireEvent.click(screen.getByRole('button', { name: 'Disqualify' }));
    await waitFor(() =>
      expect(sent(session)).toEqual([
        { Record: { Disqualified: { phase: 'finals', pilot: 'p-2b', keep_place_empty: false } } }
      ])
    );
    await waitFor(() => expect(screen.queryByRole('form', { name: 'Disqualify Bee' })).toBeNull());
  });

  it('can keep the place empty and give a reason; a refusal stays in the dialog', async () => {
    const SAID = 'Finals is locked. Unlock it first.';
    const sendCommand = vi
      .fn<LiveSession['sendCommand']>()
      .mockRejectedValueOnce(Object.assign(new Error(SAID), { status: 409, code: 'Refused' }))
      .mockResolvedValue({ cursor: 1 });
    const { session } = await open(eventView({ phases: phases() }), { sendCommand });
    await fireEvent.click(screen.getByRole('button', { name: 'Disqualify Ace from Finals' }));
    const form = await screen.findByRole('form', { name: 'Disqualify Ace' });
    await fireEvent.input(within(form).getByLabelText('Reason'), {
      target: { value: 'Two false starts' }
    });
    await fireEvent.click(within(form).getByLabelText('Keep their place empty'));
    await fireEvent.click(screen.getByRole('button', { name: 'Disqualify' }));
    expect(await within(form).findByText(SAID)).toBeVisible();
    await fireEvent.click(screen.getByRole('button', { name: 'Disqualify' }));
    await waitFor(() => expect(sent(session)).toHaveLength(2));
    expect(sent(session)[1]).toEqual({
      Record: {
        Disqualified: {
          phase: 'finals',
          pilot: 'p-1a',
          reason: 'Two false starts',
          keep_place_empty: true
        }
      }
    });
  });

  it('reinstates a disqualified pilot', async () => {
    const { session } = await open(
      eventView({
        phases: phases(
          {},
          { ranking: [placed('p-1a', 1), placed('p-2b', 2, { disqualified: true })] }
        )
      })
    );
    const row = within(screen.getByRole('list', { name: 'Ranking of Finals' })).getAllByRole(
      'listitem'
    )[1];
    expect(row).toHaveTextContent('Disqualified');
    await fireEvent.click(screen.getByRole('button', { name: 'Reinstate Bee in Finals' }));
    await waitFor(() =>
      expect(sent(session)).toEqual([
        { Record: { Reinstated: { phase: 'finals', pilot: 'p-2b' } } }
      ])
    );
  });

  it('shows a proposed disqualification as a notice; nothing is sent until the director decides', async () => {
    const { session } = await open(
      eventView({
        phases: phases(),
        proposed_disqualifications: [{ phase: 'finals', pilot: 'p-2b', false_starts: 2, limit: 2 }]
      })
    );
    const notice = screen.getByText('Bee has 2 false starts in Finals. The limit is 2.');
    expect(notice).toBeVisible();
    expect(session.sendCommand).not.toHaveBeenCalled();
    const alert = notice.closest('[role="alert"]') as HTMLElement;
    await fireEvent.click(
      within(alert).getByRole('button', { name: 'Disqualify Bee from Finals' })
    );
    expect(await screen.findByRole('form', { name: 'Disqualify Bee' })).toBeVisible();
    expect(session.sendCommand).not.toHaveBeenCalled();
  });

  it("a refused ruling is shown in the Director's words", async () => {
    const SAID = 'Finals holds an open protest. Resolve it before locking.';
    await open(eventView({ phases: phases() }), {
      sendCommand: vi.fn(async () =>
        Promise.reject(Object.assign(new Error(SAID), { status: 409, code: 'Refused' }))
      )
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Lock Finals' }));
    expect(await screen.findByText(SAID)).toBeVisible();
  });
});

describe('classes at the event', () => {
  const roster = (over = {}) => ({
    class: 'cls-open',
    pilots: ['p-1a', 'p-2b'],
    counted_phases: ['finals'],
    standings: [
      { pilot: 'p-2b', position: 1, placed_by: 'finals' },
      { pilot: 'p-1a', position: 2 }
    ],
    ...over
  });
  const phases = [
    group('finals', 'Finals'),
    group('quali', 'Qualifying'),
    heat('q-1', 'Qualifying Heat 1', ['p-1a'], { parent: 'quali' })
  ];

  it('says so when no class is in the event', async () => {
    await open(eventView());
    expect(screen.getByText(/No class is in this event yet\./)).toBeVisible();
  });

  it('shows each class by name with its standings and what placed each pilot', async () => {
    await open(eventView({ phases, rosters: [roster()] }));
    const open_ = screen.getByRole('listitem', { name: 'Open' });
    expect(open_).toHaveTextContent('2 pilots');
    expect(within(open_).getByTestId('counted')).toHaveTextContent('Standings come from Finals.');
    const rows = within(within(open_).getByRole('table', { name: 'Standings of Open' }))
      .getAllByRole('row')
      .map((row) =>
        [...row.querySelectorAll('th, td')].map((cell) => cell.textContent?.trim()).join(' | ')
      );
    expect(rows).toEqual([
      'Position | Pilot | Placed by',
      '1 | Bee | Finals',
      '2 | Ace | Not placed yet'
    ]);
    expectNoRawIds();
  });

  it('adds a class from the directory with a roster from the pilot directory', async () => {
    const { session } = await open(eventView({ phases, rosters: [roster()] }));
    await fireEvent.click(screen.getByRole('button', { name: '+ Add class' }));
    const form = await screen.findByRole('form', { name: 'Add a class' });
    // Open is already in the event and Old is hidden, so only Spec is offered.
    const options = within(within(form).getByLabelText('Class')).getAllByRole('option');
    expect(options.map((option) => option.textContent)).toEqual(['Spec']);
    await fireEvent.click(within(form).getByRole('button', { name: 'Cat' }));
    await fireEvent.click(within(form).getByRole('button', { name: 'Ace' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Add class' }));
    await waitFor(() =>
      expect(sent(session)).toEqual([
        { Record: { RosterSet: { class: 'cls-spec', pilots: ['p-3c', 'p-1a'] } } }
      ])
    );
    await waitFor(() => expect(screen.queryByRole('form', { name: 'Add a class' })).toBeNull());
  });

  it('edits a roster, starting from the pilots in it', async () => {
    const { session } = await open(eventView({ phases, rosters: [roster()] }));
    await fireEvent.click(screen.getByRole('button', { name: 'Edit the roster of Open' }));
    const form = await screen.findByRole('form', { name: 'Roster of Open' });
    await fireEvent.click(within(form).getByRole('button', { name: 'Remove Ace' }));
    await fireEvent.click(within(form).getByRole('button', { name: 'Cat' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Save roster' }));
    await waitFor(() =>
      expect(sent(session)).toEqual([
        { Record: { RosterSet: { class: 'cls-open', pilots: ['p-2b', 'p-3c'] } } }
      ])
    );
  });

  it('chooses which phases count and in what order', async () => {
    const { session } = await open(eventView({ phases, rosters: [roster()] }));
    await fireEvent.click(screen.getByRole('button', { name: 'Choose what counts for Open' }));
    const dialog = await screen.findByLabelText('What counts for Open');
    expect(within(dialog).getByRole('list', { name: 'Phases that count' })).toHaveTextContent(
      'Finals'
    );
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Qualifying' }));
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Move Qualifying up' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() =>
      expect(sent(session)).toEqual([
        { Record: { StandingsSet: { class: 'cls-open', phases: ['quali', 'finals'] } } }
      ])
    );
  });

  it('a new heat can start from a class roster, and the pilots can still be changed', async () => {
    const { session } = await open(eventView({ phases, rosters: [roster()] }));
    await fireEvent.click(screen.getByRole('button', { name: '+ Add heat' }));
    const form = await screen.findByRole('form', { name: 'Add a heat' });
    await fireEvent.input(within(form).getByLabelText('Heat name'), {
      target: { value: 'Open 1' }
    });
    await fireEvent.change(within(form).getByLabelText('Start from a class'), {
      target: { value: 'cls-open' }
    });
    const chosen = within(form).getByRole('list', { name: 'Chosen pilots' });
    expect(within(chosen).getAllByRole('listitem')).toHaveLength(2);
    await fireEvent.click(within(form).getByRole('button', { name: 'Cat' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Add heat' }));
    await waitFor(() => expect(session.sendCommand).toHaveBeenCalled());
    const fact = (sent(session)[0] as { Record: { PhaseDefined: { def: { kind: unknown } } } })
      .Record.PhaseDefined.def.kind as { Heat: { lineup: { source: unknown }[] } };
    expect(fact.Heat.lineup.map((seat) => seat.source)).toEqual([
      { Pilot: 'p-1a' },
      { Pilot: 'p-2b' },
      { Pilot: 'p-3c' }
    ]);
  });

  it('a new bracket can start from a class roster as its field', async () => {
    const createBracket = vi.fn<LiveSession['createBracket']>(async () => ({ cursor: 1 }));
    await open(eventView({ phases, rosters: [roster()] }), { createBracket });
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
