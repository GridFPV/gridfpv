/**
 * The live signal behind the Tune page's traces. Pure, so it can be tested alone.
 *
 * A timer sends each seat's signal in chunks: a start time on the timer's clock, a gap
 * between samples, and the samples. {@link SignalRing} keeps the newest of them for one seat,
 * exactly as they arrived. Drawing reads the ring and **thins for the screen only**
 * ({@link thin}): ten seconds at 1 kHz is ten thousand samples for a plot a few hundred
 * pixels wide, so each pixel column draws the lowest and highest sample that falls in it. No
 * sample in the ring is ever changed or dropped by drawing.
 */
import type { SignalUnit } from '@gridfpv/types';

/** How much of the trace is on screen: the last ten seconds. */
export const TRACE_WINDOW_MICROS = 10_000_000;

/**
 * How many samples a ring keeps: ten seconds at 2 kHz. An OFT timer samples at 1 kHz, so
 * this is the window twice over.
 */
export const RING_CAPACITY = 20_000;

/** The newest samples of one seat, each with its time on the timer's clock. Oldest go first. */
export class SignalRing {
  readonly capacity: number;
  readonly #times: Float64Array;
  readonly #levels: Float64Array;
  /** Where the oldest sample is. */
  #start = 0;
  #length = 0;

  constructor(capacity: number = RING_CAPACITY) {
    this.capacity = capacity;
    this.#times = new Float64Array(capacity);
    this.#levels = new Float64Array(capacity);
  }

  /** How many samples are held. */
  get length(): number {
    return this.#length;
  }

  /** The time of the newest sample, in microseconds on the timer's clock. */
  get latest(): number | undefined {
    if (this.#length === 0) return undefined;
    return this.#times[(this.#start + this.#length - 1) % this.capacity];
  }

  /** Add a chunk: `levels[i]` was taken at `from + i * periodMicros`. */
  push(from: number, periodMicros: number, levels: readonly number[]): void {
    for (let i = 0; i < levels.length; i += 1) {
      const at = (this.#start + this.#length) % this.capacity;
      this.#times[at] = from + i * periodMicros;
      this.#levels[at] = levels[i];
      if (this.#length < this.capacity) this.#length += 1;
      else this.#start = (this.#start + 1) % this.capacity;
    }
  }

  /** Forget everything. */
  clear(): void {
    this.#start = 0;
    this.#length = 0;
  }

  /** Visit every sample taken at or after `since` and before `until`, oldest first. */
  each(since: number, until: number, visit: (time: number, level: number) => void): void {
    for (let i = 0; i < this.#length; i += 1) {
      const at = (this.#start + i) % this.capacity;
      const time = this.#times[at];
      if (time < since || time >= until) continue;
      visit(time, this.#levels[at]);
    }
  }
}

/** A trace thinned to one lowest and one highest level for each pixel column. */
export interface Thinned {
  /** The lowest level in each column. `NaN` where the column has no sample. */
  min: Float64Array;
  /** The highest level in each column. `NaN` where the column has no sample. */
  max: Float64Array;
  /** The lowest level in the whole window, or `undefined` when it holds no sample. */
  lo: number | undefined;
  /** The highest level in the whole window. */
  hi: number | undefined;
}

/**
 * Thin the samples between `from` and `to` into `columns` columns for drawing. A peak is
 * never lost: each column keeps the lowest and the highest sample that fell in it.
 */
export function thin(ring: SignalRing, from: number, to: number, columns: number): Thinned {
  const min = new Float64Array(columns).fill(Number.NaN);
  const max = new Float64Array(columns).fill(Number.NaN);
  let lo: number | undefined;
  let hi: number | undefined;
  const span = to - from;
  if (columns > 0 && span > 0) {
    ring.each(from, to, (time, level) => {
      const column = Math.min(columns - 1, Math.floor(((time - from) / span) * columns));
      if (Number.isNaN(min[column]) || level < min[column]) min[column] = level;
      if (Number.isNaN(max[column]) || level > max[column]) max[column] = level;
      if (lo === undefined || level < lo) lo = level;
      if (hi === undefined || level > hi) hi = level;
    });
  }
  return { min, max, lo, hi };
}

/** A stretch of time the signal spent in a crossing. */
export interface Span {
  from: number;
  to: number;
}

/**
 * The stretches between `from` and `to` where the signal is in a crossing against these
 * levels: a crossing opens when the signal reaches `enter` and closes when it falls below
 * `exit`. A crossing still open at the newest sample ends there.
 *
 * This reads every sample, not the thinned ones, so a short crossing is not missed.
 */
export function crossingSpans(
  ring: SignalRing,
  from: number,
  to: number,
  enter: number,
  exit: number
): Span[] {
  const spans: Span[] = [];
  let opened: number | undefined;
  let last = from;
  ring.each(from, to, (time, level) => {
    if (opened === undefined) {
      if (level >= enter) opened = time;
    } else if (level < exit) {
      spans.push({ from: opened, to: time });
      opened = undefined;
    }
    last = time;
  });
  if (opened !== undefined) spans.push({ from: opened, to: last });
  return spans;
}

/** The lowest and highest level a plot shows. */
export interface Range {
  min: number;
  max: number;
}

/** What a plot shows before any signal has arrived. */
function restingRange(unit: SignalUnit): Range {
  return unit === 'Counts' ? { min: 0, max: 250 } : { min: -1000, max: -300 };
}

/**
 * The vertical range of a plot. Before any signal arrives it is fixed. After that it follows
 * what is on screen: the signal (`lo` to `hi`) and the `marks` (the enter, exit and noise
 * floor lines), with a little room, snapped outward to whole steps of 5 dB (or 10 counts) so
 * it does not twitch, and never less than 20 dB tall.
 */
export function verticalRange(
  unit: SignalUnit,
  lo: number | undefined,
  hi: number | undefined,
  marks: readonly (number | undefined)[] = []
): Range {
  if (lo === undefined || hi === undefined) return restingRange(unit);
  const step = unit === 'Counts' ? 10 : 50;
  const least = unit === 'Counts' ? 40 : 200;
  let min = lo;
  let max = hi;
  for (const mark of marks) {
    if (mark === undefined) continue;
    if (mark < min) min = mark;
    if (mark > max) max = mark;
  }
  const room = step / 2;
  min = Math.floor((min - room) / step) * step;
  max = Math.ceil((max + room) / step) * step;
  while (max - min < least) {
    max += step;
    if (max - min < least) min -= step;
  }
  return { min, max };
}

/**
 * Where "now" is on a timer's clock.
 *
 * Sample times are on the timer's clock, and a trace should scroll smoothly between chunks
 * and keep scrolling when chunks stop (so a seat that went quiet shows as quiet). Each chunk
 * says how far the timer's clock is ahead of this machine's; the largest lead seen is the one
 * with the least delay in it, and "now" is this machine's clock plus that lead.
 *
 * A timer that restarts starts its clock again, so a lead that falls far below the one held
 * replaces it.
 */
export class TimerClock {
  /** Timer microseconds minus local microseconds. */
  #lead: number | undefined;

  /** A sample taken at `timerMicros` arrived at `localMs` on this machine's clock. */
  observe(timerMicros: number, localMs: number): void {
    const lead = timerMicros - localMs * 1000;
    if (this.#lead === undefined || lead > this.#lead || lead < this.#lead - 2_000_000) {
      this.#lead = lead;
    }
  }

  /** The time on the timer's clock at `localMs`, or `undefined` before any sample. */
  now(localMs: number): number | undefined {
    return this.#lead === undefined ? undefined : localMs * 1000 + this.#lead;
  }
}
