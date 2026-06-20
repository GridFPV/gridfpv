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
import type {
  Command,
  CommandAck,
  HeatId,
  HeatResult,
  LiveRaceState,
  ProjectionBody,
  Scope
} from '@gridfpv/types';

const STORAGE_KEY = 'gridfpv.rd.session';

/**
 * The built-in **Practice** event id the console connects to by default (issue #72). Events
 * are now first-class containers — every read/realtime/control surface is rooted under one.
 * The full startup/event-picker UI is a follow-up PR; until then the console defaults to the
 * always-present Practice event so the whole stack works end to end.
 */
const PRACTICE_EVENT_ID = 'practice';

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
  /**
   * The latest full protocol state (body + cursor + status + error). `$state.raw`:
   * the body is an immutable whole value replaced wholesale on every stream update,
   * so we reassign rather than deep-proxy it — deep `$state` proxying a large
   * projection body is wasteful and a re-render footgun for external immutable data.
   */
  protocolState = $state.raw<ProtocolState | undefined>(undefined);
  /** The current `LiveRaceState`, when the live scope is connected (immutable whole). */
  liveState = $state.raw<LiveRaceState | undefined>(undefined);
  /**
   * The latest scored heat result the RD pulled via {@link fetchHeatResult}. The live
   * read stream only carries `LiveRaceState`; a scored `HeatResult` is a separate,
   * tighter heat-scope read (`?projection=result`), so the Results screen reads this.
   */
  heatResult = $state.raw<HeatResult | undefined>(undefined);
  /** The last control-path error surfaced to the RD (cleared on the next send). */
  lastCommandError = $state<CommandAck['error']>(undefined);

  // Non-reactive internals.
  #token: string | undefined;
  /** The event every surface is rooted under (#72); defaults to the built-in Practice event. */
  #eventId: string = PRACTICE_EVENT_ID;
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
    // Control is rooted under the current event (#72) — default Practice.
    this.#control = this.#controlFactory(baseUrl, token, { eventId: this.#eventId });
    persist({ baseUrl, token });

    // Default to the whole (Practice) event so the console has a live view from login. The
    // event id is the built-in Practice event (#72); the startup/event-picker UI that lets
    // the RD choose another event is a follow-up PR.
    const liveScope: Scope = scope ?? { Event: { event: this.#eventId } };
    this.connectionStatus = 'connecting';
    this.#client = this.#connectImpl({
      baseUrl,
      eventId: this.#eventId,
      scope: liveScope,
      token
    });
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
    this.#client = this.#connectImpl({
      baseUrl: this.baseUrl,
      eventId: this.#eventId,
      scope,
      token: this.#token
    });
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
    this.heatResult = undefined;
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

  /**
   * Pull a heat's scored result (`GET /snapshot/heat/{heat}?projection=result`) and
   * store it on {@link heatResult} for the Results screen. The live read stream only
   * carries `LiveRaceState`; the scored `HeatResult` is a separate heat-scope read. A
   * non-2xx or malformed body leaves `heatResult` unchanged.
   */
  async fetchHeatResult(heat: HeatId): Promise<HeatResult | undefined> {
    const base = this.baseUrl.endsWith('/') ? this.baseUrl.slice(0, -1) : this.baseUrl;
    const headers: Record<string, string> = {};
    if (this.#token) headers.Authorization = `Bearer ${this.#token}`;
    try {
      const resp = await globalThis.fetch(
        `${base}/events/${encodeURIComponent(this.#eventId)}/snapshot/heat/${encodeURIComponent(
          heat
        )}?projection=result`,
        { headers }
      );
      if (!resp.ok) return undefined;
      const snap: unknown = await resp.json();
      if (
        snap &&
        typeof snap === 'object' &&
        'body' in snap &&
        snap.body &&
        typeof snap.body === 'object' &&
        'HeatResult' in snap.body
      ) {
        this.heatResult = (snap.body as { HeatResult: HeatResult }).HeatResult;
        return this.heatResult;
      }
    } catch {
      /* leave heatResult unchanged */
    }
    return undefined;
  }
}
