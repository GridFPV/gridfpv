/**
 * Words for a timer's status on the Timers page. Pure, so they can be tested alone.
 */
import type { TimerKind, TimerStatus } from '@gridfpv/types';

/** How often a screen that shows timers reads the list again. Timers have no change feed. */
export const TIMER_POLL_MS = 2000;

/** The port an OFT timer listens on. */
export const OFT_PORT = 7270;

/**
 * The kinds of timer that can be added, as the add-timer dialog offers them. `port` is the
 * port that kind listens on unless someone changed it.
 *
 * A RotorHazard timer is not offered: the Director can hold one, but that adapter is paused
 * until the OFT one is fully tested and its shape is agreed (2026-11-10).
 */
export const TIMER_KINDS: readonly {
  kind: TimerKind;
  label: string;
  port: number;
  hint: string;
}[] = [
  {
    kind: 'Oft',
    label: 'An OFT timer',
    port: OFT_PORT,
    hint: 'An Open FPV Timer gate.'
  }
];

/**
 * How well the timer's clock is known: "Clock known to within 0.2 ms". The number is the
 * most the timer's clock can be off from the Director's.
 */
export function clockSentence(errorMicros: number | undefined): string {
  if (errorMicros === undefined) return 'Clock not measured yet';
  const ms = Math.abs(errorMicros) / 1000;
  if (ms < 0.05) return 'Clock known to within 0.1 ms';
  if (ms < 10) return `Clock known to within ${ms.toFixed(1)} ms`;
  return `Clock known to within ${Math.round(ms)} ms`;
}

/** How many seats the timer reports: "4 seats", "1 seat", "No seats reported yet". */
export function seatCountSentence(status: TimerStatus): string {
  const count = status.seats.length;
  if (count === 0) return 'No seats reported yet';
  return `${count} ${count === 1 ? 'seat' : 'seats'}`;
}
