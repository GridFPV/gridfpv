/**
 * Timer **node configuration** — the read/write seam and the pure readings behind it (#412).
 *
 * The bug this exists for was found on a bench: a real 4-node NuclearHazard was configured as 8,
 * and `round_engine` caps pilots per heat on the configured number — so GridFPV would seat eight
 * pilots on a timer that can time four, and four of them would fly a heat that records nothing.
 * Until now there was no way to see that, and no way to clear it but a raw `PUT`.
 *
 * Two halves, deliberately kept apart (D27, and #355's calibration drift):
 *
 *  - **reported** is an *observation* about the hardware, re-read on every connect;
 *  - **configured** (the width override + the disabled set) is GridFPV's *decision*, persisted,
 *    and never silently overwritten by an observation.
 *
 * A disagreement between the two is a `NodeDrift` **notice**, and resolving it is the RD's call —
 * either by disabling the phantom nodes, or by {@link followTimerRequest} ("go back to trusting
 * the hardware"), which is the one-click repair for the bench timer above.
 *
 * ## 1-based on screen, 0-based on the wire
 *
 * `TimerNode.label` is the Director's own 1-based display name (index `0` is `"Node 1"`), and every
 * reading here goes through {@link nodeLabel} to get it. The raw index and the `node-{i}` seat ref
 * are **wire handles** and must never reach the screen (the repo display rule) — here that is not
 * merely tidiness: an off-by-one in this boundary puts a pilot on a dead gate.
 */
import type { CompetitorRef, SetTimerNodesRequest, Timer, TimerNodes } from '@gridfpv/types';

import { nodeIndexOf } from './channels.js';

/**
 * The width a timer falls back to when nothing is configured **and** nothing has been observed —
 * a Mock, an adapter that cannot report, or a RotorHazard not yet dialed. Mirrors the Director's
 * `DEFAULT_NODE_COUNT`; it is the value that caused #412 when it was applied to real hardware.
 */
export const DEFAULT_NODE_COUNT = 8;

/**
 * The body of the **"follow the timer"** write: clear the width override so the timer's own
 * reported count is the width again.
 *
 * `node_count` is three-valued on purpose — absent leaves it, a number pins it, and `null`
 * **clears** it. That is the difference between "the RD chose 8" and "nobody ever asked the
 * hardware", and it is the one-click repair for a timer stuck at a stale explicit width.
 */
export function followTimerRequest(): SetTimerNodesRequest {
  return { node_count: null };
}

/** The **display** name of a node index — always the Director's 1-based label, never the index. */
export function nodeLabel(view: TimerNodes, node: number): string {
  return view.nodes.find((n) => n.node === node)?.label ?? `Node ${node + 1}`;
}

/** The display names of a list of node indices, in the order given. */
export function nodeLabels(view: TimerNodes, nodes: readonly number[]): string[] {
  return nodes.map((node) => nodeLabel(view, node));
}

/** Join labels readably: "Node 5", "Node 5 and Node 6", "Node 5, Node 6 and Node 7". */
export function joinLabels(labels: readonly string[]): string {
  if (labels.length === 0) return '';
  if (labels.length === 1) return labels[0];
  return `${labels.slice(0, -1).join(', ')} and ${labels[labels.length - 1]}`;
}

/** Whether the RD has pinned an explicit width (so "follow the timer" has something to clear). */
export function hasWidthOverride(view: TimerNodes): boolean {
  return view.configured !== undefined && view.configured !== null;
}

/** How many pilots a heat can hold on this timer — the size of the **enabled** set, not the width. */
export function seatCount(view: TimerNodes): number {
  return view.enabled.length;
}

/** The one-line seat reading the dialog shows under the node list. */
export function seatSummary(view: TimerNodes): string {
  const seats = seatCount(view);
  const width = view.width;
  const seatWord = seats === 1 ? 'pilot' : 'pilots';
  if (seats === width) return `${seats} ${seatWord} per heat`;
  return `${seats} ${seatWord} per heat (${width - seats} of ${width} nodes disabled)`;
}

// ── The timer-row reading (no extra fetch) ─────────────────────────────────────
//
// The registry's `Timer` already carries all three inputs — the override, the observation and the
// disabled set — so the row can say the true thing without a per-row `GET /timers/{id}/nodes`.
// It resolves them exactly the way the Director's `Timer::node_width` does, so the row and the
// dialog can never disagree about how many pilots fit.

/** The effective width of a timer: the RD's override, else what it reported, else the fallback. */
export function timerWidth(timer: Timer): number {
  return timer.node_count ?? timer.reported_nodes ?? DEFAULT_NODE_COUNT;
}

/**
 * The node indices a heat can actually be seated on, ascending — the timer's effective width minus
 * the RD's disabled set. Mirror of the Director's `Timer::enabled_nodes`.
 *
 * The indices are **never renumbered** to close a gap: with node 2 disabled on a 4-node timer this
 * returns `[0, 1, 3]`, not `[0, 1, 2]`. A `node-{i}` competitor ref, `NodeSignal.node` and the
 * signal trace all mean the same physical gate, and compacting would make them disagree about
 * where a pass came from.
 */
export function enabledNodes(timer: Timer): number[] {
  const width = Math.max(0, Math.round(timerWidth(timer)));
  const disabled = new Set(timer.disabled_nodes ?? []);
  const out: number[] = [];
  for (let node = 0; node < width; node++) if (!disabled.has(node)) out.push(node);
  return out;
}

/** How many nodes are enabled on a timer — the real per-heat pilot cap. */
export function timerSeats(timer: Timer): number {
  return enabledNodes(timer).length;
}

/** One lineup entry and the gate it flies. */
export interface SeatNode {
  /** The **real** 0-based node index — RotorHazard's `seat_index`, never a compacted position. */
  node: number;
  /** The competitor sitting on it. */
  ref: CompetitorRef;
}

/**
 * Lay a heat's `lineup` onto real node indices — the console-side mirror of the Director's
 * `Timer::seat_nodes`, and the only rule that says which gate each competitor flies.
 *
 * Two kinds of lineup entry, handled together because a heat may mix them:
 *
 *  - a **`node-{i}` seat ref** (an open-practice / pilot-less seat) already *names* its gate, so it
 *    keeps index `i` verbatim — that is the whole point of the handle;
 *  - any other ref (a pilot id) takes the next enabled index no explicit seat has claimed.
 *
 * Entries that cannot be placed are **dropped, not squeezed in**, exactly as the Director drops
 * them: a `node-{i}` ref naming a disabled or non-existent gate, and any pilot beyond the enabled
 * set. Squeezing would seat somebody on the wrong gate, which records *the wrong pilot*.
 *
 * This exists so the seating editor can show the RD the gate a seat will actually fly rather than
 * guessing from row order — the two must not be able to disagree.
 */
export function seatNodes(
  enabled: readonly number[],
  lineup: readonly CompetitorRef[]
): SeatNode[] {
  const seatable = new Set(enabled);
  const claimed = new Set<number>();
  for (const ref of lineup) {
    const named = nodeIndexOf(ref);
    if (named !== undefined && seatable.has(named)) claimed.add(named);
  }
  const free = enabled.filter((node) => !claimed.has(node));
  let next = 0;
  const seats: SeatNode[] = [];
  for (const ref of lineup) {
    const named = nodeIndexOf(ref);
    if (named !== undefined) {
      // An explicit seat ref names its own gate; a gate the RD switched off is not flown.
      if (seatable.has(named)) seats.push({ node: named, ref });
      continue;
    }
    if (next >= free.length) break; // Beyond the enabled set — the heat-size cap should have refused it.
    seats.push({ node: free[next++], ref });
  }
  return seats;
}

/**
 * Whether the row should flag drift: the timer reported a width and GridFPV is using another one.
 *
 * Deliberately looser than the Director's `NodeDrift` (which also fires on an enabled phantom
 * seat): the row's job is only to say *"these two numbers disagree — open this"*, and the dialog
 * carries the exact reading.
 */
export function timerDrifts(timer: Timer): boolean {
  return timer.reported_nodes !== undefined && timer.reported_nodes !== timerWidth(timer);
}

/**
 * The timer row's node reading: the seat count, and the width when some nodes are disabled.
 * Never a raw index — this is a count, and the per-node names live in the dialog.
 */
export function timerNodeSummary(timer: Timer): string {
  const width = timerWidth(timer);
  const seats = timerSeats(timer);
  if (seats === width) return `${width} nodes`;
  return `${seats} of ${width} nodes`;
}

/** What a `NodeDrift` means, phrased for an RD rather than as two numbers. */
export interface DriftReading {
  /** `danger` when GridFPV would seat pilots the hardware cannot time; `info` for spare capacity. */
  tone: 'danger' | 'info';
  /** The headline: what the timer says vs what GridFPV is using. */
  headline: string;
  /** What it costs, in plain words. */
  detail: string;
  /** The **display labels** of the enabled seats at or beyond `reported` — the phantom nodes. */
  phantomLabels: string[];
}

/**
 * Read a timer's drift, or `undefined` when reported and configured agree (the quiet case).
 *
 * The two directions mean different things and must not be flattened into one warning:
 *
 *  - `reported < configured` with an enabled seat at or beyond `reported` is the bench bug — those
 *    are *phantom nodes*, and a pilot seated on one flies a heat that records nothing. `danger`,
 *    and the phantom nodes are **named** (by their 1-based labels).
 *  - `reported < configured` with every extra node already disabled is merely untidy: nobody is
 *    seated on nothing. Informational.
 *  - `reported > configured` is capacity GridFPV is not using. It costs nothing, but a timer that
 *    grew — or an override nobody remembers setting — is worth knowing about.
 */
export function driftReading(view: TimerNodes): DriftReading | undefined {
  const drift = view.drift;
  if (!drift) return undefined;
  const phantomLabels = nodeLabels(view, drift.enabled_beyond_reported);
  const nodeWord = (n: number) => (n === 1 ? 'node' : 'nodes');
  const headline =
    `This timer reports ${drift.reported} ${nodeWord(drift.reported)}; ` +
    `GridFPV is configured for ${drift.configured}.`;
  if (drift.reported > drift.configured) {
    return {
      tone: 'info',
      headline,
      detail:
        'The timer has more nodes than GridFPV is using — spare capacity, not a lost lap. Follow the timer to use them all.',
      phantomLabels
    };
  }
  if (phantomLabels.length === 0) {
    return {
      tone: 'info',
      headline,
      detail:
        'Every node beyond what the timer reports is already disabled, so no pilot is seated on one.',
      phantomLabels
    };
  }
  const many = phantomLabels.length > 1;
  return {
    tone: 'danger',
    headline,
    detail:
      `${joinLabels(phantomLabels)} ${many ? 'are' : 'is'} enabled but ${many ? 'do' : 'does'} not ` +
      `exist on the hardware. ${many ? 'Pilots' : 'A pilot'} seated there would fly and record ` +
      `nothing. Disable ${many ? 'them' : 'it'}, or follow the timer.`,
    phantomLabels
  };
}

/** How badly the scheduled heats overrun this timer's enabled seats. */
export interface HeatOverflow {
  /** How many enabled nodes there are — the real per-heat cap. */
  seats: number;
  /** The largest scheduled heat's pilot count. */
  largest: number;
  /** How many scheduled heats exceed `seats`. */
  heats: number;
}

/**
 * Whether any scheduled heat seats more pilots than this timer has **enabled** nodes — the warning
 * the RD needs *before* the race, not after four pilots have flown for nothing.
 *
 * Compared against the enabled set rather than the width, because a disabled node is not a seat.
 * `undefined` when every heat fits (or there are no heats to check).
 */
export function heatOverflow(
  view: TimerNodes,
  heats: readonly { lineup: readonly unknown[] }[]
): HeatOverflow | undefined {
  const seats = seatCount(view);
  const sizes = heats.map((h) => h.lineup.length);
  const over = sizes.filter((n) => n > seats);
  if (over.length === 0) return undefined;
  return { seats, largest: Math.max(...sizes), heats: over.length };
}

/** The overflow warning as one sentence. */
export function heatOverflowMessage(overflow: HeatOverflow): string {
  const heatWord = overflow.heats === 1 ? 'heat is' : 'heats are';
  const seatWord = overflow.seats === 1 ? 'node' : 'nodes';
  const isAre = overflow.seats === 1 ? 'is' : 'are';
  const stranded = overflow.largest - overflow.seats;
  const pilotWord = stranded === 1 ? 'pilot' : 'pilots';
  return (
    `${overflow.heats} scheduled ${heatWord} built for more pilots than this timer can time: ` +
    `the largest seats ${overflow.largest}, but only ${overflow.seats} ${seatWord} ${isAre} ` +
    `enabled. ${stranded} ${pilotWord} in that heat would record nothing.`
  );
}
