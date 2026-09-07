import { describe, expect, it } from 'vitest';
import type { NodeSignal, TimerSignal } from '@gridfpv/types';

import { deadCount, gateGroups, gateStateOf, gatesForHeat } from '../src/lib/gateSignal.js';

/**
 * The pure half of Race control's read-only gate view (#415).
 *
 * Three things have to be right before any pixel is:
 *
 *  • **which gate is timing whom** — an open-practice lineup IS node seats, a competition lineup is
 *    paired by channel, and an ambiguous channel is left unpaired rather than guessed;
 *  • **`seen` decides dead, not the samples** — an unreported node arrives with a full ring of
 *    zeroes, which plots as a flat trace along the floor: exactly the picture of a live node over a
 *    quiet gate, which is one of the three states this screen exists to tell apart;
 *  • **crossings come from the sticky flag** — `crossed_recently` survives the Director's
 *    decimation, so a fast pass between two samples still registers; `crossing` alone misses it.
 */

const node = (over: Partial<NodeSignal> & { node: number }): NodeSignal => ({
  seat: `node-${over.node}`,
  seen: true,
  crossing: false,
  crossed_recently: false,
  samples: [40, 41, 42],
  ...over
});

const snapshot = (nodes: NodeSignal[]): TimerSignal => ({
  timer: 'rh-1',
  streaming: true,
  lease_ms_remaining: 5_000,
  period_micros: 200_000,
  sample_micros: [0, 200_000, 400_000],
  nodes
});

describe('gatesForHeat — which gate is timing whom', () => {
  it('maps an open-practice lineup straight through: the ref IS the gate', () => {
    const nodes = [node({ node: 0 }), node({ node: 1 })];
    const owner = gatesForHeat(['node-1', 'node-0'], nodes, () => undefined);
    expect(owner.get(0)).toBe('node-0');
    expect(owner.get(1)).toBe('node-1');
  });

  it('pairs a competition lineup by CHANNEL — the only thing tying a pilot to a gate', () => {
    const nodes = [node({ node: 0, frequency_mhz: 5880 }), node({ node: 1, frequency_mhz: 5695 })];
    const mhz = { ALICE: 5695, BOB: 5880 } as Record<string, number>;
    const owner = gatesForHeat(['ALICE', 'BOB'], nodes, (r) => mhz[r]);
    expect(owner.get(1)).toBe('ALICE');
    expect(owner.get(0)).toBe('BOB');
  });

  it('refuses to guess when two nodes share a frequency — a callsign on the wrong gate is worse', () => {
    // A real and common misconfiguration. Both nodes are on 5880; nothing says which one is
    // ALICE's, so neither is claimed and both still get plotted under their own seat.
    const nodes = [node({ node: 0, frequency_mhz: 5880 }), node({ node: 1, frequency_mhz: 5880 })];
    const owner = gatesForHeat(['ALICE'], nodes, () => 5880);
    expect(owner.size).toBe(0);
  });

  it('refuses to guess when two competitors want one frequency', () => {
    const nodes = [node({ node: 0, frequency_mhz: 5880 })];
    const owner = gatesForHeat(['ALICE', 'BOB'], nodes, () => 5880);
    expect(owner.size).toBe(0);
  });

  it('claims nothing when GridFPV knows no channel at all (a sim heat / an empty pool)', () => {
    const nodes = [node({ node: 0 }), node({ node: 1 })];
    expect(gatesForHeat(['ALICE', 'BOB'], nodes, () => undefined).size).toBe(0);
  });

  it('pairs an UNSEEN node too — a dead gate still belongs to the pilot on that channel', () => {
    // The whole point of the strip: "ALICE's gate is not reporting" is the answer, and it needs the
    // pairing to survive the node being dead.
    const nodes = [node({ node: 0, seen: false, frequency_mhz: 5880, samples: [0, 0, 0] })];
    expect(gatesForHeat(['ALICE'], nodes, () => 5880).get(0)).toBe('ALICE');
  });
});

describe('gateStateOf — dead, crossing, live', () => {
  it('calls an unreported node DEAD even though its ring of zeroes plots perfectly', () => {
    const dead = node({ node: 2, seen: false, samples: [0, 0, 0] });
    expect(gateStateOf(dead)).toBe('dead');
  });

  it('reads the STICKY crossing flag, so a pass between two samples still registers', () => {
    // `crossing` false at both sampled instants; `crossed_recently` is what survives the
    // Director's decimation and is the only reason a fast pass lights the mark at all.
    const fast = node({ node: 0, crossing: false, crossed_recently: true });
    expect(gateStateOf(fast)).toBe('crossing');
  });

  it('is live — not crossing — when the gate is simply quiet', () => {
    expect(gateStateOf(node({ node: 0 }))).toBe('live');
  });
});

describe('gateGroups — the heat first, everything else second', () => {
  const nodes = [
    node({ node: 0, frequency_mhz: 5880 }),
    node({ node: 1, frequency_mhz: 5695 }),
    node({ node: 2, seen: false, samples: [0, 0, 0] })
  ];
  const mhz = { ALICE: 5695, BOB: 5880 } as Record<string, number>;

  it('orders the heat gates by the LINEUP, not by node index', () => {
    // The RD reads the heat in the order the board lists it; ALICE is on node 1.
    const { racing } = gateGroups(snapshot(nodes), ['ALICE', 'BOB'], (r) => mhz[r]);
    expect(racing.map((r) => r.node)).toEqual([1, 0]);
    expect(racing.map((r) => r.competitor)).toEqual(['ALICE', 'BOB']);
  });

  it('keeps every other node the timer reports — including the ones it has never heard from', () => {
    // "Is node 3 even alive?" is a question an RD asks mid-event, and the snapshot carries
    // unseated nodes precisely so it can be answered without leaving Race control.
    const { others } = gateGroups(snapshot(nodes), ['ALICE', 'BOB'], (r) => mhz[r]);
    expect(others.map((r) => r.node)).toEqual([2]);
    expect(deadCount(others)).toBe(1);
  });

  it('leaves `racing` empty when nothing could be attributed — the caller shows one group', () => {
    const { racing, others } = gateGroups(snapshot(nodes), ['ALICE', 'BOB'], () => undefined);
    expect(racing).toEqual([]);
    expect(others.map((r) => r.node)).toEqual([0, 1, 2]);
  });

  it('is empty, not broken, before the first snapshot lands', () => {
    expect(gateGroups(undefined, ['ALICE'], () => 5880)).toEqual({ racing: [], others: [] });
  });
});
