/**
 * The console session: the current **event**, the RD token, and every call to the Director.
 *
 * The Director is the page's own origin (defaulted from `location.origin`), so there is no
 * address to log into. What is left of auth is the **RD token**, and it is requested **lazily**:
 * reading needs no token; the token is only asked for when a write is refused.
 *
 * Lifecycle:
 *
 *   • No event open → the app is on the hub or one of its pages (`currentEvent === undefined`).
 *   • `chooseEvent(meta)` / `createEventAndEnter(name)` make the event the Director's active
 *     event and open it here.
 *   • `leaveEvent()` closes it locally. The Director's active event is left as it is.
 *
 * The RD token lives in memory, mirrored to `sessionStorage` so a tab reload inside the
 * same session keeps it, but it never persists to disk. A {@link TokenProvider} (set by
 * the shell) lets a write prompt for the token only when the Director actually asks for one.
 *
 * **Full-trust by default (#72):** writes are sent **without** a token first. Only if the
 * Director responds **401/403** (it has a token configured and is gating control) does the lazy
 * token dialog open; the entered token is then reused for the rest of the session and the write
 * is retried. Against an open (unconfigured) Director there is therefore **no prompt ever**.
 *
 * Everything reactive is Svelte 5 runes, so screens read `session.currentEvent` directly.
 */

import {
  listEvents,
  createEvent,
  deleteEvent,
  getActiveEvent,
  setActiveEvent,
  listPilots,
  createPilot,
  updatePilot,
  deletePilot,
  listClasses,
  createClass,
  updateClass,
  deleteClass,
  setClassHidden,
  getBuiltinClasses,
  restoreBuiltinClasses,
  listTimers,
  listPublicChannels,
  listDiscoveredTimers,
  createTimer,
  deleteTimer,
  renameTimer,
  getTimerConfig,
  putTimerConfig,
  updateEvent,
  listFormats,
  createFormat,
  updateFormat,
  deleteFormat,
  listProfiles,
  createProfile,
  updateProfile,
  deleteProfile,
  applyProfile,
  getVideoSettings,
  putVideoSettings,
  getVideoStatus,
  postVideoRender,
  postVideoFfmpeg,
  getData,
  postDataExport,
  dataExportUrl,
  postDataImport,
  deleteDataImport,
  getNetwork,
  getServerLink,
  putServerLink,
  replaceOnServer,
  getServerPeople,
  setPilotServerAccount,
  pullFromServer,
  putDoor,
  putAccessToken,
  putPublicAddress,
  getKiosks,
  newKioskCode,
  cancelKioskCode,
  removeKiosk,
  renameKiosk,
  getPublicHello,
  getPilotAccess,
  resetPilotCode,
  endPilotSession,
  pilotQrUrl,
  signInPilot,
  signOutPilot,
  signInDirector,
  signOutDirector,
  getOverlayLook,
  putOverlayLook,
  overlayLogoUrl,
  raceVideoUrl,
  putOverlayLogo,
  deleteOverlayLogo,
  getEventState,
  sendCommand,
  createBracket,
  createMains,
  createRoundRobin,
  postPass,
  getRaceSignal,
  subscribeChanges,
  subscribeTimerLive,
  isRequestFailure
} from '@gridfpv/protocol-client';
import type { TimerLiveHandlers } from '@gridfpv/protocol-client';
import type {
  AdapterId,
  BracketRequest,
  MainsRequest,
  RoundRobinRequest,
  Class,
  ClassId,
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
  EventMeta,
  EventView,
  Format,
  KioskView,
  Door,
  DoorRequest,
  NetworkView,
  LinkPeople,
  LinkSettings,
  LinkView,
  OverlayLook,
  OverlayPlacement,
  PassRequest,
  Pilot,
  PilotAccessView,
  PilotCodeView,
  PilotId,
  PullRequest,
  PublicHello,
  PublicPilot,
  RaceId,
  RaceSignal,
  RenderRequest,
  SavedFormat,
  SeatChannel,
  SeatProfile,
  SeatProfileRequest,
  Timer,
  TimerConfig,
  TimerProfiles,
  TimerView,
  UpdateClassRequest,
  UpdatePilotRequest,
  VideoSettings,
  VideoSettingsRequest,
  VideoStatus
} from '@gridfpv/types';

const TOKEN_STORAGE_KEY = 'gridfpv.rd.token';

/** The optional descriptive fields a create-event dialog may supply alongside the name. */
export type CreateEventFields = Omit<CreateEventRequest, 'name'>;

/**
 * Whether a thrown error from a gated write is an HTTP **401/403** — i.e. the Director is gating
 * the action, and the lazy token prompt should fire.
 *
 * Keyed on the {@link RequestFailure}'s structural `status`, **never on the words**. Every
 * protocol-client rejection carries the status as a field (#433) precisely so the message can be
 * the Director's own sentence — written for the RD, and free to change — without auth detection
 * riding on its wording. A text scan for `401` would open the token dialog on an event id like
 * `evt-401`.
 */
function isAuthFailure(e: unknown): boolean {
  return isRequestFailure(e) && (e.status === 401 || e.status === 403);
}

/**
 * Asks the RD for a control token. Returns the entered token, or `undefined` if the
 * RD cancelled. The shell wires this to a token `Dialog`; the session calls it lazily
 * the first time a write needs a token it doesn't already hold.
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

/**
 * **The session's outward seams, in one place.** Every protocol-client read/write the console
 * makes — the real implementations, which a {@link Session} inherits wholesale and a test
 * overrides by key (#459).
 *
 * This list *is* the type: {@link SessionApi} is `typeof realApi`, so adding an endpoint here is
 * the whole change — there is no second declaration to keep in step.
 */
const realApi = {
  listEvents,
  createEvent,
  deleteEvent,
  getActiveEvent,
  setActiveEvent,
  listPilots,
  createPilot,
  updatePilot,
  deletePilot,
  listClasses,
  createClass,
  updateClass,
  deleteClass,
  setClassHidden,
  getBuiltinClasses,
  restoreBuiltinClasses,
  listTimers,
  listPublicChannels,
  listDiscoveredTimers,
  createTimer,
  deleteTimer,
  renameTimer,
  getTimerConfig,
  putTimerConfig,
  updateEvent,
  listFormats,
  createFormat,
  updateFormat,
  deleteFormat,
  listProfiles,
  createProfile,
  updateProfile,
  deleteProfile,
  applyProfile,
  getVideoSettings,
  putVideoSettings,
  getVideoStatus,
  postVideoRender,
  postVideoFfmpeg,
  getData,
  postDataExport,
  dataExportUrl,
  postDataImport,
  deleteDataImport,
  getNetwork,
  getServerLink,
  putServerLink,
  replaceOnServer,
  getServerPeople,
  setPilotServerAccount,
  pullFromServer,
  putDoor,
  putAccessToken,
  putPublicAddress,
  getKiosks,
  newKioskCode,
  cancelKioskCode,
  removeKiosk,
  renameKiosk,
  getPublicHello,
  getPilotAccess,
  resetPilotCode,
  endPilotSession,
  pilotQrUrl,
  signInPilot,
  signOutPilot,
  signInDirector,
  signOutDirector,
  getOverlayLook,
  putOverlayLook,
  overlayLogoUrl,
  raceVideoUrl,
  putOverlayLogo,
  deleteOverlayLogo,
  getEventState,
  sendCommand,
  createBracket,
  createMains,
  createRoundRobin,
  postPass,
  getRaceSignal,
  subscribeChanges,
  subscribeTimerLive
};

/** The record of seams a {@link Session} calls outward through — see {@link realApi}. */
export type SessionApi = typeof realApi;

/**
 * The overrides a caller actually supplied. A key present but `undefined` is dropped rather than
 * spread over the real implementation, which lets a test helper forward optional overrides
 * straight through.
 */
function definedOnly(overrides: Partial<SessionApi> | undefined): Partial<SessionApi> {
  if (!overrides) return {};
  return Object.fromEntries(
    Object.entries(overrides).filter(([, v]) => v !== undefined)
  ) as Partial<SessionApi>;
}

/** The reactive console session — one instance, shared across screens. */
export class Session {
  /**
   * The event the console currently has open (#72), or `undefined` when none is. Opening an
   * event sets this; leaving clears it.
   */
  currentEvent = $state.raw<EventMeta | undefined>(undefined);
  /**
   * Whether the session is still **resolving the Director's active event** (#90) — the brief
   * window on load before we know whether an event is active. The shell shows a loading state
   * while this is `true`; it flips to `false` once {@link resolveActiveEvent} settles (success
   * or failure).
   */
  resolvingActiveEvent = $state(true);
  /** The base URL every call targets — the Director's origin. */
  baseUrl = $state(defaultBaseUrl());
  /** Whether an RD token is currently held. */
  hasToken = $state(false);

  // Non-reactive internals.
  #token: string | undefined;
  /** The shell's lazy token prompt (a `Dialog`); set via {@link setTokenProvider}. */
  #tokenProvider: TokenProvider | undefined;
  /**
   * The protocol-client seams, as **one injected record** (#459).
   *
   * Every call the session makes outward goes through here. Tests hand `opts.api` the keys they
   * want to steer and inherit {@link realApi} for the rest.
   */
  #api: SessionApi;

  constructor(opts?: {
    /**
     * Override any of the outward seams (see {@link SessionApi}); every key left out keeps its
     * real implementation. A key present but `undefined` is treated as absent, so a test helper
     * may forward `{ listPilots: overrides?.listPilots }` without blanking the real one.
     */
    api?: Partial<SessionApi>;
    baseUrl?: string;
    autoRestore?: boolean;
  }) {
    this.#api = { ...realApi, ...definedOnly(opts?.api) };
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

  /** Set (or replace) the held RD token. */
  setToken(token: string): void {
    const trimmed = token.trim();
    if (!trimmed) return;
    this.#token = trimmed;
    this.hasToken = true;
    persistToken(trimmed);
  }

  /** Forget the held RD token. Reads keep working (they are open). */
  clearToken(): void {
    this.#token = undefined;
    this.hasToken = false;
    persistToken(undefined);
  }

  /**
   * List the events the Director knows (`GET /events`) — open, no token. Used by the Events
   * page on load. Rejects on a transport/HTTP failure (the page shows it).
   */
  listEvents(): Promise<EventMeta[]> {
    return this.#api.listEvents(this.baseUrl, { token: this.#token });
  }

  // ── Pilot directory (issue #74) ────────────────────────────────────────────
  //
  // Pilots are **application-level** (a persisted directory the RD maintains once), not event
  // state — so these live on the session itself and are not gated on `currentEvent`. Reads are
  // open; writes are control-gated and follow the full-trust-first posture: attempted tokenless,
  // and only if the Director answers 401/403 does the lazy prompt fire once and the write retry
  // — see {@link #privilegedWrite}.

  /**
   * List every pilot in the directory (`GET /pilots`, open, no token). Backs the hub's Pilots
   * count and the Pilots page. Rejects on a transport/HTTP failure (the page surfaces it).
   */
  listPilots(): Promise<Pilot[]> {
    return this.#api.listPilots(this.baseUrl, { token: this.#token });
  }

  /**
   * Create a directory pilot (`POST /pilots`). The `callsign` is required (server **400** on
   * blank); the id is auto-generated. Returns the new {@link Pilot}, `undefined` on a cancelled
   * token prompt, or throws on a non-auth failure (a bad hex `color` / `country` is a **400** the
   * form surfaces).
   */
  createPilot(request: CreatePilotRequest): Promise<Pilot | undefined> {
    return this.#privilegedWrite((token) => this.#api.createPilot(this.baseUrl, request, token));
  }

  /**
   * Edit a directory pilot (`PUT /pilots/{id}`). The request carries only the fields that
   * changed: an omitted optional field is left unchanged, a present **`null`** clears it (the way
   * the form clears `color`/`country`), and a present value sets it. Returns the updated
   * {@link Pilot}, `undefined` on a cancelled prompt, or throws on a non-auth failure.
   */
  updatePilot(id: PilotId, request: UpdatePilotRequest): Promise<Pilot | undefined> {
    return this.#privilegedWrite((token) =>
      this.#api.updatePilot(this.baseUrl, id, request, token)
    );
  }

  /**
   * Remove a directory pilot (`DELETE /pilots/{id}`). Resolves to `true` once the delete
   * succeeds, `undefined` on a cancelled token prompt, or throws on any other failure (an unknown
   * id is a **404**). (`true` lets callers tell success from a cancelled prompt — the `DELETE`
   * has no body.)
   */
  deletePilot(id: PilotId): Promise<true | undefined> {
    return this.#privilegedWrite(async (token) => {
      await this.#api.deletePilot(this.baseUrl, id, token);
      return true as const;
    });
  }

  // ── Class directory (issue #84) ────────────────────────────────────────────
  //
  // Classes are **application-level** too, exactly like pilots: open reads, gated writes with the
  // same full-trust-first → lazy-token-prompt posture (see {@link #privilegedWrite}).

  /**
   * List every class in the directory (`GET /classes`, open, no token). Backs the hub's Classes
   * count and the Classes page. Rejects on a transport/HTTP failure (the page surfaces it).
   */
  listClasses(): Promise<Class[]> {
    return this.#api.listClasses(this.baseUrl, { token: this.#token });
  }

  /**
   * Create a directory class (`POST /classes`). The `name` is required (server **400** on blank);
   * the id is auto-generated. Returns the new {@link Class}, `undefined` on a cancelled prompt,
   * or throws on a non-auth failure.
   */
  createClass(request: CreateClassRequest): Promise<Class | undefined> {
    return this.#privilegedWrite((token) => this.#api.createClass(this.baseUrl, request, token));
  }

  /**
   * Edit a directory class (`PUT /classes/{id}`). The request carries only the fields that
   * changed: a present `name`/`source` replaces it, an omitted optional field is left unchanged,
   * a present **`null`** clears it (the way the form clears `reference`/`description`), and a
   * present value sets it. Returns the updated {@link Class}, `undefined` on a cancelled prompt,
   * or throws on a non-auth failure.
   */
  updateClass(id: ClassId, request: UpdateClassRequest): Promise<Class | undefined> {
    return this.#privilegedWrite((token) =>
      this.#api.updateClass(this.baseUrl, id, request, token)
    );
  }

  /**
   * Remove a directory class (`DELETE /classes/{id}`). Resolves to `true` once the delete
   * succeeds, `undefined` on a cancelled token prompt, or throws on any other failure (an unknown
   * id is a **404**). Mirrors {@link deletePilot}.
   */
  deleteClass(id: ClassId): Promise<true | undefined> {
    return this.#privilegedWrite(async (token) => {
      await this.#api.deleteClass(this.baseUrl, id, token);
      return true as const;
    });
  }

  /**
   * Hide or un-hide a directory class (`PUT /classes/{id}/hidden`). Hiding is a **visibility
   * preference**, not an edit — valid for **built-in** classes too: the class stays in the
   * directory and on the Classes page. The choice persists server-side and survives a restart
   * (including the built-in re-seed). Returns the updated {@link Class} (with its fresh `hidden`
   * flag), `undefined` on a cancelled token prompt, or throws on a non-auth failure (an unknown
   * id is a **404**).
   */
  setClassHidden(id: ClassId, hidden: boolean): Promise<Class | undefined> {
    return this.#privilegedWrite((token) =>
      this.#api.setClassHidden(this.baseUrl, id, hidden, token)
    );
  }

  /** How many built-in classes have been removed, and could be put back. A plain read. */
  async removedBuiltinClasses(): Promise<number> {
    return (await this.#api.getBuiltinClasses(this.baseUrl)).removed;
  }

  /**
   * Put back every built-in class that was removed. Returns the whole list of classes,
   * `undefined` on a cancelled token prompt, or throws on any other failure.
   */
  restoreBuiltinClasses(): Promise<Class[] | undefined> {
    return this.#privilegedWrite((token) => this.#api.restoreBuiltinClasses(this.baseUrl, token));
  }

  // ── Timer directory ────────────────────────────────────────────────────────
  //
  // Timers are application-level, like pilots and classes: an open read, gated writes on the
  // same token path. The Director holds the connection to each timer; this only keeps the list.

  /** Which video channel each seat of the open event is on, as the timers say. */
  listChannels(): Promise<SeatChannel[]> {
    return this.#api.listPublicChannels(this.baseUrl);
  }

  /**
   * List every timer with its status right now (`GET /timers`, open, no token). There is no
   * change feed for timers, so a screen that shows them calls this again every couple of seconds.
   */
  listTimers(): Promise<TimerView[]> {
    return this.#api.listTimers(this.baseUrl, { token: this.#token });
  }

  /**
   * List the OFT timers found on the network (`GET /timers/discovered`, open, no token), each
   * marked with whether it is already added. No change feed: a screen reads it again with the
   * timer list.
   */
  listDiscoveredTimers(): Promise<DiscoveredTimer[]> {
    return this.#api.listDiscoveredTimers(this.baseUrl, { token: this.#token });
  }

  /**
   * Add a timer (`POST /timers`). Returns the new {@link Timer}, `undefined` on a cancelled token
   * prompt, or throws with the Director's sentence (a blank name or a bad address is a **400**).
   */
  createTimer(request: CreateTimerRequest): Promise<Timer | undefined> {
    return this.#privilegedWrite((token) => this.#api.createTimer(this.baseUrl, request, token));
  }

  /**
   * Give a timer another name (`PUT /timers/{id}`). Returns the {@link Timer} as it now is,
   * `undefined` on a cancelled token prompt, or throws with the Director's sentence.
   */
  renameTimer(id: AdapterId, name: string): Promise<Timer | undefined> {
    return this.#privilegedWrite((token) => this.#api.renameTimer(this.baseUrl, id, name, token));
  }

  /**
   * Remove a timer (`DELETE /timers/{id}`). Resolves to `true` once it is gone, `undefined` on a
   * cancelled token prompt, or throws on any other failure. Mirrors {@link deletePilot}.
   */
  deleteTimer(id: AdapterId): Promise<true | undefined> {
    return this.#privilegedWrite(async (token) => {
      await this.#api.deleteTimer(this.baseUrl, id, token);
      return true as const;
    });
  }

  /**
   * Read a timer's settings (`GET /timers/{id}/config`, open). The Director asks the timer
   * each time. Throws with the Director's sentence when the timer cannot be reached.
   */
  getTimerConfig(id: AdapterId): Promise<TimerConfig> {
    return this.#api.getTimerConfig(this.baseUrl, id, { token: this.#token });
  }

  /**
   * Change a timer's settings (`PUT /timers/{id}/config`). Resolves to the settings as the
   * timer reports them afterwards, `undefined` on a cancelled token prompt, or throws with the
   * Director's sentence (the timer said no, could not be reached, or did not keep the change).
   */
  putTimerConfig(id: AdapterId, config: TimerConfig): Promise<TimerConfig | undefined> {
    return this.#privilegedWrite((token) =>
      this.#api.putTimerConfig(this.baseUrl, id, config, token)
    );
  }

  // ── Video ──────────────────────────────────────────────────────────────────

  /** The video settings, without the password (`GET /video/settings`, open). */
  getVideoSettings(): Promise<VideoSettings> {
    return this.#api.getVideoSettings(this.baseUrl, { token: this.#token });
  }

  /**
   * Change the video settings. Resolves to them as they now are, `undefined` on a cancelled
   * token prompt, or throws with the Director's sentence.
   */
  putVideoSettings(request: VideoSettingsRequest): Promise<VideoSettings | undefined> {
    return this.#privilegedWrite((token) =>
      this.#api.putVideoSettings(this.baseUrl, request, token)
    );
  }

  /**
   * Ask for the overlay to be drawn onto a race's clean recording, or onto every race of
   * the event that needs it (`POST /video/render`). Resolves to how many were asked for,
   * `undefined` if the director cancelled the token prompt, or throws with the Director's
   * sentence.
   */
  renderVideo(request: RenderRequest): Promise<number | undefined> {
    return this.#privilegedWrite((token) =>
      this.#api.postVideoRender(this.baseUrl, request, token)
    );
  }

  /** Have the Director download ffmpeg (`POST /video/ffmpeg`). The status follows it. */
  fetchFfmpeg(): Promise<true | undefined> {
    return this.#privilegedWrite(async (token) => {
      await this.#api.postVideoFfmpeg(this.baseUrl, token);
      return true as const;
    });
  }

  /** The link to OBS and the runs recorded so far (`GET /video/status`, open). */
  getVideoStatus(): Promise<VideoStatus> {
    return this.#api.getVideoStatus(this.baseUrl, { token: this.#token });
  }

  // ── A copy of everything ───────────────────────────────────────────────────

  /** Where Grid keeps its data, and the copy that waits to be brought in (`GET /data`). */
  getData(): Promise<DataInfo> {
    return this.#api.getData(this.baseUrl, { token: this.#token });
  }

  /** Make a copy of everything (`POST /data/export`). `undefined` on a cancelled prompt. */
  exportData(): Promise<DataExport | undefined> {
    return this.#privilegedWrite((token) => this.#api.postDataExport(this.baseUrl, token));
  }

  /** The address a copy that was just made downloads from. */
  exportUrl(name: string): string {
    return this.#api.dataExportUrl(this.baseUrl, name);
  }

  /**
   * Hand the Director a copy to bring in (`POST /data/import`). It takes over when Grid is
   * next started. `undefined` on a cancelled prompt; a file that is not a copy throws.
   */
  importData(file: Blob): Promise<DataCopy | undefined> {
    return this.#privilegedWrite((token) => this.#api.postDataImport(this.baseUrl, file, token));
  }

  /** Forget the copy that waits (`DELETE /data/import`). */
  cancelImport(): Promise<true | undefined> {
    return this.#privilegedWrite(async (token) => {
      await this.#api.deleteDataImport(this.baseUrl, token);
      return true as const;
    });
  }

  // ── The race overlay's look ────────────────────────────────────────────────

  /** Who the Director takes this browser for, and which event is open. Open to anyone. */
  hello(): Promise<PublicHello> {
    return this.#api.getPublicHello(this.baseUrl, { token: this.#token });
  }

  /** Every pilot in an event with their sign-in code. For the director. */
  getPilotAccess(event: EventMeta['id']): Promise<PilotAccessView> {
    return this.#api.getPilotAccess(this.baseUrl, event, { token: this.#token });
  }

  /** Give a pilot a new sign-in code. `undefined` on a cancelled prompt. */
  resetPilotCode(event: EventMeta['id'], pilot: PilotId): Promise<PilotCodeView | undefined> {
    return this.#privilegedWrite((token) =>
      this.#api.resetPilotCode(this.baseUrl, event, pilot, token)
    );
  }

  /** Sign a pilot's device out. `true` when done, `undefined` on a cancelled prompt. */
  endPilotSession(event: EventMeta['id'], pilot: PilotId): Promise<true | undefined> {
    return this.#privilegedWrite(async (token) => {
      await this.#api.endPilotSession(this.baseUrl, event, pilot, token);
      return true as const;
    });
  }

  /** Where a pilot's QR code picture is served from. */
  pilotQrUrl(event: EventMeta['id'], pilot: PilotId): string {
    return this.#api.pilotQrUrl(this.baseUrl, event, pilot);
  }

  /** Sign this browser in as a pilot, with their code. Rejects with the Director's words. */
  signInPilot(code: string): Promise<PublicPilot> {
    return this.#api.signInPilot(this.baseUrl, code);
  }

  /** Sign this browser out as a pilot. */
  signOutPilot(): Promise<void> {
    return this.#api.signOutPilot(this.baseUrl);
  }

  /** The link to a remote server: its settings and how the pushing stands. */
  getServerLink(): Promise<LinkView> {
    return this.#api.getServerLink(this.baseUrl, { token: this.#token });
  }

  /** Set the link to a remote server. `undefined` on a cancelled prompt. */
  putServerLink(settings: LinkSettings): Promise<LinkView | undefined> {
    return this.#privilegedWrite((token) => this.#api.putServerLink(this.baseUrl, settings, token));
  }

  /** Replace the server's copy of an event with this Director's. */
  replaceOnServer(eventId: string): Promise<LinkView | undefined> {
    return this.#privilegedWrite((token) =>
      this.#api.replaceOnServer(this.baseUrl, eventId, token)
    );
  }

  /**
   * The linked server's club: its members, and its pilots with who holds each. Asks the
   * server there and then. `undefined` on a cancelled prompt.
   */
  getServerPeople(): Promise<LinkPeople | undefined> {
    return this.#privilegedWrite((token) => this.#api.getServerPeople(this.baseUrl, token));
  }

  /**
   * Say which member holds a pilot on the linked server, or with no account that nobody
   * does. Nothing changes here unless the server agreed.
   */
  setPilotServerAccount(id: PilotId, account: string | undefined): Promise<Pilot | undefined> {
    return this.#privilegedWrite((token) =>
      this.#api.setPilotServerAccount(this.baseUrl, id, account, token)
    );
  }

  /** Bring pilots in from the linked server, already tied to their accounts. */
  pullFromServer(request: PullRequest): Promise<Pilot[] | undefined> {
    return this.#privilegedWrite((token) => this.#api.pullFromServer(this.baseUrl, request, token));
  }

  /** The two doors other devices come in by, and where their listeners stand. */
  getNetwork(): Promise<NetworkView> {
    return this.#api.getNetwork(this.baseUrl, { token: this.#token });
  }

  /** Open or shut a door, or change its port. `undefined` on a cancelled prompt. */
  putDoor(door: Door, request: DoorRequest): Promise<NetworkView | undefined> {
    return this.#privilegedWrite((token) => this.#api.putDoor(this.baseUrl, door, request, token));
  }

  /**
   * Change the access token of the Remote RD door: to `typed`, or to a new one Grid makes.
   * `undefined` on a cancelled prompt.
   */
  putAccessToken(typed?: string): Promise<NetworkView | undefined> {
    const request = typed === undefined ? {} : { token: typed };
    return this.#privilegedWrite((token) => this.#api.putAccessToken(this.baseUrl, request, token));
  }

  /**
   * Set the public address every public link and QR code uses, or clear it with an empty
   * one. `undefined` on a cancelled prompt.
   */
  putPublicAddress(address: string): Promise<NetworkView | undefined> {
    const typed = address.trim();
    const request = typed === '' ? {} : { address: typed };
    return this.#privilegedWrite((token) =>
      this.#api.putPublicAddress(this.baseUrl, request, token)
    );
  }

  /** The kiosk codes waiting to be used, and the kiosks set up. */
  getKiosks(): Promise<KioskView> {
    return this.#api.getKiosks(this.baseUrl, { token: this.#token });
  }

  /** Make one more kiosk code. `undefined` on a cancelled prompt. */
  newKioskCode(): Promise<KioskView | undefined> {
    return this.#privilegedWrite((token) => this.#api.newKioskCode(this.baseUrl, token));
  }

  /** Cancel a kiosk code that has not been used. */
  cancelKioskCode(code: string): Promise<KioskView | undefined> {
    return this.#privilegedWrite((token) => this.#api.cancelKioskCode(this.baseUrl, code, token));
  }

  /** Name a kiosk, so two tablets can be told apart. An empty name takes it away. */
  renameKiosk(number: number, name: string): Promise<KioskView | undefined> {
    return this.#privilegedWrite((token) =>
      this.#api.renameKiosk(this.baseUrl, number, name, token)
    );
  }

  /** Remove a kiosk: its tablet stops being one. */
  removeKiosk(number: number): Promise<KioskView | undefined> {
    return this.#privilegedWrite((token) => this.#api.removeKiosk(this.baseUrl, number, token));
  }

  /** Sign in as the director from another device. Rejects with the Director's words. */
  async signInDirector(token: string): Promise<void> {
    await this.#api.signInDirector(this.baseUrl, token);
    // Signed in, the cookie does the work. The typed token is not kept, and neither is
    // one this tab was still holding from an earlier run: it would go out with every
    // request the console makes next, and it may no longer be right.
    this.clearToken();
  }

  /** Sign this browser out as the director. */
  async signOutDirector(): Promise<void> {
    await this.#api.signOutDirector(this.baseUrl);
    // Signed out means signed out: the tab forgets the token too.
    this.clearToken();
  }

  /** Where the race overlay sits and which logos it has (`GET /overlay/look`, open). */
  getOverlayLook(): Promise<OverlayLook> {
    return this.#api.getOverlayLook(this.baseUrl, { token: this.#token });
  }

  /** Move or resize the race overlay. `undefined` on a cancelled prompt. */
  putOverlayLook(placement: OverlayPlacement): Promise<OverlayLook | undefined> {
    return this.#privilegedWrite((token) =>
      this.#api.putOverlayLook(this.baseUrl, placement, token)
    );
  }

  /** Where one of a race's videos plays from, for the Marshal page's player. */
  raceVideoUrl(event: EventMeta['id'], race: string, which: 'recording' | 'overlay'): string {
    return this.#api.raceVideoUrl(this.baseUrl, event, race, which);
  }

  /** Where one of the overlay's logos is served from. */
  overlayLogoUrl(side: 'left' | 'right'): string {
    return this.#api.overlayLogoUrl(this.baseUrl, side);
  }

  /** Keep a picture as one of the overlay's logos. `undefined` on a cancelled prompt. */
  putOverlayLogo(side: 'left' | 'right', file: Blob): Promise<OverlayLook | undefined> {
    return this.#privilegedWrite((token) =>
      this.#api.putOverlayLogo(this.baseUrl, side, file, token)
    );
  }

  /** Take one of the overlay's logos away. `undefined` on a cancelled prompt. */
  deleteOverlayLogo(side: 'left' | 'right'): Promise<OverlayLook | undefined> {
    return this.#privilegedWrite((token) => this.#api.deleteOverlayLogo(this.baseUrl, side, token));
  }

  // ── Saved formats and seat profiles ────────────────────────────────────────

  /** The saved formats (`GET /formats`, open). */
  listFormats(): Promise<SavedFormat[]> {
    return this.#api.listFormats(this.baseUrl, { token: this.#token });
  }

  /**
   * Save a new format. Resolves to it, `undefined` on a cancelled token prompt, or throws
   * with the Director's sentence (a name already taken).
   */
  createFormat(format: Format): Promise<SavedFormat | undefined> {
    return this.#privilegedWrite((token) => this.#api.createFormat(this.baseUrl, format, token));
  }

  /** Replace a saved format. Same answers as {@link createFormat}. */
  updateFormat(id: string, format: Format): Promise<SavedFormat | undefined> {
    return this.#privilegedWrite((token) =>
      this.#api.updateFormat(this.baseUrl, id, format, token)
    );
  }

  /** Remove a saved format. Resolves to `true` once it is gone. */
  deleteFormat(id: string): Promise<true | undefined> {
    return this.#privilegedWrite(async (token) => {
      await this.#api.deleteFormat(this.baseUrl, id, token);
      return true as const;
    });
  }

  /** A timer's saved profiles, and which one is active (`GET /timers/{id}/profiles`, open). */
  listProfiles(timer: AdapterId): Promise<TimerProfiles> {
    return this.#api.listProfiles(this.baseUrl, timer, { token: this.#token });
  }

  /** Save settings as a new profile of a timer. Nothing is written to the timer. */
  createProfile(timer: AdapterId, request: SeatProfileRequest): Promise<SeatProfile | undefined> {
    return this.#privilegedWrite((token) =>
      this.#api.createProfile(this.baseUrl, timer, request, token)
    );
  }

  /** Rename a profile or replace its settings. Nothing is written to the timer. */
  updateProfile(
    timer: AdapterId,
    profile: string,
    request: SeatProfileRequest
  ): Promise<SeatProfile | undefined> {
    return this.#privilegedWrite((token) =>
      this.#api.updateProfile(this.baseUrl, timer, profile, request, token)
    );
  }

  /** Remove a profile. The timer keeps the settings it has. */
  deleteProfile(timer: AdapterId, profile: string): Promise<true | undefined> {
    return this.#privilegedWrite(async (token) => {
      await this.#api.deleteProfile(this.baseUrl, timer, profile, token);
      return true as const;
    });
  }

  /**
   * Write a profile to its timer. Resolves to the settings as the timer reports them
   * afterwards, or throws with the Director's sentence.
   */
  applyProfile(timer: AdapterId, profile: string): Promise<TimerConfig | undefined> {
    return this.#privilegedWrite((token) =>
      this.#api.applyProfile(this.baseUrl, timer, profile, token)
    );
  }

  /**
   * Change the open event's name and details. Resolves to the event as it now is, and the
   * session's open event follows.
   */
  async updateEvent(
    id: EventMeta['id'],
    request: CreateEventRequest
  ): Promise<EventMeta | undefined> {
    const meta = await this.#privilegedWrite((token) =>
      this.#api.updateEvent(this.baseUrl, id, request, token)
    );
    if (meta && this.currentEvent?.id === meta.id) this.currentEvent = meta;
    return meta;
  }

  /**
   * Listen to a timer's live signal and levels, for tuning. Returns a function that stops
   * listening.
   */
  subscribeTimerLive(id: AdapterId, handlers: TimerLiveHandlers): () => void {
    return this.#api.subscribeTimerLive(this.baseUrl, id, handlers);
  }

  // ── An event's competition ─────────────────────────────────────────────────
  //
  // The snapshot and the change feed are open reads. Commands, presets and passes are
  // control-gated writes and take the same path as every other write here: sent without a
  // token first, and the token dialog opens only if the Director answers 401/403.
  // `lib/eventLive.svelte.ts` is the one caller; screens go through it.

  /** Read an event's competition snapshot (`GET /events/{id}/state`). */
  getEventState(id: EventMeta['id']): Promise<EventView> {
    return this.#api.getEventState(this.baseUrl, id, { token: this.#token });
  }

  /** Read a race's logged signal (`GET /events/{id}/signal?race=…`). An open read. */
  getRaceSignal(id: EventMeta['id'], race: RaceId): Promise<RaceSignal> {
    return this.#api.getRaceSignal(this.baseUrl, id, race, { token: this.#token });
  }

  /**
   * Listen to an event's change feed. `onCursor` hears the log length each time it moves.
   * Returns a function that stops listening.
   */
  subscribeChanges(id: EventMeta['id'], onCursor: (cursor: number) => void): () => void {
    return this.#api.subscribeChanges(this.baseUrl, id, onCursor);
  }

  /**
   * Send one command. Resolves `undefined` if the token prompt was cancelled. A refusal
   * (409) throws with the Director's sentence.
   */
  sendCommand(id: EventMeta['id'], command: Command): Promise<CommandOutcome | undefined> {
    return this.#privilegedWrite((token) =>
      this.#api.sendCommand(this.baseUrl, id, command, token)
    );
  }

  /** Generate a bracket's phases. Same answers as {@link sendCommand}. */
  createBracket(id: EventMeta['id'], request: BracketRequest): Promise<CommandOutcome | undefined> {
    return this.#privilegedWrite((token) =>
      this.#api.createBracket(this.baseUrl, id, request, token)
    );
  }

  /** Generate stacked mains. Same answers as {@link sendCommand}. */
  createMains(id: EventMeta['id'], request: MainsRequest): Promise<CommandOutcome | undefined> {
    return this.#privilegedWrite((token) =>
      this.#api.createMains(this.baseUrl, id, request, token)
    );
  }

  /** Generate a round robin. Same answers as {@link sendCommand}. */
  createRoundRobin(
    id: EventMeta['id'],
    request: RoundRobinRequest
  ): Promise<CommandOutcome | undefined> {
    return this.#privilegedWrite((token) =>
      this.#api.createRoundRobin(this.baseUrl, id, request, token)
    );
  }

  /** Log one gate crossing. Same answers as {@link sendCommand}. */
  postPass(id: EventMeta['id'], pass: PassRequest): Promise<CommandOutcome | undefined> {
    return this.#privilegedWrite((token) => this.#api.postPass(this.baseUrl, id, pass, token));
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

  // ── Events and the Director's active event (issues #72, #90) ───────────────

  /**
   * Read the id of the Director's **active event** (`GET /active-event`, open, no token) — issue
   * #91. The Events page uses this to mark which event is currently active, independent of
   * whether this console has it open: {@link leaveEvent} closes the event here *without* clearing
   * the server's active event. Resolves the id, or `undefined` when nothing is active or the read
   * fails (the page simply shows no pill — it never blocks the list).
   */
  async getActiveEventId(): Promise<EventMeta['id'] | undefined> {
    try {
      const { event } = await this.#api.getActiveEvent(this.baseUrl, { token: this.#token });
      return event?.id;
    } catch {
      return undefined;
    }
  }

  /**
   * Read the Director's **active event** in full (`GET /active-event`, open, no token):
   * the event, or `null` when none is active. Unlike {@link getActiveEventId} a failed read
   * **throws**, so a caller that follows the active event (the overlay) can tell "nothing is
   * active" from "the Director did not answer" and keep what it has for the second.
   */
  async readActiveEvent(): Promise<EventMeta | null> {
    const { event } = await this.#api.getActiveEvent(this.baseUrl, { token: this.#token });
    return event;
  }

  /**
   * Resolve the Director's **active event** on load (issue #90). The active event is server-side
   * Director state — there is one RD on one event — so a reload reads it (`GET /active-event`)
   * and opens the same event every client is on.
   *
   * - active set → {@link selectEvent} opens it.
   * - active `null`, or the read fails (Director unreachable) → no event is open.
   *
   * Always clears {@link resolvingActiveEvent} when it settles, so the shell can leave its
   * loading state whichever way it resolves.
   */
  async resolveActiveEvent(): Promise<void> {
    this.resolvingActiveEvent = true;
    try {
      const { event } = await this.#api.getActiveEvent(this.baseUrl, { token: this.#token });
      if (event) this.selectEvent(event);
    } catch {
      // The Director is unreachable / errored — leave the Events page to surface it (it re-lists
      // events and shows its own error). The active event simply stays unresolved.
    } finally {
      this.resolvingActiveEvent = false;
    }
  }

  /**
   * Choose an event from the Events page (issue #90): persist it as the Director's **active
   * event** (`PUT /active-event`, RD-gated, full-trust first) so every client — and this console
   * after a reload — resumes into it, then open it. Returns the event on success, or `undefined`
   * if a gated Director's token prompt was cancelled; a non-auth failure throws.
   */
  async chooseEvent(meta: EventMeta): Promise<EventMeta | undefined> {
    const set = await this.#privilegedWrite((token) =>
      this.#api.setActiveEvent(this.baseUrl, meta.id, token)
    );
    if (!set) return undefined;
    this.selectEvent(meta);
    return meta;
  }

  /**
   * **Permanently delete** an event and all of its data (`DELETE /events/{id}`). Called from the
   * Events page only after the RD confirms an unmissable, irreversible warning dialog. RD-gated,
   * full-trust first (like {@link chooseEvent}).
   *
   * If the deleted event is the one this console has open, it is closed ({@link leaveEvent}) so
   * the console doesn't dangle on a now-gone event. Resolves to `true` once the delete succeeds,
   * `undefined` if a gated Director's token prompt was cancelled, or throws on any other failure
   * (an unknown id is a **404**).
   */
  async deleteEvent(id: EventMeta['id']): Promise<true | undefined> {
    const deleted = await this.#privilegedWrite(async (token) => {
      await this.#api.deleteEvent(this.baseUrl, id, token);
      return true as const;
    });
    if (!deleted) return undefined;
    if (this.currentEvent?.id === id) this.leaveEvent();
    return true as const;
  }

  /**
   * Open an event in this console. This is the pure **open** step — it does NOT persist the
   * selection. The Events page persists via {@link chooseEvent} (`PUT /active-event`);
   * {@link resolveActiveEvent} calls this directly to *resume* into the already-persisted active
   * event without re-writing it.
   */
  selectEvent(meta: EventMeta): void {
    this.currentEvent = meta;
  }

  /**
   * Close the open event (a **client-side** view change), **without clearing the Director's
   * active event** (issue #90). Leaving the server's active event set means other clients are not
   * disrupted by one RD browsing the Events page; choosing a new event (`PUT /active-event` via
   * {@link chooseEvent}) is what actually re-points the Director.
   */
  leaveEvent(): void {
    this.currentEvent = undefined;
  }

  /**
   * Prompt for a token via the lazy provider and hold it. Returns `true` once a token was
   * entered, `false` if there is no provider or the RD cancelled.
   */
  async #promptForToken(): Promise<boolean> {
    if (!this.#tokenProvider) return false;
    const entered = await this.#tokenProvider();
    if (!entered?.trim()) return false;
    this.setToken(entered);
    return true;
  }

  /**
   * Create a persistent event by name (`POST /events`), make it the Director's active event,
   * and open it.
   *
   * Full-trust first (#72): the create is attempted **without** a token; only if the Director
   * rejects it for **auth** (it has a token configured) does the lazy prompt fire, and the create
   * is retried once with the entered token. Returns the new {@link EventMeta}, throws on a
   * non-auth transport/HTTP failure, or resolves `undefined` if the RD cancelled the prompt.
   */
  async createEventAndEnter(
    name: string,
    fields?: CreateEventFields
  ): Promise<EventMeta | undefined> {
    const meta = await this.#privilegedWrite((token) =>
      this.#api.createEvent(this.baseUrl, name, token, { fields })
    );
    if (!meta) return undefined;
    await this.#persistActive(meta.id);
    this.selectEvent(meta);
    return meta;
  }

  /**
   * Persist `id` as the Director's active event (issue #90), best-effort: the create has already
   * obtained any needed token, so this set should succeed. A failure here is swallowed — the RD
   * still opens the event locally; the worst case is a reload that lands on the Events page, not
   * a broken session.
   */
  async #persistActive(id: EventMeta['id']): Promise<void> {
    try {
      await this.#api.setActiveEvent(this.baseUrl, id, this.#token);
    } catch {
      /* leave the active event unset; opening locally still works */
    }
  }
}
