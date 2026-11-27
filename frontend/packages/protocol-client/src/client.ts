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
  CertificateLook,
  CertificateRequest,
  ActiveEvent,
  AdapterId,
  AddressRequest,
  BracketRequest,
  BuiltinClasses,
  Class,
  ClassId,
  ClubPilotView,
  ClubRole,
  Command,
  CommandOutcome,
  CreateClassRequest,
  CreateEventRequest,
  CreatePilotRequest,
  CreateTimerRequest,
  DataCopy,
  DataExport,
  DataInfo,
  DiscoveredTimer,
  Door,
  DoorRequest,
  EventId,
  EventMeta,
  EventView,
  Format,
  AccountSignIns,
  InviteMade,
  ReconnectMade,
  ReconnectView,
  ServerClub,
  LinkTokenMade,
  LinkTokenView,
  InviteRequest,
  InviteView,
  KioskPassView,
  KioskView,
  LinkPeople,
  ClubSettings,
  LinkView,
  MainsRequest,
  Me,
  MemberView,
  MyResult,
  NetworkView,
  OverlayLook,
  OverlayPlacement,
  PassRequest,
  PhaseId,
  Pilot,
  PilotAccessView,
  PilotCodeView,
  PilotId,
  ProtocolError,
  PublicEvent,
  PublicHello,
  PullRequest,
  PublicPilot,
  RaceCodeView,
  RaceId,
  RaceSignal,
  RenameTimerRequest,
  RenderRequest,
  RoundRobinRequest,
  SavedFormat,
  SeatChannel,
  SeatLevels,
  SeatProfile,
  SeatProfileRequest,
  SeatRef,
  ServerEvent,
  SetClassHiddenRequest,
  SignalChunk,
  SignInProvider,
  SignupSeat,
  Timer,
  TimerConfig,
  TimerProfiles,
  TimerView,
  TokenRequest,
  UpdateClassRequest,
  UpdatePilotRequest,
  VideoSettings,
  VideoSettingsRequest,
  VideoStatus
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
  /** A file to send as it is, in place of a JSON body. */
  file?: Blob;
  /** Bearer token, sent as `Authorization: Bearer …` only when held. */
  token?: string;
  /** Inject a `fetch` (defaults to the global). Used by tests and Node. */
  fetch?: FetchLike;
  /** A kiosk pass: the pilot who proved who they are at a shared tablet (§11.4). */
  kiosk?: string;
}

/** The header a kiosk pass is sent in. */
export const KIOSK_HEADER = 'X-GridFPV-Kiosk';

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
  if (options.kiosk) headers[KIOSK_HEADER] = options.kiosk;
  const init: RequestInit = { headers };
  if (options.method) init.method = options.method;
  if (options.body !== undefined) init.body = JSON.stringify(options.body);
  if (options.file !== undefined) {
    headers['Content-Type'] = 'application/zip';
    init.body = options.file;
  }
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
 * Give a timer another name (`PUT /timers/{id}`). RD-gated. Only the name people see changes;
 * the timer itself is not told. A blank name is a **400** with a sentence for the director, an
 * unknown id a **404**. Resolves to the {@link Timer} as it now is.
 */
export async function renameTimer(
  baseUrl: string,
  id: AdapterId,
  name: string,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<Timer> {
  return apiJson<Timer>(
    baseUrl,
    `/timers/${encodeURIComponent(id)}`,
    { method: 'PUT', body: { name } satisfies RenameTimerRequest, token, fetch: options.fetch },
    'rename the timer'
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
 * Change an event's name and details (`PUT /events/{id}`). RD-gated. The body is the same
 * shape an event is created from. Resolves to the event as it now is. An empty name is a
 * **400**; an unknown id is a **404**.
 */
export async function updateEvent(
  baseUrl: string,
  id: EventId,
  request: CreateEventRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<EventMeta> {
  return apiJson<EventMeta>(
    baseUrl,
    `/events/${encodeURIComponent(id)}`,
    { method: 'PUT', body: request, token, fetch: options.fetch },
    'change the event'
  );
}

/** The saved formats (`GET /formats`). An open read. */
export async function listFormats(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<SavedFormat[]> {
  return apiJson<SavedFormat[]>(baseUrl, '/formats', options, 'list the formats');
}

/**
 * Save a new format (`POST /formats`). RD-gated. A name another saved format has is a
 * **409** with a sentence for the director.
 */
export async function createFormat(
  baseUrl: string,
  format: Format,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<SavedFormat> {
  return apiJson<SavedFormat>(
    baseUrl,
    '/formats',
    { method: 'POST', body: format, token, fetch: options.fetch },
    'save the format'
  );
}

/** Replace a saved format (`PUT /formats/{id}`). RD-gated. */
export async function updateFormat(
  baseUrl: string,
  id: string,
  format: Format,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<SavedFormat> {
  return apiJson<SavedFormat>(
    baseUrl,
    `/formats/${encodeURIComponent(id)}`,
    { method: 'PUT', body: format, token, fetch: options.fetch },
    'change the format'
  );
}

/** Remove a saved format (`DELETE /formats/{id}`). RD-gated. */
export async function deleteFormat(
  baseUrl: string,
  id: string,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    `/formats/${encodeURIComponent(id)}`,
    { method: 'DELETE', token, fetch: options.fetch },
    'delete the format'
  );
}

/** A timer's saved seat profiles (`GET /timers/{id}/profiles`). An open read. */
export async function listProfiles(
  baseUrl: string,
  timer: AdapterId,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<TimerProfiles> {
  return apiJson<TimerProfiles>(
    baseUrl,
    `/timers/${encodeURIComponent(timer)}/profiles`,
    options,
    "list the timer's profiles"
  );
}

/**
 * Save settings as a new profile of a timer (`POST /timers/{id}/profiles`). RD-gated.
 * Nothing is written to the timer. The new profile becomes the timer's active one.
 */
export async function createProfile(
  baseUrl: string,
  timer: AdapterId,
  request: SeatProfileRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<SeatProfile> {
  return apiJson<SeatProfile>(
    baseUrl,
    `/timers/${encodeURIComponent(timer)}/profiles`,
    { method: 'POST', body: request, token, fetch: options.fetch },
    'save the profile'
  );
}

/** Rename a profile or replace its settings (`PUT /timers/{id}/profiles/{pid}`). RD-gated. */
export async function updateProfile(
  baseUrl: string,
  timer: AdapterId,
  profile: string,
  request: SeatProfileRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<SeatProfile> {
  return apiJson<SeatProfile>(
    baseUrl,
    `/timers/${encodeURIComponent(timer)}/profiles/${encodeURIComponent(profile)}`,
    { method: 'PUT', body: request, token, fetch: options.fetch },
    'change the profile'
  );
}

/** Remove a profile (`DELETE /timers/{id}/profiles/{pid}`). RD-gated. */
export async function deleteProfile(
  baseUrl: string,
  timer: AdapterId,
  profile: string,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    `/timers/${encodeURIComponent(timer)}/profiles/${encodeURIComponent(profile)}`,
    { method: 'DELETE', token, fetch: options.fetch },
    'delete the profile'
  );
}

/**
 * Write a profile to its timer (`POST /timers/{id}/profiles/{pid}/apply`). RD-gated.
 * Resolves to the settings as the timer reports them afterwards. A timer that refuses or
 * cannot be reached answers **409** with a sentence for the director.
 */
export async function applyProfile(
  baseUrl: string,
  timer: AdapterId,
  profile: string,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<TimerConfig> {
  return apiJson<TimerConfig>(
    baseUrl,
    `/timers/${encodeURIComponent(timer)}/profiles/${encodeURIComponent(profile)}/apply`,
    { method: 'POST', token, fetch: options.fetch },
    'apply the profile'
  );
}

/** The video settings, without the password (`GET /video/settings`). An open read. */
export async function getVideoSettings(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<VideoSettings> {
  return apiJson<VideoSettings>(baseUrl, '/video/settings', options, 'read the video settings');
}

/**
 * Change the video settings (`PUT /video/settings`). RD-gated. A password left out of the
 * request keeps the stored one. An address that is not `host:port` is a **400**.
 */
export async function putVideoSettings(
  baseUrl: string,
  request: VideoSettingsRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<VideoSettings> {
  return apiJson<VideoSettings>(
    baseUrl,
    '/video/settings',
    { method: 'PUT', body: request, token, fetch: options.fetch },
    'change the video settings'
  );
}

/**
 * Ask for the overlay to be drawn onto a race's clean recording, or (with no race named)
 * onto every race of the event that needs it (`POST /video/render`). RD-gated. Answers with
 * how many were asked for. The drawing runs in the background: `getVideoStatus` follows it.
 * A race with no clean recording is a **409**.
 */
export async function postVideoRender(
  baseUrl: string,
  request: RenderRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<number> {
  return apiJson<number>(
    baseUrl,
    '/video/render',
    { method: 'POST', body: request, token, fetch: options.fetch },
    'draw the overlay'
  );
}

/**
 * Have the Director download ffmpeg and keep it in its data folder (`POST /video/ffmpeg`),
 * RD-gated. The download runs in the background: `getVideoStatus` says when it is there.
 * A computer Grid has no build for is a **409**.
 */
export async function postVideoFfmpeg(
  baseUrl: string,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    '/video/ffmpeg',
    { method: 'POST', token, fetch: options.fetch },
    'get ffmpeg'
  );
}

/** Where Grid keeps its data, and the copy that waits to be brought in (`GET /data`). */
export async function getData(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<DataInfo> {
  return apiJson<DataInfo>(baseUrl, '/data', options, 'read where the data is kept');
}

/**
 * Make a copy of everything Grid keeps (`POST /data/export`), RD-gated. The copy is a zip in
 * the data folder on the Director's computer; {@link dataExportUrl} downloads it.
 */
export async function postDataExport(
  baseUrl: string,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<DataExport> {
  return apiJson<DataExport>(
    baseUrl,
    '/data/export',
    { method: 'POST', token, fetch: options.fetch },
    'make the copy'
  );
}

/** The address a copy made by {@link postDataExport} downloads from. */
export function dataExportUrl(baseUrl: string, name: string): string {
  return `${trimSlash(baseUrl)}/data/export/${encodeURIComponent(name)}`;
}

/**
 * Bring in a copy (`POST /data/import`), RD-gated. The body is the zip itself. The Director
 * checks it and keeps it; it takes over when Grid is next started. A file that is not a
 * copy is a **400**, and a race being on is a **409**.
 */
export async function postDataImport(
  baseUrl: string,
  file: Blob,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<DataCopy> {
  return apiJson<DataCopy>(
    baseUrl,
    '/data/import',
    { method: 'POST', file, token, fetch: options.fetch },
    'bring in the copy'
  );
}

/** Forget the copy that waits to be brought in (`DELETE /data/import`), RD-gated. */
export async function deleteDataImport(
  baseUrl: string,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    '/data/import',
    { method: 'DELETE', token, fetch: options.fetch },
    'forget the copy'
  );
}

/** The clubs on a remote server (`GET /api/clubs`). Asked of a server, not a Director. */
export async function listServerClubs(
  baseUrl: string,
  options: { fetch?: FetchLike } = {}
): Promise<ServerClub[]> {
  return apiJson<ServerClub[]>(baseUrl, '/api/clubs', options, 'read the clubs');
}

/**
 * The club a friendly address leads to (`GET /api/club-words/{word}`). `undefined` when no
 * club has that word, or ever had it.
 */
export async function getServerClubByWord(
  baseUrl: string,
  word: string,
  options: { fetch?: FetchLike } = {}
): Promise<ServerClub | undefined> {
  try {
    return await apiJson<ServerClub>(
      baseUrl,
      `/api/club-words/${encodeURIComponent(word)}`,
      options,
      'read the club'
    );
  } catch (err) {
    if (isRequestFailure(err) && err.status === 404) return undefined;
    throw err;
  }
}

/** Make a club (`POST /api/clubs`). For an owner of the server. */
export async function makeServerClub(
  baseUrl: string,
  name: string,
  options: { fetch?: FetchLike } = {}
): Promise<ServerClub> {
  return apiJson<ServerClub>(
    baseUrl,
    '/api/clubs',
    { method: 'POST', body: { name }, fetch: options.fetch },
    'make the club'
  );
}

/**
 * Archive a club, or bring it back (`PUT /api/clubs/{club}/archived`). For an owner. An
 * archived club is read-only and off the list.
 */
export async function setServerClubArchived(
  baseUrl: string,
  club: string,
  archived: boolean,
  options: { fetch?: FetchLike } = {}
): Promise<ServerClub> {
  return apiJson<ServerClub>(
    baseUrl,
    `/api/clubs/${encodeURIComponent(club)}/archived`,
    { method: 'PUT', body: { archived }, fetch: options.fetch },
    archived ? 'archive the club' : 'bring the club back'
  );
}

/** A club's events on a remote server, newest first (`GET /api/clubs/{club}/events`). */
export async function listServerEvents(
  baseUrl: string,
  club: string,
  options: { fetch?: FetchLike } = {}
): Promise<ServerEvent[]> {
  return apiJson<ServerEvent[]>(
    baseUrl,
    `/api/clubs/${encodeURIComponent(club)}/events`,
    options,
    'read the events'
  );
}

/**
 * Where one event on a remote server is read from. Its public reads sit under this, with
 * the same names a Director's have, so `publicSource` reads either.
 */
export function serverEventBase(baseUrl: string, club: string, event: string): string {
  return `${baseUrl.replace(/\/$/, '')}/api/clubs/${encodeURIComponent(club)}/events/${encodeURIComponent(event)}`;
}

// ── Accounts on a remote server (docs/primitives.md §13.8). Asked of a server. ──────────
// The browser's session cookie says who is asking, so none of these takes a token.

/** Who this browser is signed in as on a server, if anyone (`GET /api/me`). */
export async function getServerMe(
  baseUrl: string,
  options: { fetch?: FetchLike } = {}
): Promise<Me> {
  return apiJson<Me>(baseUrl, '/api/me', options, 'read the account');
}

/** Change the signed-in account's name (`PATCH /api/me`). */
export async function renameServerMe(
  baseUrl: string,
  name: string,
  options: { fetch?: FetchLike } = {}
): Promise<Me> {
  return apiJson<Me>(
    baseUrl,
    '/api/me',
    { method: 'PATCH', body: { name }, fetch: options.fetch },
    'change the name'
  );
}

/** Take a sign-in off the signed-in account (`DELETE /api/me/sign-ins/{provider}/{subject}`). */
export async function removeServerSignIn(
  baseUrl: string,
  provider: string,
  subject: string,
  options: { fetch?: FetchLike } = {}
): Promise<Me> {
  return apiJson<Me>(
    baseUrl,
    `/api/me/sign-ins/${encodeURIComponent(provider)}/${encodeURIComponent(subject)}`,
    { method: 'DELETE', fetch: options.fetch },
    'remove the sign-in'
  );
}

/**
 * Mark the club the front page opens on for the signed-in account
 * (`PUT /api/me/primary-club`).
 */
export async function setServerPrimaryClub(
  baseUrl: string,
  club: string,
  options: { fetch?: FetchLike } = {}
): Promise<Me> {
  return apiJson<Me>(
    baseUrl,
    '/api/me/primary-club',
    { method: 'PUT', body: { club }, fetch: options.fetch },
    'mark the club as primary'
  );
}

/** Sign this browser out of a server (`POST /api/sign-out`). */
export async function signOutOfServer(
  baseUrl: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(baseUrl, '/api/sign-out', { method: 'POST', fetch: options.fetch }, 'sign out');
}

/** The providers a server lets people sign in with (`GET /api/sign-in/providers`). */
export async function listSignInProviders(
  baseUrl: string,
  options: { fetch?: FetchLike } = {}
): Promise<SignInProvider[]> {
  return apiJson<SignInProvider[]>(
    baseUrl,
    '/api/sign-in/providers',
    options,
    'read how to sign in'
  );
}

/**
 * The address that sends a browser to a provider to sign in (`/auth/{provider}/start`).
 * `invite` is the invite the person came with; `add` adds the provider to the account that
 * is signed in.
 */
export function signInUrl(
  baseUrl: string,
  provider: string,
  how: { invite?: string; add?: boolean } = {}
): string {
  const query = how.add ? '?add=1' : how.invite ? `?invite=${encodeURIComponent(how.invite)}` : '';
  return `${trimSlash(baseUrl)}/auth/${encodeURIComponent(provider)}/start${query}`;
}

/** What an invite link is for, when it still works (`GET /api/invites/{secret}`). */
export async function getServerInvite(
  baseUrl: string,
  secret: string,
  options: { fetch?: FetchLike } = {}
): Promise<InviteView> {
  return apiJson<InviteView>(
    baseUrl,
    `/api/invites/${encodeURIComponent(secret)}`,
    options,
    'read the invite'
  );
}

/** Use an invite as the signed-in account (`POST /api/invites/{secret}/accept`). */
export async function acceptServerInvite(
  baseUrl: string,
  secret: string,
  options: { fetch?: FetchLike } = {}
): Promise<Me> {
  return apiJson<Me>(
    baseUrl,
    `/api/invites/${encodeURIComponent(secret)}/accept`,
    { method: 'POST', fetch: options.fetch },
    'use the invite'
  );
}

const accountPath = (account: string, rest: string): string =>
  `/api/accounts/${encodeURIComponent(account)}/${rest}`;

/**
 * How an account signs in, and whether it has a re-connect link waiting
 * (`GET /api/accounts/{account}/sign-ins`). For an owner of the server.
 */
export async function getAccountSignIns(
  baseUrl: string,
  account: string,
  options: { fetch?: FetchLike } = {}
): Promise<AccountSignIns> {
  return apiJson<AccountSignIns>(
    baseUrl,
    accountPath(account, 'sign-ins'),
    options,
    'read how the account signs in'
  );
}

/**
 * Take a sign-in off someone's account, even their last
 * (`DELETE /api/accounts/{account}/sign-ins/{provider}/{subject}`). For an owner. The
 * account is signed out everywhere.
 */
export async function dropAccountSignIn(
  baseUrl: string,
  account: string,
  provider: string,
  subject: string,
  options: { fetch?: FetchLike } = {}
): Promise<AccountSignIns> {
  return apiJson<AccountSignIns>(
    baseUrl,
    accountPath(account, `sign-ins/${encodeURIComponent(provider)}/${encodeURIComponent(subject)}`),
    { method: 'DELETE', fetch: options.fetch },
    'take the sign-in off'
  );
}

/**
 * Make a link that lets someone back in to an account
 * (`POST /api/accounts/{account}/reconnect`). For an owner. The link's secret is in the
 * answer, this once.
 */
export async function makeAccountReconnect(
  baseUrl: string,
  account: string,
  options: { fetch?: FetchLike } = {}
): Promise<ReconnectMade> {
  return apiJson<ReconnectMade>(
    baseUrl,
    accountPath(account, 'reconnect'),
    { method: 'POST', fetch: options.fetch },
    'make the re-connect link'
  );
}

/** Take an account's re-connect link back (`DELETE /api/accounts/{account}/reconnect`). */
export async function revokeAccountReconnect(
  baseUrl: string,
  account: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    accountPath(account, 'reconnect'),
    { method: 'DELETE', fetch: options.fetch },
    'take the re-connect link back'
  );
}

/** Whose account a re-connect link is for, when it still works (`GET /api/reconnect/{secret}`). */
export async function getServerReconnect(
  baseUrl: string,
  secret: string,
  options: { fetch?: FetchLike } = {}
): Promise<ReconnectView> {
  return apiJson<ReconnectView>(
    baseUrl,
    `/api/reconnect/${encodeURIComponent(secret)}`,
    options,
    'read the re-connect link'
  );
}

const clubPath = (club: string, rest: string): string =>
  `/api/clubs/${encodeURIComponent(club)}/${rest}`;

/** A club's invites (`GET /api/clubs/{club}/invites`). For its directors. */
export async function listClubInvites(
  baseUrl: string,
  club: string,
  options: { fetch?: FetchLike } = {}
): Promise<InviteView[]> {
  return apiJson<InviteView[]>(baseUrl, clubPath(club, 'invites'), options, 'read the invites');
}

/** Make an invite to a club (`POST /api/clubs/{club}/invites`). Its link is given this once. */
export async function makeClubInvite(
  baseUrl: string,
  club: string,
  request: InviteRequest,
  options: { fetch?: FetchLike } = {}
): Promise<InviteMade> {
  return apiJson<InviteMade>(
    baseUrl,
    clubPath(club, 'invites'),
    { method: 'POST', body: request, fetch: options.fetch },
    'make the invite'
  );
}

/** Take an invite back (`DELETE /api/clubs/{club}/invites/{id}`). */
export async function revokeClubInvite(
  baseUrl: string,
  club: string,
  id: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    clubPath(club, `invites/${encodeURIComponent(id)}`),
    { method: 'DELETE', fetch: options.fetch },
    'take the invite back'
  );
}

/**
 * One event on a remote server, as its list shows it (`GET /api/clubs/{club}/events/{event}`).
 * `undefined` when the server holds no such event. A hidden event is not in the list, and
 * is still read here by anyone who has its address.
 */
export async function getServerEvent(
  baseUrl: string,
  club: string,
  event: string,
  options: { fetch?: FetchLike } = {}
): Promise<ServerEvent | undefined> {
  try {
    return await apiJson<ServerEvent>(
      baseUrl,
      clubPath(club, `events/${encodeURIComponent(event)}`),
      options,
      'read the event'
    );
  } catch (err) {
    if (isRequestFailure(err) && err.status === 404) return undefined;
    throw err;
  }
}

/** Give a club a new name (`PATCH /api/clubs/{club}`). For those who run it. */
export async function renameServerClub(
  baseUrl: string,
  club: string,
  name: string,
  options: { fetch?: FetchLike } = {}
): Promise<ServerClub> {
  return apiJson<ServerClub>(
    baseUrl,
    `/api/clubs/${encodeURIComponent(club)}`,
    { method: 'PATCH', body: { name }, fetch: options.fetch },
    'rename the club'
  );
}

/** A club's link tokens, newest first (`GET /api/clubs/{club}/tokens`). Never the tokens themselves. */
export async function listClubLinkTokens(
  baseUrl: string,
  club: string,
  options: { fetch?: FetchLike } = {}
): Promise<LinkTokenView[]> {
  return apiJson<LinkTokenView[]>(
    baseUrl,
    clubPath(club, 'tokens'),
    options,
    'read the link tokens'
  );
}

/** Make a link token (`POST /api/clubs/{club}/tokens`). The token is given this once. */
export async function makeClubLinkToken(
  baseUrl: string,
  club: string,
  label: string,
  options: { fetch?: FetchLike } = {}
): Promise<LinkTokenMade> {
  return apiJson<LinkTokenMade>(
    baseUrl,
    clubPath(club, 'tokens'),
    { method: 'POST', body: { label }, fetch: options.fetch },
    'make the link token'
  );
}

/** Take a link token back (`DELETE /api/clubs/{club}/tokens/{id}`). */
export async function revokeClubLinkToken(
  baseUrl: string,
  club: string,
  id: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    clubPath(club, `tokens/${encodeURIComponent(id)}`),
    { method: 'DELETE', fetch: options.fetch },
    'take the link token back'
  );
}

/** Hide an event from a club's list, or show it (`PUT …/events/{event}/listing`). */
export async function setServerEventHidden(
  baseUrl: string,
  club: string,
  event: string,
  hidden: boolean,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    clubPath(club, `events/${encodeURIComponent(event)}/listing`),
    { method: 'PUT', body: { hidden }, fetch: options.fetch },
    hidden ? 'hide the event' : 'show the event'
  );
}

/** Remove an event from a server (`DELETE /api/clubs/{club}/events/{event}`). */
export async function removeServerEvent(
  baseUrl: string,
  club: string,
  event: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    clubPath(club, `events/${encodeURIComponent(event)}`),
    { method: 'DELETE', fetch: options.fetch },
    'remove the event'
  );
}

/** A club's members (`GET /api/clubs/{club}/members`). For its directors. */
export async function listClubMembers(
  baseUrl: string,
  club: string,
  options: { fetch?: FetchLike } = {}
): Promise<MemberView[]> {
  return apiJson<MemberView[]>(baseUrl, clubPath(club, 'members'), options, 'read the members');
}

/** Give a member of a club a role (`PUT /api/clubs/{club}/members/{account}`). */
export async function setClubRole(
  baseUrl: string,
  club: string,
  account: string,
  role: ClubRole,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    clubPath(club, `members/${encodeURIComponent(account)}`),
    { method: 'PUT', body: { role }, fetch: options.fetch },
    'change the role'
  );
}

/** Remove a member from a club (`DELETE /api/clubs/{club}/members/{account}`). */
export async function removeClubMember(
  baseUrl: string,
  club: string,
  account: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    clubPath(club, `members/${encodeURIComponent(account)}`),
    { method: 'DELETE', fetch: options.fetch },
    'remove the member'
  );
}

const pilotPath = (club: string, pilot: string, rest: string): string =>
  clubPath(club, `pilots/${encodeURIComponent(pilot)}/${rest}`);

/** A club's pilots, and which are taken (`GET /api/clubs/{club}/pilots`). For its members. */
export async function listClubPilots(
  baseUrl: string,
  club: string,
  options: { fetch?: FetchLike } = {}
): Promise<ClubPilotView[]> {
  return apiJson<ClubPilotView[]>(baseUrl, clubPath(club, 'pilots'), options, 'read the pilots');
}

/** Ask for a pilot of a club (`POST /api/clubs/{club}/pilots/{pilot}/claim`). */
export async function askForClubPilot(
  baseUrl: string,
  club: string,
  pilot: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    pilotPath(club, pilot, 'claim'),
    { method: 'POST', fetch: options.fetch },
    'ask for the pilot'
  );
}

/** Stop asking for a pilot, or let go of one (`DELETE …/pilots/{pilot}/claim`). */
export async function dropClubPilotClaim(
  baseUrl: string,
  club: string,
  pilot: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    pilotPath(club, pilot, 'claim'),
    { method: 'DELETE', fetch: options.fetch },
    'let go of the pilot'
  );
}

/**
 * Say an account holds a pilot (`PUT …/pilots/{pilot}/holder`): the yes to an account that
 * asked, or a director giving it with no ask.
 */
export async function setClubPilotHolder(
  baseUrl: string,
  club: string,
  pilot: string,
  account: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    pilotPath(club, pilot, 'holder'),
    { method: 'PUT', body: { account }, fetch: options.fetch },
    'give the pilot'
  );
}

/** Take a pilot from its holder (`DELETE …/pilots/{pilot}/holder`). For directors. */
export async function freeClubPilot(
  baseUrl: string,
  club: string,
  pilot: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    pilotPath(club, pilot, 'holder'),
    { method: 'DELETE', fetch: options.fetch },
    'take the pilot back'
  );
}

/** Say no to an account that asked for a pilot (`DELETE …/pilots/{pilot}/asks/{account}`). */
export async function refuseClubPilotAsk(
  baseUrl: string,
  club: string,
  pilot: string,
  account: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    pilotPath(club, pilot, `asks/${encodeURIComponent(account)}`),
    { method: 'DELETE', fetch: options.fetch },
    'say no'
  );
}

/** The events the signed-in account's pilots flew, newest first (`GET /api/me/results`). */
export async function getServerMyResults(
  baseUrl: string,
  options: { fetch?: FetchLike } = {}
): Promise<MyResult[]> {
  return apiJson<MyResult[]>(baseUrl, '/api/me/results', options, 'read your results');
}

/**
 * The clubs this Director can send to, which club each event goes to, and how the sending
 * stands (`GET /server-link`). A link token is never in the answer.
 */
export async function getServerLink(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<LinkView> {
  return apiJson<LinkView>(baseUrl, '/server-link', options, 'read the server setting');
}

/**
 * Save a club to send to: a new one (`POST /server-link/clubs`), or a change to the one
 * with `id` (`PUT /server-link/clubs/{club}`). RD-gated.
 */
export async function saveLinkClub(
  baseUrl: string,
  id: string | undefined,
  settings: ClubSettings,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<LinkView> {
  return apiJson<LinkView>(
    baseUrl,
    id === undefined ? '/server-link/clubs' : `/server-link/clubs/${encodeURIComponent(id)}`,
    { method: id === undefined ? 'POST' : 'PUT', body: settings, token, fetch: options.fetch },
    'save the club'
  );
}

/** Forget a saved club (`DELETE /server-link/clubs/{club}`), RD-gated. */
export async function removeLinkClub(
  baseUrl: string,
  id: string,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<LinkView> {
  return apiJson<LinkView>(
    baseUrl,
    `/server-link/clubs/${encodeURIComponent(id)}`,
    { method: 'DELETE', token, fetch: options.fetch },
    'remove the club'
  );
}

/**
 * Say which saved club an event goes to, or `null` for none
 * (`PUT /server-link/events/{id}/club`), RD-gated.
 */
export async function setEventLinkClub(
  baseUrl: string,
  eventId: string,
  club: string | null,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<LinkView> {
  return apiJson<LinkView>(
    baseUrl,
    `/server-link/events/${encodeURIComponent(eventId)}/club`,
    { method: 'PUT', body: { club }, token, fetch: options.fetch },
    'change where the event is sent'
  );
}

/**
 * The linked server's club: its members, and its pilots with the account that holds each
 * (`GET /server-link/people`), RD-gated. Asks the server there and then.
 */
export async function getServerPeople(
  baseUrl: string,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<LinkPeople> {
  return apiJson<LinkPeople>(
    baseUrl,
    '/server-link/people',
    { token, fetch: options.fetch },
    "read the server's members"
  );
}

/**
 * Say which member of the club holds a pilot on the linked server, or with no account that
 * nobody does (`PUT /pilots/{id}/server-account`), RD-gated. Nothing changes here unless the
 * server agreed.
 */
export async function setPilotServerAccount(
  baseUrl: string,
  id: PilotId,
  account: string | undefined,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<Pilot> {
  return apiJson<Pilot>(
    baseUrl,
    `/pilots/${encodeURIComponent(id)}/server-account`,
    { method: 'PUT', body: { account: account ?? null }, token, fetch: options.fetch },
    "change the pilot's account"
  );
}

/**
 * Bring pilots in from the linked server, already tied to their accounts
 * (`POST /server-link/pull`), RD-gated. Resolves to the pilots that were added.
 */
export async function pullFromServer(
  baseUrl: string,
  request: PullRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<Pilot[]> {
  return apiJson<Pilot[]>(
    baseUrl,
    '/server-link/pull',
    { method: 'POST', body: request, token, fetch: options.fetch },
    'add pilots from the server'
  );
}

/**
 * Read the certificate the server at an address shows (`POST /server-link/certificate`), so
 * the director can check its fingerprint and accept it. Nothing is trusted or changed by
 * looking. RD-gated.
 */
export async function lookAtServerCertificate(
  baseUrl: string,
  request: CertificateRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<CertificateLook> {
  return apiJson<CertificateLook>(
    baseUrl,
    '/server-link/certificate',
    { method: 'POST', body: request, token, fetch: options.fetch },
    "look at the server's certificate"
  );
}

/**
 * Replace the server's copy of an event with this Director's
 * (`POST /server-link/events/{id}/replace`), RD-gated.
 */
export async function replaceOnServer(
  baseUrl: string,
  eventId: string,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<LinkView> {
  return apiJson<LinkView>(
    baseUrl,
    `/server-link/events/${encodeURIComponent(eventId)}/replace`,
    { method: 'POST', body: {}, token, fetch: options.fetch },
    "replace the server's copy"
  );
}

/**
 * The two doors other devices come in by, and where their listeners stand (`GET /network`).
 * The access token is in the answer only on the Grid computer.
 */
export async function getNetwork(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<NetworkView> {
  return apiJson<NetworkView>(baseUrl, '/network', options, 'read the network setting');
}

/** Open or shut a door, or change its port (`PUT /network/{door}`), RD-gated. */
export async function putDoor(
  baseUrl: string,
  door: Door,
  request: DoorRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<NetworkView> {
  return apiJson<NetworkView>(
    baseUrl,
    `/network/${door === 'Public' ? 'public' : 'remote'}`,
    { method: 'PUT', body: request, token, fetch: options.fetch },
    'change the network setting'
  );
}

/**
 * Change the access token of the Remote RD door (`PUT /network/token`): to the one given,
 * or to a new one Grid makes. Only the Grid computer may.
 */
export async function putAccessToken(
  baseUrl: string,
  request: TokenRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<NetworkView> {
  return apiJson<NetworkView>(
    baseUrl,
    '/network/token',
    { method: 'PUT', body: request, token, fetch: options.fetch },
    'change the access token'
  );
}

/**
 * Set the public address (`PUT /network/address`): the address every public link and QR code
 * uses, such as a name on the race network or an internet address a tunnel carries to the
 * Public door. An empty one clears it. RD-gated.
 */
export async function putPublicAddress(
  baseUrl: string,
  request: AddressRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<NetworkView> {
  return apiJson<NetworkView>(
    baseUrl,
    '/network/address',
    { method: 'PUT', body: request, token, fetch: options.fetch },
    'change the public address'
  );
}

/**
 * Who the Director takes this browser for, and which event is open (`GET /public/hello`).
 * Open to everyone: it is the first thing a page asks.
 */
export async function getPublicHello(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<PublicHello> {
  return apiJson<PublicHello>(baseUrl, '/public/hello', options, 'reach Grid');
}

/**
 * Every pilot in an event with their sign-in code (`GET /events/{id}/access`). For the
 * director: a visitor is refused.
 */
export async function getPilotAccess(
  baseUrl: string,
  event: EventId,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<PilotAccessView> {
  return apiJson<PilotAccessView>(
    baseUrl,
    eventPath(event, '/access'),
    options,
    "read the pilots' sign-in codes"
  );
}

/** Give a pilot a new sign-in code; the old one stops working. RD-gated. */
export async function resetPilotCode(
  baseUrl: string,
  event: EventId,
  pilot: PilotId,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<PilotCodeView> {
  return apiJson<PilotCodeView>(
    baseUrl,
    eventPath(event, `/access/${encodeURIComponent(pilot)}/reset`),
    { method: 'POST', token, fetch: options.fetch },
    'give the pilot a new code'
  );
}

/** Sign a pilot's device out; their code still works. RD-gated. */
export async function endPilotSession(
  baseUrl: string,
  event: EventId,
  pilot: PilotId,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    eventPath(event, `/access/${encodeURIComponent(pilot)}/session`),
    { method: 'DELETE', token, fetch: options.fetch },
    "sign the pilot's device out"
  );
}

/**
 * The event's race code (`GET /events/{id}/race-code`): what a pilot types on the server's
 * page to show they are at the race. For the director: a visitor is refused.
 */
export async function getRaceCode(
  baseUrl: string,
  event: EventId,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<RaceCodeView> {
  return apiJson<RaceCodeView>(
    baseUrl,
    eventPath(event, '/race-code'),
    options,
    'read the race code'
  );
}

/** Give the event a new race code; the old one stops working. RD-gated. */
export async function newRaceCode(
  baseUrl: string,
  event: EventId,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<RaceCodeView> {
  return apiJson<RaceCodeView>(
    baseUrl,
    eventPath(event, '/race-code/new'),
    { method: 'POST', token, fetch: options.fetch },
    'make a new race code'
  );
}

/** Where the race code's QR picture is served from, for an `<img>`. */
export function raceCodeQrUrl(baseUrl: string, event: EventId): string {
  return `${trimSlash(baseUrl)}${eventPath(event, '/race-code/qr')}`;
}

/**
 * A signed-in pilot checks in to an open seat from a remote server's page
 * (`POST …/public/pilot/checkin/take`), with the race code that shows they are at the race.
 * `baseUrl` is the event's own base on the server ({@link serverEventBase}).
 */
export async function takeSeatOnServer(
  baseUrl: string,
  race: RaceId,
  seat: { adapter: string; seat: string },
  code: string | undefined,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    '/public/pilot/checkin/take',
    {
      method: 'POST',
      body: { race, adapter: seat.adapter, seat: seat.seat, ...(code ? { code } : {}) },
      fetch: options.fetch
    },
    'take the seat'
  );
}

/** Where a pilot's QR code picture is served from, for an `<img>`. */
export function pilotQrUrl(baseUrl: string, event: EventId, pilot: PilotId): string {
  return `${trimSlash(baseUrl)}${eventPath(event, `/access/${encodeURIComponent(pilot)}/qr`)}`;
}

/**
 * Sign in as a pilot with their code (`POST /public/pilot/session`). The Director sets a
 * cookie the browser then sends by itself, and answers who the pilot is.
 */
export async function signInPilot(
  baseUrl: string,
  code: string,
  options: { fetch?: FetchLike } = {}
): Promise<PublicPilot> {
  return apiJson<PublicPilot>(
    baseUrl,
    '/public/pilot/session',
    { method: 'POST', body: { code }, fetch: options.fetch },
    'sign in'
  );
}

/** Sign out as a pilot (`DELETE /public/pilot/session`). */
export async function signOutPilot(
  baseUrl: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    '/public/pilot/session',
    { method: 'DELETE', fetch: options.fetch },
    'sign out'
  );
}

/** The kiosk codes waiting to be used and the kiosks set up (`GET /kiosk`). Director only. */
export async function getKiosks(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<KioskView> {
  return apiJson<KioskView>(baseUrl, '/kiosk', options, 'read the kiosks');
}

/** Make one more kiosk code (`POST /kiosk/codes`). Each sets up one tablet, once. */
export async function newKioskCode(
  baseUrl: string,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<KioskView> {
  return apiJson<KioskView>(
    baseUrl,
    '/kiosk/codes',
    { method: 'POST', token, fetch: options.fetch },
    'make a kiosk code'
  );
}

/** Cancel a kiosk code that has not been used (`DELETE /kiosk/codes/{code}`). */
export async function cancelKioskCode(
  baseUrl: string,
  code: string,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<KioskView> {
  return apiJson<KioskView>(
    baseUrl,
    `/kiosk/codes/${encodeURIComponent(code)}`,
    { method: 'DELETE', token, fetch: options.fetch },
    'cancel the kiosk code'
  );
}

/**
 * Name a kiosk, so two tablets can be told apart (`PUT /kiosk/devices/{number}`). An
 * empty name takes the name away.
 */
export async function renameKiosk(
  baseUrl: string,
  number: number,
  name: string,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<KioskView> {
  return apiJson<KioskView>(
    baseUrl,
    `/kiosk/devices/${number}`,
    { method: 'PUT', body: { name }, token, fetch: options.fetch },
    'name the kiosk'
  );
}

/** Remove a kiosk, so its tablet stops being one (`DELETE /kiosk/devices/{number}`). */
export async function removeKiosk(
  baseUrl: string,
  number: number,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<KioskView> {
  return apiJson<KioskView>(
    baseUrl,
    `/kiosk/devices/${number}`,
    { method: 'DELETE', token, fetch: options.fetch },
    'remove the kiosk'
  );
}

/**
 * Make this device a kiosk, with a kiosk code the director made
 * (`POST /public/pilot/kiosk/device`). The code is used up. The Director sets a cookie the
 * browser then sends by itself.
 */
export async function kioskSetUp(
  baseUrl: string,
  code: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    '/public/pilot/kiosk/device',
    { method: 'POST', body: { code }, fetch: options.fetch },
    'set up the kiosk'
  );
}

/**
 * Prove who a pilot is at a kiosk, with their code (`POST /public/pilot/kiosk`). Unlike
 * {@link signInPilot} it sets no cookie and leaves the pilot's own phone signed in. The
 * pass it answers is what the kiosk hands to the calls below, and it stops working by
 * itself a little while after it was last used.
 */
export async function kioskIn(
  baseUrl: string,
  code: string,
  options: { fetch?: FetchLike } = {}
): Promise<KioskPassView> {
  return apiJson<KioskPassView>(
    baseUrl,
    '/public/pilot/kiosk',
    { method: 'POST', body: { code }, fetch: options.fetch },
    'check the code'
  );
}

/** The pilot is done at the kiosk (`DELETE /public/pilot/kiosk`). */
export async function kioskDone(
  baseUrl: string,
  pass: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    '/public/pilot/kiosk',
    { method: 'DELETE', kiosk: pass, fetch: options.fetch },
    'sign out'
  );
}

/**
 * The signed-in pilot takes a seat in a heat (`POST /public/pilot/signup/take`). Who the
 * pilot is comes from their sign-in, never from here. Rejects with the Director's reason
 * when the rules do not allow it.
 */
export async function takeSpot(
  baseUrl: string,
  heat: PhaseId,
  seat: SignupSeat,
  options: { fetch?: FetchLike; kiosk?: string } = {}
): Promise<CommandOutcome> {
  return apiJson<CommandOutcome>(
    baseUrl,
    '/public/pilot/signup/take',
    { method: 'POST', body: { heat, seat }, fetch: options.fetch, kiosk: options.kiosk },
    'take the spot'
  );
}

/** The signed-in pilot takes the first free spot on a seat (`POST /public/pilot/signup/next`). */
export async function takeNextSpot(
  baseUrl: string,
  phase: PhaseId,
  seat: SignupSeat,
  options: { fetch?: FetchLike; kiosk?: string } = {}
): Promise<CommandOutcome> {
  return apiJson<CommandOutcome>(
    baseUrl,
    '/public/pilot/signup/next',
    { method: 'POST', body: { phase, seat }, fetch: options.fetch, kiosk: options.kiosk },
    'take the next free spot'
  );
}

/** The signed-in pilot gives their spot in a heat up (`POST /public/pilot/signup/leave`). */
export async function leaveSpot(
  baseUrl: string,
  heat: PhaseId,
  options: { fetch?: FetchLike; kiosk?: string } = {}
): Promise<CommandOutcome> {
  return apiJson<CommandOutcome>(
    baseUrl,
    '/public/pilot/signup/leave',
    { method: 'POST', body: { heat }, fetch: options.fetch, kiosk: options.kiosk },
    'leave the heat'
  );
}

/**
 * The signed-in pilot takes an open seat in a race that is being flown
 * (`POST /public/pilot/checkin/take`). The laps on the seat are theirs from their next
 * crossing. Who the pilot is comes from their sign-in, never from here.
 */
export async function takeSeat(
  baseUrl: string,
  race: RaceId,
  seat: { adapter: AdapterId; seat: SeatRef },
  options: { fetch?: FetchLike; kiosk?: string } = {}
): Promise<CommandOutcome> {
  return apiJson<CommandOutcome>(
    baseUrl,
    '/public/pilot/checkin/take',
    {
      method: 'POST',
      body: { race, adapter: seat.adapter, seat: seat.seat },
      fetch: options.fetch,
      kiosk: options.kiosk
    },
    'take the seat'
  );
}

/** The signed-in pilot gives up the seat they took (`POST /public/pilot/checkin/drop`). */
export async function dropSeat(
  baseUrl: string,
  race: RaceId,
  options: { fetch?: FetchLike; kiosk?: string } = {}
): Promise<CommandOutcome> {
  return apiJson<CommandOutcome>(
    baseUrl,
    '/public/pilot/checkin/drop',
    { method: 'POST', body: { race }, fetch: options.fetch, kiosk: options.kiosk },
    'drop the seat'
  );
}

/** Which event is open, for a visitor (`GET /public/event`). Empty when none is. */
export async function getPublicEvent(
  baseUrl: string,
  options: { fetch?: FetchLike } = {}
): Promise<PublicEvent> {
  return apiJson<PublicEvent>(baseUrl, '/public/event', options, 'see which event is open');
}

/**
 * The open event's snapshot as a visitor may see it (`GET /public/event/state`): the
 * console's own shape, with the director's things taken out.
 */
export async function getPublicEventState(
  baseUrl: string,
  options: { fetch?: FetchLike } = {}
): Promise<EventView> {
  return apiJson<EventView>(baseUrl, '/public/event/state', options, 'read the event');
}

/** The pilots in the open event: callsign and colour only (`GET /public/event/pilots`). */
export async function listPublicPilots(
  baseUrl: string,
  options: { fetch?: FetchLike } = {}
): Promise<Pilot[]> {
  return apiJson<Pilot[]>(baseUrl, '/public/event/pilots', options, 'read the pilots');
}

/** The classes in the open event: name only (`GET /public/event/classes`). */
export async function listPublicClasses(
  baseUrl: string,
  options: { fetch?: FetchLike } = {}
): Promise<Class[]> {
  return apiJson<Class[]>(baseUrl, '/public/event/classes', options, 'read the classes');
}

/** The timers the open event is flown on: name and seats only (`GET /public/event/timers`). */
export async function listPublicTimers(
  baseUrl: string,
  options: { fetch?: FetchLike } = {}
): Promise<TimerView[]> {
  return apiJson<TimerView[]>(baseUrl, '/public/event/timers', options, 'read the timers');
}

/**
 * The video channel of each seat the open event is flown on (`GET /public/event/channels`).
 * A bench seat has none. The Director asks each timer at most once in a while.
 */
export async function listPublicChannels(
  baseUrl: string,
  options: { fetch?: FetchLike } = {}
): Promise<SeatChannel[]> {
  return apiJson<SeatChannel[]>(baseUrl, '/public/event/channels', options, 'read the channels');
}

/**
 * Hear when the open event changes, as a visitor (`GET /public/event/changes`). The same
 * feed as {@link subscribeChanges}, for the one event that is open.
 */
export function subscribePublicChanges(
  baseUrl: string,
  onCursor: (cursor: number) => void,
  opts: { EventSource?: EventSourceConstructor; onError?: () => void } = {}
): () => void {
  const Source: EventSourceConstructor | undefined =
    opts.EventSource ?? (globalThis as { EventSource?: EventSourceConstructor }).EventSource;
  if (!Source) throw new Error('This browser cannot listen for live changes.');
  const source = new Source(`${trimSlash(baseUrl)}/public/event/changes`);
  source.addEventListener('cursor', (event) => {
    const cursor = Number(event.data);
    if (Number.isFinite(cursor)) onCursor(cursor);
  });
  if (opts.onError) source.addEventListener('error', opts.onError);
  return () => source.close();
}

/**
 * Sign in as the director from another device (`POST /public/session`). The Director sets
 * a cookie the browser then sends by itself; the page never keeps the token.
 */
export async function signInDirector(
  baseUrl: string,
  token: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    '/public/session',
    { method: 'POST', token, fetch: options.fetch },
    'sign in'
  );
}

/** Sign out again (`DELETE /public/session`). */
export async function signOutDirector(
  baseUrl: string,
  options: { fetch?: FetchLike } = {}
): Promise<void> {
  await apiRequest(
    baseUrl,
    '/public/session',
    { method: 'DELETE', fetch: options.fetch },
    'sign out'
  );
}

/** How the race overlay looks: where it sits and which logos it has (`GET /overlay/look`). */
export async function getOverlayLook(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<OverlayLook> {
  return apiJson<OverlayLook>(baseUrl, '/overlay/look', options, "read the overlay's look");
}

/** Move or resize the race overlay (`PUT /overlay/look`), RD-gated. */
export async function putOverlayLook(
  baseUrl: string,
  placement: OverlayPlacement,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<OverlayLook> {
  return apiJson<OverlayLook>(
    baseUrl,
    '/overlay/look',
    { method: 'PUT', body: placement, token, fetch: options.fetch },
    'move the overlay'
  );
}

/**
 * Where one of a race's videos plays from (`GET /events/{event}/races/{race}/video/{which}`).
 * `recording` is the recording itself; `overlay` is a video with the overlay in it, whether
 * or not its laps are still right. The Director answers byte ranges, so a player can seek.
 */
export function raceVideoUrl(
  baseUrl: string,
  event: string,
  race: string,
  which: 'recording' | 'overlay'
): string {
  return `${trimSlash(baseUrl)}/events/${encodeURIComponent(event)}/races/${encodeURIComponent(
    race
  )}/video/${which}`;
}

/** Where one of the overlay's two logos is served from (`GET /overlay/logo/{side}`). */
export function overlayLogoUrl(baseUrl: string, side: 'left' | 'right'): string {
  return `${trimSlash(baseUrl)}/overlay/logo/${side}`;
}

/** Keep a picture as one of the overlay's logos (`PUT /overlay/logo/{side}`), RD-gated. */
export async function putOverlayLogo(
  baseUrl: string,
  side: 'left' | 'right',
  file: Blob,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<OverlayLook> {
  return apiJson<OverlayLook>(
    baseUrl,
    `/overlay/logo/${side}`,
    { method: 'PUT', file, token, fetch: options.fetch },
    'keep the logo'
  );
}

/** Take one of the overlay's logos away (`DELETE /overlay/logo/{side}`), RD-gated. */
export async function deleteOverlayLogo(
  baseUrl: string,
  side: 'left' | 'right',
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<OverlayLook> {
  return apiJson<OverlayLook>(
    baseUrl,
    `/overlay/logo/${side}`,
    { method: 'DELETE', token, fetch: options.fetch },
    'take the logo away'
  );
}

/** The link to OBS and the runs recorded since the Director started (`GET /video/status`). */
export async function getVideoStatus(
  baseUrl: string,
  options: { token?: string; fetch?: FetchLike } = {}
): Promise<VideoStatus> {
  return apiJson<VideoStatus>(baseUrl, '/video/status', options, 'read the video status');
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
 * A built-in class can be removed too; {@link restoreBuiltinClasses} puts them back. Resolves
 * once the delete succeeds, or rejects on a non-2xx / transport failure.
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

/** How many built-in classes the director has removed (`GET /classes/builtins`). Open. */
export async function getBuiltinClasses(
  baseUrl: string,
  options: { fetch?: FetchLike } = {}
): Promise<BuiltinClasses> {
  return apiJson<BuiltinClasses>(baseUrl, '/classes/builtins', options, 'read the classes');
}

/**
 * Put back every built-in class that was removed (`POST /classes/builtins`). RD-gated.
 * Resolves to the whole list of classes.
 */
export async function restoreBuiltinClasses(
  baseUrl: string,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<Class[]> {
  return apiJson<Class[]>(
    baseUrl,
    '/classes/builtins',
    { method: 'POST', token, fetch: options.fetch },
    'put the built-in classes back'
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
 * Generate a round robin (`POST /events/{id}/presets/round-robin`). RD-gated. All or none,
 * like {@link createBracket}.
 */
export async function createRoundRobin(
  baseUrl: string,
  id: EventId,
  request: RoundRobinRequest,
  token?: string,
  options: { fetch?: FetchLike } = {}
): Promise<CommandOutcome> {
  return apiJson<CommandOutcome>(
    baseUrl,
    eventPath(id, '/presets/round-robin'),
    { method: 'POST', body: request, token, fetch: options.fetch },
    'add the round robin'
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
