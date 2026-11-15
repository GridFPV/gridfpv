/**
 * Replay: a finished race as it stood at any moment of its recording.
 *
 * The replay overlay (`screens/ReplayOverlay.svelte`) draws the race overlay for a race that
 * is over, so the Director can lay it onto the race's clean recording. For each frame of the
 * video it needs the race as the live overlay would have had it at that moment: counting
 * down before the start, then running with only the laps flown so far, then finished.
 *
 * Everything here is pure. A moment is always a time on the Director's clock, in epoch
 * microseconds, the same clock `tone_at`, `started_at` and `stopped_at` are on. A lap's own
 * times are on its timer's clock; a seat's `start_at` is the race's start on that clock,
 * which is what places a lap in the race.
 */
import type { EventView, RaceId, RaceSeatView, RaceView } from '@gridfpv/types';
import { bestLapRun, consecutiveLaps } from './overlay.js';

/** How long before the start a replay begins when the race has no recording to follow. */
export const LEAD_MICROS = 5_000_000;

/** When the race started, on the Director's clock. `undefined` for a race that never did. */
export function raceStart(race: RaceView): number | undefined {
  return race.started_at ?? race.tone_at;
}

/**
 * The Director's clock at the first frame of the race's recording. A race with no recording
 * gets a short lead before its start, so the page can still be looked at.
 */
export function firstFrameAt(race: RaceView): number | undefined {
  const start = raceStart(race);
  if (start === undefined) return undefined;
  return race.video?.first_frame_at ?? start - LEAD_MICROS;
}

/**
 * The race's start on a seat's own clock: what a crossing's time is measured from.
 *
 * A timer's seat carries it (`start_at`). A seat without one is timed by hand, on the
 * Director's own clock, so the race's start there is the start itself. That is only trusted
 * when the seat's crossings then land inside the race; otherwise `undefined`, and the laps
 * are laid end to end from the start instead.
 */
function seatBase(seat: RaceSeatView, start: number, length: number): number | undefined {
  if (seat.start_at !== undefined) return seat.start_at;
  const first = seat.hole_shot?.at ?? seat.laps[0]?.started_at;
  const last = seat.laps.at(-1)?.ended_at ?? first;
  if (first === undefined || last === undefined) return start;
  return first >= start && last <= start + length ? start : undefined;
}

/** A seat cut back to `sinceStart` microseconds into the race, and when its last lap ended. */
function seatAt(
  seat: RaceSeatView,
  sinceStart: number,
  base: number | undefined
): { seat: RaceSeatView; lastAt?: number } {
  /** For a seat with no start on its timer's clock: laps laid end to end from the start. */
  let laidOut = 0;
  let lastAt: number | undefined;
  const laps = seat.laps.filter((lap) => {
    laidOut += lap.micros;
    const endedAt = base === undefined ? laidOut : lap.ended_at - base;
    if (endedAt > sinceStart) return false;
    if (!lap.restart && !lap.timer_restarted) lastAt = endedAt;
    return true;
  });
  const counted = laps.filter((lap) => !lap.restart && !lap.timer_restarted);
  const holeShot =
    seat.hole_shot !== undefined && (base === undefined || seat.hole_shot.at - base <= sinceStart)
      ? seat.hole_shot
      : undefined;
  return {
    lastAt,
    seat: {
      ...seat,
      laps,
      hole_shot: holeShot,
      finish: undefined,
      best_lap_micros:
        counted.length > 0 ? Math.min(...counted.map((lap) => lap.micros)) : undefined,
      total_micros: undefined
    }
  };
}

/**
 * Places for a race in progress, 1 first, for the seats that have a pilot. On laps, then on
 * who got there first. A heat ranked on best N consecutive (`runLength`) goes on the best
 * run so far first. This is the running order a live race shows; the final order is the
 * race's own and is never worked out here.
 */
function placesSoFar(
  seats: { seat: RaceSeatView; lastAt?: number }[],
  runLength: number | undefined
): RaceSeatView[] {
  const last = Number.POSITIVE_INFINITY;
  const standing = seats
    .map((entry, index) => ({
      index,
      laps: entry.seat.laps.filter((lap) => !lap.restart && !lap.timer_restarted).length,
      lastAt: entry.lastAt ?? last,
      run:
        runLength === undefined
          ? last
          : (bestLapRun(entry.seat.laps, runLength)?.totalMicros ?? last)
    }))
    .filter((entry) => seats[entry.index].seat.pilot !== undefined)
    .sort((a, b) => a.run - b.run || b.laps - a.laps || a.lastAt - b.lastAt || a.index - b.index);
  const place = new Map(standing.map((entry, position) => [entry.index, position + 1]));
  return seats.map((entry, index) => ({ ...entry.seat, finish: place.get(index) }));
}

/**
 * A finished race as it stood at `now` on the Director's clock.
 *
 * - Before the start: counting down, with nothing flown.
 * - From the start to the stop: running, with the laps that had ended by then.
 * - From the stop on: finished, exactly as the race now stands, places and all.
 *
 * `runLength` is the N of a heat ranked on best N consecutive. A race that never started
 * is handed back as it is.
 */
export function raceAt(race: RaceView, now: number, runLength?: number): RaceView {
  const start = raceStart(race);
  if (start === undefined) return race;
  if (now < start) {
    return {
      ...race,
      state: 'Staging',
      tone_at: race.tone_at ?? start,
      started_at: undefined,
      stopped_at: undefined,
      seats: race.seats.map((seat) => ({
        ...seat,
        laps: [],
        hole_shot: undefined,
        finish: undefined,
        best_lap_micros: undefined,
        total_micros: undefined
      }))
    };
  }
  if (race.stopped_at === undefined || now >= race.stopped_at) {
    return { ...race, state: 'Stopped' };
  }
  const length = race.stopped_at - start;
  const seats = race.seats.map((seat) => seatAt(seat, now - start, seatBase(seat, start, length)));
  return {
    ...race,
    state: 'Running',
    started_at: start,
    stopped_at: undefined,
    seats: placesSoFar(seats, runLength)
  };
}

/**
 * The event as the overlay should see it at `now` while replaying `raceId`: that race cut
 * back to the moment, and only the races before it (a best time "this heat" must not know
 * about a race that had not been flown yet). `undefined` when the event has no such race.
 */
export function replayAt(
  view: EventView,
  raceId: RaceId,
  now: number
): { race: RaceView; view: EventView } | undefined {
  const index = view.races.findIndex((candidate) => candidate.race === raceId);
  if (index < 0) return undefined;
  const whole = view.races[index];
  const phase = view.phases.find((candidate) => candidate.def.id === whole.heat);
  const race = raceAt(whole, now, consecutiveLaps(phase?.format));
  return { race, view: { ...view, races: [...view.races.slice(0, index), race] } };
}
