import { describe, expect, it } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import type { CompetitorRef } from '@gridfpv/types';
import RssiGraph from '../src/lib/RssiGraph.svelte';
import { signalTrace, lapList } from './fixtures.js';

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
