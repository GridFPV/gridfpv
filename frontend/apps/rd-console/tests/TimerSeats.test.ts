/**
 * One timer's seats and settings (the body of the Seats page) against a mocked session and a
 * fake live feed: the settings read from the timer, live levels shown as numbers, the draft
 * and its apply, a refusal shown in the Director's words, the profile bar, and the live feed
 * stopping when the page closes. The timer list is the Seats page's and is handed in; what
 * the Seats page does with it is in `SeatsPage.test.ts`.
 *
 * The trace is a canvas, which jsdom does not draw; `tests/signal.test.ts` covers what it
 * draws from. Here the canvas only has to be present, named, and beside its numbers.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { TimerLiveHandlers } from '@gridfpv/protocol-client';
import type {
  SeatLevels,
  SeatProfile,
  SeatProfileRequest,
  SignalChunk,
  TimerConfig,
  TimerProfiles,
  TimerView
} from '@gridfpv/types';
import TimerSeats from '../src/screens/TimerSeats.svelte';
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
  threshold: { Auto: { snr_enter: 150, hysteresis: 50 } }
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
    getTimerConfigImpl,
    // No profile is saved unless a test says so.
    listProfilesImpl: vi.fn(async (): Promise<TimerProfiles> => ({ profiles: [] })),
    putTimerConfigImpl,
    subscribeTimerLiveImpl: feed.subscribeTimerLiveImpl,
    ...seams
  });
  const rendered = render(TimerSeats, { session, timer: ID, timers: [view] });
  return { session, feed, getTimerConfigImpl, putTimerConfigImpl, ...rendered };
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

describe('TimerSeats', () => {
  it("shows the seats by name with a Tune link on each seat that is on, and never the timer's id", async () => {
    open();
    const one = await waitFor(() => seatRow('Seat 1'));
    expect(screen.queryByTestId('not-connected')).toBeNull();
    expect(screen.queryByTestId('settings-problem')).toBeNull();
    // The link's address holds the ids; nothing a person reads does.
    expect(within(one).getByRole('link', { name: 'Tune Seat 1' })).toHaveAttribute(
      'href',
      `#/seats/${ID}/0`
    );
    // Seat 2 is off on the timer, so there is no signal to record.
    expect(within(seatRow('Seat 2')).queryByRole('link')).toBeNull();
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

    // The timer's own settings, in dB.
    expect(screen.getByRole('heading', { name: 'Timer settings' })).toBeVisible();
    expect(screen.queryByRole('heading', { name: 'Detection' })).toBeNull();
    expect(screen.getByRole('radio', { name: 'Follow the noise floor' })).toBeChecked();
    expect(screen.getByLabelText('Enter this far above the floor, in dB')).toHaveValue(15);
    expect(screen.getByLabelText('Exit this far below enter, in dB')).toHaveValue(5);
    // The minimum lap is the event's rule and is not on this page. Seats per receiver is the
    // timer's own business and is not here either. A timer that reports no lap cooldown and
    // no exit hold is offered neither.
    expect(screen.queryByLabelText('Minimum lap time, in seconds')).toBeNull();
    expect(screen.queryByText(/Seats per receiver/i)).toBeNull();
    expect(screen.queryByLabelText('Lap cooldown, in seconds')).toBeNull();
    expect(screen.queryByLabelText('Exit hold, in milliseconds')).toBeNull();

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
      // The timer rounds a level its own way; the page must show the timer's answer.
      threshold: { Auto: { snr_enter: 150, hysteresis: 40 } }
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
    await fireEvent.input(screen.getByLabelText('Exit this far below enter, in dB'), {
      target: { value: '4.4' }
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
          threshold: { Auto: { snr_enter: 150, hysteresis: 44 } }
        },
        'tok'
      )
    );
    expect(await screen.findByText('This is what the timer is using.')).toBeVisible();
    expect(screen.getByLabelText('Exit this far below enter, in dB')).toHaveValue(4);
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
    await fireEvent.input(screen.getByLabelText('Exit this far below enter, in dB'), {
      target: { value: '2' }
    });
    await fireEvent.click(await screen.findByRole('button', { name: 'Apply to timer' }));

    const problem = await screen.findByTestId('apply-problem');
    expect(problem).toHaveTextContent('The timer was not changed.');
    expect(problem).toHaveTextContent(REFUSED);
    expect(screen.getByLabelText('Exit this far below enter, in dB')).toHaveValue(2);
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
    await fireEvent.input(screen.getByLabelText('Exit this far below enter, in dB'), {
      target: { value: '2' }
    });
    await fireEvent.click(await screen.findByRole('button', { name: 'Discard changes' }));
    await waitFor(() => expect(getTimerConfigImpl).toHaveBeenCalledTimes(2));
    expect(screen.getByLabelText('Exit this far below enter, in dB')).toHaveValue(5);
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
    const problem = await screen.findByTestId('settings-problem');
    expect(problem).toHaveTextContent("The timer's settings could not be read.");
    expect(problem).toHaveTextContent(REACH);
    expect(screen.queryByRole('list', { name: 'Seats' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Apply to timer' })).toBeNull();
    expect(document.body.textContent).not.toContain(ID);
  });

  it('reads the settings again when the timer connects, and on Try again', async () => {
    let reachable = false;
    const getTimerConfigImpl = vi.fn(async () => {
      if (!reachable)
        throw Object.assign(new Error('Could not reach Gate: timed out'), { status: 409 });
      return structuredClone(CONFIG);
    });
    const { rerender } = open({ getTimerConfigImpl }, OFF);
    expect(await screen.findByTestId('settings-problem')).toBeVisible();
    expect(getTimerConfigImpl).toHaveBeenCalledTimes(1);

    // Still not connected: a manual try, still refused.
    await fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(getTimerConfigImpl).toHaveBeenCalledTimes(2));

    // The same list again changes nothing.
    await rerender({ timers: [structuredClone(OFF)] });
    expect(getTimerConfigImpl).toHaveBeenCalledTimes(2);

    // The timer connects; the next list the Seats page hands in brings the settings by itself.
    reachable = true;
    await rerender({ timers: [GATE] });
    await waitFor(() => expect(getTimerConfigImpl).toHaveBeenCalledTimes(3));
    expect(await screen.findByRole('list', { name: 'Seats' })).toBeVisible();
    expect(screen.queryByTestId('settings-problem')).toBeNull();
    expect(screen.queryByTestId('not-connected')).toBeNull();
  });

  it('stops the live feed when it closes', async () => {
    const { feed, unmount } = open();
    await waitFor(() => seatRow('Seat 1'));
    expect(feed.opened).toEqual([ID]);
    expect(feed.stopped).toBe(0);
    unmount();
    expect(feed.stopped).toBe(1);
  });
});

describe('the lap cooldown', () => {
  const WITH_COOLDOWN: TimerConfig = { ...CONFIG, lap_cooldown_ms: 1500, exit_hold_ms: 40 };
  const cooldown = () => screen.getByLabelText('Lap cooldown, in seconds');

  it('shows it in seconds on a timer that has one, and sends it in milliseconds', async () => {
    const { putTimerConfigImpl } = open({
      getTimerConfigImpl: vi.fn(async () => structuredClone(WITH_COOLDOWN))
    });
    await waitFor(() => seatRow('Seat 1'));
    expect(cooldown()).toHaveValue(1.5);
    expect(screen.getByLabelText('Exit hold, in milliseconds')).toHaveValue(40);
    expect(screen.getByRole('button', { name: 'Apply to timer' })).toBeDisabled();

    await fireEvent.input(cooldown(), { target: { value: '0.8' } });
    expect(screen.getByText('These changes are not on the timer yet.')).toBeVisible();
    await fireEvent.click(screen.getByRole('button', { name: 'Apply to timer' }));
    await waitFor(() => expect(putTimerConfigImpl).toHaveBeenCalledTimes(1));
    expect(putTimerConfigImpl.mock.calls[0][2]).toEqual({ ...WITH_COOLDOWN, lap_cooldown_ms: 800 });
    // What came back is what the timer is using.
    await waitFor(() => expect(screen.getByText('This is what the timer is using.')).toBeVisible());
    expect(cooldown()).toHaveValue(0.8);
  });

  it('will not apply a cooldown below 0, and says so', async () => {
    const { putTimerConfigImpl } = open({
      getTimerConfigImpl: vi.fn(async () => structuredClone(WITH_COOLDOWN))
    });
    await waitFor(() => seatRow('Seat 1'));
    await fireEvent.input(cooldown(), { target: { value: '-1' } });
    expect(
      screen.getByText('The lap cooldown must be a number of seconds, 0 or more.')
    ).toBeVisible();
    expect(screen.getByRole('button', { name: 'Apply to timer' })).toBeDisabled();
    expect(putTimerConfigImpl).not.toHaveBeenCalled();
  });

  it('points at the Event page for the minimum lap time', async () => {
    open({ getTimerConfigImpl: vi.fn(async () => structuredClone(WITH_COOLDOWN)) });
    await waitFor(() => seatRow('Seat 1'));
    expect(screen.getByText(/Keep it shorter than the event's minimum lap time/)).toBeVisible();
  });
});

describe('the profile bar', () => {
  const GARAGE: SeatProfile = { id: 'prof-garage-3k9d', name: 'Garage whoops', config: CONFIG };
  const FIELD_CONFIG: TimerConfig = {
    seats: [
      { seat: '0', freq_mhz: 5732, enabled: true },
      { seat: '1', freq_mhz: 5843, enabled: true }
    ],
    threshold: { Manual: { enter: -700, exit: -750 } }
  };
  const FIELD: SeatProfile = { id: 'prof-field-8w2q', name: 'Field 5 inch', config: FIELD_CONFIG };

  /**
   * A page over a Director that keeps profiles the way the real one does: each change is
   * there on the next read.
   */
  function openWith(start: TimerProfiles, seams: SessionSeams = {}) {
    let kept = structuredClone(start);
    const listProfilesImpl = vi.fn(async () => structuredClone(kept));
    const createProfileImpl = vi.fn(
      async (_base: string, _timer: string, request: SeatProfileRequest) => {
        const made = { id: `prof-new-${kept.profiles.length}z7`, ...request };
        kept = { active: made.id, profiles: [...kept.profiles, made] };
        return made;
      }
    );
    const updateProfileImpl = vi.fn(
      async (_base: string, _timer: string, id: string, request: SeatProfileRequest) => {
        const changed = { id, ...request };
        kept = {
          ...kept,
          profiles: kept.profiles.map((profile) => (profile.id === id ? changed : profile))
        };
        return changed;
      }
    );
    const deleteProfileImpl = vi.fn(async (_base: string, _timer: string, id: string) => {
      kept = { profiles: kept.profiles.filter((profile) => profile.id !== id) };
    });
    const applyProfileImpl = vi.fn(async (_base: string, _timer: string, id: string) => {
      kept = { ...kept, active: id };
      return structuredClone(kept.profiles.find((profile) => profile.id === id)!.config);
    });
    const opened = open({
      listProfilesImpl,
      createProfileImpl,
      updateProfileImpl,
      deleteProfileImpl,
      applyProfileImpl,
      ...seams
    });
    return {
      ...opened,
      listProfilesImpl,
      createProfileImpl,
      updateProfileImpl,
      deleteProfileImpl,
      applyProfileImpl
    };
  }

  const bar = () => screen.findByTestId('profile-bar');
  const state = () => screen.getByTestId('profile-state');
  const picker = (within_: HTMLElement) =>
    within(within_).getByLabelText('Profile') as HTMLSelectElement;
  const optionTexts = (select: HTMLSelectElement) =>
    [...select.options].map((option) => option.textContent?.trim());

  it('with no profile saved it says what a profile is, and offers only Save as new', async () => {
    const { listProfilesImpl } = openWith({ profiles: [] });
    const profile = await bar();
    await waitFor(() => expect(state()).toHaveTextContent(/^A profile keeps these settings/));
    expect(listProfilesImpl).toHaveBeenCalledWith('http://d.local', ID, { token: 'tok' });
    expect(optionTexts(picker(profile))).toEqual(['No profile']);
    expect(within(profile).getByRole('button', { name: 'Save as new…' })).toBeEnabled();
    for (const name of ['Save', 'Rename', /^Delete/]) {
      expect(within(profile).queryByRole('button', { name })).toBeNull();
    }
  });

  it('lists the profiles by name; with none active it says the timer is on no saved profile', async () => {
    openWith({ profiles: [GARAGE, FIELD] });
    const profile = await bar();
    await waitFor(() => expect(state()).toHaveTextContent('The timer is not on a saved profile.'));
    expect(optionTexts(picker(profile))).toEqual(['No profile', 'Garage whoops', 'Field 5 inch']);
    expect(picker(profile)).toHaveValue('');
    expect(profile.textContent).not.toContain('prof-');
  });

  it('says the timer is set as the active profile says, with nothing to save', async () => {
    openWith({ active: GARAGE.id, profiles: [GARAGE, FIELD] });
    const profile = await bar();
    await waitFor(() =>
      expect(state()).toHaveTextContent('The timer is set as this profile says.')
    );
    expect(picker(profile)).toHaveValue(GARAGE.id);
    // An active profile is always one of the choices, so "No profile" is not offered.
    expect(optionTexts(picker(profile))).toEqual(['Garage whoops', 'Field 5 inch']);
    expect(within(profile).getByRole('button', { name: 'Save' })).toBeDisabled();
    expect(within(profile).getByRole('button', { name: 'Rename' })).toBeEnabled();
  });

  it('picking a profile writes it to the timer, and the page shows what the timer answered', async () => {
    const { applyProfileImpl, putTimerConfigImpl } = openWith({
      active: GARAGE.id,
      profiles: [GARAGE, FIELD]
    });
    const profile = await bar();
    await waitFor(() => expect(picker(profile)).toHaveValue(GARAGE.id));
    await fireEvent.change(picker(profile), { target: { value: FIELD.id } });
    await waitFor(() => expect(applyProfileImpl).toHaveBeenCalledTimes(1));
    expect(applyProfileImpl.mock.calls[0].slice(1, 3)).toEqual([ID, FIELD.id]);
    // The draft is now the profile's settings, with nothing left to apply.
    await waitFor(() => expect(screen.getByLabelText('Seat 1 frequency in MHz')).toHaveValue(5732));
    expect(screen.getByRole('radio', { name: 'Fixed levels' })).toBeChecked();
    expect(screen.getByText('This is what the timer is using.')).toBeVisible();
    await waitFor(() => expect(picker(profile)).toHaveValue(FIELD.id));
    expect(state()).toHaveTextContent('The timer is set as this profile says.');
    // The profile went through its own endpoint, not as a plain apply.
    expect(putTimerConfigImpl).not.toHaveBeenCalled();
  });

  it('says Changed once the timer has moved away from its profile, and Save writes the timer back into it', async () => {
    const { updateProfileImpl } = openWith({ active: GARAGE.id, profiles: [GARAGE] });
    const profile = await bar();
    await waitFor(() => expect(picker(profile)).toHaveValue(GARAGE.id));

    // An edit that is still a draft blocks the profile buttons that would lose it.
    await fireEvent.click(screen.getByRole('switch', { name: 'Seat 2 on' }));
    expect(picker(profile)).toBeDisabled();
    expect(picker(profile)).toHaveAttribute('title', 'Apply or discard the changes first.');
    expect(within(profile).getByRole('button', { name: 'Save' })).toBeDisabled();
    expect(within(profile).getByRole('button', { name: 'Save as new…' })).toBeDisabled();

    await fireEvent.click(screen.getByRole('button', { name: 'Apply to timer' }));
    await waitFor(() =>
      expect(state()).toHaveTextContent('The timer has been changed since this profile was saved.')
    );
    expect(state()).toHaveTextContent('Changed');
    expect(picker(profile)).toBeEnabled();

    await fireEvent.click(within(profile).getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(updateProfileImpl).toHaveBeenCalledTimes(1));
    const [, timer, id, request] = updateProfileImpl.mock.calls[0];
    expect([timer, id]).toEqual([ID, GARAGE.id]);
    expect(request).toEqual({
      name: 'Garage whoops',
      config: {
        ...CONFIG,
        seats: [CONFIG.seats[0], { ...CONFIG.seats[1], enabled: true }]
      }
    });
    await waitFor(() =>
      expect(state()).toHaveTextContent('The timer is set as this profile says.')
    );
  });

  it('Save as new asks for a name and keeps what the timer has now', async () => {
    const { createProfileImpl } = openWith({ profiles: [] });
    const profile = await bar();
    await waitFor(() => expect(picker(profile)).toBeEnabled());
    await fireEvent.click(within(profile).getByRole('button', { name: 'Save as new…' }));
    const form = await screen.findByRole('form', { name: 'Save as a new profile' });
    // The hint names the timer, by name.
    expect(form).toHaveTextContent('Keeps the settings Gate has now');
    expect(screen.getByRole('button', { name: 'Save profile' })).toBeDisabled();
    await fireEvent.input(within(form).getByLabelText('Profile name'), {
      target: { value: '  Garage whoops ' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Save profile' }));
    await waitFor(() => expect(createProfileImpl).toHaveBeenCalledTimes(1));
    expect(createProfileImpl.mock.calls[0].slice(1, 3)).toEqual([
      ID,
      { name: 'Garage whoops', config: CONFIG }
    ]);
    // The new profile is the active one, and the dialog closed.
    await waitFor(() => expect(optionTexts(picker(profile))).toEqual(['Garage whoops']));
    expect(state()).toHaveTextContent('The timer is set as this profile says.');
    expect(form.closest('dialog')).not.toHaveAttribute('open');
  });

  it("shows the Director's sentence under the name when it is taken, and stays open", async () => {
    const TAKEN = 'A profile called Garage whoops is already saved for this timer.';
    openWith(
      { profiles: [GARAGE] },
      {
        createProfileImpl: vi.fn(async () =>
          Promise.reject(Object.assign(new Error(TAKEN), { status: 409, code: 'Refused' }))
        )
      }
    );
    const profile = await bar();
    await waitFor(() => expect(picker(profile)).toBeEnabled());
    await fireEvent.click(within(profile).getByRole('button', { name: 'Save as new…' }));
    const form = await screen.findByRole('form', { name: 'Save as a new profile' });
    await fireEvent.input(within(form).getByLabelText('Profile name'), {
      target: { value: 'Garage whoops' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Save profile' }));
    expect(await within(form).findByText(TAKEN)).toBeVisible();
    expect(form.closest('dialog')).toHaveAttribute('open');
    expect(screen.queryByTestId('profile-problem')).toBeNull();
  });

  it('Rename changes the name and keeps the settings the profile has, not the timer now', async () => {
    // The timer has moved on from the profile: a rename must not save that by the back door.
    const moved: TimerConfig = {
      ...CONFIG,
      threshold: { Auto: { snr_enter: 200, hysteresis: 50 } }
    };
    const { updateProfileImpl } = openWith(
      { active: GARAGE.id, profiles: [GARAGE] },
      { getTimerConfigImpl: vi.fn(async () => structuredClone(moved)) }
    );
    const profile = await bar();
    await waitFor(() => expect(state()).toHaveTextContent('Changed'));
    await fireEvent.click(within(profile).getByRole('button', { name: 'Rename' }));
    const form = await screen.findByRole('form', { name: 'Rename the profile' });
    expect(within(form).getByLabelText('Profile name')).toHaveValue('Garage whoops');
    await fireEvent.input(within(form).getByLabelText('Profile name'), {
      target: { value: 'Garage tinywhoops' }
    });
    await fireEvent.click(
      within(form.closest('dialog') as HTMLElement).getByRole('button', { name: 'Rename' })
    );
    await waitFor(() => expect(updateProfileImpl).toHaveBeenCalledTimes(1));
    expect(updateProfileImpl.mock.calls[0].slice(1, 4)).toEqual([
      ID,
      GARAGE.id,
      { name: 'Garage tinywhoops', config: CONFIG }
    ]);
    await waitFor(() => expect(optionTexts(picker(profile))).toEqual(['Garage tinywhoops']));
    // Still changed: only the name was saved.
    expect(state()).toHaveTextContent('Changed');
  });

  it('Delete asks first, deletes the active profile and leaves the timer as it is', async () => {
    const { deleteProfileImpl, putTimerConfigImpl, applyProfileImpl } = openWith({
      active: GARAGE.id,
      profiles: [GARAGE, FIELD]
    });
    const profile = await bar();
    await waitFor(() => expect(picker(profile)).toHaveValue(GARAGE.id));
    await fireEvent.click(within(profile).getByRole('button', { name: /^Delete / }));
    expect(deleteProfileImpl).not.toHaveBeenCalled();
    await fireEvent.click(within(profile).getByRole('button', { name: 'Confirm' }));
    await waitFor(() => expect(deleteProfileImpl).toHaveBeenCalledTimes(1));
    expect(deleteProfileImpl.mock.calls[0].slice(1, 3)).toEqual([ID, GARAGE.id]);
    await waitFor(() =>
      expect(optionTexts(picker(profile))).toEqual(['No profile', 'Field 5 inch'])
    );
    expect(state()).toHaveTextContent('The timer is not on a saved profile.');
    expect(putTimerConfigImpl).not.toHaveBeenCalled();
    expect(applyProfileImpl).not.toHaveBeenCalled();
    // The seats are as they were.
    expect(screen.getByLabelText('Seat 1 frequency in MHz')).toHaveValue(5658);
  });

  it("shows the Director's sentence when a profile cannot be put on the timer, and the timer's settings stay", async () => {
    const REFUSED = 'Gate did not answer. The profile was not applied.';
    openWith(
      { active: GARAGE.id, profiles: [GARAGE, FIELD] },
      {
        applyProfileImpl: vi.fn(async () =>
          Promise.reject(Object.assign(new Error(REFUSED), { status: 409, code: 'Refused' }))
        )
      }
    );
    const profile = await bar();
    await waitFor(() => expect(picker(profile)).toHaveValue(GARAGE.id));
    await fireEvent.change(picker(profile), { target: { value: FIELD.id } });
    const problem = await screen.findByTestId('profile-problem');
    expect(problem).toHaveTextContent('The profile was not changed.');
    expect(problem).toHaveTextContent(REFUSED);
    expect(screen.getByLabelText('Seat 1 frequency in MHz')).toHaveValue(5658);
    // The picker goes back to the profile the timer is still on.
    await waitFor(() => expect(picker(profile)).toHaveValue(GARAGE.id));
    expect(document.body.textContent).not.toContain('prof-');
  });

  it('says so when the profiles cannot be read, and the seats still work', async () => {
    openWith(
      { profiles: [] },
      {
        listProfilesImpl: vi.fn(async () =>
          Promise.reject(new Error('The profiles could not be read from disk.'))
        )
      }
    );
    expect(await screen.findByTestId('profile-problem')).toHaveTextContent(
      'The profiles could not be read from disk.'
    );
    const profile = await bar();
    expect(picker(profile)).toBeDisabled();
    expect(within(profile).getByRole('button', { name: 'Save as new…' })).toBeDisabled();
    expect(seatRow('Seat 1')).toBeVisible();
  });
});
