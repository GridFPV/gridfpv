/**
 * Check-in (docs/primitives.md §11.9): practice run as one long heat with open seats. A
 * pilot takes a seat part-way through, and the laps on it are theirs until someone else
 * takes it or they drop it. A **stint** is one such spell on a seat.
 *
 * The Director works out which laps belong to which stint. These helpers only shape what
 * the snapshot already says for the screens that show it.
 */
import type { AdapterId, EventView, PilotId, RaceId, RaceView, SeatRef } from '@gridfpv/types';
import type { Names } from './names.js';

/** One open seat of a race that pilots can take. */
export interface OpenSeat {
  adapter: AdapterId;
  seat: SeatRef;
  /** "Seat 1". */
  seatName: string;
  /** Who has it now, when someone does. */
  holder?: PilotId;
  /** That pilot's callsign. */
  holderName?: string;
}

/** What a pilot can ask for. Each rejects with the Director's reason when refused. */
export interface CheckinActions {
  take(race: RaceId, seat: { adapter: AdapterId; seat: SeatRef }): Promise<unknown>;
  drop(race: RaceId): Promise<unknown>;
}

/** Whether a race can have a seat taken in it: it is counting down or being flown. */
export const takesCheckIns = (race: RaceView): boolean =>
  race.state === 'Staging' || race.state === 'Running';

/**
 * The race pilots can check in to right now: one that is being flown and has a seat with
 * no pilot named on it. There is at most one race going at a time.
 */
export function checkInRace(view: EventView | undefined): RaceView | undefined {
  return view?.races.find(
    (race) => takesCheckIns(race) && race.seats.some((seat) => seat.pilot === undefined)
  );
}

/** The open seats of a race, in lineup order, each with who has it now. */
export function openSeats(race: RaceView, names: Names): OpenSeat[] {
  return race.seats.flatMap((seat, index) =>
    seat.pilot !== undefined
      ? []
      : [
          {
            adapter: seat.adapter,
            seat: seat.seat,
            seatName: names.seat(seat.adapter, seat.seat, index),
            holder: seat.holder,
            holderName: seat.holder === undefined ? undefined : names.pilot(seat.holder)
          }
        ]
  );
}

/** One spell a pilot had on an open seat. */
export interface PracticeRun {
  /** A wire handle, for a list key only. */
  key: string;
  /** "Open Practice · Seat 1". */
  name: string;
  /** Whether the pilot still has the seat. */
  open: boolean;
  /** Every lap of the stint in order, in microseconds. */
  laps: number[];
  bestLapMicros?: number;
  /** The index in `laps` of the best lap, for marking it. */
  bestLapIndex?: number;
}

/**
 * A pilot's stints on open seats, newest first: the practice laps that are theirs. They
 * are shown whether or not the practice is ranked, and while the race is still going. A
 * race that was thrown out is left out.
 */
export function practiceRuns(
  view: EventView | undefined,
  names: Names,
  pilot: PilotId
): PracticeRun[] {
  if (!view) return [];
  const runs: PracticeRun[] = [];
  for (const race of view.races) {
    if (race.state === 'Discarded') continue;
    race.seats.forEach((seat, seatIndex) => {
      seat.stints.forEach((stint, stintIndex) => {
        if (stint.pilot !== pilot) return;
        const laps = stint.laps
          .filter((lap) => !lap.restart && !lap.timer_restarted)
          .map((lap) => lap.micros);
        const best = stint.best_lap_micros ?? undefined;
        const bestLapIndex = best === undefined ? -1 : laps.indexOf(best);
        runs.push({
          key: `${race.race}/${seat.adapter}/${seat.seat}/${stintIndex}`,
          name: `${names.phase(race.heat)} · ${names.seat(seat.adapter, seat.seat, seatIndex)}`,
          open: stint.open && takesCheckIns(race),
          laps,
          bestLapMicros: best,
          bestLapIndex: bestLapIndex >= 0 ? bestLapIndex : undefined
        });
      });
    });
  }
  return runs.reverse();
}
