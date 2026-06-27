import { describe, expect, it, vi } from 'vitest';
import type { ProtocolClient, ProtocolState, StateListener } from '@gridfpv/protocol-client';
import type {
  Class,
  CommandAck,
  CreateClassRequest,
  CreatePilotRequest,
  EventMeta,
  HeatSummary,
  Pilot,
  Timer
} from '@gridfpv/types';
import { Session } from '../src/lib/session.svelte.js';
import type { ControlClient, createControlClient } from '../src/lib/control.js';
import { liveRunning, okAck, failAck } from './fixtures.js';

/**
 * Per-test overrides for the `Session` constructor's injected impls. Derived from the
 * constructor's own options so the `*Impl` fields stay in lock-step with `Session`.
 * (Vitest 4 widened `vi.fn()`'s return type, so the old `ReturnType<typeof vi.fn>`
 * field typing no longer assigned to the specific impl signatures.)
 */
type SessionOverrides = Partial<NonNullable<ConstructorParameters<typeof Session>[0]>>;

const PRACTICE: EventMeta = {
  id: 'practice',
  name: 'Practice',
  created_at: 0,
  persistent: false,
  timers: ['mock'],
  roster: [],
  classes: []
};
const EVENT_A: EventMeta = {
  id: 'evt-a',
  name: 'Friday Series',
  created_at: 1,
  persistent: true,
  timers: ['mock'],
  roster: [],
  classes: []
};

/** A mock ProtocolClient that lets a test push state into the session. */
function mockConnect(initial: ProtocolState) {
  let listener: StateListener | undefined;
  const client: ProtocolClient = {
    baseUrl: 'http://d.local',
    scope: { Event: { event: 'e' } },
    getState: () => initial,
    onState: (l) => {
      listener = l;
      l(initial);
      return () => (listener = undefined);
    },
    close: vi.fn()
  };
  const connect = vi.fn(() => client);
  const push = (s: ProtocolState) => listener?.(s);
  return { connect, client, push };
}

const connecting: ProtocolState = {
  body: undefined,
  cursor: undefined,
  status: 'connecting',
  error: undefined
};

describe('Session', () => {
  it('starts at the picker (no event) and only connects once an event is selected', () => {
    const { connect, push } = mockConnect(connecting);
    const control = { baseUrl: 'http://d.local', sendCommand: vi.fn(async () => okAck) };
    const controlFactory = vi.fn(() => control);

    const session = new Session({ connectImpl: connect, controlFactory, autoRestore: false });
    expect(session.currentEvent).toBeUndefined();
    expect(connect).not.toHaveBeenCalled();

    session.selectEvent(PRACTICE);
    expect(session.currentEvent?.id).toBe('practice');
    expect(connect).toHaveBeenCalledOnce();
    // Both seams are rooted under the selected event (#72).
    expect(controlFactory).toHaveBeenCalledWith(session.baseUrl, undefined, {
      eventId: 'practice'
    });

    // Stream pushes a LiveRaceState body → session.liveState reflects it.
    push({ body: { LiveRaceState: liveRunning }, cursor: 1, status: 'live', error: undefined });
    expect(session.connectionStatus).toBe('live');
    expect(session.liveState?.current_heat).toBe('heat-1');
  });

  it('syncServerClock anchors serverNowMs to the Director clock (offset-corrected)', async () => {
    const { connect } = mockConnect(connecting);
    const control = { baseUrl: 'http://d.local', sendCommand: vi.fn(async () => okAck) };
    const session = new Session({
      connectImpl: connect,
      controlFactory: () => control,
      autoRestore: false
    });
    // The Director's wall clock is 1000ms AHEAD of this client device.
    const serverAheadMs = 1000;
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue({
      ok: true,
      json: async () => ({ now_micros: (Date.now() + serverAheadMs) * 1000 })
    } as unknown as Response);

    // No offset before the first sync — serverNowMs() is just Date.now(). Allow a 1-tick slop: the
    // two clock reads straddle a possible millisecond rollover (was a flaky `-1 !== 0` in loaded CI).
    expect(Math.abs(session.serverNowMs() - Date.now())).toBeLessThanOrEqual(2);
    await session.syncServerClock();
    // serverNowMs() now reads ~1s ahead of raw Date.now() — the countdown + race clock use server time.
    const delta = session.serverNowMs() - Date.now();
    expect(delta).toBeGreaterThan(900);
    expect(delta).toBeLessThan(1100);
    fetchSpy.mockRestore();
  });

  it('switching events re-homes both seams to the new event', () => {
    const { connect } = mockConnect(connecting);
    const controlFactory = vi.fn(() => ({
      baseUrl: 'http://d.local',
      sendCommand: vi.fn(async () => okAck)
    }));
    const session = new Session({ connectImpl: connect, controlFactory, autoRestore: false });

    session.selectEvent(PRACTICE);
    session.selectEvent(EVENT_A);
    expect(session.currentEvent?.id).toBe('evt-a');
    // The latest connect/control target the new event.
    expect(connect).toHaveBeenLastCalledWith(expect.objectContaining({ eventId: 'evt-a' }));
    expect(controlFactory).toHaveBeenLastCalledWith(session.baseUrl, undefined, {
      eventId: 'evt-a'
    });
  });

  it('send goes through with NO prompt against an open Director (full-trust)', async () => {
    // An open (unconfigured) Director accepts the command tokenless, so the lazy prompt
    // must never fire — this is the no-prompt-ever path (#72, Slice 1b).
    const { connect } = mockConnect(connecting);
    const sendCommand = vi.fn(async (): Promise<CommandAck> => okAck);
    const controlFactory = vi.fn(() => ({ baseUrl: 'http://d.local', sendCommand }));
    const tokenProvider = vi.fn(async () => 'lazy-tok');

    const session = new Session({ connectImpl: connect, controlFactory, autoRestore: false });
    session.setTokenProvider(tokenProvider);
    session.selectEvent(PRACTICE);

    const ack = await session.send({ Stage: { heat: 'heat-1' } });
    expect(ack.ok).toBe(true);
    expect(tokenProvider).not.toHaveBeenCalled();
    expect(session.hasToken).toBe(false);
    expect(sendCommand).toHaveBeenCalledWith({ Stage: { heat: 'heat-1' } });
  });

  it('send prompts on a 401 ack, then retries with the entered token and reuses it', async () => {
    // A token-gated Director answers Unauthorized; the lazy prompt fires once, the command
    // is retried with the entered token, and the token is reused for the next send.
    const { connect } = mockConnect(connecting);
    const unauthorized: CommandAck = {
      ok: false,
      error: { code: 'Unauthorized', message: 'gated' }
    };
    let calls = 0;
    const sendCommand = vi.fn(async (): Promise<CommandAck> => {
      calls += 1;
      // Reject until a token is held (the factory is re-invoked with the token on retry).
      return calls === 1 ? unauthorized : okAck;
    });
    const controlFactory = vi.fn(() => ({ baseUrl: 'http://d.local', sendCommand }));
    const tokenProvider = vi.fn(async () => 'lazy-tok');

    const session = new Session({ connectImpl: connect, controlFactory, autoRestore: false });
    session.setTokenProvider(tokenProvider);
    session.selectEvent(PRACTICE);

    const ack = await session.send({ Stage: { heat: 'heat-1' } });
    expect(ack.ok).toBe(true);
    expect(tokenProvider).toHaveBeenCalledOnce();
    expect(session.hasToken).toBe(true);
    expect(sendCommand).toHaveBeenCalledTimes(2); // initial reject + retry

    // A subsequent send reuses the held token — no second prompt.
    await session.send({ Start: { heat: 'heat-1' } });
    expect(tokenProvider).toHaveBeenCalledOnce();
  });

  it('send surfaces the Unauthorized ack when the RD cancels the token prompt', async () => {
    const { connect } = mockConnect(connecting);
    const unauthorized: CommandAck = {
      ok: false,
      error: { code: 'Unauthorized', message: 'gated' }
    };
    const sendCommand = vi.fn(async (): Promise<CommandAck> => unauthorized);
    const controlFactory = vi.fn(() => ({ baseUrl: 'http://d.local', sendCommand }));
    const tokenProvider = vi.fn(async () => undefined); // cancelled

    const session = new Session({ connectImpl: connect, controlFactory, autoRestore: false });
    session.setTokenProvider(tokenProvider);
    session.selectEvent(PRACTICE);

    const ack = await session.send({ Stage: { heat: 'heat-1' } });
    expect(ack.ok).toBe(false);
    expect(ack.error?.code).toBe('Unauthorized');
    expect(tokenProvider).toHaveBeenCalledOnce();
    expect(session.hasToken).toBe(false);
  });

  it('send routes a Command and records control errors when a token is held', async () => {
    const { connect } = mockConnect(connecting);
    const sendCommand = vi.fn(async (): Promise<CommandAck> => failAck);
    const controlFactory = vi.fn(() => ({ baseUrl: 'http://d.local', sendCommand }));

    const session = new Session({ connectImpl: connect, controlFactory, autoRestore: false });
    session.setToken('tok');
    session.selectEvent(PRACTICE);

    const ack = await session.send({ Stage: { heat: 'heat-1' } });
    expect(sendCommand).toHaveBeenCalledWith({ Stage: { heat: 'heat-1' } });
    expect(ack.ok).toBe(false);
    expect(session.lastCommandError?.code).toBe('BadRequest');

    session.clearCommandError();
    expect(session.lastCommandError).toBeUndefined();
  });

  it('refuses to send when no event is selected', async () => {
    const session = new Session({ autoRestore: false });
    const ack = await session.send({ Stage: { heat: 'h' } });
    expect(ack.ok).toBe(false);
    expect(ack.error?.code).toBe('Unauthorized');
  });

  it('createEventAndEnter creates by name and enters the new event', async () => {
    const { connect } = mockConnect(connecting);
    const controlFactory = vi.fn(() => ({
      baseUrl: 'http://d.local',
      sendCommand: vi.fn(async () => okAck)
    }));
    const createEventImpl = vi.fn(async () => EVENT_A);

    const session = new Session({
      connectImpl: connect,
      controlFactory,
      createEventImpl,
      autoRestore: false
    });
    session.setToken('tok');

    const meta = await session.createEventAndEnter('Friday Series');
    expect(createEventImpl).toHaveBeenCalledWith(session.baseUrl, 'Friday Series', 'tok', {
      fields: undefined
    });
    expect(meta?.id).toBe('evt-a');
    expect(session.currentEvent?.id).toBe('evt-a');
  });

  it('createEventAndEnter creates tokenless against an open Director (no prompt)', async () => {
    const { connect } = mockConnect(connecting);
    const controlFactory = vi.fn(() => ({
      baseUrl: 'http://d.local',
      sendCommand: vi.fn(async () => okAck)
    }));
    const createEventImpl = vi.fn(async () => EVENT_A);
    const tokenProvider = vi.fn(async () => 'lazy-tok');

    const session = new Session({
      connectImpl: connect,
      controlFactory,
      createEventImpl,
      autoRestore: false
    });
    session.setTokenProvider(tokenProvider);

    const meta = await session.createEventAndEnter('Friday Series', {
      date: '2026-06-20',
      location: 'Main field'
    });
    // Sent with no token and the optional fields forwarded; no prompt against an open Director.
    expect(createEventImpl).toHaveBeenCalledWith(session.baseUrl, 'Friday Series', undefined, {
      fields: { date: '2026-06-20', location: 'Main field' }
    });
    expect(tokenProvider).not.toHaveBeenCalled();
    expect(meta?.id).toBe('evt-a');
  });

  it('createEventAndEnter prompts + retries when the Director gates create with a 401', async () => {
    const { connect } = mockConnect(connecting);
    const controlFactory = vi.fn(() => ({
      baseUrl: 'http://d.local',
      sendCommand: vi.fn(async () => okAck)
    }));
    let calls = 0;
    const createEventImpl = vi.fn(async () => {
      calls += 1;
      if (calls === 1) throw new Error('POST /events failed: HTTP 401');
      return EVENT_A;
    });
    const tokenProvider = vi.fn(async () => 'lazy-tok');

    const session = new Session({
      connectImpl: connect,
      controlFactory,
      createEventImpl,
      autoRestore: false
    });
    session.setTokenProvider(tokenProvider);

    const meta = await session.createEventAndEnter('Friday Series');
    expect(tokenProvider).toHaveBeenCalledOnce();
    expect(createEventImpl).toHaveBeenCalledTimes(2);
    expect(session.hasToken).toBe(true);
    expect(meta?.id).toBe('evt-a');
  });

  // ── deleteEvent: permanent delete + all data (the papercut fix) ──────────────────────

  it('deleteEvent calls DELETE with the held token and resolves true', async () => {
    const deleteEventImpl = vi.fn(async () => undefined as unknown as void);
    const session = new Session({ deleteEventImpl, autoRestore: false });
    session.setToken('tok');

    const ok = await session.deleteEvent('evt-a');
    expect(deleteEventImpl).toHaveBeenCalledWith(session.baseUrl, 'evt-a', 'tok');
    expect(ok).toBe(true);
  });

  it('deleteEvent leaves the event locally when deleting the current one', async () => {
    const { connect } = mockConnect(connecting);
    const controlFactory = vi.fn(() => ({
      baseUrl: 'http://d.local',
      sendCommand: vi.fn(async () => okAck)
    }));
    const deleteEventImpl = vi.fn(async () => undefined as unknown as void);
    const session = new Session({
      connectImpl: connect,
      controlFactory,
      deleteEventImpl,
      autoRestore: false
    });
    session.selectEvent(EVENT_A);
    expect(session.currentEvent?.id).toBe('evt-a');

    await session.deleteEvent('evt-a');
    // The just-deleted current event is torn down → back to the picker.
    expect(session.currentEvent).toBeUndefined();
  });

  it('deleteEvent prompts + retries when the Director gates delete with a 401', async () => {
    let calls = 0;
    const deleteEventImpl = vi.fn(async () => {
      calls += 1;
      if (calls === 1) throw new Error('DELETE /events/evt-a failed: HTTP 401');
      return undefined as unknown as void;
    });
    const tokenProvider = vi.fn(async () => 'lazy-tok');
    const session = new Session({ deleteEventImpl, autoRestore: false });
    session.setTokenProvider(tokenProvider);

    const ok = await session.deleteEvent('evt-a');
    expect(tokenProvider).toHaveBeenCalledOnce();
    expect(deleteEventImpl).toHaveBeenCalledTimes(2);
    expect(ok).toBe(true);
  });

  it('deleteEvent re-throws a non-auth (400/404) failure for the UI to surface', async () => {
    const deleteEventImpl = vi.fn(async () => {
      throw new Error('DELETE /events/practice failed: HTTP 400');
    });
    const session = new Session({ deleteEventImpl, autoRestore: false });
    session.setToken('tok');
    await expect(session.deleteEvent('practice')).rejects.toThrow(/400/);
  });

  // ── #90: the active event is Director state — resume across reloads ──────────────────

  it('resolveActiveEvent resumes into the Director active event on load', async () => {
    const { connect } = mockConnect(connecting);
    const controlFactory = vi.fn(() => ({
      baseUrl: 'http://d.local',
      sendCommand: vi.fn(async () => okAck)
    }));
    // The Director has an active event set → resume into it (no picker).
    const getActiveEventImpl = vi.fn(async () => ({ event: EVENT_A }));
    const session = new Session({
      connectImpl: connect,
      controlFactory,
      getActiveEventImpl,
      autoRestore: false
    });

    expect(session.resolvingActiveEvent).toBe(true);
    await session.resolveActiveEvent();
    expect(getActiveEventImpl).toHaveBeenCalledWith(session.baseUrl, { token: undefined });
    expect(session.currentEvent?.id).toBe('evt-a');
    expect(connect).toHaveBeenLastCalledWith(expect.objectContaining({ eventId: 'evt-a' }));
    expect(session.resolvingActiveEvent).toBe(false);
  });

  it('resolveActiveEvent stays at the picker when no event is active', async () => {
    const { connect } = mockConnect(connecting);
    const getActiveEventImpl = vi.fn(async () => ({ event: null }));
    const session = new Session({ connectImpl: connect, getActiveEventImpl, autoRestore: false });

    await session.resolveActiveEvent();
    expect(session.currentEvent).toBeUndefined();
    expect(connect).not.toHaveBeenCalled();
    expect(session.resolvingActiveEvent).toBe(false);
  });

  it('resolveActiveEvent falls back to the picker when the Director is unreachable', async () => {
    const getActiveEventImpl = vi.fn(async () => {
      throw new Error('fetch failed');
    });
    const session = new Session({ getActiveEventImpl, autoRestore: false });
    await session.resolveActiveEvent();
    expect(session.currentEvent).toBeUndefined();
    expect(session.resolvingActiveEvent).toBe(false);
  });

  // ── #91: the picker reads the active event id to mark the live row ───────────────────

  it('getActiveEventId returns the Director active event id', async () => {
    const getActiveEventImpl = vi.fn(async () => ({ event: EVENT_A }));
    const session = new Session({ getActiveEventImpl, autoRestore: false });
    expect(await session.getActiveEventId()).toBe('evt-a');
    expect(getActiveEventImpl).toHaveBeenCalledWith(session.baseUrl, { token: undefined });
  });

  it('getActiveEventId resolves undefined when nothing is active', async () => {
    const getActiveEventImpl = vi.fn(async () => ({ event: null }));
    const session = new Session({ getActiveEventImpl, autoRestore: false });
    expect(await session.getActiveEventId()).toBeUndefined();
  });

  it('getActiveEventId swallows a read failure (no pill, never blocks the list)', async () => {
    const getActiveEventImpl = vi.fn(async () => {
      throw new Error('fetch failed');
    });
    const session = new Session({ getActiveEventImpl, autoRestore: false });
    expect(await session.getActiveEventId()).toBeUndefined();
  });

  it('chooseEvent persists the active event server-side, then enters it', async () => {
    const { connect } = mockConnect(connecting);
    const controlFactory = vi.fn(() => ({
      baseUrl: 'http://d.local',
      sendCommand: vi.fn(async () => okAck)
    }));
    const setActiveEventImpl = vi.fn(async () => EVENT_A);
    const session = new Session({
      connectImpl: connect,
      controlFactory,
      setActiveEventImpl,
      autoRestore: false
    });

    const chosen = await session.chooseEvent(EVENT_A);
    // The choice is persisted as the Director active event (no token against an open Director)…
    expect(setActiveEventImpl).toHaveBeenCalledWith(session.baseUrl, 'evt-a', undefined);
    // …and then entered.
    expect(chosen?.id).toBe('evt-a');
    expect(session.currentEvent?.id).toBe('evt-a');
    expect(connect).toHaveBeenCalledOnce();
  });

  it('chooseEvent prompts + retries when the Director gates the active-event set with a 401', async () => {
    const { connect } = mockConnect(connecting);
    const controlFactory = vi.fn(() => ({
      baseUrl: 'http://d.local',
      sendCommand: vi.fn(async () => okAck)
    }));
    let calls = 0;
    const setActiveEventImpl = vi.fn(async () => {
      calls += 1;
      if (calls === 1) throw new Error('PUT /active-event failed: HTTP 401');
      return EVENT_A;
    });
    const tokenProvider = vi.fn(async () => 'lazy-tok');
    const session = new Session({
      connectImpl: connect,
      controlFactory,
      setActiveEventImpl,
      autoRestore: false
    });
    session.setTokenProvider(tokenProvider);

    const chosen = await session.chooseEvent(EVENT_A);
    expect(tokenProvider).toHaveBeenCalledOnce();
    expect(setActiveEventImpl).toHaveBeenCalledTimes(2);
    expect(chosen?.id).toBe('evt-a');
    expect(session.currentEvent?.id).toBe('evt-a');
  });

  it('leaveEvent (switch event) does NOT clear the Director active event', async () => {
    // Switching back to the picker is a client-side view change; the server active event stays
    // set (so a reload mid-switch resumes, and other clients are not disrupted, #90).
    const { connect } = mockConnect(connecting);
    const controlFactory = vi.fn(() => ({
      baseUrl: 'http://d.local',
      sendCommand: vi.fn(async () => okAck)
    }));
    const setActiveEventImpl = vi.fn(async () => EVENT_A);
    const session = new Session({
      connectImpl: connect,
      controlFactory,
      setActiveEventImpl,
      autoRestore: false
    });

    await session.chooseEvent(EVENT_A);
    setActiveEventImpl.mockClear();
    session.leaveEvent();
    // No call to clear/reset the server active event — switch is purely local.
    expect(setActiveEventImpl).not.toHaveBeenCalled();
    expect(session.currentEvent).toBeUndefined();
  });

  it('leaveEvent tears the read client down and returns to the picker', () => {
    const { connect, client } = mockConnect({
      body: undefined,
      cursor: undefined,
      status: 'live',
      error: undefined
    });
    const controlFactory = vi.fn(() => ({
      baseUrl: 'http://d.local',
      sendCommand: vi.fn(async () => okAck)
    }));
    const session = new Session({ connectImpl: connect, controlFactory, autoRestore: false });
    session.selectEvent(PRACTICE);

    session.leaveEvent();
    expect(client.close).toHaveBeenCalled();
    expect(session.currentEvent).toBeUndefined();
    expect(session.connectionStatus).toBe('idle');
    expect(session.liveState).toBeUndefined();
  });

  it('clearToken keeps reads working and rebuilds the control client tokenless', () => {
    const { connect } = mockConnect(connecting);
    const controlFactory = vi.fn(() => ({
      baseUrl: 'http://d.local',
      sendCommand: vi.fn(async () => okAck)
    }));
    const session = new Session({ connectImpl: connect, controlFactory, autoRestore: false });
    session.setToken('tok');
    session.selectEvent(PRACTICE);
    expect(session.hasToken).toBe(true);

    session.clearToken();
    expect(session.hasToken).toBe(false);
    // Control was rebuilt without a token; reads (the connect call) are untouched.
    expect(controlFactory).toHaveBeenLastCalledWith(session.baseUrl, undefined, {
      eventId: 'practice'
    });
  });

  // ── Timer registry (issue #73) ─────────────────────────────────────────────
  describe('timers', () => {
    const MOCK_TIMER: Timer = {
      id: 'mock',
      name: 'Mock',
      kind: { Mock: { laps: 3, lap_ms: 30000 } },
      status: 'Ready',
      channel_capability: 'Flexible',
      node_count: 8,
      available_channels: []
    };

    function timerSession(overrides?: SessionOverrides) {
      const { connect } = mockConnect(connecting);
      const controlFactory = vi.fn(() => ({
        baseUrl: 'http://d.local',
        sendCommand: vi.fn(async () => okAck)
      }));
      const session = new Session({
        connectImpl: connect,
        controlFactory,
        baseUrl: 'http://d.local',
        autoRestore: false,
        ...overrides
      });
      return session;
    }

    it('listTimers reads the registry open (no token)', async () => {
      const listTimersImpl = vi.fn(async () => [MOCK_TIMER]);
      const session = timerSession({ listTimersImpl });
      const timers = await session.listTimers();
      expect(timers).toEqual([MOCK_TIMER]);
      expect(listTimersImpl).toHaveBeenCalledWith('http://d.local', { token: undefined });
    });

    it('createTimer returns the new timer (full-trust, no token)', async () => {
      const created: Timer = {
        id: 'fast-x1',
        name: 'Fast',
        kind: { Mock: { laps: 5, lap_ms: 12000 } },
        status: 'Ready',
        channel_capability: 'Flexible',
        node_count: 8,
        available_channels: []
      };
      const createTimerImpl = vi.fn(async () => created);
      const session = timerSession({ createTimerImpl });
      const req = { name: 'Fast', kind: { Mock: { laps: 5, lap_ms: 12000 } } } as const;
      const result = await session.createTimer(req);
      expect(result).toEqual(created);
      expect(createTimerImpl).toHaveBeenCalledWith('http://d.local', req, undefined);
    });

    it('createTimer prompts then retries once on an auth (401) failure', async () => {
      const created: Timer = {
        ...MOCK_TIMER,
        id: 'rh-1',
        name: 'RH',
        kind: { Rotorhazard: { url: 'http://rh' } }
      };
      const createTimerImpl = vi
        .fn()
        .mockRejectedValueOnce(new Error('POST /timers failed: HTTP 401'))
        .mockResolvedValueOnce(created);
      const session = timerSession({ createTimerImpl });
      session.setTokenProvider(async () => 'tok');
      const result = await session.createTimer({
        name: 'RH',
        kind: { Rotorhazard: { url: 'http://rh' } }
      });
      expect(result).toEqual(created);
      expect(createTimerImpl).toHaveBeenCalledTimes(2);
      // The retry carried the freshly-entered token.
      expect(createTimerImpl).toHaveBeenLastCalledWith('http://d.local', expect.anything(), 'tok');
    });

    it('createTimer resolves undefined when the auth prompt is cancelled', async () => {
      const createTimerImpl = vi.fn().mockRejectedValue(new Error('POST /timers failed: HTTP 401'));
      const session = timerSession({ createTimerImpl });
      session.setTokenProvider(async () => undefined);
      const result = await session.createTimer({
        name: 'X',
        kind: { Mock: { laps: 1, lap_ms: 1000 } }
      });
      expect(result).toBeUndefined();
      expect(createTimerImpl).toHaveBeenCalledTimes(1);
    });

    it('deleteTimer re-throws a non-auth (400) failure for the UI to surface', async () => {
      const deleteTimerImpl = vi
        .fn()
        .mockRejectedValue(new Error('DELETE /timers/mock failed: HTTP 400'));
      const session = timerSession({ deleteTimerImpl });
      await expect(session.deleteTimer('mock')).rejects.toThrow(/400/);
    });

    it('setEventTimers is a no-op (undefined) with no event selected', async () => {
      const setEventTimersImpl = vi.fn();
      const session = timerSession({ setEventTimersImpl });
      const result = await session.setEventTimers(['mock']);
      expect(result).toBeUndefined();
      expect(setEventTimersImpl).not.toHaveBeenCalled();
    });

    // ── Live status polling for the header pills (#73, Slice 2b) ─────────────────
    const MOCK_CONNECTED: Timer = { ...MOCK_TIMER, status: 'Connected' };

    it('polls listTimers while inside an event and exposes the live list', async () => {
      vi.useFakeTimers();
      try {
        const listTimersImpl = vi.fn(async () => [MOCK_TIMER]);
        const session = timerSession({ listTimersImpl });
        session.selectEvent(PRACTICE);
        // An immediate poll fires on enter.
        await vi.advanceTimersByTimeAsync(0);
        expect(listTimersImpl).toHaveBeenCalledTimes(1);
        expect(session.timers).toEqual([MOCK_TIMER]);

        // The Director mutates status live; the next poll picks it up.
        listTimersImpl.mockResolvedValue([MOCK_CONNECTED]);
        await vi.advanceTimersByTimeAsync(2_500);
        expect(listTimersImpl).toHaveBeenCalledTimes(2);
        expect(session.timers).toEqual([MOCK_CONNECTED]);
      } finally {
        vi.clearAllTimers();
        vi.useRealTimers();
      }
    });

    it('stops polling and clears the list on leaveEvent', async () => {
      vi.useFakeTimers();
      try {
        const listTimersImpl = vi.fn(async () => [MOCK_TIMER]);
        const session = timerSession({ listTimersImpl });
        session.selectEvent(PRACTICE);
        await vi.advanceTimersByTimeAsync(0);
        expect(listTimersImpl).toHaveBeenCalledTimes(1);

        session.leaveEvent();
        expect(session.timers).toEqual([]);
        // No further polls after leaving.
        await vi.advanceTimersByTimeAsync(10_000);
        expect(listTimersImpl).toHaveBeenCalledTimes(1);
      } finally {
        vi.clearAllTimers();
        vi.useRealTimers();
      }
    });

    it('selectedTimers intersects the event selection with the polled registry, in saved order', async () => {
      vi.useFakeTimers();
      try {
        const rh: Timer = {
          id: 'rh-1',
          name: 'Track RH',
          kind: { Rotorhazard: { url: 'http://rh' } },
          status: 'Disconnected',
          channel_capability: 'Flexible',
          node_count: 8,
          available_channels: []
        };
        const listTimersImpl = vi.fn(async () => [rh, MOCK_CONNECTED]); // registry order differs
        const session = timerSession({ listTimersImpl });
        // Event selects mock then rh-1 — selectedTimers must honor THAT order.
        session.selectEvent({ ...PRACTICE, timers: ['mock', 'rh-1'] });
        await vi.advanceTimersByTimeAsync(0);

        expect(session.selectedTimers.map((t) => t.id)).toEqual(['mock', 'rh-1']);
        expect(session.selectedTimers.map((t) => t.status)).toEqual(['Connected', 'Disconnected']);
      } finally {
        vi.clearAllTimers();
        vi.useRealTimers();
      }
    });

    it('selectedTimers skips a selected id not yet in the registry, and is empty at the picker', async () => {
      vi.useFakeTimers();
      try {
        const listTimersImpl = vi.fn(async () => [MOCK_TIMER]); // only mock is known
        const session = timerSession({ listTimersImpl });
        expect(session.selectedTimers).toEqual([]); // picker: no event
        session.selectEvent({ ...PRACTICE, timers: ['mock', 'ghost'] });
        await vi.advanceTimersByTimeAsync(0);
        // The unknown id is skipped rather than rendered.
        expect(session.selectedTimers.map((t) => t.id)).toEqual(['mock']);
      } finally {
        vi.clearAllTimers();
        vi.useRealTimers();
      }
    });

    it('setEventTimers saves and re-homes currentEvent with the server response', async () => {
      const updated: EventMeta = { ...PRACTICE, timers: ['mock', 'rh-1'] };
      const setEventTimersImpl = vi.fn(async () => updated);
      const session = timerSession({ setEventTimersImpl });
      session.selectEvent(PRACTICE);
      const result = await session.setEventTimers(['mock', 'rh-1']);
      expect(result).toEqual(updated);
      expect(setEventTimersImpl).toHaveBeenCalledWith(
        'http://d.local',
        'practice',
        ['mock', 'rh-1'],
        undefined
      );
      expect(session.currentEvent?.timers).toEqual(['mock', 'rh-1']);
    });

    it('setPrimaryTimer is a no-op (undefined) with no event selected', async () => {
      const setPrimaryTimerImpl = vi.fn();
      const session = timerSession({ setPrimaryTimerImpl });
      const result = await session.setPrimaryTimer('mock');
      expect(result).toBeUndefined();
      expect(setPrimaryTimerImpl).not.toHaveBeenCalled();
    });

    it('setPrimaryTimer designates and re-homes currentEvent with the server response', async () => {
      const updated: EventMeta = { ...PRACTICE, timers: ['mock', 'rh-1'], primary_timer: 'rh-1' };
      const setPrimaryTimerImpl = vi.fn(async () => updated);
      const session = timerSession({ setPrimaryTimerImpl });
      session.selectEvent({ ...PRACTICE, timers: ['mock', 'rh-1'] });
      const result = await session.setPrimaryTimer('rh-1');
      expect(result).toEqual(updated);
      expect(setPrimaryTimerImpl).toHaveBeenCalledWith(
        'http://d.local',
        'practice',
        'rh-1',
        undefined
      );
      expect(session.currentEvent?.primary_timer).toBe('rh-1');
    });

    it('primaryTimerId applies the "first selected = primary when null" rule', async () => {
      const session = timerSession({});
      // No event → undefined.
      expect(session.primaryTimerId).toBeUndefined();
      // No explicit primary → the first selected timer.
      session.selectEvent({ ...PRACTICE, timers: ['mock', 'rh-1'] });
      expect(session.primaryTimerId).toBe('mock');
      // An explicit, in-selection primary wins.
      session.selectEvent({ ...PRACTICE, timers: ['mock', 'rh-1'], primary_timer: 'rh-1' });
      expect(session.primaryTimerId).toBe('rh-1');
      // An explicit primary NOT in the selection is ignored → first selected.
      session.selectEvent({ ...PRACTICE, timers: ['mock', 'rh-1'], primary_timer: 'gone' });
      expect(session.primaryTimerId).toBe('mock');
    });

    it('setPrimaryTimer prompts then retries once on an auth (401) failure', async () => {
      const updated: EventMeta = { ...PRACTICE, timers: ['mock', 'rh-1'], primary_timer: 'rh-1' };
      const setPrimaryTimerImpl = vi
        .fn()
        .mockRejectedValueOnce(new Error('PUT /events/practice/primary-timer failed: HTTP 401'))
        .mockResolvedValueOnce(updated);
      const session = timerSession({ setPrimaryTimerImpl });
      session.selectEvent({ ...PRACTICE, timers: ['mock', 'rh-1'] });
      session.setTokenProvider(async () => 'tok');
      const result = await session.setPrimaryTimer('rh-1');
      expect(result).toEqual(updated);
      expect(setPrimaryTimerImpl).toHaveBeenCalledTimes(2);
      expect(setPrimaryTimerImpl).toHaveBeenLastCalledWith(
        'http://d.local',
        'practice',
        'rh-1',
        'tok'
      );
    });
  });

  describe('pilots (#74)', () => {
    const ACE: Pilot = { id: 'p1', callsign: 'Ace', vtx_types: [] };

    function pilotSession(overrides?: SessionOverrides) {
      const { connect } = mockConnect(connecting);
      const controlFactory = vi.fn(() => ({
        baseUrl: 'http://d.local',
        sendCommand: vi.fn(async () => okAck)
      }));
      return new Session({
        connectImpl: connect,
        controlFactory,
        baseUrl: 'http://d.local',
        autoRestore: false,
        ...overrides
      });
    }

    it('listPilots reads the directory open (no token)', async () => {
      const listPilotsImpl = vi.fn(async () => [ACE]);
      const session = pilotSession({ listPilotsImpl });
      const pilots = await session.listPilots();
      expect(pilots).toEqual([ACE]);
      expect(listPilotsImpl).toHaveBeenCalledWith('http://d.local', { token: undefined });
    });

    it('createPilot returns the new pilot (full-trust, no token)', async () => {
      const createPilotImpl = vi.fn(async () => ACE);
      const session = pilotSession({ createPilotImpl });
      const req: CreatePilotRequest = { callsign: 'Ace', vtx_types: [] };
      const result = await session.createPilot(req);
      expect(result).toEqual(ACE);
      expect(createPilotImpl).toHaveBeenCalledWith('http://d.local', req, undefined);
    });

    it('createPilot prompts then retries once on an auth (401) failure', async () => {
      const createPilotImpl = vi
        .fn()
        .mockRejectedValueOnce(new Error('POST /pilots failed: HTTP 401'))
        .mockResolvedValueOnce(ACE);
      const session = pilotSession({ createPilotImpl });
      session.setTokenProvider(async () => 'tok');
      const result = await session.createPilot({ callsign: 'Ace', vtx_types: [] });
      expect(result).toEqual(ACE);
      expect(createPilotImpl).toHaveBeenCalledTimes(2);
      expect(createPilotImpl).toHaveBeenLastCalledWith('http://d.local', expect.anything(), 'tok');
    });

    it('createPilot resolves undefined when the auth prompt is cancelled', async () => {
      const createPilotImpl = vi.fn().mockRejectedValue(new Error('POST /pilots failed: HTTP 401'));
      const session = pilotSession({ createPilotImpl });
      session.setTokenProvider(async () => undefined);
      const result = await session.createPilot({ callsign: 'X', vtx_types: [] });
      expect(result).toBeUndefined();
      expect(createPilotImpl).toHaveBeenCalledTimes(1);
    });

    it('updatePilot passes the clear-via-null diff through verbatim', async () => {
      const updated: Pilot = { ...ACE, name: 'Alice' };
      const updatePilotImpl = vi.fn(async () => updated);
      const session = pilotSession({ updatePilotImpl });
      // A representative diff: set name, clear color/country with null, leave the rest absent.
      const req = { name: 'Alice', color: null, country: null } as const;
      const result = await session.updatePilot('p1', req);
      expect(result).toEqual(updated);
      expect(updatePilotImpl).toHaveBeenCalledWith('http://d.local', 'p1', req, undefined);
    });

    it('deletePilot resolves true on success and re-throws a non-auth (404) failure', async () => {
      const deletePilotImpl = vi.fn(async () => undefined as unknown as void);
      const session = pilotSession({ deletePilotImpl });
      await expect(session.deletePilot('p1')).resolves.toBe(true);
      expect(deletePilotImpl).toHaveBeenCalledWith('http://d.local', 'p1', undefined);

      const failing = pilotSession({
        deletePilotImpl: vi.fn().mockRejectedValue(new Error('DELETE /pilots/p1 failed: HTTP 404'))
      });
      await expect(failing.deletePilot('p1')).rejects.toThrow(/404/);
    });
  });

  describe('classes (#84)', () => {
    const OPEN: Class = { id: 'c1', name: 'Open', source: 'MultiGP' };

    function classSession(overrides?: SessionOverrides) {
      const { connect } = mockConnect(connecting);
      const controlFactory = vi.fn(() => ({
        baseUrl: 'http://d.local',
        sendCommand: vi.fn(async () => okAck)
      }));
      return new Session({
        connectImpl: connect,
        controlFactory,
        baseUrl: 'http://d.local',
        autoRestore: false,
        ...overrides
      });
    }

    it('listClasses reads the directory open (no token)', async () => {
      const listClassesImpl = vi.fn(async () => [OPEN]);
      const session = classSession({ listClassesImpl });
      const classes = await session.listClasses();
      expect(classes).toEqual([OPEN]);
      expect(listClassesImpl).toHaveBeenCalledWith('http://d.local', { token: undefined });
    });

    it('createClass returns the new class (full-trust, no token)', async () => {
      const createClassImpl = vi.fn(async () => OPEN);
      const session = classSession({ createClassImpl });
      const req: CreateClassRequest = { name: 'Open', source: 'MultiGP' };
      const result = await session.createClass(req);
      expect(result).toEqual(OPEN);
      expect(createClassImpl).toHaveBeenCalledWith('http://d.local', req, undefined);
    });

    it('createClass prompts then retries once on an auth (401) failure', async () => {
      const createClassImpl = vi
        .fn()
        .mockRejectedValueOnce(new Error('POST /classes failed: HTTP 401'))
        .mockResolvedValueOnce(OPEN);
      const session = classSession({ createClassImpl });
      session.setTokenProvider(async () => 'tok');
      const result = await session.createClass({ name: 'Open', source: 'MultiGP' });
      expect(result).toEqual(OPEN);
      expect(createClassImpl).toHaveBeenCalledTimes(2);
      expect(createClassImpl).toHaveBeenLastCalledWith('http://d.local', expect.anything(), 'tok');
    });

    it('updateClass passes the clear-via-null diff through verbatim', async () => {
      const updated: Class = { ...OPEN, name: 'Pro Open' };
      const updateClassImpl = vi.fn(async () => updated);
      const session = classSession({ updateClassImpl });
      const req = { name: 'Pro Open', reference: null, description: null } as const;
      const result = await session.updateClass('c1', req);
      expect(result).toEqual(updated);
      expect(updateClassImpl).toHaveBeenCalledWith('http://d.local', 'c1', req, undefined);
    });

    it('deleteClass resolves true on success and re-throws a non-auth (404) failure', async () => {
      const deleteClassImpl = vi.fn(async () => undefined as unknown as void);
      const session = classSession({ deleteClassImpl });
      await expect(session.deleteClass('c1')).resolves.toBe(true);
      expect(deleteClassImpl).toHaveBeenCalledWith('http://d.local', 'c1', undefined);

      const failing = classSession({
        deleteClassImpl: vi.fn().mockRejectedValue(new Error('DELETE /classes/c1 failed: HTTP 404'))
      });
      await expect(failing.deleteClass('c1')).rejects.toThrow(/404/);
    });

    it('setEventClasses is a no-op (undefined) with no event selected', async () => {
      const setEventClassesImpl = vi.fn();
      const session = classSession({ setEventClassesImpl });
      const result = await session.setEventClasses(['c1']);
      expect(result).toBeUndefined();
      expect(setEventClassesImpl).not.toHaveBeenCalled();
    });

    it('setEventClasses saves and re-homes currentEvent with the server response', async () => {
      const updated: EventMeta = { ...PRACTICE, classes: ['c1'] };
      const setEventClassesImpl = vi.fn(async () => updated);
      const session = classSession({ setEventClassesImpl });
      session.setToken('tok');
      session.selectEvent(PRACTICE);
      const result = await session.setEventClasses(['c1']);
      expect(result).toEqual(updated);
      expect(setEventClassesImpl).toHaveBeenCalledWith('http://d.local', 'practice', ['c1'], 'tok');
      expect(session.currentEvent?.classes).toEqual(['c1']);
    });
  });

  describe('heats (race redesign Slice 3b)', () => {
    function heatSession(overrides?: {
      sendCommand?: ControlClient['sendCommand'];
      listHeatsImpl?: SessionOverrides['listHeatsImpl'];
    }) {
      const { connect } = mockConnect(connecting);
      const sendCommand = overrides?.sendCommand ?? vi.fn(async () => okAck);
      const controlFactory: typeof createControlClient = () => ({
        baseUrl: 'http://d.local',
        sendCommand
      });
      const session = new Session({
        connectImpl: connect,
        controlFactory,
        baseUrl: 'http://d.local',
        autoRestore: false,
        listHeatsImpl: overrides?.listHeatsImpl
      });
      return { session, sendCommand };
    }

    it('fillRound sends a FillRound command tagged with the round (defaults to single-step Next)', async () => {
      const sendCommand = vi.fn(async () => okAck);
      const { session } = heatSession({ sendCommand });
      session.setToken('tok');
      session.selectEvent(PRACTICE);
      const ack = await session.fillRound('r1');
      expect(ack).toEqual(okAck);
      // No explicit mode → 'Next' (the single-step default, wire-compatible). #216.
      expect(sendCommand).toHaveBeenCalledWith({ FillRound: { round: 'r1', mode: 'Next' } });
    });

    it('fillRound carries the fill-all mode when asked (generate-all, #216)', async () => {
      const sendCommand = vi.fn(async () => okAck);
      const { session } = heatSession({ sendCommand });
      session.setToken('tok');
      session.selectEvent(PRACTICE);
      await session.fillRound('r1', 'All');
      expect(sendCommand).toHaveBeenCalledWith({ FillRound: { round: 'r1', mode: 'All' } });
    });

    it('scheduleHeat sends a tagged ScheduleHeat with the lineup, class, and round', async () => {
      const sendCommand = vi.fn(async () => okAck);
      const { session } = heatSession({ sendCommand });
      session.setToken('tok');
      session.selectEvent(PRACTICE);
      await session.scheduleHeat('q-1', ['p1', 'p2'], { class: 'c1', round: 'r1' });
      expect(sendCommand).toHaveBeenCalledWith({
        ScheduleHeat: { heat: 'q-1', lineup: ['p1', 'p2'], class: 'c1', round: 'r1' }
      });
    });

    it('listHeats reads the round-tagged heats open (no token), [] with no event', async () => {
      const heats: HeatSummary[] = [
        { heat: 'q-1', lineup: ['p1'], round: 'r1', phase: 'Scheduled', is_current: true }
      ];
      const listHeatsImpl = vi.fn(async () => heats);
      const { session } = heatSession({ listHeatsImpl });
      // No event selected → resolves [] without calling the impl.
      await expect(session.listHeats()).resolves.toEqual([]);
      expect(listHeatsImpl).not.toHaveBeenCalled();
      // Inside an event → reads GET /events/{id}/heats.
      session.selectEvent(PRACTICE);
      await expect(session.listHeats()).resolves.toEqual(heats);
      expect(listHeatsImpl).toHaveBeenCalledWith('http://d.local', 'practice', {
        token: undefined
      });
    });
  });
});
