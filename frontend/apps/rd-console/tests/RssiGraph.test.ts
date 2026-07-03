import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/dom';
import type { CompetitorRef, LapList, SignalTraceView } from '@gridfpv/types';
import RssiGraph from '../src/lib/RssiGraph.svelte';
import { signalTrace, lapList } from './fixtures.js';

// The graph's px→source-time mapping is pure geometry: W=1000, PAD_L=8 → plotW=984, and the trace
// spans `from`=0 … `to`=90_000_000 (91 samples at 1s). jsdom gives an SVG a 0-size client rect, so
// the component reads the pointer X from `clientX` against the element's box — here we pin that box
// to {left:0, width:1000} so a `clientX` maps 1:1 onto the viewBox X. With that, a click/hover at a
// viewBox X of `xv` resolves to a source time of `((xv - 8) / 984) * 90_000_000`.
const W = 1000;
const PAD_L = 8;
const PLOT_W = W - PAD_L - PAD_L; // 984
const SPAN = 90_000_000;
/** The viewBox X that the mapping turns into the given source-clock time (µs). */
function xForTime(timeMicros: number): number {
  return PAD_L + (timeMicros / SPAN) * PLOT_W;
}
/** Pin the SVG's client box to {left:0, width:1000} so clientX maps 1:1 onto the viewBox X. */
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

/**
 * RssiGraph trace label (the raw-id leak fix): the per-competitor trace is labelled with the
 * competitor ref, which is a raw id ("ALICE", a pilot id, …). The graph takes a `nameFor` resolver
 * so the figure caption + the human-facing aria-labels read as the resolved callsign, matching the
 * lap-list headings the marshal sees next to it. The default is identity so existing callers/tests
 * that don't pass a resolver keep rendering the ref unchanged.
 */
describe('RssiGraph trace labels', () => {
  it('labels the trace with the resolved name when a nameFor resolver is passed', () => {
    const nameFor = (ref: CompetitorRef) => (ref === 'ALICE' ? 'Maverick' : ref);
    render(RssiGraph, {
      trace: signalTrace,
      laps: lapList,
      selected: null,
      onselect: () => {},
      nameFor
    });

    const graph = screen.getByLabelText('RSSI signal graph');
    // The figcaption `.who` label shows the callsign, never the raw ref.
    expect(within(graph).getByText('Maverick')).toBeInTheDocument();
    expect(within(graph).queryByText('ALICE')).toBeNull();
    // The human-facing aria-labels (figure + svg) read as the callsign too.
    expect(within(graph).getByLabelText('RSSI for Maverick')).toBeInTheDocument();
    expect(within(graph).getByLabelText(/RSSI trace for Maverick/)).toBeInTheDocument();
  });

  it('falls back to the raw ref via the identity default when no resolver is given', () => {
    render(RssiGraph, {
      trace: signalTrace,
      laps: lapList,
      selected: null,
      onselect: () => {}
    });

    const graph = screen.getByLabelText('RSSI signal graph');
    // Identity default: the label stays the raw ref (no resolver supplied).
    expect(within(graph).getByText('ALICE')).toBeInTheDocument();
    expect(within(graph).getByLabelText('RSSI for ALICE')).toBeInTheDocument();
  });
});

/**
 * Threshold band + complete lap coverage (the pre-recalc legibility pass): the enter/exit levels are
 * shaded as an area (not just lines), and EVERY current lap gets a marker — even one whose pass time
 * falls outside the captured sample window (the dense trace can be shorter than the race).
 */
describe('RssiGraph thresholds + lap coverage', () => {
  it('shades a detection window per crossing (enter→exit) the engine saw', () => {
    render(RssiGraph, { trace: signalTrace, laps: lapList, selected: null, onselect: () => {} });
    const svg = screen.getByLabelText(/RSSI trace for ALICE/);
    // The fixture has two peaks crossing enter=110 then dropping below exit=95 → two windows.
    expect(svg.querySelectorAll('.crossing')).toHaveLength(2);
  });

  it('plots dense samples at their actual (non-uniform) times, not a compressed uniform grid', () => {
    // The bug: RH's dense history is bursty, so `period_micros` (first delta, 15ms here) hugely
    // understates the span. With explicit `times` the trace must span its REAL duration (0..20s) so
    // the signal lines up with the laps instead of compressing into the left edge.
    const trace: SignalTraceView = {
      competitors: [
        {
          competitor: { adapter: 'rh-1', competitor: 'ALICE' },
          from: 0,
          period_micros: 15000,
          samples: [70, 120, 80, 120, 80],
          times: [0, 100_000, 5_000_000, 10_000_000, 20_000_000],
          enter: 110,
          exit: 95
        }
      ]
    };
    const laps: LapList = {
      competitors: [
        {
          competitor: { adapter: 'rh-1', competitor: 'ALICE' },
          laps: [
            { number: 1, duration_micros: 20_000_000, at: 20_000_000, start_ref: 1, end_ref: 2 }
          ]
        }
      ]
    };
    render(RssiGraph, { trace, laps, selected: null, onselect: () => {} });
    const svg = screen.getByLabelText(/RSSI trace for ALICE/);
    const poly = svg.querySelector('polyline.signal')!;
    const pts = poly.getAttribute('points')!.trim().split(' ');
    const lastX = parseFloat(pts[pts.length - 1].split(',')[0]);
    // The last sample (t=20s) reaches the right edge (~PAD_L+plotW≈992), not crammed to the left.
    expect(lastX).toBeGreaterThan(900);
  });

  it('renders a marker for every lap, including one past the captured sample window', () => {
    // ALICE's samples span 0…90s; add a lap at 200s. The plot must widen to include it so its
    // marker still shows rather than rendering off-frame.
    const farLaps = {
      ...lapList,
      competitors: [
        {
          ...lapList.competitors[0],
          laps: [
            ...lapList.competitors[0].laps,
            { number: 9, duration_micros: 1_000_000, at: 200_000_000, start_ref: 99, end_ref: 100 }
          ]
        },
        ...lapList.competitors.slice(1)
      ]
    };
    render(RssiGraph, { trace: signalTrace, laps: farLaps, selected: null, onselect: () => {} });
    const graph = screen.getByLabelText('RSSI signal graph');
    expect(within(graph).getByRole('button', { name: /Lap 1 at/ })).toBeInTheDocument();
    expect(within(graph).getByRole('button', { name: /Lap 9 at/ })).toBeInTheDocument();
  });
});

/**
 * Hover crosshair + time/RSSI readout (gap 1): moving over a trace must report the EXACT
 * race-relative time + RSSI at the cursor, using the same px→source-time mapping the lap markers
 * use. The fixture's ALICE trace samples `i` are at `from + i·period` (1s cadence), so the RSSI at
 * a known X is the sample nearest that time.
 */
describe('RssiGraph hover readout', () => {
  it('reports the time + RSSI at the cursor for a known X', async () => {
    render(RssiGraph, { trace: signalTrace, laps: lapList, selected: null, onselect: () => {} });
    const svg = screen.getByLabelText(/RSSI trace for ALICE/);
    pinSvgBox(svg);

    // Hover at the X that maps to 41.0s (ALICE's first gate pass / a signal peak).
    await fireEvent.mouseMove(svg, { clientX: xForTime(41_000_000) });

    const readout = svg.querySelector('[data-testid="rssi-readout"]')!;
    expect(readout).not.toBeNull();
    // The time reads as race-relative seconds (S.mmm) — 41.000s here.
    expect(readout.querySelector('.readout-time')!.textContent).toContain('41.000');
    // The RSSI is the sample nearest 41s. fixture: 70 + near(41)=60 + near(81)=0 + (41%3=2) = 132.
    expect(readout.querySelector('.readout-rssi')!.textContent).toContain('132');
  });

  it('clears the readout on pointer leave', async () => {
    render(RssiGraph, { trace: signalTrace, laps: lapList, selected: null, onselect: () => {} });
    const svg = screen.getByLabelText(/RSSI trace for ALICE/);
    pinSvgBox(svg);
    await fireEvent.mouseMove(svg, { clientX: xForTime(10_000_000) });
    expect(svg.querySelector('[data-testid="rssi-readout"]')).not.toBeNull();
    await fireEvent.mouseLeave(svg);
    expect(svg.querySelector('[data-testid="rssi-readout"]')).toBeNull();
  });
});

/**
 * Add-a-lap is EXPLICIT-ONLY: a bare click on the trace must never add a lap — the browser
 * synthesizes a click on the svg when a threshold drag ends inside it, and stray clicks land there
 * too; each used to plant a phantom "Lap inserted" ruling (live 2026-07-03). The only add path is
 * the labelled "Add lap here" button in the cursor readout below the plot, role-gated by
 * `canControl`.
 */
describe('RssiGraph add-lap', () => {
  it('a click on the trace sends NOTHING — the trace has no click-to-insert path', async () => {
    const onaddlap = vi.fn();
    render(RssiGraph, {
      trace: signalTrace,
      laps: lapList,
      selected: null,
      onselect: () => {},
      onaddlap,
      canControl: true
    });
    const svg = screen.getByLabelText(/RSSI trace for ALICE/);
    pinSvgBox(svg);

    // Click at the X mapping to 30.0s — the old click-to-insert misfire surface.
    await fireEvent.click(svg, { clientX: xForTime(30_000_000) });
    expect(onaddlap).not.toHaveBeenCalled();
  });

  it('the "Add lap here" button adds at the cursor time (the ONLY add path)', async () => {
    const onaddlap = vi.fn();
    render(RssiGraph, {
      trace: signalTrace,
      laps: lapList,
      selected: null,
      onselect: () => {},
      onaddlap,
      canControl: true
    });
    const svg = screen.getByLabelText(/RSSI trace for ALICE/);
    pinSvgBox(svg);
    await fireEvent.mouseMove(svg, { clientX: xForTime(50_000_000) });
    const addBtn = screen.getByRole('button', { name: /Add lap for ALICE at .* seconds/ });
    await fireEvent.click(addBtn);
    expect(onaddlap).toHaveBeenCalledTimes(1);
    expect(onaddlap.mock.calls[0][0]).toBe('ALICE');
    expect(onaddlap.mock.calls[0][1]).toBeGreaterThan(49_900_000);
    expect(onaddlap.mock.calls[0][1]).toBeLessThan(50_100_000);
  });

  it('clicking a lap marker still selects (not adds) — the add-lap handler does not fire', async () => {
    const onaddlap = vi.fn();
    const onselect = vi.fn();
    render(RssiGraph, {
      trace: signalTrace,
      laps: lapList,
      selected: null,
      onselect,
      onaddlap,
      canControl: true
    });
    const graph = screen.getByLabelText('RSSI signal graph');
    await fireEvent.click(within(graph).getByRole('button', { name: /Lap 1 at .* — select/ }));
    expect(onselect).toHaveBeenCalledTimes(1);
    expect(onaddlap).not.toHaveBeenCalled();
  });

  it('drag + preview props absent → display-only: no handles, no preview markers', () => {
    render(RssiGraph, { trace: signalTrace, laps: lapList, selected: null, onselect: () => {} });
    const svg = screen.getByLabelText(/RSSI trace for ALICE/);
    expect(svg.querySelector('.th-handle')).toBeNull();
    expect(svg.querySelector('.preview-added')).toBeNull();
    // The threshold lines themselves still draw (the recorded levels).
    expect(svg.querySelector('.enter-line')).not.toBeNull();
  });

  it('is review-only when canControl is false: no "Add lap here" affordance is offered', async () => {
    const onaddlap = vi.fn();
    render(RssiGraph, {
      trace: signalTrace,
      laps: lapList,
      selected: null,
      onselect: () => {},
      onaddlap,
      canControl: false
    });
    const svg = screen.getByLabelText(/RSSI trace for ALICE/);
    pinSvgBox(svg);
    await fireEvent.click(svg, { clientX: xForTime(30_000_000) });
    expect(onaddlap).not.toHaveBeenCalled();
    // Hovering shows the readout but never the add button.
    await fireEvent.mouseMove(svg, { clientX: xForTime(30_000_000) });
    expect(screen.queryByRole('button', { name: /Add lap for/ })).toBeNull();
  });
});

/**
 * Live threshold tuning (the RH-style "Recalculate", marshaling.html §5): with `onthresholds`
 * supplied, the enter/exit lines carry draggable + keyboard-nudgeable handles emitting the tuned
 * levels; `tuned` overrides the drawn levels; `preview` draws the uncommitted re-detection diff
 * (hollow dashed added-pass candidates, struck/dimmed would-be-removed lap markers). All
 * preview-only — the graph itself never re-detects or sends anything.
 */
describe('RssiGraph threshold tuning + preview', () => {
  // The value↔Y mapping mirrors the component's: the fixture's range is min/max over samples AND
  // the recorded thresholds, padded 8% on each side; Y spans PAD_T..PAD_T+plotH top-down.
  const PAD_T = 10;
  const PAD_B = 18;
  const PLOT_H = 220 - PAD_T - PAD_B; // 192
  const ct = signalTrace.competitors[0];
  const rawLo = Math.min(...ct.samples, ct.enter!, ct.exit!);
  const rawHi = Math.max(...ct.samples, ct.enter!, ct.exit!);
  const PAD = (rawHi - rawLo) * 0.08;
  const LO = rawLo - PAD;
  const HI = rawHi + PAD;
  /** The clientY (box pinned 1:1 to the viewBox) that maps back to the given RSSI value. */
  function yForValue(v: number): number {
    return PAD_T + PLOT_H - ((v - LO) / (HI - LO)) * PLOT_H;
  }
  /** jsdom has no PointerEvent — dispatch a MouseEvent with the pointer type (same fields). */
  function firePointer(el: Element, type: string, init: MouseEventInit): boolean {
    return fireEvent(el, new MouseEvent(type, { bubbles: true, ...init }));
  }

  it('dragging the enter handle emits onthresholds with the level at the pointer', async () => {
    const onthresholds = vi.fn();
    render(RssiGraph, {
      trace: signalTrace,
      laps: lapList,
      selected: null,
      onselect: () => {},
      canControl: true,
      onthresholds
    });
    const svg = screen.getByLabelText(/RSSI trace for ALICE/);
    pinSvgBox(svg);

    const handle = screen.getByRole('slider', { name: 'Enter threshold for ALICE' });
    await firePointer(handle, 'pointerdown', { clientY: yForValue(110) });
    await firePointer(handle, 'pointermove', { clientY: yForValue(120) });
    // The drag emits BOTH levels: the dragged enter at the pointer, the exit unchanged.
    expect(onthresholds).toHaveBeenLastCalledWith('ALICE', 120, 95);
    // After release, further moves emit nothing.
    await firePointer(handle, 'pointerup', {});
    onthresholds.mockClear();
    await firePointer(handle, 'pointermove', { clientY: yForValue(80) });
    expect(onthresholds).not.toHaveBeenCalled();
  });

  it('dragging the exit handle emits the tuned exit with the enter unchanged', async () => {
    const onthresholds = vi.fn();
    render(RssiGraph, {
      trace: signalTrace,
      laps: lapList,
      selected: null,
      onselect: () => {},
      canControl: true,
      onthresholds
    });
    const svg = screen.getByLabelText(/RSSI trace for ALICE/);
    pinSvgBox(svg);

    const handle = screen.getByRole('slider', { name: 'Exit threshold for ALICE' });
    await firePointer(handle, 'pointerdown', { clientY: yForValue(95) });
    await firePointer(handle, 'pointermove', { clientY: yForValue(85) });
    expect(onthresholds).toHaveBeenLastCalledWith('ALICE', 110, 85);
  });

  it('finishing a threshold DRAG never adds a lap (the browser-synthesized click is inert)', async () => {
    // The live bug (2026-07-03): pointerdown→pointerup on a handle inside the svg makes the
    // browser synthesize a CLICK on the svg — with the old click-to-insert path, EVERY threshold
    // drag planted a phantom "Lap inserted". The synthesized click must send nothing.
    const onaddlap = vi.fn();
    const onthresholds = vi.fn();
    render(RssiGraph, {
      trace: signalTrace,
      laps: lapList,
      selected: null,
      onselect: () => {},
      onaddlap,
      canControl: true,
      onthresholds
    });
    const svg = screen.getByLabelText(/RSSI trace for ALICE/);
    pinSvgBox(svg);

    // The full drag gesture on the enter handle…
    const handle = screen.getByRole('slider', { name: 'Enter threshold for ALICE' });
    await firePointer(handle, 'pointerdown', { clientY: yForValue(110) });
    await firePointer(handle, 'pointermove', { clientY: yForValue(120) });
    await firePointer(handle, 'pointerup', {});
    // …then the click the browser synthesizes on the svg where the pointer was released.
    await fireEvent.click(svg, { clientX: xForTime(45_000_000), clientY: yForValue(120) });

    // The drag tuned the thresholds — but NOTHING was inserted.
    expect(onthresholds).toHaveBeenLastCalledWith('ALICE', 120, 95);
    expect(onaddlap).not.toHaveBeenCalled();
  });

  it('arrow keys nudge the focused handle ±1 (keyboard access)', async () => {
    const onthresholds = vi.fn();
    render(RssiGraph, {
      trace: signalTrace,
      laps: lapList,
      selected: null,
      onselect: () => {},
      canControl: true,
      onthresholds
    });
    const enter = screen.getByRole('slider', { name: 'Enter threshold for ALICE' });
    await fireEvent.keyDown(enter, { key: 'ArrowUp' });
    expect(onthresholds).toHaveBeenLastCalledWith('ALICE', 111, 95);
    await fireEvent.keyDown(enter, { key: 'ArrowDown' });
    expect(onthresholds).toHaveBeenLastCalledWith('ALICE', 109, 95);
    const exit = screen.getByRole('slider', { name: 'Exit threshold for ALICE' });
    await fireEvent.keyDown(exit, { key: 'ArrowDown' });
    expect(onthresholds).toHaveBeenLastCalledWith('ALICE', 110, 94);
  });

  it('`tuned` overrides the drawn levels for the tuned competitor', () => {
    render(RssiGraph, {
      trace: signalTrace,
      laps: lapList,
      selected: null,
      onselect: () => {},
      canControl: true,
      onthresholds: () => {},
      tuned: { competitor: 'ALICE', enter: 120, exit: 80 }
    });
    // The handles report the TUNED values, not the recorded 110/95.
    const enter = screen.getByRole('slider', { name: 'Enter threshold for ALICE' });
    expect(enter).toHaveAttribute('aria-valuenow', '120');
    const exit = screen.getByRole('slider', { name: 'Exit threshold for ALICE' });
    expect(exit).toHaveAttribute('aria-valuenow', '80');
    // The caption meta reflects the live levels too.
    const graph = screen.getByLabelText('RSSI signal graph');
    expect(within(graph).getByText(/enter 120/)).toBeInTheDocument();
  });

  it('renders preview markers for added passes and restyles would-be-removed lap markers', () => {
    render(RssiGraph, {
      trace: signalTrace,
      laps: lapList,
      selected: null,
      onselect: () => {},
      canControl: true,
      preview: { competitor: 'ALICE', added: [30_000_000, 60_000_000], removedRefs: [14] }
    });
    const svg = screen.getByLabelText(/RSSI trace for ALICE/);
    // Two hollow/dashed candidate markers for the added passes.
    expect(svg.querySelectorAll('.preview-added')).toHaveLength(2);
    // ALICE's lap 2 (end_ref 14) is restyled as would-be-removed; lap 1 (end_ref 12) is not.
    const markers = svg.querySelectorAll('.marker');
    expect(markers[1].classList.contains('removed')).toBe(true);
    expect(markers[0].classList.contains('removed')).toBe(false);
    // The legend explains the preview style.
    const graph = screen.getByLabelText('RSSI signal graph');
    expect(within(graph).getByText(/Preview pass/)).toBeInTheDocument();
  });
});
