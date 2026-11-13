/**
 * The Run page, rendered against a stand-in live store: which heat is on the timer, the one
 * big action following the race, commands carrying the right wire handles, a refusal shown
 * in the Director's words, the keyboard, the lap callouts, and no raw id on the screen.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { EventMeta, EventView, Pilot } from '@gridfpv/types';
import RunPage from '../src/screens/RunPage.svelte';
import type { LiveSession } from '../src/lib/eventLive.svelte.js';
import {
  eventView,
  FIRST_TO_3,
  group,
  heat,
  lap,
  placed,
  race,
  raceSeat,
  timerView,
  TWO_MINUTE_QUALI
} from './competition.js';
import { openLive } from './support.js';

const EVENT: EventMeta = { id: 'evt-77', name: 'Club Night', created_at: 0, persistent: true };
const PILOTS: Pilot[] = [
  { id: 'p-1a', callsign: 'Ace', vtx_types: [] },
  { id: 'p-2b', callsign: 'Bee', vtx_types: [] }
];
const HEAT = 'quali-x9z1';
const S = 1_000_000;

/**
 * Open the page on `heat` (the qualifying heat unless told otherwise; `null` names none, as
 * the bare `#/event/run` does).
 */
async function open(
  view: EventView,
  over: Partial<LiveSession> = {},
  named: string | null = HEAT,
  props: { ontune?: (timer: string, seat: string) => void } = {}
) {
  const { session, live } = await openLive(
    view,
    { listPilots: vi.fn(async () => PILOTS), ...over },
    EVENT.id
  );
  const nav = { onrun: vi.fn() };
  const rendered = render(RunPage, { live, heat: named ?? undefined, ...nav, ...props });
  return { session, live, nav, ...rendered };
}

const phasePicker = () => screen.getByLabelText('Phase') as HTMLSelectElement;
const heatPicker = () => screen.getByLabelText('Heat') as HTMLSelectElement;
const optionTexts = (select: HTMLSelectElement) =>
  [...select.options].map((option) => option.textContent?.trim());

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  localStorage.clear();
});

const quali = (over = {}) => heat(HEAT, 'Qualifying Heat 1', ['p-1a', 'p-2b'], over);

/** Every raw handle in these fixtures. None may be on the page. */
function expectNoRawIds() {
  const text = document.body.textContent ?? '';
  for (const raw of [HEAT, '#1', 'bench', 'seat-1', 'p-1a', 'p-2b', EVENT.id]) {
    expect(text).not.toContain(raw);
  }
}

describe('RunPage', () => {
  it('a ready heat shows its lineup by seat and callsign, and Start sends StartRace', async () => {
    const { session } = await open(eventView({ phases: [quali()] }));
    // The page is the Run page; the heat is named in the Heat box, by its label.
    expect(screen.getByRole('heading', { level: 1, name: /^Run/ })).toBeVisible();
    expect(heatPicker()).toHaveValue(HEAT);
    expect(optionTexts(heatPicker())).toEqual(['Qualifying Heat 1']);
    expect(screen.queryByRole('navigation', { name: 'Breadcrumb' })).toBeNull();
    const seats = within(screen.getByRole('list', { name: 'Seats' })).getAllByRole('listitem');
    expect(seats.map((seat) => seat.getAttribute('aria-label'))).toEqual([
      'Seat 1, Ace',
      'Seat 2, Bee'
    ]);
    expect(screen.getByTestId('run-status')).toHaveTextContent('Ready to start.');
    expectNoRawIds();

    await fireEvent.click(screen.getByRole('button', { name: 'Start' }));
    await waitFor(() =>
      expect(session.sendCommand).toHaveBeenCalledWith(EVENT.id, { StartRace: { heat: HEAT } })
    );
  });

  it('while counting down the action is Call off, which discards the race', async () => {
    const staging = race(HEAT, 1, 'Staging', {
      tone_at: 5 * S,
      seats: [raceSeat(0, 'p-1a'), raceSeat(1, 'p-2b')]
    });
    const { session } = await open(
      eventView({ phases: [quali()], races: [staging], current_race: staging.race })
    );
    expect(screen.queryByRole('button', { name: 'Start' })).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Call off' }));
    await waitFor(() =>
      expect(session.sendCommand).toHaveBeenCalledWith(EVENT.id, {
        DiscardRace: { race: staging.race }
      })
    );
  });

  it('a running race shows laps per seat; Crossing posts a pass and Stop sends StopRace', async () => {
    const running = race(HEAT, 1, 'Running', {
      started_at: 0,
      end: { FirstToLaps: 3 },
      seats: [
        raceSeat(0, 'p-1a', {
          finish: 1,
          laps: [lap(3, 9.5), lap(5, 8.25)],
          best_lap_micros: 8_250_000
        }),
        raceSeat(1, 'p-2b', { finish: 2 })
      ]
    });
    const { session } = await open(
      eventView({ phases: [quali()], races: [running], current_race: running.race })
    );
    const ace = screen.getByRole('listitem', { name: 'Seat 1, Ace' });
    expect(ace.querySelector('[data-stat="place"]')).toHaveTextContent('P1');
    expect(ace.querySelector('[data-stat="laps"]')).toHaveTextContent('2');
    expect(ace.querySelector('[data-stat="last"]')).toHaveTextContent('8.250');
    expect(ace.querySelector('[data-stat="best"]')).toHaveTextContent('8.250');
    expect(within(ace).getByRole('list', { name: 'Laps for Seat 1' })).toHaveTextContent('9.500');
    // The bench tool is marked as one.
    expect(within(ace).getByText('Bench')).toBeVisible();
    expectNoRawIds();

    await fireEvent.click(screen.getByRole('button', { name: 'Crossing for Seat 2' }));
    await waitFor(() => expect(session.postPass).toHaveBeenCalled());
    expect(session.postPass).toHaveBeenCalledWith(
      EVENT.id,
      expect.objectContaining({ adapter: 'bench', seat: 'seat-2' })
    );

    await fireEvent.click(screen.getByRole('button', { name: 'Stop' }));
    await waitFor(() =>
      expect(session.sendCommand).toHaveBeenCalledWith(EVENT.id, {
        StopRace: { race: running.race }
      })
    );
  });

  it('a stopped race waits for Save or Discard; Save sends SaveRace', async () => {
    const stopped = race(HEAT, 1, 'Stopped', {
      started_at: 0,
      stopped_at: 40 * S,
      seats: [raceSeat(0, 'p-1a'), raceSeat(1, 'p-2b')]
    });
    const { session } = await open(
      eventView({ phases: [quali()], races: [stopped], current_race: stopped.race })
    );
    expect(screen.getByRole('timer')).toHaveTextContent('0:40.000');
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() =>
      expect(session.sendCommand).toHaveBeenCalledWith(EVENT.id, {
        SaveRace: { race: stopped.race, needs_marshal: false }
      })
    );
  });

  it('the save options add "Needs a look" and a note to the save', async () => {
    const stopped = race(HEAT, 1, 'Stopped', { started_at: 0, stopped_at: 40 * S });
    const { session } = await open(
      eventView({ phases: [quali()], races: [stopped], current_race: stopped.race })
    );
    await fireEvent.click(screen.getByText('Save options'));
    await fireEvent.click(screen.getByRole('checkbox', { name: 'Needs a look' }));
    await fireEvent.input(screen.getByRole('textbox', { name: 'Note for this race' }), {
      target: { value: ' Bee may have a stray lap ' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() =>
      expect(session.sendCommand).toHaveBeenCalledWith(EVENT.id, {
        SaveRace: { race: stopped.race, needs_marshal: true, note: 'Bee may have a stray lap' }
      })
    );
  });

  it('a saved race can be discarded from the earlier races, unless it is locked', async () => {
    const saved = race(HEAT, 1, 'Saved', {
      needs_marshal: true,
      note: 'check lap 2',
      seats: [raceSeat(0, 'p-1a'), raceSeat(1, 'p-2b')]
    });
    const locked = race(HEAT, 2, 'Saved', { locked: true });
    const { session } = await open(eventView({ phases: [quali()], races: [saved, locked] }));
    const earlier = within(screen.getByRole('list', { name: 'Earlier races' })).getAllByRole(
      'listitem'
    );
    expect(earlier[0]).toHaveTextContent('Needs a look');
    expect(earlier[0]).toHaveTextContent('check lap 2');
    expect(screen.queryByRole('button', { name: 'Discard race 2' })).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Discard race 1' }));
    await fireEvent.click(await screen.findByRole('button', { name: 'False start' }));
    await fireEvent.click(await screen.findByRole('button', { name: 'Ace' }));
    await waitFor(() => expect(session.sendCommand).toHaveBeenCalledTimes(2));
    expect(vi.mocked(session.sendCommand).mock.calls.map((call) => call[1])).toEqual([
      { Record: { FalseStart: { race: saved.race, pilot: 'p-1a' } } },
      { DiscardRace: { race: saved.race, reason: 'FalseStart' } }
    ]);
    expectNoRawIds();
  });

  it('discarding for a false start asks who, records it, then discards with the reason', async () => {
    const stopped = race(HEAT, 1, 'Stopped', {
      started_at: 0,
      stopped_at: 4 * S,
      seats: [raceSeat(0, 'p-1a'), raceSeat(1, 'p-2b')]
    });
    const { session } = await open(
      eventView({ phases: [quali()], races: [stopped], current_race: stopped.race })
    );
    await fireEvent.click(screen.getByRole('button', { name: 'Discard' }));
    await fireEvent.click(await screen.findByRole('button', { name: 'False start' }));
    await fireEvent.click(await screen.findByRole('button', { name: 'Bee' }));
    await waitFor(() => expect(session.sendCommand).toHaveBeenCalledTimes(2));
    expect(vi.mocked(session.sendCommand).mock.calls.map((call) => call[1])).toEqual([
      { Record: { FalseStart: { race: stopped.race, pilot: 'p-2b' } } },
      { DiscardRace: { race: stopped.race, reason: 'FalseStart' } }
    ]);
  });

  it('any other reason discards in one tap', async () => {
    const stopped = race(HEAT, 1, 'Stopped', { started_at: 0, stopped_at: S });
    const { session } = await open(
      eventView({ phases: [quali()], races: [stopped], current_race: stopped.race })
    );
    await fireEvent.click(screen.getByRole('button', { name: 'Discard' }));
    await fireEvent.click(await screen.findByRole('button', { name: 'Timer problem' }));
    await waitFor(() =>
      expect(session.sendCommand).toHaveBeenCalledWith(EVENT.id, {
        DiscardRace: { race: stopped.race, reason: 'TimerProblem' }
      })
    );
  });

  it("a refusal is shown in the Director's words", async () => {
    const SAID = 'the heat is waiting on another heat to finish';
    await open(eventView({ phases: [quali()] }), {
      sendCommand: vi.fn(async () =>
        Promise.reject(Object.assign(new Error(SAID), { status: 409, code: 'Refused' }))
      )
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Start' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(SAID);
  });

  it('a complete heat shows its ranking and earlier races by callsign, with no Start', async () => {
    const saved = race(HEAT, 2, 'Saved', {
      seats: [
        raceSeat(0, 'p-1a', { finish: 2, laps: [lap(1, 1)] }),
        raceSeat(1, 'p-2b', { finish: 1, laps: [lap(2, 1), lap(3, 1)] })
      ]
    });
    await open(
      eventView({
        phases: [
          quali({
            complete: true,
            ranking: [placed('p-2b', 1, { laps: 2 }), placed('p-1a', 2, { laps: 1 })]
          })
        ],
        races: [race(HEAT, 1, 'Discarded', { discard_reason: 'TimerProblem' }), saved]
      })
    );
    expect(screen.getByTestId('run-status')).toHaveTextContent('This heat is complete.');
    expect(screen.queryByRole('button', { name: 'Start' })).toBeNull();
    const ranking = within(screen.getByRole('list', { name: 'Heat ranking' })).getAllByRole(
      'listitem'
    );
    expect(ranking[0]).toHaveTextContent('Bee');
    expect(ranking[1]).toHaveTextContent('Ace');
    const earlier = within(screen.getByRole('list', { name: 'Earlier races' })).getAllByRole(
      'listitem'
    );
    expect(earlier[0]).toHaveTextContent('Race 1');
    expect(earlier[0]).toHaveTextContent('Discarded: timer problem');
    expect(earlier[1]).toHaveTextContent('Race 2');
    expect(earlier[1]).toHaveTextContent('1. Bee (2 laps)');
    expectNoRawIds();
  });

  it('a heat the director ends offers "Call heat done"; a bye offers "Advance without flying"', async () => {
    const practice = { ...FIRST_TO_3, completion: 'DirectorDecides' as const };
    const first = await open(eventView({ phases: [quali({ format: practice })] }));
    await fireEvent.click(screen.getByRole('button', { name: 'Call heat done' }));
    await waitFor(() =>
      expect(first.session.sendCommand).toHaveBeenCalledWith(EVENT.id, {
        Record: { HeatDone: { heat: HEAT } }
      })
    );
    document.body.innerHTML = '';

    await open(eventView({ phases: [heat(HEAT, 'Winners Round 1 Heat 3', ['p-1a', 'Nobody'])] }));
    expect(screen.getByRole('button', { name: 'Advance without flying' })).toBeVisible();
    expect(screen.queryByRole('button', { name: 'Call heat done' })).toBeNull();
  });

  it('says which heat is in the way when another heat has an undecided race', async () => {
    const other = heat('other-7', 'Qualifying Heat 2', ['p-2b']);
    const stopped = race('other-7', 1, 'Stopped');
    const { nav } = await open(
      eventView({ phases: [quali(), other], races: [stopped], current_race: stopped.race })
    );
    expect(screen.getByTestId('run-status')).toHaveTextContent(
      'Qualifying Heat 2 has a race that is not saved or discarded yet.'
    );
    expect(screen.getByRole('button', { name: 'Start' })).toBeDisabled();
    await fireEvent.click(screen.getByRole('button', { name: 'Go to Qualifying Heat 2' }));
    expect(nav.onrun).toHaveBeenCalledWith('other-7');
  });
});

describe('RunPage: which heat is on the timer', () => {
  const day = (over: Partial<EventView> = {}) =>
    eventView({
      phases: [
        group('quali', 'Qualifying'),
        heat('q-1', 'Qualifying Heat 1', ['p-1a'], { parent: 'quali', complete: true }),
        heat('q-2', 'Qualifying Heat 2', ['p-2b'], { parent: 'quali' }),
        heat('q-3', 'Qualifying Heat 3', ['p-1a'], { parent: 'quali' }),
        group('mains', 'Mains'),
        group('mains/a', 'A-Main', 'mains'),
        heat('a-1', 'A-Main Heat 1', ['p-1a'], { parent: 'mains/a', locked: true }),
        heat('a-2', 'A-Main Heat 2', ['p-1a', 'p-2b'], { parent: 'mains/a', waiting: true }),
        heat('solo', 'Open practice', ['p-1a'])
      ],
      races: [race('q-1', 1, 'Saved'), race('q-2', 1, 'Saved'), race('q-2', 2, 'Discarded')],
      ...over
    });

  it('says so when the event has no heat, and points at the Phases page', async () => {
    await open(eventView({ phases: [group('quali', 'Qualifying')] }), {}, null);
    const none = screen.getByTestId('no-heats');
    expect(none).toHaveTextContent('There is no heat to run yet.');
    expect(within(none).getByRole('link')).toHaveAttribute('href', '#/event/phases');
    expect(screen.queryByRole('button', { name: 'Start' })).toBeNull();
  });

  it('with no heat named it runs the heat that has an undecided race', async () => {
    const running = race('q-3', 1, 'Running', { started_at: 0, seats: [raceSeat(0, 'p-1a')] });
    const view = day({ queue: ['q-2', 'q-3'] });
    await open({ ...view, races: [...view.races, running], current_race: running.race }, {}, null);
    expect(heatPicker()).toHaveValue('q-3');
    expect(screen.getByRole('button', { name: 'Stop' })).toBeVisible();
  });

  it('with no heat named and nothing racing it runs the first heat in the queue', async () => {
    const { session } = await open(day({ queue: ['q-3', 'q-2'] }), {}, null);
    expect(heatPicker()).toHaveValue('q-3');
    await fireEvent.click(screen.getByRole('button', { name: 'Start' }));
    await waitFor(() =>
      expect(session.sendCommand).toHaveBeenCalledWith(EVENT.id, { StartRace: { heat: 'q-3' } })
    );
  });

  it('with no heat named and an empty queue it shows the heat raced last', async () => {
    await open(day(), {}, null);
    expect(heatPicker()).toHaveValue('q-2');
  });

  it('offers the phases that hold heats, then the heats of the one chosen with where each stands', async () => {
    await open(day(), {}, 'q-2');
    expect(optionTexts(phasePicker())).toEqual(['Qualifying', 'Mains › A-Main', 'Heats']);
    expect(phasePicker().selectedOptions[0]).toHaveTextContent('Qualifying');
    expect(optionTexts(heatPicker())).toEqual([
      'Qualifying Heat 1 (complete)',
      // One saved race; the discarded one is not counted.
      'Qualifying Heat 2 (1 race)',
      'Qualifying Heat 3'
    ]);
    expect(heatPicker()).toHaveValue('q-2');
    const text = document.body.textContent ?? '';
    for (const raw of ['q-1', 'q-2', 'mains/', 'a-1', 'solo']) expect(text).not.toContain(raw);
  });

  it('says locked and waiting in the heat box too', async () => {
    await open(day(), {}, 'a-2');
    expect(optionTexts(heatPicker())).toEqual([
      'A-Main Heat 1 (locked)',
      'A-Main Heat 2 (waiting)'
    ]);
  });

  it('picking a heat asks for it by id', async () => {
    const { nav } = await open(day(), {}, 'q-2');
    await fireEvent.change(heatPicker(), { target: { value: 'q-3' } });
    expect(nav.onrun).toHaveBeenCalledWith('q-3');
  });

  it('picking a phase puts its first heat that can still be raced on the timer', async () => {
    const { nav } = await open(day(), {}, 'solo');
    const keyOf = (label: string) =>
      [...phasePicker().options].find((option) => option.textContent === label)!.value;
    // Qualifying Heat 1 is complete, so Heat 2 is the first that can be raced.
    await fireEvent.change(phasePicker(), { target: { value: keyOf('Qualifying') } });
    expect(nav.onrun).toHaveBeenLastCalledWith('q-2');
    // A-Main Heat 1 is locked; Heat 2 is waiting but is not done.
    await fireEvent.change(phasePicker(), { target: { value: keyOf('Mains › A-Main') } });
    expect(nav.onrun).toHaveBeenLastCalledWith('a-2');
    expect(nav.onrun).toHaveBeenCalledTimes(2);
  });

  it('holds both boxes still while a race is on', async () => {
    const running = race('q-3', 1, 'Running', { started_at: 0, seats: [raceSeat(0, 'p-1a')] });
    const view = day();
    await open({ ...view, races: [...view.races, running], current_race: running.race }, {}, 'q-3');
    expect(phasePicker()).toBeDisabled();
    expect(heatPicker()).toBeDisabled();
  });

  it('says so for a heat that is not in the event, and offers the next heat', async () => {
    const { nav } = await open(day({ queue: ['q-3'] }), {}, 'gone-heat-0a0a');
    expect(screen.getByText('This heat is not in the event.')).toBeVisible();
    expect(screen.queryByRole('button', { name: 'Start' })).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Go to the next heat' }));
    expect(nav.onrun).toHaveBeenCalledWith('q-3');
    expect(document.body.textContent).not.toContain('gone-heat-0a0a');
  });

  it('names the format in force as a link to the Formats page', async () => {
    await open(eventView({ phases: [quali()] }));
    const rules = screen.getByTestId('run-rules');
    expect(within(rules).getByRole('link', { name: 'First to 3 laps' })).toHaveAttribute(
      'href',
      '#/formats'
    );
  });

  it('says "No format" for a heat with none', async () => {
    await open(eventView({ phases: [quali({ format: undefined })] }));
    expect(screen.getByTestId('run-rules')).toHaveTextContent('No format');
  });
});

describe('RunPage: earlier races', () => {
  it('a saved race that is not locked links to Marshal to fix its laps', async () => {
    const saved = race(HEAT, 1, 'Saved', { seats: [raceSeat(0, 'p-1a')] });
    const discarded = race(HEAT, 2, 'Discarded');
    const locked = race(HEAT, 3, 'Saved', { locked: true });
    await open(eventView({ phases: [quali()], races: [saved, discarded, locked] }));
    const earlier = within(screen.getByRole('list', { name: 'Earlier races' })).getAllByRole(
      'listitem'
    );
    // The address holds the race's id; what is read is its number in the heat.
    expect(
      within(earlier[0]).getByRole('link', { name: 'Fix the laps of race 1' })
    ).toHaveAttribute('href', `#/event/marshal/${encodeURIComponent(saved.race)}`);
    expect(within(earlier[1]).queryByRole('link')).toBeNull();
    expect(within(earlier[2]).queryByRole('link')).toBeNull();
    expectNoRawIds();
  });
});

describe('RunPage: best laps in a row', () => {
  const ranked = (over = {}) => quali({ format: TWO_MINUTE_QUALI, ...over });
  const stat = (seat: HTMLElement, name: string) => seat.querySelector(`[data-stat="${name}"]`);

  it("a heat ranked on best 3 in a row shows this race's run, the heat's best, and which laps make it", async () => {
    const earlier = race(HEAT, 1, 'Saved', {
      seats: [raceSeat(0, 'p-1a', { laps: [lap(1, 10), lap(2, 10), lap(3, 9)] })]
    });
    const running = race(HEAT, 2, 'Running', {
      started_at: 0,
      seats: [
        raceSeat(0, 'p-1a', { laps: [lap(4, 14), lap(5, 10), lap(6, 11), lap(7, 10)] }),
        raceSeat(1, 'p-2b', { laps: [lap(8, 12), lap(9, 12)] })
      ]
    });
    await open(
      eventView({ phases: [ranked()], races: [earlier, running], current_race: running.race })
    );
    const ace = screen.getByRole('listitem', { name: 'Seat 1, Ace' });
    expect(within(ace).getByText('Best 3')).toBeVisible();
    expect(within(ace).getByText('Heat best 3')).toBeVisible();
    // This race: laps 2 to 4 (31 s) beat laps 1 to 3 (35 s). The heat's best is race 1's 29 s.
    expect(stat(ace, 'run')).toHaveTextContent('31.000');
    expect(stat(ace, 'heat-run')).toHaveTextContent('29.000');
    const laps = within(within(ace).getByRole('list', { name: 'Laps for Seat 1' })).getAllByRole(
      'listitem'
    );
    expect(laps.map((item) => item.classList.contains('in-run'))).toEqual([
      false,
      true,
      true,
      true
    ]);
    // Bee has two laps: no run of three yet, here or in the heat.
    const bee = screen.getByRole('listitem', { name: 'Seat 2, Bee' });
    expect(stat(bee, 'run')).toHaveTextContent('—');
    expect(stat(bee, 'heat-run')).toHaveTextContent('—');
  });

  it("the heat's ranking leads with each pilot's best run, the number it is ranked on", async () => {
    const earlier = race(HEAT, 1, 'Saved', {
      seats: [
        raceSeat(0, 'p-1a', { laps: [lap(1, 10), lap(2, 10), lap(3, 9)] }),
        raceSeat(1, 'p-2b', { laps: [lap(4, 12), lap(5, 12)] })
      ]
    });
    await open(
      eventView({
        phases: [
          ranked({ ranking: [placed('p-1a', 1, { laps: 3 }), placed('p-2b', 2, { laps: 2 })] })
        ],
        races: [earlier]
      })
    );
    const rows = within(screen.getByRole('list', { name: 'Heat ranking' })).getAllByRole(
      'listitem'
    );
    expect(stat(rows[0], 'rank-run')).toHaveTextContent('best 3 29.000');
    // Two laps make no run of three.
    expect(stat(rows[1], 'rank-run')).toHaveTextContent('best 3 —');
  });

  it('a heat ranked any other way shows neither', async () => {
    await open(eventView({ phases: [quali()] }));
    const ace = screen.getByRole('listitem', { name: 'Seat 1, Ace' });
    expect(stat(ace, 'run')).toBeNull();
    expect(stat(ace, 'heat-run')).toBeNull();
    expect(within(ace).queryByText(/^Best \d/)).toBeNull();
  });
});

describe('RunPage: keyboard', () => {
  const key = (k: string, init: KeyboardEventInit = {}) =>
    fireEvent.keyDown(window, { key: k, ...init });
  const commands = (session: LiveSession) =>
    vi.mocked(session.sendCommand).mock.calls.map((call) => call[1]);
  const stoppedView = () => {
    const stopped = race(HEAT, 1, 'Stopped', {
      started_at: 0,
      stopped_at: 40 * S,
      seats: [raceSeat(0, 'p-1a'), raceSeat(1, 'p-2b')]
    });
    return {
      stopped,
      view: eventView({ phases: [quali()], races: [stopped], current_race: stopped.race })
    };
  };

  it('the legend is closed until asked for, by its button or by ?', async () => {
    await open(eventView({ phases: [quali()] }));
    const toggle = screen.getByRole('button', { name: 'Keyboard shortcuts' });
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByTestId('run-keys')).toBeNull();
    await fireEvent.click(toggle);
    expect(toggle).toHaveAttribute('aria-expanded', 'true');
    const rows = [...screen.getByTestId('run-keys').children].map((row) =>
      row.textContent?.replace(/\s+/g, ' ').trim()
    );
    expect(rows).toEqual([
      'Space the next step: Start, then Stop, then Save',
      'z Start',
      'x Stop, or call off a countdown',
      'sc Save',
      'dd Discard (press twice within 3 seconds; v works too)',
      '? show or hide this list'
    ]);
    await key('?', { shiftKey: true });
    expect(screen.queryByTestId('run-keys')).toBeNull();
    await key('?', { shiftKey: true });
    expect(screen.getByTestId('run-keys')).toBeVisible();
  });

  it('Space and z start a ready heat', async () => {
    for (const k of [' ', 'z']) {
      const { session, unmount } = await open(eventView({ phases: [quali()] }));
      await key(k);
      await waitFor(() => expect(commands(session)).toEqual([{ StartRace: { heat: HEAT } }]));
      unmount();
    }
  });

  it('keys that do not fit the moment do nothing', async () => {
    const { session } = await open(eventView({ phases: [quali()] }));
    for (const k of ['x', 's', 'c', 'd', 'v', 'q', 'Enter']) await key(k);
    expect(session.sendCommand).not.toHaveBeenCalled();
  });

  it('a heat that cannot start does not start from the keyboard either', async () => {
    const { session } = await open(eventView({ phases: [quali({ waiting: true })] }));
    await key(' ');
    await key('z');
    expect(session.sendCommand).not.toHaveBeenCalled();
  });

  it('Space and x stop a running race', async () => {
    for (const k of [' ', 'x']) {
      const running = race(HEAT, 1, 'Running', { started_at: 0, seats: [raceSeat(0, 'p-1a')] });
      const { session, unmount } = await open(
        eventView({ phases: [quali()], races: [running], current_race: running.race })
      );
      await key(k);
      await waitFor(() =>
        expect(commands(session)).toEqual([{ StopRace: { race: running.race } }])
      );
      unmount();
    }
  });

  it('a second key while the first command is still on its way sends nothing more', async () => {
    let release!: () => void;
    const sendCommand = vi.fn<LiveSession['sendCommand']>(
      () => new Promise((resolve) => (release = () => resolve({ cursor: 1 })))
    );
    const { session } = await open(eventView({ phases: [quali()] }), { sendCommand });
    await key(' ');
    await key(' ');
    await key('z');
    expect(commands(session)).toEqual([{ StartRace: { heat: HEAT } }]);
    release();
  });

  it('x calls off a countdown', async () => {
    const staging = race(HEAT, 1, 'Staging', { tone_at: 5 * S, seats: [raceSeat(0, 'p-1a')] });
    const { session } = await open(
      eventView({ phases: [quali()], races: [staging], current_race: staging.race })
    );
    await key('x');
    await waitFor(() =>
      expect(commands(session)).toEqual([{ DiscardRace: { race: staging.race } }])
    );
  });

  it('Space, s and c save a stopped race', async () => {
    for (const k of [' ', 's', 'c']) {
      const { stopped, view } = stoppedView();
      const { session, unmount } = await open(view);
      await key(k);
      await waitFor(() =>
        expect(commands(session)).toEqual([
          { SaveRace: { race: stopped.race, needs_marshal: false } }
        ])
      );
      unmount();
    }
  });

  it('one d does not discard: it arms the button, and a second d discards', async () => {
    const { stopped, view } = stoppedView();
    const { session } = await open(view);
    await key('d');
    expect(session.sendCommand).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: 'Discard (d again)' })).toBeVisible();
    await key('v');
    await waitFor(() =>
      expect(commands(session)).toEqual([{ DiscardRace: { race: stopped.race } }])
    );
    expect(screen.getByRole('button', { name: 'Discard' })).toBeVisible();
  });

  it('the first d runs out after 3 seconds', async () => {
    vi.useFakeTimers();
    const { view } = stoppedView();
    const { session } = await open(view);
    await key('d');
    await vi.advanceTimersByTimeAsync(2999);
    expect(screen.getByRole('button', { name: 'Discard (d again)' })).toBeVisible();
    await vi.advanceTimersByTimeAsync(1);
    expect(screen.getByRole('button', { name: 'Discard' })).toBeVisible();
    await key('d');
    expect(session.sendCommand).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: 'Discard (d again)' })).toBeVisible();
  });

  it('any other key between the two presses disarms the discard', async () => {
    const { view } = stoppedView();
    const { session } = await open(view);
    await key('d');
    await key('?', { shiftKey: true });
    expect(screen.getByRole('button', { name: 'Discard' })).toBeVisible();
    await key('d');
    expect(session.sendCommand).not.toHaveBeenCalled();
  });

  it('a key typed into the note field is text, not a command', async () => {
    const { view } = stoppedView();
    const { session } = await open(view);
    await fireEvent.click(screen.getByText('Save options'));
    const note = screen.getByRole('textbox', { name: 'Note for this race' });
    note.focus();
    for (const k of [' ', 's', 'd', 'd']) await fireEvent.keyDown(note, { key: k });
    expect(session.sendCommand).not.toHaveBeenCalled();
    note.blur();
  });

  it('keys with Ctrl, Alt or the command key held, and a held key repeating, are left alone', async () => {
    const { session } = await open(eventView({ phases: [quali()] }));
    await key('z', { ctrlKey: true });
    await key('z', { metaKey: true });
    await key('z', { altKey: true });
    await key(' ', { repeat: true });
    expect(session.sendCommand).not.toHaveBeenCalled();
  });

  it('nothing is run from the keyboard while the discard dialog is open', async () => {
    const { view } = stoppedView();
    const { session } = await open(view);
    await fireEvent.click(screen.getByRole('button', { name: 'Discard' }));
    await screen.findByRole('button', { name: 'Timer problem' });
    for (const k of [' ', 's', 'd', 'd']) await key(k);
    expect(session.sendCommand).not.toHaveBeenCalled();
  });
});

describe('RunPage: lap callouts', () => {
  /** A stand-in for the browser's speech engine that keeps what it was asked to say. */
  function stubSpeech() {
    const said: string[] = [];
    const cancel = vi.fn();
    vi.stubGlobal('speechSynthesis', {
      speak: (utterance: { text: string }) => said.push(utterance.text),
      cancel
    });
    vi.stubGlobal(
      'SpeechSynthesisUtterance',
      class {
        rate = 1;
        text: string;
        constructor(text: string) {
          this.text = text;
        }
      }
    );
    return { said, cancel };
  }

  const runningWith = (ace: number[], bee: number[]) => {
    const running = race(HEAT, 1, 'Running', {
      started_at: 0,
      seats: [
        raceSeat(0, 'p-1a', { laps: ace.map((seconds, index) => lap(index + 1, seconds)) }),
        raceSeat(1, 'p-2b', { laps: bee.map((seconds, index) => lap(index + 101, seconds)) })
      ]
    });
    return (cursor: number) =>
      eventView({ cursor, phases: [quali()], races: [running], current_race: running.race });
  };

  /** Open on `first`, then let the test hand the store newer snapshots. */
  async function openFollowing(first: EventView) {
    let view = first;
    const opened = await open(first, { getEventState: vi.fn(async () => view) });
    return {
      ...opened,
      next: async (later: EventView) => {
        view = later;
        await opened.live.reload();
      }
    };
  }

  const callouts = () => screen.getByRole('checkbox', { name: 'Call out laps' });

  it('is on until switched off, and the choice is kept in this browser', async () => {
    stubSpeech();
    const first = await open(eventView({ phases: [quali()] }));
    expect(callouts()).toBeChecked();
    await fireEvent.click(callouts());
    expect(callouts()).not.toBeChecked();
    first.unmount();
    // The page opened again remembers.
    await open(eventView({ phases: [quali()] }));
    expect(callouts()).not.toBeChecked();
    await fireEvent.click(callouts());
    expect(callouts()).toBeChecked();
  });

  it('says each new lap with the pilot and the lap number, and never the laps already there', async () => {
    const { said } = stubSpeech();
    const { next } = await openFollowing(runningWith([9.5], [])(1));
    expect(said).toEqual([]);
    await next(runningWith([9.5, 8.25], [])(2));
    await waitFor(() => expect(said).toEqual(['Ace, lap 2, 8.25']));
    await next(runningWith([9.5, 8.25], [10])(3));
    await waitFor(() => expect(said).toEqual(['Ace, lap 2, 8.25', 'Bee, lap 1, 10.00']));
    // A snapshot with no new lap says nothing more.
    await next(runningWith([9.5, 8.25], [10])(4));
    expect(said).toHaveLength(2);
  });

  it('says nothing while switched off, and picks up with the next lap when switched on', async () => {
    const { said } = stubSpeech();
    const { next } = await openFollowing(runningWith([], [])(1));
    await fireEvent.click(callouts());
    await next(runningWith([9.5], [])(2));
    await fireEvent.click(callouts());
    expect(said).toEqual([]);
    await next(runningWith([9.5, 8.25], [])(3));
    await waitFor(() => expect(said).toEqual(['Ace, lap 2, 8.25']));
  });

  it('stops talking when the page closes', async () => {
    const { cancel } = stubSpeech();
    const { unmount } = await open(eventView({ phases: [quali()] }));
    expect(cancel).not.toHaveBeenCalled();
    unmount();
    expect(cancel).toHaveBeenCalled();
  });

  it('works in a browser with no speech engine: laps still show, nothing is thrown', async () => {
    const { next } = await openFollowing(runningWith([], [])(1));
    await next(runningWith([9.5], [])(2));
    const ace = await screen.findByRole('listitem', { name: 'Seat 1, Ace' });
    await waitFor(() => expect(ace.querySelector('[data-stat="laps"]')).toHaveTextContent('1'));
  });
});

describe('RunPage: what times the heat', () => {
  const GATE_ID = 'gate-x1y2';
  /** The qualifying heat, on a timer's seats instead of the bench. */
  const onGate = (over = {}) => {
    const phase = quali(over);
    phase.lineup = phase.lineup.map((seat, index) => ({
      ...seat,
      adapter: GATE_ID,
      seat: String(index)
    }));
    return phase;
  };
  const gateRace = (state: 'Running' | 'Saved') =>
    race(HEAT, 1, state, {
      started_at: 0,
      seats: [
        { ...raceSeat(0, 'p-1a'), adapter: GATE_ID, seat: '0' },
        { ...raceSeat(1, 'p-2b'), adapter: GATE_ID, seat: '1' }
      ]
    });

  it('a bench heat says it is timed by the Bench and shows the Crossing buttons', async () => {
    await open(eventView({ phases: [quali()] }));
    expect(screen.getByTestId('timed-by')).toHaveTextContent('Timed by Bench');
    expect(screen.getByRole('button', { name: 'Crossing for Seat 1' })).toBeInTheDocument();
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('a heat on a connected timer names the timer and has no Crossing button and no warning', async () => {
    const running = gateRace('Running');
    await open(eventView({ phases: [onGate()], races: [running], current_race: running.race }), {
      listTimers: vi.fn(async () => [timerView(GATE_ID, 'Gate', 4)])
    });
    expect(screen.getByTestId('timed-by')).toHaveTextContent('Timed by Gate');
    expect(screen.getByRole('listitem', { name: 'Seat 1, Ace' })).toBeVisible();
    expect(screen.getByRole('listitem', { name: 'Seat 2, Bee' })).toBeVisible();
    expect(screen.queryByRole('button', { name: /^Crossing/ })).toBeNull();
    expect(screen.queryByText('Bench')).toBeNull();
    expect(screen.queryByRole('alert')).toBeNull();
    expect(document.body.textContent).not.toContain(GATE_ID);
  });

  it("warns in the Director's words when the heat's timer is not connected", async () => {
    const SAID = 'Nothing answered at 10.0.0.9:7270.';
    await open(eventView({ phases: [onGate()] }), {
      listTimers: vi.fn(async () => [
        timerView(GATE_ID, 'Gate', 0, { connected: false, problem: SAID })
      ])
    });
    const warning = screen.getByRole('alert');
    expect(warning).toHaveTextContent('Gate is not connected.');
    expect(warning).toHaveTextContent(SAID);
    // The seats are still named, though the timer has reported none.
    expect(screen.getByRole('listitem', { name: 'Seat 2, Bee' })).toBeVisible();
    expect(document.body.textContent).not.toContain(GATE_ID);
  });

  it('says so when the timer was removed, and never shows its id', async () => {
    await open(eventView({ phases: [onGate()] }));
    expect(screen.getByTestId('timed-by')).toHaveTextContent('Timed by Unknown timer');
    expect(screen.getByRole('alert')).toHaveTextContent('The timer for this heat was removed.');
    expect(screen.getByRole('listitem', { name: 'Seat 1, Ace' })).toBeVisible();
    expect(document.body.textContent).not.toContain(GATE_ID);
  });

  it('a finished heat carries no timer warning', async () => {
    await open(eventView({ phases: [onGate({ complete: true })], races: [gateRace('Saved')] }), {
      listTimers: vi.fn(async () => [timerView(GATE_ID, 'Gate', 0, { connected: false })])
    });
    expect(screen.getByTestId('timed-by')).toHaveTextContent('Timed by Gate');
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it("links each seat on a timer to that seat's Tune page, and no bench seat", async () => {
    await open(
      eventView({ phases: [onGate()] }),
      { listTimers: vi.fn(async () => [timerView(GATE_ID, 'Gate', 4)]) },
      HEAT,
      { ontune: vi.fn() }
    );
    const bee = screen.getByRole('listitem', { name: 'Seat 2, Bee' });
    expect(within(bee).getByRole('link', { name: 'Tune Seat 2' })).toHaveAttribute(
      'href',
      `#/seats/${GATE_ID}/1`
    );
    expect(document.body.textContent).not.toContain(GATE_ID);
    document.body.innerHTML = '';

    await open(eventView({ phases: [quali()] }), {}, HEAT, { ontune: vi.fn() });
    expect(screen.queryByRole('link', { name: /^Tune/ })).toBeNull();
  });
});
