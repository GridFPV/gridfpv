/**
 * The Tune page against a mocked session and a fake live feed: the timer's name and status,
 * the settings read from the timer, live levels shown as numbers, the draft and its apply, a
 * refusal shown in the Director's words, and everything stopping when the page closes.
 *
 * The trace is a canvas, which jsdom does not draw; `tests/signal.test.ts` covers what it
 * draws from. Here the canvas only has to be present, named, and beside its numbers.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { TimerLiveHandlers } from '@gridfpv/protocol-client';
import type { SeatLevels, SignalChunk, TimerConfig, TimerView } from '@gridfpv/types';
import TunePage from '../src/screens/TunePage.svelte';
import { timerView } from './competition.js';
import { makeTestSession, type SessionSeams } from './support.js';

const ID = 'gate-x1y2';
const GATE = timerView(ID, 'Gate', 2);
const SAID = 'Nothing answered at 127.0.0.1:9.';
const OFF: TimerView = {
  timer: { id: ID, name: 'Gate', kind: 'Oft', address: '127.0.0.1:9' },
  status: { connected: false, problem: SAID, seats: [] }
};

/** The same timer, saying it takes levels per seat. */
const PER_SEAT = timerView(ID, 'Gate', 2, { seat_thresholds: true });

const CONFIG: TimerConfig = {
  seats: [
    { seat: '0', freq_mhz: 5658, enabled: true },
    { seat: '1', freq_mhz: 5700, enabled: false }
  ],
  seats_per_node: 4,
  threshold: { Auto: { snr_enter: 150, hysteresis: 50 } },
  min_lap_ms: 2500
};

const LEVELS: SeatLevels[] = [
  { seat: '0', level: -773, floor: -921, enter: -771, exit: -821, crossing: true },
  { seat: '1', enter: -771, exit: -821, crossing: false }
];

const CHUNK: SignalChunk = {
  adapter: ID,
  seat: '0',
  from: 5_000_000,
  period_micros: 1000,
  levels: [-900, -895, -640],
  unit: 'TenthsDbm'
};

/** A live feed the test drives by hand. */
function fakeFeed() {
  const feed = {
    handlers: undefined as TimerLiveHandlers | undefined,
    opened: [] as string[],
    stopped: 0,
    subscribeTimerLiveImpl: (_base: string, id: string, handlers: TimerLiveHandlers) => {
      feed.opened.push(id);
      feed.handlers = handlers;
      return () => {
        feed.stopped += 1;
      };
    }
  };
  return feed;
}

function open(seams: SessionSeams = {}, view: TimerView = GATE) {
  const feed = fakeFeed();
  const getTimerConfigImpl = vi.fn(async () => structuredClone(CONFIG));
  const putTimerConfigImpl = vi.fn(async (_base: string, _id: string, config: TimerConfig) =>
    structuredClone(config)
  );
  const { session } = makeTestSession({
    noEnter: true,
    listTimersImpl: vi.fn(async () => [view]),
    getTimerConfigImpl,
    putTimerConfigImpl,
    subscribeTimerLiveImpl: feed.subscribeTimerLiveImpl,
    ...seams
  });
  const onhome = vi.fn();
  const ontimers = vi.fn();
  const rendered = render(TunePage, { session, timer: ID, onhome, ontimers });
  return { feed, getTimerConfigImpl, putTimerConfigImpl, onhome, ontimers, ...rendered };
}

const seatRow = (name: string) =>
  within(screen.getByRole('list', { name: 'Seats' })).getByRole('listitem', { name });

beforeEach(() => {
  // jsdom has no canvas. The trace draws nothing here; this only keeps jsdom quiet about it.
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
});
afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe('TunePage', () => {
  it("shows the timer's name, status and breadcrumbs, and never its id", async () => {
    const { ontimers, onhome } = open();
    expect(await screen.findByRole('heading', { name: 'Gate', level: 1 })).toBeVisible();
    expect(screen.getByText('Connected')).toBeVisible();
    const crumbs = screen.getByRole('navigation', { name: 'Breadcrumb' });
    expect(crumbs).toHaveTextContent('Home');
    expect(crumbs).toHaveTextContent('Timers');
    expect(crumbs).toHaveTextContent('Gate');
    await fireEvent.click(within(crumbs).getByRole('button', { name: 'Timers' }));
    expect(ontimers).toHaveBeenCalled();
    await fireEvent.click(within(crumbs).getByRole('button', { name: 'Home' }));
    expect(onhome).toHaveBeenCalled();
    await screen.findByRole('list', { name: 'Seats' });
    expect(screen.queryByTestId('not-connected')).toBeNull();
    expect(document.body.textContent).not.toContain(ID);
  });

  it("reads the settings from the timer and shows each seat's frequency and channel", async () => {
    const { getTimerConfigImpl } = open();
    const one = await waitFor(() => seatRow('Seat 1'));
    expect(getTimerConfigImpl).toHaveBeenCalledWith('http://d.local', ID, { token: 'tok' });
    expect(within(one).getByLabelText('Seat 1 frequency in MHz')).toHaveValue(5658);
    expect(within(one).getByLabelText('Seat 1 channel')).toHaveValue('5658');
    expect(within(one).getByTestId('seat-channel')).toHaveTextContent('R1');
    expect(within(one).getByRole('switch', { name: 'Seat 1 on' })).toBeChecked();

    // A frequency that is not a channel is plain MHz, and the picker says "Other".
    const two = seatRow('Seat 2');
    expect(within(two).getByLabelText('Seat 2 frequency in MHz')).toHaveValue(5700);
    expect(within(two).getByLabelText('Seat 2 channel')).toHaveValue('other');
    expect(within(two).getByTestId('seat-channel')).toHaveTextContent('');
    expect(within(two).getByRole('switch', { name: 'Seat 2 on' })).not.toBeChecked();
    expect(within(two).getByTestId('seat-live')).toHaveTextContent('Off on the timer');

    // Detection, in dB and seconds.
    expect(screen.getByRole('radio', { name: 'Follow the noise floor' })).toBeChecked();
    expect(screen.getByLabelText('Enter this far above the floor, in dB')).toHaveValue(15);
    expect(screen.getByLabelText('Exit this far below enter, in dB')).toHaveValue(5);
    expect(screen.getByLabelText('Minimum lap time, in seconds')).toHaveValue(2.5);
    expect(screen.getByLabelText('Seats per receiver')).toHaveValue(4);
    expect(screen.getByText(/each seat's signal is read less often/)).toBeVisible();

    // Nothing is changed yet.
    expect(screen.getByText('This is what the timer is using.')).toBeVisible();
    expect(screen.getByRole('button', { name: 'Apply to timer' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Discard changes' })).toBeDisabled();
  });

  it('shows live levels as numbers, a lit crossing, and a named trace with its numbers', async () => {
    const { feed } = open();
    const one = await waitFor(() => seatRow('Seat 1'));
    expect(feed.opened).toEqual([ID]);
    expect(within(one).getByTestId('seat-live')).toHaveTextContent('Waiting for levels…');

    // One trace for the seat that is on; none for the seat that is off.
    const traces = screen.getByRole('list', { name: 'Signal' });
    expect(within(traces).getAllByRole('listitem')).toHaveLength(1);
    const trace = within(traces).getByRole('listitem', { name: 'Seat 1' });
    expect(trace).toHaveTextContent('R1 · 5658 MHz');
    expect(
      within(trace).getByRole('img', { name: 'Signal on Seat 1 over the last 10 seconds' })
    ).toBeInTheDocument();

    feed.handlers!.onLevels(LEVELS);
    await waitFor(() => expect(within(one).getByTestId('seat-live')).toHaveTextContent('-77.3 dB'));
    expect(within(one).getByTestId('seat-live')).toHaveTextContent('Noise floor -92.1 dB');
    const lamp = within(one).getByText('Crossing now');
    expect(lamp).toHaveAttribute('data-crossing', 'true');
    expect(lamp).toHaveClass('lit');

    // The same numbers beside the trace, with the enter and exit levels.
    const numbers = within(trace).getByLabelText('Seat 1 levels');
    expect(numbers).toHaveTextContent('Level -77.3 dB');
    expect(numbers).toHaveTextContent('Noise floor -92.1 dB');
    expect(numbers).toHaveTextContent('Enter -77.1 dB');
    expect(numbers).toHaveTextContent('Exit -82.1 dB');

    // A calibrated timer's chunks say dBm, and the page follows.
    feed.handlers!.onSignal(CHUNK);
    await waitFor(() => expect(numbers).toHaveTextContent('Level -77.3 dBm'));

    // The crossing ends.
    feed.handlers!.onLevels([{ ...LEVELS[0], level: -900, crossing: false }, LEVELS[1]]);
    const dark = await within(one).findByText('No crossing');
    expect(dark).not.toHaveClass('lit');
  });

  it('keeps edits as a draft, then applies the whole settings and takes what came back', async () => {
    const kept: TimerConfig = {
      ...CONFIG,
      seats: [
        { seat: '0', freq_mhz: 5806, enabled: true },
        { seat: '1', freq_mhz: 5700, enabled: true }
      ],
      // The timer rounds the lap time its own way; the page must show the timer's answer.
      min_lap_ms: 3000
    };
    const putTimerConfigImpl = vi.fn(async () => structuredClone(kept));
    open({ putTimerConfigImpl });
    const one = await waitFor(() => seatRow('Seat 1'));

    await fireEvent.change(within(one).getByLabelText('Seat 1 channel'), {
      target: { value: '5806' }
    });
    await waitFor(() => expect(within(one).getByTestId('seat-channel')).toHaveTextContent('R5'));
    expect(within(one).getByLabelText('Seat 1 frequency in MHz')).toHaveValue(5806);
    await fireEvent.click(screen.getByRole('switch', { name: 'Seat 2 on' }));
    await fireEvent.input(screen.getByLabelText('Minimum lap time, in seconds'), {
      target: { value: '3.2' }
    });

    // Nothing was sent by editing.
    expect(putTimerConfigImpl).not.toHaveBeenCalled();
    expect(await screen.findByText('These changes are not on the timer yet.')).toBeVisible();
    expect(screen.getByText('Not applied')).toBeVisible();

    await fireEvent.click(screen.getByRole('button', { name: 'Apply to timer' }));
    await waitFor(() =>
      expect(putTimerConfigImpl).toHaveBeenCalledWith(
        'http://d.local',
        ID,
        {
          seats: [
            { seat: '0', freq_mhz: 5806, enabled: true },
            { seat: '1', freq_mhz: 5700, enabled: true }
          ],
          seats_per_node: 4,
          threshold: { Auto: { snr_enter: 150, hysteresis: 50 } },
          min_lap_ms: 3200
        },
        'tok'
      )
    );
    expect(await screen.findByText('This is what the timer is using.')).toBeVisible();
    expect(screen.getByLabelText('Minimum lap time, in seconds')).toHaveValue(3);
    // Seat 2 is on now, so it has a trace.
    expect(
      within(screen.getByRole('list', { name: 'Signal' })).getAllByRole('listitem')
    ).toHaveLength(2);
  });

  it("shows the Director's sentence when the timer says no, and keeps the draft", async () => {
    const REFUSED = 'The timer did not take the change. It is still set the way it was.';
    open({
      putTimerConfigImpl: vi.fn(async () => {
        throw Object.assign(new Error(REFUSED), { status: 409, code: 'Refused' });
      })
    });
    await waitFor(() => seatRow('Seat 1'));
    await fireEvent.input(screen.getByLabelText('Seats per receiver'), { target: { value: '2' } });
    await fireEvent.click(await screen.findByRole('button', { name: 'Apply to timer' }));

    const problem = await screen.findByTestId('apply-problem');
    expect(problem).toHaveTextContent('The timer was not changed.');
    expect(problem).toHaveTextContent(REFUSED);
    expect(screen.getByLabelText('Seats per receiver')).toHaveValue(2);
    expect(screen.getByText('These changes are not on the timer yet.')).toBeVisible();
    expect(screen.getByRole('button', { name: 'Apply to timer' })).toBeEnabled();
  });

  it('will not apply a draft with a hole in it, and says what is missing', async () => {
    const { putTimerConfigImpl } = open();
    const one = await waitFor(() => seatRow('Seat 1'));
    // "Other frequency" empties the field so the director types one.
    await fireEvent.change(within(one).getByLabelText('Seat 1 channel'), {
      target: { value: 'other' }
    });
    expect(await screen.findByText('Seat 1 needs a frequency in whole MHz.')).toBeVisible();
    expect(screen.getByRole('button', { name: 'Apply to timer' })).toBeDisabled();
    await fireEvent.input(within(one).getByLabelText('Seat 1 frequency in MHz'), {
      target: { value: '5710' }
    });
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Apply to timer' })).toBeEnabled()
    );
    expect(putTimerConfigImpl).not.toHaveBeenCalled();
  });

  it('discards the draft and reads the settings from the timer again', async () => {
    const { getTimerConfigImpl } = open();
    await waitFor(() => seatRow('Seat 1'));
    expect(getTimerConfigImpl).toHaveBeenCalledTimes(1);
    await fireEvent.input(screen.getByLabelText('Seats per receiver'), { target: { value: '2' } });
    await fireEvent.click(await screen.findByRole('button', { name: 'Discard changes' }));
    await waitFor(() => expect(getTimerConfigImpl).toHaveBeenCalledTimes(2));
    expect(screen.getByLabelText('Seats per receiver')).toHaveValue(4);
    expect(await screen.findByText('This is what the timer is using.')).toBeVisible();
  });

  it('fills the fixed levels from the levels the timer is using right now', async () => {
    const { feed, putTimerConfigImpl } = open();
    await waitFor(() => seatRow('Seat 1'));
    feed.handlers!.onLevels(LEVELS);
    await fireEvent.click(screen.getByRole('radio', { name: 'Fixed levels' }));
    expect(await screen.findByLabelText('Enter level, in dB')).toHaveValue(-77.1);
    expect(screen.getByLabelText('Exit level, in dB')).toHaveValue(-82.1);
    await fireEvent.click(screen.getByRole('button', { name: 'Apply to timer' }));
    await waitFor(() => expect(putTimerConfigImpl).toHaveBeenCalled());
    expect(putTimerConfigImpl.mock.calls[0][2].threshold).toEqual({
      Manual: { enter: -771, exit: -821 }
    });
  });

  describe("a seat's own levels", () => {
    const choice = (name: string) =>
      within(seatRow(name)).getByLabelText(`${name}: this seat's levels`);
    const TIMER_WIDE =
      'When the timer counts a crossing. These settings are for every seat on this timer.';
    const TIMER_DEFAULT =
      "When the timer counts a crossing. These levels are the timer's own. Every seat that has none of its own uses them.";

    it('offers none on a timer that does not take them, and sends none', async () => {
      const { putTimerConfigImpl } = open();
      await waitFor(() => seatRow('Seat 1'));
      expect(screen.queryByText("This seat's levels")).toBeNull();
      expect(screen.queryByLabelText("Seat 1: this seat's levels")).toBeNull();
      expect(screen.getByText(TIMER_WIDE)).toBeVisible();
      expect(screen.queryByText(TIMER_DEFAULT)).toBeNull();

      await fireEvent.click(screen.getByRole('switch', { name: 'Seat 2 on' }));
      await fireEvent.click(await screen.findByRole('button', { name: 'Apply to timer' }));
      await waitFor(() => expect(putTimerConfigImpl).toHaveBeenCalled());
      for (const seat of putTimerConfigImpl.mock.calls[0][2].seats) {
        expect('threshold' in seat).toBe(false);
      }
    });

    it("offers the choice on a timer that takes them, and says the timer's levels are the default", async () => {
      open({}, PER_SEAT);
      await waitFor(() => seatRow('Seat 1'));
      expect(choice('Seat 1')).toHaveValue('timer');
      expect(choice('Seat 2')).toHaveValue('timer');
      expect(within(seatRow('Seat 1')).getByText("This seat's levels")).toBeVisible();
      expect(
        within(choice('Seat 1')).getByRole('option', { name: 'Same as the timer' })
      ).toBeTruthy();
      expect(within(choice('Seat 1')).getByRole('option', { name: 'Its own' })).toBeTruthy();
      // No fields for a seat on the timer's levels.
      expect(within(seatRow('Seat 1')).queryByRole('radio')).toBeNull();
      expect(screen.getByText(TIMER_DEFAULT)).toBeVisible();
      expect(screen.queryByText(TIMER_WIDE)).toBeNull();
      expect(screen.getByText('This is what the timer is using.')).toBeVisible();
    });

    it('gives a seat its own levels, filled from the levels it is using right now, and applies them', async () => {
      const { feed, putTimerConfigImpl } = open({}, PER_SEAT);
      const one = await waitFor(() => seatRow('Seat 1'));
      feed.handlers!.onLevels(LEVELS);
      await waitFor(() => expect(within(one).getByTestId('seat-live')).toHaveTextContent('-77.3'));

      await fireEvent.change(choice('Seat 1'), { target: { value: 'own' } });
      // It starts the way the timer's levels are set, and the draft is changed already.
      const group = await within(one).findByRole('group', {
        name: 'Seat 1 enter and exit levels'
      });
      expect(within(group).getByRole('radio', { name: 'Follow the noise floor' })).toBeChecked();
      expect(
        within(one).getByLabelText('Seat 1 enter this far above the floor, in dB')
      ).toHaveValue(15);
      expect(within(one).getByLabelText('Seat 1 exit this far below enter, in dB')).toHaveValue(5);
      expect(await screen.findByText('These changes are not on the timer yet.')).toBeVisible();

      // Fixed levels start from the levels this seat is using right now.
      await fireEvent.click(within(group).getByRole('radio', { name: 'Fixed levels' }));
      expect(await within(one).findByLabelText('Seat 1 enter level, in dB')).toHaveValue(-77.1);
      expect(within(one).getByLabelText('Seat 1 exit level, in dB')).toHaveValue(-82.1);
      await fireEvent.input(within(one).getByLabelText('Seat 1 enter level, in dB'), {
        target: { value: '-70' }
      });

      // The timer's own levels are not touched by any of it.
      expect(screen.getByLabelText('Enter this far above the floor, in dB')).toHaveValue(15);
      const timerMode = screen.getByRole('group', { name: 'Enter and exit levels' });
      expect(
        within(timerMode).getByRole('radio', { name: 'Follow the noise floor' })
      ).toBeChecked();
      // The other seat stays on the timer's.
      expect(choice('Seat 2')).toHaveValue('timer');

      await fireEvent.click(screen.getByRole('button', { name: 'Apply to timer' }));
      await waitFor(() => expect(putTimerConfigImpl).toHaveBeenCalled());
      const sent = putTimerConfigImpl.mock.calls[0][2];
      expect(sent.seats[0]).toEqual({
        seat: '0',
        freq_mhz: 5658,
        enabled: true,
        threshold: { Manual: { enter: -700, exit: -821 } }
      });
      expect('threshold' in sent.seats[1]).toBe(false);
      expect(sent.threshold).toEqual(CONFIG.threshold);
      // What came back has the seat's own levels, and the page shows them as applied.
      expect(await screen.findByText('This is what the timer is using.')).toBeVisible();
      expect(choice('Seat 1')).toHaveValue('own');
      expect(within(seatRow('Seat 1')).getByLabelText('Seat 1 enter level, in dB')).toHaveValue(
        -70
      );
    });

    it("shows a seat's own levels from the timer, sends them again with any other change, and omits the key for 'Same as the timer'", async () => {
      const own: TimerConfig = {
        ...CONFIG,
        seats: [
          { ...CONFIG.seats[0], threshold: { Manual: { enter: -700, exit: -760 } } },
          { ...CONFIG.seats[1], threshold: { Auto: { snr_enter: 120, hysteresis: 40 } } }
        ]
      };
      const { putTimerConfigImpl } = open(
        { getTimerConfigImpl: vi.fn(async () => structuredClone(own)) },
        PER_SEAT
      );
      const one = await waitFor(() => seatRow('Seat 1'));
      expect(choice('Seat 1')).toHaveValue('own');
      expect(within(one).getByLabelText('Seat 1 enter level, in dB')).toHaveValue(-70);
      expect(within(one).getByLabelText('Seat 1 exit level, in dB')).toHaveValue(-76);
      expect(
        within(seatRow('Seat 2')).getByLabelText('Seat 2 enter this far above the floor, in dB')
      ).toHaveValue(12);
      expect(screen.getByText('This is what the timer is using.')).toBeVisible();

      // Seat 2 goes back to the timer's levels; seat 1 is not touched.
      await fireEvent.change(choice('Seat 2'), { target: { value: 'timer' } });
      await waitFor(() => expect(within(seatRow('Seat 2')).queryByRole('radio')).toBeNull());
      await fireEvent.click(await screen.findByRole('button', { name: 'Apply to timer' }));
      await waitFor(() => expect(putTimerConfigImpl).toHaveBeenCalled());
      const sent = putTimerConfigImpl.mock.calls[0][2];
      expect(sent.seats[0].threshold).toEqual({ Manual: { enter: -700, exit: -760 } });
      expect('threshold' in sent.seats[1]).toBe(false);
    });

    it("says which seat's levels have a hole, and Discard puts the seat back as the timer has it", async () => {
      const { getTimerConfigImpl } = open({}, PER_SEAT);
      const one = await waitFor(() => seatRow('Seat 1'));
      await fireEvent.change(choice('Seat 1'), { target: { value: 'own' } });
      const group = await within(one).findByRole('group', {
        name: 'Seat 1 enter and exit levels'
      });
      // No live levels yet, and the timer follows the floor, so fixed levels start empty.
      await fireEvent.click(within(group).getByRole('radio', { name: 'Fixed levels' }));
      expect(await screen.findByText('Seat 1 needs an enter level.')).toBeVisible();
      expect(screen.getByRole('button', { name: 'Apply to timer' })).toBeDisabled();

      await fireEvent.click(screen.getByRole('button', { name: 'Discard changes' }));
      await waitFor(() => expect(getTimerConfigImpl).toHaveBeenCalledTimes(2));
      expect(await screen.findByText('This is what the timer is using.')).toBeVisible();
      expect(choice('Seat 1')).toHaveValue('timer');
      expect(within(seatRow('Seat 1')).queryByRole('radio')).toBeNull();
    });

    it("shows the Director's sentence as it is when the timer will not take a seat's levels", async () => {
      const REFUSED =
        'This timer has one threshold for all its seats. It cannot take one for a single seat.';
      open(
        {
          putTimerConfigImpl: vi.fn(async () => {
            throw Object.assign(new Error(REFUSED), { status: 400, code: 'BadRequest' });
          })
        },
        PER_SEAT
      );
      await waitFor(() => seatRow('Seat 1'));
      await fireEvent.change(choice('Seat 1'), { target: { value: 'own' } });
      await fireEvent.click(await screen.findByRole('button', { name: 'Apply to timer' }));
      expect(await screen.findByTestId('apply-problem')).toHaveTextContent(REFUSED);
      expect(choice('Seat 1')).toHaveValue('own');
    });

    it("draws each seat's trace against that seat's own levels", async () => {
      const bothOn: TimerConfig = {
        ...CONFIG,
        seats: [
          CONFIG.seats[0],
          { ...CONFIG.seats[1], enabled: true, threshold: { Manual: { enter: -600, exit: -660 } } }
        ]
      };
      const { feed } = open(
        { getTimerConfigImpl: vi.fn(async () => structuredClone(bothOn)) },
        PER_SEAT
      );
      await waitFor(() => seatRow('Seat 1'));
      feed.handlers!.onLevels([
        LEVELS[0],
        { seat: '1', level: -880, floor: -915, enter: -600, exit: -660, crossing: false }
      ]);
      const traces = screen.getByRole('list', { name: 'Signal' });
      const one = within(traces).getByRole('listitem', { name: 'Seat 1' });
      const two = within(traces).getByRole('listitem', { name: 'Seat 2' });
      await waitFor(() =>
        expect(within(two).getByLabelText('Seat 2 levels')).toHaveTextContent('Enter -60.0 dB')
      );
      expect(within(two).getByLabelText('Seat 2 levels')).toHaveTextContent('Exit -66.0 dB');
      expect(within(one).getByLabelText('Seat 1 levels')).toHaveTextContent('Enter -77.1 dB');
      expect(within(one).getByLabelText('Seat 1 levels')).toHaveTextContent('Exit -82.1 dB');
    });
  });

  it('adds a seat to the draft, up to eight, and names it Seat N', async () => {
    const { putTimerConfigImpl } = open();
    await waitFor(() => seatRow('Seat 1'));
    const add = screen.getByRole('button', { name: '+ Add seat' });
    await fireEvent.click(add);
    const three = await waitFor(() => seatRow('Seat 3'));
    expect(within(three).getByLabelText('Seat 3 frequency in MHz')).toHaveValue(5695);
    expect(within(three).getByTestId('seat-live')).toHaveTextContent('Not on the timer yet');
    expect(putTimerConfigImpl).not.toHaveBeenCalled();
    for (let i = 0; i < 5; i += 1) await fireEvent.click(add);
    await waitFor(() => seatRow('Seat 8'));
    expect(add).toBeDisabled();
    expect(document.body.textContent).not.toContain(ID);
  });

  it("says the timer is not connected, with the Director's sentences, where the settings would be", async () => {
    const REACH = 'Could not reach Gate: connection refused';
    open(
      {
        getTimerConfigImpl: vi.fn(async () => {
          throw Object.assign(new Error(REACH), { status: 409, code: 'Refused' });
        })
      },
      OFF
    );
    const banner = await screen.findByTestId('not-connected');
    expect(banner).toHaveTextContent('Gate is not connected.');
    expect(banner).toHaveTextContent(SAID);
    expect(screen.getByText('Not connected')).toBeVisible();
    const problem = await screen.findByTestId('settings-problem');
    expect(problem).toHaveTextContent("The timer's settings could not be read.");
    expect(problem).toHaveTextContent(REACH);
    expect(screen.queryByRole('list', { name: 'Seats' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Apply to timer' })).toBeNull();
    expect(document.body.textContent).not.toContain(ID);
  });

  it('reads the settings again when the timer connects, and on Try again', async () => {
    let view = OFF;
    let reachable = false;
    const getTimerConfigImpl = vi.fn(async () => {
      if (!reachable)
        throw Object.assign(new Error('Could not reach Gate: timed out'), { status: 409 });
      return structuredClone(CONFIG);
    });
    vi.useFakeTimers();
    open({ listTimersImpl: vi.fn(async () => [view]), getTimerConfigImpl });
    await vi.advanceTimersByTimeAsync(0);
    expect(screen.getByTestId('settings-problem')).toBeVisible();
    expect(getTimerConfigImpl).toHaveBeenCalledTimes(1);

    // Still not connected: a manual try, still refused.
    await fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await vi.advanceTimersByTimeAsync(0);
    expect(getTimerConfigImpl).toHaveBeenCalledTimes(2);

    // The timer connects; the next read of the list brings the settings by itself.
    view = GATE;
    reachable = true;
    await vi.advanceTimersByTimeAsync(2000);
    expect(getTimerConfigImpl).toHaveBeenCalledTimes(3);
    expect(screen.getByRole('list', { name: 'Seats' })).toBeVisible();
    expect(screen.queryByTestId('settings-problem')).toBeNull();
    expect(screen.queryByTestId('not-connected')).toBeNull();
  });

  it('says so when the timer is not in the list any more, without its id', async () => {
    const { ontimers } = open({ listTimersImpl: vi.fn(async () => []) });
    expect(await screen.findByText('This timer is not in the list any more.')).toBeVisible();
    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Unknown timer');
    await fireEvent.click(screen.getByRole('button', { name: 'Back to Timers' }));
    expect(ontimers).toHaveBeenCalled();
    expect(document.body.textContent).not.toContain(ID);
  });

  it('stops the live feed and the reads when it closes', async () => {
    vi.useFakeTimers();
    const listTimersImpl = vi.fn(async () => [GATE]);
    const { feed, unmount } = open({ listTimersImpl });
    await vi.advanceTimersByTimeAsync(0);
    expect(listTimersImpl).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(4000);
    expect(listTimersImpl).toHaveBeenCalledTimes(3);
    expect(feed.stopped).toBe(0);
    unmount();
    expect(feed.stopped).toBe(1);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(listTimersImpl).toHaveBeenCalledTimes(3);
  });
});
