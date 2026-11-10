/**
 * The live competition of the open event.
 *
 * It holds the latest snapshot (`EventView`) and keeps it fresh. The Director's change
 * feed sends a cursor whenever the event's log grows; on each one the snapshot is read
 * again. That is the whole live mechanism, so there is no second copy of the truth here
 * that could drift.
 *
 * It also holds the pilot directory and the timer directory, and builds the one name resolver
 * ({@link Names}) every event screen uses, and it keeps the offset between the Director's clock and this
 * browser's, so a countdown runs on the Director's clock.
 *
 * Timers have no change feed, so while the store is running it asks for the timer list again
 * every {@link TIMER_POLL_MS}. It stops asking when the store stops.
 *
 * Writes go through the {@link Session}, so they get its token handling: sent without a
 * token first, and the token dialog opens only if the Director asks for one.
 */
import { isRequestFailure } from '@gridfpv/protocol-client';
import type {
  BracketRequest,
  Class,
  Command,
  EventId,
  EventView,
  PassRequest,
  Pilot,
  ProtocolError,
  RaceId,
  RaceSignal,
  TimerView
} from '@gridfpv/types';
import { buildNames, type Names } from './names.js';
import { seatSources, type SeatSource } from './seats.js';
import type { Session } from './session.svelte.js';
import { TIMER_POLL_MS } from './timers.js';

/** The parts of the session the store needs. Narrow, so a test can hand in a stand-in. */
export type LiveSession = Pick<
  Session,
  | 'getEventState'
  | 'subscribeChanges'
  | 'sendCommand'
  | 'createBracket'
  | 'postPass'
  | 'getRaceSignal'
  | 'listPilots'
  | 'listTimers'
  | 'listClasses'
>;

/**
 * Turn whatever a failed call threw into the error shape `ErrorBanner` shows. The message
 * is the Director's own sentence and is passed on untouched.
 */
export function toProtocolError(e: unknown): ProtocolError {
  return {
    code: (isRequestFailure(e) ? e.code : undefined) ?? 'Internal',
    message: e instanceof Error ? e.message : String(e)
  };
}

export class EventLive {
  /** The latest snapshot, or `undefined` before the first one arrives. */
  view = $state.raw<EventView | undefined>(undefined);
  /** Why the last read failed, in the Director's words. Cleared by the next good read. */
  loadError = $state<string | undefined>(undefined);
  /** The pilot directory, for callsigns and the pilot pickers. */
  pilots = $state.raw<Pilot[]>([]);
  /** Director clock minus browser clock, in microseconds. */
  clockOffsetMicros = $state(0);
  /** The timer directory with each timer's status, for seat and timer names and the pickers. */
  timers = $state.raw<TimerView[]>([]);
  /** Whether the timer directory has been read at least once. */
  timersLoaded = $state(false);
  /** The class directory, for class names and the class picker. */
  classes = $state.raw<Class[]>([]);
  /** What a heat can be timed by: every timer, then the bench. */
  seatSources: SeatSource[] = $derived(seatSources(this.timers));
  /** The one resolver from ids to names for this event. */
  names: Names = $derived(
    buildNames(this.view?.phases ?? [], this.pilots, this.timers, this.classes)
  );

  readonly eventId: EventId;
  #session: LiveSession;
  #unsubscribe: (() => void) | undefined;
  #timerPoll: ReturnType<typeof setInterval> | undefined;
  #stopped = false;
  #loading = false;
  #loadAgain = false;
  /** The quickest round trip seen so far. A quicker read gives a truer clock offset. */
  #bestRoundTripMs = Number.POSITIVE_INFINITY;
  #clock: () => number;

  constructor(session: LiveSession, eventId: EventId, opts?: { now?: () => number }) {
    this.#session = session;
    this.eventId = eventId;
    this.#clock = opts?.now ?? (() => Date.now());
  }

  /** Read the snapshot, the pilots and the timers, and start listening for changes. */
  start(): void {
    this.#stopped = false;
    void this.reload();
    void this.reloadPilots();
    void this.reloadTimers();
    void this.reloadClasses();
    clearInterval(this.#timerPoll);
    this.#timerPoll = setInterval(() => void this.reloadTimers(), TIMER_POLL_MS);
    try {
      // The feed sends the current cursor on every connect and reconnect, so a console
      // that was cut off reads a fresh snapshot as soon as it is back.
      this.#unsubscribe = this.#session.subscribeChanges(this.eventId, (cursor) => {
        if (cursor !== this.view?.cursor) void this.reload();
      });
    } catch (e) {
      this.loadError = e instanceof Error ? e.message : String(e);
    }
  }

  /** Stop listening. A read still in flight is dropped when it lands. */
  stop(): void {
    this.#stopped = true;
    this.#unsubscribe?.();
    this.#unsubscribe = undefined;
    clearInterval(this.#timerPoll);
    this.#timerPoll = undefined;
  }

  /**
   * Read the snapshot again. Two reads never run at once: a call that arrives while one is
   * running makes it run once more when it finishes, so the last change is never missed
   * and a burst of cursors costs at most two reads.
   */
  async reload(): Promise<void> {
    if (this.#loading) {
      this.#loadAgain = true;
      return;
    }
    this.#loading = true;
    try {
      do {
        this.#loadAgain = false;
        await this.#loadOnce();
      } while (this.#loadAgain && !this.#stopped);
    } finally {
      this.#loading = false;
    }
  }

  async #loadOnce(): Promise<void> {
    const sent = this.#clock();
    try {
      const view = await this.#session.getEventState(this.eventId);
      if (this.#stopped) return;
      const received = this.#clock();
      this.#noteClock(view.now, sent, received);
      // Reads can land out of order with a write's own reload; never step backwards.
      if (!this.view || view.cursor >= this.view.cursor) this.view = view;
      this.loadError = undefined;
    } catch (e) {
      if (this.#stopped) return;
      this.loadError = e instanceof Error ? e.message : String(e);
    }
  }

  /**
   * Work out the clock offset from one read. The Director stamped `directorNow` somewhere
   * between our send and our receive, so the middle of the two is the best guess. Only a
   * read at least as quick as the quickest so far moves the offset: a slow read is a worse
   * measurement, and letting it in would make the race clock jump.
   */
  #noteClock(directorNow: number, sentMs: number, receivedMs: number): void {
    const roundTrip = receivedMs - sentMs;
    if (roundTrip > this.#bestRoundTripMs) return;
    this.#bestRoundTripMs = roundTrip;
    this.clockOffsetMicros = directorNow - ((sentMs + receivedMs) / 2) * 1000;
  }

  /** The time on the Director's clock right now, in epoch microseconds. */
  nowMicros(): number {
    return this.#clock() * 1000 + this.clockOffsetMicros;
  }

  /** Read the pilot directory again (a pilot may have been added since the page opened). */
  async reloadPilots(): Promise<void> {
    try {
      const pilots = await this.#session.listPilots();
      if (!this.#stopped) this.pilots = pilots;
    } catch {
      // Callsigns fall back to "Unknown pilot"; the snapshot error, if any, says why.
    }
  }

  /** Read the class directory again (a class may have been added since the page opened). */
  async reloadClasses(): Promise<void> {
    try {
      const classes = await this.#session.listClasses();
      if (!this.#stopped) this.classes = classes;
    } catch {
      // Class names fall back to "Unknown class".
    }
  }

  /**
   * Read the timer directory again. A read that fails keeps the last list: a timer's name
   * should not turn into "Unknown timer" because one request was dropped.
   */
  async reloadTimers(): Promise<void> {
    try {
      const timers = await this.#session.listTimers();
      if (this.#stopped) return;
      // The list is read every couple of seconds; only a real change is passed on.
      if (JSON.stringify(timers) !== JSON.stringify(this.timers)) this.timers = timers;
      this.timersLoaded = true;
    } catch {
      // Keep what we have.
    }
  }

  /**
   * Send one command. Resolves `true` when the Director took it and `false` when the token
   * prompt was cancelled. A refusal throws; show it with {@link toProtocolError}.
   */
  async send(command: Command): Promise<boolean> {
    return this.#wrote(await this.#session.sendCommand(this.eventId, command));
  }

  /** Generate a bracket. Same answers as {@link send}. */
  async createBracket(request: BracketRequest): Promise<boolean> {
    return this.#wrote(await this.#session.createBracket(this.eventId, request));
  }

  /** Log one gate crossing. Same answers as {@link send}. */
  async postPass(pass: PassRequest): Promise<boolean> {
    return this.#wrote(await this.#session.postPass(this.eventId, pass));
  }

  /**
   * Read a race's logged signal: one trace per seat, on the timer's clock. Read when a marshal
   * opens the race, never kept here, because it can be large. A failure throws.
   */
  raceSignal(race: RaceId): Promise<RaceSignal> {
    return this.#session.getRaceSignal(this.eventId, race);
  }

  /**
   * After a write, read the snapshot straight away rather than waiting for the feed, so
   * the screen answers the click at once. Resolves once that read has landed.
   */
  async #wrote(outcome: { cursor: number } | undefined): Promise<boolean> {
    if (!outcome) return false;
    await this.reload();
    return true;
  }
}
