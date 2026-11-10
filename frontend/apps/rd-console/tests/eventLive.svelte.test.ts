/**
 * The live event store (`lib/eventLive.svelte.ts`): it reads the snapshot, reads it again on
 * each new cursor without ever running two reads at once, keeps the Director's clock, and
 * sends writes through the session.
 */
import { describe, expect, it, vi } from 'vitest';
import type { EventView, Pilot, TimerView } from '@gridfpv/types';
import { EventLive, toProtocolError, type LiveSession } from '../src/lib/eventLive.svelte.js';
import { eventView, heat, timerView } from './competition.js';

/** A promise the test settles by hand. */
function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => (resolve = r));
  return { promise, resolve };
}

const settle = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

function fakeSession(over: Partial<LiveSession> = {}): {
  session: LiveSession;
  cursor: (n: number) => void;
  stopped: () => boolean;
} {
  let onCursor: (n: number) => void = () => {};
  let stopped = false;
  const session: LiveSession = {
    getEventState: vi.fn(async () => eventView()),
    subscribeChanges: vi.fn((_id, heard) => {
      onCursor = heard;
      return () => (stopped = true);
    }),
    sendCommand: vi.fn(async () => ({ cursor: 2 })),
    createBracket: vi.fn(async () => ({ cursor: 2 })),
    postPass: vi.fn(async () => ({ cursor: 2 })),
    getRaceSignal: vi.fn(async (_id, race) => ({ race, seats: [] })),
    listPilots: vi.fn(async (): Promise<Pilot[]> => []),
    listTimers: vi.fn(async (): Promise<TimerView[]> => []),
    listClasses: vi.fn(async () => []),
    ...over
  };
  return { session, cursor: (n) => onCursor(n), stopped: () => stopped };
}

describe('EventLive', () => {
  it('reads the snapshot and the pilots on start, and names things from both', async () => {
    const view = eventView({ phases: [heat('q-1', 'Qualifying', ['p1'])] });
    const { session } = fakeSession({
      getEventState: vi.fn(async () => view),
      listPilots: vi.fn(async () => [{ id: 'p1', callsign: 'Ace', vtx_types: [] }])
    });
    const live = new EventLive(session, 'evt');
    live.start();
    await settle();
    expect(session.getEventState).toHaveBeenCalledWith('evt');
    expect(live.view).toBe(view);
    expect(live.names.phase('q-1')).toBe('Qualifying');
    expect(live.names.pilot('p1')).toBe('Ace');
  });

  it('reads again on a new cursor, and not on the cursor it already shows', async () => {
    let cursor = 1;
    const getEventState = vi.fn(async () => eventView({ cursor }));
    const feed = fakeSession({ getEventState });
    const live = new EventLive(feed.session, 'evt');
    live.start();
    await settle();
    expect(getEventState).toHaveBeenCalledTimes(1);

    feed.cursor(1);
    await settle();
    expect(getEventState).toHaveBeenCalledTimes(1);

    cursor = 2;
    feed.cursor(2);
    await settle();
    expect(getEventState).toHaveBeenCalledTimes(2);
    expect(live.view?.cursor).toBe(2);
  });

  it('never runs two reads at once: cursors that arrive mid-read cost one more read', async () => {
    const first = deferred<EventView>();
    const getEventState = vi
      .fn<LiveSession['getEventState']>()
      .mockReturnValueOnce(first.promise)
      .mockResolvedValue(eventView({ cursor: 9 }));
    const feed = fakeSession({ getEventState });
    const live = new EventLive(feed.session, 'evt');
    live.start();
    // Three cursors land while the first read is still out.
    feed.cursor(5);
    feed.cursor(6);
    feed.cursor(7);
    await settle();
    expect(getEventState).toHaveBeenCalledTimes(1);

    first.resolve(eventView({ cursor: 4 }));
    await settle();
    expect(getEventState).toHaveBeenCalledTimes(2);
    expect(live.view?.cursor).toBe(9);
  });

  it('never steps back to an older snapshot', async () => {
    const getEventState = vi
      .fn<LiveSession['getEventState']>()
      .mockResolvedValueOnce(eventView({ cursor: 8 }))
      .mockResolvedValueOnce(eventView({ cursor: 3 }));
    const live = new EventLive(fakeSession({ getEventState }).session, 'evt');
    await live.reload();
    await live.reload();
    expect(live.view?.cursor).toBe(8);
  });

  it("keeps the Director's clock: the offset comes from the middle of the quickest read", async () => {
    // Browser clock in ms; the Director is 10 s ahead.
    let browser = 1_000;
    const getEventState = vi.fn(async () => {
      browser += 20; // a 20 ms round trip
      return eventView({ now: (browser - 10 + 10_000) * 1000 });
    });
    const live = new EventLive(fakeSession({ getEventState }).session, 'evt', {
      now: () => browser
    });
    await live.reload();
    expect(live.clockOffsetMicros).toBe(10_000_000);
    expect(live.nowMicros()).toBe((browser + 10_000) * 1000);

    // A slow read is a worse measurement and does not move the offset.
    getEventState.mockImplementationOnce(async () => {
      browser += 900;
      return eventView({ cursor: 2, now: (browser + 10_000) * 1000 });
    });
    await live.reload();
    expect(live.clockOffsetMicros).toBe(10_000_000);
  });

  it('keeps the failure sentence when a read fails, and clears it on the next good one', async () => {
    const getEventState = vi
      .fn<LiveSession['getEventState']>()
      .mockRejectedValueOnce(new Error('The Director could not read the event (HTTP 500).'))
      .mockResolvedValue(eventView());
    const live = new EventLive(fakeSession({ getEventState }).session, 'evt');
    await live.reload();
    expect(live.loadError).toBe('The Director could not read the event (HTTP 500).');
    await live.reload();
    expect(live.loadError).toBeUndefined();
  });

  it('send goes through the session and then reads the snapshot', async () => {
    const feed = fakeSession({ getEventState: vi.fn(async () => eventView({ cursor: 2 })) });
    const live = new EventLive(feed.session, 'evt');
    expect(await live.send({ StartRace: { heat: 'q-1' } })).toBe(true);
    expect(feed.session.sendCommand).toHaveBeenCalledWith('evt', { StartRace: { heat: 'q-1' } });
    expect(live.view?.cursor).toBe(2);
  });

  it('send answers false when the token prompt was cancelled, and reads nothing', async () => {
    const feed = fakeSession({ sendCommand: vi.fn(async () => undefined) });
    const live = new EventLive(feed.session, 'evt');
    expect(await live.send({ StartRace: { heat: 'q-1' } })).toBe(false);
    expect(feed.session.getEventState).not.toHaveBeenCalled();
  });

  it('a refused command throws the sentence for the screen to show', async () => {
    const refusal = Object.assign(new Error('"q-1" is already complete'), {
      status: 409,
      code: 'Refused'
    });
    const feed = fakeSession({ sendCommand: vi.fn(async () => Promise.reject(refusal)) });
    const live = new EventLive(feed.session, 'evt');
    const thrown = await live.send({ StartRace: { heat: 'q-1' } }).catch((e: unknown) => e);
    expect(toProtocolError(thrown)).toEqual({
      code: 'Refused',
      message: '"q-1" is already complete'
    });
  });

  it('reads the class directory on start and names classes from it', async () => {
    const { session } = fakeSession({
      listClasses: vi.fn(async () => [{ id: 'cls-9k', name: 'Open', source: 'Custom' as const }])
    });
    const live = new EventLive(session, 'evt');
    live.start();
    await settle();
    live.stop();
    expect(live.classes.map((c) => c.name)).toEqual(['Open']);
    expect(live.names.klass('cls-9k')).toBe('Open');
    expect(live.names.klass('gone')).toBe('Unknown class');
  });

  it("raceSignal reads the race's signal for this event", async () => {
    const feed = fakeSession();
    const live = new EventLive(feed.session, 'evt');
    expect(await live.raceSignal('q-1#1')).toEqual({ race: 'q-1#1', seats: [] });
    expect(feed.session.getRaceSignal).toHaveBeenCalledWith('evt', 'q-1#1');
  });

  it('stop closes the feed and drops a read that lands afterwards', async () => {
    const pending = deferred<EventView>();
    const feed = fakeSession({ getEventState: vi.fn(() => pending.promise) });
    const live = new EventLive(feed.session, 'evt');
    live.start();
    live.stop();
    expect(feed.stopped()).toBe(true);
    pending.resolve(eventView({ cursor: 5 }));
    await settle();
    expect(live.view).toBeUndefined();
  });
});

describe('toProtocolError', () => {
  it('keeps the words and falls back to Internal when there is no code', () => {
    expect(toProtocolError(new Error('network down'))).toEqual({
      code: 'Internal',
      message: 'network down'
    });
    expect(toProtocolError('odd')).toEqual({ code: 'Internal', message: 'odd' });
  });

  it('reads the timers on start and names seats and timers from them', async () => {
    const { session } = fakeSession({
      listTimers: vi.fn(async () => [timerView('gate-x1y2', 'Gate', 2)])
    });
    const live = new EventLive(session, 'evt');
    expect(live.timersLoaded).toBe(false);
    live.start();
    await settle();
    live.stop();
    expect(live.timersLoaded).toBe(true);
    expect(live.names.timer('gate-x1y2')).toBe('Gate');
    expect(live.names.seat('gate-x1y2', '1')).toBe('Seat 2');
    expect(live.seatSources.map((source) => source.name)).toEqual(['Gate', 'Bench']);
  });

  it('asks for the timers again every 2 seconds while running, and stops when stopped', async () => {
    vi.useFakeTimers();
    try {
      const listTimers = vi.fn(async (): Promise<TimerView[]> => []);
      const live = new EventLive(fakeSession({ listTimers }).session, 'evt');
      live.start();
      expect(listTimers).toHaveBeenCalledTimes(1);
      await vi.advanceTimersByTimeAsync(2000);
      expect(listTimers).toHaveBeenCalledTimes(2);
      await vi.advanceTimersByTimeAsync(4000);
      expect(listTimers).toHaveBeenCalledTimes(4);

      live.stop();
      await vi.advanceTimersByTimeAsync(10_000);
      expect(listTimers).toHaveBeenCalledTimes(4);
    } finally {
      vi.useRealTimers();
    }
  });

  it('with pollTimers off it reads the timers once, for seat names, and never again', async () => {
    vi.useFakeTimers();
    try {
      const listTimers = vi.fn(async () => [timerView('gate-x1y2', 'Gate', 2)]);
      const live = new EventLive(fakeSession({ listTimers }).session, 'evt', {
        pollTimers: false
      });
      live.start();
      await vi.advanceTimersByTimeAsync(10_000);
      expect(listTimers).toHaveBeenCalledTimes(1);
      expect(live.names.seat('gate-x1y2', '1')).toBe('Seat 2');
      live.stop();
    } finally {
      vi.useRealTimers();
    }
  });

  it('keeps the last timer list when a read fails, and the same list when nothing changed', async () => {
    const listTimers = vi
      .fn<LiveSession['listTimers']>()
      .mockResolvedValueOnce([timerView('gate-x1y2', 'Gate', 2)])
      .mockResolvedValueOnce([timerView('gate-x1y2', 'Gate', 2)])
      .mockRejectedValueOnce(new Error('unreachable'));
    const live = new EventLive(fakeSession({ listTimers }).session, 'evt');
    await live.reloadTimers();
    const first = live.timers;
    await live.reloadTimers();
    // An equal list is not passed on, so nothing that reads it runs again.
    expect(live.timers).toBe(first);
    await live.reloadTimers();
    expect(live.names.timer('gate-x1y2')).toBe('Gate');
  });
});
