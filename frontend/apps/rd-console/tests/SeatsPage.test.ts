/**
 * The Seats page: it reads the timer list, picks the timer whose seats are shown (the one
 * the URL names, else the first connected one), offers a box to pick between timers when
 * there is more than one, and hands the list down to `TimerSeats`. What the seats
 * themselves do is in `TimerSeats.test.ts`.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { TimerConfig, TimerProfiles, TimerView } from '@gridfpv/types';
import SeatsPage from '../src/screens/SeatsPage.svelte';
import { timerView } from './competition.js';
import { makeTestSession, type SessionSeams } from './support.js';

const GATE = timerView('gate-x1y2', 'Gate', 2);
const FAR = timerView('far-gate-9zz0', 'Far Gate', 2);
const OFF: TimerView = {
  timer: { id: 'old-gate-3c3c', name: 'Old Gate', kind: 'Oft', address: '127.0.0.1:9' },
  status: { connected: false, problem: 'Nothing answered at 127.0.0.1:9.', seats: [] }
};
const RH: TimerView = {
  timer: { id: 'rh-pi-4c1d', name: 'Club Pi', kind: 'RotorHazard', address: '10.0.0.5:5000' },
  status: { connected: true, seats: ['0'] }
};

/** Each timer's settings, told apart by the frequency of its first seat. */
const configOf = (id: string): TimerConfig => ({
  seats: [{ seat: '0', freq_mhz: id === GATE.timer.id ? 5658 : 5732, enabled: true }],
  threshold: { Auto: { snr_enter: 150, hysteresis: 50 } }
});

function open(timers: TimerView[], named?: string, seams: SessionSeams = {}) {
  const getTimerConfigImpl = vi.fn(async (_base: string, id: string) => configOf(id));
  const subscribed: string[] = [];
  let stopped = 0;
  const { session } = makeTestSession({
    noEnter: true,
    listTimersImpl: vi.fn(async () => timers),
    getTimerConfigImpl,
    listProfilesImpl: vi.fn(async (): Promise<TimerProfiles> => ({ profiles: [] })),
    subscribeTimerLiveImpl: (_base: string, id: string) => {
      subscribed.push(id);
      return () => {
        stopped += 1;
      };
    },
    ...seams
  });
  const onpick = vi.fn();
  const rendered = render(SeatsPage, { session, timer: named, onpick });
  return { session, onpick, getTimerConfigImpl, subscribed, stopped: () => stopped, ...rendered };
}

const allIds = [GATE, FAR, OFF, RH].map((view) => view.timer.id);
function expectNoTimerIds() {
  const text = document.body.textContent ?? '';
  for (const id of allIds) expect(text).not.toContain(id);
}

beforeEach(() => {
  // jsdom has no canvas. The trace draws nothing here; this only keeps jsdom quiet about it.
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
});
afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe('SeatsPage', () => {
  it('has no tab row: Timers is its own page under Setup', async () => {
    open([]);
    expect(screen.queryByRole('navigation')).toBeNull();
    expect(screen.getByRole('link', { name: 'Help for Seats' })).toHaveAttribute(
      'href',
      '#/help/seats'
    );
  });

  it('says so when no timer is added, and points at the Timers page', async () => {
    const { getTimerConfigImpl } = open([]);
    const none = await screen.findByTestId('no-timer');
    expect(none).toHaveTextContent('No timer is added yet, so there are no seats to set.');
    expect(within(none).getByRole('link', { name: /Timers page/ })).toHaveAttribute(
      'href',
      '#/timers'
    );
    expect(getTimerConfigImpl).not.toHaveBeenCalled();
  });

  it('says so when the only timers are ones whose seats are not set from here', async () => {
    const { getTimerConfigImpl } = open([RH]);
    expect(await screen.findByTestId('no-timer')).toHaveTextContent(
      'None of the timers has seats that are set from here.'
    );
    expect(getTimerConfigImpl).not.toHaveBeenCalled();
    expectNoTimerIds();
  });

  it('with one timer shows it by name with its status and address, and no picker', async () => {
    const { getTimerConfigImpl, subscribed } = open([GATE]);
    expect(await screen.findByTestId('timer-name')).toHaveTextContent('Gate');
    expect(screen.getByText('Connected')).toBeVisible();
    expect(screen.getByText('192.168.1.50:7270')).toBeVisible();
    expect(screen.queryByLabelText('Timer')).toBeNull();
    // Its seats are read and its live feed opened.
    expect(await screen.findByRole('list', { name: 'Seats' })).toBeVisible();
    expect(getTimerConfigImpl.mock.calls.map((call) => call[1])).toEqual([GATE.timer.id]);
    expect(subscribed).toEqual([GATE.timer.id]);
    expectNoTimerIds();
  });

  it('with none named, picks the first connected timer rather than the first in the list', async () => {
    const { getTimerConfigImpl } = open([OFF, RH, FAR, GATE]);
    const picker = (await screen.findByLabelText('Timer')) as HTMLSelectElement;
    expect(picker).toHaveValue(FAR.timer.id);
    // The box offers the timers whose seats Grid sets, by name, in list order.
    expect([...picker.options].map((option) => option.textContent)).toEqual([
      'Old Gate',
      'Far Gate',
      'Gate'
    ]);
    await waitFor(() =>
      expect(getTimerConfigImpl.mock.calls.map((call) => call[1])).toEqual([FAR.timer.id])
    );
    expectNoTimerIds();
  });

  it('with none connected, shows the first timer and says it is not connected', async () => {
    open([OFF]);
    expect(await screen.findByTestId('timer-name')).toHaveTextContent('Old Gate');
    expect(screen.getByText('Not connected')).toBeVisible();
    const banner = await screen.findByTestId('not-connected');
    expect(banner).toHaveTextContent('Old Gate is not connected.');
    expect(banner).toHaveTextContent('Nothing answered at 127.0.0.1:9.');
  });

  it('picking another timer asks for it by id; nothing changes until the page is given it', async () => {
    const { onpick, rerender, getTimerConfigImpl, subscribed, stopped } = open([GATE, FAR]);
    const picker = (await screen.findByLabelText('Timer')) as HTMLSelectElement;
    expect(picker).toHaveValue(GATE.timer.id);
    await waitFor(() => expect(screen.getByLabelText('Seat 1 frequency in MHz')).toHaveValue(5658));
    await fireEvent.change(picker, { target: { value: FAR.timer.id } });
    expect(onpick).toHaveBeenCalledWith(FAR.timer.id);

    // The URL now names the other timer: a fresh draft and a fresh live feed.
    await rerender({ timer: FAR.timer.id });
    await waitFor(() => expect(screen.getByLabelText('Seat 1 frequency in MHz')).toHaveValue(5732));
    expect(screen.getByLabelText('Timer')).toHaveValue(FAR.timer.id);
    expect(getTimerConfigImpl.mock.calls.map((call) => call[1])).toEqual([
      GATE.timer.id,
      FAR.timer.id
    ]);
    expect(subscribed).toEqual([GATE.timer.id, FAR.timer.id]);
    expect(stopped()).toBe(1);
  });

  it('shows the timer the URL names, even when another one is connected first', async () => {
    open([GATE, FAR], FAR.timer.id);
    expect(await screen.findByLabelText('Timer')).toHaveValue(FAR.timer.id);
    await waitFor(() => expect(screen.getByLabelText('Seat 1 frequency in MHz')).toHaveValue(5732));
  });

  it('says so when the named timer is not in the list any more, without its id', async () => {
    const { getTimerConfigImpl } = open([GATE], 'gone-gate-0a0a');
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('This timer is not in the list any more.');
    expect(within(alert).getByRole('link', { name: 'See the timers.' })).toHaveAttribute(
      'href',
      '#/timers'
    );
    // It does not quietly show another timer's seats instead.
    expect(screen.queryByRole('list', { name: 'Seats' })).toBeNull();
    expect(getTimerConfigImpl).not.toHaveBeenCalled();
    expect(document.body.textContent).not.toContain('gone-gate-0a0a');
  });

  it('says so when the timers cannot be read', async () => {
    open([], undefined, {
      listTimersImpl: vi.fn(async () => Promise.reject(new Error('The Director is not answering.')))
    });
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('The timers could not be read.');
    expect(alert).toHaveTextContent('The Director is not answering.');
    expect(screen.queryByTestId('no-timer')).toBeNull();
  });

  it('a fresh copy of the same list does not reopen the live feed or read anything again', async () => {
    vi.useFakeTimers();
    // The Director answers each read with new objects, as parsed JSON always is.
    const listTimersImpl = vi.fn(async () => [structuredClone(GATE), structuredClone(FAR)]);
    const listProfilesImpl = vi.fn(async (): Promise<TimerProfiles> => ({ profiles: [] }));
    const { getTimerConfigImpl, subscribed, stopped } = open([], undefined, {
      listTimersImpl,
      listProfilesImpl
    });
    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(listTimersImpl).toHaveBeenCalledTimes(6);
    expect(subscribed).toEqual([GATE.timer.id]);
    expect(stopped()).toBe(0);
    expect(listProfilesImpl).toHaveBeenCalledTimes(1);
    expect(getTimerConfigImpl).toHaveBeenCalledTimes(1);
  });

  it('reads the list every 2 seconds, follows a timer that connects, and stops when it closes', async () => {
    vi.useFakeTimers();
    let list: TimerView[] = [OFF];
    const listTimersImpl = vi.fn(async () => list);
    const getTimerConfigImpl = vi.fn(async (_base: string, id: string) => {
      if (!list[0].status.connected) throw new Error('Could not reach Old Gate: timed out');
      return configOf(id);
    });
    const { unmount, stopped } = open([], undefined, { listTimersImpl, getTimerConfigImpl });
    await vi.advanceTimersByTimeAsync(0);
    expect(listTimersImpl).toHaveBeenCalledTimes(1);
    expect(screen.getByText('Not connected')).toBeVisible();
    expect(screen.getByTestId('settings-problem')).toBeVisible();

    // The timer connects: the next read of the list shows it and brings its seats.
    list = [{ ...OFF, status: { connected: true, seats: ['0'] } }];
    await vi.advanceTimersByTimeAsync(2000);
    expect(listTimersImpl).toHaveBeenCalledTimes(2);
    expect(screen.getByText('Connected')).toBeVisible();
    expect(screen.getByRole('list', { name: 'Seats' })).toBeVisible();
    expect(screen.queryByTestId('settings-problem')).toBeNull();

    unmount();
    expect(stopped()).toBe(1);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(listTimersImpl).toHaveBeenCalledTimes(2);
  });
});
