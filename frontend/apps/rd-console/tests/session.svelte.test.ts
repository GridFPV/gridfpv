import { describe, expect, it, vi } from 'vitest';
import type { ProtocolClient, ProtocolState, StateListener } from '@gridfpv/protocol-client';
import type { CommandAck } from '@gridfpv/types';
import { Session } from '../src/lib/session.svelte.js';
import { liveRunning, okAck, failAck } from './fixtures.js';

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

describe('Session', () => {
  it('login holds auth, opens both seams, and surfaces live state from the stream', () => {
    const { connect, push } = mockConnect({
      body: undefined,
      cursor: undefined,
      status: 'connecting',
      error: undefined
    });
    const control = { baseUrl: 'http://d.local', sendCommand: vi.fn(async () => okAck) };
    const controlFactory = vi.fn(() => control);

    const session = new Session({ connectImpl: connect, controlFactory, autoRestore: false });
    session.login('http://d.local', 'tok');

    expect(session.authenticated).toBe(true);
    expect(connect).toHaveBeenCalledOnce();
    expect(controlFactory).toHaveBeenCalledWith('http://d.local', 'tok');

    // Stream pushes a LiveRaceState body → session.liveState reflects it.
    push({ body: { LiveRaceState: liveRunning }, cursor: 1n, status: 'live', error: undefined });
    expect(session.connectionStatus).toBe('live');
    expect(session.liveState?.current_heat).toBe('heat-1');
  });

  it('send routes a Command through the control client and records errors', async () => {
    const { connect } = mockConnect({
      body: undefined,
      cursor: undefined,
      status: 'live',
      error: undefined
    });
    const sendCommand = vi.fn(async (): Promise<CommandAck> => failAck);
    const controlFactory = vi.fn(() => ({ baseUrl: 'http://d.local', sendCommand }));

    const session = new Session({ connectImpl: connect, controlFactory, autoRestore: false });
    session.login('http://d.local', 'tok');

    const ack = await session.send({ Stage: { heat: 'heat-1' } });
    expect(sendCommand).toHaveBeenCalledWith({ Stage: { heat: 'heat-1' } });
    expect(ack.ok).toBe(false);
    expect(session.lastCommandError?.code).toBe('BadRequest');

    session.clearCommandError();
    expect(session.lastCommandError).toBeUndefined();
  });

  it('refuses to send when not signed in', async () => {
    const session = new Session({ autoRestore: false });
    const ack = await session.send({ Stage: { heat: 'h' } });
    expect(ack.ok).toBe(false);
    expect(ack.error?.code).toBe('Unauthorized');
  });

  it('logout tears down the read client and clears state', () => {
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
    session.login('http://d.local', 'tok');

    session.logout();
    expect(client.close).toHaveBeenCalled();
    expect(session.authenticated).toBe(false);
    expect(session.liveState).toBeUndefined();
  });
});
