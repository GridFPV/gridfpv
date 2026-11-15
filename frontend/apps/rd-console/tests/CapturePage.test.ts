/**
 * One seat's Tune page against a mocked session and a fake live feed: the seat and its timer
 * by name, the recording made from the feed, the passes the levels on screen would make, the
 * − and + buttons, and Apply writing the levels and reading back the timer's answer.
 *
 * The plot is SVG and is drawn in jsdom, but dragging its lines needs real layout; the
 * arithmetic behind a drag is in `capture.test.ts`. Here the levels move by the buttons.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { TimerLiveHandlers } from '@gridfpv/protocol-client';
import type { SeatLevels, SignalChunk, TimerConfig, TimerView } from '@gridfpv/types';
import CapturePage from '../src/screens/CapturePage.svelte';
import { timerView } from './competition.js';
import { makeTestSession, type SessionSeams } from './support.js';

const ID = 'gate-x1y2';
const GATE = timerView(ID, 'Gate', 2);
/** The same timer, saying it takes levels per seat. */
const PER_SEAT = timerView(ID, 'Gate', 2, { seat_thresholds: true });

const CONFIG: TimerConfig = {
  seats: [
    { seat: '0', freq_mhz: 5658, enabled: true },
    { seat: '1', freq_mhz: 5700, enabled: false }
  ],
  threshold: { Auto: { snr_enter: 150, hysteresis: 50 } }
};

/** What the timer reports for the seat: enter 15 dB over a floor of -92.1, exit 5 dB lower. */
const LEVELS: SeatLevels[] = [
  { seat: '0', level: -773, floor: -921, enter: -771, exit: -821, crossing: false },
  { seat: '1', enter: -771, exit: -821, crossing: false }
];

/**
 * Nine samples, a millisecond apart: quiet, a strong pass (peak -65.0), quiet, a weak pass
 * that only just clears the enter level (-76.0), quiet.
 */
const chunk = (over: Partial<SignalChunk> = {}): SignalChunk => ({
  adapter: ID,
  seat: '0',
  from: 5_000_000,
  period_micros: 1000,
  levels: [-900, -900, -700, -650, -700, -900, -900, -760, -900],
  unit: 'TenthsDbm',
  ...over
});

function open(seams: SessionSeams = {}, view: TimerView = GATE, seat = '0') {
  const feed = {
    handlers: undefined as TimerLiveHandlers | undefined,
    opened: [] as string[],
    stopped: 0
  };
  const getTimerConfigImpl = vi.fn(async () => structuredClone(CONFIG));
  const putTimerConfigImpl = vi.fn(async (_base: string, _id: string, config: TimerConfig) =>
    structuredClone(config)
  );
  const { session } = makeTestSession({
    noEnter: true,
    listTimersImpl: vi.fn(async () => [view]),
    getTimerConfigImpl,
    putTimerConfigImpl,
    subscribeTimerLiveImpl: (_base: string, id: string, handlers: TimerLiveHandlers) => {
      feed.opened.push(id);
      feed.handlers = handlers;
      return () => {
        feed.stopped += 1;
      };
    },
    ...seams
  });
  const rendered = render(CapturePage, { session, timer: ID, seat });
  return { feed, getTimerConfigImpl, putTimerConfigImpl, ...rendered };
}

/** Open the page and wait until the timer has reported the seat's levels. */
async function openSeeded(seams: SessionSeams = {}, view: TimerView = GATE) {
  const opened = open(seams, view);
  await screen.findByRole('button', { name: 'Start capture' });
  opened.feed.handlers?.onLevels?.(LEVELS);
  await screen.findByTestId('enter-level');
  return opened;
}

/** Record the nine samples and stop. */
async function capture(feed: { handlers: TimerLiveHandlers | undefined }) {
  await fireEvent.click(screen.getByRole('button', { name: 'Start capture' }));
  feed.handlers?.onSignal?.(chunk());
  await fireEvent.click(screen.getByRole('button', { name: 'Stop capture' }));
}

const enterLevel = () => screen.getByTestId('enter-level');
const exitLevel = () => screen.getByTestId('exit-level');
const press = (name: string, times = 1) =>
  Array.from({ length: times }).reduce<Promise<unknown>>(
    (before) => before.then(() => fireEvent.click(screen.getByRole('button', { name }))),
    Promise.resolve()
  );

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe('CapturePage', () => {
  it("names the seat, its timer and its channel, and never shows the timer's id", async () => {
    open();
    expect(await screen.findByRole('heading', { level: 1, name: /^Tune Seat 1/ })).toBeVisible();
    expect(await screen.findByText('R1 · 5658 MHz')).toBeVisible();
    expect(screen.getByText('Gate')).toBeVisible();
    expect(screen.getByRole('link', { name: 'Back to Seats' })).toHaveAttribute(
      'href',
      `#/seats/${ID}`
    );
    expect(screen.getByRole('link', { name: 'Help for Tune Seat 1' })).toHaveAttribute(
      'href',
      '#/help/tune'
    );
    expect(document.body.textContent).not.toContain(ID);
  });

  it('starts from the levels the timer is using for the seat, once it reports them', async () => {
    const { feed } = open();
    await screen.findByRole('button', { name: 'Start capture' });
    expect(screen.getByText("Waiting for the timer to report this seat's levels…")).toBeVisible();
    expect(screen.queryByRole('button', { name: 'Apply to timer' })).toBeNull();
    expect(feed.opened).toEqual([ID]);

    feed.handlers?.onLevels?.(LEVELS);
    await waitFor(() => expect(enterLevel()).toHaveTextContent('-77.1 dB'));
    expect(exitLevel()).toHaveTextContent('-82.1 dB');
    expect(screen.getByTestId('live-level')).toHaveTextContent('Level now -77.3 dB');
    expect(screen.getByTestId('live-level')).toHaveTextContent('Noise floor -92.1 dB');
    // Later reports move the numbers that are live, never the levels being tuned.
    feed.handlers?.onLevels?.([{ ...LEVELS[0], level: -600, enter: -500, exit: -550 }]);
    await waitFor(() =>
      expect(screen.getByTestId('live-level')).toHaveTextContent('Level now -60.0 dB')
    );
    expect(enterLevel()).toHaveTextContent('-77.1 dB');
  });

  it('records only between Start and Stop, only this seat, and shows the passes these levels make', async () => {
    const { feed } = await openSeeded();
    expect(screen.getByTestId('no-recording')).toHaveTextContent('Nothing is recorded yet.');
    // Samples that arrive before a capture is started are not kept.
    feed.handlers?.onSignal?.(chunk({ from: 1_000_000 }));

    await fireEvent.click(screen.getByRole('button', { name: 'Start capture' }));
    expect(screen.getByTestId('no-recording')).toHaveTextContent('Waiting for the first samples…');
    // Another seat's samples are not this recording's.
    feed.handlers?.onSignal?.(chunk({ seat: '1' }));
    feed.handlers?.onSignal?.(chunk());
    await fireEvent.click(screen.getByRole('button', { name: 'Stop capture' }));

    expect(await screen.findByText(/Captured · 0\.0 s · 9 samples/)).toBeVisible();
    expect(screen.getByTestId('pass-count')).toHaveTextContent('2 passes with these levels');
    const passes = within(
      screen.getByRole('list', { name: 'Passes with these levels' })
    ).getAllByRole('listitem');
    expect(passes.map((item) => item.textContent?.replace(/\s+/g, ' ').trim())).toEqual([
      'Pass 1 at 0.00 s peak -65.0 dBm 0.00 s wide',
      'Pass 2 at 0.01 s peak -76.0 dBm 0.00 s wide'
    ]);
    // Samples after Stop are not added.
    feed.handlers?.onSignal?.(chunk({ from: 6_000_000 }));
    expect(screen.getByText(/9 samples/)).toBeVisible();
    expect(screen.getByRole('button', { name: 'Capture again' })).toBeEnabled();
  });

  it('raising the enter level drops a pass that no longer reaches it', async () => {
    const { feed } = await openSeeded();
    await capture(feed);
    expect(screen.getByTestId('pass-count')).toHaveTextContent('2 passes');
    // One step is 1 dB. -76.1 is still under the weak pass's -76.0 peak; -75.1 is not.
    await press('Raise the enter level');
    expect(enterLevel()).toHaveTextContent('-76.1 dBm');
    expect(screen.getByTestId('pass-count')).toHaveTextContent('2 passes');
    await press('Raise the enter level');
    expect(enterLevel()).toHaveTextContent('-75.1 dBm');
    expect(screen.getByTestId('pass-count')).toHaveTextContent('1 pass with these levels');
    // The exit level did not move.
    expect(exitLevel()).toHaveTextContent('-82.1 dBm');
  });

  it('lowering Enter below Exit takes Exit down with it, half a dB apart', async () => {
    await openSeeded();
    // Enter starts 5 dB above exit. Four steps down leaves them 1 dB apart, untouched.
    await press('Lower the enter level', 4);
    expect(enterLevel()).toHaveTextContent('-81.1 dB');
    expect(exitLevel()).toHaveTextContent('-82.1 dB');
    // The fifth lands on the exit level, which is pushed half a dB under it.
    await press('Lower the enter level');
    expect(enterLevel()).toHaveTextContent('-82.1 dB');
    expect(exitLevel()).toHaveTextContent('-82.6 dB');
    await press('Lower the enter level');
    expect(enterLevel()).toHaveTextContent('-83.1 dB');
    expect(exitLevel()).toHaveTextContent('-83.6 dB');
  });

  it('raising Exit above Enter takes Enter up with it, half a dB apart', async () => {
    await openSeeded();
    await press('Raise the exit level', 5);
    expect(exitLevel()).toHaveTextContent('-77.1 dB');
    expect(enterLevel()).toHaveTextContent('-76.6 dB');
    // Coming back down leaves enter where it was pushed to.
    await press('Lower the exit level', 2);
    expect(exitLevel()).toHaveTextContent('-79.1 dB');
    expect(enterLevel()).toHaveTextContent('-76.6 dB');
  });

  it('stops the live feed and the reads when it closes', async () => {
    vi.useFakeTimers();
    const listTimersImpl = vi.fn(async () => [GATE]);
    const { feed, unmount } = open({ listTimersImpl });
    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(4000);
    expect(listTimersImpl).toHaveBeenCalledTimes(3);
    expect(feed.stopped).toBe(0);
    unmount();
    expect(feed.stopped).toBe(1);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(listTimersImpl).toHaveBeenCalledTimes(3);
  });
});

describe('CapturePage: Apply', () => {
  it('on a timer with one setting for all seats, writes the fixed levels to the timer and says so', async () => {
    const { putTimerConfigImpl } = await openSeeded();
    const state = screen.getByTestId('apply-state');
    expect(state).toHaveTextContent('Enter at -77.1 dB, exit at -82.1 dB.');
    expect(state).toHaveTextContent(
      'This timer has one setting for all its seats, so every seat gets these levels.'
    );
    await fireEvent.click(screen.getByRole('button', { name: 'Apply to timer' }));
    await waitFor(() => expect(putTimerConfigImpl).toHaveBeenCalledTimes(1));
    expect(putTimerConfigImpl.mock.calls[0].slice(1, 3)).toEqual([
      ID,
      { ...CONFIG, threshold: { Manual: { enter: -771, exit: -821 } } }
    ]);
    // The timer's answer is what is on it now.
    await waitFor(() => expect(state).toHaveTextContent('On the timer'));
    expect(screen.getByRole('button', { name: 'Apply to timer' })).toBeDisabled();
    // A change after that is not on the timer.
    await press('Raise the enter level');
    expect(state).not.toHaveTextContent('On the timer');
    expect(screen.getByRole('button', { name: 'Apply to timer' })).toBeEnabled();
  });

  it('on a timer that takes levels per seat, writes them to this seat alone', async () => {
    const { putTimerConfigImpl } = await openSeeded({}, PER_SEAT);
    expect(screen.getByTestId('apply-state')).not.toHaveTextContent('every seat');
    await press('Raise the enter level');
    await fireEvent.click(screen.getByRole('button', { name: 'Apply to timer' }));
    await waitFor(() => expect(putTimerConfigImpl).toHaveBeenCalledTimes(1));
    expect(putTimerConfigImpl.mock.calls[0][2]).toEqual({
      ...CONFIG,
      seats: [
        { ...CONFIG.seats[0], threshold: { Manual: { enter: -761, exit: -821 } } },
        CONFIG.seats[1]
      ]
    });
    await waitFor(() =>
      expect(screen.getByTestId('apply-state')).toHaveTextContent('On the timer')
    );
  });

  it('can write them as a distance above the noise floor instead', async () => {
    const { putTimerConfigImpl } = await openSeeded();
    await fireEvent.change(screen.getByLabelText('Write the levels as'), {
      target: { value: 'floor' }
    });
    const state = screen.getByTestId('apply-state');
    // 15 dB over the floor and 5 dB back is what this timer already has.
    expect(state).toHaveTextContent('Enter 15.0 dB above the noise floor, exit 5.0 dB below that.');
    expect(state).toHaveTextContent('On the timer');
    expect(screen.getByRole('button', { name: 'Apply to timer' })).toBeDisabled();

    await press('Raise the enter level');
    expect(state).toHaveTextContent('Enter 16.0 dB above the noise floor, exit 6.0 dB below that.');
    await fireEvent.click(screen.getByRole('button', { name: 'Apply to timer' }));
    await waitFor(() => expect(putTimerConfigImpl).toHaveBeenCalledTimes(1));
    expect(putTimerConfigImpl.mock.calls[0][2]).toEqual({
      ...CONFIG,
      threshold: { Auto: { snr_enter: 160, hysteresis: 60 } }
    });
  });

  it('will not write a distance above the floor for an enter level under the floor', async () => {
    const { putTimerConfigImpl } = await openSeeded();
    await fireEvent.change(screen.getByLabelText('Write the levels as'), {
      target: { value: 'floor' }
    });
    // The floor is -92.1; sixteen steps down puts enter at -93.1.
    await press('Lower the enter level', 16);
    expect(screen.getByTestId('apply-state')).toHaveTextContent(
      'The enter level must be above the noise floor for this.'
    );
    expect(screen.getByRole('button', { name: 'Apply to timer' })).toBeDisabled();
    expect(putTimerConfigImpl).not.toHaveBeenCalled();
  });

  it("shows the Director's sentence when the timer says no, and keeps the levels", async () => {
    const SAID = 'Gate did not take the levels: enter must be above exit.';
    await openSeeded({
      putTimerConfigImpl: vi.fn(async () =>
        Promise.reject(Object.assign(new Error(SAID), { status: 409, code: 'Refused' }))
      )
    });
    await press('Raise the enter level');
    await fireEvent.click(screen.getByRole('button', { name: 'Apply to timer' }));
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('The timer was not changed.');
    expect(alert).toHaveTextContent(SAID);
    expect(enterLevel()).toHaveTextContent('-76.1 dB');
    expect(screen.getByTestId('apply-state')).not.toHaveTextContent('On the timer');
    expect(screen.getByRole('button', { name: 'Apply to timer' })).toBeEnabled();
  });

  it('cannot be applied while a capture is running', async () => {
    await openSeeded();
    await fireEvent.click(screen.getByRole('button', { name: 'Start capture' }));
    expect(screen.getByRole('button', { name: 'Apply to timer' })).toBeDisabled();
    await fireEvent.click(screen.getByRole('button', { name: 'Stop capture' }));
    expect(screen.getByRole('button', { name: 'Apply to timer' })).toBeEnabled();
  });
});

describe('CapturePage: when the seat cannot be tuned', () => {
  it('says the seat is off, and points at the Seats page', async () => {
    open({}, GATE, '1');
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('Seat 2 is off.');
    expect(within(alert).getByRole('link', { name: 'Seats page' })).toHaveAttribute(
      'href',
      `#/seats/${ID}`
    );
    expect(screen.queryByRole('button', { name: 'Start capture' })).toBeNull();
  });

  it('says so for a seat the timer does not have, without its handle', async () => {
    open({}, GATE, 'seat-zz9');
    expect(await screen.findByRole('alert')).toHaveTextContent('This timer has no such seat.');
    expect(screen.queryByRole('button', { name: 'Start capture' })).toBeNull();
    expect(document.body.textContent).not.toContain('seat-zz9');
  });

  it('says so when the timer is not in the list any more, without its id', async () => {
    open({ listTimersImpl: vi.fn(async () => []) });
    expect(await screen.findByText('This timer is not in the list any more.')).toBeVisible();
    expect(screen.queryByRole('button', { name: 'Start capture' })).toBeNull();
    expect(document.body.textContent).not.toContain(ID);
  });

  it("says the timer is not connected in the Director's words, and nothing can be recorded", async () => {
    const SAID = 'Nothing answered at 127.0.0.1:9.';
    open(
      {},
      {
        timer: { id: ID, name: 'Gate', kind: 'Oft', address: '127.0.0.1:9' },
        status: { connected: false, problem: SAID, seats: [] }
      }
    );
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('Gate is not connected.');
    expect(alert).toHaveTextContent(SAID);
    expect(await screen.findByRole('button', { name: 'Start capture' })).toBeDisabled();
  });

  it("shows the Director's sentence when the settings cannot be read, and Try again reads them", async () => {
    let reachable = false;
    const getTimerConfigImpl = vi.fn(async () => {
      if (!reachable) throw new Error('Could not reach Gate: timed out');
      return structuredClone(CONFIG);
    });
    open({ getTimerConfigImpl });
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent("The timer's settings could not be read.");
    expect(alert).toHaveTextContent('Could not reach Gate: timed out');
    reachable = true;
    await fireEvent.click(within(alert).getByRole('button', { name: 'Try again' }));
    expect(await screen.findByRole('button', { name: 'Start capture' })).toBeVisible();
    expect(screen.queryByRole('alert')).toBeNull();
  });
});
