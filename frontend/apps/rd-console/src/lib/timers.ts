/**
 * Words for a timer's status on the Timers page, and the timers found on the network. Pure, so they can be tested alone.
 */
import type { DiscoveredTimer, Timer, TimerKind, TimerStatus } from '@gridfpv/types';

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
    label: 'Open FPV Timer',
    port: OFT_PORT,
    hint: ''
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

/** A number of seats in words: "4 seats", "1 seat". */
export function seatCount(count: number): string {
  return `${count} ${count === 1 ? 'seat' : 'seats'}`;
}

/** How many seats the timer reports: "4 seats", "1 seat", "No seats reported yet". */
export function seatCountSentence(status: TimerStatus): string {
  const count = status.seats.length;
  return count === 0 ? 'No seats reported yet' : seatCount(count);
}

/**
 * The timers found on the network that are not added yet, in the order the Director gave
 * them. One that is already added is in the timer list, so it is not offered again.
 */
export function notAddedYet(found: readonly DiscoveredTimer[]): DiscoveredTimer[] {
  return found.filter((timer) => !timer.added);
}

/**
 * What to say on the Run page when the event's pass window is not in force on a timer
 * (docs/primitives.md §12.6), or `undefined` when there is nothing to say.
 *
 * The timer applies the pass window, so it only holds on a timer that has one. With none
 * set on the event there is nothing to miss. A timer that is not connected is said to be
 * not connected, which is the bigger problem, so nothing is added here.
 */
export function passWindowTrouble(
  name: string,
  status: Pick<TimerStatus, 'connected' | 'pass_window' | 'pass_window_problem'>,
  windowMicros: number | undefined
): { trouble: string; detail: string } | undefined {
  if (!status.connected) return undefined;
  if (status.pass_window_problem !== undefined) {
    return {
      trouble: `The pass window could not be set on ${name}.`,
      detail: status.pass_window_problem
    };
  }
  if (windowMicros === undefined || windowMicros <= 0) return undefined;
  if (!status.pass_window) {
    const seconds = windowMicros / 1_000_000;
    return {
      trouble: `${name} has no pass window.`,
      detail: `The event's pass window of ${seconds} s is not in force on it: every crossing it reports counts by itself.`
    };
  }
  return undefined;
}

/**
 * What to say when another program has control of a timer, or `undefined` when Grid may
 * change it.
 *
 * An OFT timer lets one program at a time change its settings (OFT spec §6.6). Grid asks to
 * be that program when it connects, and the first to ask is the one. When another program
 * got there first (RotorHazard, or a person at the timer's own page), Grid still reads the
 * timer and takes its laps, but the timer refuses Grid's changes. Only a person at the
 * timer's own page can move control, so the words point there: `page` is that page's
 * address, and `detail` names what Grid is listed as on it.
 *
 * A timer that is not connected is said to be not connected, which is the bigger problem.
 */
export function controlTrouble(
  timer: Pick<Timer, 'name' | 'address'>,
  status: Pick<TimerStatus, 'connected' | 'control' | 'control_name'>
): { trouble: string; detail: string; page: string } | undefined {
  if (!status.connected || status.control?.holder !== 'Other') return undefined;
  const who = status.control.name.trim();
  const grid = status.control_name ? `“${status.control_name}”` : 'Grid';
  return {
    trouble: who
      ? `“${who}” has control of ${timer.name}.`
      : `Another program has control of ${timer.name}.`,
    detail: `Grid still shows its settings and takes its laps, but cannot change it. To change it from here, open the timer's own page and give control to ${grid}.`,
    page: `http://${timer.address}/`
  };
}
