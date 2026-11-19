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
  /** Whether the pilot is marked out of the race (crashed, not going to finish). */
  out: boolean;
  /**
   * Whether this is an open seat: no pilot is named on it, so a pilot can be checked in
   * to it while the race is flown. `pilot` and `who` are then whoever has it now.
   */
  open: boolean;
}

/** The rows for a race in progress: one per seat of its lineup, with the laps so far. */
export function raceSeatRows(race: RaceView, names: Names): SeatRow[] {
  return race.seats.map((seat, index) => {
    // An open seat a pilot has taken shows that pilot and the laps of their stint. With
    // nobody on it, it shows every lap flown on the seat, as an open seat always has.
    const stint = seat.holder === undefined ? undefined : seat.stints.at(-1);
    const counted = (stint?.laps ?? seat.laps).filter(
      (lap) => !lap.restart && !lap.timer_restarted
    );
    if (seat.pilot === undefined && stint !== undefined) {
      return {
        adapter: seat.adapter,
        seat: seat.seat,
        seatName: names.seat(seat.adapter, seat.seat, index),
        bench: isBench(seat.adapter),
        who: names.pilot(stint.pilot),
        pilot: stint.pilot,
        laps: counted,
        lastLapMicros: counted.at(-1)?.micros,
        bestLapMicros: stint.best_lap_micros,
        onCourse: counted.length > 0,
        falseStart: false,
        out: false,
        open: true
      };
    }
    const laps = counted;
    return {
      adapter: seat.adapter,
      seat: seat.seat,
      seatName: names.seat(seat.adapter, seat.seat, index),
      bench: isBench(seat.adapter),
      who:
        seat.pilot === undefined
          ? names.seatWho('Open', seat.adapter, seat.seat, index)
          : names.pilot(seat.pilot),
      pilot: seat.pilot,
      place: seat.finish,
      laps,
      lastLapMicros: laps.at(-1)?.micros,
      bestLapMicros: seat.best_lap_micros,
      onCourse: seat.hole_shot !== undefined || laps.length > 0,
      falseStart: seat.false_start,
      out: seat.out,
      open: seat.pilot === undefined
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
    who: names.seatWho(seat.who, seat.adapter, seat.seat, index),
    pilot: typeof seat.who === 'object' && 'Pilot' in seat.who ? seat.who.Pilot : undefined,
    laps: [],
    onCourse: false,
    falseStart: false,
    out: false,
    open: seat.who === 'Open'
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

// ── A seat card's laps, and every lap of a race ─────────────────────────────

/** How many of a seat's latest laps its card lists. The rest are in the All laps table. */
export const CARD_LAPS = 9;

/** One lap as a seat card lists it. */
export interface CardLap {
  lap: LapView;
  /** The lap's number among the seat's counted laps, 1 first. */
  number: number;
  best: boolean;
  /** Whether it is one of the best run of N laps in a row. */
  inRun: boolean;
  /**
   * Whether it is older than the card's window and kept on the card all the same: the best
   * lap, or one of the best run. It is listed before the window, apart from it.
   */
  kept: boolean;
}

/**
 * The laps a seat card lists: the latest {@link CARD_LAPS}, newest last, so the card stays
 * one size however long the race runs. The best lap is always among them, and so is the
 * best run of N laps where the heat is ranked on one: any of those older than the window
 * is kept and listed first. `hidden` is how many laps are not on the card.
 *
 * With {@link CARD_LAPS} laps or fewer nothing is hidden and nothing is kept.
 */
export function cardLaps(
  laps: readonly LapView[],
  bestLapMicros: number | undefined,
  run?: { start: number; length: number },
  window: number = CARD_LAPS
): { laps: CardLap[]; hidden: number } {
  const from = Math.max(0, laps.length - window);
  const inRun = (index: number) =>
    run !== undefined && index >= run.start && index < run.start + run.length;
  // Only the first lap that ties for best is the best: one lap is marked, not several.
  const bestAt =
    bestLapMicros === undefined ? -1 : laps.findIndex((lap) => lap.micros === bestLapMicros);
  const listed: CardLap[] = [];
  laps.forEach((lap, index) => {
    const best = index === bestAt;
    const run_ = inRun(index);
    if (index >= from || best || run_) {
      listed.push({ lap, number: index + 1, best, inRun: run_, kept: index < from });
    }
  });
  return { laps: listed, hidden: laps.length - listed.length };
}

/** One row of the All laps table. */
export interface LapRow {
  /** Unique in the table: the seat and the crossing that ended the lap. */
  key: string;
  seatKey: string;
  seatName: string;
  /** The pilot whose lap it is. Left out for a lap on an open seat nobody held. */
  pilot?: PilotId;
  /** A callsign, or "Nobody" for a lap on an open seat nobody held. */
  who: string;
  /** The lap's number for that pilot on that seat, 1 first. */
  number: number;
  micros: number;
  /** Whether it is the fastest lap of its pilot on its seat. */
  best: boolean;
}

/**
 * Every counted lap of a race, seat by seat in flying order. On a named pilot's seat the
 * laps are theirs. On an open seat each lap belongs to whoever held the seat when it was
 * flown (their stint, docs/primitives.md §11.9); a lap flown with nobody on the seat is
 * listed as nobody's, since it counts for nobody.
 */
export function allLapRows(race: RaceView, names: Names): LapRow[] {
  const counts = (lap: LapView) => !lap.restart && !lap.timer_restarted;
  const rows: LapRow[] = [];
  race.seats.forEach((seat, index) => {
    const seatKey = `${seat.adapter}/${seat.seat}`;
    const seatName = names.seat(seat.adapter, seat.seat, index);
    const push = (laps: LapView[], pilot: PilotId | undefined) => {
      const flown = laps.filter(counts);
      const fastest = Math.min(...flown.map((lap) => lap.micros));
      flown.forEach((lap, at) =>
        rows.push({
          key: `${seatKey}/${lap.crossing}`,
          seatKey,
          seatName,
          pilot,
          who: pilot === undefined ? 'Nobody' : names.pilot(pilot),
          number: at + 1,
          micros: lap.micros,
          best:
            pilot !== undefined &&
            lap.micros === fastest &&
            at === flown.findIndex((l) => l.micros === fastest)
        })
      );
    };
    if (seat.pilot !== undefined) {
      push(seat.laps, seat.pilot);
      return;
    }
    // An open seat: each stint's laps are its pilot's, and what is left is nobody's.
    const held = new Set(seat.stints.flatMap((stint) => stint.laps.map((lap) => lap.crossing)));
    const byPilot = new Map<PilotId, LapView[]>();
    for (const stint of seat.stints) {
      byPilot.set(stint.pilot, [...(byPilot.get(stint.pilot) ?? []), ...stint.laps]);
    }
    for (const [pilot, laps] of byPilot) push(laps, pilot);
    push(
      seat.laps.filter((lap) => !held.has(lap.crossing)),
      undefined
    );
  });
  return rows;
}

/** How the All laps table is ordered. */
export type LapOrder = 'flown' | 'fastest';

/** Narrow the All laps rows to one seat and one pilot (either may be left out), and order them. */
export function filterLapRows(
  rows: readonly LapRow[],
  filter: { seat?: string; pilot?: PilotId; order?: LapOrder }
): LapRow[] {
  const kept = rows.filter(
    (row) =>
      (filter.seat === undefined || row.seatKey === filter.seat) &&
      (filter.pilot === undefined || row.pilot === filter.pilot)
  );
  // A stable sort: laps of the same time stay in flying order.
  return filter.order === 'fastest' ? [...kept].sort((a, b) => a.micros - b.micros) : kept;
}
