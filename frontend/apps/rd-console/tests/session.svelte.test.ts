import { describe, expect, it, vi } from 'vitest';
import type { ProtocolClient, ProtocolState, StateListener } from '@gridfpv/protocol-client';
import type { CommandAck, EventMeta } from '@gridfpv/types';
import { Session } from '../src/lib/session.svelte.js';
import { liveRunning, okAck, failAck } from './fixtures.js';

const PRACTICE: EventMeta = { id: 'practice', name: 'Practice', created_at: 0, persistent: false };
const EVENT_A: EventMeta = { id: 'evt-a', name: 'Friday Series', created_at: 1, persistent: true };

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

  it('send prompts for the token lazily the first time, then reuses it', async () => {
    const { connect } = mockConnect(connecting);
    const sendCommand = vi.fn(async (): Promise<CommandAck> => okAck);
    const controlFactory = vi.fn(() => ({ baseUrl: 'http://d.local', sendCommand }));
    const tokenProvider = vi.fn(async () => 'lazy-tok');

    const session = new Session({ connectImpl: connect, controlFactory, autoRestore: false });
    session.setTokenProvider(tokenProvider);
    session.selectEvent(PRACTICE);
    expect(session.hasToken).toBe(false);

    const ack = await session.send({ Stage: { heat: 'heat-1' } });
    expect(ack.ok).toBe(true);
    expect(tokenProvider).toHaveBeenCalledOnce();
    expect(session.hasToken).toBe(true);
    expect(sendCommand).toHaveBeenCalledWith({ Stage: { heat: 'heat-1' } });

    // Second send reuses the held token — no second prompt.
    await session.send({ Arm: { heat: 'heat-1' } });
    expect(tokenProvider).toHaveBeenCalledOnce();
  });

  it('send returns Unauthorized when the RD cancels the token prompt', async () => {
    const { connect } = mockConnect(connecting);
    const sendCommand = vi.fn(async (): Promise<CommandAck> => okAck);
    const controlFactory = vi.fn(() => ({ baseUrl: 'http://d.local', sendCommand }));
    const tokenProvider = vi.fn(async () => undefined); // cancelled

    const session = new Session({ connectImpl: connect, controlFactory, autoRestore: false });
    session.setTokenProvider(tokenProvider);
    session.selectEvent(PRACTICE);

    const ack = await session.send({ Stage: { heat: 'heat-1' } });
    expect(ack.ok).toBe(false);
    expect(ack.error?.code).toBe('Unauthorized');
    expect(sendCommand).not.toHaveBeenCalled();
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
    expect(createEventImpl).toHaveBeenCalledWith(session.baseUrl, 'Friday Series', 'tok');
    expect(meta?.id).toBe('evt-a');
    expect(session.currentEvent?.id).toBe('evt-a');
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
