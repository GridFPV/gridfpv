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
  isRequestFailure
} from '@gridfpv/protocol-client';
import type {
  Class,
  ClassId,
  CreateClassRequest,
  CreateEventRequest,
  CreatePilotRequest,
  EventMeta,
  Pilot,
  PilotId,
  UpdateClassRequest,
  UpdatePilotRequest
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
  setClassHidden
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
