/**
 * A seat's signal recorded for tuning, and the passes a pair of levels would make of it.
 * Pure, so it can be tested alone.
 *
 * Tuning by capture is: record the signal while a pilot flies a few passes, stop, then move
 * the enter and exit levels on the recording until each pass is one clean crossing. The
 * recording is the samples exactly as the timer sent them. {@link detectPasses} reads every
 * one of them; only drawing thins them (`thin` in `lib/signal.ts`).
 *
 * A recording lives in the page that made it. It is not sent to the Director or kept.
 */
import type { SampleSource } from './signal.js';

/** The most a recording holds: five minutes at 1 kHz. Recording stops itself there. */
export const RECORDING_CAPACITY = 300_000;

/** The samples of one seat, in the order they arrived, each with its time on the timer's clock. */
export class Recording implements SampleSource {
  readonly capacity: number;
  #times: number[] = [];
  #levels: number[] = [];

  constructor(capacity: number = RECORDING_CAPACITY) {
    this.capacity = capacity;
  }

  /** How many samples are held. */
  get length(): number {
    return this.#times.length;
  }

  /** Whether the recording has no room left. */
  get full(): boolean {
    return this.#times.length >= this.capacity;
  }

  /** The time of the first sample, in microseconds on the timer's clock. */
  get from(): number | undefined {
    return this.#times[0];
  }

  /** The time of the newest sample. */
  get to(): number | undefined {
    return this.#times.at(-1);
  }

  /** How long the recording is, in microseconds. */
  get spanMicros(): number {
    const from = this.from;
    const to = this.to;
    return from === undefined || to === undefined ? 0 : to - from;
  }

  /**
   * Add a chunk: `levels[i]` was taken at `from + i * periodMicros`. Samples past the
   * capacity are dropped. A chunk from before the newest sample (the timer's clock began
   * again) is dropped whole: one recording is on one clock.
   */
  push(from: number, periodMicros: number, levels: readonly number[]): void {
    const newest = this.to;
    if (newest !== undefined && from < newest) return;
    for (let i = 0; i < levels.length && !this.full; i += 1) {
      this.#times.push(from + i * periodMicros);
      this.#levels.push(levels[i]);
    }
  }

  each(since: number, until: number, visit: (time: number, level: number) => void): void {
    for (let i = 0; i < this.#times.length; i += 1) {
      const time = this.#times[i];
      if (time < since) continue;
      if (time >= until) break;
      visit(time, this.#levels[i]);
    }
  }

  /** The lowest and highest level recorded, or `undefined` for an empty recording. */
  range(): { lo: number; hi: number } | undefined {
    if (this.#levels.length === 0) return undefined;
    let lo = Number.POSITIVE_INFINITY;
    let hi = Number.NEGATIVE_INFINITY;
    for (const level of this.#levels) {
      if (level < lo) lo = level;
      if (level > hi) hi = level;
    }
    return { lo, hi };
  }
}

/** One pass a pair of levels makes of a recording. Times are on the timer's clock. */
export interface Pass {
  /** When the signal reached the enter level. */
  start: number;
  /** When the crossing closed, or the newest sample when it is still `open`. */
  end: number;
  /** The middle of the stretch at the peak level: where the lap time is taken. */
  at: number;
  peak: number;
  /** The recording ended before the crossing closed. */
  open: boolean;
}

/**
 * The passes these levels would make of a recording.
 *
 * A crossing opens when the signal reaches `enter`. It closes once the signal has stayed
 * below `exit` for `exitHoldMicros` (0 closes it at the first sample below). A dip below
 * exit shorter than the hold does not split a pass in two, which is what the hold is for.
 * A crossing closes at the sample where the signal first went below exit.
 *
 * This is the same rule the timer follows, so the passes shown are the ones the timer
 * would report. A timer adds its lap cooldown on top; that is left out here on purpose, so
 * a bounce shows up as two passes and can be tuned away.
 */
export function detectPasses(
  source: SampleSource,
  enter: number,
  exit: number,
  exitHoldMicros = 0
): Pass[] {
  const passes: Pass[] = [];
  let open = false;
  let start = 0;
  let peak = Number.NEGATIVE_INFINITY;
  let peakFirst = 0;
  let peakLast = 0;
  /** When the signal went below exit, while the hold is running. */
  let below: number | undefined;
  let last = 0;

  const close = (end: number, stillOpen: boolean) => {
    passes.push({ start, end, at: (peakFirst + peakLast) / 2, peak, open: stillOpen });
    open = false;
    below = undefined;
  };

  source.each(Number.NEGATIVE_INFINITY, Number.POSITIVE_INFINITY, (time, level) => {
    last = time;
    if (!open) {
      if (level < enter) return;
      open = true;
      start = time;
      peak = level;
      peakFirst = time;
      peakLast = time;
      below = undefined;
      return;
    }
    if (level >= exit) {
      // Back above exit before the hold ran out: still the same pass.
      below = undefined;
      if (level > peak) {
        peak = level;
        peakFirst = time;
        peakLast = time;
      } else if (level === peak) {
        peakLast = time;
      }
      return;
    }
    if (below === undefined) below = time;
    if (time - below >= exitHoldMicros) close(below, false);
  });
  // The recording ended inside a crossing, or before the exit hold ran out: still open.
  if (open) close(below ?? last, true);
  return passes;
}

/**
 * Move one of the two levels. Exit always stays at least `gap` below enter: a level moved
 * past the other one pushes it along, so a line never gets stuck against its neighbour.
 */
export function moveLevel(
  which: 'enter' | 'exit',
  value: number,
  enter: number,
  exit: number,
  gap = 1
): { enter: number; exit: number } {
  return which === 'enter'
    ? { enter: value, exit: Math.min(exit, value - gap) }
    : { enter: Math.max(enter, value + gap), exit: value };
}

/** The least room kept between enter and exit: half a dB, or one count. */
export function levelGap(unit: 'Counts' | 'TenthsDb' | 'TenthsDbm'): number {
  return unit === 'Counts' ? 1 : 5;
}
