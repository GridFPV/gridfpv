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
  CalibrationRequest,
  CaptureDispatch,
  CaptureRequest,
  ChangeEnvelope,
  ChannelCatalogEntry,
  ChannelDispatch,
  ChannelLayouts,
  ChannelRequest,
  Class,
  ClassId,
  ClassStandings,
  CreateClassRequest,
  CreateEventRequest,
  CreatePilotRequest,
  CreateTimerRequest,
  Cursor,
  EventAuditEntry,
  EventId,
  EventMeta,
  FormatSchema,
  HeatSummary,
  ImdReading,
  LayoutId,
  MemberSlot,
  NewChannelLayoutRequest,
  NewRoundReq,
  Pilot,
  PilotId,
  ProjectionBody,
  ProtocolError,
  RankEntry,
  RoundDef,
  RoundId,
  RoundIssue,
  RoundStanding,
  Scope,
  SetChannelLayoutRequest,
  SetClassHiddenRequest,
  SetTimerNodesRequest,
  Snapshot,
  SubscribeRequest,
  Timer,
  TimerId,
  TimerNodes,
  TimerSignal,
  UpdateClassRequest,
  UpdatePilotRequest,
  UpdateRoundReq,
  UpdateTimerRequest
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
   * is rooted under `/events/{eventId}/…`, so the client targets one event's own log.
   * **Required** — there is no built-in event to fall back to (#414), and silently defaulting
   * to a magic id would connect a caller to something it never chose.
   */
  eventId: EventId;
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

// ── Request failures: the Director's own words, and the status carried structurally ───────────
//
// Every non-2xx response in this file rejects through `requestFailed` (#433). Before it, each of
// ~40 call sites formatted its own `METHOD /events/{id}/rounds/{id} failed: HTTP 400` line and
// threw the Director's typed body away — which put two raw ids on screen (the repo display rule
// forbids exactly that) *and* discarded the one sentence worth showing.

/**
 * The rejection every non-2xx response in this client produces (#433).
 *
 * `status` is carried **structurally**, not spelled into the message, so a caller branches on the
 * number (`isAuthFailure` in the console keys on 401/403) while the *words* stay free to be the
 * Director's own sentence. Matching a status out of prose was how `evt-401` in a 500's message
 * used to open the token dialog.
 */
export interface RequestFailure extends Error {
  /** The HTTP status the Director answered with. */
  status: number;
  /** The Director's branchable {@link ProtocolError} category, when it sent a typed body. */
  code?: ProtocolError['code'];
}

/** Whether a thrown value is a {@link RequestFailure} — i.e. it carries an HTTP `status`. */
export function isRequestFailure(e: unknown): e is RequestFailure {
  return e instanceof Error && typeof (e as Partial<RequestFailure>).status === 'number';
}

/** The `message` of a typed error body, trimmed, or `''` when the body carries no usable one. */
function bodyMessage(v: unknown): string {
  if (typeof v !== 'object' || v === null || !('message' in v)) return '';
  const m = (v as { message: unknown }).message;
  return typeof m === 'string' ? m.trim() : '';
}

/**
 * Build the error a failed request rejects with: **the Director's typed refusal, verbatim.**
 *
 * The Director's `ProtocolError` body is already phrased for the RD and names heats, timers, nodes
 * and channels by their **friendly** names — deliberately, precisely so it can be shown:
 *
 * > this round has a heat in progress (Practice Heat) — finalize or reset it before removing the
 * > round
 *
 * That is the message the RD can act on, so it is thrown as-is. Wrapping it in a route line would
 * both bury it and put the raw ids from the path in front of a user, which the repo display rule
 * forbids.
 *
 * Only a response with **no usable body** falls back, and the fallback says what was *attempted*
 * in words — `attempted` is an infinitive phrase like `'remove the round'` — never the method and
 * URL. A bodyless 500 is a poor message, but an honest one; it must never be a silent one.
 *
 * The status is attached to the error rather than spelled into it (see {@link RequestFailure}).
 */
async function requestFailed(resp: Response, attempted: string): Promise<RequestFailure> {
  let body: unknown;
  try {
    body = await resp.json();
  } catch {
    // No body, a truncated one, or an HTML error page from something in front of the Director.
  }
  const detail = bodyMessage(body);
  const err = new Error(
    detail || `The Director could not ${attempted} (HTTP ${resp.status}).`
  ) as RequestFailure;
  err.status = resp.status;
  if (isProtocolError(body)) err.code = body.code;
  return err;
}

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

/**
 * List every event the server knows (`GET /events`) — issue #72. Reads are open on the LAN,
 * so no token is needed; an optional token is sent when present. Resolves to the events'
 * {@link EventMeta} in id order — **possibly empty**, which is a fresh Director's first-run
 * state (#414), not an error — or rejects on a transport/HTTP failure.
 */
export async function listEvents(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<EventMeta[]> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (options.token) headers.Authorization = `Bearer ${options.token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/events`, { headers });
  if (!resp.ok) throw await requestFailed(resp, 'list the events');
  return (await resp.json()) as EventMeta[];
}

/**
 * Create a new event (`POST /events`) — issue #72. Control is **full-trust by default**
 * (#72, Slice 1b): the `token` is **optional** — an open (unconfigured) Director accepts the
 * create with no credential; a token-gated Director answers **401/403** and the caller obtains
 * a token lazily and retries. The body carries the display `name` plus any optional descriptive
 * `fields` (`date`/`location`/`description`/`organizer`); the id is auto-generated server-side.
 * Resolves to the new event's {@link EventMeta}, or rejects on a non-2xx / transport failure
 * (a {@link RequestFailure} carrying the status, so the caller can branch on 401/403).
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
  if (!resp.ok) throw await requestFailed(resp, 'create the event');
  return (await resp.json()) as EventMeta;
}

/**
 * **Permanently delete** an event and ALL of its data (`DELETE /events/{id}`) — the papercut fix.
 * RD-gated like {@link createEvent} (full-trust by default: an open Director accepts it tokenless;
 * a gated one answers **401/403** and the caller obtains a token and retries). The delete is total
 * and irreversible server-side: the event's registry entry, its persisted state, and the active
 * pointer if it pointed here are all removed. The built-in **Practice** event cannot be deleted
 * (the Director answers **400**); an unknown id is a **404**. Resolves on a 2xx, or rejects with an
 * `Error` whose message carries the HTTP status (so the caller can branch on 401/403/400/404).
 */
export async function deleteEvent(
  baseUrl: string,
  id: EventId,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/events/${encodeURIComponent(id)}`, {
    method: 'DELETE',
    headers
  });
  if (!resp.ok) throw await requestFailed(resp, 'delete the event');
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
  if (!resp.ok) throw await requestFailed(resp, 'read the active event');
  return (await resp.json()) as ActiveEvent;
}

/**
 * Set the Director's **active event** (`PUT /active-event`) — issue #90. RD-gated like every
 * other control write (full-trust by default: an open Director accepts it tokenless; a gated one
 * answers **401/403** and the caller obtains a token and retries). The body carries the event
 * `id`; the server validates it names a known event (else **404**) and persists the selection so
 * it survives a Director restart. Resolves to the now-active event's {@link EventMeta}, or rejects
 * on a non-2xx / transport failure (a {@link RequestFailure} carrying the status for branching).
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
  if (!resp.ok) throw await requestFailed(resp, 'set the active event');
  return (await resp.json()) as EventMeta;
}

/**
 * List every configured timer (`GET /timers`) — issue #73. Timers are **application-level
 * configuration**: the RD configures them once (a persisted registry) and each event selects
 * which to use. Reads are open on the LAN (no token needed; an optional token is sent when
 * present). Resolves to the {@link Timer}s (the built-in **Mock** first), or rejects on a
 * transport/HTTP failure.
 */
export async function listTimers(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<Timer[]> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (options.token) headers.Authorization = `Bearer ${options.token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/timers`, { headers });
  if (!resp.ok) throw await requestFailed(resp, 'list the timers');
  return (await resp.json()) as Timer[];
}

/**
 * Create a timer (`POST /timers`) — issue #73. RD-gated (full-trust by default: an open Director
 * accepts it tokenless; a gated one answers **401/403** and the caller obtains a token and
 * retries). The body carries the display `name` plus the {@link CreateTimerRequest['kind']} config
 * (a `Sim` or a reserved `Rotorhazard`); the id is auto-generated server-side. Resolves to the new
 * {@link Timer}, or rejects on a non-2xx / transport failure (a {@link RequestFailure}).
 */
export async function createTimer(
  baseUrl: string,
  request: CreateTimerRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<Timer> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'application/json'
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/timers`, {
    method: 'POST',
    headers,
    body: JSON.stringify(request)
  });
  if (!resp.ok) throw await requestFailed(resp, 'add the timer');
  return (await resp.json()) as Timer;
}

/**
 * Edit a timer (`PUT /timers/{id}`) — issue #73. RD-gated. The body's fields are all optional
 * (a partial edit of `name` and/or `kind`); the built-in Mock may be retuned but not deleted.
 * An unknown id answers **404**. Resolves to the updated {@link Timer}, or rejects on a non-2xx /
 * transport failure.
 */
export async function updateTimer(
  baseUrl: string,
  id: TimerId,
  request: UpdateTimerRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<Timer> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'application/json'
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/timers/${encodeURIComponent(id)}`, {
    method: 'PUT',
    headers,
    body: JSON.stringify(request)
  });
  if (!resp.ok) throw await requestFailed(resp, 'save the timer');
  return (await resp.json()) as Timer;
}

/**
 * **Hold** a live connection to a RotorHazard timer (`POST /timers/{id}/connect`) — issue #383.
 * RD-gated. The hold is independent of any event: the Director's connection reconciler dials the
 * timer on its next tick whether or not an event exists, so the Timers screen can answer "is this
 * URL right? does it have the plugin?" while an RD is setting up at a venue. The hold is explicit
 * and lasts until {@link disconnectTimer}.
 *
 * The built-in **Mock has nothing to dial** and answers **400**; an unknown id answers **404**.
 * Resolves to the updated {@link Timer} (its `manual_connect` now `true`), or rejects on a
 * non-2xx / transport failure (a {@link RequestFailure} carrying the status for branching).
 */
export async function connectTimer(
  baseUrl: string,
  id: TimerId,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<Timer> {
  return setTimerConnection(baseUrl, id, 'connect', token, options);
}

/**
 * **Release** a manually-held RotorHazard connection (`POST /timers/{id}/disconnect`) — issue #383.
 * RD-gated. Clears the hold; the reconciler drops the link on its next tick — unless the active
 * event also selects the timer, in which case that connection stays up (the two inputs are held
 * separately on purpose). An unknown id answers **404**. Resolves to the updated {@link Timer}
 * (its `manual_connect` now `false`), or rejects on a non-2xx / transport failure.
 */
export async function disconnectTimer(
  baseUrl: string,
  id: TimerId,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<Timer> {
  return setTimerConnection(baseUrl, id, 'disconnect', token, options);
}

/**
 * Restart a RotorHazard timer's server (`POST /timers/{id}/restart`) — issue #386.
 *
 * The guided plugin install's last step: RotorHazard imports plugins **once at startup**, so the
 * `plugins/gridfpv/` folder the RD just dropped in stays inert until RH re-executes. The Director
 * emits RotorHazard's `restart_server` on the socket it already holds, so the whole install stays
 * inside GridFPV. RD-gated.
 *
 * The Director **refuses** (a **400**) while a race is in progress on the timer — the message names
 * the heat — and for a Mock or a timer that is not connected; an unknown id answers **404**.
 * Resolves to the {@link Timer}, or rejects on a non-2xx / transport failure (a
 * {@link RequestFailure} whose message is the Director's own refusal).
 *
 * What follows a success is an **expected** drop → reconnect: RotorHazard re-executes, the timer
 * passes through `Disconnected`/`Error` for a few seconds, and the Director's reconnect re-probes
 * the plugin — which is what flips `plugin` from `Missing` to `Present`. That window is a restart in
 * progress, not a fault, and the console presents it as such.
 */
export async function restartTimer(
  baseUrl: string,
  id: TimerId,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<Timer> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/timers/${encodeURIComponent(id)}/restart`, {
    method: 'POST',
    headers
  });
  if (!resp.ok) throw await requestFailed(resp, 'restart the timer');
  return (await resp.json()) as Timer;
}

/**
 * Read a timer's **live tuning signal** (`GET /timers/{id}/signal`) — issue #355.
 *
 * **The call is the subscription.** The Director streams a timer's telemetry only while somebody is
 * looking at it: the first call opens the stream and *every* call renews a short lease on it
 * (`SIGNAL_LEASE`). Stop calling and the stream stops by itself — which is what makes a closed tab,
 * a crashed browser or a dropped network safe, and why nothing here has to say goodbye. A caller
 * that wants the plot to keep moving must therefore poll well inside that lease, not merely inside
 * it; see the Tune page's `holdsLease`.
 *
 * RD-gated (a token-gated Director answers **401** without one). A Mock is a **400** — it has no
 * signal to read — and an unknown id a **404**. Pass `options.signal` to abandon a poll in flight.
 *
 * Nothing this touches is an event or a log: it is a bounded in-memory window that exists only
 * while an RD is watching it.
 */
export async function timerSignal(
  baseUrl: string,
  id: TimerId,
  options: { token?: string; fetch?: FetchLike; signal?: AbortSignal } = {}
): Promise<TimerSignal> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (options.token) headers.Authorization = `Bearer ${options.token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/timers/${encodeURIComponent(id)}/signal`, {
    headers,
    signal: options.signal
  });
  if (!resp.ok) throw await requestFailed(resp, 'read the timer signal');
  return (await resp.json()) as TimerSignal;
}

/**
 * End a timer's tuning stream now (`POST /timers/{id}/signal/stop`) — issue #355.
 *
 * The lease {@link timerSignal} renews already guarantees the stream stops on its own; this makes
 * it stop *promptly*, the moment the RD closes the Tune view, instead of seconds later with the
 * timer still parsing telemetry nobody is reading. Idempotent, and harmless on a timer that was
 * never streaming. RD-gated; an unknown id answers **404**.
 */
export async function stopTimerSignal(
  baseUrl: string,
  id: TimerId,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(
    `${trimSlash(baseUrl)}/timers/${encodeURIComponent(id)}/signal/stop`,
    { method: 'POST', headers }
  );
  if (!resp.ok) throw await requestFailed(resp, 'stop the signal feed');
}

/**
 * Set a node's enter/exit detection thresholds (`POST /timers/{id}/calibration`) — issue #355.
 *
 * The write half of the Tune page. RD-gated exactly like {@link restartTimer}, so a token-gated
 * Director answers **401** without one — which is a different failure from "the timer refused" and
 * has to reach the RD as one.
 *
 * **The response is deliberately not read.** RotorHazard does not echo a level set synchronously;
 * it broadcasts `enter_and_exit_at_levels`, which surfaces as `NodeSignal.enter_at` / `exit_at` on
 * a later `GET /timers/{id}/signal`. The route answers with what it *dispatched*, which is not a
 * readback and must never be treated as one — so nothing here consumes it, and a resolved promise
 * means *accepted*, never *applied*. The caller confirms by polling the feed it is already reading.
 *
 * The Director **refuses** (a **400**) for a Mock, a timer that is not connected, or a node the
 * timer does not have; an unknown id answers **404**. Rejects on any non-2xx / transport failure.
 */
export async function setCalibration(
  baseUrl: string,
  id: TimerId,
  request: CalibrationRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = {
    Accept: 'application/json',
    'content-type': 'application/json'
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(
    `${trimSlash(baseUrl)}/timers/${encodeURIComponent(id)}/calibration`,
    { method: 'POST', headers, body: JSON.stringify(request) }
  );
  if (!resp.ok) throw await requestFailed(resp, 'save the calibration');
}

/**
 * Start a **capture** of one node's thresholds (`POST /timers/{id}/capture`) — issues #355, #465.
 *
 * The Tune page's third write, and the only one that does not carry a number: RotorHazard measures
 * the levels instead of being told them. That is the honest bootstrap for a timer nobody has ever
 * tuned — GridFPV ships no fabricated default because the right level depends on craft, VTX power,
 * antenna and gate geometry (#411), so the only non-guessing starting point is the RD's own craft
 * flown through their own gate.
 *
 * **One request, one pass, both thresholds** (#465). The body names only the node. The Director runs
 * both of RotorHazard's captures over the single pass — enter while the craft is at the gate, exit
 * once it has cleared — sequenced `exit_delay_ms` apart, because RotorHazard averages both capture
 * branches off the same samples and a simultaneous pair returns exit == enter.
 *
 * **The window starts now.** RotorHazard samples for `window_ms` (3000 on every version we support)
 * from the moment each emit lands and averages what it sees — it does not look back at a lap already
 * flown, and it does not take the peak. The dispatch is returned (unlike {@link setCalibration}'s)
 * precisely so the caller can count those windows down and tell the RD to fly *now*, and then to
 * stay clear.
 *
 * **The response is a dispatch, not a readback** — one step stronger than {@link setCalibration}'s,
 * because neither level exists yet when this resolves. The captured levels arrive as
 * `NodeSignal.enter_at` / `exit_at` on a later `GET /timers/{id}/signal`, each after its own window.
 * A level that never comes back did not land, and must be reported as such rather than shown as a
 * success: RotorHazard refuses a capture (a node not answering, one already capturing) in complete
 * silence. The two halves settle independently, so one can land and the other not.
 *
 * RD-gated exactly like {@link setCalibration}, so a token-gated Director answers **401** without
 * one. The Director **refuses** (a **400**) for a Mock, a timer that is not connected, a node the
 * timer does not have or the RD has disabled, a scored heat in progress, or a capture already
 * running on that node; an unknown id answers **404**.
 */
export async function captureLevel(
  baseUrl: string,
  id: TimerId,
  request: CaptureRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<CaptureDispatch> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = {
    Accept: 'application/json',
    'content-type': 'application/json'
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/timers/${encodeURIComponent(id)}/capture`, {
    method: 'POST',
    headers,
    body: JSON.stringify(request)
  });
  if (!resp.ok) throw await requestFailed(resp, 'start the capture');
  return (await resp.json()) as CaptureDispatch;
}

/**
 * Read a timer's **node set** (`GET /timers/{id}/nodes`) — issue #412.
 *
 * What the timer reported, what GridFPV is configured for, the effective `width`, every node with
 * its **1-based display label** and enabled flag, the `enabled` indices in seat order, and any
 * `drift` between the two. This is the shared answer to "which gates exist, and which may be
 * used?" — a console that re-derives it from `Timer.node_count` / `disabled_nodes` is one
 * off-by-one away from offering a node the hardware does not have.
 *
 * An **open read** (no token needed; it is the same information `GET /timers` already carries,
 * resolved). An unknown id answers **404**.
 */
export async function timerNodes(
  baseUrl: string,
  id: TimerId,
  options: { token?: string; fetch?: FetchLike; signal?: AbortSignal } = {}
): Promise<TimerNodes> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (options.token) headers.Authorization = `Bearer ${options.token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/timers/${encodeURIComponent(id)}/nodes`, {
    headers,
    signal: options.signal
  });
  if (!resp.ok) throw await requestFailed(resp, 'read the timer nodes');
  return (await resp.json()) as TimerNodes;
}

/**
 * Write a timer's node configuration (`PUT /timers/{id}/nodes`), answering with the resulting view.
 *
 * RD-gated. The Director **refuses** (a **400**, whose message is already phrased for the RD) a
 * `node_count` of `0` and any edit that would leave no node enabled — both cap every heat to no
 * pilots. Those refusals are surfaced verbatim rather than as an HTTP line, because they say the
 * useful thing ("at least one node must stay enabled").
 */
export async function setTimerNodes(
  baseUrl: string,
  id: TimerId,
  request: SetTimerNodesRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<TimerNodes> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = {
    Accept: 'application/json',
    'content-type': 'application/json'
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/timers/${encodeURIComponent(id)}/nodes`, {
    method: 'PUT',
    headers,
    body: JSON.stringify(request)
  });
  if (!resp.ok) throw await requestFailed(resp, 'save the node change');
  return (await resp.json()) as TimerNodes;
}

/**
 * Set one node's **channel** (`POST /timers/{id}/channel`) — issue #413.
 *
 * The Tune page's other write: a gate cannot be tuned meaningfully until its node is listening on
 * the channel it will race. RD-gated exactly like {@link setCalibration}, so a token-gated Director
 * answers **401** without one.
 *
 * **Send the band and channel, not just the MHz.** RotorHazard's `on_set_frequency` stores the
 * label on its active profile when it is given, and the RD validates a channel change *by
 * refreshing RotorHazard's own page* — where a bare frequency with no `R7` beside it reads as "it
 * half worked". The Director validates the label against its own catalog, so an invented band name
 * never reaches the timer.
 *
 * **The response is a dispatch, not a readback** (same rule as {@link setCalibration}): a resolved
 * promise means *accepted*, never *applied*. The confirmation is the next `GET /timers/{id}/signal`
 * showing `NodeSignal.frequency_mhz` holding what was sent — RotorHazard's heartbeat carries it, so
 * the feed the caller is already polling is the confirmation. The `ChannelDispatch` body *is* worth
 * reading for one thing the caller cannot know: whether the node's stored thresholds were tuned on
 * a different channel.
 *
 * The Director **refuses** (a **400**) for a Mock, a timer that is not connected, a **scored** heat
 * running on it (open practice is allowed), a node beyond the timer's width or one the RD has
 * disabled, and a frequency a Fixed timer cannot tune to; an unknown id answers **404**.
 */
export async function setNodeChannel(
  baseUrl: string,
  id: TimerId,
  request: ChannelRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<ChannelDispatch> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = {
    Accept: 'application/json',
    'content-type': 'application/json'
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/timers/${encodeURIComponent(id)}/channel`, {
    method: 'POST',
    headers,
    body: JSON.stringify(request)
  });
  if (!resp.ok) throw await requestFailed(resp, 'set the node channel');
  return (await resp.json()) as ChannelDispatch;
}

/** The shared body of {@link connectTimer} / {@link disconnectTimer} — same shape, same errors. */
async function setTimerConnection(
  baseUrl: string,
  id: TimerId,
  action: 'connect' | 'disconnect',
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<Timer> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/timers/${encodeURIComponent(id)}/${action}`, {
    method: 'POST',
    headers
  });
  if (!resp.ok)
    throw await requestFailed(
      resp,
      action === 'connect' ? 'connect to that timer' : 'disconnect from that timer'
    );
  return (await resp.json()) as Timer;
}

/**
 * Delete a timer (`DELETE /timers/{id}`) — issue #73. RD-gated. The built-in **Mock cannot be
 * deleted** (a **400**); an unknown id answers **404**. Resolves once the delete succeeds, or
 * rejects on a non-2xx / transport failure (a {@link RequestFailure} carrying the status).
 */
export async function deleteTimer(
  baseUrl: string,
  id: TimerId,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/timers/${encodeURIComponent(id)}`, {
    method: 'DELETE',
    headers
  });
  if (!resp.ok) throw await requestFailed(resp, 'delete the timer');
}

/**
 * Set an event's **selected timers** (`PUT /events/{id}/timers`) — issue #73. The per-event
 * reference into the app-level timer registry: an event selects which timers it uses. RD-gated;
 * the server validates the event exists and that **each** id names a known timer (else **404**).
 * Resolves to the updated event {@link EventMeta}, or rejects on a non-2xx / transport failure.
 */
export async function setEventTimers(
  baseUrl: string,
  eventId: EventId,
  ids: TimerId[],
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<EventMeta> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'application/json'
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}${eventRoot(eventId)}/timers`, {
    method: 'PUT',
    headers,
    body: JSON.stringify({ ids })
  });
  if (!resp.ok) throw await requestFailed(resp, 'save the event timers');
  return (await resp.json()) as EventMeta;
}

/**
 * Designate an event's **primary** timer (`PUT /events/{id}/primary-timer`) — issue #112. Among
 * the event's selected timers, exactly one is the primary (it feeds the race); the rest are
 * **alternates** (hot standby). Pass `id` to make that timer primary (it must be one of the
 * event's currently-selected timers, else **400**); pass `null` to clear the override so the
 * **first** selected timer becomes the effective primary. RD-gated. Resolves to the updated event
 * {@link EventMeta}, or rejects on a non-2xx / transport failure.
 */
export async function setPrimaryTimer(
  baseUrl: string,
  eventId: EventId,
  id: TimerId | null,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<EventMeta> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'application/json'
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}${eventRoot(eventId)}/primary-timer`, {
    method: 'PUT',
    headers,
    body: JSON.stringify({ id })
  });
  if (!resp.ok) throw await requestFailed(resp, 'set the primary timer');
  return (await resp.json()) as EventMeta;
}

/**
 * List every pilot in the directory (`GET /pilots`) — issue #74. Pilots are **application-level
 * configuration**: the RD maintains a directory once (a persisted address book) and each event
 * rosters which pilots race it. Reads are open on the LAN (no token needed; an optional token is
 * sent when present). Resolves to the {@link Pilot}s (in id order), or rejects on a transport/HTTP
 * failure.
 */
export async function listPilots(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<Pilot[]> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (options.token) headers.Authorization = `Bearer ${options.token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/pilots`, { headers });
  if (!resp.ok) throw await requestFailed(resp, 'list the pilots');
  return (await resp.json()) as Pilot[];
}

/**
 * Create a pilot (`POST /pilots`) — issue #74. RD-gated (full-trust by default). The body's
 * `callsign` is required; everything else (`name`, `vtx_types`, `multigp_id`, `velocidrone_id`) is
 * optional. The id is auto-generated server-side. Resolves to the new {@link Pilot}, or rejects on
 * a non-2xx / transport failure (a missing/blank callsign is a **400**).
 */
export async function createPilot(
  baseUrl: string,
  request: CreatePilotRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<Pilot> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'application/json'
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/pilots`, {
    method: 'POST',
    headers,
    body: JSON.stringify(request)
  });
  if (!resp.ok) throw await requestFailed(resp, 'add the pilot');
  return (await resp.json()) as Pilot;
}

/**
 * Edit a pilot (`PUT /pilots/{id}`) — issue #74. RD-gated. Every field is optional (a partial
 * edit); for the optional metadata, omit a field to leave it unchanged, or pass `null` to clear it.
 * An unknown id answers **404**. Resolves to the updated {@link Pilot}, or rejects on a non-2xx /
 * transport failure.
 */
export async function updatePilot(
  baseUrl: string,
  id: PilotId,
  request: UpdatePilotRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<Pilot> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'application/json'
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/pilots/${encodeURIComponent(id)}`, {
    method: 'PUT',
    headers,
    body: JSON.stringify(request)
  });
  if (!resp.ok) throw await requestFailed(resp, 'save the pilot');
  return (await resp.json()) as Pilot;
}

/**
 * Delete a pilot (`DELETE /pilots/{id}`) — issue #74. RD-gated. An unknown id answers **404**.
 * Resolves once the delete succeeds, or rejects on a non-2xx / transport failure (the HTTP status
 * is in the message). A stale roster id on some event is harmless (rosters tolerate an unknown id).
 */
export async function deletePilot(
  baseUrl: string,
  id: PilotId,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/pilots/${encodeURIComponent(id)}`, {
    method: 'DELETE',
    headers
  });
  if (!resp.ok) throw await requestFailed(resp, 'delete the pilot');
}

/**
 * Set an event's **roster** (`PUT /events/{id}/roster`) — issue #74. The per-event reference into
 * the app-level pilot directory: an event rosters which directory pilots race it. RD-gated; the
 * server validates the event exists and that **each** id names a known directory pilot (else
 * **404**). Resolves to the updated event {@link EventMeta}, or rejects on a non-2xx / transport
 * failure.
 */
export async function setEventRoster(
  baseUrl: string,
  eventId: EventId,
  pilotIds: PilotId[],
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<EventMeta> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'application/json'
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}${eventRoot(eventId)}/roster`, {
    method: 'PUT',
    headers,
    body: JSON.stringify({ pilot_ids: pilotIds })
  });
  if (!resp.ok) throw await requestFailed(resp, 'save the roster');
  return (await resp.json()) as EventMeta;
}

/**
 * Add one pilot to an event's roster (`POST /events/{id}/roster/{pilotId}`) — issue #74. RD-gated;
 * the event must exist and the pilot must name a known directory pilot (else **404**). Idempotent.
 * Resolves to the updated event {@link EventMeta}, or rejects on a non-2xx / transport failure.
 */
export async function addToRoster(
  baseUrl: string,
  eventId: EventId,
  pilotId: PilotId,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<EventMeta> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(
    `${trimSlash(baseUrl)}${eventRoot(eventId)}/roster/${encodeURIComponent(pilotId)}`,
    { method: 'POST', headers }
  );
  if (!resp.ok) throw await requestFailed(resp, 'add that pilot to the roster');
  return (await resp.json()) as EventMeta;
}

/**
 * Remove one pilot from an event's roster (`DELETE /events/{id}/roster/{pilotId}`) — issue #74.
 * RD-gated; the event must exist (else **404**). Removing a pilot not on the roster is a no-op.
 * Resolves to the updated event {@link EventMeta}, or rejects on a non-2xx / transport failure.
 */
export async function removeFromRoster(
  baseUrl: string,
  eventId: EventId,
  pilotId: PilotId,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<EventMeta> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(
    `${trimSlash(baseUrl)}${eventRoot(eventId)}/roster/${encodeURIComponent(pilotId)}`,
    { method: 'DELETE', headers }
  );
  if (!resp.ok) throw await requestFailed(resp, 'remove that pilot from the roster');
  return (await resp.json()) as EventMeta;
}

/**
 * List every class in the application-level directory (`GET /classes`) — issue #84. An open read
 * (no token), mirroring `GET /pilots`: classes are app-level configuration the RD maintains once
 * and each event selects from. Resolves to the classes in id order, or rejects on a non-2xx /
 * transport failure.
 */
export async function listClasses(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<Class[]> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (options.token) headers.Authorization = `Bearer ${options.token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/classes`, { headers });
  if (!resp.ok) throw await requestFailed(resp, 'list the classes');
  return (await resp.json()) as Class[];
}

/**
 * Create a class (`POST /classes`) — issue #84. RD-gated (full-trust by default). The body's
 * `name` is required; everything else (`source`, `reference`, `description`) is optional. The id is
 * auto-generated server-side. Resolves to the new {@link Class}, or rejects on a non-2xx / transport
 * failure (a missing/blank name is a **400**).
 */
export async function createClass(
  baseUrl: string,
  request: CreateClassRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<Class> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'application/json'
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/classes`, {
    method: 'POST',
    headers,
    body: JSON.stringify(request)
  });
  if (!resp.ok) throw await requestFailed(resp, 'add the class');
  return (await resp.json()) as Class;
}

/**
 * Edit a class (`PUT /classes/{id}`) — issue #84. RD-gated. Every field is optional (a partial
 * edit); a present `name`/`source` replaces it, and for the optional metadata (`reference`,
 * `description`), omit a field to leave it unchanged, or pass `null` to clear it. An unknown id
 * answers **404**. Resolves to the updated {@link Class}, or rejects on a non-2xx / transport
 * failure.
 */
export async function updateClass(
  baseUrl: string,
  id: ClassId,
  request: UpdateClassRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<Class> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'application/json'
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/classes/${encodeURIComponent(id)}`, {
    method: 'PUT',
    headers,
    body: JSON.stringify(request)
  });
  if (!resp.ok) throw await requestFailed(resp, 'save the class');
  return (await resp.json()) as Class;
}

/**
 * Delete a class (`DELETE /classes/{id}`) — issue #84. RD-gated. An unknown id answers **404**.
 * Resolves once the delete succeeds, or rejects on a non-2xx / transport failure (the HTTP status
 * is in the message). A stale selection id on some event is harmless (selections tolerate an
 * unknown id).
 */
export async function deleteClass(
  baseUrl: string,
  id: ClassId,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/classes/${encodeURIComponent(id)}`, {
    method: 'DELETE',
    headers
  });
  if (!resp.ok) throw await requestFailed(resp, 'delete the class');
}

/**
 * Hide or un-hide a class (`PUT /classes/{id}/hidden`) — hide/archive classes. RD-gated. The body is
 * `{ hidden }`: `true` archives the class from the per-event class picker, `false` brings it back.
 * Hiding is a **visibility preference**, not an edit — so it is valid for **built-in** classes too
 * (never a read-only rejection): the class stays in the directory and the main Classes view, it is
 * just filtered out of the picker. The choice is persisted server-side and survives a restart
 * (including the built-in re-seed). An unknown id answers **404**. Resolves to the updated
 * {@link Class} (with its fresh `hidden` flag), or rejects on a non-2xx / transport failure.
 */
export async function setClassHidden(
  baseUrl: string,
  id: ClassId,
  hidden: boolean,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<Class> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'application/json'
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const body: SetClassHiddenRequest = { hidden };
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/classes/${encodeURIComponent(id)}/hidden`, {
    method: 'PUT',
    headers,
    body: JSON.stringify(body)
  });
  if (!resp.ok) throw await requestFailed(resp, 'change the class visibility');
  return (await resp.json()) as Class;
}

/**
 * Set an event's **class selection** (`PUT /events/{id}/classes`) — issue #84. The per-event
 * reference into the app-level class directory: an event runs which directory classes it selects.
 * RD-gated; the server validates the event exists and that **each** id names a known directory class
 * (else **404**). Mirrors {@link setEventTimers} (a wholesale set with per-id validation). Resolves
 * to the updated event {@link EventMeta}, or rejects on a non-2xx / transport failure.
 */
export async function setEventClasses(
  baseUrl: string,
  eventId: EventId,
  ids: ClassId[],
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<EventMeta> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'application/json'
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}${eventRoot(eventId)}/classes`, {
    method: 'PUT',
    headers,
    body: JSON.stringify({ ids })
  });
  if (!resp.ok) throw await requestFailed(resp, 'save the event classes');
  return (await resp.json()) as EventMeta;
}

/**
 * Set which roster pilots race a single class (`PUT /events/{id}/classes/{classId}/membership`) —
 * race redesign Slice 1a / 7b. Replaces *that class's* member list wholesale (an empty list clears
 * it); other classes' memberships are untouched. RD-gated; the server validates the event exists,
 * the class names a known directory class, **each** pilot id names a known directory pilot, and each
 * set channel is one of the event's **primary timer**'s `available_channels` (else **404 / 400**).
 *
 * `members` may be plain pilot ids (a channel-less membership, the legacy wire shape) or full
 * {@link MemberSlot}s (`{ pilot, channel? }`) — the Classes & Roster picker passes slots so each
 * member carries the fixed channel they fly in a *static*-channel-mode round. Resolves to the
 * updated event {@link EventMeta}, or rejects on a non-2xx / transport failure.
 */
export async function setClassMembership(
  baseUrl: string,
  eventId: EventId,
  classId: ClassId,
  members: (PilotId | MemberSlot)[],
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<EventMeta> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'application/json'
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(
    `${trimSlash(baseUrl)}${eventRoot(eventId)}/classes/${encodeURIComponent(classId)}/membership`,
    {
      method: 'PUT',
      headers,
      // The wire shape carries member **slots** (`{ pilot, channel? }`) — race redesign Slice 7a.
      // The server accepts a bare pilot-id element too (legacy shim), so a plain id here sets a
      // channel-less slot while a `MemberSlot` carries the pilot's fixed channel (Slice 7b).
      body: JSON.stringify({ pilots: members })
    }
  );
  if (!resp.ok) throw await requestFailed(resp, 'save the class membership');
  return (await resp.json()) as EventMeta;
}

/**
 * List the valid **formats + their param schemas** (`GET /formats`) — race redesign Slice 2b / 7a.
 * An open read (no token): each production format (`FormatRegistry::standard()`) with the param
 * schema its generator reads (`{ name, params: [{ key, label, kind, options?, default? }] }`) — the
 * single source of truth the Rounds UI reads for the format dropdown and a per-format params editor.
 * Resolves to the schemas in sorted name order, or rejects on a non-2xx / transport failure.
 */
export async function listFormatSchemas(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<FormatSchema[]> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (options.token) headers.Authorization = `Bearer ${options.token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/formats`, { headers });
  if (!resp.ok) throw await requestFailed(resp, 'list the formats');
  return (await resp.json()) as FormatSchema[];
}

/**
 * List the valid **format names** (`GET /formats`) — the names-only convenience over
 * {@link listFormatSchemas} the Rounds UI's format dropdown reads. Resolves to the schema names in
 * sorted order, or rejects on a non-2xx / transport failure. The full per-format param schema (for
 * a params editor) is {@link listFormatSchemas}.
 */
export async function listFormats(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<string[]> {
  const schemas = await listFormatSchemas(baseUrl, options);
  return schemas.map((s) => s.name);
}

/**
 * List the standard **FPV channel catalog** (`GET /channels`) — race redesign Slice 4b. An open
 * read (no token): the band/channel ↔ raw-MHz vocabulary the server compiles in, the single source
 * of truth the Channels UI offers when picking a timer's available channels and reads back to label
 * a heat's assigned frequencies. Resolves to the catalog in its stable order, or rejects on a
 * non-2xx / transport failure.
 */
export async function listChannels(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<ChannelCatalogEntry[]> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (options.token) headers.Authorization = `Bearer ${options.token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/channels`, { headers });
  if (!resp.ok) throw await requestFailed(resp, 'list the channels');
  return (await resp.json()) as ChannelCatalogEntry[];
}

/**
 * Rate a candidate **channel set** (`GET /channels/imd?channels=…`) — #117 S4. An open read (no
 * token): IMDTabler's 0–100 rating for those channels flown together, plus the worst offending
 * two-tone mixing product — or no offender at all, when nothing lands within 35 MHz of a channel
 * somebody is flying.
 *
 * The Director owns the metric, and this is the **only** implementation of it in the system. That
 * is #430's whole point: an RD must read the same number off GridFPV that they read off
 * RotorHazard for the same channels, and a second port of the algorithm in the console is exactly
 * how that stops being true. Pure over its query — no event, no timer, no state — so it is safe to
 * call as fast as an RD can tick a dropdown.
 *
 * Order and repeats do not matter (a set is a set). Resolves to the `ImdReading`, or rejects on a
 * non-2xx / transport failure.
 */
export async function rateChannels(
  baseUrl: string,
  channels: readonly number[],
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<ImdReading> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (options.token) headers.Authorization = `Bearer ${options.token}`;
  const query = encodeURIComponent(channels.join(','));
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/channels/imd?channels=${query}`, { headers });
  if (!resp.ok) throw await requestFailed(resp, 'rate those channels');
  return (await resp.json()) as ImdReading;
}

/**
 * Add a **round** to an event (`POST /events/{id}/rounds`) — race redesign Slice 2b. RD-gated; the
 * round id is auto-generated server-side. The server validates each class is selected by the event,
 * the `format` is a known {@link listFormats} name, and a `FromRanking` seeding source names an
 * existing round (else **400**); an unknown event is **404**. Resolves to the created
 * {@link RoundDef} (with its generated id), or rejects on a non-2xx / transport failure.
 */
export async function createRound(
  baseUrl: string,
  eventId: EventId,
  request: NewRoundReq,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<RoundDef> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'application/json'
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}${eventRoot(eventId)}/rounds`, {
    method: 'POST',
    headers,
    body: JSON.stringify(request)
  });
  if (!resp.ok) throw await requestFailed(resp, 'add the round');
  return (await resp.json()) as RoundDef;
}

/**
 * Replace an existing **round**'s fields (`PUT /events/{id}/rounds/{round}`) — race redesign Slice
 * 2b. RD-gated; the round id is the path segment (not editable) and every other field is replaced
 * wholesale. Same validation as {@link createRound} (bad class / format / seeding → **400**); an
 * unknown event or round id is **404**. Resolves to the updated {@link RoundDef}, or rejects on a
 * non-2xx / transport failure.
 */
export async function updateRound(
  baseUrl: string,
  eventId: EventId,
  roundId: RoundId,
  request: UpdateRoundReq,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<RoundDef> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'application/json'
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(
    `${trimSlash(baseUrl)}${eventRoot(eventId)}/rounds/${encodeURIComponent(roundId)}`,
    {
      method: 'PUT',
      headers,
      body: JSON.stringify(request)
    }
  );
  if (!resp.ok) throw await requestFailed(resp, 'save the round');
  return (await resp.json()) as RoundDef;
}

/**
 * Remove a **round** from an event (`DELETE /events/{id}/rounds/{round}`) — race redesign Slice 2b.
 * RD-gated; an unknown event or round id is **404**. Resolves to the event's updated
 * {@link EventMeta}, or rejects on a non-2xx / transport failure.
 */
export async function deleteRound(
  baseUrl: string,
  eventId: EventId,
  roundId: RoundId,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<EventMeta> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(
    `${trimSlash(baseUrl)}${eventRoot(eventId)}/rounds/${encodeURIComponent(roundId)}`,
    {
      method: 'DELETE',
      headers
    }
  );
  if (!resp.ok) throw await requestFailed(resp, 'remove the round');
  return (await resp.json()) as EventMeta;
}

// ── Event channel layouts (#117 S2) ──────────────────────────────────────────────────────────────
//
// A **layout** is one complete tuning of the event's timer — one channel per enabled node, drawn
// from the timer's *allowed* set. Layouts are **event** state: they live on the event's meta beside
// `timers` / `roster` / `classes`, so editing one never touches the global timer record (which is
// what the Timers-page checkboxes do, and the bug this slice closes).
//
// Every write answers with the whole {@link ChannelLayouts} view — the layouts *and* the advisory
// cross-layout `overlaps` — not just the layout that changed, because an overlap is a property of the
// set. The console renders what the Director computed rather than re-deriving the rule.

/**
 * List an event's **channel layouts** (`GET /events/{id}/layouts`) — #117 S2. A read (open, no token):
 * the layouts in definition order plus the advisory `overlaps` between them. Resolves the
 * {@link ChannelLayouts} view, or rejects on a non-2xx / transport failure; an unknown event is 404.
 */
export async function listChannelLayouts(
  baseUrl: string,
  eventId: EventId,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<ChannelLayouts> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (options.token) headers.Authorization = `Bearer ${options.token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}${eventRoot(eventId)}/layouts`, { headers });
  if (!resp.ok) throw await requestFailed(resp, 'list the channel layouts');
  return (await resp.json()) as ChannelLayouts;
}

/**
 * Define a **channel layout** on an event (`POST /events/{id}/layouts`) — #117 S2. RD-gated; the layout
 * id is generated server-side (never in the body).
 *
 * **Omitting `nodes` seeds the layout from the timer's allowed set** — the global→event seam: what
 * the RD ticked on the Timers page is the default an event starts from, and from here on the layout
 * is event-local. A tuning that puts two nodes on one channel, names a channel the timer is not
 * allowed to use, names a disabled/out-of-range node, or leaves an enabled node untuned is a
 * **400** whose message is thrown verbatim (it is already written for the RD). Cross-layout channel
 * reuse is **not** a refusal — it comes back in `overlaps` on a 200. Resolves the whole updated
 * {@link ChannelLayouts} view.
 */
export async function createChannelLayout(
  baseUrl: string,
  eventId: EventId,
  request: NewChannelLayoutRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<ChannelLayouts> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'application/json'
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}${eventRoot(eventId)}/layouts`, {
    method: 'POST',
    headers,
    body: JSON.stringify(request)
  });
  if (!resp.ok) throw await requestFailed(resp, 'add the channel layout');
  return (await resp.json()) as ChannelLayouts;
}

/**
 * Replace a **channel layout**'s name and mapping (`PUT /events/{id}/layouts/{layout}`) — #117 S2.
 * RD-gated; the layout id is the path segment (not editable) and the name plus the whole node →
 * channel mapping are replaced wholesale, re-validated exactly as on create. An unknown event or
 * layout is **404**; an invalid tuning is a **400** thrown verbatim. Resolves the whole updated
 * {@link ChannelLayouts} view.
 */
export async function updateChannelLayout(
  baseUrl: string,
  eventId: EventId,
  layoutId: LayoutId,
  request: SetChannelLayoutRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<ChannelLayouts> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'application/json'
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(
    `${trimSlash(baseUrl)}${eventRoot(eventId)}/layouts/${encodeURIComponent(layoutId)}`,
    { method: 'PUT', headers, body: JSON.stringify(request) }
  );
  if (!resp.ok) throw await requestFailed(resp, 'save the channel layout');
  return (await resp.json()) as ChannelLayouts;
}

/**
 * Remove a **channel layout** (`DELETE /events/{id}/layouts/{layout}`) — #117 S2. RD-gated; an unknown
 * event or layout is **404** (not a silent success). Resolves the whole updated
 * {@link ChannelLayouts} view.
 */
export async function deleteChannelLayout(
  baseUrl: string,
  eventId: EventId,
  layoutId: LayoutId,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<ChannelLayouts> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (token) headers.Authorization = `Bearer ${token}`;
  const resp = await fetchImpl(
    `${trimSlash(baseUrl)}${eventRoot(eventId)}/layouts/${encodeURIComponent(layoutId)}`,
    { method: 'DELETE', headers }
  );
  if (!resp.ok) throw await requestFailed(resp, 'delete the channel layout');
  return (await resp.json()) as ChannelLayouts;
}

/**
 * List an event's **scheduled heats** (`GET /events/{id}/heats`) — race redesign Slice 3b. A read
 * (open, no token): the server folds the event log into one {@link HeatSummary} per scheduled heat —
 * id, lineup, the round/class it was tagged with, its derived phase, and whether it is the current
 * heat — in first-scheduled order. The Heats UI groups this by round to render each round's heats
 * list. Resolves the list, or rejects on a non-2xx / transport failure; an unknown event is a 404.
 */
export async function listHeats(
  baseUrl: string,
  eventId: EventId,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<HeatSummary[]> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (options.token) headers.Authorization = `Bearer ${options.token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}${eventRoot(eventId)}/heats`, { headers });
  if (!resp.ok) throw await requestFailed(resp, 'list the heats');
  return (await resp.json()) as HeatSummary[];
}

/**
 * List an event's **round issues** (`GET /events/{id}/round-issues`) — #416. A read (open, no
 * token): every stored round whose open-practice seating names a node that cannot record a lap —
 * one beyond the primary timer's width, one the RD has disabled, or one beyond what the timer
 * reported.
 *
 * #412 refuses an impossible seat when a round is *written*; this is the same rule applied to what
 * is already stored, because the rounds already on disk are the ones that predate the fix. Each
 * entry carries the round's label, the timer's name, the 1-based node label and the RD-facing
 * sentence, so the console renders the server's explanation rather than re-deriving one.
 *
 * An **empty list means nothing is wrong** — including for an event with no resolvable primary
 * timer, which has no node set to check against. Rejects on a non-2xx / transport failure; an
 * unknown event is a 404.
 */
export async function listRoundIssues(
  baseUrl: string,
  eventId: EventId,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<RoundIssue[]> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (options.token) headers.Authorization = `Bearer ${options.token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}${eventRoot(eventId)}/round-issues`, {
    headers
  });
  if (!resp.ok) throw await requestFailed(resp, 'check the rounds for issues');
  return (await resp.json()) as RoundIssue[];
}

/**
 * Read an event's **event-wide audit trail** (`GET /events/{id}/audit`) — the "defensible
 * results" review surface. A read (open, no token): every heat's marshaling audit fold,
 * heat-tagged ({@link EventAuditEntry} = the per-heat `AuditEntry` fields plus `heat`) and merged
 * **newest first** across the whole event — what the console's Audit page renders and filters.
 * Resolves the list, or rejects on a non-2xx / transport failure; an unknown event is a 404.
 */
export async function eventAudit(
  baseUrl: string,
  eventId: EventId,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<EventAuditEntry[]> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (options.token) headers.Authorization = `Bearer ${options.token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}${eventRoot(eventId)}/audit`, { headers });
  if (!resp.ok) throw await requestFailed(resp, 'read the event audit log');
  return (await resp.json()) as EventAuditEntry[];
}

/**
 * Read a round's **ranking** (`GET /events/{id}/rounds/{round}/ranking`) — race redesign Slice 5/6a.
 * A read (open, no token): the ordered per-pilot {@link RankEntry} list the engine seeds
 * `FromRanking` from — the same provisional-or-final ordering a bracket carries — for the
 * bracket-carry display. Resolves the ranking (best first), or rejects on a non-2xx / transport
 * failure; an unknown event or round is a 404, an unscorable round a 400.
 */
export async function roundRanking(
  baseUrl: string,
  eventId: EventId,
  roundId: RoundId,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<RankEntry[]> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (options.token) headers.Authorization = `Bearer ${options.token}`;
  const resp = await fetchImpl(
    `${trimSlash(baseUrl)}${eventRoot(eventId)}/rounds/${encodeURIComponent(roundId)}/ranking`,
    { headers }
  );
  if (!resp.ok) throw await requestFailed(resp, 'read the round ranking');
  return (await resp.json()) as RankEntry[];
}

/**
 * Read a round's **standings** (`GET /events/{id}/rounds/{round}/standings`) — the time-trial / qual
 * display. A read (open, no token): one {@link RoundStanding} per pilot for a single round — each
 * pilot's best single lap plus the win-condition metric they're ranked on (best-N-consecutive time,
 * lap count, or best lap), in the same order (and with the same positions) as {@link roundRanking}.
 * Resolves the standings (best first), or rejects on a non-2xx / transport failure; an unknown event
 * or round is a 404, an unscorable round a 400.
 */
export async function roundStandings(
  baseUrl: string,
  eventId: EventId,
  roundId: RoundId,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<RoundStanding[]> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (options.token) headers.Authorization = `Bearer ${options.token}`;
  const resp = await fetchImpl(
    `${trimSlash(baseUrl)}${eventRoot(eventId)}/rounds/${encodeURIComponent(roundId)}/standings`,
    { headers }
  );
  if (!resp.ok) throw await requestFailed(resp, 'read the round standings');
  return (await resp.json()) as RoundStanding[];
}

/**
 * Read a class's **standings** (`GET /events/{id}/classes/{class}/standings`) — race redesign
 * Slice 5/6a. A read (open, no token): the season-join {@link ClassStandings} the Results UI reads —
 * one per-pilot row per competitor that raced the class, aggregated across the class's rounds
 * (points, best lap, total laps), best standing first. Resolves the standings, or rejects on a
 * non-2xx / transport failure; an unknown event is a 404, an unscorable class round a 400. A class
 * with no rounds resolves to empty standings.
 */
export async function classStandings(
  baseUrl: string,
  eventId: EventId,
  classId: ClassId,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<ClassStandings> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (options.token) headers.Authorization = `Bearer ${options.token}`;
  const resp = await fetchImpl(
    `${trimSlash(baseUrl)}${eventRoot(eventId)}/classes/${encodeURIComponent(classId)}/standings`,
    { headers }
  );
  if (!resp.ok) throw await requestFailed(resp, 'read the class standings');
  return (await resp.json()) as ClassStandings;
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
  // The event this connection is rooted under (issue #72). Always explicit — see ConnectOptions.
  const eventId = options.eventId;

  // ── Mutable connection state ───────────────────────────────────────────────
  let body: ProjectionBody | undefined;
  // The resume cursor: a log offset (protocol.html §2/§3 "as built") used ONLY as the
  // `from:` resume point — it is not the stream's ordering counter. Seeded by each
  // snapshot and re-seeded from each applied envelope's own `cursor` (see
  // `applyEnvelope`), so a reconnect resumes EXACTLY where this client left off instead
  // of replaying the whole backlog from the snapshot's original offset.
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
  // Returns 'applied', 'duplicate' (already seen — idempotent no-op), 'gap'
  // (missed envelopes → caller must re-snapshot), or 'unsupported' (a delta this
  // client cannot fold → caller must re-snapshot, the same fail-safe as a
  // StaleCursor).
  function applyEnvelope(env: ChangeEnvelope): 'applied' | 'duplicate' | 'gap' | 'unsupported' {
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
    if (!('FreshValue' in change)) {
      // Delta. The per-projection delta encodings are deferred (#43): the wire
      // type carries an opaque payload this client cannot fold into `body`.
      // Advancing the sequence without the mutation would silently FREEZE the
      // view while `status` still reads 'live' — so an unhandled delta fails
      // safe instead: report it so the caller re-snapshots (always correct, §3),
      // exactly the path a StaleCursor takes. Once #43 pins the typed deltas,
      // fold them into `body` here per ProjectionKind and return 'applied'.
      return 'unsupported';
    }
    body = change.FreshValue;
    streamSeq = seq;
    // Re-seed the RESUME cursor from the envelope's own `cursor` — the log offset the
    // server folded this body through (#422). It is exact, so a reconnect resubscribes
    // from precisely the position this client is at and the server has nothing to replay.
    //
    // This used to be `cursor = (cursor ?? 0) + 1`: the wire echoed no offset, so the
    // client advanced one per APPLIED envelope and called it "conservative (at-or-behind
    // the true offset)". It was conservative and it was wrong — every append that moved
    // no projection (a SignalHistory chunk, a CompetitorSeen, a marshaling no-op) emitted
    // no envelope and widened the drift. A reconnect then resumed from `tail - drift`,
    // which is inside the server's retained window, so the stream REPLAYED: `body` was
    // overwritten with an older fold carrying fewer laps and then climbed back through
    // every intermediate one. Live lap counts stepped backwards on screen, mid-race,
    // looking exactly like a marshal voiding a pass. Nothing here may re-derive the
    // offset; it is the server's to state.
    //
    // Fallback: a Director too old to echo the field leaves `cursor` undefined here, so
    // keep the old lower-bound advance for it rather than losing the resume position
    // outright. Its stream still replays on reconnect — that is the bug this field fixes —
    // but the client degrades instead of re-snapshotting on every blip.
    cursor = typeof env.cursor === 'number' ? env.cursor : (cursor ?? 0) + 1;
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
      if (result === 'gap' || result === 'unsupported') {
        // Missed envelopes the stream can't replay, or a delta this client can't
        // fold (#43) → re-snapshot and re-subscribe (always correct, §3).
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
