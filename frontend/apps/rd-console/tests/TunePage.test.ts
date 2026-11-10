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
