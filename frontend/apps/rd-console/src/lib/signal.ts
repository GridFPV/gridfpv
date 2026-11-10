/**
 * The signal behind the Tune page's live traces and the Marshal page's race traces. Pure, so
 * it can be tested alone.
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

/** Anything that can hand over its samples in a stretch of time: the live ring or a race's trace. */
export interface SampleSource {
  /** Visit every sample taken at or after `since` and before `until`, oldest first. */
  each(since: number, until: number, visit: (time: number, level: number) => void): void;
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
export function thin(ring: SampleSource, from: number, to: number, columns: number): Thinned {
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
  ring: SampleSource,
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

// ── A race's whole trace (the Marshal page) ──────────────────────────────────────────────────

/** A run of samples as it is logged: a start, a gap between samples, and the levels. */
export interface ChunkLike {
  from: number;
  period_micros: number;
  levels: readonly number[];
}

/**
 * One seat's logged signal for a whole race, in time order. It does not scroll and nothing is
 * dropped: drawing thins it for the screen with {@link thin}, the same as the live trace.
 */
export class RaceTrace implements SampleSource {
  readonly #times: Float64Array;
  readonly #levels: Float64Array;

  constructor(chunks: readonly ChunkLike[]) {
    const total = chunks.reduce((sum, chunk) => sum + chunk.levels.length, 0);
    this.#times = new Float64Array(total);
    this.#levels = new Float64Array(total);
    let at = 0;
    // The Director sends chunks oldest first; sorting here keeps `each` right if one is not.
    for (const chunk of [...chunks].sort((a, b) => a.from - b.from)) {
      for (let i = 0; i < chunk.levels.length; i += 1) {
        this.#times[at] = chunk.from + i * chunk.period_micros;
        this.#levels[at] = chunk.levels[i];
        at += 1;
      }
    }
  }

  /** How many samples are held. */
  get length(): number {
    return this.#times.length;
  }

  /** The time of the first sample, on the timer's clock. */
  get first(): number | undefined {
    return this.length === 0 ? undefined : this.#times[0];
  }

  /** The time of the last sample. */
  get last(): number | undefined {
    return this.length === 0 ? undefined : this.#times[this.length - 1];
  }

  each(since: number, until: number, visit: (time: number, level: number) => void): void {
    // Find the first sample at or after `since` by halving, so a zoomed-in view of a long
    // race does not walk the whole of it.
    let low = 0;
    let high = this.length;
    while (low < high) {
      const mid = (low + high) >> 1;
      if (this.#times[mid] < since) low = mid + 1;
      else high = mid;
    }
    for (let i = low; i < this.length && this.#times[i] < until; i += 1) {
      visit(this.#times[i], this.#levels[i]);
    }
  }
}

/** The stretch of time a plot shows, on the timer's clock. */
export interface TimeWindow {
  from: number;
  to: number;
}

/** The shortest stretch a race trace zooms in to: a fifth of a second. */
export const MIN_WINDOW_MICROS = 200_000;

/**
 * The whole of a race trace: from the earliest of the samples and the marks to the latest,
 * with a little room each side so a mark at the edge can be seen. `undefined` when there is
 * nothing to show.
 */
export function wholeWindow(
  trace: RaceTrace,
  marks: readonly { at: number }[] = []
): TimeWindow | undefined {
  const times = [
    ...(trace.first === undefined ? [] : [trace.first]),
    ...(trace.last === undefined ? [] : [trace.last]),
    ...marks.map((mark) => mark.at)
  ];
  if (times.length === 0) return undefined;
  const from = Math.min(...times);
  const to = Math.max(...times);
  const room = Math.max((to - from) * 0.02, MIN_WINDOW_MICROS / 4);
  return { from: from - room, to: to + room };
}

/** Keep a window inside `bounds`, no shorter than {@link MIN_WINDOW_MICROS}, without changing its length where it fits. */
export function clampWindow(window: TimeWindow, bounds: TimeWindow): TimeWindow {
  const full = bounds.to - bounds.from;
  const span = Math.min(full, Math.max(Math.min(MIN_WINDOW_MICROS, full), window.to - window.from));
  let from = window.from;
  if (from < bounds.from) from = bounds.from;
  if (from + span > bounds.to) from = bounds.to - span;
  return { from, to: from + span };
}

/**
 * Zoom a window by `factor` (below 1 zooms in, above 1 zooms out), keeping the time at
 * `anchor` where it is on the screen. The anchor defaults to the middle.
 */
export function zoomWindow(
  window: TimeWindow,
  bounds: TimeWindow,
  factor: number,
  anchor: number = (window.from + window.to) / 2
): TimeWindow {
  const span = window.to - window.from;
  if (span <= 0) return clampWindow(window, bounds);
  const full = bounds.to - bounds.from;
  const next = Math.min(full, Math.max(Math.min(MIN_WINDOW_MICROS, full), span * factor));
  const share = Math.min(1, Math.max(0, (anchor - window.from) / span));
  const from = anchor - share * next;
  return clampWindow({ from, to: from + next }, bounds);
}

/** Slide a window by `micros` (negative is earlier), stopping at the ends of `bounds`. */
export function panWindow(window: TimeWindow, bounds: TimeWindow, micros: number): TimeWindow {
  return clampWindow({ from: window.from + micros, to: window.to + micros }, bounds);
}

/** The time under the pixel `x` of a plot `width` pixels wide, in whole microseconds. */
export function timeAtX(window: TimeWindow, width: number, x: number): number {
  if (width <= 0) return Math.round(window.from);
  return Math.round(window.from + (x / width) * (window.to - window.from));
}

/** The pixel a time is drawn at. */
export function xAtTime(window: TimeWindow, width: number, time: number): number {
  const span = window.to - window.from;
  return span <= 0 ? 0 : ((time - window.from) / span) * width;
}

/**
 * The mark nearest to `time` among those no further than `reach` away, or `undefined`. Used
 * to tell a click on a crossing from a click on an empty spot.
 */
export function nearestMark<T extends { at: number }>(
  marks: readonly T[],
  time: number,
  reach: number
): T | undefined {
  let best: T | undefined;
  let bestGap = reach;
  for (const mark of marks) {
    const gap = Math.abs(mark.at - time);
    if (gap <= bestGap) {
      best = mark;
      bestGap = gap;
    }
  }
  return best;
}
