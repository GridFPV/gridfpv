import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/dom';
import {
  monotoneTangents,
  pointsAttr,
  samplePoints,
  smoothPath,
  type Point
} from '../src/lib/rssiGraph.js';
import RssiGraph from '../src/lib/RssiGraph.svelte';
import type { CompetitorTrace } from '@gridfpv/types';

/**
 * RSSI graph **visual smoothing** (#473) — the interpolation math.
 *
 * The feature's whole constraint is that smoothing may change the drawing and nothing else: it
 * never touches the stored samples, and — the part these tests exist for — *it must not move where
 * a pass peak sits*. A marshal reads crossing edges off this plot, so a curve that lags the signal
 * (an EMA, a leading-edge tween) or one that rings past a peak (an unconstrained spline) would
 * relocate the apparent instant of a gate pass. Monotone cubic Hermite interpolation is what makes
 * both impossible, and these tests pin exactly that:
 *
 *  1. it INTERPOLATES — the curve passes through every sample, so the peak stays at the peak's x;
 *  2. it never OVERSHOOTS — no segment leaves the box of its two endpoints, so no crossing appears
 *     that the raw samples never made.
 *
 * A test that passes for the wrong reason is worse than none (CLAUDE.md), so these evaluate the
 * actual cubic rather than eyeballing the path string.
 */

/** Evaluate the cubic Hermite segment `i` at local parameter `t ∈ [0,1]` — the drawn curve. */
function hermite(xs: number[], ys: number[], m: number[], i: number, t: number): number {
  const h = xs[i + 1] - xs[i];
  const t2 = t * t;
  const t3 = t2 * t;
  const h00 = 2 * t3 - 3 * t2 + 1;
  const h10 = t3 - 2 * t2 + t;
  const h01 = -2 * t3 + 3 * t2;
  const h11 = t3 - t2;
  return h00 * ys[i] + h10 * h * m[i] + h01 * ys[i + 1] + h11 * h * m[i + 1];
}

/** Sample the whole interpolant densely: `[x, y]` pairs across every segment. */
function densify(xs: number[], ys: number[], steps = 40): [number, number][] {
  const m = monotoneTangents(xs, ys);
  const out: [number, number][] = [];
  for (let i = 0; i < xs.length - 1; i++) {
    for (let s = 0; s <= steps; s++) {
      const t = s / steps;
      out.push([xs[i] + t * (xs[i + 1] - xs[i]), hermite(xs, ys, m, i, t)]);
    }
  }
  return out;
}

/** A trace on the uniform grid (no explicit `times`), which is the streaming-cadence shape. */
function traceOf(samples: number[], periodMicros = 1_000_000): CompetitorTrace {
  return {
    competitor: { adapter: 'rh-1', competitor: 'node-0' },
    from: 0,
    period_micros: periodMicros,
    samples
  };
}

describe('monotoneTangents — the Fritsch–Carlson limiter', () => {
  it('interpolates: the curve passes exactly through every sample', () => {
    const xs = [0, 10, 20, 30, 40];
    const ys = [50, 55, 120, 60, 52];
    const m = monotoneTangents(xs, ys);
    for (let i = 0; i < xs.length - 1; i++) {
      expect(hermite(xs, ys, m, i, 0)).toBeCloseTo(ys[i], 10);
      expect(hermite(xs, ys, m, i, 1)).toBeCloseTo(ys[i + 1], 10);
    }
  });

  it('never overshoots: every segment stays inside the box of its two endpoints', () => {
    // A sharp isolated peak — the shape a gate pass makes, and the one a plain Catmull-Rom or
    // natural cubic rings past on both sides.
    const xs = [0, 10, 20, 30, 40, 50];
    const ys = [50, 51, 200, 52, 50, 50];
    const m = monotoneTangents(xs, ys);
    for (let i = 0; i < xs.length - 1; i++) {
      const lo = Math.min(ys[i], ys[i + 1]);
      const hi = Math.max(ys[i], ys[i + 1]);
      for (let s = 0; s <= 40; s++) {
        const y = hermite(xs, ys, m, i, s / 40);
        expect(y).toBeGreaterThanOrEqual(lo - 1e-9);
        expect(y).toBeLessThanOrEqual(hi + 1e-9);
      }
    }
  });

  it('puts a flat tangent at a local maximum, so the peak sits ON its sample', () => {
    // THE crossing-honesty property. The drawn maximum must be the sample's own value, at the
    // sample's own x — not a hair later and higher because the spline carried momentum through it.
    const xs = [0, 10, 20, 30, 40];
    const ys = [50, 60, 180, 60, 50];
    const m = monotoneTangents(xs, ys);
    expect(m[2]).toBe(0);

    const dense = densify(xs, ys);
    let best = dense[0];
    for (const p of dense) if (p[1] > best[1]) best = p;
    expect(best[1]).toBeCloseTo(180, 6); // the peak VALUE is the sample's, not higher
    expect(best[0]).toBeCloseTo(20, 6); // at the sample's own x — the peak did not move
  });

  it('a smoothed pass crosses a threshold at the same samples the raw trace does', () => {
    // The detector's view must survive smoothing: between two samples that both sit below `enter`,
    // the curve must not rise above it and draw a crossing that never happened.
    const xs = [0, 10, 20, 30, 40, 50, 60];
    const ys = [50, 52, 98, 99, 97, 51, 50];
    const ENTER = 100;
    for (const [, y] of densify(xs, ys)) expect(y).toBeLessThan(ENTER);
  });

  it('is exactly linear through collinear samples (smoothing adds no wobble)', () => {
    const xs = [0, 10, 20, 30];
    const ys = [10, 20, 30, 40];
    for (const [x, y] of densify(xs, ys)) expect(y).toBeCloseTo(x + 10, 9);
  });

  it('flattens a plateau rather than dipping between equal samples', () => {
    const xs = [0, 10, 20, 30];
    const ys = [50, 90, 90, 50];
    const m = monotoneTangents(xs, ys);
    expect(m[1]).toBe(0);
    expect(m[2]).toBe(0);
    for (const [, y] of densify(xs, ys)) {
      expect(y).toBeGreaterThanOrEqual(50 - 1e-9);
      expect(y).toBeLessThanOrEqual(90 + 1e-9);
    }
  });

  it('handles degenerate inputs without dividing by zero', () => {
    expect(monotoneTangents([], [])).toEqual([]);
    expect(monotoneTangents([5], [7])).toEqual([0]);
    // Two samples stamped at the same instant: no slope to take, and no NaN either.
    for (const t of monotoneTangents([0, 0, 10], [1, 2, 3])) expect(Number.isFinite(t)).toBe(true);
  });
});

describe('smoothPath — the SVG the curve is drawn as', () => {
  const pts = (xy: [number, number][]): Point[] => xy.map(([x, y]) => ({ x, y }));

  it('is empty for no points and a bare move for one', () => {
    expect(smoothPath([])).toBe('');
    expect(smoothPath(pts([[3, 4]]))).toBe('M 3.0,4.0');
  });

  it('starts at the first sample and ends at the last', () => {
    const d = smoothPath(
      pts([
        [0, 100],
        [10, 50],
        [20, 80]
      ])
    );
    expect(d.startsWith('M 0.0,100.0')).toBe(true);
    expect(d.endsWith('20.0,80.0')).toBe(true);
  });

  it('emits one cubic per segment, and the Bézier controls sit a third of the way in', () => {
    // Hermite → Bézier is exact only if the control x's are at h/3; pin that, because a wrong
    // control x would draw a different curve from the one the tangents describe.
    const d = smoothPath(
      pts([
        [0, 10],
        [30, 40]
      ])
    );
    expect(d).toBe('M 0.0,10.0 C 10.0,20.0 20.0,30.0 30.0,40.0');
  });

  it('degrades a zero-width segment to a line instead of an undrawable curve', () => {
    const d = smoothPath(
      pts([
        [0, 10],
        [0, 20],
        [10, 30]
      ])
    );
    expect(d).toContain('L 0.0,20.0');
    expect(d).not.toContain('NaN');
  });

  it('never emits NaN for a real trace', () => {
    const t = traceOf([50, 51, 120, 180, 130, 60, 50, 50]);
    const d = smoothPath(samplePoints(t, { from: 0, to: 7_000_000 }, { lo: 40, hi: 200 }));
    expect(d).not.toContain('NaN');
  });
});

describe('samplePoints / pointsAttr — one geometry, two renderings', () => {
  it('gives the smooth curve and the raw polyline the SAME points', () => {
    // The honesty guarantee at the component level: the ghosted raw trace and the curve are drawn
    // from one point list, so they cannot disagree about where a sample is.
    const t = traceOf([50, 60, 140, 70, 55]);
    const span = { from: 0, to: 4_000_000 };
    const range = { lo: 40, hi: 150 };
    const points = samplePoints(t, span, range);
    const attr = pointsAttr(points);

    // Every vertex of the polyline is the start/end of a curve segment.
    const d = smoothPath(points);
    const first = attr.split(' ')[0];
    expect(d.startsWith(`M ${first}`)).toBe(true);
    for (const vertex of attr.split(' ')) expect(d).toContain(vertex);
  });

  it('places a sample at the same x whether or not it is smoothed', () => {
    const t = traceOf([50, 200, 50]);
    const span = { from: 0, to: 2_000_000 };
    const range = { lo: 40, hi: 210 };
    const points = samplePoints(t, span, range);
    // The peak sample's projected point is a literal vertex of the path — it cannot have slid.
    const peak = points[1];
    expect(smoothPath(points)).toContain(`${peak.x.toFixed(1)},${peak.y.toFixed(1)}`);
  });

  it('returns nothing for an empty trace', () => {
    expect(samplePoints(traceOf([]), { from: 0, to: 1 }, { lo: 0, hi: 1 })).toEqual([]);
    expect(pointsAttr([])).toBe('');
  });
});

describe('RssiGraph — the smoothing toggle', () => {
  const trace = {
    competitors: [
      {
        competitor: { adapter: 'rh-1', competitor: 'node-0' },
        from: 0,
        period_micros: 200_000,
        samples: [50, 55, 120, 180, 130, 60, 50],
        enter: 100,
        exit: 60
      }
    ]
  };
  const toggle = () => screen.getByRole('button', { name: /Smoothing:/ });
  const plot = () => document.querySelector('svg.plot')!;

  it('review starts on the RAW samples — marshaling evidence loads unsmoothed', () => {
    render(RssiGraph, { trace, mode: 'review' });
    expect(toggle()).toHaveAttribute('aria-pressed', 'false');
    expect(document.querySelector('polyline.signal')).not.toBeNull();
    expect(document.querySelector('path.signal')).toBeNull();
    expect(document.querySelector('polyline.signal-raw')).toBeNull();
  });

  it('live starts smoothed — that is the trace whose movement the issue is about', () => {
    render(RssiGraph, { trace, mode: 'live' });
    expect(toggle()).toHaveAttribute('aria-pressed', 'true');
    expect(document.querySelector('path.signal')).not.toBeNull();
  });

  it('keeps the raw trace on screen underneath whenever smoothing is on', () => {
    // The honesty requirement: smoothing may never HIDE the recorded samples.
    render(RssiGraph, { trace, mode: 'live' });
    const raw = document.querySelector('polyline.signal-raw');
    expect(raw).not.toBeNull();
    // …and it is the same geometry the curve is built from.
    const d = document.querySelector('path.signal')!.getAttribute('d')!;
    for (const vertex of raw!.getAttribute('points')!.split(' ')) expect(d).toContain(vertex);
  });

  it('toggles both ways, and the operator’s choice overrides the mode default', async () => {
    render(RssiGraph, { trace, mode: 'review' });
    await fireEvent.click(toggle());
    expect(toggle()).toHaveAttribute('aria-pressed', 'true');
    expect(document.querySelector('path.signal')).not.toBeNull();
    expect(document.querySelector('polyline.signal-raw')).not.toBeNull();

    await fireEvent.click(toggle());
    expect(toggle()).toHaveAttribute('aria-pressed', 'false');
    expect(document.querySelector('path.signal')).toBeNull();
    expect(document.querySelector('polyline.signal')).not.toBeNull();
  });

  it('does not move the crossing bands when smoothing is turned on', async () => {
    // The core guarantee, at the component level: smoothing is render-side ONLY. The detection
    // windows come from the STORED samples via `crossingWindows`, so toggling the curve on must
    // leave them pixel-identical — same mode, same axis, only the drawing changes.
    render(RssiGraph, { trace, mode: 'review' });
    const bandGeometry = () =>
      [...plot().querySelectorAll('.crossing')].map(
        (r) => `${r.getAttribute('x')}/${r.getAttribute('width')}`
      );

    const raw = bandGeometry();
    expect(raw.length).toBeGreaterThan(0);

    await fireEvent.click(toggle()); // smoothing ON
    expect(document.querySelector('path.signal')).not.toBeNull();
    expect(bandGeometry()).toEqual(raw);
  });
});
