/**
 * What a signed-in pilot sees beyond what a spectator does (docs/primitives.md §11.2):
 * their own upcoming heats with the seat and channel for each, an alert when they are
 * close to flying, and their own races laid out lap by lap.
 *
 * Pure, over the same snapshot everyone reads. Nothing here is private to the pilot: it
 * is the public race data, picked out for one person.
 */
import type { EventView, PhaseId, PilotId, RaceView } from '@gridfpv/types';
import type { Names } from './names.js';
import { raceNumber } from './marshal.js';
import { currentRace } from './phases.js';
import { upcomingHeats } from './spectator.js';

/** How close a pilot is to flying. */
export type Nearness = 'now' | 'next' | 'soon' | 'later';

/** One heat the pilot is in. */
export interface MyHeat {
  id: PhaseId;
  /** "Qualifying Heat 3". */
  name: string;
  /** "Seat 2". */
  seatName: string;
  /** "R1 · 5658 MHz", when the timer has said it. */
  channel?: string;
  /** How many heats fly before this one. 0 for the heat on now and for the next one up. */
  heatsBefore: number;
  nearness: Nearness;
}

/** A heat this many away or closer is "soon": time to get the quad and goggles ready. */
export const SOON_WITHIN = 2;

/**
 * The heats a pilot is in, in the order they will fly: the heat on now (if they are in
 * it), then the heats coming up.
 *
 * `heatsBefore` counts whole heats still to fly before theirs, the one on now included.
 * So with a race on, the first heat in the queue has one heat before it.
 */
export function myHeats(view: EventView | undefined, names: Names, pilot: PilotId): MyHeat[] {
  if (!view) return [];
  const found: MyHeat[] = [];
  const on = currentRace(view);
  if (on) {
    const index = on.seats.findIndex((seat) => seat.pilot === pilot);
    if (index >= 0) {
      const seat = on.seats[index];
      found.push({
        id: on.heat,
        name: names.phase(on.heat),
        seatName: names.seatPlain(seat.adapter, seat.seat, index),
        channel: names.channel(seat.adapter, seat.seat),
        heatsBefore: 0,
        nearness: 'now'
      });
    }
  }
  const racing = on ? 1 : 0;
  upcomingHeats(view, names).forEach((heat, position) => {
    const index = heat.seats.findIndex((entry) => entry.pilot === pilot);
    const seat = heat.seats[index];
    if (!seat) return;
    const heatsBefore = position + racing;
    found.push({
      id: heat.id,
      name: heat.name,
      // The channel is shown in full beside it, so the seat is named without one.
      seatName: names.seatPlain(seat.adapter, seat.seat, index),
      channel: names.channel(seat.adapter, seat.seat),
      heatsBefore,
      nearness: heatsBefore === 0 ? 'next' : heatsBefore <= SOON_WITHIN ? 'soon' : 'later'
    });
  });
  return found;
}

/** What to tell a pilot right now. */
export interface PilotAlert {
  nearness: Exclude<Nearness, 'later'>;
  /** "You are next". */
  title: string;
  /** "Qualifying Heat 3 · Seat 2 · R1 · 5658 MHz". */
  detail: string;
  /** Changes when the alert is about something new, so it buzzes once per change. */
  key: string;
}

/**
 * The alert for a pilot: about their nearest heat, when it is on now, next, or within
 * {@link SOON_WITHIN} heats. Nothing when their next heat is further off, or they have none.
 */
export function pilotAlert(heats: readonly MyHeat[]): PilotAlert | undefined {
  const nearest = heats[0];
  if (!nearest || nearest.nearness === 'later') return undefined;
  const where = [nearest.name, nearest.seatName, nearest.channel].filter(Boolean).join(' · ');
  const title =
    nearest.nearness === 'now'
      ? 'You are flying now'
      : nearest.nearness === 'next'
        ? 'You are next'
        : nearest.heatsBefore === 1
          ? 'You are 1 heat away'
          : `You are ${nearest.heatsBefore} heats away`;
  return {
    nearness: nearest.nearness,
    title,
    detail: where,
    key: `${nearest.id}\u0000${nearest.nearness}\u0000${nearest.heatsBefore}`
  };
}

/** How far off a heat is, for the list of a pilot's heats. */
export function nearnessWords(heat: MyHeat): string {
  if (heat.nearness === 'now') return 'On now';
  if (heat.nearness === 'next') return 'Next';
  return heat.heatsBefore === 1 ? '1 heat away' : `${heat.heatsBefore} heats away`;
}

/** One race a pilot has flown. */
export interface MyRace {
  /** A wire handle, for a list key only. */
  key: string;
  /** "Qualifying Heat 1 · race 2". */
  name: string;
  /** Place in the race, 1 first. */
  place?: number;
  /** Every counted lap in order, in microseconds. */
  laps: number[];
  bestLapMicros?: number;
  /** The index in `laps` of the best lap, for marking it. */
  bestLapIndex?: number;
  /** From the start of lap 1 to the last counted crossing. */
  totalMicros?: number;
  /** Whether a false start is recorded against the pilot in this race. */
  falseStart: boolean;
  /** Whether the pilot was marked out of this race (crashed, did not finish). */
  out: boolean;
}

/**
 * The races a pilot has flown that count, newest first. A race that was thrown out is not
 * theirs to look back on, and the race still going is on the "On now" tab.
 */
export function myRaces(view: EventView | undefined, names: Names, pilot: PilotId): MyRace[] {
  if (!view) return [];
  const mine: MyRace[] = [];
  for (const race of view.races) {
    if (race.state !== 'Saved') continue;
    const seat = race.seats.find((entry) => entry.pilot === pilot);
    if (!seat) continue;
    const laps = seat.laps
      .filter((lap) => !lap.restart && !lap.timer_restarted)
      .map((lap) => lap.micros);
    const best = seat.best_lap_micros ?? undefined;
    const bestLapIndex = best === undefined ? -1 : laps.indexOf(best);
    mine.push({
      key: race.race,
      name: `${names.phase(race.heat)} · race ${raceNumber(view, race)}`,
      place: seat.finish ?? undefined,
      laps,
      bestLapMicros: best,
      bestLapIndex: bestLapIndex >= 0 ? bestLapIndex : undefined,
      totalMicros: seat.total_micros ?? undefined,
      falseStart: seat.false_start,
      out: seat.out
    });
  }
  return mine.reverse();
}

/** Whether a race is the pilot's: they are on one of its seats. */
export function isMine(race: RaceView, pilot: PilotId | undefined): boolean {
  return pilot !== undefined && race.seats.some((seat) => seat.pilot === pilot);
}
