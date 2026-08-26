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
  connectTimer,
  disconnectTimer,
  restartTimer,
  setCalibration,
  setNodeChannel,
  timerNodes,
  setTimerNodes,
  timerSignal,
  stopTimerSignal,
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
  setClassHidden,
  setEventClasses,
  setClassMembership,
  listFormats,
  listFormatSchemas,
  listChannels,
  createRound,
  updateRound,
  deleteRound,
  listChannelLayers,
  createChannelLayer,
  updateChannelLayer,
  deleteChannelLayer,
  listHeats,
  listRoundIssues,
  eventAudit,
  roundRanking,
  roundStandings,
  classStandings
} from '@gridfpv/protocol-client';
import type {
  ProtocolClient,
  ProtocolState,
  ConnectionStatus,
  CalibrationRequest,
  ChannelDispatch,
  ChannelRequest
} from '@gridfpv/protocol-client';
import { createControlClient } from './control.js';
import type { ControlClient } from './control.js';
import type {
  AdapterId,
  AuditEntry,
  ChannelCatalogEntry,
  ChannelLayers,
  Class,
  ClassId,
  ClassStandings,
  Command,
  CommandAck,
  CompetitorRef,
  LapList,
  CreateClassRequest,
  CreateEventRequest,
  CreatePilotRequest,
  CreateTimerRequest,
  EventAuditEntry,
  EventMeta,
  FillMode,
  FormatSchema,
  HeatId,
  HeatResult,
  HeatSummary,
  LayerId,
  LiveRaceState,
  MemberSlot,
  Pilot,
  PilotId,
  NewChannelLayerRequest,
  NewRoundReq,
  ProjectionBody,
  RankEntry,
  RoundDef,
  RoundId,
  RoundIssue,
  RoundStanding,
  Scope,
  SetChannelLayerRequest,
  SetTimerNodesRequest,
  SignalTraceView,
  Timer,
  TimerId,
  TimerNodes,
  TimerSignal,
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
 * The HTTP status a thrown request error carries, if any: a structural `status` field when the
 * error provides one, else the trailing `… failed: HTTP <status>` every protocol-client request
 * rejection ends with. Anything else — a transport error, or a message that merely *contains*
 * "401" somewhere (an event id like `evt-401`, a 500 body echoing a token) — resolves
 * `undefined`, so it can never masquerade as an auth status. (The old whole-message
 * `\b(401|403)\b` scan opened the token dialog on exactly those.)
 */
function httpStatusOf(e: unknown): number | undefined {
  if (
    e &&
    typeof e === 'object' &&
    'status' in e &&
    typeof (e as { status: unknown }).status === 'number'
  ) {
    return (e as { status: number }).status;
  }
  const msg = e instanceof Error ? e.message : String(e);
  const m = /\bfailed: HTTP (\d{3})$/.exec(msg);
  return m ? Number(m[1]) : undefined;
}

/**
 * Whether a thrown error from `createEvent` (and the other gated writes) is an HTTP **401/403**
 * — i.e. the Director is gating the action. Matched on the error's HTTP *status* (structural, or
 * the protocol client's anchored `failed: HTTP <status>` suffix — see {@link httpStatusOf}),
 * never on digits appearing anywhere in the message.
 */
function isAuthFailure(e: unknown): boolean {
  const status = httpStatusOf(e);
  return status === 401 || status === 403;
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

/**
 * The session's auth tier (auth-tls-model): `'rd'` is the loopback / token-holding operator who
 * may commit; `'readonly'` is the read-only pilot tier that may only observe. Drives UI gating of
 * mutating controls (the Director is the enforced boundary; this mirrors it client-side).
 */
export type SessionRole = 'rd' | 'readonly';

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
  /**
   * Measured **server − client** wall-clock offset in ms (see {@link syncServerClock}). The start
   * countdown + race clock read {@link serverNowMs} (Date.now + this) instead of raw `Date.now()`,
   * so an RD device whose clock differs from the Director's by ~1s still shows the countdown hitting
   * zero at the real race-go (the "Armed timer stops at ~1s" bug on a separate laptop). `0` until the
   * first sync (loopback ⇒ ~0 anyway).
   */
  serverClockOffsetMs = $state(0);
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
  /**
   * The **durable per-heat registration bindings** (competitor ref → pilot id), one map per heat,
   * read off each heat's own live-state fold (`?projection=live` over that heat's log window — the
   * same durable source Marshaling's `heatLiveState` uses) and cached for the life of the event.
   * The event-wide surfaces (Results, Audit) resolve competitor names from THIS, never just the
   * global live stream — whose `progress` only carries the *current* heat, so a finished
   * node-seeded heat's `node-0` seats rendered raw (the friendly-names rule, CLAUDE.md).
   * Populated on demand via {@link ensureHeatBindings}; `$state.raw`: replaced wholesale per pull.
   */
  heatBindings = $state.raw<ReadonlyMap<HeatId, ReadonlyMap<CompetitorRef, PilotId>>>(new Map());
  /** The last control-path error surfaced to the RD (cleared on the next send). */
  lastCommandError = $state<CommandAck['error']>(undefined);
  /**
   * The session's **role** (#80, auth tiers): `'rd'` may commit corrections; `'readonly'` (the
   * read-only pilot tier) sees the record but cannot change it. The Director is the real boundary
   * (it gates control via `ControlAuth`); this is the **UI reflection** of that — mutating controls
   * hide/disable when the role is `'readonly'`, so a pilot view never shows actions it can't take.
   * Defaults to `'rd'` (the loopback / token-holding operator). The read-only-pilot wiring that
   * *sets* this from the connection tier is a later slice; the gate is first-class now.
   */
  role = $state<SessionRole>('rd');
  /**
   * The current heat's marshaling lap list (`?projection=laps`), pulled via
   * {@link refreshMarshaling}. The live read stream carries only `LiveRaceState`, so the
   * Marshaling screen reads the heat's laps (with marshaling folded in, each lap carrying its
   * `start_ref`/`end_ref` command target) here, refreshed whenever the live stream signals a change.
   */
  lapList = $state.raw<LapList | undefined>(undefined);
  /**
   * The current heat's marshaling **audit trail** (`?projection=audit`, #55) — the
   * reverse-chronological "what changed, when, what kind" the defensible-results panel renders.
   * Pulled alongside {@link lapList} and refreshed on every correction.
   */
  marshalingAudit = $state.raw<AuditEntry[] | undefined>(undefined);
  /**
   * The current heat's captured **RSSI signal trace** (`?projection=signal`, marshaling Slice 1) —
   * one per-competitor trace of streaming-cadence RSSI samples plus the enter/exit thresholds the
   * timer detected against. Pulled alongside {@link lapList} / {@link marshalingAudit} by
   * {@link refreshMarshaling}; the Marshaling screen renders it as the signal-as-evidence graph
   * (Slice 4) when present, and falls back to the lap-only layout when a heat has **no trace**
   * (a sim heat — `competitors` empty). `$state.raw`: an immutable whole replaced per pull.
   */
  signalTrace = $state.raw<SignalTraceView | undefined>(undefined);
  /**
   * The marshaled heat's own `LiveRaceState` (`?projection=live`), folded over **that heat's** log
   * window — pulled alongside the lap list / audit by {@link refreshMarshaling}. This is distinct
   * from {@link liveState}, which is the global live stream's *current* heat: the marshaled heat is
   * frequently NOT the current one (a finished / non-current heat under review), so its live stream
   * progress is empty or stale. The heat-scope fold carries the heat's durable `progress[].pilot`
   * registration bindings (the `node-0 → pilot` bind lives in the heat window's `CompetitorRegistered`
   * facts), which the Marshaling screen feeds to the competitor-name resolver so a node-seeded /
   * finished heat resolves callsigns — not the raw "node-0" ref. `$state.raw`: replaced per pull.
   */
  heatLiveState = $state.raw<LiveRaceState | undefined>(undefined);
  /** Whether this session may issue mutating marshaling commands (an RD, not a read-only pilot). */
  get canControl(): boolean {
    return this.role === 'rd';
  }
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
  /** The heat {@link heatResult} was fetched for — so a heat change / Revert can drop it. */
  #heatResultHeat: HeatId | undefined;
  /** Heats whose {@link heatBindings} read is in flight (dedupes concurrent `ensure` calls). */
  #heatBindingsInFlight = new Set<HeatId>();
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
  #connectTimerImpl: typeof connectTimer;
  #disconnectTimerImpl: typeof disconnectTimer;
  #restartTimerImpl: typeof restartTimer;
  #setCalibrationImpl: typeof setCalibration;
  #setNodeChannelImpl: typeof setNodeChannel;
  #timerSignalImpl: typeof timerSignal;
  #stopTimerSignalImpl: typeof stopTimerSignal;
  #timerNodesImpl: typeof timerNodes;
  #setTimerNodesImpl: typeof setTimerNodes;
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
  #setClassHiddenImpl: typeof setClassHidden;
  #setEventClassesImpl: typeof setEventClasses;
  #setClassMembershipImpl: typeof setClassMembership;
  #listFormatsImpl: typeof listFormats;
  #listFormatSchemasImpl: typeof listFormatSchemas;
  #listChannelsImpl: typeof listChannels;
  #createRoundImpl: typeof createRound;
  #updateRoundImpl: typeof updateRound;
  #deleteRoundImpl: typeof deleteRound;
  #listChannelLayersImpl: typeof listChannelLayers;
  #createChannelLayerImpl: typeof createChannelLayer;
  #updateChannelLayerImpl: typeof updateChannelLayer;
  #deleteChannelLayerImpl: typeof deleteChannelLayer;
  #listHeatsImpl: typeof listHeats;
  #listRoundIssuesImpl: typeof listRoundIssues;
  #eventAuditImpl: typeof eventAudit;
  #roundRankingImpl: typeof roundRanking;
  #roundStandingsImpl: typeof roundStandings;
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
    connectTimerImpl?: typeof connectTimer;
    disconnectTimerImpl?: typeof disconnectTimer;
    restartTimerImpl?: typeof restartTimer;
    setCalibrationImpl?: typeof setCalibration;
    setNodeChannelImpl?: typeof setNodeChannel;
    timerSignalImpl?: typeof timerSignal;
    stopTimerSignalImpl?: typeof stopTimerSignal;
    timerNodesImpl?: typeof timerNodes;
    setTimerNodesImpl?: typeof setTimerNodes;
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
    setClassHiddenImpl?: typeof setClassHidden;
    setEventClassesImpl?: typeof setEventClasses;
    setClassMembershipImpl?: typeof setClassMembership;
    listFormatsImpl?: typeof listFormats;
    listFormatSchemasImpl?: typeof listFormatSchemas;
    listChannelsImpl?: typeof listChannels;
    createRoundImpl?: typeof createRound;
    updateRoundImpl?: typeof updateRound;
    deleteRoundImpl?: typeof deleteRound;
    listChannelLayersImpl?: typeof listChannelLayers;
    createChannelLayerImpl?: typeof createChannelLayer;
    updateChannelLayerImpl?: typeof updateChannelLayer;
    deleteChannelLayerImpl?: typeof deleteChannelLayer;
    listHeatsImpl?: typeof listHeats;
    listRoundIssuesImpl?: typeof listRoundIssues;
    eventAuditImpl?: typeof eventAudit;
    roundRankingImpl?: typeof roundRanking;
    roundStandingsImpl?: typeof roundStandings;
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
    this.#connectTimerImpl = opts?.connectTimerImpl ?? connectTimer;
    this.#disconnectTimerImpl = opts?.disconnectTimerImpl ?? disconnectTimer;
    this.#restartTimerImpl = opts?.restartTimerImpl ?? restartTimer;
    this.#setCalibrationImpl = opts?.setCalibrationImpl ?? setCalibration;
    this.#setNodeChannelImpl = opts?.setNodeChannelImpl ?? setNodeChannel;
    this.#timerSignalImpl = opts?.timerSignalImpl ?? timerSignal;
    this.#stopTimerSignalImpl = opts?.stopTimerSignalImpl ?? stopTimerSignal;
    this.#timerNodesImpl = opts?.timerNodesImpl ?? timerNodes;
    this.#setTimerNodesImpl = opts?.setTimerNodesImpl ?? setTimerNodes;
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
    this.#setClassHiddenImpl = opts?.setClassHiddenImpl ?? setClassHidden;
    this.#setEventClassesImpl = opts?.setEventClassesImpl ?? setEventClasses;
    this.#setClassMembershipImpl = opts?.setClassMembershipImpl ?? setClassMembership;
    this.#listFormatsImpl = opts?.listFormatsImpl ?? listFormats;
    this.#listFormatSchemasImpl = opts?.listFormatSchemasImpl ?? listFormatSchemas;
    this.#listChannelsImpl = opts?.listChannelsImpl ?? listChannels;
    this.#createRoundImpl = opts?.createRoundImpl ?? createRound;
    this.#updateRoundImpl = opts?.updateRoundImpl ?? updateRound;
    this.#deleteRoundImpl = opts?.deleteRoundImpl ?? deleteRound;
    this.#listChannelLayersImpl = opts?.listChannelLayersImpl ?? listChannelLayers;
    this.#createChannelLayerImpl = opts?.createChannelLayerImpl ?? createChannelLayer;
    this.#updateChannelLayerImpl = opts?.updateChannelLayerImpl ?? updateChannelLayer;
    this.#deleteChannelLayerImpl = opts?.deleteChannelLayerImpl ?? deleteChannelLayer;
    this.#listHeatsImpl = opts?.listHeatsImpl ?? listHeats;
    this.#listRoundIssuesImpl = opts?.listRoundIssuesImpl ?? listRoundIssues;
    this.#eventAuditImpl = opts?.eventAuditImpl ?? eventAudit;
    this.#roundRankingImpl = opts?.roundRankingImpl ?? roundRanking;
    this.#roundStandingsImpl = opts?.roundStandingsImpl ?? roundStandings;
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
   * Hide or un-hide a directory class (`PUT /classes/{id}/hidden`) — hide/archive classes. RD-gated,
   * full-trust first like the other class writes. Hiding is a **visibility preference**, not an edit
   * — valid for **built-in** classes too: the class stays in the directory and the main Classes
   * view, it is just filtered out of the per-event class picker. The choice persists server-side and
   * survives a restart (including the built-in re-seed). Returns the updated {@link Class} (with its
   * fresh `hidden` flag), `undefined` on a cancelled token prompt, or throws on a non-auth failure
   * (an unknown id is a **404**).
   */
  setClassHidden(id: ClassId, hidden: boolean): Promise<Class | undefined> {
    return this.#privilegedWrite((token) =>
      this.#setClassHiddenImpl(this.baseUrl, id, hidden, token)
    );
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
   * **Hold** a live connection to a RotorHazard timer (`POST /timers/{id}/connect`) — issue #383.
   *
   * Deliberately **event-independent**: this is how the Timers screen answers "is this timer even
   * reachable?" while an RD is setting up at a venue, before any event exists. The Director's
   * reconciler dials the held timer on its next tick and publishes the same `status` /
   * `plugin` the event-driven connection does, so the existing badges tell the story.
   *
   * Returns the updated {@link Timer}, `undefined` on a cancelled token prompt, or throws
   * otherwise (the built-in Mock has nothing to dial and answers **400**).
   */
  connectTimer(id: TimerId): Promise<Timer | undefined> {
    return this.#privilegedWrite((token) => this.#connectTimerImpl(this.baseUrl, id, token));
  }

  /**
   * **Release** a manually-held RotorHazard connection (`POST /timers/{id}/disconnect`) — #383.
   * The hold is explicit, so this is the only thing that clears it. If the active event also
   * selects the timer its connection stays up; only the manual hold is released. Returns the
   * updated {@link Timer}, `undefined` on a cancelled token prompt, or throws otherwise.
   */
  disconnectTimer(id: TimerId): Promise<Timer | undefined> {
    return this.#privilegedWrite((token) => this.#disconnectTimerImpl(this.baseUrl, id, token));
  }

  /**
   * Restart a RotorHazard timer's server (`POST /timers/{id}/restart`) — the guided plugin
   * install's last step (#386), so installing the plugin never means opening RotorHazard's own web
   * UI. Resolves to the {@link Timer}, `undefined` on a cancelled token prompt, or throws on any
   * other failure — including the Director's **refusal while a race is in progress on the timer**
   * (a 400 whose message names the heat), which the caller surfaces verbatim.
   */
  restartTimer(id: TimerId): Promise<Timer | undefined> {
    return this.#privilegedWrite((token) => this.#restartTimerImpl(this.baseUrl, id, token));
  }

  /**
   * Set a timer node's enter/exit detection thresholds (`POST /timers/{id}/calibration`, #355) —
   * the Tune page's write half. Resolves when the Director **accepted** the change, `undefined` on
   * a cancelled token prompt, or throws on any other failure (including the Director's refusal for
   * a Mock, a disconnected timer, or an unknown node, whose message the caller surfaces verbatim).
   *
   * Accepted is not applied: RotorHazard does not echo a level set, so the Tune page confirms by
   * watching `NodeSignal.enter_at` / `exit_at` on the signal feed it is already polling.
   */
  setCalibration(id: TimerId, request: CalibrationRequest): Promise<void | undefined> {
    return this.#privilegedWrite((token) =>
      this.#setCalibrationImpl(this.baseUrl, id, request, token)
    );
  }

  /**
   * Set a timer node's **channel** (`POST /timers/{id}/channel`, #413) — the Tune page's other
   * write. Resolves with the Director's `ChannelDispatch` when it **accepted** the change,
   * `undefined` on a cancelled token prompt, or throws on any other failure (a Mock, a disconnected
   * timer, a scored heat running on it, a disabled or non-existent node, a channel a Fixed timer
   * cannot tune to — each message already phrased for the RD, so the caller surfaces it verbatim).
   *
   * Accepted is not applied: the page confirms by watching `NodeSignal.frequency_mhz` come back on
   * the signal feed it is already polling. The dispatch is still worth reading for the one thing
   * the page cannot know on its own — whether the node's stored thresholds were tuned on a
   * different channel.
   */
  setNodeChannel(id: TimerId, request: ChannelRequest): Promise<ChannelDispatch | undefined> {
    return this.#privilegedWrite((token) =>
      this.#setNodeChannelImpl(this.baseUrl, id, request, token)
    );
  }

  /**
   * Poll a timer's live tuning signal (`GET /timers/{id}/signal`, #355) — the Tune page's read half.
   *
   * Deliberately **not** a `#privilegedWrite`: this runs several times a second, and the lazy
   * token prompt that path opens on a 401 would fire a dialog per poll. It sends whatever token the
   * session already holds and lets the failure surface, which the page renders as a lost feed.
   *
   * Every call renews the Director's lease on the stream — so this *is* the subscription, and the
   * page's cadence is what keeps the timer streaming. See {@link stopTimerSignal}.
   */
  timerSignal(id: TimerId, opts: { signal?: AbortSignal } = {}): Promise<TimerSignal> {
    return this.#timerSignalImpl(this.baseUrl, id, { token: this.#token, signal: opts.signal });
  }

  /**
   * End a timer's tuning stream now (`POST /timers/{id}/signal/stop`, #355) — what the Tune page
   * calls when it leaves, so a timer is not left streaming to nobody for the rest of its lease.
   *
   * Same reasoning as {@link timerSignal} for skipping the token prompt: this fires on teardown,
   * where a modal dialog would be absurd, and the lease is the backstop if it fails.
   */
  stopTimerSignal(id: TimerId): Promise<void> {
    return this.#stopTimerSignalImpl(this.baseUrl, id, this.#token);
  }

  /**
   * Read a timer's **node configuration** (`GET /timers/{id}/nodes`, #412) — what the hardware
   * reported, what GridFPV is configured for, which nodes are enabled, and any drift between them.
   *
   * An open read like {@link timerSignal}, and for the same reason it is not a `#privilegedWrite`:
   * it is used to *render* a screen, and a token prompt fired by a render would be absurd. It sends
   * whatever token the session already holds.
   */
  timerNodes(id: TimerId, opts: { signal?: AbortSignal } = {}): Promise<TimerNodes> {
    return this.#timerNodesImpl(this.baseUrl, id, { token: this.#token, signal: opts.signal });
  }

  /**
   * Write a timer's **node configuration** (`PUT /timers/{id}/nodes`, #412) — the width override
   * (`node_count: null` clears it and follows the hardware again) and/or the enabled node set.
   *
   * A **decision**, so it is RD-gated and persisted: a node disabled here stays disabled across a
   * reconnect. Resolves to the resulting {@link TimerNodes}, `undefined` on a cancelled token
   * prompt, or throws — including the Director's refusals (a zero width, or an edit leaving no node
   * enabled), whose messages are already phrased for the RD and are surfaced verbatim.
   */
  setTimerNodes(id: TimerId, request: SetTimerNodesRequest): Promise<TimerNodes | undefined> {
    return this.#privilegedWrite((token) =>
      this.#setTimerNodesImpl(this.baseUrl, id, request, token)
    );
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

  // --- Event channel layers (#117 S2) -----------------------------------------------------------
  //
  // A **layer** is one complete tuning of the event's timer — one channel per enabled node, drawn
  // from the timer's *allowed* set. Layers are **event** state: the read is open, the writes are
  // control-gated, and every one of them keeps {@link currentEvent}'s `channel_layers` in step so
  // the cached meta never disagrees with what the Director holds.
  //
  // Note what these do NOT do: no layer write touches a `Timer`. That is the whole point — the
  // Timers page's checkboxes edit the global allowed set (what a timer may *ever* use), and a layer
  // is the event's own answer to what goes on which node.

  /**
   * List the current event's **channel layers** (`GET /events/{id}/layers`, open, no token) — #117
   * S2. Resolves the {@link ChannelLayers} view — the layers in definition order plus the advisory
   * cross-layer `overlaps` the Director computed. Resolves an empty view when no event is selected,
   * so a caller never has to special-case the picker; rejects on a transport/HTTP failure.
   */
  listChannelLayers(): Promise<ChannelLayers> {
    const event = this.currentEvent;
    if (!event) return Promise.resolve({ layers: [], overlaps: [] });
    return this.#listChannelLayersImpl(this.baseUrl, event.id, { token: this.#token });
  }

  /**
   * Define a **channel layer** on the current event (`POST /events/{id}/layers`) — #117 S2.
   * RD-gated; the layer id is auto-generated server-side.
   *
   * **Omit `nodes` to seed the layer from the timer's allowed set** — the global→event seam. No-op
   * (resolves `undefined`) when no event is selected. On success the returned {@link ChannelLayers}
   * view also re-homes {@link currentEvent}'s `channel_layers`; returns it, `undefined` on a
   * cancelled prompt, or throws with the Director's own refusal sentence (already phrased for the
   * RD — it names the node, the channel and the timer).
   */
  async createChannelLayer(request: NewChannelLayerRequest): Promise<ChannelLayers | undefined> {
    const event = this.currentEvent;
    if (!event) return undefined;
    const view = await this.#privilegedWrite((token) =>
      this.#createChannelLayerImpl(this.baseUrl, event.id, request, token)
    );
    if (view) this.currentEvent = { ...event, channel_layers: view.layers };
    return view;
  }

  /**
   * Replace a **channel layer**'s name and mapping (`PUT /events/{id}/layers/{layer}`) — #117 S2.
   * RD-gated; the id is not editable and the whole mapping is replaced wholesale, re-validated as on
   * create. No-op (resolves `undefined`) when no event is selected. On success the returned
   * {@link ChannelLayers} view re-homes {@link currentEvent}'s `channel_layers`; returns it,
   * `undefined` on a cancelled prompt, or throws with the Director's own refusal sentence.
   */
  async updateChannelLayer(
    layerId: LayerId,
    request: SetChannelLayerRequest
  ): Promise<ChannelLayers | undefined> {
    const event = this.currentEvent;
    if (!event) return undefined;
    const view = await this.#privilegedWrite((token) =>
      this.#updateChannelLayerImpl(this.baseUrl, event.id, layerId, request, token)
    );
    if (view) this.currentEvent = { ...event, channel_layers: view.layers };
    return view;
  }

  /**
   * Remove a **channel layer** (`DELETE /events/{id}/layers/{layer}`) — #117 S2. RD-gated. No-op
   * (resolves `undefined`) when no event is selected. On success the returned {@link ChannelLayers}
   * view re-homes {@link currentEvent}'s `channel_layers`; returns it, `undefined` on a cancelled
   * prompt, or throws (an unknown layer is a **404**).
   */
  async deleteChannelLayer(layerId: LayerId): Promise<ChannelLayers | undefined> {
    const event = this.currentEvent;
    if (!event) return undefined;
    const view = await this.#privilegedWrite((token) =>
      this.#deleteChannelLayerImpl(this.baseUrl, event.id, layerId, token)
    );
    if (view) this.currentEvent = { ...event, channel_layers: view.layers };
    return view;
  }

  // --- Heats (race redesign Slice 3b) -----------------------------------------------------------
  // The Heats half of the Rounds & Heats stage. `fillRound` and the tagged `scheduleHeat` are
  // privileged **control commands** (they go through the same `send` path as every transition);
  // `listHeats` is an open read of the round-tagged heats list the UI groups by round.

  /**
   * Fill a round (`Command::FillRound`) — race redesign Slice 3b; fill-all added #216. Asks the
   * per-event round engine to append format-generated `HeatScheduled`(s) tagged with this round (and
   * its class when single-class):
   *
   *  - `'Next'` (the default) appends the **single** next heat — the interactive single-step used by
   *    the dynamic Open Practice format and as the building block.
   *  - `'All'` fills the **whole** round in one round-trip, looping the generator until the round is
   *    complete — for the deterministic formats (Time Trials, Round Robin, Multi-Main, brackets).
   *
   * A successful ack means the engine reached its terminal state for that mode (a heat/heats were
   * appended, the round is already complete, or its outstanding heat must be scored first — all
   * expected no-ops), so the caller re-reads {@link listHeats} to see which heats appeared. Sent
   * through the control path (full-trust first → lazy token); returns the raw {@link CommandAck}.
   */
  fillRound(round: RoundId, mode: FillMode = 'Next'): Promise<CommandAck> {
    return this.send({ FillRound: { round, mode } });
  }

  /**
   * **Select the current heat** in Live control (`Command::SetCurrentHeat`) — the RD's explicit
   * "show/control *this* heat". The live `current_heat` derivation then follows the choice, so the
   * sheet/clock/leaderboard and the transition buttons target the selected heat. Sent through the
   * control path (full-trust first → lazy token); returns the raw {@link CommandAck} like
   * {@link send}.
   */
  setCurrentHeat(heat: HeatId): Promise<CommandAck> {
    return this.send({ SetCurrentHeat: { heat } });
  }

  /**
   * Schedule a heat by hand (`Command::ScheduleHeat`) — race redesign Slice 3b, the manual build that
   * replaces the retired free-text NewHeat form. The lineup is real {@link CompetitorRef}s drawn from
   * a round's eligible class members (no typed names), and the heat is **tagged** with the round and
   * its class so it lands in that round's heats list. An optional `label` overrides the derived
   * display name (the RD's custom heat name); omitted (undefined/empty) keeps the auto-name. Sent
   * through the control path; returns the raw {@link CommandAck}.
   */
  scheduleHeat(
    heat: HeatId,
    lineup: CompetitorRef[],
    tag: { class?: ClassId; round?: RoundId; label?: string } = {}
  ): Promise<CommandAck> {
    return this.send({
      ScheduleHeat: { heat, lineup, class: tag.class, round: tag.round, label: tag.label }
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

  /**
   * List the current event's **round issues** (`GET /events/{id}/round-issues`, open, no token) —
   * #416. One {@link RoundIssue} per stored round seat that cannot record a lap: a node beyond the
   * primary timer's width, one the RD disabled, or one beyond what the timer reported.
   *
   * The Rounds & Heats stage renders these on the round they belong to, beside the edit control
   * that repairs them. An **empty list means nothing is wrong**; a failed read is surfaced by the
   * caller rather than swallowed, because silently showing a seat that cannot record is the exact
   * behaviour this read exists to stop. No-op (resolves `[]`) when no event is selected.
   */
  listRoundIssues(): Promise<RoundIssue[]> {
    const event = this.currentEvent;
    if (!event) return Promise.resolve([]);
    return this.#listRoundIssuesImpl(this.baseUrl, event.id, { token: this.#token });
  }

  /**
   * Read the current event's **event-wide audit trail** (`GET /events/{id}/audit`) — every heat's
   * marshaling audit fold, heat-tagged ({@link EventAuditEntry}) and merged newest first. This is
   * what the Audit page renders and filters; Marshaling's per-heat trail keeps reading the
   * heat-scoped `?projection=audit` snapshot ({@link refreshMarshaling}), and both derive from the
   * same server-side fold so they can never disagree. Open to read (no token, role-agnostic).
   * No-op (resolves `[]`) when no event is selected; rejects on a transport/HTTP failure (the
   * screen surfaces it — no silent empty list, #340).
   */
  eventAudit(): Promise<EventAuditEntry[]> {
    const event = this.currentEvent;
    if (!event) return Promise.resolve([]);
    return this.#eventAuditImpl(this.baseUrl, event.id, { token: this.#token });
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
   * Read a **round's standings** (`GET /events/{id}/rounds/{round}/standings`) — the time-trial
   * (timed_qual) display. One {@link RoundStanding} per pilot for the round: each pilot's best single
   * lap plus the win-condition metric they're ranked on (best-N-consecutive time, lap count, or best
   * lap), in the same order (and with the same positions) as {@link roundRanking}. No-op (resolves
   * `[]`) when no event is selected; rejects on a non-2xx / transport failure (an unscorable round is
   * a 400 the stage surfaces).
   */
  roundStandings(roundId: RoundId): Promise<RoundStanding[]> {
    const event = this.currentEvent;
    if (!event) return Promise.resolve([]);
    return this.#roundStandingsImpl(this.baseUrl, event.id, roundId, { token: this.#token });
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
      this.#dropStaleHeatResult();
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
      // Re-measure the server clock offset on the same cadence — cheap, and keeps the countdown /
      // race clock honest as the RD device's clock drifts.
      void this.syncServerClock();
    };
    void poll();
    this.#timerPoll = setInterval(poll, TIMER_POLL_MS);
  }

  /**
   * The Director's wall clock in **ms**, via the measured {@link serverClockOffsetMs}. The start
   * countdown ({@link useArmingClock}) and race clock ({@link useRaceClock}) read this instead of
   * raw `Date.now()`, so a client device whose clock differs from the Director's still reads true.
   */
  serverNowMs(): number {
    return Date.now() + this.serverClockOffsetMs;
  }

  /**
   * Measure the server↔client wall-clock offset from `GET /time` (epoch µs), NTP-style: assume
   * symmetric latency, so the server's clock at the request *midpoint* is the returned instant, and
   * `offset = server − clientMidpoint`. On a millisecond LAN this is exact to within the round-trip;
   * the skew it corrects is ~1s. Keeps the last offset on any failure. Open read, no token.
   */
  async syncServerClock(): Promise<void> {
    const base = this.baseUrl.endsWith('/') ? this.baseUrl.slice(0, -1) : this.baseUrl;
    try {
      const t0 = Date.now();
      const resp = await globalThis.fetch(base + '/time');
      const t1 = Date.now();
      if (!resp.ok) return;
      const body: unknown = await resp.json();
      const nowMicros =
        body &&
        typeof body === 'object' &&
        typeof (body as { now_micros?: unknown }).now_micros === 'number'
          ? (body as { now_micros: number }).now_micros
          : undefined;
      if (nowMicros === undefined) return;
      this.serverClockOffsetMs = nowMicros / 1000 - (t0 + t1) / 2;
    } catch {
      /* keep the last good offset */
    }
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
      this.#dropStaleHeatResult();
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
    this.#heatResultHeat = undefined;
    // The per-heat binding cache is event-scoped (heat ids and node-seat binds from one event
    // must never resolve names under the next).
    this.heatBindings = new Map();
    this.#heatBindingsInFlight.clear();
    this.lapList = undefined;
    this.marshalingAudit = undefined;
    this.heatLiveState = undefined;
    // The signal trace too — leaving it set rendered the PREVIOUS event's RSSI evidence under
    // the next event's heat when its own trace read failed or lagged (fabricated evidence on
    // the defensible-results surface).
    this.signalTrace = undefined;
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
    // A successful Revert re-opens the heat's result — a stored scored {@link heatResult} for
    // that heat no longer stands, so drop it (the Results export must not embed it).
    if (ack.ok && 'Revert' in command && command.Revert.heat === this.#heatResultHeat) {
      this.heatResult = undefined;
      this.#heatResultHeat = undefined;
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
        this.#heatResultHeat = heat;
        return this.heatResult;
      }
    } catch {
      /* leave heatResult unchanged */
    }
    return undefined;
  }

  /**
   * Drop {@link heatResult} once it no longer describes the heat it was fetched for: it is set on
   * Finalize and was previously never cleared, so after moving on to the next heat the Results
   * JSON export embedded the PREVIOUS heat's result. Called on every live-stream state — clears
   * when the current heat moves off {@link #heatResultHeat}; {@link send} additionally clears it
   * when that heat's result is **Reverted** (re-opened, so no scored result stands).
   */
  #dropStaleHeatResult(): void {
    if (this.heatResult === undefined) return;
    const current = this.liveState?.current_heat;
    if (current != null && current !== this.#heatResultHeat) {
      this.heatResult = undefined;
      this.#heatResultHeat = undefined;
    }
  }

  /**
   * Ensure {@link heatBindings} holds the **durable registration bindings** for each of `heats`,
   * fetching any missing heat's own live-state fold (`?projection=live` — the heat-window fold
   * whose `progress[].pilot` carries the heat's `CompetitorRegistered` binds; see
   * {@link refreshMarshaling}'s note) and caching the extracted ref → pilot map. Already-cached
   * and in-flight heats are skipped, so the event-wide screens can call this freely per render
   * tick. A failed read leaves the heat uncached (a later call retries); the resolver then simply
   * falls back for that heat's refs rather than erroring (#340 doesn't apply — these enrich
   * names, the primary reads surface their own failures).
   */
  async ensureHeatBindings(heats: HeatId[]): Promise<void> {
    const missing = heats.filter(
      (h) => !this.heatBindings.has(h) && !this.#heatBindingsInFlight.has(h)
    );
    if (missing.length === 0) return;
    for (const h of missing) this.#heatBindingsInFlight.add(h);
    try {
      const folds = await Promise.all(
        missing.map((h) => this.#fetchHeatProjection<LiveRaceState>(h, 'live', 'LiveRaceState'))
      );
      const next = new Map(this.heatBindings);
      let changed = false;
      folds.forEach((live, i) => {
        if (!live) return; // failed read — stay uncached so the next ensure retries
        const bound = new Map<CompetitorRef, PilotId>();
        for (const p of live.progress ?? []) if (p.pilot != null) bound.set(p.competitor, p.pilot);
        next.set(missing[i], bound);
        changed = true;
      });
      if (changed) this.heatBindings = next;
    } finally {
      for (const h of missing) this.#heatBindingsInFlight.delete(h);
    }
  }

  /**
   * Set the session role (#80 auth tiers). `'readonly'` hides/disables mutating controls across
   * the console (e.g. the Marshaling actions); `'rd'` restores them. The Director still enforces
   * the boundary — this only reflects it in the UI so a read-only viewer never sees actions it
   * cannot take.
   */
  setRole(role: SessionRole): void {
    this.role = role;
  }

  /**
   * Pull the current heat's marshaling projections — the lap list (`?projection=laps`) and the
   * audit trail (`?projection=audit`, #55) — and store them on {@link lapList} / {@link marshalingAudit}.
   *
   * The live read stream carries only `LiveRaceState`, so the Marshaling screen reads these tighter
   * heat-scope snapshots and re-pulls them whenever the live stream signals a change (a correction
   * re-folds both). Corrections therefore flow into the lap list, the audit, AND standings (the live
   * stream) by the same "append → re-fold → re-fetch" path — nothing reconciled locally. Open to
   * read (no token); a failed fetch leaves the last good value.
   */
  async refreshMarshaling(heat: HeatId): Promise<void> {
    const event = this.currentEvent;
    if (!event) return;
    const [laps, audit, signal, live] = await Promise.all([
      this.#fetchHeatProjection<LapList>(heat, 'laps', 'LapList'),
      this.#fetchHeatProjection<AuditEntry[]>(heat, 'audit', 'MarshalingAudit'),
      this.#fetchHeatProjection<SignalTraceView>(heat, 'signal', 'SignalTrace'),
      // The marshaled heat's own live-state fold — for its durable `progress[].pilot` registration
      // bindings, so the name resolver renders callsigns for a node-seeded / finished heat whose
      // global live stream (`this.liveState`) carries no progress for it (the raw-"node-0" bug).
      this.#fetchHeatProjection<LiveRaceState>(heat, 'live', 'LiveRaceState')
    ]);
    if (laps !== undefined) this.lapList = laps;
    if (audit !== undefined) this.marshalingAudit = audit;
    if (signal !== undefined) this.signalTrace = signal;
    if (live !== undefined) this.heatLiveState = live;
  }

  /**
   * One-shot read of a heat-scope projection snapshot, narrowing `body[bodyKey]`. Shared by the
   * result / marshaling reads. Open to read; returns `undefined` on any failure (caller keeps the
   * last value).
   */
  async #fetchHeatProjection<T>(
    heat: HeatId,
    projection: string,
    bodyKey: string
  ): Promise<T | undefined> {
    const event = this.currentEvent;
    if (!event) return undefined;
    const base = this.baseUrl.endsWith('/') ? this.baseUrl.slice(0, -1) : this.baseUrl;
    const headers: Record<string, string> = {};
    if (this.#token) headers.Authorization = `Bearer ${this.#token}`;
    try {
      const resp = await globalThis.fetch(
        `${base}/events/${encodeURIComponent(event.id)}/snapshot/heat/${encodeURIComponent(
          heat
        )}?projection=${projection}`,
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
        bodyKey in snap.body
      ) {
        return (snap.body as Record<string, T>)[bodyKey];
      }
    } catch {
      /* leave the caller's value unchanged */
    }
    return undefined;
  }
}
