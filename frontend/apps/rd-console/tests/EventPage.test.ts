/**
 * The Event page: the open event's details and the dialog that edits them, what the event
 * holds in a few numbers, the lap rules every race of the event runs under, and the classes
 * with their rosters and standings. Rendered against a stand-in live store.
 */
import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { Class, Command, EventMeta, EventView, Pilot } from '@gridfpv/types';
import EventPage from '../src/screens/EventPage.svelte';
import type { LiveSession } from '../src/lib/eventLive.svelte.js';
import { eventView, group, heat, race } from './competition.js';
import { makeTestSession, openLive, type SessionSeams } from './support.js';

const EVENT: EventMeta = {
  id: 'evt-77',
  name: 'Club Night',
  created_at: 0,
  persistent: true,
  date: '2026-11-14',
  location: 'The old gym'
};
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

async function open(
  view: EventView,
  over: Partial<LiveSession> = {},
  seams: SessionSeams = {},
  event: EventMeta = EVENT
) {
  const { session: liveSession, live } = await openLive(view, {
    listPilots: vi.fn(async () => PILOTS),
    listClasses: vi.fn(async () => CLASSES),
    ...over
  });
  const { session } = makeTestSession({ event, ...seams });
  render(EventPage, { session, event, live });
  return { session, liveSession, live };
}

const sent = (session: LiveSession): Command[] =>
  vi.mocked(session.sendCommand).mock.calls.map((call) => call[1]);

/** Every raw handle in these fixtures. None may be on the page. */
function expectNoRawIds() {
  const text = document.body.textContent ?? '';
  for (const raw of ['evt-77', 'finals', 'q-1', '#1', 'bench', 'seat-1', 'p-1a', 'p-2b', 'cls-']) {
    expect(text).not.toContain(raw);
  }
}

describe('EventPage details', () => {
  it('is titled with the event name and shows each detail, with a dash for one not set', async () => {
    await open(eventView());
    expect(screen.getByRole('heading', { level: 1, name: /Club Night/ })).toBeVisible();
    const rows = [...screen.getByLabelText('Event details').children].map((row) =>
      (row.textContent ?? '').replace(/\s+/g, ' ').trim()
    );
    expect(rows).toEqual([
      'Date 2026-11-14',
      'Location The old gym',
      'Organizer —',
      'Description —'
    ]);
    expectNoRawIds();
  });

  it('has a ? beside the title that opens the help for this page', async () => {
    await open(eventView());
    expect(screen.getByRole('link', { name: 'Help for Club Night' })).toHaveAttribute(
      'href',
      '#/help/event'
    );
  });

  it('counts the classes, the pilots in them once each, the heats and the saved races', async () => {
    await open(
      eventView({
        phases: [
          group('quali', 'Qualifying'),
          heat('q-1', 'Qualifying Heat 1', ['p-1a'], { parent: 'quali' }),
          heat('q-2', 'Qualifying Heat 2', ['p-2b'], { parent: 'quali' })
        ],
        races: [race('q-1', 1, 'Saved'), race('q-1', 2, 'Discarded'), race('q-2', 1, 'Running')],
        rosters: [
          { class: 'cls-open', pilots: ['p-1a', 'p-2b'], counted_phases: [], standings: [] },
          { class: 'cls-spec', pilots: ['p-2b', 'p-3c'], counted_phases: [], standings: [] }
        ]
      })
    );
    const rows = [...screen.getByLabelText('In this event').children].map((row) =>
      (row.textContent ?? '').replace(/\s+/g, ' ').trim()
    );
    // Bee is in both classes and is counted once. A group is not a heat. Only saved races count.
    expect(rows).toEqual(['Classes 2', 'Pilots in them 3', 'Heats 2', 'Saved races 1']);
    expect(screen.getByRole('link', { name: 'Phases page' })).toHaveAttribute(
      'href',
      '#/event/phases'
    );
  });

  it('no longer holds the phases, the run queue or the buttons that add a heat', async () => {
    await open(
      eventView({
        phases: [heat('q-1', 'Qualifying Heat 1', ['p-1a'])],
        queue: ['q-1']
      })
    );
    expect(screen.queryByRole('list', { name: 'Phases' })).toBeNull();
    expect(screen.queryByRole('list', { name: 'Up next' })).toBeNull();
    expect(screen.queryByRole('button', { name: '+ Add heat' })).toBeNull();
    expect(screen.queryByRole('button', { name: '+ Add bracket' })).toBeNull();
  });

  it('says so when the event cannot be read', async () => {
    await open(eventView(), {
      getEventState: vi.fn(async () => Promise.reject(new Error('The Director is not answering.')))
    });
    const alert = screen.getByRole('alert');
    expect(alert).toHaveTextContent('The event could not be read.');
    expect(alert).toHaveTextContent('The Director is not answering.');
    // With nothing read there are no lap rules to set and no class to add.
    expect(screen.queryByRole('form', { name: 'Lap rules' })).toBeNull();
    expect(screen.queryByRole('button', { name: '+ Add class' })).toBeNull();
  });
});

describe('Edit the event', () => {
  it('starts from what the event has and saves the name and details through the session', async () => {
    const updateEvent = vi.fn<NonNullable<SessionSeams['updateEventImpl']>>(
      async (_base, id, request) => ({ ...EVENT, id, ...request })
    );
    const { session } = await open(eventView(), {}, { updateEventImpl: updateEvent });
    await fireEvent.click(screen.getByRole('button', { name: 'Edit' }));
    const form = await screen.findByRole('form', { name: 'Edit the event' });
    expect(within(form).getByLabelText('Event name')).toHaveValue('Club Night');
    expect(within(form).getByLabelText('Event date')).toHaveValue('2026-11-14');
    expect(within(form).getByLabelText('Event location')).toHaveValue('The old gym');
    expect(within(form).getByLabelText('Event organizer')).toHaveValue('');

    await fireEvent.input(within(form).getByLabelText('Event name'), {
      target: { value: 'Club Night 2' }
    });
    await fireEvent.input(within(form).getByLabelText('Event organizer'), {
      target: { value: 'Ryan' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));

    await waitFor(() => expect(updateEvent).toHaveBeenCalledTimes(1));
    const [, id, request] = updateEvent.mock.calls[0];
    expect(id).toBe('evt-77');
    expect(request).toEqual({
      name: 'Club Night 2',
      date: '2026-11-14',
      location: 'The old gym',
      organizer: 'Ryan',
      description: ''
    });
    // The open event follows, and the dialog closes.
    await waitFor(() => expect(session.currentEvent?.name).toBe('Club Night 2'));
    await waitFor(() =>
      expect(
        (screen.getByRole('form', { name: 'Edit the event', hidden: true }) as HTMLElement).closest(
          'dialog'
        )
      ).not.toHaveAttribute('open')
    );
  });

  it('does not save an event with no name', async () => {
    const updateEvent = vi.fn<NonNullable<SessionSeams['updateEventImpl']>>();
    await open(eventView(), {}, { updateEventImpl: updateEvent });
    await fireEvent.click(screen.getByRole('button', { name: 'Edit' }));
    const form = await screen.findByRole('form', { name: 'Edit the event' });
    await fireEvent.input(within(form).getByLabelText('Event name'), { target: { value: '  ' } });
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled();
    await fireEvent.submit(form);
    expect(updateEvent).not.toHaveBeenCalled();
  });

  it("shows the Director's sentence in the dialog when it says no, and stays open", async () => {
    const SAID = 'Another event is already called Club Night 2.';
    await open(
      eventView(),
      {},
      {
        updateEventImpl: vi.fn(async () =>
          Promise.reject(Object.assign(new Error(SAID), { status: 409, code: 'Refused' }))
        )
      }
    );
    await fireEvent.click(screen.getByRole('button', { name: 'Edit' }));
    const form = await screen.findByRole('form', { name: 'Edit the event' });
    await fireEvent.input(within(form).getByLabelText('Event name'), {
      target: { value: 'Club Night 2' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(await within(form).findByText(SAID)).toBeVisible();
    expect(form.closest('dialog')).toHaveAttribute('open');
    expect(within(form).getByLabelText('Event name')).toHaveValue('Club Night 2');
  });
});

describe('Lap rules', () => {
  const minLap = () => screen.getByLabelText('Minimum lap time, in seconds') as HTMLInputElement;
  const ignore = () =>
    screen.getByLabelText('Ignore crossings before, in seconds') as HTMLInputElement;
  const state = () => screen.getByTestId('lap-rules-state');

  /** A live session whose snapshot follows the lap rules it is sent, as the Director's does. */
  function following(start: EventView['lap_rules']) {
    let view = eventView({ lap_rules: start });
    const sendCommand = vi.fn<LiveSession['sendCommand']>(async (_id, command) => {
      if (typeof command === 'object' && 'Record' in command) {
        const fact = command.Record;
        if (typeof fact === 'object' && 'LapRulesSet' in fact) {
          view = { ...view, cursor: view.cursor + 1, lap_rules: fact.LapRulesSet.rules };
        }
      }
      return { cursor: view.cursor };
    });
    return { first: view, over: { getEventState: vi.fn(async () => view), sendCommand } };
  }

  it('shows the rules the Director has, in seconds, and empty for a rule not set', async () => {
    await open(eventView({ lap_rules: { min_lap_micros: 2_500_000 } }));
    expect(minLap()).toHaveValue(2.5);
    expect(ignore()).toHaveValue(null);
    expect(state()).toHaveTextContent('');
  });

  it('sends both rules when a field is left, and says the Director has them', async () => {
    const { first, over } = following({ min_lap_micros: 2_000_000 });
    const { liveSession } = await open(first, over);
    await fireEvent.input(ignore(), { target: { value: '1.5' } });
    await fireEvent.blur(ignore());
    await waitFor(() =>
      expect(sent(liveSession)).toEqual([
        {
          Record: {
            LapRulesSet: { rules: { min_lap_micros: 2_000_000, ignore_before_micros: 1_500_000 } }
          }
        }
      ])
    );
    await waitFor(() => expect(state()).toHaveTextContent('Saved. The Director has these rules.'));
    // What is shown afterwards is what the Director has.
    expect(ignore()).toHaveValue(1.5);
    expect(minLap()).toHaveValue(2);
  });

  it('saves on Enter as it does on leaving the field', async () => {
    const { first, over } = following({});
    const { liveSession } = await open(first, over);
    await fireEvent.input(minLap(), { target: { value: '3' } });
    await fireEvent.submit(screen.getByRole('form', { name: 'Lap rules' }));
    await waitFor(() =>
      expect(sent(liveSession)).toEqual([
        { Record: { LapRulesSet: { rules: { min_lap_micros: 3_000_000 } } } }
      ])
    );
  });

  it('sends nothing when a field is left as the Director has it', async () => {
    const { liveSession } = await open(eventView({ lap_rules: { min_lap_micros: 2_000_000 } }));
    await fireEvent.blur(minLap());
    await fireEvent.input(minLap(), { target: { value: '2' } });
    await fireEvent.blur(minLap());
    expect(liveSession.sendCommand).not.toHaveBeenCalled();
    expect(state()).toHaveTextContent('');
  });

  it('takes 0 as no rule, and leaves the rule out of what it sends', async () => {
    const { first, over } = following({ min_lap_micros: 2_000_000, ignore_before_micros: 500_000 });
    const { liveSession } = await open(first, over);
    await fireEvent.input(minLap(), { target: { value: '0' } });
    await fireEvent.blur(minLap());
    await waitFor(() =>
      expect(sent(liveSession)).toEqual([
        { Record: { LapRulesSet: { rules: { ignore_before_micros: 500_000 } } } }
      ])
    );
    await waitFor(() => expect(minLap()).toHaveValue(null));
  });

  it('takes an emptied field as no rule', async () => {
    const { first, over } = following({ min_lap_micros: 2_000_000 });
    const { liveSession } = await open(first, over);
    await fireEvent.input(minLap(), { target: { value: '' } });
    await fireEvent.blur(minLap());
    await waitFor(() =>
      expect(sent(liveSession)).toEqual([{ Record: { LapRulesSet: { rules: {} } } }])
    );
  });

  it('refuses a time below 0 with a sentence and sends nothing', async () => {
    const { liveSession } = await open(eventView());
    await fireEvent.input(minLap(), { target: { value: '-1' } });
    await fireEvent.blur(minLap());
    expect(state()).toHaveTextContent('A lap rule is a number of seconds, 0 or more.');
    expect(liveSession.sendCommand).not.toHaveBeenCalled();
  });

  it("shows the Director's sentence when it refuses, and keeps what was typed", async () => {
    const SAID = 'The event is locked.';
    await open(
      eventView(),
      {
        sendCommand: vi.fn(async () =>
          Promise.reject(Object.assign(new Error(SAID), { status: 409, code: 'Refused' }))
        )
      },
      {}
    );
    await fireEvent.input(minLap(), { target: { value: '4' } });
    await fireEvent.blur(minLap());
    await waitFor(() => expect(state()).toHaveTextContent(SAID));
    expect(minLap()).toHaveValue(4);
  });

  it('points at the Seats page for the timer own lap cooldown', async () => {
    await open(eventView());
    expect(screen.getByRole('link', { name: 'Seats page' })).toHaveAttribute('href', '#/seats');
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
    const { liveSession } = await open(eventView({ phases, rosters: [roster()] }));
    await fireEvent.click(screen.getByRole('button', { name: '+ Add class' }));
    const form = await screen.findByRole('form', { name: 'Add a class' });
    // Open is already in the event and Old is hidden, so only Spec is offered.
    const options = within(within(form).getByLabelText('Class')).getAllByRole('option');
    expect(options.map((option) => option.textContent)).toEqual(['Spec']);
    await fireEvent.click(within(form).getByRole('button', { name: 'Cat' }));
    await fireEvent.click(within(form).getByRole('button', { name: 'Ace' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Add class' }));
    await waitFor(() =>
      expect(sent(liveSession)).toEqual([
        { Record: { RosterSet: { class: 'cls-spec', pilots: ['p-3c', 'p-1a'] } } }
      ])
    );
    await waitFor(() => expect(screen.queryByRole('form', { name: 'Add a class' })).toBeNull());
  });

  it('edits a roster, starting from the pilots in it', async () => {
    const { liveSession } = await open(eventView({ phases, rosters: [roster()] }));
    await fireEvent.click(screen.getByRole('button', { name: 'Edit the roster of Open' }));
    const form = await screen.findByRole('form', { name: 'Roster of Open' });
    await fireEvent.click(within(form).getByRole('button', { name: 'Remove Ace' }));
    await fireEvent.click(within(form).getByRole('button', { name: 'Cat' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Save roster' }));
    await waitFor(() =>
      expect(sent(liveSession)).toEqual([
        { Record: { RosterSet: { class: 'cls-open', pilots: ['p-2b', 'p-3c'] } } }
      ])
    );
  });

  it('chooses which phases count and in what order', async () => {
    const { liveSession } = await open(eventView({ phases, rosters: [roster()] }));
    await fireEvent.click(screen.getByRole('button', { name: 'Choose what counts for Open' }));
    const dialog = await screen.findByLabelText('What counts for Open');
    expect(within(dialog).getByRole('list', { name: 'Phases that count' })).toHaveTextContent(
      'Finals'
    );
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Qualifying' }));
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Move Qualifying up' }));
    // The Edit dialog of the page is closed, so this is the only Save on the screen.
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() =>
      expect(sent(liveSession)).toEqual([
        { Record: { StandingsSet: { class: 'cls-open', phases: ['quali', 'finals'] } } }
      ])
    );
  });
});
