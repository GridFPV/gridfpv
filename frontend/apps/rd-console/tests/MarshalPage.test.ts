/**
 * The Marshal page against a stand-in live store: the to-do list and why each race is on it,
 * the race review with its rulings, the trace, protests, the locked (read-only) case, a
 * refusal in the Director's words, and no raw id on the screen.
 *
 * The trace is a canvas, which jsdom does not draw; `tests/signal.test.ts` covers what it
 * draws from. Here it has to be present and named, and a click on it has to pick a crossing
 * or offer a lap at the exact time under the pointer.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { EventMeta, EventView, LapView, Pilot, RaceSignal, RaceView } from '@gridfpv/types';
import MarshalPage from '../src/screens/MarshalPage.svelte';
import { EventLive, type LiveSession } from '../src/lib/eventLive.svelte.js';
import { traceMarks } from '../src/lib/marshal.js';
import { RaceTrace, timeAtX, wholeWindow } from '../src/lib/signal.js';
import { eventView, group, heat, race, raceSeat, timerView } from './competition.js';

const EVENT: EventMeta = { id: 'evt-77', name: 'Club Night', created_at: 0, persistent: true };
const PILOTS: Pilot[] = [
  { id: 'p-1a', callsign: 'Ace', vtx_types: [] },
  { id: 'p-2b', callsign: 'Bee', vtx_types: [] }
];
const HEAT = 'quali-x9z1';
const S = 1_000_000;
const START = 100 * S;

const lap = (crossing: number, from: number, to: number, restart = false): LapView => ({
  crossing,
  started_at: START + from * S,
  ended_at: START + to * S,
  micros: (to - from) * S,
  restart
});

const placed = (pilot: string, position: number, laps: number, over = {}) => ({
  pilot,
  position,
  disqualified: false,
  position_set: false,
  flew: true,
  wins: 0,
  points: 0,
  laps,
  ...over
});

/** A saved bench race: Ace flew two laps with a stray crossing between; Bee one lap. */
function savedRace(over: Partial<RaceView> = {}): RaceView {
  return race(HEAT, 1, 'Saved', {
    started_at: START,
    stopped_at: START + 30 * S,
    needs_marshal: true,
    note: 'Ace may have a stray lap',
    seats: [
      raceSeat(0, 'p-1a', {
        finish: 1,
        hole_shot: { crossing: 10, at: START + 1 * S },
        laps: [lap(11, 1, 9), lap(12, 9, 10.5), lap(13, 10.5, 18)],
        best_lap_micros: 1.5 * S,
        total_micros: 17 * S,
        uncounted: [
          { crossing: 14, at: START + 18.4 * S, why: 'UnderMinLap' },
          { crossing: 15, at: START + 22 * S, why: 'Removed', reason: 'FalseDetection' }
        ]
      }),
      raceSeat(1, 'p-2b', {
        finish: 2,
        hole_shot: { crossing: 20, at: START + 2 * S },
        laps: [lap(21, 2, 11)],
        best_lap_micros: 9 * S
      })
    ],
    ...over
  });
}

const quali = (over = {}) =>
  heat(HEAT, 'Qualifying Heat 1', ['p-1a', 'p-2b'], {
    parent: 'quali',
    ranking: [placed('p-1a', 1, 3), placed('p-2b', 2, 1)],
    ...over
  });
const tree = (over = {}, top = {}) => [group('quali', 'Qualifying', undefined, top), quali(over)];

async function open(
  view: EventView,
  raceId?: string,
  over: Partial<LiveSession> = {},
  timers = [timerView('gate-x1y2', 'Gate', 2)]
) {
  const session: LiveSession = {
    getEventState: vi.fn(async () => view),
    subscribeChanges: vi.fn(() => () => {}),
    sendCommand: vi.fn(async () => ({ cursor: view.cursor })),
    createBracket: vi.fn(async () => ({ cursor: view.cursor })),
    postPass: vi.fn(async () => ({ cursor: view.cursor })),
    listPilots: vi.fn(async () => PILOTS),
    listTimers: vi.fn(async () => timers),
    listClasses: vi.fn(async () => []),
    getRaceSignal: vi.fn(async (_id: string, id: string) => ({ race: id, seats: [] })),
    ...over
  };
  const live = new EventLive(session, EVENT.id);
  await live.reload();
  await live.reloadPilots();
  await live.reloadTimers();
  const nav = {
    onhome: vi.fn(),
    onevents: vi.fn(),
    onevent: vi.fn(),
    onopen: vi.fn(),
    onrun: vi.fn()
  };
  render(MarshalPage, { event: EVENT, live, race: raceId, ...nav });
  return { session, live, nav };
}

/** A table row as its cells' words, joined by single spaces. */
const cells = (row: HTMLElement): string =>
  [...row.querySelectorAll('th, td')]
    .map((cell) => cell.textContent?.replace(/\s+/g, ' ').trim())
    .filter(Boolean)
    .join(' ');

/** Click once the last ruling has landed (the buttons are off while one is being sent). */
async function clickWhenReady(button: HTMLElement) {
  await waitFor(() => expect(button).toBeEnabled());
  await fireEvent.click(button);
}

const sent = (session: LiveSession) =>
  vi.mocked(session.sendCommand).mock.calls.map((call) => call[1]);

/** Every raw handle in these fixtures. None may be on the page. */
function expectNoRawIds() {
  const text = document.body.textContent ?? '';
  for (const raw of [HEAT, '#1', '#2', 'bench', 'seat-1', 'p-1a', 'p-2b', EVENT.id, 'gate-x1y2']) {
    expect(text).not.toContain(raw);
  }
  for (const element of document.body.querySelectorAll('[aria-label], [title]')) {
    const words = `${element.getAttribute('aria-label') ?? ''} ${element.getAttribute('title') ?? ''}`;
    for (const raw of [HEAT, 'p-1a', 'p-2b', 'gate-x1y2']) expect(words).not.toContain(raw);
  }
}

afterEach(() => vi.restoreAllMocks());

describe('MarshalPage lists', () => {
  it('lists what needs a look with why, and every other race by heat', async () => {
    const flagged = savedRace();
    const protested = race(HEAT, 2, 'Saved');
    const stopped = race(HEAT, 4, 'Stopped');
    const view = eventView({
      phases: tree(),
      races: [flagged, protested, race(HEAT, 3, 'Discarded', { discard_reason: 'Other' }), stopped],
      current_race: stopped.race,
      protests: [{ protest: 'pr-9z', race: protested.race }]
    });
    const { nav } = await open(view);

    const todo = within(screen.getByRole('list', { name: 'Races that need a look' }));
    const items = todo.getAllByRole('listitem');
    expect(items).toHaveLength(3);
    expect(items[0]).toHaveTextContent('Qualifying Heat 1 · race 1');
    expect(items[0]).toHaveTextContent('Needs a look.');
    expect(items[0]).toHaveTextContent('Ace may have a stray lap');
    expect(items[1]).toHaveTextContent('Qualifying Heat 1 · race 2');
    expect(items[1]).toHaveTextContent('1 open protest.');
    expect(items[2]).toHaveTextContent('Stopped. Not saved or discarded yet.');

    const rest = within(screen.getByRole('region', { name: 'Qualifying Heat 1' }));
    expect(rest.getAllByRole('listitem')).toHaveLength(1);
    await fireEvent.click(rest.getByRole('button', { name: /race 3, Discarded: other/ }));
    expect(nav.onopen).toHaveBeenCalledWith(`${HEAT}#3`);

    await fireEvent.click(todo.getByRole('button', { name: /race 2/ }));
    expect(nav.onopen).toHaveBeenCalledWith(protested.race);
    expect(screen.getByText('Pick a race on the left to look at it.')).toBeVisible();
    expectNoRawIds();
  });

  it('says so when nothing needs a look and nothing has been flown', async () => {
    await open(eventView({ phases: tree() }));
    expect(screen.getByText('Nothing needs a look.')).toBeVisible();
    expect(screen.getByText('No race has been flown yet.')).toBeVisible();
  });

  it('the breadcrumb ends on Marshal and leads back to the event', async () => {
    const { nav } = await open(eventView({ phases: tree() }));
    const crumbs = screen.getByRole('navigation', { name: 'Breadcrumb' });
    expect(within(crumbs).getByText('Marshal')).toHaveAttribute('aria-current', 'page');
    await fireEvent.click(within(crumbs).getByRole('button', { name: 'Club Night' }));
    expect(nav.onevent).toHaveBeenCalled();
  });

  it('says so for a race that is not in the event', async () => {
    await open(eventView({ phases: tree() }), 'gone#1');
    expect(screen.getByText('This race is not in the event.')).toBeVisible();
  });
});

describe('MarshalPage race review', () => {
  const view = () => eventView({ phases: tree(), races: [savedRace()] });
  const RACE = `${HEAT}#1`;

  it('shows each seat by callsign, seat and timer, with its laps, place and what does not count', async () => {
    await open(view(), RACE);
    expect(screen.getByTestId('review-title')).toHaveTextContent('Qualifying Heat 1 · race 1');
    expect(screen.getByText('Note: Ace may have a stray lap')).toBeVisible();

    const ace = screen.getByRole('listitem', { name: 'Seat 1, Ace' });
    expect(ace).toHaveTextContent('Seat 1 · Bench');
    expect(ace.querySelector('[data-stat="place"]')).toHaveTextContent('1st');
    expect(ace.querySelector('[data-stat="laps"]')).toHaveTextContent('3 laps');
    const rows = within(within(ace).getByRole('table', { name: 'Laps for Ace' })).getAllByRole(
      'row'
    );
    // Header, hole shot, three laps.
    expect(rows.map(cells)).toEqual([
      'Lap Time Into the race Rulings',
      expect.stringMatching(/^Hole shot Starts lap 1 1\.000 s/),
      expect.stringMatching(/^Lap 1 8\.000 9\.000 s/),
      expect.stringMatching(/^Lap 2 1\.500 Best 10\.500 s/),
      expect.stringMatching(/^Lap 3 7\.500 18\.000 s/)
    ]);
    const uncounted = within(ace).getByRole('list', {
      name: 'Crossings of Ace that do not count'
    });
    expect(uncounted).toHaveTextContent('18.400 s');
    expect(uncounted).toHaveTextContent('under the minimum lap time');
    expect(uncounted).toHaveTextContent('Removed by a marshal: false detection.');
    // Only the removed one can be restored.
    expect(within(uncounted).getAllByRole('button', { name: /Restore/ })).toHaveLength(1);

    const ranking = within(screen.getByRole('list', { name: 'Heat ranking' })).getAllByRole(
      'listitem'
    );
    expect(ranking[0]).toHaveTextContent('Ace');
    expect(ranking[1]).toHaveTextContent('Bee');
    // A bench seat has no signal: one plain sentence, and everything else works.
    expect(await within(ace).findByTestId('no-signal')).toHaveTextContent(
      'Bench crossings are logged by hand, so there is no signal trace for this seat.'
    );
    expectNoRawIds();
  });

  it('Remove asks for an optional reason in one tap and sends LapRemoved', async () => {
    const { session } = await open(view(), RACE);
    await fireEvent.click(screen.getByRole('button', { name: 'Remove lap 2 of Ace' }));
    const why = within(screen.getByRole('group', { name: 'Why remove lap 2?' }));
    await fireEvent.click(why.getByRole('button', { name: 'False detection' }));
    await waitFor(() =>
      expect(sent(session)).toEqual([
        { Record: { LapRemoved: { race: RACE, crossing: 12, reason: 'FalseDetection' } } }
      ])
    );
    // The reason row closes once the Director took it.
    await waitFor(() =>
      expect(screen.queryByRole('group', { name: 'Why remove lap 2?' })).toBeNull()
    );

    await fireEvent.click(screen.getByRole('button', { name: 'Remove the hole shot of Bee' }));
    await clickWhenReady(screen.getByRole('button', { name: 'No reason' }));
    await waitFor(() =>
      expect(sent(session)[1]).toEqual({ Record: { LapRemoved: { race: RACE, crossing: 20 } } })
    );
  });

  it('Keep it closes the reason row without sending anything', async () => {
    const { session } = await open(view(), RACE);
    await fireEvent.click(screen.getByRole('button', { name: 'Remove lap 1 of Ace' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Keep it' }));
    expect(screen.queryByRole('group', { name: 'Why remove lap 1?' })).toBeNull();
    expect(session.sendCommand).not.toHaveBeenCalled();
  });

  it('Restore, Mark as restart and Unmark restart each send their fact', async () => {
    const withRestart = savedRace();
    withRestart.seats[1].laps = [lap(21, 2, 11, true), lap(22, 11, 19)];
    const { session } = await open(eventView({ phases: tree(), races: [withRestart] }), RACE);

    await fireEvent.click(screen.getByRole('button', { name: /Restore the crossing of Ace/ }));
    await waitFor(() => expect(sent(session)).toHaveLength(1));
    await clickWhenReady(screen.getByRole('button', { name: 'Mark lap 3 of Ace as a restart' }));
    await waitFor(() => expect(sent(session)).toHaveLength(2));
    // Bee's first lap is a marked restart: no number, no time, and it can be unmarked.
    const bee = screen.getByRole('listitem', { name: 'Seat 2, Bee' });
    expect(within(bee).getByText('No lap time')).toBeVisible();
    expect(within(bee).getByRole('button', { name: /^Lap 1$/ })).toBeVisible();
    await clickWhenReady(screen.getByRole('button', { name: 'Unmark the restart of Bee' }));
    await waitFor(() => expect(sent(session)).toHaveLength(3));
    expect(sent(session)).toEqual([
      { Record: { LapRestored: { race: RACE, crossing: 15 } } },
      { Record: { RestartMarked: { race: RACE, crossing: 13 } } },
      { Record: { RestartUnmarked: { race: RACE, crossing: 21 } } }
    ]);
  });

  it('adds a lap at a time typed as seconds into the race', async () => {
    const { session } = await open(view(), RACE);
    const box = screen.getByRole('textbox', { name: 'Seconds into the race, for Bee' });
    const form = screen.getByRole('form', { name: 'Add a lap for Bee' });
    await fireEvent.input(box, { target: { value: 'soon' } });
    await fireEvent.submit(form);
    expect(await within(form).findByRole('alert')).toHaveTextContent(
      'Type a time in seconds, like 12.5.'
    );
    expect(session.sendCommand).not.toHaveBeenCalled();

    await fireEvent.input(box, { target: { value: '19.25' } });
    await fireEvent.submit(form);
    await waitFor(() =>
      expect(sent(session)).toEqual([
        {
          Record: {
            LapInserted: { race: RACE, adapter: 'bench', seat: 'seat-2', at: START + 19_250_000 }
          }
        }
      ])
    );
  });

  it('records a false start against a pilot, and clears one', async () => {
    const flagged = savedRace();
    flagged.seats[1].false_start = true;
    const { session } = await open(eventView({ phases: tree(), races: [flagged] }), RACE);
    await fireEvent.click(screen.getByRole('button', { name: 'False start by Ace' }));
    await waitFor(() => expect(sent(session)).toHaveLength(1));
    await clickWhenReady(screen.getByRole('button', { name: 'Clear the false start for Bee' }));
    await waitFor(() => expect(sent(session)).toHaveLength(2));
    expect(sent(session)).toEqual([
      { Record: { FalseStart: { race: RACE, pilot: 'p-1a' } } },
      { Record: { FalseStartCleared: { race: RACE, pilot: 'p-2b' } } }
    ]);
  });

  it('"Looked at" clears the flag, and a quiet "Needs a look" puts it back', async () => {
    const first = await open(view(), RACE);
    expect(screen.queryByRole('button', { name: 'Needs a look' })).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Looked at' }));
    await waitFor(() =>
      expect(sent(first.session)).toEqual([
        { Record: { MarshalFlagSet: { race: RACE, needs_marshal: false } } }
      ])
    );
    document.body.innerHTML = '';

    const second = await open(
      eventView({ phases: tree(), races: [savedRace({ needs_marshal: false })] }),
      RACE
    );
    expect(screen.queryByRole('button', { name: 'Looked at' })).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Needs a look' }));
    await waitFor(() =>
      expect(sent(second.session)).toEqual([
        { Record: { MarshalFlagSet: { race: RACE, needs_marshal: true } } }
      ])
    );
  });

  it('shows the laps the Director worked out after a ruling', async () => {
    const before = view();
    const after = savedRace();
    // Lap 2 removed: lap 1 is as it was, and the next lap starts where lap 1 ended.
    after.seats[0].laps = [lap(11, 1, 9), lap(13, 9, 18)];
    after.seats[0].best_lap_micros = 8 * S;
    const getEventState = vi
      .fn<LiveSession['getEventState']>()
      .mockResolvedValueOnce(before)
      .mockResolvedValue(eventView({ cursor: 2, phases: tree(), races: [after] }));
    await open(before, RACE, { getEventState });
    await fireEvent.click(screen.getByRole('button', { name: 'Remove lap 2 of Ace' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Incorrect lap' }));
    const ace = screen.getByRole('listitem', { name: 'Seat 1, Ace' });
    await waitFor(() =>
      expect(ace.querySelector('[data-stat="laps"]')).toHaveTextContent('2 laps')
    );
    const rows = within(within(ace).getByRole('table')).getAllByRole('row');
    expect(rows[2]).toHaveTextContent('8.000');
    expect(rows[3]).toHaveTextContent('9.000');
  });

  it("a refusal is shown in the Director's words", async () => {
    const SAID = 'Qualifying is locked. Unlock it to make a ruling.';
    await open(view(), RACE, {
      sendCommand: vi.fn(async () =>
        Promise.reject(Object.assign(new Error(SAID), { status: 409, code: 'Refused' }))
      )
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Looked at' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(SAID);
  });

  it('a locked race is read-only and names the phase that is locked', async () => {
    await open(
      eventView({
        phases: tree({ locked: true }, { locked: true, locked_here: true }),
        races: [savedRace({ locked: true })]
      }),
      RACE
    );
    expect(screen.getByTestId('review-status')).toHaveTextContent(
      'Qualifying is locked, so this race is official and cannot be changed.'
    );
    // The laps are still there to read.
    expect(screen.getByRole('table', { name: 'Laps for Ace' })).toBeVisible();
    for (const name of [/^Remove/, /Mark .* as a restart/, /Restore/, /False start by/]) {
      expect(screen.queryByRole('button', { name })).toBeNull();
    }
    expect(screen.queryByRole('button', { name: 'Looked at' })).toBeNull();
    expect(screen.queryByRole('form', { name: /Add a lap/ })).toBeNull();
    expect(screen.queryByRole('form', { name: 'File a protest' })).toBeNull();
    expectNoRawIds();
  });

  it('a discarded race is read-only and says why', async () => {
    await open(eventView({ phases: tree(), races: [savedRace({ state: 'Discarded' })] }), RACE);
    expect(screen.getByTestId('review-status')).toHaveTextContent('This race was discarded.');
    expect(screen.queryByRole('button', { name: /^Remove/ })).toBeNull();
  });

  it('Open the heat leads to the Run view of the race heat', async () => {
    const { nav } = await open(view(), RACE);
    await fireEvent.click(screen.getByRole('button', { name: 'Open the heat' }));
    expect(nav.onrun).toHaveBeenCalledWith(HEAT);
  });
});

describe('MarshalPage protests', () => {
  const RACE = `${HEAT}#1`;

  it('files a protest with an optional pilot and note', async () => {
    const { session } = await open(eventView({ phases: tree(), races: [savedRace()] }), RACE);
    expect(screen.getByText('No protest has been filed against this race.')).toBeVisible();
    const form = within(screen.getByRole('form', { name: 'File a protest' }));
    await fireEvent.click(form.getByRole('button', { name: 'File protest' }));
    await waitFor(() => expect(sent(session)).toEqual([{ FileProtest: { race: RACE } }]));
    await waitFor(() => expect(form.getByRole('button', { name: 'File protest' })).toBeEnabled());

    await fireEvent.change(form.getByRole('combobox', { name: 'Protest by' }), {
      target: { value: 'p-2b' }
    });
    await fireEvent.input(form.getByRole('textbox', { name: 'What is protested' }), {
      target: { value: ' Ace cut gate 3 ' }
    });
    await clickWhenReady(form.getByRole('button', { name: 'File protest' }));
    await waitFor(() =>
      expect(sent(session)[1]).toEqual({
        FileProtest: { race: RACE, by: 'p-2b', note: 'Ace cut gate 3' }
      })
    );
  });

  it('lists the protests by number and resolves an open one with a note', async () => {
    const { session } = await open(
      eventView({
        phases: tree(),
        races: [savedRace()],
        protests: [
          { protest: 'pr-1q', race: RACE, by: 'p-2b', note: 'Ace cut gate 3' },
          { protest: 'pr-2w', race: RACE, outcome: 'Denied' },
          { protest: 'pr-3e', race: 'other#1' }
        ]
      }),
      RACE
    );
    const list = within(screen.getByRole('list', { name: 'Protests against this race' }));
    const first = list.getByRole('listitem', { name: 'Protest 1' });
    expect(first).toHaveTextContent('Open');
    expect(first).toHaveTextContent('By Bee');
    expect(first).toHaveTextContent('Ace cut gate 3');
    const second = list.getByRole('listitem', { name: 'Protest 2' });
    expect(second).toHaveTextContent('Denied');
    expect(second).toHaveTextContent('No pilot named');
    expect(within(second).queryByRole('button')).toBeNull();
    expect(list.queryByRole('listitem', { name: 'Protest 3' })).toBeNull();
    expect(document.body.textContent).not.toContain('pr-1q');

    await fireEvent.input(
      within(first).getByRole('textbox', { name: 'Note for the outcome of protest 1' }),
      { target: { value: 'Video shows a clean pass' } }
    );
    await fireEvent.click(within(first).getByRole('button', { name: 'Denied: protest 1' }));
    await waitFor(() =>
      expect(sent(session)).toEqual([
        {
          Record: {
            ProtestResolved: {
              protest: 'pr-1q',
              outcome: 'Denied',
              note: 'Video shows a clean pass'
            }
          }
        }
      ])
    );
  });
});

describe('MarshalPage trace', () => {
  const RACE = `${HEAT}#1`;
  const GATE = 'gate-x1y2';

  /** The saved race, flown on the Gate timer, with signal logged for the first seat. */
  function onGate(): { view: EventView; signal: RaceSignal } {
    const flown = savedRace();
    flown.seats = flown.seats.map((seat, index) => ({
      ...seat,
      adapter: GATE,
      seat: String(index)
    }));
    const signal: RaceSignal = {
      race: RACE,
      seats: [
        {
          adapter: GATE,
          seat: '0',
          start_at: START,
          chunks: [
            {
              adapter: GATE,
              seat: '0',
              from: START,
              period_micros: 100_000,
              levels: Array.from({ length: 300 }, (_, i) => (i % 80 === 10 ? -500 : -900)),
              unit: 'TenthsDbm'
            }
          ],
          thresholds: [{ adapter: GATE, seat: '0', enter: -600, exit: -700, unit: 'TenthsDbm' }]
        },
        { adapter: GATE, seat: '1', start_at: START, chunks: [], thresholds: [] }
      ]
    };
    return { view: eventView({ phases: tree(), races: [flown] }), signal };
  }

  /** jsdom lays nothing out, so give the canvas a width to click on. */
  function layOut() {
    vi.spyOn(HTMLCanvasElement.prototype, 'getBoundingClientRect').mockReturnValue({
      left: 0,
      top: 0,
      right: 1000,
      bottom: 100,
      width: 1000,
      height: 100,
      x: 0,
      y: 0,
      toJSON: () => ({})
    });
  }

  const clickAt = async (canvas: Element, x: number) => {
    await fireEvent.mouseDown(canvas, { clientX: x, button: 0 });
    await fireEvent.mouseUp(canvas, { clientX: x, button: 0 });
  };

  it('reads the signal for the race and shows a named trace where a seat has one', async () => {
    const { view, signal } = onGate();
    const { session } = await open(view, RACE, { getRaceSignal: vi.fn(async () => signal) });
    const ace = screen.getByRole('listitem', { name: 'Seat 1, Ace' });
    const picture = await within(ace).findByRole('img');
    expect(session.getRaceSignal).toHaveBeenCalledWith(EVENT.id, RACE);
    expect(picture.getAttribute('aria-label')).toContain('Signal for Ace on Seat 1');
    expect(picture.querySelector('canvas')).not.toBeNull();
    expect(ace).toHaveTextContent('Seat 1 · Gate');
    // The other seat logged nothing: one plain sentence naming the timer.
    const bee = screen.getByRole('listitem', { name: 'Seat 2, Bee' });
    expect(within(bee).getByTestId('no-signal')).toHaveTextContent(
      'Gate logged no signal for this seat, so there is no trace.'
    );
    expectNoRawIds();
  });

  it('a click on a crossing picks its lap in the list, and picking in the list shows on the trace', async () => {
    layOut();
    const { view, signal } = onGate();
    await open(view, RACE, { getRaceSignal: vi.fn(async () => signal) });
    const ace = screen.getByRole('listitem', { name: 'Seat 1, Ace' });
    const canvas = (await within(ace).findByRole('img')).querySelector('canvas')!;
    const marks = traceMarks(view.races[0].seats[0], START);
    const whole = wholeWindow(new RaceTrace(signal.seats[0].chunks), marks)!;
    // Lap 2 ended 10.5 s into the race.
    const x = ((START + 10.5 * S - whole.from) / (whole.to - whole.from)) * 1000;
    await clickAt(canvas, x + 2);
    const lap2 = within(ace).getByRole('button', { name: /^Lap 2$/ });
    await waitFor(() => expect(lap2).toHaveAttribute('aria-pressed', 'true'));
    // Picking another row moves the highlight; picking it again clears it.
    const lap1 = within(ace).getByRole('button', { name: /^Lap 1$/ });
    await fireEvent.click(lap1);
    expect(lap1).toHaveAttribute('aria-pressed', 'true');
    expect(lap2).toHaveAttribute('aria-pressed', 'false');
    await fireEvent.click(lap1);
    expect(lap1).toHaveAttribute('aria-pressed', 'false');
  });

  it('a click on an empty spot offers "Add a lap here" at that exact time', async () => {
    layOut();
    const { view, signal } = onGate();
    const { session } = await open(view, RACE, { getRaceSignal: vi.fn(async () => signal) });
    const ace = screen.getByRole('listitem', { name: 'Seat 1, Ace' });
    const canvas = (await within(ace).findByRole('img')).querySelector('canvas')!;
    const marks = traceMarks(view.races[0].seats[0], START);
    const whole = wholeWindow(new RaceTrace(signal.seats[0].chunks), marks)!;
    const at = timeAtX(whole, 1000, 480);

    await clickAt(canvas, 480);
    const box = within(ace).getByRole('textbox', { name: 'Seconds into the race, for Ace' });
    await waitFor(() => expect(box).toHaveValue(((at - START) / S).toFixed(6)));
    await fireEvent.click(within(ace).getByRole('button', { name: 'Add a lap here' }));
    await waitFor(() =>
      expect(sent(session)).toEqual([
        { Record: { LapInserted: { race: RACE, adapter: GATE, seat: '0', at } } }
      ])
    );
  });

  it('the buttons zoom and pan the trace, and Whole race puts it back', async () => {
    const { view, signal } = onGate();
    await open(view, RACE, { getRaceSignal: vi.fn(async () => signal) });
    const ace = screen.getByRole('listitem', { name: 'Seat 1, Ace' });
    await within(ace).findByRole('img');
    const shown = within(ace).getByTestId('trace-window');
    const whole = shown.textContent;
    expect(within(ace).getByRole('button', { name: 'Zoom out' })).toBeDisabled();
    await fireEvent.click(within(ace).getByRole('button', { name: 'Zoom in' }));
    const zoomed = shown.textContent;
    expect(zoomed).not.toBe(whole);
    await fireEvent.click(within(ace).getByRole('button', { name: 'Later' }));
    expect(shown.textContent).not.toBe(zoomed);
    await fireEvent.click(within(ace).getByRole('button', { name: 'Whole race' }));
    expect(shown.textContent).toBe(whole);
  });

  it('draws the start, each crossing with its number, what does not count, and the levels', async () => {
    // jsdom has no canvas, so stand one in that records what is written on it.
    const written: string[] = [];
    const context = new Proxy(
      { fillText: (text: string) => written.push(text) },
      {
        get: (target, key) =>
          key in target ? target[key as keyof typeof target] : () => undefined,
        set: () => true
      }
    );
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(
      context as unknown as CanvasRenderingContext2D
    );
    vi.spyOn(HTMLCanvasElement.prototype, 'clientWidth', 'get').mockReturnValue(1000);
    vi.spyOn(HTMLCanvasElement.prototype, 'clientHeight', 'get').mockReturnValue(160);
    const { view, signal } = onGate();
    await open(view, RACE, { getRaceSignal: vi.fn(async () => signal) });
    const ace = screen.getByRole('listitem', { name: 'Seat 1, Ace' });
    await within(ace).findByRole('img');
    await waitFor(() => expect(written).toContain('Start'));
    // The hole shot, laps 1 to 3, the two crossings that do not count, and both levels.
    for (const text of ['HS', '1', '2', '3', 'Enter', 'Exit']) expect(written).toContain(text);
    expect(written.filter((text) => text === '×')).toHaveLength(2);
    expect(written.some((text) => text.endsWith(' dBm'))).toBe(true);

    // Picking a lap in the list draws the trace again.
    const before = written.length;
    await fireEvent.click(within(ace).getByRole('button', { name: /^Lap 2$/ }));
    await waitFor(() => expect(written.length).toBeGreaterThan(before));
  });

  it('says so when the signal cannot be read, and the lap list still works', async () => {
    const { view } = onGate();
    await open(view, RACE, {
      getRaceSignal: vi.fn(async () => Promise.reject(new Error('unreachable')))
    });
    const ace = screen.getByRole('listitem', { name: 'Seat 1, Ace' });
    expect(
      await within(ace).findByText(
        'The signal for this race could not be read. The lap list still works.'
      )
    ).toBeVisible();
    expect(within(ace).getByRole('button', { name: 'Remove lap 1 of Ace' })).toBeEnabled();
  });
});
