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
  },
  {
    kind: 'RotorHazard',
    label: 'A RotorHazard timer',
    port: 5000,
    hint: 'Used as it is. Nothing is installed on it. Its settings are changed on its own pages for now.'
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
