import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/dom';
import type { CompetitorTrace } from '@gridfpv/types';
import RssiGraph from '../src/lib/RssiGraph.svelte';
import { lapList } from './fixtures.js';

/**
 * RssiGraph **live mode** (#355, slice 1) — the tuning view.
 *
 * Driven entirely by fixture buffers, not a socket: this slice owns the component, and the
 * telemetry pipeline that fills the buffer is slice 2's. What is proved here is what the component
 * must do once something *is* feeding it:
 *
 *  • a **rolling window** — the axis is the last `windowMicros` ending at the newest sample, not
 *    whatever the buffer happens to span, so the trace scrolls leftwards at a steady rate;
 *  • the **crossing band** — opens when the signal rises past `enter`, runs to *now* while the
 *    craft is still in the gate, and closes when it falls back past `exit`. This is the thing an
 *    RD is actually reading while tuning: it answers "do my thresholds bracket the pass?", which
 *    a bare RSSI number cannot;
 *  • **draggable thresholds**, the same handles marshaling uses;
 *  • and none of the review furniture — no laps, no add-lap, no zoom.
 */

// Plot geometry, mirroring the component's (src/lib/rssiGraph.ts).
const W = 1000;
const PAD_L = 8;
const PLOT_W = W - PAD_L - PAD_L; // 984
const PAD_T = 10;
const PLOT_H = 220 - PAD_T - 18; // 192

/** 5 Hz — the cadence RotorHazard's heartbeat lands at. */
const PERIOD = 200_000;
/** The default rolling window used across these tests. */
const WINDOW = 10_000_000;

const ENTER = 100;
const EXIT = 60;
const FLOOR = 50;

/**
 * A live node buffer: `n` samples at 5 Hz from t=0, floor RSSI, with an optional gate pass
 * written over it. The competitor key is a NODE seat (`node-0`), which is what live mode plots.
 */
function nodeBuffer(samples: number[]): CompetitorTrace {
  return {
    competitor: { adapter: 'rh-1', competitor: 'node-0' },
    from: 0,
    period_micros: PERIOD,
    samples,
    enter: ENTER,
    exit: EXIT
  };
}

/** A floor-level buffer of `n` samples. */
function floor(n: number): number[] {
  return Array.from({ length: n }, () => FLOOR);
}

/**
 * A complete gate pass written into a floor buffer at sample `i`: rise, two samples above
 * `enter`, then fall back below `exit`. Crossing opens at `i+1` and closes at `i+4`.
 */
function withPass(samples: number[], i: number): number[] {
  const out = [...samples];
  out[i] = 90;
  out[i + 1] = 130;
  out[i + 2] = 130;
  out[i + 3] = 90;
  out[i + 4] = 55;
  return out;
}

function renderLive(
  trace: CompetitorTrace,
  props: Record<string, unknown> = {}
): { unmount: () => void } {
  const { unmount } = render(RssiGraph, {
    trace: { competitors: [trace] },
    mode: 'live',
    windowMicros: WINDOW,
    nameFor: (ref: string) => (ref === 'node-0' ? 'Node 1 — Maverick' : ref),
    ...props
  });
  return { unmount };
}

/** The one live plot on screen. */
function plot(): SVGSVGElement {
  return screen.getByLabelText(/Live RSSI for/) as unknown as SVGSVGElement;
}

/** The x a source-clock time projects to, given the window ending at `end`. */
function xForTime(time: number, end: number): number {
  return PAD_L + ((time - (end - WINDOW)) / WINDOW) * PLOT_W;
}

/**
 * The RAW sample polyline's `points`, whichever way the graph is currently drawn (#473).
 *
 * Smoothing is a rendering choice on top of one shared point list, so the raw polyline is always
 * present: it is `.signal` when smoothing is off and the ghosted `.signal-raw` under the curve when
 * it is on, with identical `points` either way. These tests are about the AXIS — where a sample
 * projects — so they read the raw trace and stay true whichever default the component ships.
 */
function rawPoints(svg: Element): string {
  const poly = svg.querySelector('polyline.signal, polyline.signal-raw');
  if (!poly) throw new Error('no raw sample polyline rendered');
  return poly.getAttribute('points')!;
}

/** Pin the SVG's client box so clientX/clientY map 1:1 onto the viewBox. */
function pinSvgBox(svg: Element): void {
  vi.spyOn(svg as SVGElement, 'getBoundingClientRect').mockReturnValue({
    left: 0,
    top: 0,
    right: W,
    bottom: 220,
    width: W,
    height: 220,
    x: 0,
    y: 0,
    toJSON: () => ({})
  } as DOMRect);
}

describe('RssiGraph live mode — the rolling window', () => {
  it('draws the last `windowMicros`, pinned to the newest sample — not the whole buffer', () => {
    // 100 samples at 5 Hz = a 19.8s buffer, shown through a 10s window. If the axis fitted the
    // buffer (the review behaviour) the oldest sample would sit at the left edge; rolling means it
    // is off the left of the plot entirely, and the NEWEST sample is flush to the right edge.
    renderLive(nodeBuffer(floor(100)));
    const svg = plot();
    const pts = rawPoints(svg)
      .trim()
      .split(' ')
      .map((p) => parseFloat(p.split(',')[0]));

    expect(pts[pts.length - 1]).toBeCloseTo(PAD_L + PLOT_W, 1); // newest sample at "now"
    expect(pts[0]).toBeLessThanOrEqual(PAD_L); // the window's leading edge, clipped
    // t=0 (19.8s old) would project far off the left of a 10s window.
    expect(xForTime(0, 99 * PERIOD)).toBeLessThan(-800);
  });

  it('slides: appending samples moves an existing crossing leftwards by the elapsed time', () => {
    // The pass sits at samples 60..64 → 12.0…12.8s. Same buffer, then five more samples (1.0s)
    // arrive: the window's leading edge advances 1.0s, so the band must move 1.0s LEFT — a tenth
    // of a 10s window, 98.4 user units.
    const before = withPass(floor(100), 60);
    const first = renderLive(nodeBuffer(before));
    const x1 = parseFloat(plot().querySelector('.crossing')!.getAttribute('x')!);
    first.unmount();

    renderLive(nodeBuffer([...before, ...floor(5)]));
    const x2 = parseFloat(plot().querySelector('.crossing')!.getAttribute('x')!);

    expect(x1 - x2).toBeCloseTo((1_000_000 / WINDOW) * PLOT_W, 1);
  });

  it('labels the plot and the trace by the resolved node name, never the raw seat', () => {
    renderLive(nodeBuffer(floor(20)));
    const graph = screen.getByLabelText('RSSI signal graph');
    expect(within(graph).getByText('Node 1 — Maverick')).toBeInTheDocument();
    expect(within(graph).queryByText('node-0')).toBeNull();
    expect(within(graph).getByLabelText('Live RSSI for Node 1 — Maverick')).toBeInTheDocument();
  });

  it('reads out the cursor time as seconds behind now', async () => {
    renderLive(nodeBuffer(floor(100)));
    const svg = plot();
    pinSvgBox(svg);
    const end = 99 * PERIOD;
    await fireEvent.mouseMove(svg, { clientX: xForTime(end - 4_000_000, end) });
    const readout = svg.querySelector('[data-testid="rssi-readout"]')!;
    expect(readout.querySelector('.readout-time')!.textContent).toContain('-4.0');
    expect(readout.querySelector('.readout-rssi')!.textContent).toContain(String(FLOOR));
  });
});

describe('RssiGraph live mode — the crossing band', () => {
  it('opens at the enter crossing and closes at the exit crossing', () => {
    // Pass at samples 60..64: rises to/above enter=100 at 61 (12.2s), falls to/below exit=60 at
    // 64 (12.8s). One band, spanning exactly those two instants on the rolling axis.
    renderLive(nodeBuffer(withPass(floor(100), 60)));
    const svg = plot();
    const bands = svg.querySelectorAll('.crossing');
    expect(bands).toHaveLength(1);

    const end = 99 * PERIOD;
    expect(parseFloat(bands[0].getAttribute('x')!)).toBeCloseTo(xForTime(61 * PERIOD, end), 1);
    const width = parseFloat(bands[0].getAttribute('width')!);
    expect(width).toBeCloseTo(xForTime(64 * PERIOD, end) - xForTime(61 * PERIOD, end), 1);
  });

  it('runs the still-open band all the way to now while the craft is still in the gate', () => {
    // The signal has crossed enter and has NOT come back down — mid-pass, which is exactly when
    // an RD is watching. The band opens and stays open, reaching the right edge ("now").
    const samples = floor(60).concat([90, 130, 130, 130]);
    renderLive(nodeBuffer(samples));
    const svg = plot();
    const band = svg.querySelector('.crossing')!;
    const end = (samples.length - 1) * PERIOD;
    expect(parseFloat(band.getAttribute('x')!)).toBeCloseTo(xForTime(61 * PERIOD, end), 1);
    const right = parseFloat(band.getAttribute('x')!) + parseFloat(band.getAttribute('width')!);
    expect(right).toBeCloseTo(PAD_L + PLOT_W, 1); // still open → runs to "now"
  });

  it('closes that band the moment the signal falls back past exit', () => {
    const open = floor(60).concat([90, 130, 130, 130]);
    renderLive(nodeBuffer(open.concat([90, 55, 50, 50])));
    const svg = plot();
    const band = svg.querySelector('.crossing')!;
    const right = parseFloat(band.getAttribute('x')!) + parseFloat(band.getAttribute('width')!);
    // Closed at sample 65 (the first ≤ exit), which is 3 samples = 0.6s before the newest.
    const end = 67 * PERIOD;
    expect(right).toBeCloseTo(xForTime(65 * PERIOD, end), 1);
    expect(right).toBeLessThan(PAD_L + PLOT_W - 1);
  });

  it('re-bands live as the operator drags: `tuned` levels re-run the same hysteresis', () => {
    // Two passes in the buffer, one strong and one weak. At the recorded levels only the strong
    // one bands; drop enter to 70 with `tuned` and the weak one bands too — the operator seeing,
    // live, that their new threshold would now catch it.
    const samples = withPass(floor(100), 20);
    samples[60] = 65;
    samples[61] = 80;
    samples[62] = 80;
    samples[63] = 65;
    samples[64] = 50;
    const { unmount } = render(RssiGraph, {
      trace: { competitors: [nodeBuffer(samples)] },
      mode: 'live',
      windowMicros: 25_000_000,
      onthresholds: () => {}
    });
    expect(screen.getByLabelText(/Live RSSI for/).querySelectorAll('.crossing')).toHaveLength(1);
    unmount();

    render(RssiGraph, {
      trace: { competitors: [nodeBuffer(samples)] },
      mode: 'live',
      windowMicros: 25_000_000,
      onthresholds: () => {},
      tuned: { competitor: 'node-0', enter: 70, exit: 60 }
    });
    expect(screen.getByLabelText(/Live RSSI for/).querySelectorAll('.crossing')).toHaveLength(2);
  });
});

describe('RssiGraph live mode — draggable thresholds', () => {
  // The live buffer's value range: min 50 / max 130 over the samples, widened by the thresholds
  // and padded 8% each side, exactly as review's is.
  const LO = 50 - (130 - 50) * 0.08;
  const HI = 130 + (130 - 50) * 0.08;
  const yForValue = (v: number) => PAD_T + PLOT_H - ((v - LO) / (HI - LO)) * PLOT_H;
  const firePointer = (el: Element, type: string, init: MouseEventInit) =>
    fireEvent(el, new MouseEvent(type, { bubbles: true, ...init }));

  it('drags the enter handle and emits the tuned pair for the node', async () => {
    const onthresholds = vi.fn();
    renderLive(nodeBuffer(withPass(floor(100), 60)), { onthresholds });
    const svg = plot();
    pinSvgBox(svg);

    const handle = screen.getByRole('slider', { name: 'Enter threshold for Node 1 — Maverick' });
    expect(handle).toHaveAttribute('aria-valuenow', String(ENTER));
    await firePointer(handle, 'pointerdown', { clientY: yForValue(ENTER) });
    await firePointer(handle, 'pointermove', { clientY: yForValue(112) });
    expect(onthresholds).toHaveBeenLastCalledWith('node-0', 112, EXIT);

    await firePointer(handle, 'pointerup', {});
    onthresholds.mockClear();
    await firePointer(handle, 'pointermove', { clientY: yForValue(80) });
    expect(onthresholds).not.toHaveBeenCalled();
  });

  it('nudges either handle from the keyboard', async () => {
    const onthresholds = vi.fn();
    renderLive(nodeBuffer(withPass(floor(100), 60)), { onthresholds });
    await fireEvent.keyDown(
      screen.getByRole('slider', { name: 'Enter threshold for Node 1 — Maverick' }),
      { key: 'ArrowUp' }
    );
    expect(onthresholds).toHaveBeenLastCalledWith('node-0', ENTER + 1, EXIT);
    await fireEvent.keyDown(
      screen.getByRole('slider', { name: 'Exit threshold for Node 1 — Maverick' }),
      { key: 'ArrowDown' }
    );
    expect(onthresholds).toHaveBeenLastCalledWith('node-0', ENTER, EXIT - 1);
  });

  it('stays display-only without `onthresholds`, exactly as review does', () => {
    renderLive(nodeBuffer(floor(20)));
    const svg = plot();
    expect(svg.querySelector('.th-handle')).toBeNull();
    expect(svg.querySelector('.enter-line')).not.toBeNull();
    expect(svg.querySelector('.exit-line')).not.toBeNull();
  });
});

describe('RssiGraph live mode — none of the review furniture', () => {
  it('draws no lap markers, offers no add-lap, and shows no zoom controls', async () => {
    const onaddlap = vi.fn();
    const onselect = vi.fn();
    // Even handed review-only props, live mode must ignore them — the mode decides, not the caller.
    renderLive(nodeBuffer(floor(100)), {
      laps: lapList,
      onselect,
      onaddlap,
      canControl: true,
      preview: { competitor: 'node-0', added: [1_000_000], removedRefs: [12] }
    });
    const svg = plot();
    expect(svg.querySelectorAll('.marker')).toHaveLength(0);
    expect(svg.querySelectorAll('.preview-added')).toHaveLength(0);
    expect(screen.queryByRole('button', { name: 'Zoom in' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Reset zoom' })).toBeNull();

    pinSvgBox(svg);
    await fireEvent.mouseMove(svg, { clientX: 400 });
    expect(svg.querySelector('[data-testid="rssi-readout"]')).not.toBeNull(); // the readout stays
    expect(screen.queryByRole('button', { name: /Add lap/ })).toBeNull();
    expect(onaddlap).not.toHaveBeenCalled();
    expect(onselect).not.toHaveBeenCalled();
  });

  it('does not zoom or pan on a wheel — the rolling window owns the axis', async () => {
    renderLive(nodeBuffer(floor(100)));
    const svg = plot();
    pinSvgBox(svg);
    const before = rawPoints(svg);
    await fireEvent.wheel(svg, { deltaY: -120, clientX: 500 });
    expect(rawPoints(svg)).toBe(before);
  });

  it('says it is waiting rather than "no samples captured" when a node has sent nothing', () => {
    renderLive(nodeBuffer([]));
    expect(screen.queryByLabelText(/Live RSSI for/)).toBeNull(); // no plot at all
    expect(screen.getByText('No signal from this node yet.')).toBeInTheDocument();
    expect(screen.queryByText(/No samples captured/)).toBeNull();
  });

  it('legends the live source and drops the lap swatch', () => {
    renderLive(nodeBuffer(floor(20)));
    const graph = screen.getByLabelText('RSSI signal graph');
    expect(within(graph).getByText(/Signal \(live\)/)).toBeInTheDocument();
    expect(within(graph).getByText(/rolling window of the timer's heartbeat/)).toBeInTheDocument();
    expect(within(graph).queryByText('Lap pass')).toBeNull();
    // The detection-window swatch stays — it is the same band, in the same visual language.
    expect(within(graph).getByText(/Detection window/)).toBeInTheDocument();
  });
});
