/**
 * The protocol client implementation.
 *
 * Lifecycle (protocol.html §2–§3):
 *
 *  1. `connect()` → GET the scoped snapshot. The snapshot carries the projection
 *     `body` plus the `cursor` the stream resumes from.
 *  2. Open a WebSocket and send a `SubscribeRequest { scope, from: cursor }`.
 *  3. Apply each incoming `ChangeEnvelope` in strictly increasing `sequence`
 *     order. Application is idempotent and keyed by `sequence`: an envelope at or
 *     below the last-applied cursor is a no-op (at-least-once, deduplicated).
 *  4. A *gap* (an envelope whose sequence skips past `lastApplied + 1`) means we
 *     missed envelopes the stream cannot replay → re-snapshot and re-subscribe
 *     from the fresh cursor.
 *  5. On socket drop, reconnect and resume from the last-applied cursor; if the
 *     server reports the cursor is too old (`StaleCursor`) — or resume otherwise
 *     fails — fall back to a re-snapshot.
 */

import type {
  ActiveEvent,
  ChangeEnvelope,
  CreateEventRequest,
  Cursor,
  EventId,
  EventMeta,
  ProjectionBody,
  ProtocolError,
  Scope,
  Snapshot,
  SubscribeRequest
} from '@gridfpv/types';

/** Minimal `fetch` surface this client needs (injectable for tests / Node). */
export type FetchLike = (input: string, init?: RequestInit) => Promise<Response>;

/**
 * Minimal `WebSocket` surface this client needs — a structural subset of the DOM
 * `WebSocket` so tests can inject a mock and Node can supply a polyfill.
 */
export interface WebSocketLike {
  send(data: string): void;
  close(code?: number, reason?: string): void;
  onopen: ((this: WebSocketLike, ev: unknown) => unknown) | null;
  onclose: ((this: WebSocketLike, ev: unknown) => unknown) | null;
  onerror: ((this: WebSocketLike, ev: unknown) => unknown) | null;
  onmessage: ((this: WebSocketLike, ev: { data: unknown }) => unknown) | null;
}

/** Factory that opens a {@link WebSocketLike} for a `ws(s)://…` URL. */
export type WebSocketFactory = (url: string) => WebSocketLike;

/** Where the connection is in its snapshot/stream lifecycle. */
export type ConnectionStatus =
  | 'connecting'
  | 'snapshotting'
  | 'subscribing'
  | 'live'
  | 'reconnecting'
  | 'closed';

/** The current typed projection state the client exposes to consumers. */
export interface ProtocolState {
  /** The materialized projection body, or `undefined` before the first snapshot. */
  readonly body: ProjectionBody | undefined;
  /** The last-applied stream cursor (snapshot cursor, advanced by each envelope). */
  readonly cursor: Cursor | undefined;
  /** Lifecycle status. */
  readonly status: ConnectionStatus;
  /** The last protocol/transport error, if the connection is degraded. */
  readonly error: ProtocolError | undefined;
}

/** A listener notified on every state change. Returns an unsubscribe function. */
export type StateListener = (state: ProtocolState) => void;

/** Options for {@link connect}. */
export interface ConnectOptions {
  /**
   * Base URL of the Director (or Cloud) protocol server, e.g.
   * `http://director.local:8080` or `https://cloud.gridfpv.example`. The client
   * is configured with the base URL *only*, so it cannot tell LAN from Cloud.
   */
  baseUrl: string;
  /**
   * The **event** this connection's scope lives in (issue #72). Every read/realtime surface
   * is rooted under `/events/{eventId}/…`, so the client targets one event's own log. Defaults
   * to the built-in `practice` event when omitted, so an un-migrated caller still connects to
   * a working event.
   */
  eventId?: EventId;
  /** The resource this connection is scoped to (protocol.html §4). */
  scope: Scope;
  /** Optional bearer token (sent as `Authorization: Bearer …` and on the WS URL). */
  token?: string;
  /** Inject a `fetch` (defaults to the global). Used by tests and Node. */
  fetch?: FetchLike;
  /** Inject a WebSocket factory (defaults to the global `WebSocket`). */
  webSocketFactory?: WebSocketFactory;
  /**
   * Reconnect backoff in ms (delay before re-opening a dropped socket).
   * Defaults to 1000ms. A timer of `0` reconnects on the next tick.
   */
  reconnectDelayMs?: number;
  /** Inject a timer (defaults to `setTimeout`). Used by tests. */
  setTimer?: (cb: () => void, ms: number) => unknown;
  /** Inject a timer-clear (defaults to `clearTimeout`). Used by tests. */
  clearTimer?: (handle: unknown) => void;
}

/**
 * A live connection to the protocol server: snapshot + WS subscribe, exposing the
 * current typed projection state via a framework-agnostic subscribe API.
 */
export interface ProtocolClient {
  readonly baseUrl: string;
  readonly scope: Scope;
  /** A synchronous snapshot of the current state. */
  getState(): ProtocolState;
  /**
   * Subscribe to state changes. The listener is invoked immediately with the
   * current state, then on every subsequent change. Returns an unsubscribe fn.
   */
  onState(listener: StateListener): () => void;
  /** Close the connection and tear down the WebSocket. Idempotent. */
  close(): void;
}

// The stream frames are `StreamMessage` (protocol.html §3, bindings/StreamMessage.ts),
// externally tagged: `{ "Change": ChangeEnvelope }` | `{ "ReSnapshotRequired": ProtocolError }`.
// A bare `ProtocolError` may also arrive (e.g. a VersionMismatch just before close).

// ── Cursor wire handling ───────────────────────────────────────────────────────
//
// `Cursor` is a u64 rendered as a plain TS `number` (bindings/Cursor.ts). Our
// cursors/sequences are bounded well below 2^53, and serde serialises them as JSON
// numbers, so they arrive already as `number`s — no coercion or custom stringifier
// is needed.

const isProtocolError = (v: unknown): v is ProtocolError =>
  typeof v === 'object' &&
  v !== null &&
  'code' in v &&
  'message' in v &&
  typeof (v as ProtocolError).message === 'string';

const isChangeEnvelope = (v: unknown): v is ChangeEnvelope =>
  typeof v === 'object' && v !== null && 'sequence' in v && 'projection' in v && 'change' in v;

const isStreamChange = (v: unknown): v is { Change: ChangeEnvelope } =>
  typeof v === 'object' &&
  v !== null &&
  'Change' in v &&
  isChangeEnvelope((v as { Change: unknown }).Change);

const isReSnapshotRequired = (v: unknown): v is { ReSnapshotRequired: ProtocolError } =>
  typeof v === 'object' && v !== null && 'ReSnapshotRequired' in v;

/** Map an http(s) base URL to its ws(s) equivalent. */
function toWebSocketBase(baseUrl: string): string {
  if (baseUrl.startsWith('https://')) return 'wss://' + baseUrl.slice('https://'.length);
  if (baseUrl.startsWith('http://')) return 'ws://' + baseUrl.slice('http://'.length);
  return baseUrl;
}

const trimSlash = (s: string): string => (s.endsWith('/') ? s.slice(0, -1) : s);

/** The event-root prefix every per-event surface lives under (issue #72): `/events/{id}`. */
function eventRoot(eventId: string): string {
  return `/events/${encodeURIComponent(eventId)}`;
}

/**
 * Build the snapshot path for a scope **within an event** (issue #72). The server addresses
 * snapshots by PATH, now rooted under the event: `/events/{eventId}/snapshot/event/{id}`,
 * `…/snapshot/heat/{id}`, `…/snapshot/class/{event}/{class}`, `…/snapshot/pilot/{event}/{pilot}`
 * (protocol.html §4 endpoint surface), NOT a `?scope=` query — so the client maps the scope to
 * that path under the resolved event.
 */
function snapshotPath(eventId: string, scope: Scope): string {
  const root = `${eventRoot(eventId)}/snapshot`;
  if ('Event' in scope) return `${root}/event/${encodeURIComponent(scope.Event.event)}`;
  if ('Heat' in scope) return `${root}/heat/${encodeURIComponent(scope.Heat.heat)}`;
  if ('Class' in scope) {
    return `${root}/class/${encodeURIComponent(scope.Class.event)}/${encodeURIComponent(
      scope.Class.class
    )}`;
  }
  return `${root}/pilot/${encodeURIComponent(scope.Pilot.event)}/${encodeURIComponent(
    scope.Pilot.pilot
  )}`;
}

/** The built-in Practice event id — the default the client connects to when none is given. */
export const PRACTICE_EVENT_ID = 'practice';

/**
 * List every event the server knows (`GET /events`) — issue #72. Reads are open on the LAN,
 * so no token is needed; an optional token is sent when present. Resolves to the events'
 * {@link EventMeta} (Practice first), or rejects on a transport/HTTP failure.
 */
export async function listEvents(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<EventMeta[]> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (options.token) headers.Authorization = `Bearer ${options.token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/events`, { headers });
  if (!resp.ok) throw new Error(`GET /events failed: HTTP ${resp.status}`);
  return (await resp.json()) as EventMeta[];
}

/**
 * Create a new event (`POST /events`) — issue #72. Control is **full-trust by default**
 * (#72, Slice 1b): the `token` is **optional** — an open (unconfigured) Director accepts the
 * create with no credential; a token-gated Director answers **401/403** and the caller obtains
 * a token lazily and retries. The body carries the display `name` plus any optional descriptive
 * `fields` (`date`/`location`/`description`/`organizer`); the id is auto-generated server-side.
 * Resolves to the new event's {@link EventMeta}, or rejects on a non-2xx / transport failure
 * (the HTTP status is in the error message so the caller can branch on 401/403).
 */
export async function createEvent(
  baseUrl: string,
  name: string,
  token?: string,
  options: { fetch?: FetchLike; fields?: Omit<CreateEventRequest, 'name'> } = {}
): Promise<EventMeta> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const body: CreateEventRequest = { name, ...(options.fields ?? {}) };
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'application/json'
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/events`, {
    method: 'POST',
    headers,
    body: JSON.stringify(body)
  });
  if (!resp.ok) throw new Error(`POST /events failed: HTTP ${resp.status}`);
  return (await resp.json()) as EventMeta;
}

/**
 * Read the Director's **active event** (`GET /active-event`) — issue #90. The active event is
 * Director (server-side) state: there is exactly one Race Director on one event, so every client
 * resolves this on connect/reload to resume into the selected event (the returned `event` is its
 * full {@link EventMeta}) or fall back to the picker (`event` is `null`). An open read — no token
 * needed. Resolves to the {@link ActiveEvent} body, or rejects on a transport/HTTP failure.
 */
export async function getActiveEvent(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<ActiveEvent> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (options.token) headers.Authorization = `Bearer ${options.token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/active-event`, { headers });
  if (!resp.ok) throw new Error(`GET /active-event failed: HTTP ${resp.status}`);
  return (await resp.json()) as ActiveEvent;
}

/**
 * Set the Director's **active event** (`PUT /active-event`) — issue #90. RD-gated like every
 * other control write (full-trust by default: an open Director accepts it tokenless; a gated one
 * answers **401/403** and the caller obtains a token and retries). The body carries the event
 * `id`; the server validates it names a known event (else **404**) and persists the selection so
 * it survives a Director restart. Resolves to the now-active event's {@link EventMeta}, or rejects
 * on a non-2xx / transport failure (the HTTP status is in the error message for branching).
 */
export async function setActiveEvent(
  baseUrl: string,
  id: EventId,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<EventMeta> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'application/json'
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/active-event`, {
    method: 'PUT',
    headers,
    body: JSON.stringify({ id })
  });
  if (!resp.ok) throw new Error(`PUT /active-event failed: HTTP ${resp.status}`);
  return (await resp.json()) as EventMeta;
}

/**
 * Connect to a GridFPV protocol server and begin the snapshot→subscribe handshake.
 *
 * Returns immediately with a {@link ProtocolClient}; the snapshot fetch and WS
 * subscribe proceed asynchronously and surface through the state/`onState` API.
 */
export function connect(options: ConnectOptions): ProtocolClient {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const wsFactory: WebSocketFactory =
    options.webSocketFactory ??
    ((url) => new globalThis.WebSocket(url) as unknown as WebSocketLike);
  const setTimer = options.setTimer ?? ((cb, ms) => setTimeout(cb, ms));
  const clearTimer =
    options.clearTimer ?? ((h) => clearTimeout(h as ReturnType<typeof setTimeout>));
  const reconnectDelayMs = options.reconnectDelayMs ?? 1000;

  const baseUrl = trimSlash(options.baseUrl);
  const wsBase = trimSlash(toWebSocketBase(options.baseUrl));
  const scope = options.scope;
  const token = options.token;
  // The event this connection is rooted under (issue #72); defaults to Practice.
  const eventId = options.eventId ?? PRACTICE_EVENT_ID;

  // ── Mutable connection state ───────────────────────────────────────────────
  let body: ProjectionBody | undefined;
  // The snapshot `cursor` is a log offset (protocol.html §2) used ONLY as the `from:`
  // resume point — it is not the stream's ordering counter.
  let cursor: Cursor | undefined;
  // The per-stream `sequence` axis (protocol.html §3/§9.5): starts at 1 on each
  // subscription, distinct from `cursor`. Reset to 0 on every (re)subscribe so the
  // first envelope is accepted whatever the snapshot cursor's value.
  let streamSeq: Cursor = 0;
  let status: ConnectionStatus = 'connecting';
  let lastError: ProtocolError | undefined;

  let ws: WebSocketLike | null = null;
  let closed = false;
  let reconnectHandle: unknown = null;
  /** Bumps every (re)connect attempt so stale callbacks from an old socket no-op. */
  let generation = 0;

  const listeners = new Set<StateListener>();

  const snapshot = (): ProtocolState => ({ body, cursor, status, error: lastError });

  function emit(): void {
    const s = snapshot();
    for (const l of listeners) l(s);
  }

  function setStatus(next: ConnectionStatus): void {
    if (status !== next) {
      status = next;
      emit();
    }
  }

  function fail(err: ProtocolError): void {
    lastError = err;
    emit();
  }

  // ── Snapshot (protocol.html §2) ────────────────────────────────────────────
  async function fetchSnapshot(gen: number): Promise<boolean> {
    setStatus('snapshotting');
    const headers: Record<string, string> = { Accept: 'application/json' };
    if (token) headers.Authorization = `Bearer ${token}`;
    let resp: Response;
    try {
      resp = await fetchImpl(`${baseUrl}${snapshotPath(eventId, scope)}`, { headers });
    } catch (e) {
      if (gen !== generation || closed) return false;
      fail({ code: 'Internal', message: `snapshot fetch failed: ${String(e)}` });
      return false;
    }
    if (gen !== generation || closed) return false;
    if (!resp.ok) {
      let err: ProtocolError = { code: 'Internal', message: `snapshot HTTP ${resp.status}` };
      try {
        const data: unknown = await resp.json();
        if (isProtocolError(data)) err = data;
      } catch {
        /* keep the HTTP-status error */
      }
      fail(err);
      return false;
    }
    const data = (await resp.json()) as Snapshot;
    if (gen !== generation || closed) return false;
    body = data.body;
    cursor = data.cursor;
    lastError = undefined;
    emit();
    return true;
  }

  // ── Apply one ordered change envelope (protocol.html §3) ────────────────────
  //
  // Returns 'applied', 'duplicate' (already seen — idempotent no-op), or 'gap'
  // (missed envelopes → caller must re-snapshot).
  function applyEnvelope(env: ChangeEnvelope): 'applied' | 'duplicate' | 'gap' {
    const seq = env.sequence;
    // Order + dedup against the per-stream `sequence` axis — NOT the snapshot
    // `cursor` (a log offset). The two are distinct monotonic counters
    // (protocol.html §3/§9.5); conflating them drops the early stream as bogus
    // "duplicates" and freezes the live view. `streamSeq === 0` means this is the
    // first envelope of a fresh subscription, so accept it whatever its value.
    if (streamSeq !== 0) {
      // Idempotent, keyed by sequence: anything at or below the last applied is a no-op.
      if (seq <= streamSeq) return 'duplicate';
      // The stream is contiguous: the next envelope must be exactly +1.
      if (seq !== streamSeq + 1) return 'gap';
    }
    const change = env.change;
    if ('FreshValue' in change) {
      body = change.FreshValue;
    } else {
      // Delta. The per-projection delta encodings are deferred (#43): the wire
      // type carries an opaque payload today. We advance the sequence so ordering
      // and gap-detection stay correct; once #43 pins the typed deltas, fold them
      // into `body` here per ProjectionKind. Until then a delta cannot mutate
      // `body`, and a re-snapshot (always correct, §3) reconciles any drift.
      void change.Delta;
    }
    streamSeq = seq;
    return 'applied';
  }

  // ── WebSocket subscribe + stream (protocol.html §3) ─────────────────────────
  function openSocket(gen: number): void {
    if (gen !== generation || closed) return;
    // A fresh subscription: the server restarts the per-stream sequence at 1, so
    // reset our tracker to accept it from the top.
    streamSeq = 0;
    setStatus('subscribing');
    const streamPath = `${eventRoot(eventId)}/stream`;
    const url = token
      ? `${wsBase}${streamPath}?token=${encodeURIComponent(token)}`
      : `${wsBase}${streamPath}`;
    let socket: WebSocketLike;
    try {
      socket = wsFactory(url);
    } catch (e) {
      scheduleReconnect(gen, { code: 'Internal', message: `WS open failed: ${String(e)}` });
      return;
    }
    ws = socket;

    socket.onopen = () => {
      if (gen !== generation || closed) return;
      const req: SubscribeRequest = { scope, from: cursor };
      socket.send(JSON.stringify(req));
      setStatus('live');
    };

    socket.onmessage = (ev) => {
      if (gen !== generation || closed) return;
      void handleMessage(gen, ev.data);
    };

    socket.onerror = () => {
      /* surfaced via onclose; nothing actionable here */
    };

    socket.onclose = () => {
      if (gen !== generation || closed) return;
      scheduleReconnect(gen, undefined);
    };
  }

  async function handleMessage(gen: number, raw: unknown): Promise<void> {
    let parsed: unknown;
    try {
      parsed = typeof raw === 'string' ? JSON.parse(raw) : raw;
    } catch {
      fail({ code: 'BadRequest', message: 'malformed stream frame' });
      return;
    }

    // `StreamMessage::Change(envelope)` — the common case: unwrap + apply.
    if (isStreamChange(parsed)) {
      const result = applyEnvelope(parsed.Change);
      if (result === 'gap') {
        // Missed envelopes the stream can't replay → re-snapshot and re-subscribe.
        await resnapshot(gen);
        return;
      }
      if (result === 'applied') emit();
      return;
    }

    // `StreamMessage::ReSnapshotRequired(error)` — the cursor is unreplayable →
    // re-snapshot from scratch (always correct, §3).
    if (isReSnapshotRequired(parsed)) {
      await resnapshot(gen);
      return;
    }

    // A bare ProtocolError (e.g. a VersionMismatch sent just before the socket closes).
    if (isProtocolError(parsed)) {
      if (parsed.code === 'StaleCursor') await resnapshot(gen);
      else fail(parsed);
      return;
    }

    // Unknown frame (e.g. a future additive message) — ignore it so an additive
    // protocol change doesn't break an older client (§7).
  }

  // Re-snapshot in place (gap or stale cursor), then re-subscribe from the fresh
  // cursor. The existing socket is torn down so the new subscribe is unambiguous.
  async function resnapshot(gen: number): Promise<void> {
    if (gen !== generation || closed) return;
    teardownSocket();
    const ok = await fetchSnapshot(gen);
    if (gen !== generation || closed) return;
    if (ok) openSocket(gen);
    else scheduleReconnect(gen, lastError);
  }

  function scheduleReconnect(gen: number, err: ProtocolError | undefined): void {
    if (gen !== generation || closed) return;
    teardownSocket();
    if (err) lastError = err;
    setStatus('reconnecting');
    reconnectHandle = setTimer(() => {
      if (closed) return;
      reconnect();
    }, reconnectDelayMs);
  }

  // Reconnect (protocol.html §3): resume from the last-applied cursor by
  // re-subscribing; if we never got a snapshot, take one first. A StaleCursor on
  // resume drives a re-snapshot via the stream error path.
  function reconnect(): void {
    if (closed) return;
    const gen = ++generation;
    if (cursor === undefined) {
      void (async () => {
        const ok = await fetchSnapshot(gen);
        if (gen !== generation || closed) return;
        if (ok) openSocket(gen);
        else scheduleReconnect(gen, lastError);
      })();
    } else {
      openSocket(gen);
    }
  }

  function teardownSocket(): void {
    if (reconnectHandle !== null) {
      clearTimer(reconnectHandle);
      reconnectHandle = null;
    }
    if (ws) {
      const dead = ws;
      ws = null;
      dead.onopen = null;
      dead.onclose = null;
      dead.onerror = null;
      dead.onmessage = null;
      try {
        dead.close();
      } catch {
        /* ignore */
      }
    }
  }

  // ── Kick off the handshake ─────────────────────────────────────────────────
  function start(): void {
    const gen = ++generation;
    void (async () => {
      const ok = await fetchSnapshot(gen);
      if (gen !== generation || closed) return;
      if (ok) openSocket(gen);
      else scheduleReconnect(gen, lastError);
    })();
  }

  start();

  return {
    baseUrl,
    scope,
    getState: snapshot,
    onState(listener: StateListener): () => void {
      listeners.add(listener);
      listener(snapshot());
      return () => listeners.delete(listener);
    },
    close(): void {
      if (closed) return;
      closed = true;
      generation++;
      teardownSocket();
      setStatus('closed');
      listeners.clear();
    }
  };
}
