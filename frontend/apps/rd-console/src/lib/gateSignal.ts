/**
 * Race control's read-only gate view — the pure half (#415).
 *
 * Mid-race is exactly when an RD **cannot** tune and most needs to know what the gate is seeing.
 * When a lap does not register, three completely different faults look identical on the board — a
 * lap count that does not move:
 *
 *  1. the craft is producing **no signal at all** (dead VTX, wrong channel);
 *  2. it **is** crossing, but staying under the enter threshold (mistuned);
 *  3. it is **not crossing** (the pilot took a different line).
 *
 * The trace, the threshold lines and the crossing marks separate them. Everything in this module is
 * the part of that with no DOM in it: which nodes belong to the heat on the timer, which gate is
 * timing which competitor, and what state one gate is in.
 *
 * Nothing here writes, and nothing here carries a tuning level. #355 already refuses threshold
 * writes during a scored heat, and this surface must not imply otherwise.
 */
import type { CompetitorRef, NodeSignal, TimerSignal } from '@gridfpv/types';

import { nodeIndexOf } from './channels.js';

/**
 * What one gate is doing, as the strip renders it.
 *
 * `dead` is the one that has to be got right. The Director samples every node on the same pass and
 * fills an unreported one's slot with `0.0`, so a node RotorHazard has never reported arrives
 * carrying a full, perfectly plottable ring of **zeroes** — which drawn is a flat trace along the
 * floor, indistinguishable from a live node watching a quiet gate. Those are two of the three
 * states this screen exists to tell apart, so [`NodeSignal.seen`] decides, and an unseen node is
 * rendered as dead rather than quiet.
 */
export type GateState = 'dead' | 'crossing' | 'live';

/** One gate on the strip: a node, what it is doing, and who (if anyone) it is timing. */
export interface GateRow {
  /** The node's index on the timer, `0`-based. */
  node: number;
  /**
   * The timer's OWN per-node handle (`node-0`, …), never a locally re-spelled `node-{i}`: it is
   * what a heat's registration binds a competitor to, so re-deriving it is precisely the drift the
   * repo display rule exists to prevent. A wire handle — resolve it before rendering.
   */
  seat: CompetitorRef;
  /** The node's snapshot — thresholds, crossing flags, and the rolling sample ring. */
  signal: NodeSignal;
  /** What the gate is doing. See {@link GateState}. */
  state: GateState;
  /**
   * The heat competitor this gate is timing, when GridFPV can actually say which — see
   * {@link gatesForHeat}. `undefined` is **unknown**, not "nobody": the gate is still plotted, it
   * just carries the seat's own name instead of a callsign.
   */
  competitor?: CompetitorRef;
}

/**
 * Which gate is timing which competitor, for the heat on the timer.
 *
 * Two joins, in order, and neither of them guesses:
 *
 *  1. an **open-practice** lineup is already spelled in node seats (`node-{i}`) — the ref IS the
 *     gate, so it maps straight through;
 *  2. a competition lineup is spelled in pilot handles, and the only thing tying a pilot to a gate
 *     is the **channel**: the heat assigned the seat a frequency and the node reports what it is
 *     actually tuned to. A ref is attributed only when exactly one node is on its frequency **and**
 *     exactly one competitor wants that frequency. Two nodes on 5880 (a real and common
 *     misconfiguration) attribute to neither — a strip that pinned a callsign to the wrong gate
 *     would be worse than one that shows the seat.
 *
 * Unattributed competitors are not an error and are not hidden: every node still gets a plot,
 * labelled by its own seat.
 */
export function gatesForHeat(
  lineup: readonly CompetitorRef[],
  nodes: readonly NodeSignal[],
  mhzFor: (ref: CompetitorRef) => number | undefined
): Map<number, CompetitorRef> {
  const byNode = new Map<number, CompetitorRef>();
  const known = new Set(nodes.map((n) => n.node));
  const claimed = new Set<number>();

  // (1) The lineup ref that IS a gate.
  const unplaced: CompetitorRef[] = [];
  for (const ref of lineup) {
    const node = nodeIndexOf(ref);
    if (node !== undefined && known.has(node) && !claimed.has(node)) {
      byNode.set(node, ref);
      claimed.add(node);
    } else {
      unplaced.push(ref);
    }
  }

  // (2) The channel join, on a strict one-to-one only.
  const nodesByMhz = new Map<number, number[]>();
  for (const n of nodes) {
    if (n.frequency_mhz === undefined || claimed.has(n.node)) continue;
    const at = nodesByMhz.get(n.frequency_mhz);
    if (at) at.push(n.node);
    else nodesByMhz.set(n.frequency_mhz, [n.node]);
  }
  const refsByMhz = new Map<number, CompetitorRef[]>();
  for (const ref of unplaced) {
    const mhz = mhzFor(ref);
    if (mhz === undefined) continue;
    const at = refsByMhz.get(mhz);
    if (at) at.push(ref);
    else refsByMhz.set(mhz, [ref]);
  }
  for (const [mhz, refs] of refsByMhz) {
    const candidates = nodesByMhz.get(mhz);
    if (refs.length !== 1 || candidates?.length !== 1) continue;
    byNode.set(candidates[0], refs[0]);
    claimed.add(candidates[0]);
  }

  return byNode;
}

/**
 * What one node's snapshot says the gate is doing.
 *
 * The crossing test reads [`NodeSignal.crossed_recently`] as well as `crossing`, and that is the
 * whole point of the sticky flag: it survives the Director's decimation, so a fast pass that
 * happened *between* two samples still lights the mark. `crossing` alone — true only if the craft
 * happened to be inside the gate at the instant the Director sampled — misses exactly the passes an
 * RD is squinting for.
 */
export function gateStateOf(node: NodeSignal): GateState {
  if (!node.seen) return 'dead';
  if (node.crossing || node.crossed_recently) return 'crossing';
  return 'live';
}

/** The strip's two groups: the gates timing this heat, and every other node on the timer. */
export interface GateGroups {
  /**
   * The heat's own gates, in lineup order — what the RD is reading the board for.
   *
   * Empty when nothing could be attributed (a sim heat, or a Flexible timer that has told GridFPV
   * no channels at all); the strip then shows every node as one group rather than pretending the
   * heat has no gates.
   */
  racing: GateRow[];
  /**
   * Every other node the timer reports, in node order — including the ones RotorHazard has never
   * reported at all. "Is node 3 even alive?" is a question an RD asks mid-event, and the snapshot
   * carries unseated nodes precisely so it can be answered without leaving the screen.
   */
  others: GateRow[];
}

/**
 * Split a snapshot into the heat's gates and the rest.
 *
 * `lineup` order drives `racing` (the order the RD reads the heat in), node order drives `others`.
 */
export function gateGroups(
  signal: TimerSignal | undefined,
  lineup: readonly CompetitorRef[],
  mhzFor: (ref: CompetitorRef) => number | undefined
): GateGroups {
  const nodes = signal?.nodes ?? [];
  const owner = gatesForHeat(lineup, nodes, mhzFor);
  const byNode = new Map(nodes.map((n) => [n.node, n]));
  const rowOf = (n: NodeSignal): GateRow => ({
    node: n.node,
    seat: n.seat,
    signal: n,
    state: gateStateOf(n),
    competitor: owner.get(n.node)
  });
  // node → competitor inverted, so the lineup can be walked in ITS order.
  const nodeOf = new Map<CompetitorRef, number>();
  for (const [node, competitor] of owner) nodeOf.set(competitor, node);

  const racing: GateRow[] = [];
  const placed = new Set<number>();
  // Lineup order, not node order: the RD reads the heat in the order the board lists it.
  for (const ref of lineup) {
    const node = nodeOf.get(ref);
    if (node === undefined || placed.has(node)) continue;
    const n = byNode.get(node);
    if (!n) continue;
    racing.push(rowOf(n));
    placed.add(node);
  }
  const others = nodes.filter((n) => !placed.has(n.node)).map(rowOf);
  return { racing, others };
}

/** How many of a group's gates the timer has never heard from — the headline of the `others` group. */
export function deadCount(rows: readonly GateRow[]): number {
  return rows.filter((r) => r.state === 'dead').length;
}
