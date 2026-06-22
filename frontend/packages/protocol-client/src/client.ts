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
  Class,
  ClassId,
  CreateClassRequest,
  CreateEventRequest,
  CreatePilotRequest,
  CreateTimerRequest,
  Cursor,
  EventId,
  EventMeta,
  HeatSummary,
  NewRoundReq,
  Pilot,
  PilotId,
  ProjectionBody,
  ProtocolError,
  RoundDef,
  RoundId,
  Scope,
  Snapshot,
  SubscribeRequest,
  Timer,
  TimerId,
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
  if (!resp.ok) throw new Error(`GET /timers failed: HTTP ${resp.status}`);
  return (await resp.json()) as Timer[];
}

/**
 * Create a timer (`POST /timers`) — issue #73. RD-gated (full-trust by default: an open Director
 * accepts it tokenless; a gated one answers **401/403** and the caller obtains a token and
 * retries). The body carries the display `name` plus the {@link CreateTimerRequest['kind']} config
 * (a `Sim` or a reserved `Rotorhazard`); the id is auto-generated server-side. Resolves to the new
 * {@link Timer}, or rejects on a non-2xx / transport failure (the HTTP status is in the message).
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
  if (!resp.ok) throw new Error(`POST /timers failed: HTTP ${resp.status}`);
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
  if (!resp.ok) throw new Error(`PUT /timers/${id} failed: HTTP ${resp.status}`);
  return (await resp.json()) as Timer;
}

/**
 * Delete a timer (`DELETE /timers/{id}`) — issue #73. RD-gated. The built-in **Mock cannot be
 * deleted** (a **400**); an unknown id answers **404**. Resolves once the delete succeeds, or
 * rejects on a non-2xx / transport failure (the HTTP status is in the message for branching).
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
  if (!resp.ok) throw new Error(`DELETE /timers/${id} failed: HTTP ${resp.status}`);
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
  if (!resp.ok) throw new Error(`PUT /events/${eventId}/timers failed: HTTP ${resp.status}`);
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
  if (!resp.ok) throw new Error(`PUT /events/${eventId}/primary-timer failed: HTTP ${resp.status}`);
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
  if (!resp.ok) throw new Error(`GET /pilots failed: HTTP ${resp.status}`);
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
  if (!resp.ok) throw new Error(`POST /pilots failed: HTTP ${resp.status}`);
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
  if (!resp.ok) throw new Error(`PUT /pilots/${id} failed: HTTP ${resp.status}`);
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
  if (!resp.ok) throw new Error(`DELETE /pilots/${id} failed: HTTP ${resp.status}`);
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
  if (!resp.ok) throw new Error(`PUT /events/${eventId}/roster failed: HTTP ${resp.status}`);
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
  if (!resp.ok)
    throw new Error(`POST /events/${eventId}/roster/${pilotId} failed: HTTP ${resp.status}`);
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
  if (!resp.ok)
    throw new Error(`DELETE /events/${eventId}/roster/${pilotId} failed: HTTP ${resp.status}`);
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
  if (!resp.ok) throw new Error(`GET /classes failed: HTTP ${resp.status}`);
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
  if (!resp.ok) throw new Error(`POST /classes failed: HTTP ${resp.status}`);
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
  if (!resp.ok) throw new Error(`PUT /classes/${id} failed: HTTP ${resp.status}`);
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
  if (!resp.ok) throw new Error(`DELETE /classes/${id} failed: HTTP ${resp.status}`);
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
  if (!resp.ok) throw new Error(`PUT /events/${eventId}/classes failed: HTTP ${resp.status}`);
  return (await resp.json()) as EventMeta;
}

/**
 * Set which roster pilots race a single class (`PUT /events/{id}/classes/{classId}/membership`) —
 * race redesign Slice 1a. Replaces *that class's* pilot list wholesale (an empty list clears it);
 * other classes' memberships are untouched. RD-gated; the server validates the event exists, the
 * class names a known directory class, and **each** pilot id names a known directory pilot (else
 * **404**). Resolves to the updated event {@link EventMeta}, or rejects on a non-2xx / transport
 * failure.
 */
export async function setClassMembership(
  baseUrl: string,
  eventId: EventId,
  classId: ClassId,
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
  const resp = await fetchImpl(
    `${trimSlash(baseUrl)}${eventRoot(eventId)}/classes/${encodeURIComponent(classId)}/membership`,
    {
      method: 'PUT',
      headers,
      body: JSON.stringify({ pilot_ids: pilotIds })
    }
  );
  if (!resp.ok)
    throw new Error(
      `PUT /events/${eventId}/classes/${classId}/membership failed: HTTP ${resp.status}`
    );
  return (await resp.json()) as EventMeta;
}

/**
 * List the valid **format names** (`GET /formats`) — race redesign Slice 2b. An open read (no
 * token): the production formats registered in the engine's `FormatRegistry::standard()`, the
 * single source of truth the Rounds UI's format dropdown reads. Resolves to the names in sorted
 * order, or rejects on a non-2xx / transport failure.
 */
export async function listFormats(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<string[]> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (options.token) headers.Authorization = `Bearer ${options.token}`;
  const resp = await fetchImpl(`${trimSlash(baseUrl)}/formats`, { headers });
  if (!resp.ok) throw new Error(`GET /formats failed: HTTP ${resp.status}`);
  return (await resp.json()) as string[];
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
  if (!resp.ok) throw new Error(`POST /events/${eventId}/rounds failed: HTTP ${resp.status}`);
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
  if (!resp.ok)
    throw new Error(`PUT /events/${eventId}/rounds/${roundId} failed: HTTP ${resp.status}`);
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
  if (!resp.ok)
    throw new Error(`DELETE /events/${eventId}/rounds/${roundId} failed: HTTP ${resp.status}`);
  return (await resp.json()) as EventMeta;
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
  if (!resp.ok) throw new Error(`GET /events/${eventId}/heats failed: HTTP ${resp.status}`);
  return (await resp.json()) as HeatSummary[];
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
