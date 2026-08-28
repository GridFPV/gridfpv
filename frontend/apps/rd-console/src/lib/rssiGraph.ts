/**
 * RssiGraph geometry + detection — the pure half of `RssiGraph.svelte` (#355).
 *
 * The graph renders in two modes (marshaling **review** of a finished heat, and **live** tuning
 * against a timer's heartbeat), and everything in this module is what the two modes have in
 * common: the plot's fixed geometry, the time/value projections, the downsampled sample
 * polyline, and — the piece the live crossing band and the marshaling detection band must share
 * rather than reimplement — {@link crossingWindows}.
 *
 * Nothing here touches component state, so the mode difference never reaches it: a mode picks a
 * *span* ({@link spanOf} for a whole finished heat, {@link rollingSpanOf} for a live window) and
 * everything downstream projects against that span identically.
 */
import type { CompetitorTrace } from '@gridfpv/types';

/** A source-clock time window (µs), the horizontal extent something is drawn against. */
export type Span = { from: number; to: number };
/** An RSSI value window, the vertical extent something is drawn against. */
export type Range = { lo: number; hi: number };

// Plot geometry. A fixed viewBox keeps the SVG crisp at any rendered size; strokes are in
// user units. Left/bottom gutters leave room for the axis labels.
export const W = 1000;
export const H = 220;
export const PAD_L = 8;
export const PAD_R = 8;
export const PAD_T = 10;
export const PAD_B = 18;
export const plotW = W - PAD_L - PAD_R;
export const plotH = H - PAD_T - PAD_B;

// Cap the points we actually draw — a long heat at the streaming cadence can be thousands of
// samples; more than one point per horizontal pixel is invisible. Downsample for the canvas
// only (the raw trace keeps full fidelity); we stride-pick rather than average to keep peaks.
const MAX_POINTS = 1200;

/**
 * A sample's source-clock time (µs). Dense traces carry the **actual** per-sample `times` (RH's
 * marshal history is bursty — clustered around each crossing — so the uniform `from + i·period`
 * grid badly compresses it); the coarse streaming path has no `times`, so its exact uniform grid
 * is used instead.
 */
export function sampleTimeOf(t: CompetitorTrace, i: number): number {
  const explicit = t.times?.[i];
  return explicit ?? (t.from ?? 0) + i * t.period_micros;
}

/**
 * A trace's plotted source-clock span — the first to the last sample's real instant — **widened to
 * include every lap time** in `at` so every lap still lands inside the plot and gets a marker. Using
 * the real sample times (not the uniform grid) keeps the signal spanning its true duration, so it
 * lines up with the lap markers instead of compressing into the left. Falls back to a unit span.
 *
 * This is the **review** axis: a finished heat, drawn whole.
 */
export function spanOf(t: CompetitorTrace, at: number[] = []): Span {
  const n = t.samples.length;
  let from = n > 0 ? sampleTimeOf(t, 0) : (t.from ?? 0);
  let to = n > 1 ? sampleTimeOf(t, n - 1) : from + 1;
  for (const time of at) {
    if (time < from) from = time;
    if (time > to) to = time;
  }
  return { from, to };
}

/**
 * The **live** axis: a rolling window of `windowMicros` whose RIGHT edge is pinned to the newest
 * sample. Pinning the right edge (rather than fitting whatever the buffer happens to hold) is what
 * makes the trace scroll steadily leftwards while tuning, instead of rescaling on every frame — and
 * it means a buffer shorter than the window fills in from the right rather than being stretched.
 */
export function rollingSpanOf(t: CompetitorTrace, windowMicros: number): Span {
  const n = t.samples.length;
  const to = n > 0 ? sampleTimeOf(t, n - 1) : (t.from ?? 0);
  return { from: to - Math.max(1, windowMicros), to };
}

/** RSSI value range for a trace, padded so the thresholds and peaks aren't flush to the edge. */
export function valueRange(
  t: CompetitorTrace,
  enter: number | undefined = t.enter,
  exit: number | undefined = t.exit
): Range {
  let lo = Infinity;
  let hi = -Infinity;
  for (const v of t.samples) {
    if (v < lo) lo = v;
    if (v > hi) hi = v;
  }
  for (const th of [t.enter, t.exit, enter, exit]) {
    if (th != null) {
      if (th < lo) lo = th;
      if (th > hi) hi = th;
    }
  }
  if (!isFinite(lo) || !isFinite(hi)) return { lo: 0, hi: 1 };
  if (lo === hi) {
    lo -= 1;
    hi += 1;
  }
  const pad = (hi - lo) * 0.08;
  return { lo: lo - pad, hi: hi + pad };
}

/** Project a source-clock time onto the plot's X (user units). */
export function xOf(time: number, span: Span): number {
  const w = span.to - span.from || 1;
  return PAD_L + ((time - span.from) / w) * plotW;
}

/**
 * #473 (live glide): how far past the newest sample the live plot may extrapolate, in µs.
 *
 * Between polls the plot glides left on the wall clock so the trace scrolls smoothly instead of
 * stepping at poll cadence. The cap is what keeps that honest: a healthy feed delivers every
 * ~200-250 ms, so the glide normally spans well under a second — but a stalled feed must FREEZE
 * (trace visibly stops, gap opens at the live edge) rather than scroll the last real samples off
 * the screen. 1.5 s is several missed polls: unmistakably a stall, not jitter.
 */
export const LIVE_GLIDE_CAP_MICROS = 1_500_000;

/**
 * The live plot's wall-clock extrapolation past its newest sample (µs): how long ago the current
 * trace prop was received, clamped to {@link LIVE_GLIDE_CAP_MICROS}. Pure — the component feeds it
 * `performance.now()` pairs; a fresh poll resets `latestSeenAtMs` and the glide restarts near 0.
 */
export function liveGlideMicros(
  latestSeenAtMs: number,
  nowMs: number,
  cap: number = LIVE_GLIDE_CAP_MICROS
): number {
  return Math.min(Math.max(0, (nowMs - latestSeenAtMs) * 1000), cap);
}

/**
 * The x-shift (user units, leftward-positive) for a live glide of `micros` within a rolling window
 * of `windowMicros` — the same time→px scale {@link xOf} draws with, so glided geometry lands
 * exactly where the next poll's re-render will put it.
 */
export function glideShiftPx(micros: number, windowMicros: number): number {
  return (micros / (windowMicros || 1)) * plotW;
}

/** Project an RSSI value onto the plot's Y (user units; higher value = higher on screen). */
export function yOf(value: number, range: Range): number {
  const h = range.hi - range.lo || 1;
  return PAD_T + plotH - ((value - range.lo) / h) * plotH;
}

/** Invert {@link xOf}: a plot X (user units) back to a source-clock time, clamped to the span. */
export function timeAt(x: number, span: Span): number {
  const w = span.to - span.from || 1;
  const frac = Math.min(1, Math.max(0, (x - PAD_L) / plotW));
  return span.from + frac * w;
}

/** One plotted sample, in user units. */
export type Point = { x: number; y: number };

/**
 * The downsampled sample points, in user units — the geometry both the raw polyline
 * ({@link pointsAttr}) and the smoothed curve ({@link smoothPath}) are built from.
 *
 * Split out of {@link polyline} for #473 so the two renderings are guaranteed to be drawn from the
 * SAME points: a smoothing mode that re-derived its own point list could quietly disagree with the
 * raw trace about where a sample is, which is exactly the dishonesty the issue rules out.
 */
export function samplePoints(t: CompetitorTrace, span: Span, range: Range): Point[] {
  const n = t.samples.length;
  if (n === 0) return [];
  // Only the samples inside the (possibly zoomed, or live-windowed) span, plus one neighbor each
  // side so the line enters/exits the frame — this is what makes zooming reveal detail, and what
  // keeps a live window cheap: the downsampling budget is spent on the visible window, not the
  // whole capture.
  let lo = 0;
  while (lo < n - 1 && sampleTimeOf(t, lo + 1) < span.from) lo++;
  let hi = n - 1;
  while (hi > 0 && sampleTimeOf(t, hi - 1) > span.to) hi--;
  const visible = hi - lo + 1;
  const stride = visible > MAX_POINTS ? Math.ceil(visible / MAX_POINTS) : 1;
  const pts: Point[] = [];
  for (let i = lo; i <= hi; i += stride) {
    pts.push({ x: xOf(sampleTimeOf(t, i), span), y: yOf(t.samples[i], range) });
  }
  // Always include the last visible sample so the line reaches the end of the span.
  pts.push({ x: xOf(sampleTimeOf(t, hi), span), y: yOf(t.samples[hi], range) });
  return pts;
}

/** Points in user units → an SVG `points` attribute (one decimal, the plot's drawing precision). */
export function pointsAttr(pts: Point[]): string {
  return pts.map((p) => `${p.x.toFixed(1)},${p.y.toFixed(1)}`).join(' ');
}

/** The downsampled sample polyline as an SVG points string. */
export function polyline(t: CompetitorTrace, span: Span, range: Range): string {
  return pointsAttr(samplePoints(t, span, range));
}

/**
 * Fritsch–Carlson tangents for a **monotone** cubic Hermite interpolant (#473).
 *
 * This is the half of the smoothing that has to be right, so it is a pure function of two arrays
 * and unit-tested on its own. Two properties are load-bearing, and both are why this scheme was
 * chosen over the alternatives the issue listed (draw-time EMA / leading-edge tweening):
 *
 *  1. **It interpolates.** The curve passes exactly through every sample point, so a peak stays at
 *     the x of the sample that produced it. An EMA or a tween *shifts the signal in time* — the
 *     peak slides, and with it the apparent instant of a gate pass. On a page whose whole job is
 *     telling a marshal *when* a crossing happened, that is not a cosmetic difference.
 *  2. **It never overshoots.** The limiter below (`s > 9 → τ = 3/√s`, and the zero-tangent rule at
 *     a direction change) is what keeps the curve inside the box of each segment's two endpoints.
 *     An unconstrained spline rings past a sharp peak, and a curve that bulges above `enter`
 *     between two samples that never reached it would draw a crossing the detector never saw.
 *
 * Together those two are the issue's "a smoothed curve must not visually move where a pass peak
 * sits": the peak cannot move (1) and no new peak can appear (2).
 *
 * `xs` must be non-decreasing. Segments of zero width contribute no slope (their tangents are
 * pinned to 0), so duplicate sample instants degrade to a flat join rather than dividing by zero.
 */
export function monotoneTangents(xs: number[], ys: number[]): number[] {
  const n = Math.min(xs.length, ys.length);
  if (n === 0) return [];
  if (n === 1) return [0];

  // Secant slope of each segment.
  const delta: number[] = [];
  for (let i = 0; i < n - 1; i++) {
    const h = xs[i + 1] - xs[i];
    delta.push(h > 0 ? (ys[i + 1] - ys[i]) / h : 0);
  }

  // Initial tangents: the average of the two adjacent secants, one-sided at the ends.
  const m: number[] = new Array(n);
  m[0] = delta[0];
  m[n - 1] = delta[n - 2];
  for (let i = 1; i < n - 1; i++) m[i] = (delta[i - 1] + delta[i]) / 2;

  // The Fritsch–Carlson limiter — this is what makes it monotone (and so non-overshooting).
  for (let i = 0; i < n - 1; i++) {
    if (delta[i] === 0) {
      // A flat segment. Pinning BOTH ends to zero is what puts a horizontal tangent exactly at a
      // local extremum — the peak of a pass sits ON its sample, with no ringing either side.
      m[i] = 0;
      m[i + 1] = 0;
      continue;
    }
    const a = m[i] / delta[i];
    const b = m[i + 1] / delta[i];
    // A tangent pointing against the segment's direction is a local extremum: flatten it, or the
    // curve leaves the segment's value box on the way in/out.
    if (a < 0) m[i] = 0;
    if (b < 0) m[i + 1] = 0;
    const s = a * a + b * b;
    if (s > 9) {
      const tau = 3 / Math.sqrt(s);
      m[i] = tau * a * delta[i];
      m[i + 1] = tau * b * delta[i];
    }
  }
  return m;
}

/**
 * The smoothed signal as an SVG path `d` — a monotone cubic Hermite spline through the SAME points
 * the raw polyline draws, emitted as cubic Béziers (#473).
 *
 * Hermite → Bézier is exact: for a segment of width `h`, the control points sit a third of the way
 * in along each endpoint's tangent. So this draws precisely the curve {@link monotoneTangents}
 * describes, with no re-approximation.
 *
 * Degenerate inputs render as themselves rather than as nothing: no points → an empty `d`, one
 * point → a bare move (an empty stroke, which is what a single sample looks like), two points → the
 * straight line between them. A zero-width segment (two samples at the same instant) is emitted as
 * a `L`, because a Bézier across zero width has no shape to describe.
 */
export function smoothPath(pts: Point[]): string {
  const n = pts.length;
  if (n === 0) return '';
  const head = `M ${pts[0].x.toFixed(1)},${pts[0].y.toFixed(1)}`;
  if (n === 1) return head;

  const xs = pts.map((p) => p.x);
  const ys = pts.map((p) => p.y);
  const m = monotoneTangents(xs, ys);

  const out: string[] = [head];
  for (let i = 0; i < n - 1; i++) {
    const h = xs[i + 1] - xs[i];
    if (h <= 0) {
      out.push(`L ${xs[i + 1].toFixed(1)},${ys[i + 1].toFixed(1)}`);
      continue;
    }
    const c1x = xs[i] + h / 3;
    const c1y = ys[i] + (m[i] * h) / 3;
    const c2x = xs[i + 1] - h / 3;
    const c2y = ys[i + 1] - (m[i + 1] * h) / 3;
    out.push(
      `C ${c1x.toFixed(1)},${c1y.toFixed(1)} ${c2x.toFixed(1)},${c2y.toFixed(1)} ` +
        `${xs[i + 1].toFixed(1)},${ys[i + 1].toFixed(1)}`
    );
  }
  return out.join(' ');
}

/**
 * The time windows the lap-detection engine "sees" a crossing, replaying the timer's own
 * enter→exit hysteresis over the captured samples: a window OPENS at the first sample that rises
 * to/above `enter` and CLOSES at the first subsequent sample that falls to/below `exit` (one window
 * per detected pass). A window still open at the last sample extends to it. Empty unless both
 * levels are present.
 *
 * Shared by both modes, and deliberately so: in **review** this shades what the detector saw over a
 * finished heat, and in **live** the same code produces the rolling band RotorHazard's own tuning
 * page draws — one that opens the moment the signal crosses enter and closes when it falls back
 * past exit, the still-open one running to the newest sample. Two implementations of that would
 * drift on the first change to either.
 *
 * Display-only in both modes: this visualises what the detector saw (at the tuned levels while
 * adjusting), it does not re-detect or change any lap.
 */
export function crossingWindows(
  t: CompetitorTrace,
  enter: number | undefined = t.enter,
  exit: number | undefined = t.exit
): Span[] {
  if (enter == null || exit == null) return [];
  const n = t.samples.length;
  const out: Span[] = [];
  let inCrossing = false;
  let start = 0;
  for (let i = 0; i < n; i++) {
    const v = t.samples[i];
    const time = sampleTimeOf(t, i);
    if (!inCrossing) {
      if (v >= enter) {
        inCrossing = true;
        start = time;
      }
    } else if (v <= exit) {
      out.push({ from: start, to: time });
      inCrossing = false;
    }
  }
  if (inCrossing && n > 0) out.push({ from: start, to: sampleTimeOf(t, n - 1) });
  return out;
}

/**
 * The RSSI sample nearest a source-clock time. For a dense trace (explicit, non-uniform `times`)
 * it scans for the closest instant; for the coarse uniform grid it indexes by `from`/`period`.
 */
export function rssiAt(t: CompetitorTrace, time: number): number {
  const n = t.samples.length;
  if (n === 0) return 0;
  if (t.times) {
    let best = 0;
    let bestDist = Infinity;
    for (let i = 0; i < n; i++) {
      const d = Math.abs(t.times[i] - time);
      if (d < bestDist) {
        bestDist = d;
        best = i;
      }
    }
    return t.samples[best];
  }
  const from = t.from ?? 0;
  const i = Math.min(n - 1, Math.max(0, Math.round((time - from) / (t.period_micros || 1))));
  return t.samples[i];
}

/** Map a mouse/pointer event to the plot's user-unit X (the SVG is stretched to its rendered box). */
export function pointerX(e: MouseEvent, svg: SVGSVGElement): number {
  const rect = svg.getBoundingClientRect();
  if (rect.width === 0) return PAD_L;
  return ((e.clientX - rect.left) / rect.width) * W;
}

/** Invert {@link yOf}: a pointer event's Y back to an RSSI level (rounded — RSSI is integral). */
export function valueFromPointer(e: PointerEvent, range: Range): number {
  const svg = (e.currentTarget as Element).closest('svg');
  if (!svg) return range.lo;
  const rect = svg.getBoundingClientRect();
  const y = rect.height === 0 ? PAD_T : ((e.clientY - rect.top) / rect.height) * H;
  const frac = Math.min(1, Math.max(0, (PAD_T + plotH - y) / plotH));
  return Math.round(range.lo + frac * (range.hi - range.lo));
}
