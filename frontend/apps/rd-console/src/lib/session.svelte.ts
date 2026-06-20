/**
 * The console session: auth + the two connection seams (#51).
 *
 * The RD authenticates with a bearer token (protocol.html §9.4). Per the brief the
 * token lives in memory, mirrored to `sessionStorage` so a tab reload inside the same
 * session keeps the RD signed in but it never persists to disk. That one token feeds
 * both seams:
 *
 *   • reads  — `@gridfpv/protocol-client`'s `connect({ baseUrl, scope, token })`
 *              (snapshot + WS stream; the console's whole live view).
 *   • writes — `createControlClient(baseUrl, token)` (the privileged control path).
 *
 * Everything reactive is Svelte 5 runes so screens read `session.connectionStatus`,
 * `session.liveState`, etc. directly. The protocol client is framework-agnostic, so
 * we bridge its `onState` callback into a `$state` field here.
 */

import { connect } from '@gridfpv/protocol-client';
import type { ProtocolClient, ProtocolState, ConnectionStatus } from '@gridfpv/protocol-client';
import { createControlClient } from './control.js';
import type { ControlClient } from './control.js';
import type { Command, CommandAck, LiveRaceState, ProjectionBody, Scope } from '@gridfpv/types';

const STORAGE_KEY = 'gridfpv.rd.session';

interface StoredSession {
  baseUrl: string;
  token: string;
}

function loadStored(): StoredSession | null {
  try {
    const raw = globalThis.sessionStorage?.getItem(STORAGE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<StoredSession>;
    if (typeof parsed.baseUrl === 'string' && typeof parsed.token === 'string') {
      return { baseUrl: parsed.baseUrl, token: parsed.token };
    }
  } catch {
    /* ignore malformed storage */
  }
  return null;
}

function persist(s: StoredSession | null): void {
  try {
    if (s) globalThis.sessionStorage?.setItem(STORAGE_KEY, JSON.stringify(s));
    else globalThis.sessionStorage?.removeItem(STORAGE_KEY);
  } catch {
    /* storage unavailable (private mode / SSR) — in-memory still works */
  }
}

/** Pull the `LiveRaceState` out of a projection body, if that's what it carries. */
function liveStateOf(body: ProjectionBody | undefined): LiveRaceState | undefined {
  if (body && 'LiveRaceState' in body) return body.LiveRaceState;
  return undefined;
}

/** The reactive console session — one instance, shared across screens. */
export class Session {
  /** Whether the RD has signed in (a token is held). */
  authenticated = $state(false);
  /** The base URL the RD connected to. */
  baseUrl = $state('');
  /** The live read-client's lifecycle status (connecting → live → …). */
  connectionStatus = $state<ConnectionStatus | 'idle'>('idle');
  /** The latest full protocol state (body + cursor + status + error). */
  protocolState = $state<ProtocolState | undefined>(undefined);
  /** The current `LiveRaceState`, when the live scope is connected. */
  liveState = $state<LiveRaceState | undefined>(undefined);
  /** The last control-path error surfaced to the RD (cleared on the next send). */
  lastCommandError = $state<CommandAck['error']>(undefined);

  // Non-reactive internals.
  #token: string | undefined;
  #client: ProtocolClient | undefined;
  #control: ControlClient | undefined;
  #unsub: (() => void) | undefined;
  // Injectable for tests so the session never opens a real socket.
  #connectImpl: typeof connect;
  #controlFactory: typeof createControlClient;

  constructor(opts?: {
    connectImpl?: typeof connect;
    controlFactory?: typeof createControlClient;
    autoRestore?: boolean;
  }) {
    this.#connectImpl = opts?.connectImpl ?? connect;
    this.#controlFactory = opts?.controlFactory ?? createControlClient;
    if (opts?.autoRestore !== false) {
      const stored = loadStored();
      if (stored) this.login(stored.baseUrl, stored.token);
    }
  }

  /**
   * Sign in: hold the token, open the control client, and connect the live read
   * client to the event scope so the console has a live view from the start.
   */
  login(baseUrl: string, token: string, scope?: Scope): void {
    this.logout(false);
    this.#token = token;
    this.baseUrl = baseUrl;
    this.authenticated = true;
    this.#control = this.#controlFactory(baseUrl, token);
    persist({ baseUrl, token });

    // Default to the whole event; a real event id is filled by the setup wizard, but
    // the console connects optimistically so connection status is visible at login.
    const liveScope: Scope = scope ?? { Event: { event: 'event' } };
    this.connectionStatus = 'connecting';
    this.#client = this.#connectImpl({ baseUrl, scope: liveScope, token });
    this.#unsub = this.#client.onState((state) => {
      this.protocolState = state;
      this.connectionStatus = state.status;
      this.liveState = liveStateOf(state.body);
    });
  }

  /** Re-scope the live read client (e.g. once the event id is known). */
  resubscribe(scope: Scope): void {
    if (!this.authenticated || !this.#token) return;
    this.#unsub?.();
    this.#client?.close();
    this.connectionStatus = 'connecting';
    this.#client = this.#connectImpl({ baseUrl: this.baseUrl, scope, token: this.#token });
    this.#unsub = this.#client.onState((state) => {
      this.protocolState = state;
      this.connectionStatus = state.status;
      this.liveState = liveStateOf(state.body);
    });
  }

  /** Sign out and tear down both seams. */
  logout(clearStorage = true): void {
    this.#unsub?.();
    this.#unsub = undefined;
    this.#client?.close();
    this.#client = undefined;
    this.#control = undefined;
    this.#token = undefined;
    this.authenticated = false;
    this.connectionStatus = 'idle';
    this.protocolState = undefined;
    this.liveState = undefined;
    this.lastCommandError = undefined;
    if (clearStorage) persist(null);
  }

  /**
   * Send a privileged command through the control client, recording any error for
   * the UI. Returns the raw `CommandAck` so callers can branch on success too.
   */
  async send(command: Command): Promise<CommandAck> {
    if (!this.#control) {
      const ack: CommandAck = {
        ok: false,
        error: { code: 'Unauthorized', message: 'Not signed in.' }
      };
      this.lastCommandError = ack.error;
      return ack;
    }
    const ack = await this.#control.sendCommand(command);
    this.lastCommandError = ack.ok ? undefined : ack.error;
    return ack;
  }

  /** Clear the last surfaced command error (e.g. when the RD dismisses it). */
  clearCommandError(): void {
    this.lastCommandError = undefined;
  }
}
