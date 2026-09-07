import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/dom';
import RssiGraph from '../src/lib/RssiGraph.svelte';
import { signalTrace, lapList } from './fixtures.js';

/**
 * RssiGraph **characterisation** suite (#355).
 *
 * This file exists to pin what MARSHALING draws, so that generalising `RssiGraph` into a
 * two-mode (review / live) component cannot change review mode by accident. The bar the issue
 * sets is *"review mode's rendering must be unchanged, not equivalent"* — a regression here is a
 * product regression in a shipped feature (#348/#354's draggable calibration), not a test
 * failure.
 *
 * Two kinds of pin:
 *
 *  1. **Markup snapshots** (`__snapshots__/*.svg`) of the whole rendered graph — legend,
 *     figcaption, zoom controls, every svg node and attribute — for the two review shapes that
 *     matter: display-only, and the full tuning surface (`onthresholds` + `tuned` + `preview` +
 *     a selected lap + `canControl`). Every coordinate, class name, aria value, transform and
 *     text node is pinned verbatim, in paint order; if the refactor moves a single coordinate,
 *     these fail. Only Svelte's own codegen noise is normalised away — see {@link drawn}.
 *
 *     These were generated against the PRE-refactor component and must never be regenerated to
 *     make a change pass.
 *
 *  2. **Behavioural pins** for the interactions the snapshots cannot see — lap selection in both
 *     directions, keyboard select, and the `onthresholds`/`tuned` calibration surface. (The
 *     drag/nudge/preview interactions are additionally covered in `RssiGraph.test.ts`; the
 *     overlap is deliberate — this file must stand alone as the safety net.)
 *
 * Do NOT regenerate the snapshots to make a refactor pass. If they change, the refactor changed
 * marshaling.
 */

const W = 1000;

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
 * Normalise a rendered fragment down to **what is drawn**, so the snapshots pin the picture and
 * not Svelte's codegen.
 *
 * Three things are stripped, none of which reach the screen:
 *  • `<!---->` — the anchor comments Svelte plants for `{#if}`/`{#each}`/`{expr}` blocks. Adding a
 *    block moves these around without changing a single pixel.
 *  • attribute ORDER — Svelte splits a tag's attributes between the static template string and
 *    runtime `setAttribute` calls, and which side an attribute lands on shifts as expressions move.
 *    Sorting makes the comparison about the attributes and their values.
 *  • whitespace runs — collapsed, then one element per line so a failure diffs readably.
 *
 * Everything that does reach the screen — every element, class, aria attribute, coordinate,
 * transform and text node — survives verbatim. If one of those changes, these snapshots fail.
 */
function drawn(html: string): string {
  return html
    .replace(/<!---->/g, '')
    .replace(/\s+/g, ' ')
    .replace(
      /<([a-zA-Z][-\w]*)((?:\s+[-:\w]+(?:="[^"]*")?)*)\s*(\/?)>/g,
      (_m, tag, attrs, close) =>
        `<${tag}${[...(attrs as string).matchAll(/([-:\w]+)(?:="([^"]*)")?/g)]
          .map(([, name, value]) => (value === undefined ? name : `${name}="${value}"`))
          .sort()
          .map((a) => ` ${a}`)
          .join('')}${close}>`
    )
    .replace(/</g, '\n<')
    .trim();
}

describe('RssiGraph review mode — pinned rendering', () => {
  it('renders the display-only marshaling graph exactly', async () => {
    render(RssiGraph, {
      trace: signalTrace,
      laps: lapList,
      selected: null,
      onselect: () => {}
    });
    const graph = screen.getByLabelText('RSSI signal graph');
    await expect(drawn(graph.outerHTML)).toMatchFileSnapshot(
      './__snapshots__/RssiGraph.review-display-only.svg'
    );
  });

  it('renders the full tuning surface exactly (thresholds + tuned + preview + selection)', async () => {
    render(RssiGraph, {
      trace: signalTrace,
      laps: lapList,
      selected: { competitor: 'ALICE', lap: lapList.competitors[0].laps[0] },
      onselect: () => {},
      onaddlap: () => {},
      canControl: true,
      nameFor: (ref) => (ref === 'ALICE' ? 'Maverick' : ref),
      onthresholds: () => {},
      tuned: { competitor: 'ALICE', enter: 118, exit: 88 },
      preview: { competitor: 'ALICE', added: [30_000_000, 60_000_000], removedRefs: [14] }
    });
    const graph = screen.getByLabelText('RSSI signal graph');
    await expect(drawn(graph.outerHTML)).toMatchFileSnapshot(
      './__snapshots__/RssiGraph.review-tuning.svg'
    );
  });
});

describe('RssiGraph review mode — pinned lap behaviour', () => {
  it('marks the parent-selected lap (two-way with the lap list) and no other', () => {
    render(RssiGraph, {
      trace: signalTrace,
      laps: lapList,
      selected: { competitor: 'ALICE', lap: lapList.competitors[0].laps[1] },
      onselect: () => {}
    });
    const svg = screen.getByLabelText(/RSSI trace for ALICE/);
    const markers = svg.querySelectorAll('.marker');
    expect(markers).toHaveLength(2);
    expect(markers[0].classList.contains('selected')).toBe(false);
    expect(markers[0].getAttribute('aria-pressed')).toBe('false');
    expect(markers[1].classList.contains('selected')).toBe(true);
    expect(markers[1].getAttribute('aria-pressed')).toBe('true');
  });

  it('emits the clicked lap back to the parent', async () => {
    const onselect = vi.fn();
    render(RssiGraph, { trace: signalTrace, laps: lapList, selected: null, onselect });
    const graph = screen.getByLabelText('RSSI signal graph');
    await fireEvent.click(within(graph).getByRole('button', { name: /Lap 2 at .* — select/ }));
    expect(onselect).toHaveBeenCalledTimes(1);
    expect(onselect.mock.calls[0][0]).toBe('ALICE');
    expect(onselect.mock.calls[0][1].number).toBe(2);
  });

  it('selects a lap marker from the keyboard (Enter and Space)', async () => {
    const onselect = vi.fn();
    render(RssiGraph, { trace: signalTrace, laps: lapList, selected: null, onselect });
    const graph = screen.getByLabelText('RSSI signal graph');
    const marker = within(graph).getByRole('button', { name: /Lap 1 at .* — select/ });
    await fireEvent.keyDown(marker, { key: 'Enter' });
    await fireEvent.keyDown(marker, { key: ' ' });
    expect(onselect).toHaveBeenCalledTimes(2);
    await fireEvent.keyDown(marker, { key: 'Escape' });
    expect(onselect).toHaveBeenCalledTimes(2);
  });

  it('draws only the laps belonging to the traced competitor (BOB has no trace)', () => {
    render(RssiGraph, { trace: signalTrace, laps: lapList, selected: null, onselect: () => {} });
    const graph = screen.getByLabelText('RSSI signal graph');
    expect(graph.querySelectorAll('figure.trace')).toHaveLength(1);
    expect(within(graph).queryByRole('button', { name: /Lap 1 at 43\./ })).toBeNull();
  });
});

describe('RssiGraph review mode — pinned calibration surface', () => {
  it('exposes no threshold handles without `onthresholds`, and both with it', () => {
    const { unmount } = render(RssiGraph, {
      trace: signalTrace,
      laps: lapList,
      selected: null,
      onselect: () => {}
    });
    expect(screen.queryAllByRole('slider')).toHaveLength(0);
    unmount();

    render(RssiGraph, {
      trace: signalTrace,
      laps: lapList,
      selected: null,
      onselect: () => {},
      onthresholds: () => {}
    });
    const enter = screen.getByRole('slider', { name: 'Enter threshold for ALICE' });
    const exit = screen.getByRole('slider', { name: 'Exit threshold for ALICE' });
    expect(enter).toHaveAttribute('aria-valuenow', '110');
    expect(exit).toHaveAttribute('aria-valuenow', '95');
  });

  it('`tuned` re-draws the levels, the crossing windows and the caption together', () => {
    // The recorded 110/95 give two crossing windows over the fixture's two peaks. Dropping the
    // enter level to 80 makes the whole trace one long crossing — the shaded band, the threshold
    // lines and the caption meta must all move to the tuned levels, not just the handles.
    render(RssiGraph, {
      trace: signalTrace,
      laps: lapList,
      selected: null,
      onselect: () => {},
      onthresholds: () => {},
      tuned: { competitor: 'ALICE', enter: 80, exit: 60 }
    });
    const svg = screen.getByLabelText(/RSSI trace for ALICE/);
    expect(svg.querySelectorAll('.crossing')).toHaveLength(1);
    const graph = screen.getByLabelText('RSSI signal graph');
    expect(within(graph).getByText(/enter 80/)).toBeInTheDocument();
    expect(within(graph).getByText(/exit 60/)).toBeInTheDocument();
    expect(screen.getByRole('slider', { name: 'Enter threshold for ALICE' })).toHaveAttribute(
      'aria-valuenow',
      '80'
    );
  });

  it('emits both levels on a drag, leaving the untouched one at its drawn value', async () => {
    const onthresholds = vi.fn();
    render(RssiGraph, {
      trace: signalTrace,
      laps: lapList,
      selected: null,
      onselect: () => {},
      onthresholds,
      tuned: { competitor: 'ALICE', enter: 118, exit: 88 }
    });
    const svg = screen.getByLabelText(/RSSI trace for ALICE/);
    pinSvgBox(svg);
    const enter = screen.getByRole('slider', { name: 'Enter threshold for ALICE' });
    await fireEvent.keyDown(enter, { key: 'ArrowUp' });
    // Nudging enter carries the CURRENT tuned exit (88), not the trace's recorded 95.
    expect(onthresholds).toHaveBeenLastCalledWith('ALICE', 119, 88);
  });
});
