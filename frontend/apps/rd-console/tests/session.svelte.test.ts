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
import { Session, type SessionApi } from '../src/lib/session.svelte.js';
import type { ControlClient, createControlClient } from '../src/lib/control.js';
import { heatResult, liveRunning, okAck, failAck } from './fixtures.js';

/**
 * Per-test overrides for the seams a `Session` calls outward through — the session's own
 * {@link SessionApi} record, every key optional. Derived from it rather than restated, so a new
 * endpoint is steerable here the moment it exists. (Vitest 4 widened `vi.fn()`'s return type, so
 * the old `ReturnType<typeof vi.fn>` field typing no longer assigned to the specific signatures.)
 */
type SessionOverrides = Partial<SessionApi>;

/** An ordinary **created** event named "Practice" — the RD's own, not a built-in (#414). */
const PRACTICE: EventMeta = {
  id: 'practice-ab12',
  name: 'Practice',
  created_at: 0,
  persistent: true,
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

/**
 * A protocol-client rejection as it now arrives (#433): the Director's own sentence for the RD,
 * with the HTTP status carried **structurally** rather than spelled into the words. Auth detection
 * keys on that number, so these fixtures must too — a test that puts "401" in the message and
 * expects a prompt would be asserting the bug #433 removed.
 */
const refusal = (status: number, message: string): Error =>
  Object.assign(new Error(message), { status });

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

    const session = new Session({
      autoRestore: false,
      api: { connect, createControlClient: controlFactory }
    });
    expect(session.currentEvent).toBeUndefined();
    expect(connect).not.toHaveBeenCalled();

    session.selectEvent(PRACTICE);
    expect(session.currentEvent?.id).toBe('practice-ab12');
    expect(connect).toHaveBeenCalledOnce();
    // Both seams are rooted under the selected event (#72).
    expect(controlFactory).toHaveBeenCalledWith(session.baseUrl, undefined, {
      eventId: 'practice-ab12'
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
      autoRestore: false,
      api: { connect, createControlClient: () => control }
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
    const session = new Session({
      autoRestore: false,
      api: { connect, createControlClient: controlFactory }
    });

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

    const session = new Session({
      autoRestore: false,
      api: { connect, createControlClient: controlFactory }
    });
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

    const session = new Session({
      autoRestore: false,
      api: { connect, createControlClient: controlFactory }
    });
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

    const session = new Session({
      autoRestore: false,
      api: { connect, createControlClient: controlFactory }
    });
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

    const session = new Session({
      autoRestore: false,
      api: { connect, createControlClient: controlFactory }
    });
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
    const createEvent = vi.fn(async () => EVENT_A);

    const session = new Session({
      autoRestore: false,
      api: { connect, createControlClient: controlFactory, createEvent }
    });
    session.setToken('tok');

    const meta = await session.createEventAndEnter('Friday Series');
    expect(createEvent).toHaveBeenCalledWith(session.baseUrl, 'Friday Series', 'tok', {
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
    const createEvent = vi.fn(async () => EVENT_A);
    const tokenProvider = vi.fn(async () => 'lazy-tok');

    const session = new Session({
      autoRestore: false,
      api: { connect, createControlClient: controlFactory, createEvent }
    });
    session.setTokenProvider(tokenProvider);

    const meta = await session.createEventAndEnter('Friday Series', {
      date: '2026-06-20',
      location: 'Main field'
    });
    // Sent with no token and the optional fields forwarded; no prompt against an open Director.
    expect(createEvent).toHaveBeenCalledWith(session.baseUrl, 'Friday Series', undefined, {
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
    const createEvent = vi.fn(async () => {
      calls += 1;
      if (calls === 1) throw refusal(401, 'Control on this Director needs a token.');
      return EVENT_A;
    });
    const tokenProvider = vi.fn(async () => 'lazy-tok');

    const session = new Session({
      autoRestore: false,
      api: { connect, createControlClient: controlFactory, createEvent }
    });
    session.setTokenProvider(tokenProvider);

    const meta = await session.createEventAndEnter('Friday Series');
    expect(tokenProvider).toHaveBeenCalledOnce();
    expect(createEvent).toHaveBeenCalledTimes(2);
    expect(session.hasToken).toBe(true);
    expect(meta?.id).toBe('evt-a');
  });

  // ── deleteEvent: permanent delete + all data (the papercut fix) ──────────────────────

  it('deleteEvent calls DELETE with the held token and resolves true', async () => {
    const deleteEvent = vi.fn(async () => undefined as unknown as void);
    const session = new Session({ autoRestore: false, api: { deleteEvent } });
    session.setToken('tok');

    const ok = await session.deleteEvent('evt-a');
    expect(deleteEvent).toHaveBeenCalledWith(session.baseUrl, 'evt-a', 'tok');
    expect(ok).toBe(true);
  });

  it('deleteEvent leaves the event locally when deleting the current one', async () => {
    const { connect } = mockConnect(connecting);
    const controlFactory = vi.fn(() => ({
      baseUrl: 'http://d.local',
      sendCommand: vi.fn(async () => okAck)
    }));
    const deleteEvent = vi.fn(async () => undefined as unknown as void);
    const session = new Session({
      autoRestore: false,
      api: { connect, createControlClient: controlFactory, deleteEvent }
    });
    session.selectEvent(EVENT_A);
    expect(session.currentEvent?.id).toBe('evt-a');

    await session.deleteEvent('evt-a');
    // The just-deleted current event is torn down → back to the picker.
    expect(session.currentEvent).toBeUndefined();
  });

  it('deleteEvent prompts + retries when the Director gates delete with a 401', async () => {
    let calls = 0;
    const deleteEvent = vi.fn(async () => {
      calls += 1;
      if (calls === 1) throw refusal(401, 'Control on this Director needs a token.');
      return undefined as unknown as void;
    });
    const tokenProvider = vi.fn(async () => 'lazy-tok');
    const session = new Session({ autoRestore: false, api: { deleteEvent } });
    session.setTokenProvider(tokenProvider);

    const ok = await session.deleteEvent('evt-a');
    expect(tokenProvider).toHaveBeenCalledOnce();
    expect(deleteEvent).toHaveBeenCalledTimes(2);
    expect(ok).toBe(true);
  });

  it('deleteEvent re-throws a non-auth (400/404) failure for the UI to surface', async () => {
    const deleteEvent = vi.fn(async () => {
      throw refusal(400, 'Practice is in progress — finish it before deleting the event.');
    });
    const session = new Session({ autoRestore: false, api: { deleteEvent } });
    session.setToken('tok');
    // Surfaced verbatim: the Director's sentence, not a route line (#433).
    await expect(session.deleteEvent('practice-ab12')).rejects.toThrow(
      /Practice is in progress — finish it before deleting the event\./
    );
  });

  // ── #90: the active event is Director state — resume across reloads ──────────────────

  it('resolveActiveEvent resumes into the Director active event on load', async () => {
    const { connect } = mockConnect(connecting);
    const controlFactory = vi.fn(() => ({
      baseUrl: 'http://d.local',
      sendCommand: vi.fn(async () => okAck)
    }));
    // The Director has an active event set → resume into it (no picker).
    const getActiveEvent = vi.fn(async () => ({ event: EVENT_A }));
    const session = new Session({
      autoRestore: false,
      api: { connect, createControlClient: controlFactory, getActiveEvent }
    });

    expect(session.resolvingActiveEvent).toBe(true);
    await session.resolveActiveEvent();
    expect(getActiveEvent).toHaveBeenCalledWith(session.baseUrl, { token: undefined });
    expect(session.currentEvent?.id).toBe('evt-a');
    expect(connect).toHaveBeenLastCalledWith(expect.objectContaining({ eventId: 'evt-a' }));
    expect(session.resolvingActiveEvent).toBe(false);
  });

  it('resolveActiveEvent stays at the picker when no event is active', async () => {
    const { connect } = mockConnect(connecting);
    const getActiveEvent = vi.fn(async () => ({ event: null }));
    const session = new Session({ autoRestore: false, api: { connect, getActiveEvent } });

    await session.resolveActiveEvent();
    expect(session.currentEvent).toBeUndefined();
    expect(connect).not.toHaveBeenCalled();
    expect(session.resolvingActiveEvent).toBe(false);
  });

  it('resolveActiveEvent falls back to the picker when the Director is unreachable', async () => {
    const getActiveEvent = vi.fn(async () => {
      throw new Error('fetch failed');
    });
    const session = new Session({ autoRestore: false, api: { getActiveEvent } });
    await session.resolveActiveEvent();
    expect(session.currentEvent).toBeUndefined();
    expect(session.resolvingActiveEvent).toBe(false);
  });

  // ── #91: the picker reads the active event id to mark the live row ───────────────────

  it('getActiveEventId returns the Director active event id', async () => {
    const getActiveEvent = vi.fn(async () => ({ event: EVENT_A }));
    const session = new Session({ autoRestore: false, api: { getActiveEvent } });
    expect(await session.getActiveEventId()).toBe('evt-a');
    expect(getActiveEvent).toHaveBeenCalledWith(session.baseUrl, { token: undefined });
  });

  it('getActiveEventId resolves undefined when nothing is active', async () => {
    const getActiveEvent = vi.fn(async () => ({ event: null }));
    const session = new Session({ autoRestore: false, api: { getActiveEvent } });
    expect(await session.getActiveEventId()).toBeUndefined();
  });

  it('getActiveEventId swallows a read failure (no pill, never blocks the list)', async () => {
    const getActiveEvent = vi.fn(async () => {
      throw new Error('fetch failed');
    });
    const session = new Session({ autoRestore: false, api: { getActiveEvent } });
    expect(await session.getActiveEventId()).toBeUndefined();
  });

  it('chooseEvent persists the active event server-side, then enters it', async () => {
    const { connect } = mockConnect(connecting);
    const controlFactory = vi.fn(() => ({
      baseUrl: 'http://d.local',
      sendCommand: vi.fn(async () => okAck)
    }));
    const setActiveEvent = vi.fn(async () => EVENT_A);
    const session = new Session({
      autoRestore: false,
      api: { connect, createControlClient: controlFactory, setActiveEvent }
    });

    const chosen = await session.chooseEvent(EVENT_A);
    // The choice is persisted as the Director active event (no token against an open Director)…
    expect(setActiveEvent).toHaveBeenCalledWith(session.baseUrl, 'evt-a', undefined);
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
    const setActiveEvent = vi.fn(async () => {
      calls += 1;
      if (calls === 1) throw refusal(401, 'Control on this Director needs a token.');
      return EVENT_A;
    });
    const tokenProvider = vi.fn(async () => 'lazy-tok');
    const session = new Session({
      autoRestore: false,
      api: { connect, createControlClient: controlFactory, setActiveEvent }
    });
    session.setTokenProvider(tokenProvider);

    const chosen = await session.chooseEvent(EVENT_A);
    expect(tokenProvider).toHaveBeenCalledOnce();
    expect(setActiveEvent).toHaveBeenCalledTimes(2);
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
    const setActiveEvent = vi.fn(async () => EVENT_A);
    const session = new Session({
      autoRestore: false,
      api: { connect, createControlClient: controlFactory, setActiveEvent }
    });

    await session.chooseEvent(EVENT_A);
    setActiveEvent.mockClear();
    session.leaveEvent();
    // No call to clear/reset the server active event — switch is purely local.
    expect(setActiveEvent).not.toHaveBeenCalled();
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
    const session = new Session({
      autoRestore: false,
      api: { connect, createControlClient: controlFactory }
    });
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
    const session = new Session({
      autoRestore: false,
      api: { connect, createControlClient: controlFactory }
    });
    session.setToken('tok');
    session.selectEvent(PRACTICE);
    expect(session.hasToken).toBe(true);

    session.clearToken();
    expect(session.hasToken).toBe(false);
    // Control was rebuilt without a token; reads (the connect call) are untouched.
    expect(controlFactory).toHaveBeenLastCalledWith(session.baseUrl, undefined, {
      eventId: 'practice-ab12'
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
      available_channels: [],
      manual_connect: false,
      calibration: [],
      disabled_nodes: []
    };

    function timerSession(overrides?: SessionOverrides) {
      const { connect } = mockConnect(connecting);
      const controlFactory = vi.fn(() => ({
        baseUrl: 'http://d.local',
        sendCommand: vi.fn(async () => okAck)
      }));
      const session = new Session({
        baseUrl: 'http://d.local',
        autoRestore: false,
        api: { connect, createControlClient: controlFactory, ...overrides }
      });
      return session;
    }

    it('listTimers reads the registry open (no token)', async () => {
      const listTimers = vi.fn(async () => [MOCK_TIMER]);
      const session = timerSession({ listTimers });
      const timers = await session.listTimers();
      expect(timers).toEqual([MOCK_TIMER]);
      expect(listTimers).toHaveBeenCalledWith('http://d.local', { token: undefined });
    });

    it('createTimer returns the new timer (full-trust, no token)', async () => {
      const created: Timer = {
        id: 'fast-x1',
        name: 'Fast',
        kind: { Mock: { laps: 5, lap_ms: 12000 } },
        status: 'Ready',
        channel_capability: 'Flexible',
        node_count: 8,
        available_channels: [],
        manual_connect: false,
        calibration: [],
        disabled_nodes: []
      };
      const createTimer = vi.fn(async () => created);
      const session = timerSession({ createTimer });
      const req = { name: 'Fast', kind: { Mock: { laps: 5, lap_ms: 12000 } } } as const;
      const result = await session.createTimer(req);
      expect(result).toEqual(created);
      expect(createTimer).toHaveBeenCalledWith('http://d.local', req, undefined);
    });

    it('createTimer prompts then retries once on an auth (401) failure', async () => {
      const created: Timer = {
        ...MOCK_TIMER,
        id: 'rh-1',
        name: 'RH',
        kind: { Rotorhazard: { url: 'http://rh' } }
      };
      const createTimer = vi
        .fn()
        .mockRejectedValueOnce(refusal(401, 'Control on this Director needs a token.'))
        .mockResolvedValueOnce(created);
      const session = timerSession({ createTimer });
      session.setTokenProvider(async () => 'tok');
      const result = await session.createTimer({
        name: 'RH',
        kind: { Rotorhazard: { url: 'http://rh' } }
      });
      expect(result).toEqual(created);
      expect(createTimer).toHaveBeenCalledTimes(2);
      // The retry carried the freshly-entered token.
      expect(createTimer).toHaveBeenLastCalledWith('http://d.local', expect.anything(), 'tok');
    });

    it('createTimer resolves undefined when the auth prompt is cancelled', async () => {
      const createTimer = vi
        .fn()
        .mockRejectedValue(refusal(401, 'Control on this Director needs a token.'));
      const session = timerSession({ createTimer });
      session.setTokenProvider(async () => undefined);
      const result = await session.createTimer({
        name: 'X',
        kind: { Mock: { laps: 1, lap_ms: 1000 } }
      });
      expect(result).toBeUndefined();
      expect(createTimer).toHaveBeenCalledTimes(1);
    });

    it('deleteTimer re-throws a non-auth (400) failure for the UI to surface', async () => {
      const deleteTimer = vi
        .fn()
        .mockRejectedValue(refusal(400, 'The built-in Mock timer cannot be deleted.'));
      const session = timerSession({ deleteTimer });
      await expect(session.deleteTimer('mock')).rejects.toThrow(
        /The built-in Mock timer cannot be deleted\./
      );
    });

    it('setEventTimers is a no-op (undefined) with no event selected', async () => {
      const setEventTimers = vi.fn();
      const session = timerSession({ setEventTimers });
      const result = await session.setEventTimers(['mock']);
      expect(result).toBeUndefined();
      expect(setEventTimers).not.toHaveBeenCalled();
    });

    // ── Live status polling for the header pills (#73, Slice 2b) ─────────────────
    const MOCK_CONNECTED: Timer = { ...MOCK_TIMER, status: 'Connected' };

    it('polls listTimers while inside an event and exposes the live list', async () => {
      vi.useFakeTimers();
      try {
        const listTimers = vi.fn(async () => [MOCK_TIMER]);
        const session = timerSession({ listTimers });
        session.selectEvent(PRACTICE);
        // An immediate poll fires on enter.
        await vi.advanceTimersByTimeAsync(0);
        expect(listTimers).toHaveBeenCalledTimes(1);
        expect(session.timers).toEqual([MOCK_TIMER]);

        // The Director mutates status live; the next poll picks it up.
        listTimers.mockResolvedValue([MOCK_CONNECTED]);
        await vi.advanceTimersByTimeAsync(2_500);
        expect(listTimers).toHaveBeenCalledTimes(2);
        expect(session.timers).toEqual([MOCK_CONNECTED]);
      } finally {
        vi.clearAllTimers();
        vi.useRealTimers();
      }
    });

    it('stops polling and clears the list on leaveEvent', async () => {
      vi.useFakeTimers();
      try {
        const listTimers = vi.fn(async () => [MOCK_TIMER]);
        const session = timerSession({ listTimers });
        session.selectEvent(PRACTICE);
        await vi.advanceTimersByTimeAsync(0);
        expect(listTimers).toHaveBeenCalledTimes(1);

        session.leaveEvent();
        expect(session.timers).toEqual([]);
        // No further polls after leaving.
        await vi.advanceTimersByTimeAsync(10_000);
        expect(listTimers).toHaveBeenCalledTimes(1);
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
          available_channels: [],
          manual_connect: false,
          calibration: [],
          disabled_nodes: []
        };
        const listTimers = vi.fn(async () => [rh, MOCK_CONNECTED]); // registry order differs
        const session = timerSession({ listTimers });
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
        const listTimers = vi.fn(async () => [MOCK_TIMER]); // only mock is known
        const session = timerSession({ listTimers });
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
      const setEventTimers = vi.fn(async () => updated);
      const session = timerSession({ setEventTimers });
      session.selectEvent(PRACTICE);
      const result = await session.setEventTimers(['mock', 'rh-1']);
      expect(result).toEqual(updated);
      expect(setEventTimers).toHaveBeenCalledWith(
        'http://d.local',
        'practice-ab12',
        ['mock', 'rh-1'],
        undefined
      );
      expect(session.currentEvent?.timers).toEqual(['mock', 'rh-1']);
    });

    it('setPrimaryTimer is a no-op (undefined) with no event selected', async () => {
      const setPrimaryTimer = vi.fn();
      const session = timerSession({ setPrimaryTimer });
      const result = await session.setPrimaryTimer('mock');
      expect(result).toBeUndefined();
      expect(setPrimaryTimer).not.toHaveBeenCalled();
    });

    it('setPrimaryTimer designates and re-homes currentEvent with the server response', async () => {
      const updated: EventMeta = { ...PRACTICE, timers: ['mock', 'rh-1'], primary_timer: 'rh-1' };
      const setPrimaryTimer = vi.fn(async () => updated);
      const session = timerSession({ setPrimaryTimer });
      session.selectEvent({ ...PRACTICE, timers: ['mock', 'rh-1'] });
      const result = await session.setPrimaryTimer('rh-1');
      expect(result).toEqual(updated);
      expect(setPrimaryTimer).toHaveBeenCalledWith(
        'http://d.local',
        'practice-ab12',
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
      const setPrimaryTimer = vi
        .fn()
        .mockRejectedValueOnce(refusal(401, 'Control on this Director needs a token.'))
        .mockResolvedValueOnce(updated);
      const session = timerSession({ setPrimaryTimer });
      session.selectEvent({ ...PRACTICE, timers: ['mock', 'rh-1'] });
      session.setTokenProvider(async () => 'tok');
      const result = await session.setPrimaryTimer('rh-1');
      expect(result).toEqual(updated);
      expect(setPrimaryTimer).toHaveBeenCalledTimes(2);
      expect(setPrimaryTimer).toHaveBeenLastCalledWith(
        'http://d.local',
        'practice-ab12',
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
        baseUrl: 'http://d.local',
        autoRestore: false,
        api: { connect, createControlClient: controlFactory, ...overrides }
      });
    }

    it('listPilots reads the directory open (no token)', async () => {
      const listPilots = vi.fn(async () => [ACE]);
      const session = pilotSession({ listPilots });
      const pilots = await session.listPilots();
      expect(pilots).toEqual([ACE]);
      expect(listPilots).toHaveBeenCalledWith('http://d.local', { token: undefined });
    });

    it('createPilot returns the new pilot (full-trust, no token)', async () => {
      const createPilot = vi.fn(async () => ACE);
      const session = pilotSession({ createPilot });
      const req: CreatePilotRequest = { callsign: 'Ace', vtx_types: [] };
      const result = await session.createPilot(req);
      expect(result).toEqual(ACE);
      expect(createPilot).toHaveBeenCalledWith('http://d.local', req, undefined);
    });

    it('createPilot prompts then retries once on an auth (401) failure', async () => {
      const createPilot = vi
        .fn()
        .mockRejectedValueOnce(refusal(401, 'Control on this Director needs a token.'))
        .mockResolvedValueOnce(ACE);
      const session = pilotSession({ createPilot });
      session.setTokenProvider(async () => 'tok');
      const result = await session.createPilot({ callsign: 'Ace', vtx_types: [] });
      expect(result).toEqual(ACE);
      expect(createPilot).toHaveBeenCalledTimes(2);
      expect(createPilot).toHaveBeenLastCalledWith('http://d.local', expect.anything(), 'tok');
    });

    it('createPilot resolves undefined when the auth prompt is cancelled', async () => {
      const createPilot = vi
        .fn()
        .mockRejectedValue(refusal(401, 'Control on this Director needs a token.'));
      const session = pilotSession({ createPilot });
      session.setTokenProvider(async () => undefined);
      const result = await session.createPilot({ callsign: 'X', vtx_types: [] });
      expect(result).toBeUndefined();
      expect(createPilot).toHaveBeenCalledTimes(1);
    });

    it('updatePilot passes the clear-via-null diff through verbatim', async () => {
      const updated: Pilot = { ...ACE, name: 'Alice' };
      const updatePilot = vi.fn(async () => updated);
      const session = pilotSession({ updatePilot });
      // A representative diff: set name, clear color/country with null, leave the rest absent.
      const req = { name: 'Alice', color: null, country: null } as const;
      const result = await session.updatePilot('p1', req);
      expect(result).toEqual(updated);
      expect(updatePilot).toHaveBeenCalledWith('http://d.local', 'p1', req, undefined);
    });

    it('deletePilot resolves true on success and re-throws a non-auth (404) failure', async () => {
      const deletePilot = vi.fn(async () => undefined as unknown as void);
      const session = pilotSession({ deletePilot });
      await expect(session.deletePilot('p1')).resolves.toBe(true);
      expect(deletePilot).toHaveBeenCalledWith('http://d.local', 'p1', undefined);

      const failing = pilotSession({
        deletePilot: vi.fn().mockRejectedValue(refusal(404, 'That pilot no longer exists.'))
      });
      await expect(failing.deletePilot('p1')).rejects.toThrow(/That pilot no longer exists\./);
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
        baseUrl: 'http://d.local',
        autoRestore: false,
        api: { connect, createControlClient: controlFactory, ...overrides }
      });
    }

    it('listClasses reads the directory open (no token)', async () => {
      const listClasses = vi.fn(async () => [OPEN]);
      const session = classSession({ listClasses });
      const classes = await session.listClasses();
      expect(classes).toEqual([OPEN]);
      expect(listClasses).toHaveBeenCalledWith('http://d.local', { token: undefined });
    });

    it('createClass returns the new class (full-trust, no token)', async () => {
      const createClass = vi.fn(async () => OPEN);
      const session = classSession({ createClass });
      const req: CreateClassRequest = { name: 'Open', source: 'MultiGP' };
      const result = await session.createClass(req);
      expect(result).toEqual(OPEN);
      expect(createClass).toHaveBeenCalledWith('http://d.local', req, undefined);
    });

    it('createClass prompts then retries once on an auth (401) failure', async () => {
      const createClass = vi
        .fn()
        .mockRejectedValueOnce(refusal(401, 'Control on this Director needs a token.'))
        .mockResolvedValueOnce(OPEN);
      const session = classSession({ createClass });
      session.setTokenProvider(async () => 'tok');
      const result = await session.createClass({ name: 'Open', source: 'MultiGP' });
      expect(result).toEqual(OPEN);
      expect(createClass).toHaveBeenCalledTimes(2);
      expect(createClass).toHaveBeenLastCalledWith('http://d.local', expect.anything(), 'tok');
    });

    it('updateClass passes the clear-via-null diff through verbatim', async () => {
      const updated: Class = { ...OPEN, name: 'Pro Open' };
      const updateClass = vi.fn(async () => updated);
      const session = classSession({ updateClass });
      const req = { name: 'Pro Open', reference: null, description: null } as const;
      const result = await session.updateClass('c1', req);
      expect(result).toEqual(updated);
      expect(updateClass).toHaveBeenCalledWith('http://d.local', 'c1', req, undefined);
    });

    it('deleteClass resolves true on success and re-throws a non-auth (404) failure', async () => {
      const deleteClass = vi.fn(async () => undefined as unknown as void);
      const session = classSession({ deleteClass });
      await expect(session.deleteClass('c1')).resolves.toBe(true);
      expect(deleteClass).toHaveBeenCalledWith('http://d.local', 'c1', undefined);

      const failing = classSession({
        deleteClass: vi.fn().mockRejectedValue(refusal(404, 'That class no longer exists.'))
      });
      await expect(failing.deleteClass('c1')).rejects.toThrow(/That class no longer exists\./);
    });

    it('setEventClasses is a no-op (undefined) with no event selected', async () => {
      const setEventClasses = vi.fn();
      const session = classSession({ setEventClasses });
      const result = await session.setEventClasses(['c1']);
      expect(result).toBeUndefined();
      expect(setEventClasses).not.toHaveBeenCalled();
    });

    it('setEventClasses saves and re-homes currentEvent with the server response', async () => {
      const updated: EventMeta = { ...PRACTICE, classes: ['c1'] };
      const setEventClasses = vi.fn(async () => updated);
      const session = classSession({ setEventClasses });
      session.setToken('tok');
      session.selectEvent(PRACTICE);
      const result = await session.setEventClasses(['c1']);
      expect(result).toEqual(updated);
      expect(setEventClasses).toHaveBeenCalledWith(
        'http://d.local',
        'practice-ab12',
        ['c1'],
        'tok'
      );
      expect(session.currentEvent?.classes).toEqual(['c1']);
    });
  });

  describe('heats (race redesign Slice 3b)', () => {
    function heatSession(overrides?: {
      sendCommand?: ControlClient['sendCommand'];
      listHeats?: SessionOverrides['listHeats'];
    }) {
      const { connect } = mockConnect(connecting);
      const sendCommand = overrides?.sendCommand ?? vi.fn(async () => okAck);
      const controlFactory: typeof createControlClient = () => ({
        baseUrl: 'http://d.local',
        sendCommand
      });
      const session = new Session({
        baseUrl: 'http://d.local',
        autoRestore: false,
        api: { connect, createControlClient: controlFactory, listHeats: overrides?.listHeats }
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
        {
          heat: 'q-1',
          name: 'Qualifying R1 Heat 1',
          lineup: ['p1'],
          round: 'r1',
          phase: 'Scheduled',
          is_current: true
        }
      ];
      const listHeats = vi.fn(async () => heats);
      const { session } = heatSession({ listHeats });
      // No event selected → resolves [] without calling the impl.
      await expect(session.listHeats()).resolves.toEqual([]);
      expect(listHeats).not.toHaveBeenCalled();
      // Inside an event → reads GET /events/{id}/heats.
      session.selectEvent(PRACTICE);
      await expect(session.listHeats()).resolves.toEqual(heats);
      expect(listHeats).toHaveBeenCalledWith('http://d.local', 'practice-ab12', {
        token: undefined
      });
    });
  });

  // ── Auth-failure detection matches the real HTTP status, never digits inside a message ──────
  describe('auth-failure status matching', () => {
    it('does NOT prompt on a 500 whose message merely contains "403"', async () => {
      // The status is 500 — "403" appears only inside the Director's prose. The old whole-message
      // \b(401|403)\b scan matched it and opened the token dialog on a plain server error, and
      // matching a `failed: HTTP <status>` suffix would tie auth detection to wording the Director
      // now writes itself (#433). Only the structural status counts.
      const deleteEvent = vi.fn(async () => {
        throw refusal(500, 'The event could not be deleted — 403 laps were still being written.');
      });
      const tokenProvider = vi.fn(async () => 'lazy-tok');
      const session = new Session({ autoRestore: false, api: { deleteEvent } });
      session.setTokenProvider(tokenProvider);

      await expect(session.deleteEvent('evt-401')).rejects.toThrow(/403 laps/);
      expect(tokenProvider).not.toHaveBeenCalled();
      expect(deleteEvent).toHaveBeenCalledOnce();
    });

    it('prompts + retries on an error carrying the HTTP status STRUCTURALLY (status: 403)', async () => {
      let calls = 0;
      const deleteEvent = vi.fn(async () => {
        calls += 1;
        if (calls === 1) throw refusal(403, 'Control on this Director needs a token.');
        return undefined as unknown as void;
      });
      const tokenProvider = vi.fn(async () => 'lazy-tok');
      const session = new Session({ autoRestore: false, api: { deleteEvent } });
      session.setTokenProvider(tokenProvider);

      const ok = await session.deleteEvent('evt-a');
      expect(tokenProvider).toHaveBeenCalledOnce();
      expect(deleteEvent).toHaveBeenCalledTimes(2);
      expect(ok).toBe(true);
    });
  });

  // ── heatResult lifecycle: a scored result must never outlive the heat it describes ──────────
  describe('heatResult staleness (clear on heat change / Revert)', () => {
    /** Serve `?projection=result` with the fixture result; everything else fails (inert). */
    function stubResultFetch() {
      return vi.spyOn(globalThis, 'fetch').mockImplementation(async (input) => {
        const url = String(input);
        if (/\/snapshot\/heat\/[^/?]+\?projection=result$/.test(url)) {
          return {
            ok: true,
            json: async () => ({ body: { HeatResult: heatResult } })
          } as unknown as Response;
        }
        return { ok: false, json: async () => ({}) } as unknown as Response;
      });
    }

    function resultSession() {
      const { connect, push } = mockConnect(connecting);
      const control = { baseUrl: 'http://d.local', sendCommand: vi.fn(async () => okAck) };
      const session = new Session({
        autoRestore: false,
        api: { connect, createControlClient: () => control }
      });
      session.selectEvent(PRACTICE);
      return { session, push };
    }

    it('clears heatResult once the live current heat moves on (stale-export fix)', async () => {
      const { session, push } = resultSession();
      const fetchSpy = stubResultFetch();
      await session.fetchHeatResult('heat-1');
      expect(session.heatResult).toBeDefined();

      // The same heat still current → the result stands.
      push({
        body: { LiveRaceState: { ...liveRunning, current_heat: 'heat-1' } },
        cursor: 2,
        status: 'live',
        error: undefined
      });
      expect(session.heatResult).toBeDefined();

      // The current heat moves to the NEXT heat → the stored result no longer describes it;
      // leaving it set embedded the previous heat's result in the Results JSON export.
      push({
        body: { LiveRaceState: { ...liveRunning, current_heat: 'heat-2' } },
        cursor: 3,
        status: 'live',
        error: undefined
      });
      expect(session.heatResult).toBeUndefined();
      fetchSpy.mockRestore();
    });

    it('clears heatResult when ITS heat is Reverted — not on a Revert of another heat', async () => {
      const { session } = resultSession();
      const fetchSpy = stubResultFetch();
      await session.fetchHeatResult('heat-1');
      expect(session.heatResult).toBeDefined();

      // Reverting a DIFFERENT heat leaves this result standing…
      await session.send({ Revert: { heat: 'heat-9' } });
      expect(session.heatResult).toBeDefined();
      // …but Reverting the result's own heat re-opens it: no scored result stands.
      await session.send({ Revert: { heat: 'heat-1' } });
      expect(session.heatResult).toBeUndefined();
      fetchSpy.mockRestore();
    });
  });

  // ── Durable per-heat bindings (the Results/Audit friendly-name source) ───────────────────────
  describe('ensureHeatBindings (durable per-heat registration bindings)', () => {
    it('fetches each heat-window fold once, caches the ref→pilot map, and retries failures', async () => {
      const { connect } = mockConnect(connecting);
      const control = { baseUrl: 'http://d.local', sendCommand: vi.fn(async () => okAck) };
      const session = new Session({
        baseUrl: 'http://d.local',
        autoRestore: false,
        api: { connect, createControlClient: () => control }
      });
      session.selectEvent(PRACTICE);

      // h1's heat-window fold carries the durable `node-0 → p1` bind; h2's read fails.
      const fetchSpy = vi.spyOn(globalThis, 'fetch').mockImplementation(async (input) => {
        const url = String(input);
        const m = /\/snapshot\/heat\/([^/?]+)\?projection=live$/.exec(url);
        if (m && m[1] === 'h1') {
          const live = {
            current_heat: 'h1',
            phase: 'Unofficial',
            progress: [{ competitor: 'node-0', pilot: 'p1', laps_completed: 2 }]
          };
          return {
            ok: true,
            json: async () => ({ body: { LiveRaceState: live } })
          } as unknown as Response;
        }
        return { ok: false, json: async () => ({}) } as unknown as Response;
      });
      const liveFetchUrls = () =>
        fetchSpy.mock.calls.map(([u]) => String(u)).filter((u) => u.includes('projection=live'));

      await session.ensureHeatBindings(['h1', 'h2']);
      expect(session.heatBindings.get('h1')?.get('node-0')).toBe('p1');
      // h2's read failed → stays uncached (the resolver just falls back for its refs).
      expect(session.heatBindings.has('h2')).toBe(false);

      // A second ensure re-fetches ONLY the still-missing heat — h1 is served from the cache.
      const before = liveFetchUrls().length;
      await session.ensureHeatBindings(['h1', 'h2']);
      const fresh = liveFetchUrls().slice(before);
      expect(fresh.some((u) => u.includes('/h1?'))).toBe(false);
      expect(fresh.some((u) => u.includes('/h2?'))).toBe(true);

      // The cache is event-scoped: leaving the event drops it.
      session.leaveEvent();
      expect(session.heatBindings.size).toBe(0);
      fetchSpy.mockRestore();
    });
  });
});
