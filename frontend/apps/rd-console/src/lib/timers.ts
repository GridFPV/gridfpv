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
 * address as the timer gave it (absent on a timer that names no page), and `detail` names
 * what Grid is listed as on it.
 *
 * A timer that is not connected is said to be not connected, which is the bigger problem.
 */
export function controlTrouble(
  timer: Pick<Timer, 'name'>,
  status: Pick<TimerStatus, 'connected' | 'control' | 'control_name' | 'manage_url'>
): { trouble: string; detail: string; page: string | undefined } | undefined {
  if (!status.connected || status.control?.holder !== 'Other') return undefined;
  const who = status.control.name.trim();
  const grid = status.control_name ? `“${status.control_name}”` : 'Grid';
  return {
    trouble: who
      ? `“${who}” has control of ${timer.name}.`
      : `Another program has control of ${timer.name}.`,
    detail: `Grid still shows its settings and takes its laps, but cannot change it. To change it from here, open the timer's own page and give control to ${grid}.`,
    page: status.manage_url
  };
}

/** One thing a timer says is wrong with it, in words. */
export interface TimerProblem {
  /** The timer's own short name for it (`node_missing`). A key, not for showing. */
  code: string;
  trouble: string;
  detail: string;
}

/**
 * What a timer says is wrong with it (OFT spec §6.3), in words, in the timer's order. Empty
 * when it says nothing is.
 *
 * The timer gives short names. The ones Grid knows get a sentence. One it does not know is
 * shown as the timer wrote it, as the spec asks, so a newer timer's problem is still seen.
 *
 * A timer that is not connected is said to be not connected, which is the bigger problem.
 */
export function timerProblems(
  name: string,
  status: Pick<TimerStatus, 'connected' | 'timer_problems'>
): TimerProblem[] {
  if (!status.connected) return [];
  const seen = new Set<string>();
  return (status.timer_problems ?? [])
    .filter((code) => !seen.has(code) && seen.add(code))
    .map((code) => {
      switch (code) {
        case 'node_missing':
          return {
            code,
            trouble: `A receiver on ${name} is not answering.`,
            detail: 'Some seats may not be timed.'
          };
        case 'safe_mode':
          return {
            code,
            trouble: `${name} started in safe mode.`,
            detail: 'It may be using fewer receivers. Its own page says how to leave safe mode.'
          };
        case 'crashed':
          return {
            code,
            trouble: `${name} restarted after a failure.`,
            detail: 'It is timing again. The details are on its own page.'
          };
        default:
          return {
            code,
            trouble: `${name} reports a problem: ${code}.`,
            detail: 'Grid does not know this one. The timer’s own page may say more.'
          };
      }
    });
}
