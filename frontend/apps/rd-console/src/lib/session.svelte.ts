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
 * **Full-trust by default (#72, Slice 1b):** privileged actions are sent **without** a token
 * first. Only if the Director responds **401/403** (i.e. it has a token configured and is
 * gating control) does the lazy {@link TokenDialog} open; the entered token is then reused for
 * the rest of the session and the action is retried. Against an open (unconfigured) Director —
 * the local-trust posture — there is therefore **no prompt ever**. (The proper loopback-trust +
 * remote-passphrase split is tracked separately as #80 and is not built here.)
 *
 * Everything reactive is Svelte 5 runes so screens read `session.connectionStatus`,
 * `session.liveState`, etc. directly. The protocol client is framework-agnostic, so we
 * bridge its `onState` callback into a `$state` field here.
 */

import {
  connect,
  listEvents,
  createEvent,
  deleteEvent,
  getActiveEvent,
  setActiveEvent,
  listTimers,
  createTimer,
  updateTimer,
  deleteTimer,
  setEventTimers,
  setPrimaryTimer,
  listPilots,
  createPilot,
  updatePilot,
  deletePilot,
  setEventRoster,
  addToRoster,
  removeFromRoster,
  listClasses,
  createClass,
  updateClass,
  deleteClass,
  setEventClasses,
  setClassMembership,
  listFormats,
  listFormatSchemas,
  listChannels,
  createRound,
  updateRound,
  deleteRound,
  listHeats,
  roundRanking,
  classStandings,
  PRACTICE_EVENT_ID
} from '@gridfpv/protocol-client';
import type { ProtocolClient, ProtocolState, ConnectionStatus } from '@gridfpv/protocol-client';
import { createControlClient } from './control.js';
import type { ControlClient } from './control.js';
import type {
  AdapterId,
  ChannelCatalogEntry,
  Class,
  ClassId,
  ClassStandings,
  Command,
  CommandAck,
  CompetitorRef,
  CreateClassRequest,
  CreateEventRequest,
  CreatePilotRequest,
  CreateTimerRequest,
  EventMeta,
  FormatSchema,
  HeatId,
  HeatResult,
  HeatSummary,
  LiveRaceState,
  MemberSlot,
  Pilot,
  PilotId,
  NewRoundReq,
  ProjectionBody,
  RankEntry,
  RoundDef,
  RoundId,
  Scope,
  Timer,
  TimerId,
  UpdateClassRequest,
  UpdatePilotRequest,
  UpdateRoundReq,
  UpdateTimerRequest
} from '@gridfpv/types';

const TOKEN_STORAGE_KEY = 'gridfpv.rd.token';

/**
 * How often the session re-reads `GET /timers` for live connection status while inside an event
 * (#73, Slice 2b). Polling is the agreed v1 — `GET /timers` returns current status per request —
 * and ~2.5s reads "at a glance" without hammering the Director. The interval only runs in-event.
 */
const TIMER_POLL_MS = 2_500;

/** The optional descriptive fields a create-event dialog may supply alongside the name. */
export type CreateEventFields = Omit<CreateEventRequest, 'name'>;

/**
 * Whether a failed {@link CommandAck} is an **auth** rejection (the Director is gating
 * control). These are the only acks that warrant the lazy token prompt; everything else
 * (a bad transition, a transport error) is surfaced as-is.
 */
function isAuthAck(ack: CommandAck): boolean {
  // The Director rejects an unauthenticated control caller with `Unauthorized` (HTTP 401);
  // that is the one ack that means "control is gated — a token is needed".
  return ack.error?.code === 'Unauthorized';
}

/**
 * Whether a thrown error from `createEvent` is an HTTP **401/403** — i.e. the Director is
 * gating event creation. `createEvent` rejects with an `Error` whose message carries the HTTP
 * status (`POST /events failed: HTTP 401`), so we match on that.
 */
function isAuthFailure(e: unknown): boolean {
  const msg = e instanceof Error ? e.message : String(e);
  return /\b(401|403)\b/.test(msg);
}

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
  /**
   * Whether the session is still **resolving the Director's active event** (#90) — the brief
   * window on load before we know whether to show the workspace (an active event is set) or the
   * picker (none). The shell shows a loading state while this is `true`; it flips to `false`
   * once {@link resolveActiveEvent} settles (success or failure).
   */
  resolvingActiveEvent = $state(true);
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
  /**
   * The Director's timer registry with **live** status, polled while inside an event (#73,
   * Slice 2b). `GET /timers` returns each timer's *current* status on every request (the
   * Director mutates an RH timer's status live as its connection comes and goes), so the
   * console polls it (~{@link TIMER_POLL_MS}) so a timer dropping off shows up at a glance.
   * `$state.raw`: an immutable list replaced wholesale per poll. Empty until the first poll.
   */
  timers = $state.raw<Timer[]>([]);

  // Non-reactive internals.
  #token: string | undefined;
  #client: ProtocolClient | undefined;
  #control: ControlClient | undefined;
  #unsub: (() => void) | undefined;
  /** The shell's lazy token prompt (a `Dialog`); set via {@link setTokenProvider}. */
  #tokenProvider: TokenProvider | undefined;
  /** The live-status poll interval while inside an event; cleared on leave/teardown. */
  #timerPoll: ReturnType<typeof setInterval> | undefined;
  // Injectable for tests so the session never opens a real socket.
  #connectImpl: typeof connect;
  #controlFactory: typeof createControlClient;
  #listEventsImpl: typeof listEvents;
  #createEventImpl: typeof createEvent;
  #deleteEventImpl: typeof deleteEvent;
  #getActiveEventImpl: typeof getActiveEvent;
  #setActiveEventImpl: typeof setActiveEvent;
  #listTimersImpl: typeof listTimers;
  #createTimerImpl: typeof createTimer;
  #updateTimerImpl: typeof updateTimer;
  #deleteTimerImpl: typeof deleteTimer;
  #setEventTimersImpl: typeof setEventTimers;
  #setPrimaryTimerImpl: typeof setPrimaryTimer;
  #listPilotsImpl: typeof listPilots;
  #createPilotImpl: typeof createPilot;
  #updatePilotImpl: typeof updatePilot;
  #deletePilotImpl: typeof deletePilot;
  #setEventRosterImpl: typeof setEventRoster;
  #addToRosterImpl: typeof addToRoster;
  #removeFromRosterImpl: typeof removeFromRoster;
  #listClassesImpl: typeof listClasses;
  #createClassImpl: typeof createClass;
  #updateClassImpl: typeof updateClass;
  #deleteClassImpl: typeof deleteClass;
  #setEventClassesImpl: typeof setEventClasses;
  #setClassMembershipImpl: typeof setClassMembership;
  #listFormatsImpl: typeof listFormats;
  #listFormatSchemasImpl: typeof listFormatSchemas;
  #listChannelsImpl: typeof listChannels;
  #createRoundImpl: typeof createRound;
  #updateRoundImpl: typeof updateRound;
  #deleteRoundImpl: typeof deleteRound;
  #listHeatsImpl: typeof listHeats;
  #roundRankingImpl: typeof roundRanking;
  #classStandingsImpl: typeof classStandings;

  constructor(opts?: {
    connectImpl?: typeof connect;
    controlFactory?: typeof createControlClient;
    listEventsImpl?: typeof listEvents;
    createEventImpl?: typeof createEvent;
    deleteEventImpl?: typeof deleteEvent;
    getActiveEventImpl?: typeof getActiveEvent;
    setActiveEventImpl?: typeof setActiveEvent;
    listTimersImpl?: typeof listTimers;
    createTimerImpl?: typeof createTimer;
    updateTimerImpl?: typeof updateTimer;
    deleteTimerImpl?: typeof deleteTimer;
    setEventTimersImpl?: typeof setEventTimers;
    setPrimaryTimerImpl?: typeof setPrimaryTimer;
    listPilotsImpl?: typeof listPilots;
    createPilotImpl?: typeof createPilot;
    updatePilotImpl?: typeof updatePilot;
    deletePilotImpl?: typeof deletePilot;
    setEventRosterImpl?: typeof setEventRoster;
    addToRosterImpl?: typeof addToRoster;
    removeFromRosterImpl?: typeof removeFromRoster;
    listClassesImpl?: typeof listClasses;
    createClassImpl?: typeof createClass;
    updateClassImpl?: typeof updateClass;
    deleteClassImpl?: typeof deleteClass;
    setEventClassesImpl?: typeof setEventClasses;
    setClassMembershipImpl?: typeof setClassMembership;
    listFormatsImpl?: typeof listFormats;
    listFormatSchemasImpl?: typeof listFormatSchemas;
    listChannelsImpl?: typeof listChannels;
    createRoundImpl?: typeof createRound;
    updateRoundImpl?: typeof updateRound;
    deleteRoundImpl?: typeof deleteRound;
    listHeatsImpl?: typeof listHeats;
    roundRankingImpl?: typeof roundRanking;
    classStandingsImpl?: typeof classStandings;
    baseUrl?: string;
    autoRestore?: boolean;
  }) {
    this.#connectImpl = opts?.connectImpl ?? connect;
    this.#controlFactory = opts?.controlFactory ?? createControlClient;
    this.#listEventsImpl = opts?.listEventsImpl ?? listEvents;
    this.#createEventImpl = opts?.createEventImpl ?? createEvent;
    this.#deleteEventImpl = opts?.deleteEventImpl ?? deleteEvent;
    this.#getActiveEventImpl = opts?.getActiveEventImpl ?? getActiveEvent;
    this.#setActiveEventImpl = opts?.setActiveEventImpl ?? setActiveEvent;
    this.#listTimersImpl = opts?.listTimersImpl ?? listTimers;
    this.#createTimerImpl = opts?.createTimerImpl ?? createTimer;
    this.#updateTimerImpl = opts?.updateTimerImpl ?? updateTimer;
    this.#deleteTimerImpl = opts?.deleteTimerImpl ?? deleteTimer;
    this.#setEventTimersImpl = opts?.setEventTimersImpl ?? setEventTimers;
    this.#setPrimaryTimerImpl = opts?.setPrimaryTimerImpl ?? setPrimaryTimer;
    this.#listPilotsImpl = opts?.listPilotsImpl ?? listPilots;
    this.#createPilotImpl = opts?.createPilotImpl ?? createPilot;
    this.#updatePilotImpl = opts?.updatePilotImpl ?? updatePilot;
    this.#deletePilotImpl = opts?.deletePilotImpl ?? deletePilot;
    this.#setEventRosterImpl = opts?.setEventRosterImpl ?? setEventRoster;
    this.#addToRosterImpl = opts?.addToRosterImpl ?? addToRoster;
    this.#removeFromRosterImpl = opts?.removeFromRosterImpl ?? removeFromRoster;
    this.#listClassesImpl = opts?.listClassesImpl ?? listClasses;
    this.#createClassImpl = opts?.createClassImpl ?? createClass;
    this.#updateClassImpl = opts?.updateClassImpl ?? updateClass;
    this.#deleteClassImpl = opts?.deleteClassImpl ?? deleteClass;
    this.#setEventClassesImpl = opts?.setEventClassesImpl ?? setEventClasses;
    this.#setClassMembershipImpl = opts?.setClassMembershipImpl ?? setClassMembership;
    this.#listFormatsImpl = opts?.listFormatsImpl ?? listFormats;
    this.#listFormatSchemasImpl = opts?.listFormatSchemasImpl ?? listFormatSchemas;
    this.#listChannelsImpl = opts?.listChannelsImpl ?? listChannels;
    this.#createRoundImpl = opts?.createRoundImpl ?? createRound;
    this.#updateRoundImpl = opts?.updateRoundImpl ?? updateRound;
    this.#deleteRoundImpl = opts?.deleteRoundImpl ?? deleteRound;
    this.#listHeatsImpl = opts?.listHeatsImpl ?? listHeats;
    this.#roundRankingImpl = opts?.roundRankingImpl ?? roundRanking;
    this.#classStandingsImpl = opts?.classStandingsImpl ?? classStandings;
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

  // ── Timer registry (issue #73) ─────────────────────────────────────────────
  //
  // Timers are **application-level** configuration (a persisted registry the RD tunes once),
  // not event state — so these live on the session itself, reachable from the picker, and not
  // gated on `currentEvent`. Reads are open; writes are control-gated and follow the same
  // full-trust-first posture as every other privileged action: attempted tokenless, and only
  // if the Director answers 401/403 (it has a token configured) does the lazy prompt fire once
  // and the write retry — see {@link #privilegedWrite}.

  /**
   * List every configured timer (`GET /timers`, open, no token) — the built-in **Mock** first.
   * Used by the app-level Timers management screen. Rejects on a transport/HTTP failure.
   */
  listTimers(): Promise<Timer[]> {
    return this.#listTimersImpl(this.baseUrl, { token: this.#token });
  }

  /**
   * List every pilot in the application-level directory (`GET /pilots`, open, no token) — issue
   * #74. Like timers, pilots are app-level configuration (a persisted directory the RD maintains
   * once, rostered per event), so this lives on the session and is reachable from the home hub's
   * **Pilots** page. Rejects on a transport/HTTP failure (the page surfaces it). The full pilot
   * directory management (`PilotManager`) is the next slice; for now this read backs the hub's
   * Pilots count and the placeholder page's read-only callsign list.
   */
  listPilots(): Promise<Pilot[]> {
    return this.#listPilotsImpl(this.baseUrl, { token: this.#token });
  }

  /**
   * Create a directory pilot (`POST /pilots`) — issue #74. RD-gated, full-trust first like the
   * timer writes: attempted tokenless, and only if the Director answers 401/403 does the lazy
   * prompt fire once and the create retry. The `callsign` is required (server **400** on blank);
   * the id is auto-generated. Returns the new {@link Pilot}, `undefined` on a cancelled prompt, or
   * throws on a non-auth failure (a bad hex `color` / `country` is a **400** the form surfaces).
   */
  createPilot(request: CreatePilotRequest): Promise<Pilot | undefined> {
    return this.#privilegedWrite((token) => this.#createPilotImpl(this.baseUrl, request, token));
  }

  /**
   * Edit a directory pilot (`PUT /pilots/{id}`) — issue #74. RD-gated. The request carries only the
   * fields that changed: an omitted optional field is left unchanged, a present **`null`** clears it
   * (the way the form clears `color`/`country`), and a present value sets it. Returns the updated
   * {@link Pilot}, `undefined` on a cancelled prompt, or throws on a non-auth failure.
   */
  updatePilot(id: PilotId, request: UpdatePilotRequest): Promise<Pilot | undefined> {
    return this.#privilegedWrite((token) =>
      this.#updatePilotImpl(this.baseUrl, id, request, token)
    );
  }

  /**
   * Remove a directory pilot (`DELETE /pilots/{id}`) — issue #74. RD-gated. Resolves to `true` once
   * the delete succeeds, `undefined` on a cancelled token prompt, or throws on any other failure
   * (an unknown id is a **404**). (`true` lets callers tell success from a cancelled prompt — the
   * `DELETE` has no body, mirroring {@link deleteTimer}.)
   */
  deletePilot(id: PilotId): Promise<true | undefined> {
    return this.#privilegedWrite(async (token) => {
      await this.#deletePilotImpl(this.baseUrl, id, token);
      return true as const;
    });
  }

  // ── Class registry (issue #84) ─────────────────────────────────────────────
  //
  // Classes are **application-level** configuration (a persisted directory the RD maintains once,
  // selected per event), exactly like pilots and timers — so these live on the session, reachable
  // from the home hub's **Classes** page. Reads are open; writes are control-gated and follow the
  // same full-trust-first → lazy-token-prompt posture (see {@link #privilegedWrite}).

  /**
   * List every class in the application-level directory (`GET /classes`, open, no token) — issue
   * #84. Like pilots, classes are app-level configuration (a persisted directory the RD maintains
   * once, selected per event), so this lives on the session and backs the hub's **Classes** page +
   * its count. Rejects on a transport/HTTP failure (the page surfaces it).
   */
  listClasses(): Promise<Class[]> {
    return this.#listClassesImpl(this.baseUrl, { token: this.#token });
  }

  /**
   * Create a directory class (`POST /classes`) — issue #84. RD-gated, full-trust first like the
   * pilot writes: attempted tokenless, and only if the Director answers 401/403 does the lazy
   * prompt fire once and the create retry. The `name` is required (server **400** on blank); the id
   * is auto-generated. Returns the new {@link Class}, `undefined` on a cancelled prompt, or throws
   * on a non-auth failure.
   */
  createClass(request: CreateClassRequest): Promise<Class | undefined> {
    return this.#privilegedWrite((token) => this.#createClassImpl(this.baseUrl, request, token));
  }

  /**
   * Edit a directory class (`PUT /classes/{id}`) — issue #84. RD-gated. The request carries only the
   * fields that changed: a present `name`/`source` replaces it, an omitted optional field is left
   * unchanged, a present **`null`** clears it (the way the form clears `reference`/`description`),
   * and a present value sets it. Returns the updated {@link Class}, `undefined` on a cancelled
   * prompt, or throws on a non-auth failure.
   */
  updateClass(id: ClassId, request: UpdateClassRequest): Promise<Class | undefined> {
    return this.#privilegedWrite((token) =>
      this.#updateClassImpl(this.baseUrl, id, request, token)
    );
  }

  /**
   * Remove a directory class (`DELETE /classes/{id}`) — issue #84. RD-gated. Resolves to `true` once
   * the delete succeeds, `undefined` on a cancelled token prompt, or throws on any other failure (an
   * unknown id is a **404**). Mirrors {@link deletePilot}.
   */
  deleteClass(id: ClassId): Promise<true | undefined> {
    return this.#privilegedWrite(async (token) => {
      await this.#deleteClassImpl(this.baseUrl, id, token);
      return true as const;
    });
  }

  /**
   * Run a control-gated write with the full-trust-then-lazy-prompt retry: call `attempt` with
   * the currently-held token; if it fails for **auth** and no token is held yet, prompt once and
   * retry. Returns the result, or `undefined` if the RD cancelled the prompt; re-throws any
   * non-auth failure (so the caller surfaces a 400/404/transport error verbatim).
   */
  async #privilegedWrite<T>(
    attempt: (token: string | undefined) => Promise<T>
  ): Promise<T | undefined> {
    try {
      return await attempt(this.#token);
    } catch (e) {
      if (this.#token || !isAuthFailure(e)) throw e;
      if (!(await this.#promptForToken())) return undefined;
      return await attempt(this.#token);
    }
  }

  /**
   * Create a timer (`POST /timers`). Returns the new {@link Timer}, `undefined` if a gated
   * Director's token prompt was cancelled, or throws on a non-auth failure.
   */
  createTimer(request: CreateTimerRequest): Promise<Timer | undefined> {
    return this.#privilegedWrite((token) => this.#createTimerImpl(this.baseUrl, request, token));
  }

  /**
   * Edit a timer (`PUT /timers/{id}`) — retune Mock laps/pace, or repoint a RotorHazard url.
   * Returns the updated {@link Timer}, `undefined` on a cancelled prompt, or throws otherwise.
   */
  updateTimer(id: TimerId, request: UpdateTimerRequest): Promise<Timer | undefined> {
    return this.#privilegedWrite((token) =>
      this.#updateTimerImpl(this.baseUrl, id, request, token)
    );
  }

  /**
   * Remove a timer (`DELETE /timers/{id}`). The built-in **Mock cannot be deleted** (the Director
   * answers **400**); that surfaces to the caller as a thrown error to handle gracefully. Resolves
   * to `true` once the delete succeeds, `undefined` on a cancelled token prompt, or throws on any
   * other failure. (Returning `true` lets callers tell success apart from a cancelled prompt — the
   * `DELETE` itself has no body, so a raw `void` would be indistinguishable from the cancel case.)
   */
  deleteTimer(id: TimerId): Promise<true | undefined> {
    return this.#privilegedWrite(async (token) => {
      await this.#deleteTimerImpl(this.baseUrl, id, token);
      return true as const;
    });
  }

  /**
   * Set the **current event's** selected timers (`PUT /events/{id}/timers`) — the per-event
   * reference into the app-level registry. No-op (resolves `undefined`) when no event is selected.
   * On success the updated {@link EventMeta} replaces {@link currentEvent} so the workspace's view
   * of the selection stays in sync; returns it, `undefined` on a cancelled prompt, or throws.
   */
  async setEventTimers(ids: TimerId[]): Promise<EventMeta | undefined> {
    const event = this.currentEvent;
    if (!event) return undefined;
    const updated = await this.#privilegedWrite((token) =>
      this.#setEventTimersImpl(this.baseUrl, event.id, ids, token)
    );
    if (updated) this.currentEvent = updated;
    return updated;
  }

  /**
   * Designate the **current event's** primary timer (`PUT /events/{id}/primary-timer`) — issue
   * #112. Among the selected timers exactly one is the primary (it feeds the race); the rest are
   * alternates. Pass a timer `id` (it must be one of the event's selected timers) to make it
   * primary, or `null` to clear the override so the **first** selected timer is the effective
   * primary. No-op (resolves `undefined`) when no event is selected. On success the updated
   * {@link EventMeta} replaces {@link currentEvent}; returns it, `undefined` on a cancelled prompt,
   * or throws.
   */
  async setPrimaryTimer(id: TimerId | null): Promise<EventMeta | undefined> {
    const event = this.currentEvent;
    if (!event) return undefined;
    const updated = await this.#privilegedWrite((token) =>
      this.#setPrimaryTimerImpl(this.baseUrl, event.id, id, token)
    );
    if (updated) this.currentEvent = updated;
    return updated;
  }

  /**
   * Set the **current event's** roster (`PUT /events/{id}/roster`) — issue #74. The per-event
   * reference into the app-level pilot directory: the event rosters which directory pilots race it.
   * Pass the full set of pilot ids (replaces the roster wholesale). No-op (resolves `undefined`)
   * when no event is selected. On success the updated {@link EventMeta} replaces
   * {@link currentEvent} so the workspace's view of the roster stays in sync; returns it,
   * `undefined` on a cancelled prompt, or throws.
   */
  async setEventRoster(pilotIds: PilotId[]): Promise<EventMeta | undefined> {
    const event = this.currentEvent;
    if (!event) return undefined;
    const updated = await this.#privilegedWrite((token) =>
      this.#setEventRosterImpl(this.baseUrl, event.id, pilotIds, token)
    );
    if (updated) this.currentEvent = updated;
    return updated;
  }

  /**
   * Set the **current event's** class selection (`PUT /events/{id}/classes`) — issue #84. The
   * per-event reference into the app-level class directory: the event runs which directory classes
   * it selects. Pass the full set of class ids (replaces the selection wholesale). No-op (resolves
   * `undefined`) when no event is selected. On success the updated {@link EventMeta} replaces
   * {@link currentEvent} so the workspace's view of the selection stays in sync; returns it,
   * `undefined` on a cancelled prompt, or throws. Mirrors {@link setEventRoster}.
   */
  async setEventClasses(classIds: ClassId[]): Promise<EventMeta | undefined> {
    const event = this.currentEvent;
    if (!event) return undefined;
    const updated = await this.#privilegedWrite((token) =>
      this.#setEventClassesImpl(this.baseUrl, event.id, classIds, token)
    );
    if (updated) this.currentEvent = updated;
    return updated;
  }

  /**
   * Set which roster pilots race a single **class** of the current event
   * (`PUT /events/{id}/classes/{classId}/membership`) — race redesign Slice 1b. This is the finer
   * per-class join layered on the roster (who is *present*) and the class selection (which
   * categories *run*): given those, which present pilots race *this* class. Pass the full set of
   * pilot ids for the class (replaces *that class's* membership wholesale; an empty list clears it;
   * other classes are untouched). No-op (resolves `undefined`) when no event is selected. On success
   * the updated {@link EventMeta} replaces {@link currentEvent} so the workspace's view of
   * `classes_membership` stays in sync; returns it, `undefined` on a cancelled prompt, or throws.
   */
  async setClassMembership(
    classId: ClassId,
    members: (PilotId | MemberSlot)[]
  ): Promise<EventMeta | undefined> {
    const event = this.currentEvent;
    if (!event) return undefined;
    const updated = await this.#privilegedWrite((token) =>
      this.#setClassMembershipImpl(this.baseUrl, event.id, classId, members, token)
    );
    if (updated) this.currentEvent = updated;
    return updated;
  }

  // --- Rounds (race redesign Slice 2b) ----------------------------------------------------------
  // The event's rounds are event-level, class-tagged, dynamic format-instances. Reads of the valid
  // **format names** are open (`GET /formats`); the add/update/remove writes are control-gated and
  // re-home `currentEvent` so the Rounds stage stays in sync, mirroring the class/roster writes.

  /**
   * List the valid **format names** (`GET /formats`, open, no token) — race redesign Slice 2b. The
   * single source of truth (the engine's `FormatRegistry::standard()`, sorted) the Rounds UI's
   * format dropdown reads, rather than a hard-coded list. Rejects on a transport/HTTP failure.
   */
  listFormats(): Promise<string[]> {
    return this.#listFormatsImpl(this.baseUrl, { token: this.#token });
  }

  /**
   * List the valid **formats + their param schemas** (`GET /formats`, open, no token) — race redesign
   * Slice 7b. Each production format with the param schema its generator reads
   * (`{ name, params: [{ key, label, kind, options?, default? }] }`) — the single source of truth the
   * Rounds UI's guided params editor reads to offer the chosen format's params and render the right
   * typed control per knob. Rejects on a transport/HTTP failure.
   */
  listFormatSchemas(): Promise<FormatSchema[]> {
    return this.#listFormatSchemasImpl(this.baseUrl, { token: this.#token });
  }

  /**
   * List the standard **FPV channel catalog** (`GET /channels`, open, no token) — race redesign
   * Slice 4b. The band/channel ↔ raw-MHz vocabulary the server compiles in: the Channels UI offers
   * it when picking a Flexible timer's available channels, and reads it back to label a heat's
   * assigned frequencies. Rejects on a transport/HTTP failure.
   */
  listChannels(): Promise<ChannelCatalogEntry[]> {
    return this.#listChannelsImpl(this.baseUrl, { token: this.#token });
  }

  /**
   * Add a **round** to the current event (`POST /events/{id}/rounds`) — race redesign Slice 2b.
   * RD-gated; the round id is auto-generated server-side. No-op (resolves `undefined`) when no event
   * is selected. On success the created {@link RoundDef} is appended to {@link currentEvent}'s
   * rounds so the stage reflects the add immediately; returns the new round, `undefined` on a
   * cancelled prompt, or throws (a bad class / format / seeding is a **400**).
   */
  async createRound(request: NewRoundReq): Promise<RoundDef | undefined> {
    const event = this.currentEvent;
    if (!event) return undefined;
    const round = await this.#privilegedWrite((token) =>
      this.#createRoundImpl(this.baseUrl, event.id, request, token)
    );
    if (round) {
      this.currentEvent = { ...event, rounds: [...(event.rounds ?? []), round] };
    }
    return round;
  }

  /**
   * Replace an existing **round**'s fields (`PUT /events/{id}/rounds/{round}`) — race redesign Slice
   * 2b. RD-gated; the id is not editable and every other field is replaced wholesale. No-op
   * (resolves `undefined`) when no event is selected. On success the updated {@link RoundDef}
   * replaces its entry in {@link currentEvent}'s rounds; returns it, `undefined` on a cancelled
   * prompt, or throws (a bad class / format / seeding is a **400**; an unknown round a **404**).
   */
  async updateRound(roundId: RoundId, request: UpdateRoundReq): Promise<RoundDef | undefined> {
    const event = this.currentEvent;
    if (!event) return undefined;
    const round = await this.#privilegedWrite((token) =>
      this.#updateRoundImpl(this.baseUrl, event.id, roundId, request, token)
    );
    if (round) {
      const rounds = (event.rounds ?? []).map((r) => (r.id === roundId ? round : r));
      this.currentEvent = { ...event, rounds };
    }
    return round;
  }

  /**
   * Remove a **round** from the current event (`DELETE /events/{id}/rounds/{round}`) — race redesign
   * Slice 2b. RD-gated. No-op (resolves `undefined`) when no event is selected. On success the
   * updated {@link EventMeta} replaces {@link currentEvent} so the stage drops the round; returns
   * it, `undefined` on a cancelled prompt, or throws (an unknown round is a **404**).
   */
  async deleteRound(roundId: RoundId): Promise<EventMeta | undefined> {
    const event = this.currentEvent;
    if (!event) return undefined;
    const updated = await this.#privilegedWrite((token) =>
      this.#deleteRoundImpl(this.baseUrl, event.id, roundId, token)
    );
    if (updated) this.currentEvent = updated;
    return updated;
  }

  // --- Heats (race redesign Slice 3b) -----------------------------------------------------------
  // The Heats half of the Rounds & Heats stage. `fillRound` and the tagged `scheduleHeat` are
  // privileged **control commands** (they go through the same `send` path as every transition);
  // `listHeats` is an open read of the round-tagged heats list the UI groups by round.

  /**
   * Fill a round's **next heat** (`Command::FillRound`) — race redesign Slice 3b. Asks the per-event
   * round engine to append the next format-generated `HeatScheduled` tagged with this round (and its
   * class when single-class). A successful ack means *either* a heat was appended *or* the round is
   * already complete / its outstanding heat must be scored first — the engine treats those as
   * expected no-ops, so the caller re-reads {@link listHeats} to see whether a new heat appeared.
   * Sent through the control path (full-trust first → lazy token); returns the raw {@link CommandAck}
   * like {@link send}.
   */
  fillRound(round: RoundId): Promise<CommandAck> {
    return this.send({ FillRound: { round } });
  }

  /**
   * Schedule a heat by hand (`Command::ScheduleHeat`) — race redesign Slice 3b, the manual build that
   * replaces the retired free-text NewHeat form. The lineup is real {@link CompetitorRef}s drawn from
   * a round's eligible class members (no typed names), and the heat is **tagged** with the round and
   * its class so it lands in that round's heats list. Sent through the control path; returns the raw
   * {@link CommandAck}.
   */
  scheduleHeat(
    heat: HeatId,
    lineup: CompetitorRef[],
    tag: { class?: ClassId; round?: RoundId } = {}
  ): Promise<CommandAck> {
    return this.send({
      ScheduleHeat: { heat, lineup, class: tag.class, round: tag.round }
    });
  }

  /**
   * List the current event's **scheduled heats** (`GET /events/{id}/heats`, open, no token) — race
   * redesign Slice 3b. One {@link HeatSummary} per heat the log ever scheduled, with its round/class
   * tag, lineup, derived phase, and current flag. The Heats UI filters this by round. No-op (resolves
   * `[]`) when no event is selected; rejects on a transport/HTTP failure (the screen surfaces it).
   */
  listHeats(): Promise<HeatSummary[]> {
    const event = this.currentEvent;
    if (!event) return Promise.resolve([]);
    return this.#listHeatsImpl(this.baseUrl, event.id, { token: this.#token });
  }

  // --- Rankings & standings (race redesign Slice 5/6a + 5/6b) -----------------------------------
  // The season-join reads the Results screen + the Rounds stage's per-round standings render, and
  // the source the bracket seeds `FromRanking` from. Both are open reads (no token); they resolve
  // empty / reject (e.g. an unscored round 400s) for the screen to surface.

  /**
   * Read a **round's ranking** (`GET /events/{id}/rounds/{round}/ranking`) — race redesign Slice
   * 5/6a. The ordered per-competitor {@link RankEntry} list (best first, 1-based tie-aware
   * positions) the bracket seeds `FromRanking` from, shown as a round's compact "Standings" view.
   * No-op (resolves `[]`) when no event is selected; rejects on a non-2xx / transport failure (an
   * unscorable round is a 400 the stage surfaces).
   */
  roundRanking(roundId: RoundId): Promise<RankEntry[]> {
    const event = this.currentEvent;
    if (!event) return Promise.resolve([]);
    return this.#roundRankingImpl(this.baseUrl, event.id, roundId, { token: this.#token });
  }

  /**
   * Read a **class's standings** (`GET /events/{id}/classes/{class}/standings`) — race redesign
   * Slice 5/6a. The season-join {@link ClassStandings} the Results screen reads: one per-pilot row
   * per competitor that raced the class, aggregated across the class's rounds (points, best lap,
   * total laps, rounds entered), best first. No-op (resolves empty standings for the class) when no
   * event is selected; rejects on a non-2xx / transport failure (a class with no rounds resolves to
   * empty standings server-side).
   */
  classStandings(classId: ClassId): Promise<ClassStandings> {
    const event = this.currentEvent;
    if (!event) return Promise.resolve({ class: classId, standings: [] });
    return this.#classStandingsImpl(this.baseUrl, event.id, classId, { token: this.#token });
  }

  /**
   * Bind a timing-source competitor to a pilot for lap attribution (`Command::Register`) — the
   * IRL **binding** step of the Roster stage. A timing source (RotorHazard node, sim player) reports
   * a source-local {@link CompetitorRef} that means nothing to GridFPV until the RD binds it to a
   * directory {@link PilotId}; the projection then attributes that competitor's laps to the pilot.
   * (The sim auto-binds its players, so this is the manual IRL path.) Sent through the control path
   * (full-trust first → lazy token), so it returns the raw {@link CommandAck} like {@link send}.
   */
  registerCompetitor(
    adapter: AdapterId,
    competitor: CompetitorRef,
    pilot: PilotId
  ): Promise<CommandAck> {
    return this.send({ Register: { adapter, competitor, pilot } });
  }

  /**
   * Add one pilot to the **current event's** roster (`POST /events/{id}/roster/{pilotId}`) — issue
   * #74. Idempotent. No-op (resolves `undefined`) when no event is selected. On success the updated
   * {@link EventMeta} replaces {@link currentEvent}; returns it, `undefined` on a cancelled prompt,
   * or throws.
   */
  async addToRoster(pilotId: PilotId): Promise<EventMeta | undefined> {
    const event = this.currentEvent;
    if (!event) return undefined;
    const updated = await this.#privilegedWrite((token) =>
      this.#addToRosterImpl(this.baseUrl, event.id, pilotId, token)
    );
    if (updated) this.currentEvent = updated;
    return updated;
  }

  /**
   * Remove one pilot from the **current event's** roster (`DELETE /events/{id}/roster/{pilotId}`) —
   * issue #74. Removing a pilot not on the roster is a no-op. Resolves `undefined` when no event is
   * selected. On success the updated {@link EventMeta} replaces {@link currentEvent}; returns it,
   * `undefined` on a cancelled prompt, or throws.
   */
  async removeFromRoster(pilotId: PilotId): Promise<EventMeta | undefined> {
    const event = this.currentEvent;
    if (!event) return undefined;
    const updated = await this.#privilegedWrite((token) =>
      this.#removeFromRosterImpl(this.baseUrl, event.id, pilotId, token)
    );
    if (updated) this.currentEvent = updated;
    return updated;
  }

  /**
   * Read the id of the Director's **active event** (`GET /active-event`, open, no token) — issue
   * #91. The picker uses this to mark which event is currently live, independent of whether this
   * console is inside it: "Switch event" ({@link leaveEvent}) returns to the picker *without*
   * clearing the server's active event, so the picker can show the **Active** pill on the row of
   * the still-live event. Resolves the id, or `undefined` when nothing is active or the read fails
   * (the picker simply shows no pill — it never blocks the list).
   */
  async getActiveEventId(): Promise<EventMeta['id'] | undefined> {
    try {
      const { event } = await this.#getActiveEventImpl(this.baseUrl, { token: this.#token });
      return event?.id;
    } catch {
      return undefined;
    }
  }

  /**
   * Resolve the Director's **active event** on load/connect (issue #90) and either resume into
   * it or fall back to the picker. The active event is server-side Director state — there is one
   * RD on one event — so a reload/reconnect/app-restart reads it (`GET /active-event`) and enters
   * the same event every client is on, instead of dropping to the picker.
   *
   * - active set → {@link selectEvent} resumes the workspace (the live read stream reconnects).
   * - active `null`, or the read fails (Director unreachable) → stay at the picker.
   *
   * Always clears {@link resolvingActiveEvent} when it settles, so the shell can leave its
   * loading state whichever way it resolves.
   */
  async resolveActiveEvent(): Promise<void> {
    this.resolvingActiveEvent = true;
    try {
      const { event } = await this.#getActiveEventImpl(this.baseUrl, { token: this.#token });
      if (event) this.selectEvent(event);
    } catch {
      // The Director is unreachable / errored — leave the picker to surface it (it re-lists
      // events and shows its own error). The active event simply stays unresolved.
    } finally {
      this.resolvingActiveEvent = false;
    }
  }

  /**
   * Choose an event from the picker (issue #90): persist it as the Director's **active event**
   * (`PUT /active-event`, RD-gated, full-trust first) so every client — and this console after a
   * reload — resumes into it, then enter it. Returns the event on success, or `undefined` if a
   * gated Director's token prompt was cancelled; on a non-auth failure the set still throws.
   *
   * If setting the active event fails for **auth** (a token-gated Director) the lazy prompt
   * fires once and the set is retried — mirroring {@link createEventAndEnter}. We still
   * {@link selectEvent} locally even if the *persist* could not be completed in the no-provider
   * edge case, so the RD is never blocked from entering an event they picked.
   */
  async chooseEvent(meta: EventMeta): Promise<EventMeta | undefined> {
    try {
      await this.#setActiveEventImpl(this.baseUrl, meta.id, this.#token);
    } catch (e) {
      if (this.#token || !isAuthFailure(e)) throw e;
      if (!(await this.#promptForToken())) return undefined;
      await this.#setActiveEventImpl(this.baseUrl, meta.id, this.#token);
    }
    this.selectEvent(meta);
    return meta;
  }

  /**
   * **Permanently delete** an event and all of its data (`DELETE /events/{id}`) — the papercut
   * fix. Called from the Events page only after the RD confirms an unmissable, irreversible
   * warning dialog. RD-gated, full-trust first (like {@link chooseEvent}): the delete is
   * attempted with whatever token is held; only if a gated Director answers **401/403** does the
   * lazy prompt fire once and the delete retry.
   *
   * If the deleted event is the one this console is currently inside, the local seams are torn
   * down ({@link leaveEvent}) so the workspace doesn't dangle on a now-gone event. Resolves to
   * `true` once the delete succeeds, `undefined` if a gated Director's token prompt was cancelled,
   * or throws on any other failure (Practice is a **400**, an unknown id a **404**).
   */
  async deleteEvent(id: EventMeta['id']): Promise<true | undefined> {
    try {
      await this.#deleteEventImpl(this.baseUrl, id, this.#token);
    } catch (e) {
      if (this.#token || !isAuthFailure(e)) throw e;
      if (!(await this.#promptForToken())) return undefined;
      await this.#deleteEventImpl(this.baseUrl, id, this.#token);
    }
    // If we were inside the just-deleted event, leave it so the workspace doesn't dangle.
    if (this.currentEvent?.id === id) this.leaveEvent();
    return true as const;
  }

  /**
   * Enter an event (#72): root both seams under `/events/{id}/…` and open the live read
   * stream (open, no token). The control client is built with whatever token is held;
   * if none, the first privileged {@link send} will lazily obtain one.
   *
   * This is the pure **enter** primitive — it does NOT persist the selection. The picker
   * persists via {@link chooseEvent} (`PUT /active-event`); {@link resolveActiveEvent} calls
   * this directly to *resume* into the already-persisted active event without re-writing it.
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

    // Begin polling the timer registry's live status for the header pills (#73, Slice 2b).
    this.#startTimerPoll();
  }

  /**
   * Poll `GET /timers` while inside an event so the header pills (and the Timers screens, which
   * read the same {@link timers}) reflect each timer's **live** connection status (#73, Slice 2b).
   * Reads once immediately, then every {@link TIMER_POLL_MS}; a failed poll keeps the last list
   * (a transient blip shouldn't blank the pills). {@link selectEvent} starts it; {@link leaveEvent}
   * (and a re-select) clears it first, so only one interval ever runs.
   */
  #startTimerPoll(): void {
    this.#stopTimerPoll();
    const poll = async () => {
      try {
        this.timers = await this.listTimers();
      } catch {
        /* keep the last good list; the next tick retries */
      }
    };
    void poll();
    this.#timerPoll = setInterval(poll, TIMER_POLL_MS);
  }

  /** Stop the timer-status poll and clear the handle (idempotent). */
  #stopTimerPoll(): void {
    if (this.#timerPoll !== undefined) {
      clearInterval(this.#timerPoll);
      this.#timerPoll = undefined;
    }
  }

  /**
   * The current event's **selected** timers, paired with their live polled status (#73, Slice 2b):
   * `currentEvent.timers` (the per-event selection, in its saved order) intersected with the polled
   * {@link timers} registry. Drives the context header's status pills. Empty at the picker, or until
   * the first poll lands; a selected id not yet in the registry is simply skipped.
   */
  get selectedTimers(): Timer[] {
    const ids = this.currentEvent?.timers;
    if (!ids?.length) return [];
    const byId = new Map(this.timers.map((t) => [t.id, t]));
    return ids.map((id) => byId.get(id)).filter((t): t is Timer => t !== undefined);
  }

  /**
   * The current event's **effective primary** timer id (issue #112), applying the "first selected
   * = primary when null" rule: `EventMeta.primary_timer` when set and still in the selection,
   * otherwise the first selected timer. `undefined` when no event/selection. The remaining selected
   * timers are alternates (hot standby).
   */
  get primaryTimerId(): TimerId | undefined {
    const ids = this.currentEvent?.timers;
    if (!ids?.length) return undefined;
    const explicit = this.currentEvent?.primary_timer;
    if (explicit && ids.includes(explicit)) return explicit;
    return ids[0];
  }

  /**
   * The current event's effective **primary timer** ({@link Timer}), resolving
   * {@link primaryTimerId} against the polled {@link timers} registry. `undefined` when no event /
   * selection, or until the first timer poll lands. Open practice reads its `node_count` +
   * `available_channels` off this to lay out the active-channels picker / per-channel live board.
   */
  get primaryTimer(): Timer | undefined {
    const id = this.primaryTimerId;
    if (!id) return undefined;
    return this.timers.find((t) => t.id === id);
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

  /**
   * "Switch event" — return to the picker (a **client-side** view change) and tear the read seam
   * down, **without clearing the Director's active event** (issue #90). Leaving the server's
   * active event set means a reload mid-switch resumes into the current event, and other clients
   * (pilot/read views) are not disrupted by one RD browsing the picker; choosing a new event
   * (`PUT /active-event` via {@link chooseEvent}) is what actually re-points the Director.
   */
  leaveEvent(): void {
    this.#stopTimerPoll();
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
    this.timers = [];
  }

  /**
   * Prompt for a token via the lazy provider and hold it. Returns `true` once a token was
   * entered, `false` if there is no provider or the RD cancelled. The control client is
   * rebuilt (via {@link setToken}) to carry the freshly-obtained token.
   */
  async #promptForToken(): Promise<boolean> {
    if (!this.#tokenProvider) return false;
    const entered = await this.#tokenProvider();
    if (!entered?.trim()) return false;
    this.setToken(entered);
    return true;
  }

  /**
   * Create a persistent event by name (`POST /events`) and enter it.
   *
   * Full-trust first (#72, Slice 1b): the create is attempted **without** a token; only if the
   * Director rejects it for **auth** (it has a token configured) does the lazy prompt fire, and
   * the create is retried once with the entered token. Returns the new {@link EventMeta}, throws
   * on a non-auth transport/HTTP failure, or resolves `undefined` if the RD cancelled the prompt.
   */
  async createEventAndEnter(
    name: string,
    fields?: CreateEventFields
  ): Promise<EventMeta | undefined> {
    try {
      const meta = await this.#createEventImpl(this.baseUrl, name, this.#token, { fields });
      await this.#persistActive(meta.id);
      this.selectEvent(meta);
      return meta;
    } catch (e) {
      // A held token that still failed for auth, or a non-auth failure, is a real error.
      if (this.#token || !isAuthFailure(e)) throw e;
      // Open Director would have succeeded; a 401/403 means control is gated — prompt once.
      if (!(await this.#promptForToken())) return undefined;
      const meta = await this.#createEventImpl(this.baseUrl, name, this.#token, { fields });
      await this.#persistActive(meta.id);
      this.selectEvent(meta);
      return meta;
    }
  }

  /**
   * Persist `id` as the Director's active event (issue #90), best-effort: the create/choose has
   * already obtained any needed token, so this set should succeed. A failure here is swallowed —
   * the RD still enters the event locally; the worst case is a reload that drops to the picker,
   * not a broken session.
   */
  async #persistActive(id: EventMeta['id']): Promise<void> {
    try {
      await this.#setActiveEventImpl(this.baseUrl, id, this.#token);
    } catch {
      /* leave the active event unset; entering locally still works */
    }
  }

  /**
   * Send a privileged command through the control client, recording any error for the UI.
   *
   * Full-trust first (issue #72, Slice 1b): the command is sent **without** prompting. Against
   * an open (unconfigured) Director it just succeeds — no prompt ever. Only if the Director
   * **rejects it for auth** (`Unauthorized`/`Forbidden` — it has a token configured) and no
   * token is held yet does the lazy prompt fire; on a token, the command is **retried once**
   * and the token reused for the rest of the session. Cancelling the prompt leaves the original
   * `Unauthorized` ack. Returns the raw `CommandAck` so callers branch on success.
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
    let ack = await this.#control.sendCommand(command);
    // Only an *auth* rejection with no token yet triggers the lazy prompt + one retry; any
    // other failure (a bad transition, a transport error) is surfaced as-is.
    if (!ack.ok && !this.#token && isAuthAck(ack) && (await this.#promptForToken())) {
      // setToken rebuilt #control with the token; resend on the new client.
      ack = (await this.#control?.sendCommand(command)) ?? ack;
    }
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
