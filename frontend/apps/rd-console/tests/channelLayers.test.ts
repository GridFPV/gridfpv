/**
 * Channel-layer readings (#117 S2) — the pure half.
 *
 * These are the sentences an RD reads while laying channels onto nodes, so they are asserted as
 * **text**. Two things are load-bearing here and are asserted in both directions:
 *
 *  - no raw id, raw node index or bare MHz reaches the screen — a node is `"Node 3"`, a channel is
 *    `"Raceband R7"`, a layer is `"Bracket A"` (CLAUDE.md's display rule);
 *  - cross-layer channel reuse produces a **warning sentence**, never a blocker — that split is the
 *    RD's own decision and is what makes the bracket and the GQ strategies share one mechanism.
 */
import { describe, expect, it } from 'vitest';
import type { ChannelCatalogEntry, ChannelLayer, TimerNode, TimerNodes } from '@gridfpv/types';
import {
  allowedChannels,
  draftBlocker,
  draftNodes,
  duplicateNodes,
  layerName,
  layerNodes,
  layerNodeLabel,
  layerSummary,
  overlapMessage,
  unconfiguredTimerMessage,
  untunedNodes,
  type LayerDraft
} from '../src/lib/channelLayers.js';

const CATALOG: ChannelCatalogEntry[] = [
  { band: 'Raceband', channel: 'R1', mhz: 5658 },
  { band: 'Raceband', channel: 'R2', mhz: 5695 },
  { band: 'Raceband', channel: 'R3', mhz: 5732 },
  { band: 'Raceband', channel: 'R4', mhz: 5769 },
  { band: 'Fatshark', channel: 'F4', mhz: 5800 }
];

/** One node the way the Director sends it: 0-based index, 1-based label. */
function node(index: number, enabled = true): TimerNode {
  return {
    node: index,
    label: `Node ${index + 1}`,
    seat: `node-${index}`,
    enabled,
    reported: true
  };
}

/** A four-node timer with node index 2 disabled — an enabled set with a hole in it. */
function holedView(): TimerNodes {
  return {
    timer: 'rh-1',
    width: 4,
    nodes: [node(0), node(1), node(2, false), node(3)],
    enabled: [0, 1, 3]
  };
}

function draft(name: string, pairs: [number, number][]): LayerDraft {
  return { name, channels: new Map(pairs) };
}

const LAYERS: ChannelLayer[] = [
  {
    id: 'bracket-a-k3f9',
    name: 'Bracket A',
    nodes: [
      { node: 0, channel: 5658 },
      { node: 1, channel: 5695 }
    ]
  },
  {
    id: 'pack-b-z1x8',
    name: 'Pack B',
    nodes: [
      { node: 0, channel: 5658 },
      { node: 1, channel: 5800 }
    ]
  }
];

describe('the timer a layer draws from', () => {
  it('offers the ALLOWED set, in the RD’s own order — not the catalog', () => {
    // The Tune page offers everything the hardware can tune; a layer offers what the RD said this
    // timer may use. Offering more would offer a choice the Director then refuses.
    const timer = { available_channels: [5769, 5658] } as never;
    expect(allowedChannels(timer)).toEqual([5769, 5658]);
  });

  it('reads an empty allowed set as "not configured yet", never as "no channels"', () => {
    const timer = { name: 'NuclearHazard', available_channels: [] } as never;
    const message = unconfiguredTimerMessage(timer);
    expect(message).toContain('NuclearHazard');
    expect(message).toContain('Timers page');
  });

  it('says so when the event has no timer at all', () => {
    expect(unconfiguredTimerMessage(undefined)).toContain('no timer selected');
  });

  it('is quiet once channels are ticked', () => {
    const timer = { name: 'Mock', available_channels: [5658] } as never;
    expect(unconfiguredTimerMessage(timer)).toBeUndefined();
  });
});

describe('the nodes a layer tunes', () => {
  it('is the ENABLED set, holes and all (#412)', () => {
    // Node index 2 is disabled, so a layer must not offer to tune it — a disabled node seats
    // nobody, and pretending to tune it hides a dead gate.
    expect(layerNodes(holedView())).toEqual([0, 1, 3]);
  });

  it('is empty until the node view lands (fails closed)', () => {
    expect(layerNodes(undefined)).toEqual([]);
  });
});

describe('the draft', () => {
  it('goes on the wire ascending by node, with unset nodes simply absent', () => {
    expect(
      draftNodes(
        draft('A', [
          [3, 5769],
          [0, 5658]
        ])
      )
    ).toEqual([
      { node: 0, channel: 5658 },
      { node: 3, channel: 5769 }
    ]);
  });

  it('flags every node sharing a channel — the one hard rule inside a layer', () => {
    const clashing = draft('A', [
      [0, 5658],
      [1, 5695],
      [3, 5658]
    ]);
    expect([...duplicateNodes(clashing)].sort()).toEqual([0, 3]);
  });

  it('lists the nodes still untuned — a layer is a complete tuning', () => {
    expect(untunedNodes(draft('A', [[0, 5658]]), [0, 1, 3])).toEqual([1, 3]);
  });
});

describe('why Save is blocked', () => {
  it('asks for a name first — it is what a heat picks the layer by', () => {
    expect(draftBlocker(draft('  ', []), [0], holedView(), CATALOG)).toContain('Name this layer');
  });

  it('names both clashing nodes and the channel, never an index or a bare MHz', () => {
    const message = draftBlocker(
      draft('Bracket A', [
        [0, 5658],
        [1, 5695],
        [3, 5658]
      ]),
      [0, 1, 3],
      holedView(),
      CATALOG
    );
    expect(message).toContain('Node 1');
    expect(message).toContain('Node 4');
    expect(message).toContain('Raceband R1');
    expect(message).not.toContain('5658');
    expect(message).toContain('cannot share a frequency');
  });

  it('names the untuned nodes by their 1-based labels', () => {
    const message = draftBlocker(draft('Bracket A', [[0, 5658]]), [0, 1, 3], holedView(), CATALOG);
    expect(message).toBe('Set a channel for Node 2, Node 4 — a layer tunes every enabled node.');
  });

  it('is quiet for a complete, conflict-free tuning', () => {
    const complete = draft('Bracket A', [
      [0, 5658],
      [1, 5695],
      [3, 5769]
    ]);
    expect(draftBlocker(complete, [0, 1, 3], holedView(), CATALOG)).toBeUndefined();
  });
});

describe('what a layer looks like on screen', () => {
  it('renders a node and its channel together — the pair an RD needs', () => {
    expect(layerNodeLabel({ node: 2, channel: 5732 }, CATALOG)).toBe('Node 3 · Raceband R3');
  });

  it('summarises the whole tuning without a single raw value', () => {
    const summary = layerSummary(LAYERS[0], CATALOG);
    expect(summary).toBe('Node 1 · Raceband R1 · Node 2 · Raceband R2');
    expect(summary).not.toMatch(/\d{4}/);
  });

  it('resolves a layer id to its name, and a deleted one to a phrase', () => {
    expect(layerName(LAYERS, 'pack-b-z1x8')).toBe('Pack B');
    expect(layerName(LAYERS, 'gone')).toBe('a deleted layer');
  });
});

describe('cross-layer overlap', () => {
  it('warns by naming both layers and the shared channel — and reads as ignorable', () => {
    const message = overlapMessage(
      { layer: 'bracket-a-k3f9', other: 'pack-b-z1x8', channels: [5658] },
      LAYERS,
      CATALOG
    );
    expect(message).toContain('Bracket A');
    expect(message).toContain('Pack B');
    expect(message).toContain('Raceband R1');
    // The RD's decision: fine for a bracket, only matters for the keep-pilots-on-one-channel
    // strategy. Nothing here should read as "this is broken".
    expect(message).toContain('That is fine for a bracket');
    // And no raw id or MHz leaks into it.
    expect(message).not.toContain('bracket-a-k3f9');
    expect(message).not.toContain('5658');
  });

  it('lists several shared channels readably', () => {
    const message = overlapMessage(
      { layer: 'bracket-a-k3f9', other: 'pack-b-z1x8', channels: [5658, 5695, 5732] },
      LAYERS,
      CATALOG
    );
    expect(message).toContain('Raceband R1, Raceband R2 and Raceband R3');
  });
});
