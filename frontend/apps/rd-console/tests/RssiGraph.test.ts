import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/dom';
import type { CompetitorRef } from '@gridfpv/types';
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
 * Click-to-add-a-lap (gap 2): a click on a competitor's trace calls `onaddlap` with the cursor's
 * source-time (the px→source-time mapping), role-gated by `canControl`. The explicit "Add lap here"
 * button at the crosshair does the same for trackpad/keyboard users.
 */
describe('RssiGraph add-lap', () => {
  it('clicking the trace at a known X calls onaddlap with the matching source-time', async () => {
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

    // Click at the X mapping to 30.0s.
    await fireEvent.click(svg, { clientX: xForTime(30_000_000) });
    expect(onaddlap).toHaveBeenCalledTimes(1);
    const [ref, at] = onaddlap.mock.calls[0];
    expect(ref).toBe('ALICE');
    // The source-time is the cursor's race-relative instant (≈30.0s), within a rounding tolerance.
    expect(at).toBeGreaterThan(29_900_000);
    expect(at).toBeLessThan(30_100_000);
  });

  it('the "Add lap here" button adds at the cursor time', async () => {
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

  it('clicking a lap marker selects (not adds) — the add-lap handler does not fire', async () => {
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

  it('is review-only when canControl is false: a trace click does nothing', async () => {
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
    // …and no "Add lap here" affordance is offered.
    await fireEvent.mouseMove(svg, { clientX: xForTime(30_000_000) });
    expect(screen.queryByRole('button', { name: /Add lap for/ })).toBeNull();
  });
});
