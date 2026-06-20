/**
 * The console session: the current **event** + the two connection seams (#51, #72).
 *
 * Slice 1b reshapes the session around events being the outer container (issue #72):
 * the console no longer logs into an *address* — the Director is the page's own origin,
 * defaulted from `location.origin`. What's left of auth is just the **RD token**, and
 * that is now requested **lazily**: reading the event list and browsing an event need no
 * token; the token is only obtained on a privileged action (create event, or a control
 * command). This is forward-compatible with the loopback-trust auth slice — when that
 * lands, the lazy ask simply never fires on localhost.
 *
 * Lifecycle:
 *
 *   • No event selected → the app is at the **event picker** (`currentEvent === undefined`).
 *   • `selectEvent(meta)` roots both seams under `/events/{id}/…`:
 *       - reads  — `@gridfpv/protocol-client`'s `connect({ baseUrl, eventId, scope })`
 *                  (snapshot + WS stream; the console's whole live view). Open, no token.
 *       - writes — `createControlClient(baseUrl, token, { eventId })` (the privileged path),
 *                  (re)built whenever the token becomes available.
 *   • `leaveEvent()` tears both seams down and returns to the picker.
 *
 * The RD token lives in memory, mirrored to `sessionStorage` so a tab reload inside the
 * same session keeps it, but it never persists to disk. A {@link tokenProvider} (set by
 * the shell) lets {@link send}/{@link createEventAndEnter} prompt for the token only when
 * a privileged action actually needs one.
 *
 * Everything reactive is Svelte 5 runes so screens read `session.connectionStatus`,
 * `session.liveState`, etc. directly. The protocol client is framework-agnostic, so we
 * bridge its `onState` callback into a `$state` field here.
 */

import { connect, listEvents, createEvent, PRACTICE_EVENT_ID } from '@gridfpv/protocol-client';
import type { ProtocolClient, ProtocolState, ConnectionStatus } from '@gridfpv/protocol-client';
import { createControlClient } from './control.js';
import type { ControlClient } from './control.js';
import type {
  Command,
  CommandAck,
  EventMeta,
  HeatId,
  HeatResult,
  LiveRaceState,
  ProjectionBody,
  Scope
} from '@gridfpv/types';

const TOKEN_STORAGE_KEY = 'gridfpv.rd.token';

/**
 * Asks the RD for a control token. Returns the entered token, or `undefined` if the
 * RD cancelled. The shell wires this to a token `Dialog`; the session calls it lazily
 * the first time a privileged action needs a token it doesn't already hold.
 */
export type TokenProvider = () => Promise<string | undefined>;

function loadStoredToken(): string | undefined {
  try {
    const raw = globalThis.sessionStorage?.getItem(TOKEN_STORAGE_KEY);
    return raw ?? undefined;
  } catch {
    return undefined;
  }
}

function persistToken(token: string | undefined): void {
  try {
    if (token) globalThis.sessionStorage?.setItem(TOKEN_STORAGE_KEY, token);
    else globalThis.sessionStorage?.removeItem(TOKEN_STORAGE_KEY);
  } catch {
    /* storage unavailable (private mode / SSR) — in-memory still works */
  }
}

/** The default base URL: the page's own origin (the Director serves the console). */
function defaultBaseUrl(): string {
  return globalThis.location?.origin || 'http://localhost:8080';
}

/** Pull the `LiveRaceState` out of a projection body, if that's what it carries. */
function liveStateOf(body: ProjectionBody | undefined): LiveRaceState | undefined {
  if (body && 'LiveRaceState' in body) return body.LiveRaceState;
  return undefined;
}

/** The reactive console session — one instance, shared across screens. */
export class Session {
  /**
   * The event the console is currently inside (#72), or `undefined` when the RD is at
   * the **event picker** (the landing screen). Selecting an event roots both seams
   * under it; leaving returns here.
   */
  currentEvent = $state.raw<EventMeta | undefined>(undefined);
  /** The base URL both seams target — the Director's origin. */
  baseUrl = $state(defaultBaseUrl());
  /** Whether an RD token is currently held (drives the settings/gear state). */
  hasToken = $state(false);
  /** The live read-client's lifecycle status (connecting → live → …); `idle` at the picker. */
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
  #client: ProtocolClient | undefined;
  #control: ControlClient | undefined;
  #unsub: (() => void) | undefined;
  /** The shell's lazy token prompt (a `Dialog`); set via {@link setTokenProvider}. */
  #tokenProvider: TokenProvider | undefined;
  // Injectable for tests so the session never opens a real socket.
  #connectImpl: typeof connect;
  #controlFactory: typeof createControlClient;
  #listEventsImpl: typeof listEvents;
  #createEventImpl: typeof createEvent;

  constructor(opts?: {
    connectImpl?: typeof connect;
    controlFactory?: typeof createControlClient;
    listEventsImpl?: typeof listEvents;
    createEventImpl?: typeof createEvent;
    baseUrl?: string;
    autoRestore?: boolean;
  }) {
    this.#connectImpl = opts?.connectImpl ?? connect;
    this.#controlFactory = opts?.controlFactory ?? createControlClient;
    this.#listEventsImpl = opts?.listEventsImpl ?? listEvents;
    this.#createEventImpl = opts?.createEventImpl ?? createEvent;
    if (opts?.baseUrl) this.baseUrl = opts.baseUrl;
    if (opts?.autoRestore !== false) {
      const stored = loadStoredToken();
      if (stored) {
        this.#token = stored;
        this.hasToken = true;
      }
    }
  }

  /** Install the shell's lazy token prompt. Called once by the app shell. */
  setTokenProvider(provider: TokenProvider): void {
    this.#tokenProvider = provider;
  }

  /** Set (or replace) the held RD token — e.g. from the settings/gear. */
  setToken(token: string): void {
    const trimmed = token.trim();
    if (!trimmed) return;
    this.#token = trimmed;
    this.hasToken = true;
    persistToken(trimmed);
    // Re-home the control client so it carries the new token, if inside an event.
    if (this.currentEvent) {
      this.#control = this.#controlFactory(this.baseUrl, this.#token, {
        eventId: this.currentEvent.id
      });
    }
  }

  /** Forget the held RD token (settings/gear "clear"). Reads keep working (open). */
  clearToken(): void {
    this.#token = undefined;
    this.hasToken = false;
    persistToken(undefined);
    if (this.currentEvent) {
      this.#control = this.#controlFactory(this.baseUrl, undefined, {
        eventId: this.currentEvent.id
      });
    }
  }

  /**
   * List the events the Director knows (`GET /events`) — open, no token. Used by the
   * event picker on load. Rejects on a transport/HTTP failure (the picker shows it).
   */
  listEvents(): Promise<EventMeta[]> {
    return this.#listEventsImpl(this.baseUrl, { token: this.#token });
  }

  /**
   * Enter an event (#72): root both seams under `/events/{id}/…` and open the live read
   * stream (open, no token). The control client is built with whatever token is held;
   * if none, the first privileged {@link send} will lazily obtain one.
   */
  selectEvent(meta: EventMeta, scope?: Scope): void {
    this.leaveEvent();
    this.currentEvent = meta;
    this.#control = this.#controlFactory(this.baseUrl, this.#token, { eventId: meta.id });

    const liveScope: Scope = scope ?? { Event: { event: meta.id } };
    this.connectionStatus = 'connecting';
    this.#client = this.#connectImpl({
      baseUrl: this.baseUrl,
      eventId: meta.id,
      scope: liveScope,
      token: this.#token
    });
    this.#unsub = this.#client.onState((state) => {
      this.protocolState = state;
      this.connectionStatus = state.status;
      this.liveState = liveStateOf(state.body);
    });
  }

  /** Re-scope the live read client within the current event (e.g. to a heat scope). */
  resubscribe(scope: Scope): void {
    const event = this.currentEvent;
    if (!event) return;
    this.#unsub?.();
    this.#client?.close();
    this.connectionStatus = 'connecting';
    this.#client = this.#connectImpl({
      baseUrl: this.baseUrl,
      eventId: event.id,
      scope,
      token: this.#token
    });
    this.#unsub = this.#client.onState((state) => {
      this.protocolState = state;
      this.connectionStatus = state.status;
      this.liveState = liveStateOf(state.body);
    });
  }

  /** Leave the current event and return to the picker; tears the read seam down. */
  leaveEvent(): void {
    this.#unsub?.();
    this.#unsub = undefined;
    this.#client?.close();
    this.#client = undefined;
    this.#control = undefined;
    this.currentEvent = undefined;
    this.connectionStatus = 'idle';
    this.protocolState = undefined;
    this.liveState = undefined;
    this.heatResult = undefined;
    this.lastCommandError = undefined;
  }

  /**
   * Ensure a token is held, prompting for one lazily if not. Returns `true` once a token
   * is present, `false` if the RD cancelled the prompt. The control client is rebuilt to
   * carry a freshly-obtained token.
   */
  async #ensureToken(): Promise<boolean> {
    if (this.#token) return true;
    if (!this.#tokenProvider) return false;
    const entered = await this.#tokenProvider();
    if (!entered?.trim()) return false;
    this.setToken(entered);
    return true;
  }

  /**
   * Create a persistent event by name (`POST /events`, RD-gated) and enter it. Obtains
   * the RD token lazily if none is held. Returns the new {@link EventMeta}, or throws on
   * a transport/HTTP failure, or `undefined` if the RD cancelled the token prompt.
   */
  async createEventAndEnter(name: string): Promise<EventMeta | undefined> {
    if (!(await this.#ensureToken()) || !this.#token) return undefined;
    const meta = await this.#createEventImpl(this.baseUrl, name, this.#token);
    this.selectEvent(meta);
    return meta;
  }

  /**
   * Send a privileged command through the control client, recording any error for the UI.
   * Obtains the RD token lazily first (issue #72): a control action is the moment auth is
   * actually needed, so if no token is held the shell's token prompt fires; cancelling
   * yields an `Unauthorized` ack. Returns the raw `CommandAck` so callers branch on success.
   */
  async send(command: Command): Promise<CommandAck> {
    if (!this.#control) {
      const ack: CommandAck = {
        ok: false,
        error: { code: 'Unauthorized', message: 'No event selected.' }
      };
      this.lastCommandError = ack.error;
      return ack;
    }
    // Lazy auth: only reach for the token (and its async prompt) when none is held — a
    // held token sends immediately, keeping the common path a single await.
    if (!this.#token && !(await this.#ensureToken())) {
      const ack: CommandAck = {
        ok: false,
        error: { code: 'Unauthorized', message: 'A control token is required for this action.' }
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
   * Pull a heat's scored result (`GET /events/{event}/snapshot/heat/{heat}?projection=result`)
   * and store it on {@link heatResult} for the Results screen. The live read stream only
   * carries `LiveRaceState`; the scored `HeatResult` is a separate heat-scope read. A non-2xx
   * or malformed body leaves `heatResult` unchanged.
   */
  async fetchHeatResult(heat: HeatId): Promise<HeatResult | undefined> {
    const event = this.currentEvent;
    if (!event) return undefined;
    const base = this.baseUrl.endsWith('/') ? this.baseUrl.slice(0, -1) : this.baseUrl;
    const headers: Record<string, string> = {};
    if (this.#token) headers.Authorization = `Bearer ${this.#token}`;
    try {
      const resp = await globalThis.fetch(
        `${base}/events/${encodeURIComponent(event.id)}/snapshot/heat/${encodeURIComponent(
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

export { PRACTICE_EVENT_ID };
