/**
 * Pure helpers for the Run view: what the big clock shows, what one seat row shows, and
 * the words for a race's state. Kept out of the component so they can be tested alone.
 */
import type {
  DiscardReason,
  LapView,
  PhaseView,
  PilotId,
  RaceSeatView,
  RaceView
} from '@gridfpv/types';
import type { Names } from './names.js';
import { isBench } from './seats.js';

/** What the big clock shows. Times are in milliseconds, as `RaceClock` takes them. */
export interface ClockReading {
  /** The words above the clock. */
  label: string;
  /** Set for a clock that counts up. */
  elapsedMs?: number;
  /** Set for a clock that counts down. */
  remainingMs?: number;
}

const toMs = (micros: number): number => micros / 1000;

/**
 * Read the clock for a race at `now` (Director clock, microseconds).
 *
 * - Staging: the countdown to the start tone.
 * - Running a timed race: the time left; once that is gone, the grace window left.
 * - Running any other race: the time since the start.
 * - Stopped: the length of the race, standing still.
 */
export function readClock(race: RaceView | undefined, now: number): ClockReading {
  if (!race) return { label: 'Race time', elapsedMs: 0 };
  if (race.state === 'Staging') {
    const left = race.tone_at === undefined ? 0 : race.tone_at - now;
    return { label: 'Starts in', remainingMs: Math.max(0, toMs(left)) };
  }
  const started = race.started_at;
  if (started === undefined) return { label: 'Race time', elapsedMs: 0 };
  if (race.state === 'Running') {
    const end = race.end;
    if (end !== undefined && typeof end === 'object' && 'Timed' in end) {
      const left = started + end.Timed.micros - now;
      if (left >= 0) return { label: 'Time left', remainingMs: toMs(left) };
      // Past the time: pilots finish the lap they are on inside the grace window.
      const grace = left + end.Timed.grace_micros;
      return { label: 'Grace', remainingMs: Math.max(0, toMs(grace)) };
    }
    return { label: 'Race time', elapsedMs: Math.max(0, toMs(now - started)) };
  }
  const stopped = race.stopped_at ?? started;
  return { label: 'Race time', elapsedMs: Math.max(0, toMs(stopped - started)) };
}

/** One seat's row on the Run view. */
export interface SeatRow {
  adapter: RaceSeatView['adapter'];
  seat: RaceSeatView['seat'];
  /** "Seat 1". */
  seatName: string;
  /** True for a bench seat: no timer sends its crossings, so they are logged by hand. */
  bench: boolean;
  /** A callsign, or where the pilot will come from. */
  who: string;
  pilot?: PilotId;
  /** Place in the race so far, 1 first. */
  place?: number;
  /** Counted laps, in order. Marked restarts are left out. */
  laps: LapView[];
  lastLapMicros?: number;
  bestLapMicros?: number;
  /** Whether the seat has crossed the gate to start lap 1. */
  onCourse: boolean;
  falseStart: boolean;
}

/** The rows for a race in progress: one per seat of its lineup, with the laps so far. */
export function raceSeatRows(race: RaceView, names: Names): SeatRow[] {
  return race.seats.map((seat, index) => {
    const laps = seat.laps.filter((lap) => !lap.restart && !lap.timer_restarted);
    return {
      adapter: seat.adapter,
      seat: seat.seat,
      seatName: names.seat(seat.adapter, seat.seat, index),
      bench: isBench(seat.adapter),
      who: seat.pilot === undefined ? names.who('Open') : names.pilot(seat.pilot),
      pilot: seat.pilot,
      place: seat.finish,
      laps,
      lastLapMicros: laps.at(-1)?.micros,
      bestLapMicros: seat.best_lap_micros,
      onCourse: seat.hole_shot !== undefined || laps.length > 0,
      falseStart: seat.false_start
    };
  });
}

/** The rows for a heat with no race in progress: its lineup, with nothing flown yet. */
export function lineupSeatRows(phase: PhaseView, names: Names): SeatRow[] {
  return phase.lineup.map((seat, index) => ({
    adapter: seat.adapter,
    seat: seat.seat,
    seatName: names.seat(seat.adapter, seat.seat, index),
    bench: isBench(seat.adapter),
    who: names.who(seat.who),
    pilot: typeof seat.who === 'object' && 'Pilot' in seat.who ? seat.who.Pilot : undefined,
    laps: [],
    onCourse: false,
    falseStart: false
  }));
}

/** The discard reasons, in the order the buttons are shown. */
export const DISCARD_REASONS: readonly { reason: DiscardReason; label: string }[] = [
  { reason: 'FalseStart', label: 'False start' },
  { reason: 'CrashOrObstruction', label: 'Crash or obstruction' },
  { reason: 'TimerProblem', label: 'Timer problem' },
  { reason: 'Other', label: 'Other' }
];

/** The words for a discard reason. */
export function discardReasonLabel(reason: DiscardReason): string {
  return DISCARD_REASONS.find((entry) => entry.reason === reason)?.label ?? 'Other';
}

/** The words for a race's state, with the reason when it was discarded for one. */
export function raceStateLabel(race: RaceView): string {
  switch (race.state) {
    case 'Staging':
      return 'Counting down';
    case 'Running':
      return 'Racing';
    case 'Stopped':
      return 'Stopped';
    case 'Saved':
      return 'Saved';
    case 'Discarded':
      return race.discard_reason === undefined
        ? 'Discarded'
        : `Discarded: ${discardReasonLabel(race.discard_reason).toLowerCase()}`;
  }
}

/** A race's seats in finishing order, for the one-line summary of an earlier race. */
export function finishOrder(race: RaceView): RaceSeatView[] {
  return race.seats
    .filter((seat) => seat.finish !== undefined)
    .sort((a, b) => (a.finish ?? 0) - (b.finish ?? 0));
}
