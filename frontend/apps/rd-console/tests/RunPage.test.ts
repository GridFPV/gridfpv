/**
 * The Run view, rendered against a stand-in live store: the one big action follows the
 * race, commands carry the right wire handles, a refusal is shown in the Director's words,
 * and no raw id reaches the screen.
 */
import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { EventMeta, EventView, Pilot } from '@gridfpv/types';
import RunPage from '../src/screens/RunPage.svelte';
import { EventLive, type LiveSession } from '../src/lib/eventLive.svelte.js';
import { eventView, FIRST_TO_3, heat, race, raceSeat, timerView } from './competition.js';

const EVENT: EventMeta = { id: 'evt-77', name: 'Club Night', created_at: 0, persistent: true };
const PILOTS: Pilot[] = [
  { id: 'p-1a', callsign: 'Ace', vtx_types: [] },
  { id: 'p-2b', callsign: 'Bee', vtx_types: [] }
];
const HEAT = 'quali-x9z1';
const S = 1_000_000;

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
  const nav = { onhome: vi.fn(), onevents: vi.fn(), onevent: vi.fn(), onrun: vi.fn() };
  render(RunPage, { event: EVENT, live, heat: HEAT, ...nav });
  return { session, live, nav };
}

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
    expect(screen.getByRole('heading', { level: 1, name: 'Qualifying Heat 1' })).toBeVisible();
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

  it('the breadcrumb ends on the heat and leads back to the event', async () => {
    const { nav } = await open(eventView({ phases: [quali()] }));
    const crumbs = screen.getByRole('navigation', { name: 'Breadcrumb' });
    expect(within(crumbs).getByText('Qualifying Heat 1')).toHaveAttribute('aria-current', 'page');
    await fireEvent.click(within(crumbs).getByRole('button', { name: 'Club Night' }));
    expect(nav.onevent).toHaveBeenCalled();
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
    const lap = (crossing: number, micros: number) => ({
      crossing,
      started_at: 0,
      ended_at: micros,
      micros,
      restart: false
    });
    const running = race(HEAT, 1, 'Running', {
      started_at: 0,
      end: { FirstToLaps: 3 },
      seats: [
        raceSeat(0, 'p-1a', {
          finish: 1,
          laps: [lap(3, 9_500_000), lap(5, 8_250_000)],
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
    const lap = (crossing: number) => ({
      crossing,
      started_at: 0,
      ended_at: S,
      micros: S,
      restart: false
    });
    const saved = race(HEAT, 2, 'Saved', {
      seats: [
        raceSeat(0, 'p-1a', { finish: 2, laps: [lap(1)] }),
        raceSeat(1, 'p-2b', { finish: 1, laps: [lap(2), lap(3)] })
      ]
    });
    const placed = (pilot: string, position: number, laps: number) => ({
      pilot,
      position,
      disqualified: false,
      position_set: false,
      flew: true,
      wins: 0,
      points: 0,
      laps
    });
    await open(
      eventView({
        phases: [quali({ complete: true, ranking: [placed('p-2b', 1, 2), placed('p-1a', 2, 1)] })],
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
    await fireEvent.click(screen.getByRole('button', { name: 'Open Qualifying Heat 2' }));
    expect(nav.onrun).toHaveBeenCalledWith('other-7');
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
});
