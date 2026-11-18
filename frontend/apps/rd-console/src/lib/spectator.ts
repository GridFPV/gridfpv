/**
 * What a spectator sees (docs/primitives.md §11.2): the race that is on, the heats coming
 * up with who is in them, and results. Pure helpers over the same snapshot the console
 * reads, so the spectator's page and the console can never tell two stories.
 *
 * The data comes from the Director's `/public` routes, which hand out the console's own
 * shapes with the director's things taken out. {@link publicSource} is the thin object
 * that makes the shared live store ({@link EventLive}) read from there.
 */
import {
  getPublicEvent,
  getPublicEventState,
  kioskDone,
  kioskIn,
  kioskSetUp,
  dropSeat,
  leaveSpot,
  listPublicChannels,
  listPublicClasses,
  listPublicPilots,
  listPublicTimers,
  subscribePublicChanges,
  takeNextSpot,
  takeSeat,
  takeSpot
} from '@gridfpv/protocol-client';
import type {
  Class,
  EventView,
  PhaseId,
  PhaseView,
  Pilot,
  PublicEvent,
  KioskPassView,
  RaceView,
  SeatChannel,
  SignupSeat,
  TimerView,
  RaceId,
  SeatRef,
  AdapterId
} from '@gridfpv/types';
import type { LiveSession } from './eventLive.svelte.js';
import type { Names } from './names.js';
import { lastResult } from './overlay.js';
import { currentRace, isHeat } from './phases.js';
import { lineupSeatRows, type SeatRow } from './run.js';
import { BENCH_ADAPTER } from './seats.js';

/** A seat by its wire handles: the timer it is on, and which of that timer's seats. */
export interface SeatHandle {
  adapter: AdapterId;
  seat: SeatRef;
}

/** What the spectator's page reads from: which event is open, and the live store's reads. */
export interface PublicSource extends LiveSession {
  getEvent(): Promise<PublicEvent>;
  /** Which video channel each seat is on. */
  listChannels(): Promise<SeatChannel[]>;
  /**
   * What a signed-in pilot does for themselves (§11.5). Who the pilot is comes from their
   * sign-in. Each rejects with the Director's reason when the rules do not allow it.
   *
   * `kiosk` is the pass of a pilot who proved who they are at a shared tablet (§11.4).
   * With it the request is theirs, whoever the tablet's browser is signed in as.
   */
  takeSpot(heat: PhaseId, seat: SignupSeat, kiosk?: string): Promise<unknown>;
  takeNextSpot(phase: PhaseId, seat: SignupSeat, kiosk?: string): Promise<unknown>;
  leaveSpot(heat: PhaseId, kiosk?: string): Promise<unknown>;
  /**
   * Check-in (§11.9): the signed-in pilot takes an open seat in a race that is being
   * flown, or gives up the one they took.
   */
  takeSeat(race: RaceId, seat: SeatHandle, kiosk?: string): Promise<unknown>;
  dropSeat(race: RaceId, kiosk?: string): Promise<unknown>;
  /** Prove who a pilot is at a kiosk, with their code. Rejects when the code is wrong. */
  kioskIn(code: string): Promise<KioskPassView>;
  /** The pilot is done at the kiosk. */
  kioskDone(pass: string): Promise<void>;
  /** Make this device a kiosk, with a kiosk code the director made. It is used up. */
  kioskSetUp(code: string): Promise<void>;
}

const NOT_FOR_VISITORS = 'Only the director can do this.';
const refuse = (): Promise<never> => Promise.reject(new Error(NOT_FOR_VISITORS));

/**
 * The reads of the open event, for a visitor. The event's id is never sent: a visitor is
 * only ever shown the one event that is open. Every write is refused here, before it
 * could be sent (the Director would refuse it too).
 */
export function publicSource(baseUrl: string): PublicSource {
  return {
    getEvent: () => getPublicEvent(baseUrl),
    listChannels: () => listPublicChannels(baseUrl),
    takeSpot: (heat, seat, kiosk) => takeSpot(baseUrl, heat, seat, { kiosk }),
    takeNextSpot: (phase, seat, kiosk) => takeNextSpot(baseUrl, phase, seat, { kiosk }),
    leaveSpot: (heat, kiosk) => leaveSpot(baseUrl, heat, { kiosk }),
    takeSeat: (race, seat, kiosk) => takeSeat(baseUrl, race, seat, { kiosk }),
    dropSeat: (race, kiosk) => dropSeat(baseUrl, race, { kiosk }),
    kioskIn: (code) => kioskIn(baseUrl, code),
    kioskDone: (pass) => kioskDone(baseUrl, pass),
    kioskSetUp: (code) => kioskSetUp(baseUrl, code),
    getEventState: () => getPublicEventState(baseUrl),
    subscribeChanges: (_id, onCursor) => subscribePublicChanges(baseUrl, onCursor),
    listPilots: () => listPublicPilots(baseUrl),
    listClasses: () => listPublicClasses(baseUrl),
    listTimers: () => listPublicTimers(baseUrl),
    // A visitor has no use for the saved formats: each phase carries the one in force.
    listFormats: async () => [],
    sendCommand: refuse,
    createBracket: refuse,
    createMains: refuse,
    postPass: refuse,
    getRaceSignal: refuse,
    getVideoSettings: refuse,
    getVideoStatus: refuse
  };
}

/**
 * What the Current race tab shows. One thing at a time, always with a clock:
 *
 * - `race`: a race that is counting down, running, or stopped and not yet saved.
 * - `result`: how the heat on the timer was last flown.
 * - `next`: the heat on the timer, not flown yet, with who is in it.
 */
export type CurrentRace =
  | { kind: 'race'; race: RaceView }
  | { kind: 'result'; race: RaceView }
  | { kind: 'next'; heat: UpcomingHeat };

/**
 * What to show as the current race: **the heat the director has on the timer**
 * (`on_timer`). While its race is on, that race. Once it has been flown and saved, its
 * result, which stays until the director puts another heat on the timer. Before it has
 * been flown, who is in it.
 *
 * An event where the director has not put anything on the timer yet shows the heat they
 * would run first, and with nothing lined up, the last result.
 */
export function currentRaceView(
  view: EventView | undefined,
  names: Names
): CurrentRace | undefined {
  if (!view) return undefined;
  const on = currentRace(view);
  if (on) return { kind: 'race', race: on };
  const heat = view.phases.find((phase) => phase.def.id === view.on_timer && isHeat(phase.def));
  if (heat) {
    const saved = view.races
      .filter((race) => race.heat === heat.def.id && race.state === 'Saved')
      .at(-1);
    if (saved) return { kind: 'result', race: saved };
    return {
      kind: 'next',
      heat: { id: heat.def.id, name: names.phase(heat.def.id), seats: lineupSeatRows(heat, names) }
    };
  }
  const next = upcomingHeats(view, names)[0];
  if (next) return { kind: 'next', heat: next };
  const result = lastResult(view);
  return result ? { kind: 'result', race: result } : undefined;
}

/** A heat that is coming up. */
export interface UpcomingHeat {
  id: PhaseId;
  /** "Qualifying Heat 3". */
  name: string;
  /** The phases that hold it, when its name does not say: "Sport Finals › Semi-finals". */
  within?: string;
  /** Who is in it, in seat order. */
  seats: SeatRow[];
}

/**
 * The heats coming up, in the order the director has them: the run queue, without the
 * heat that is on right now. A heat still waiting on another one to fill its seats is in
 * the list too, after the ones that are ready, so pilots can see where they will come in.
 * The heat on the timer comes first when it is still to be flown: it is the next to fly,
 * whatever the order says.
 */
export function upcomingHeats(view: EventView | undefined, names: Names): UpcomingHeat[] {
  if (!view) return [];
  const byId = new Map(view.phases.map((phase) => [phase.def.id, phase]));
  const onNow = currentRace(view)?.heat;
  const ready = view.queue
    .map((id) => byId.get(id))
    .filter((phase): phase is PhaseView => phase !== undefined);
  const queued = new Set(view.queue);
  const waiting = view.phases.filter(
    (phase) =>
      isHeat(phase.def) &&
      !queued.has(phase.def.id) &&
      phase.waiting &&
      !phase.complete &&
      !phase.locked
  );
  const coming = [...ready, ...waiting].filter((phase) => phase.def.id !== onNow);
  // The heat on the timer flies before anything else, wherever it is in the run order.
  const first = coming.findIndex((phase) => phase.def.id === view.on_timer);
  if (first > 0) coming.unshift(...coming.splice(first, 1));
  return coming.map((phase) => ({
    id: phase.def.id,
    name: names.phase(phase.def.id),
    within: names.within(phase.def.id),
    seats: lineupSeatRows(phase, names)
  }));
}

/**
 * Whether the snapshot names a pilot, class or timer the page has no name for yet. The
 * names a visitor is given are only those the event uses, so when the director adds a
 * pilot to the event the page has to ask again.
 */
export function missingNames(
  view: EventView | undefined,
  pilots: readonly Pilot[],
  classes: readonly Class[],
  timers: readonly TimerView[]
): { pilots: boolean; classes: boolean; timers: boolean } {
  if (!view) return { pilots: false, classes: false, timers: false };
  const knownPilots = new Set(pilots.map((pilot) => pilot.id));
  const knownClasses = new Set(classes.map((klass) => klass.id));
  const knownTimers = new Set(timers.map((timer) => timer.timer.id));
  let pilotMissing = false;
  let timerMissing = false;
  const seePilot = (id: string | undefined) => {
    if (id !== undefined && !knownPilots.has(id)) pilotMissing = true;
  };
  const seeTimer = (id: string) => {
    // The bench is not a timer: it has no entry in the list and needs none.
    if (id !== BENCH_ADAPTER && !knownTimers.has(id)) timerMissing = true;
  };
  for (const roster of view.rosters) roster.pilots.forEach(seePilot);
  for (const phase of view.phases) {
    phase.field.forEach(seePilot);
    phase.ranking.forEach((placed) => seePilot(placed.pilot));
    for (const seat of phase.lineup) {
      seeTimer(seat.adapter);
      if (typeof seat.who === 'object' && 'Pilot' in seat.who) seePilot(seat.who.Pilot);
    }
  }
  for (const race of view.races) {
    for (const seat of race.seats) {
      seeTimer(seat.adapter);
      seePilot(seat.pilot);
    }
  }
  return {
    pilots: pilotMissing,
    classes: view.rosters.some((roster) => !knownClasses.has(roster.class)),
    timers: timerMissing
  };
}

/** The word under a race's name: what it is doing. Sentence case, for a page, not a video. */
export function raceWord(race: RaceView): string {
  switch (race.state) {
    case 'Staging':
      return 'Starting';
    case 'Running':
      return 'Racing';
    case 'Stopped':
      return 'Finished';
    case 'Saved':
      return 'Result';
    case 'Discarded':
      return 'Thrown out';
  }
}
