/**
 * PLACEHOLDER: the seats a heat is flown on, until timers are wired in.
 *
 * No timer is connected to the console yet, so there is nothing to ask for a seat list.
 * This module stands in for one: a made-up timer called `bench` with eight seats. When
 * timers come back, the seat list comes from them and this file goes away. Everything that
 * needs a seat reads it from here, so that change is made in one place.
 */
import type { AdapterId, PresetSeat, SeatRef } from '@gridfpv/types';

/** The stand-in timer every bench seat belongs to. A wire handle; never shown. */
export const BENCH_ADAPTER: AdapterId = 'bench';

/** One seat: its wire handle and the name people see. */
export interface Seat {
  adapter: AdapterId;
  seat: SeatRef;
  label: string;
}

/** The most pilots one heat can hold: one per seat. */
export const MAX_SEATS = 8;

/** The eight bench seats, in order. */
export const SEATS: readonly Seat[] = Array.from({ length: MAX_SEATS }, (_, index) => ({
  adapter: BENCH_ADAPTER,
  seat: `seat-${index + 1}`,
  label: `Seat ${index + 1}`
}));

/**
 * The name to show for a seat. A seat this module does not know is named by its place in
 * the lineup (`position`, counted from 0), so a raw seat handle never reaches the screen.
 */
export function seatLabel(adapter: AdapterId, seat: SeatRef, position?: number): string {
  const known = SEATS.find((s) => s.adapter === adapter && s.seat === seat);
  if (known) return known.label;
  return position === undefined ? 'Seat' : `Seat ${position + 1}`;
}

/** The first `count` seats, in the shape the bracket and mains presets take. */
export function presetSeats(count: number): PresetSeat[] {
  return SEATS.slice(0, count).map(({ adapter, seat }) => ({ adapter, seat }));
}

/**
 * Bench tool: show a "Crossing" button on each seat of the Run view, which logs a gate
 * crossing by hand. It is here so a race can be run before any timer exists. Turn it off
 * (or delete it) once timers send the crossings.
 */
export const SIMULATE_CROSSINGS = true;
