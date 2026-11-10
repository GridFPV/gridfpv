/**
 * The protocol client implementation: the plain HTTP calls the Director serves.
 *
 * Every call goes through one request path ({@link apiRequest}), so headers, the optional
 * bearer token and error handling are written once. A non-2xx answer rejects with a
 * {@link RequestFailure} that carries the Director's own sentence and the HTTP status.
 *
 * The competition is live through two calls. {@link getEventState} reads the whole event
 * snapshot, and {@link subscribeChanges} listens to the Director's change feed, which sends
 * only the log cursor. A console that hears a new cursor reads the snapshot again. There are
 * no deltas and nothing to resume, so the two cannot drift apart.
 */

import type {
  ActiveEvent,
  AdapterId,
  BracketRequest,
  Class,
  ClassId,
  Command,
  CommandOutcome,
  CreateClassRequest,
  CreateEventRequest,
  CreatePilotRequest,
  CreateTimerRequest,
  DiscoveredTimer,
  EventId,
  EventMeta,
  EventView,
  MainsRequest,
  PassRequest,
  Pilot,
  PilotId,
  ProtocolError,
  RaceId,
  RaceSignal,
  SeatLevels,
  SetClassHiddenRequest,
  SignalChunk,
  Timer,
  TimerConfig,
  TimerView,
  UpdateClassRequest,
  UpdatePilotRequest
} from '@gridfpv/types';

/** Minimal `fetch` surface this client needs (injectable for tests / Node). */
export type FetchLike = (input: string, init?: RequestInit) => Promise<Response>;

const isProtocolError = (v: unknown): v is ProtocolError =>
  typeof v === 'object' &&
  v !== null &&
  'code' in v &&
  'message' in v &&
  typeof (v as ProtocolError).message === 'string';

// ── Request failures: the Director's own words, and the status carried structurally ───────────
//
// Every non-2xx response in this file rejects through `requestFailed` (#433) — from `apiRequest`,
// the single call site every endpoint shares (#459). The alternative — each call site formatting
// its own `METHOD /path/{id} failed: HTTP 400` line — throws the Director's typed body away, which
// puts raw ids on screen (the repo display rule forbids exactly that) *and* discards the one
// sentence worth showing.

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
 * The Director's `ProtocolError` body is already phrased for the RD and names things by their
 * **friendly** names — deliberately, precisely so it can be shown:
 *
 * > a built-in class cannot be edited or deleted
 *
 * That is the message the RD can act on, so it is thrown as-is. Wrapping it in a route line would
 * both bury it and put the raw ids from the path in front of a user, which the repo display rule
 * forbids.
 *
 * Only a response with **no usable body** falls back, and the fallback says what was *attempted*
 * in words — `attempted` is an infinitive phrase like `'delete the pilot'` — never the method and
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

/**
 * The per-call knobs every endpoint in this file shares (#459).
 *
 * `token` is the caller's optional bearer credential — an open (unconfigured) Director needs none,
 * so every endpoint sends it only when held. `fetch` is the injection seam tests and Node use.
 * `body` is the value to serialise (its presence is what adds `Content-Type`), and `method` is
 * omitted entirely for a GET so the init object stays exactly what it was per endpoint.
 */
interface RequestOptions {
  /** The HTTP method; omitted for a GET. */
  method?: string;
  /** The value to send as a JSON body. Its **presence** is what sets `Content-Type`. */
  body?: unknown;
  /** Bearer token, sent as `Authorization: Bearer …` only when held. */
  token?: string;
  /** Inject a `fetch` (defaults to the global). Used by tests and Node. */
  fetch?: FetchLike;
}

/**
 * **The one request path.** Every endpoint below routes through here: it resolves the injected
 * `fetch`, builds the headers (Accept always; `Content-Type` when there is a body; `Authorization`
 * when a token is held), issues the call, and funnels **every** non-2xx through {@link requestFailed}
 * — which is what keeps the Director's typed refusal verbatim and the `status` structural (#433).
 *
 * One path means a cross-cutting change (a new header, say) is a one-site edit and no endpoint can
 * drift from the rest.
 *
 * Returns the raw {@link Response}: the endpoints whose 2xx carries **no content** (the DELETEs)
 * must not parse a body that isn't there.
 * Everything that answers with JSON goes through {@link apiJson}.
 */
async function apiRequest(
  baseUrl: string,
  path: string,
  options: RequestOptions,
  attempted: string
): Promise<Response> {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (options.body !== undefined) headers['Content-Type'] = 'application/json';
  if (options.token) headers.Authorization = `Bearer ${options.token}`;
  const init: RequestInit = { headers };
  if (options.method) init.method = options.method;
  if (options.body !== undefined) init.body = JSON.stringify(options.body);
  const resp = await fetchImpl(`${trimSlash(baseUrl)}${path}`, init);
  if (!resp.ok) throw await requestFailed(resp, attempted);
  return resp;
}

/** {@link apiRequest} plus the JSON parse — the shape all but a handful of endpoints want. */
async function apiJson<T>(
  baseUrl: string,
  path: string,
  options: RequestOptions,
  attempted: string
): Promise<T> {
  const resp = await apiRequest(baseUrl, path, options, attempted);
  return (await resp.json()) as T;
}

const trimSlash = (s: string): string => (s.endsWith('/') ? s.slice(0, -1) : s);

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
  return apiJson<EventMeta[]>(baseUrl, '/events', options, 'list the events');
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
  const body: CreateEventRequest = { name, ...(options.fields ?? {}) };
  return apiJson<EventMeta>(
    baseUrl,
    '/events',
    { method: 'POST', body, token, fetch: options.fetch },
    'create the event'
  );
}

/**
 * **Permanently delete** an event and ALL of its data (`DELETE /events/{id}`) — the papercut fix.
 * RD-gated like {@link createEvent} (full-trust by default: an open Director accepts it tokenless;
 * a gated one answers **401/403** and the caller obtains a token and retries). The delete is total
 * and irreversible server-side: the event's registry entry, its persisted state, and the active
 * pointer if it pointed here are all removed. An unknown id is a **404**. Resolves on a 2xx, or
 * rejects with a {@link RequestFailure} carrying the HTTP status (so the caller can branch on it).
 */
export async function deleteEvent(
  baseUrl: string,
  id: EventId,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    `/events/${encodeURIComponent(id)}`,
    { method: 'DELETE', token, fetch: options.fetch },
    'delete the event'
  );
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
  return apiJson<ActiveEvent>(baseUrl, '/active-event', options, 'read the active event');
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
  return apiJson<EventMeta>(
    baseUrl,
    '/active-event',
    { method: 'PUT', body: { id }, token, fetch: options.fetch },
    'set the active event'
  );
}

/**
 * List every pilot in the directory (`GET /pilots`) — issue #74. Pilots are **application-level
 * configuration**: the RD maintains a directory once (a persisted address book) that outlives
 * any one event. Reads are open on the LAN (no token needed; an optional token is sent when
 * present). Resolves to the {@link Pilot}s (in id order), or rejects on a transport/HTTP
 * failure.
 */
export async function listPilots(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<Pilot[]> {
  return apiJson<Pilot[]>(baseUrl, '/pilots', options, 'list the pilots');
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
  return apiJson<Pilot>(
    baseUrl,
    '/pilots',
    { method: 'POST', body: request, token, fetch: options.fetch },
    'add the pilot'
  );
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
  return apiJson<Pilot>(
    baseUrl,
    `/pilots/${encodeURIComponent(id)}`,
    { method: 'PUT', body: request, token, fetch: options.fetch },
    'save the pilot'
  );
}

/**
 * Delete a pilot (`DELETE /pilots/{id}`) — issue #74. RD-gated. An unknown id answers **404**.
 * Resolves once the delete succeeds, or rejects on a non-2xx / transport failure.
 */
export async function deletePilot(
  baseUrl: string,
  id: PilotId,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    `/pilots/${encodeURIComponent(id)}`,
    { method: 'DELETE', token, fetch: options.fetch },
    'delete the pilot'
  );
}

/**
 * List every timer the Director knows, each with its status right now (`GET /timers`). An open
 * read (no token). There is no change feed for timers: a screen that shows them asks again
 * every couple of seconds. Resolves to the {@link TimerView}s in id order, or rejects on a
 * non-2xx / transport failure.
 */
export async function listTimers(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<TimerView[]> {
  return apiJson<TimerView[]>(baseUrl, '/timers', options, 'list the timers');
}

/**
 * List the OFT timers found on the network (`GET /timers/discovered`): the ones that advertise
 * themselves. An open read (no token). Each {@link DiscoveredTimer} says whether a timer at
 * its address is already added. Finding a timer adds nothing; a person adds it with
 * {@link createTimer}. There is no change feed, so a screen that shows them asks again every
 * couple of seconds.
 */
export async function listDiscoveredTimers(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<DiscoveredTimer[]> {
  return apiJson<DiscoveredTimer[]>(
    baseUrl,
    '/timers/discovered',
    options,
    'look for timers on the network'
  );
}

/**
 * Add a timer (`POST /timers`). RD-gated. The Director makes the id and then keeps a connection
 * to the timer by itself. A blank name or an address that is not `host:port` is a **400** with a
 * sentence for the director. Resolves to the new {@link Timer}.
 */
export async function createTimer(
  baseUrl: string,
  request: CreateTimerRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<Timer> {
  return apiJson<Timer>(
    baseUrl,
    '/timers',
    { method: 'POST', body: request, token, fetch: options.fetch },
    'add the timer'
  );
}

/**
 * Remove a timer (`DELETE /timers/{id}`). RD-gated. An unknown id answers **404**. The passes it
 * sent stay in the events it timed. Resolves once the delete succeeds.
 */
export async function deleteTimer(
  baseUrl: string,
  id: AdapterId,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    `/timers/${encodeURIComponent(id)}`,
    { method: 'DELETE', token, fetch: options.fetch },
    'remove the timer'
  );
}

/**
 * Read a timer's settings (`GET /timers/{id}/config`). An open read (no token). The Director
 * asks the timer each time and keeps no copy, so this is what the timer is doing right now. A
 * timer that cannot be reached answers **409** with a sentence for the director.
 */
export async function getTimerConfig(
  baseUrl: string,
  id: AdapterId,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<TimerConfig> {
  return apiJson<TimerConfig>(
    baseUrl,
    `/timers/${encodeURIComponent(id)}/config`,
    options,
    "read the timer's settings"
  );
}

/**
 * Change a timer's settings (`PUT /timers/{id}/config`). RD-gated. The body is the whole
 * {@link TimerConfig}. Resolves to the settings as the timer reports them afterwards. A timer
 * that refuses, cannot be reached, or did not keep the change answers **409** with a sentence
 * for the director; a seat the timer cannot have is a **400**.
 */
export async function putTimerConfig(
  baseUrl: string,
  id: AdapterId,
  config: TimerConfig,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<TimerConfig> {
  return apiJson<TimerConfig>(
    baseUrl,
    `/timers/${encodeURIComponent(id)}/config`,
    { method: 'PUT', body: config, token, fetch: options.fetch },
    "change the timer's settings"
  );
}

/**
 * List every class in the application-level directory (`GET /classes`) — issue #84. An open read
 * (no token), mirroring `GET /pilots`: classes are app-level configuration the RD maintains once.
 * Resolves to the classes in id order, or rejects on a non-2xx /
 * transport failure.
 */
export async function listClasses(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<Class[]> {
  return apiJson<Class[]>(baseUrl, '/classes', options, 'list the classes');
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
  return apiJson<Class>(
    baseUrl,
    '/classes',
    { method: 'POST', body: request, token, fetch: options.fetch },
    'add the class'
  );
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
  return apiJson<Class>(
    baseUrl,
    `/classes/${encodeURIComponent(id)}`,
    { method: 'PUT', body: request, token, fetch: options.fetch },
    'save the class'
  );
}

/**
 * Delete a class (`DELETE /classes/{id}`) — issue #84. RD-gated. An unknown id answers **404**.
 * A built-in class is read-only and answers **400**. Resolves once the delete succeeds, or rejects
 * on a non-2xx / transport failure.
 */
export async function deleteClass(
  baseUrl: string,
  id: ClassId,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    `/classes/${encodeURIComponent(id)}`,
    { method: 'DELETE', token, fetch: options.fetch },
    'delete the class'
  );
}

/**
 * Hide or un-hide a class (`PUT /classes/{id}/hidden`) — hide/archive classes. RD-gated. The body is
 * `{ hidden }`: `true` archives the class, `false` brings it back.
 * Hiding is a **visibility preference**, not an edit — so it is valid for **built-in** classes too
 * (never a read-only rejection): the class stays in the directory and the Classes page, flagged
 * `hidden`. The choice is persisted server-side and survives a restart
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
  const body: SetClassHiddenRequest = { hidden };
  return apiJson<Class>(
    baseUrl,
    `/classes/${encodeURIComponent(id)}/hidden`,
    { method: 'PUT', body, token, fetch: options.fetch },
    'change the class visibility'
  );
}

// ── The competition in an event: snapshot, commands, presets, passes, change feed ────────────

/** The route root for one event's competition. */
const eventPath = (id: EventId, rest: string): string => `/events/${encodeURIComponent(id)}${rest}`;

/**
 * Read an event's competition snapshot (`GET /events/{id}/state`). An open read. It holds
 * the rosters, every phase with its lineup and ranking, every race with its laps, and the
 * Director's clock (`now`) at the moment it was worked out.
 */
export async function getEventState(
  baseUrl: string,
  id: EventId,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<EventView> {
  return apiJson<EventView>(baseUrl, eventPath(id, '/state'), options, 'read the event');
}

/**
 * Send one command (`POST /events/{id}/commands`). RD-gated. A command the event's state
 * does not allow answers **409** with the reason; it rejects with that sentence, which is
 * written for the director and is shown as it is.
 */
export async function sendCommand(
  baseUrl: string,
  id: EventId,
  command: Command,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<CommandOutcome> {
  return apiJson<CommandOutcome>(
    baseUrl,
    eventPath(id, '/commands'),
    { method: 'POST', body: command, token, fetch: options.fetch },
    'do that'
  );
}

/**
 * Generate a bracket's phases (`POST /events/{id}/presets/bracket`). RD-gated. All of the
 * phases are added, or none. A shape that cannot work answers **400**, and one the event
 * does not allow answers **409**, each with the reason.
 */
export async function createBracket(
  baseUrl: string,
  id: EventId,
  request: BracketRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<CommandOutcome> {
  return apiJson<CommandOutcome>(
    baseUrl,
    eventPath(id, '/presets/bracket'),
    { method: 'POST', body: request, token, fetch: options.fetch },
    'add the bracket'
  );
}

/**
 * Generate stacked mains (`POST /events/{id}/presets/mains`). RD-gated. All or none, like
 * {@link createBracket}.
 */
export async function createMains(
  baseUrl: string,
  id: EventId,
  request: MainsRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<CommandOutcome> {
  return apiJson<CommandOutcome>(
    baseUrl,
    eventPath(id, '/presets/mains'),
    { method: 'POST', body: request, token, fetch: options.fetch },
    'add the mains'
  );
}

/**
 * Log one gate crossing (`POST /events/{id}/passes`). RD-gated. `at` is in microseconds on
 * the timer's own clock. The Director stamps the pass with the race that is running now.
 */
export async function postPass(
  baseUrl: string,
  id: EventId,
  pass: PassRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<CommandOutcome> {
  return apiJson<CommandOutcome>(
    baseUrl,
    eventPath(id, '/passes'),
    { method: 'POST', body: pass, token, fetch: options.fetch },
    'log the crossing'
  );
}

/**
 * Read a race's logged signal (`GET /events/{id}/signal?race=…`). An open read. One entry per
 * seat of the race, in lineup order, on the timer's own clock (the same clock as the laps). A
 * seat on a timer that reports no signal has no chunks. Read it when it is wanted: a long race
 * can be a few megabytes.
 */
export async function getRaceSignal(
  baseUrl: string,
  id: EventId,
  race: RaceId,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<RaceSignal> {
  return apiJson<RaceSignal>(
    baseUrl,
    eventPath(id, `/signal?race=${encodeURIComponent(race)}`),
    options,
    "read the race's signal"
  );
}

/** The part of an `EventSource` the change feed uses. */
export interface EventSourceLike {
  addEventListener(type: string, listener: (event: { data?: unknown }) => void): void;
  close(): void;
}

/** A constructor for an {@link EventSourceLike} (injectable for tests and Node). */
export type EventSourceConstructor = new (url: string) => EventSourceLike;

/**
 * Listen to an event's change feed (`GET /events/{id}/changes`, server-sent events). An
 * open read. `onCursor` is called with the log length each time the Director sends it.
 * Returns a function that stops listening.
 *
 * There is no resume logic on purpose. `EventSource` reconnects by itself, and the Director
 * sends the current cursor on every connect, so a console that was cut off hears a cursor
 * as soon as it is back and reads the snapshot again.
 *
 * `onError` is told when the connection drops. The browser is already reconnecting by then.
 */
export function subscribeChanges(
  baseUrl: string,
  id: EventId,
  onCursor: (cursor: number) => void,
  opts: { EventSource?: EventSourceConstructor; onError?: () => void } = {}
): () => void {
  const Source: EventSourceConstructor | undefined =
    opts.EventSource ?? (globalThis as { EventSource?: EventSourceConstructor }).EventSource;
  if (!Source) throw new Error('This browser cannot listen for live changes.');
  const source = new Source(`${trimSlash(baseUrl)}${eventPath(id, '/changes')}`);
  source.addEventListener('cursor', (event) => {
    const cursor = Number(event.data);
    // A cursor that is not a number carries nothing to act on.
    if (Number.isFinite(cursor)) onCursor(cursor);
  });
  if (opts.onError) source.addEventListener('error', opts.onError);
  return () => source.close();
}

/** What a timer's live feed hands over. */
export interface TimerLiveHandlers {
  /** A batch of signal samples for one seat. About 20 to 50 a second for each seat. */
  onSignal: (chunk: SignalChunk) => void;
  /** Every seat's levels, about once a second. */
  onLevels: (levels: SeatLevels[]) => void;
  /** The connection dropped. The browser is already reconnecting by then. */
  onError?: () => void;
}

/** The JSON an event carries, or `undefined` when it is not JSON. */
function eventJson(data: unknown): unknown {
  if (typeof data !== 'string') return undefined;
  try {
    return JSON.parse(data) as unknown;
  } catch {
    return undefined;
  }
}

/**
 * Listen to a timer's live feed (`GET /timers/{id}/live`, server-sent events), for tuning. An
 * open read. `signal` events go to `onSignal` and `levels` events to `onLevels`. Returns a
 * function that stops listening.
 *
 * Nothing arrives while the timer is not connected. The Director ends the feed when the timer
 * is removed; `EventSource` then tries again by itself and `onError` hears each failure.
 *
 * An event that is not the JSON it should be is skipped: one odd event must not stop the feed.
 */
export function subscribeTimerLive(
  baseUrl: string,
  timerId: AdapterId,
  handlers: TimerLiveHandlers,
  opts: { EventSource?: EventSourceConstructor } = {}
): () => void {
  const Source: EventSourceConstructor | undefined =
    opts.EventSource ?? (globalThis as { EventSource?: EventSourceConstructor }).EventSource;
  if (!Source) throw new Error('This browser cannot listen for live changes.');
  const source = new Source(`${trimSlash(baseUrl)}/timers/${encodeURIComponent(timerId)}/live`);
  source.addEventListener('signal', (event) => {
    const chunk = eventJson(event.data);
    if (typeof chunk !== 'object' || chunk === null) return;
    if (!Array.isArray((chunk as Partial<SignalChunk>).levels)) return;
    handlers.onSignal(chunk as SignalChunk);
  });
  source.addEventListener('levels', (event) => {
    const levels = eventJson(event.data);
    if (Array.isArray(levels)) handlers.onLevels(levels as SeatLevels[]);
  });
  if (handlers.onError) source.addEventListener('error', handlers.onError);
  return () => source.close();
}
