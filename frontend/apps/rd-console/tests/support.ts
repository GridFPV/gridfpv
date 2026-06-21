/**
 * Test support: build a `Session` whose seams are mocked so screens render and fire
 * commands without a real server. The returned `sendSpy` records every `Command` a
 * screen emits; `pushLive` injects a `LiveRaceState` onto the read stream.
 */
import { vi } from 'vitest';
import type {
  ProtocolClient,
  ProtocolState,
  StateListener,
  listTimers,
  createTimer,
  updateTimer,
  deleteTimer,
  setEventTimers,
  setPrimaryTimer,
  listPilots
} from '@gridfpv/protocol-client';
import type { Command, CommandAck, EventMeta, LiveRaceState } from '@gridfpv/types';
import { Session } from '../src/lib/session.svelte.js';

/** The built-in Practice event the screen tests render inside. */
const PRACTICE: EventMeta = {
  id: 'practice',
  name: 'Practice',
  created_at: 0,
  persistent: false,
  timers: ['mock'],
  roster: []
};

/** The registry seams a screen test can override (all optional; defaults are inert). */
export interface TimerImpls {
  listTimersImpl?: typeof listTimers;
  createTimerImpl?: typeof createTimer;
  updateTimerImpl?: typeof updateTimer;
  deleteTimerImpl?: typeof deleteTimer;
  setEventTimersImpl?: typeof setEventTimers;
  setPrimaryTimerImpl?: typeof setPrimaryTimer;
  /** The app-level pilot directory read (issue #74) — backs the hub + Pilots page tests. */
  listPilotsImpl?: typeof listPilots;
}

export interface TestSession {
  session: Session;
  sendSpy: ReturnType<typeof vi.fn<(c: Command) => Promise<CommandAck>>>;
  pushLive: (state: LiveRaceState) => void;
}

export function makeTestSession(
  opts?: {
    ack?: CommandAck;
    live?: LiveRaceState;
    event?: EventMeta;
    /** Skip entering an event — for the app-level hub/page tests that render with no event. */
    noEnter?: boolean;
  } & TimerImpls
): TestSession {
  const ack: CommandAck = opts?.ack ?? { ok: true };
  const sendSpy = vi.fn<(c: Command) => Promise<CommandAck>>(async () => ack);

  let listener: StateListener | undefined;
  const initial: ProtocolState = {
    body: opts?.live ? { LiveRaceState: opts.live } : undefined,
    cursor: undefined,
    status: 'live',
    error: undefined
  };
  const client: ProtocolClient = {
    baseUrl: 'http://d.local',
    scope: { Event: { event: 'e' } },
    getState: () => initial,
    onState: (l) => {
      listener = l;
      l(initial);
      return () => (listener = undefined);
    },
    close: () => {}
  };

  const session = new Session({
    connectImpl: () => client,
    controlFactory: () => ({ baseUrl: 'http://d.local', sendCommand: sendSpy }),
    baseUrl: 'http://d.local',
    autoRestore: false,
    // Timer-registry seams (issue #73): inert unless a test overrides them.
    listTimersImpl: opts?.listTimersImpl,
    createTimerImpl: opts?.createTimerImpl,
    updateTimerImpl: opts?.updateTimerImpl,
    deleteTimerImpl: opts?.deleteTimerImpl,
    setEventTimersImpl: opts?.setEventTimersImpl,
    setPrimaryTimerImpl: opts?.setPrimaryTimerImpl,
    listPilotsImpl: opts?.listPilotsImpl
  });
  // Seed a token (so privileged sends don't trigger the lazy prompt) and enter the event,
  // unless the test wants the app-level (no-event) context.
  session.setToken('tok');
  if (!opts?.noEnter) session.selectEvent(opts?.event ?? PRACTICE);

  const pushLive = (state: LiveRaceState) =>
    listener?.({ body: { LiveRaceState: state }, cursor: 1, status: 'live', error: undefined });

  return { session, sendSpy, pushLive };
}
