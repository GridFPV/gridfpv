import { describe, expect, it, vi } from 'vitest';
import type { ProtocolClient, ProtocolState, StateListener } from '@gridfpv/protocol-client';
import type { CommandAck, EventMeta } from '@gridfpv/types';
import { Session } from '../src/lib/session.svelte.js';
import { liveRunning, okAck, failAck } from './fixtures.js';

const PRACTICE: EventMeta = {
  id: 'practice',
  name: 'Practice',
  created_at: 0,
  persistent: false,
  timers: ['mock']
};
const EVENT_A: EventMeta = {
  id: 'evt-a',
  name: 'Friday Series',
  created_at: 1,
  persistent: true,
  timers: ['mock']
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
    await session.send({ Arm: { heat: 'heat-1' } });
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
});
