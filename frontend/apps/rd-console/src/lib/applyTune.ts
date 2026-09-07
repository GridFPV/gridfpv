/**
 * **Apply a marshaled re-detection's thresholds to the timer** (#470) — the pure half.
 *
 * Marshaling's "Tune detection" panel already lets an RD find the enter/exit levels that would
 * have called a competitor's laps correctly, by replaying the timer's hysteresis over the captured
 * trace (`redetect.ts`). Until now those levels died on the screen: committing turned the *diff*
 * into lap corrections, but the thresholds themselves — the thing that would stop the gate getting
 * it wrong again on the next heat — were never written anywhere, and the RD had to retype them on
 * the Tune page from memory.
 *
 * This module is the bridge. It answers two questions and nothing else, so both are testable with
 * no DOM and no I/O:
 *
 *  1. {@link applyTuneGate} — *may these levels be written, and to which node?*
 *  2. {@link confirmCalibration} — *did the write actually land on the hardware?*
 *
 * ## It reuses the Tune page's vocabulary rather than restating it
 *
 * The write goes through the **same** path a Tune-page slider does — `session.setCalibration` →
 * `POST /timers/{id}/calibration` → the protocol client's `setCalibration` — and it reports itself
 * with the same {@link ThresholdPhase}, {@link phaseLabel} and {@link phaseTone} the Tune page
 * shows, imported from `tuning.ts`. Two surfaces that write the same value to the same hardware
 * must not describe it in two vocabularies: "On timer" has to mean the same thing on both screens,
 * or the RD learns to distrust whichever one they saw last.
 *
 * ## Accepted is not applied, here as everywhere
 *
 * `POST /calibration` answers "accepted" and nothing more — RotorHazard does not echo a level set
 * (CLAUDE.md: *every write gets a readback*). So the proof is a **poll**: after the write this
 * re-reads `GET /timers/{id}/signal` until the node reports the levels we sent, and says
 * `mismatch` if it never does. Marshaling does not otherwise hold a signal subscription, so
 * {@link confirmCalibration} opens one for the few polls it needs and the caller stops it after —
 * the lease (`SIGNAL_LEASE_MS`) is the backstop if that stop never happens.
 *
 * ## Which node is "this pilot's node"
 *
 * The heat's **lineup position**. A RotorHazard trace is captured per node seat (`node-{n}`) and
 * the Director re-attributes it to `lineup[n]` on the way into the log
 * (`crates/app/src/source/rotorhazard.rs::remap`), so the marshaled competitor's index in
 * `HeatSummary.lineup` *is* the seat index the calibration request addresses. That inversion is
 * the only way back to a node number for a competition heat, whose trace is keyed on a pilot ref
 * rather than a seat — and it is why a heat whose lineup we cannot see refuses rather than guesses.
 */
import type {
  CalibrationRequest,
  ChannelCatalogEntry,
  CompetitorRef,
  HeatPhase,
  NodeSignal,
  Timer,
  TimerId,
  TimerSignal
} from '@gridfpv/types';

import { nodeSeatLabel } from './channels.js';
import {
  CONFIRM_TIMEOUT_MS,
  clampLevel,
  writeGate,
  type ThresholdPhase,
  type WriteGate
} from './tuning.js';

/** Which node on which timer a marshaled competitor's levels would be written to. */
export interface TuneTarget {
  /** The timer to write to. */
  timer: TimerId;
  /** The node's 0-based seat index — RotorHazard's `seat_index`, what `CalibrationRequest` takes. */
  node: number;
  /** The node's friendly name (`"Node 3 · Raceband R7"`), for every message about this write. */
  nodeName: string;
}

/** The answer to "may this discovered tune be pushed to the timer?". */
export type ApplyGate = { allowed: true; target: TuneTarget } | { allowed: false; reason: string };

/** Everything {@link applyTuneGate} needs to decide. */
export interface ApplyGateInput {
  /** Whether the session may write at all (the role gate). */
  canControl: boolean;
  /** The event's primary timer, or `undefined` when the event has none / the poll has not landed. */
  timer: Timer | undefined;
  /** The marshaled heat's lineup — the seat order the node index is read out of. */
  lineup: CompetitorRef[] | undefined;
  /** The competitor whose levels are being applied. */
  competitor: CompetitorRef;
  /** The tuned enter level. */
  enter: number;
  /** The tuned exit level. */
  exit: number;
  /** The phase of the heat currently **on the timer** (not the marshaled one). */
  livePhase: HeatPhase | undefined;
  /** Whether that live heat is open practice or a scored one. */
  liveHeatKind: 'practice' | 'competition' | undefined;
  /** The channel catalog, so the node is named rather than numbered. */
  catalog?: ChannelCatalogEntry[];
  /** What the node is tuned to, when known — for the node's friendly name. */
  nodeMhz?: number;
}

/**
 * How many nodes a timer may be written to, resolving the RD's explicit override against what the
 * hardware reported (`Timer::node_width`'s rule, mirrored). `0` when neither is known — which
 * fails closed below rather than offering a seat that may not exist.
 */
function nodeWidthOf(timer: Timer): number {
  return Math.max(0, timer.node_count ?? timer.reported_nodes ?? 0);
}

/**
 * Whether the discovered levels may be written, and to which node (#470).
 *
 * **Fails closed, with a reason, every time.** A disabled control that does not say why is the
 * dead end #405 exists to prevent, so every refusal here carries copy the screen renders next to
 * the button — never a bare disabled state.
 *
 * The order is deliberate: authority first (a read-only session cannot write anything), then the
 * value itself, then the hardware, then the node, and only last the heat-in-progress rule — so the
 * RD is told the most fundamental blocker rather than whichever one happens to be checked first.
 */
export function applyTuneGate(input: ApplyGateInput): ApplyGate {
  const { canControl, timer, lineup, competitor, enter, exit } = input;

  if (!canControl) {
    return {
      allowed: false,
      reason:
        'This session is read-only, so it cannot change a timer’s thresholds. Sign in with the Director’s control token to apply a tune.'
    };
  }

  // The same hysteresis rule re-detection itself enforces: equal or inverted levels detect nothing,
  // so writing them would leave the gate blind.
  if (!(enter > exit)) {
    return {
      allowed: false,
      reason:
        'Enter must be above exit — these levels detect nothing, so they are not worth writing.'
    };
  }

  if (!timer) {
    return {
      allowed: false,
      reason:
        'This event has no primary timer, so there is no gate to write to. Pick one on the event’s Timers page.'
    };
  }

  // A Mock has no hardware behind it; the Director refuses the write outright.
  if ('Mock' in timer.kind) {
    return {
      allowed: false,
      reason: `${timer.name} is the built-in Mock source — it has no gate, so there is nothing to calibrate.`
    };
  }

  if (timer.status !== 'Connected') {
    return {
      allowed: false,
      reason: `${timer.name} is not connected (${timer.status.toLowerCase()}), so a threshold cannot be written. Connect it on the Timers page and try again.`
    };
  }

  // The node index IS the competitor's seat in the marshaled heat's lineup — see the module doc.
  // Without the lineup there is no way back to a node, and guessing one would calibrate a gate
  // some other pilot flew.
  const node = lineup?.indexOf(competitor) ?? -1;
  if (node < 0) {
    return {
      allowed: false,
      reason:
        'GridFPV can’t tell which node this competitor flew in this heat, so it can’t say which gate to calibrate.'
    };
  }

  const width = nodeWidthOf(timer);
  if (width === 0 || node >= width) {
    return {
      allowed: false,
      reason: `${timer.name} does not report a node for this seat, so there is nothing to write to.`
    };
  }

  if (timer.disabled_nodes.includes(node)) {
    return {
      allowed: false,
      reason: `Node ${node + 1} is disabled on ${timer.name}, so it is not calibrated. Re-enable it on the Timers page first.`
    };
  }

  // Last: the live-heat rule. Shared with the Tune page rather than restated, so the two screens
  // can never disagree about when a write is refused.
  const gate: WriteGate = writeGate(input.livePhase, input.liveHeatKind);
  if (!gate.allowed) return { allowed: false, reason: gate.reason };

  return {
    allowed: true,
    target: {
      timer: timer.id,
      node,
      nodeName: nodeSeatLabel(node, input.nodeMhz, input.catalog ?? [])
    }
  };
}

/**
 * The calibration body for a discovered tune: **both** levels, clamped through the one rule every
 * threshold editor funnels through ({@link clampLevel}).
 *
 * Both are sent together, unlike the Tune page's per-slider write, because a re-detection produces
 * an enter/exit *pair* — the levels were found together and only make sense together. Sending one
 * would leave the gate straddling the old level and the new one.
 */
export function calibrationFor(node: number, enter: number, exit: number): CalibrationRequest {
  return { node, enter_at: clampLevel(enter), exit_at: clampLevel(exit) };
}

/** What a node currently reports for the two thresholds. */
function reportedLevels(
  signal: TimerSignal | undefined,
  node: number
): { enter?: number; exit?: number } {
  const n: NodeSignal | undefined = signal?.nodes.find((x) => x.node === node);
  return { enter: n?.enter_at, exit: n?.exit_at };
}

/** Whether a reported level matches what was sent, in the integer domain a level lives in. */
function matches(reported: number | undefined, sent: number): boolean {
  return reported !== undefined && clampLevel(reported) === sent;
}

/** The outcome of a calibration write, phrased the way the Tune page phrases one. */
export interface ApplyOutcome {
  /** Where the write ended up. Only `confirmed`, `mismatch` and `failed` are terminal. */
  phase: ThresholdPhase;
  /** Why, for anything that is not `confirmed`. Rendered on the panel, never swallowed. */
  detail?: string;
}

/** The seams {@link confirmCalibration} polls through, injected so it is testable without a clock. */
export interface ConfirmDeps {
  /** Poll one snapshot of the timer's signal (this is also what renews the Director's lease). */
  fetchSignal: () => Promise<TimerSignal>;
  /** Wait, between polls. */
  sleep: (ms: number) => Promise<void>;
  /** The clock the timeout runs against. */
  now: () => number;
}

/** How often to re-read the signal while waiting for the levels to come back. */
export const CONFIRM_POLL_MS = 250;

/**
 * Wait for the timer to report the levels that were just written — the readback (#470, CLAUDE.md).
 *
 * RotorHazard does not acknowledge a level set, and the Director's `200` only means it dispatched
 * one, so this is the **only** evidence the write landed: re-read `GET /timers/{id}/signal` until
 * the node reports both levels holding what was sent. A poll that keeps disagreeing past
 * {@link CONFIRM_TIMEOUT_MS} is a `mismatch`, said plainly — this is #403's failure class, where
 * every readout claimed success over a value the hardware never took.
 *
 * A failing poll is not itself a failure: the feed may be briefly unavailable while the change is
 * in flight, so read errors are swallowed and retried, and only the timeout decides. Reporting
 * "did not take" because one HTTP request lost a race would be its own kind of lie.
 */
export async function confirmCalibration(
  deps: ConfirmDeps,
  node: number,
  sentEnter: number,
  sentExit: number,
  timeoutMs: number = CONFIRM_TIMEOUT_MS
): Promise<ApplyOutcome> {
  const startedAt = deps.now();
  let last: { enter?: number; exit?: number } = {};

  for (;;) {
    try {
      last = reportedLevels(await deps.fetchSignal(), node);
      if (matches(last.enter, sentEnter) && matches(last.exit, sentExit)) {
        return { phase: 'confirmed' };
      }
    } catch {
      // A dropped poll proves nothing — keep waiting for the deadline to decide.
    }
    if (deps.now() - startedAt >= timeoutMs) break;
    await deps.sleep(CONFIRM_POLL_MS);
  }

  return { phase: 'mismatch', detail: mismatchDetail(last, sentEnter, sentExit) };
}

/**
 * What to say when the levels never came back. Names what the timer is actually holding, because
 * "it didn't work" leaves the RD with nothing to act on — the whole point of the readback is that
 * the RD learns the gate is not where they think it is.
 */
function mismatchDetail(
  reported: { enter?: number; exit?: number },
  sentEnter: number,
  sentExit: number
): string {
  if (reported.enter === undefined && reported.exit === undefined) {
    return 'The timer is not reporting this node’s thresholds at all. The change did not take.';
  }
  const say = (v: number | undefined) => (v === undefined ? 'nothing' : String(clampLevel(v)));
  return (
    `The timer reports enter ${say(reported.enter)} / exit ${say(reported.exit)}, ` +
    `not ${sentEnter} / ${sentExit}. The change did not take.`
  );
}

/** The panel's one-line summary of a finished apply, for the toast and the status line. */
export function applySummary(outcome: ApplyOutcome, nodeName: string, who: string): string {
  switch (outcome.phase) {
    case 'confirmed':
      return `${nodeName} is now tuned to ${who}’s re-detected levels.`;
    case 'mismatch':
      return `${nodeName} did not take the new levels.`;
    case 'failed':
      return `Couldn’t write ${nodeName}’s levels.`;
    default:
      return `${nodeName}: ${outcome.phase}.`;
  }
}
