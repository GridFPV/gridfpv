/**
 * Timer **tuning** — the pure half of the Tune page (#355, slice 2b).
 *
 * The Tune page is where an RD sets a timer's per-node enter/exit RSSI thresholds while standing
 * at (or walking back from) the gate. Everything in this module is the part of that with no DOM and
 * no I/O: the single clamp/round rule every editor funnels through, the write gate, the adapter
 * from a signal snapshot onto the `CompetitorTrace` shape `RssiGraph`'s live mode consumes, and the
 * per-threshold write lifecycle.
 *
 * ## The one rule this module exists to hold: clamp ONCE, at the state
 *
 * A threshold has **three editors** — a numeric box, a slider, and a draggable handle on the graph
 * — and they are three views of ONE value, not three values that sync. If each editor clamped and
 * rounded for itself, the box could hold `90.4` while the slider sat at `90` and the graph drew a
 * third position. So every editor hands its raw input to {@link clampLevel} and writes the result
 * to the single per-(node, threshold) state; nothing downstream clamps again.
 *
 * ## Why the minimum is 1 and not 0
 *
 * RotorHazard's `calibration.py` tests `enter_at_level` for *truthiness*, so a `0` is read as
 * "leave it alone and read the level back off the node" rather than as the level zero. A typed `0`
 * would therefore look accepted and silently no-op — the #403 failure class. {@link RSSI_MIN}
 * clamps it away at the only place a value can enter the state.
 */
import type {
  ChannelCatalogEntry,
  CompetitorRef,
  CompetitorTrace,
  HeatPhase,
  TimerId
} from '@gridfpv/types';

import { channelLabel } from './channels.js';

// ── The value domain ────────────────────────────────────────────────────────────────────────────

/**
 * The lowest threshold a tune write may carry. **Not 0**: RotorHazard treats `enter_at_level == 0`
 * as falsy and re-reads the level off the node instead of setting it, so a typed `0` silently
 * no-ops (`calibration.py`). One below the real floor is worth nothing; a silent no-op costs a
 * tuning session.
 */
export const RSSI_MIN = 1;

/** The highest threshold a tune write may carry — RSSI is an 8-bit filtered ADC count on RH. */
export const RSSI_MAX = 255;

/**
 * The single clamp/round for a threshold level, applied at the **state** and nowhere else.
 *
 * Accepts whatever an editor produces — a number from a slider, a string from a numeric box, a
 * fractional value from a pointer-to-RSSI projection — and yields the one integer all three
 * editors then render. Un-parseable input resolves to `fallback` (default {@link RSSI_MIN}) rather
 * than `NaN`, because a `NaN` in the state would poison every view of it at once.
 */
export function clampLevel(value: unknown, fallback: number = RSSI_MIN): number {
  let n: number;
  if (typeof value === 'number') {
    n = value;
  } else {
    // `Number('')` and `Number(null)` are both 0, which would clamp an EMPTY box to the minimum
    // rather than leaving the state alone — so blank input is un-parseable here, not zero.
    const text = String(value ?? '').trim();
    if (text === '') return fallback;
    n = Number(text);
  }
  if (!Number.isFinite(n)) return fallback;
  return Math.min(RSSI_MAX, Math.max(RSSI_MIN, Math.round(n)));
}

/** Whether a raw editor input is a number at all (an empty / half-typed box is not). */
export function isParsableLevel(raw: string): boolean {
  const t = raw.trim();
  return t !== '' && Number.isFinite(Number(t));
}

/** Which of a node's two thresholds an editor is editing. */
export type Threshold = 'enter' | 'exit';

/** Both of a node's thresholds, the pair a write and a readback carry. */
export interface Levels {
  enter: number;
  exit: number;
}

// ── The signal snapshot (ASSUMED SHAPE — reconcile with slice 2a) ───────────────────────────────

/**
 * ⚠️ **Assumed wire shape.** Slice 2a owns the telemetry pipeline and `GET /timers/{id}/signal`;
 * it was not merged when this page was built, so this is the shape the page was built against and
 * the one to reconcile when 2a lands. Everything the page needs is derived through
 * {@link nodeTraceOf} / {@link readoutsOf}, so a differently-named 2a payload is adapted **here**
 * and in the page's fetch seam — never inside `RssiGraph`.
 *
 * One node's slice of a timer's live signal: the last-value-wins heartbeat fields, the ~2 Hz
 * `node_data` readouts, the calibration levels the timer currently holds, and a bounded ring of
 * recent RSSI samples for the rolling plot.
 */
export interface TimerSignalNode {
  /** The 0-based timer seat this is (the same index `available_channels` is ordered by). */
  node: number;
  /** `heartbeat.frequency` — the raw MHz this node is tuned to. Display-only via `channels.ts`. */
  frequency?: number;
  /** `heartbeat.current_rssi` — the instantaneous level. */
  current_rssi?: number;
  /** `heartbeat.crossing_flag` — whether the node believes a craft is in the gate right now. */
  crossing_flag?: boolean;
  /** The enter threshold the **timer** currently holds (the readback truth, not our pending edit). */
  enter_at_level?: number;
  /** The exit threshold the **timer** currently holds. */
  exit_at_level?: number;
  /** `node_data.node_peak_rssi` — the highest level this node has seen since it was reset. */
  node_peak_rssi?: number;
  /** `node_data.node_nadir_rssi` — the lowest. */
  node_nadir_rssi?: number;
  /** `node_data.pass_peak_rssi` — the peak of the most recent gate pass. */
  pass_peak_rssi?: number;
  /** `node_data.pass_nadir_rssi` — the nadir between passes. */
  pass_nadir_rssi?: number;
  /** `node_data.debug_pass_count` — how many passes this node has called. */
  debug_pass_count?: number;
  /** The bounded ring of recent RSSI samples, **oldest first**. */
  samples: number[];
  /** The source-clock instant (µs) of `samples[0]`. */
  from?: number;
  /** Microseconds between consecutive samples (the Director-decimated cadence). */
  period_micros: number;
}

/** ⚠️ Assumed wire shape (see {@link TimerSignalNode}): one poll of `GET /timers/{id}/signal`. */
export interface TimerSignal {
  timer: TimerId;
  /** The server-clock instant (µs) this snapshot was taken, when the Director stamps one. */
  at?: number;
  /** One entry per node the timer reports, in seat order. */
  nodes: TimerSignalNode[];
}

/**
 * ⚠️ Assumed wire shape: the answer to a calibration write. `set_enter_at_level` does **not** echo
 * (verified on RH 4.3.0), so the Director must read the levels back off the node and return what it
 * actually found — that readback is the only thing that can confirm a write landed.
 */
export interface CalibrationReadback {
  node: number;
  enter_at_level: number;
  exit_at_level: number;
}

/**
 * Push one node's threshold(s) at the timer and read the levels back.
 *
 * ⚠️ **Assumed wire shape** (see {@link TimerSignalNode}). Only the threshold that actually changed
 * is sent; the response carries **both** levels read back off the node, because RotorHazard does
 * not echo `set_enter_at_level` and the readback is therefore the only evidence a write landed.
 */
export type ApplyLevels = (
  timer: TimerId,
  node: number,
  levels: { enter_at_level?: number; exit_at_level?: number }
) => Promise<CalibrationReadback>;

/** Poll one snapshot of a timer's live signal. ⚠️ Assumed wire shape (see {@link TimerSignal}). */
export type FetchSignal = (timer: TimerId, opts: { signal: AbortSignal }) => Promise<TimerSignal>;

// ── Node identity + display ─────────────────────────────────────────────────────────────────────

/**
 * The competitor ref a node seat plots under: `node-{i}`, the same handle open-practice heats use,
 * so `createCompetitorNameResolver` resolves a staged seat to its pilot callsign with no special
 * case here.
 */
export function nodeRefOf(node: number): CompetitorRef {
  return `node-${node}`;
}

/**
 * The display label for one node seat: `Node 1 · Raceband R7`.
 *
 * The **channel half goes through `channels.ts`'s {@link channelLabel}** — a bare `5880` reaching
 * the screen is a rule violation (CLAUDE.md), and re-deriving band+channel here is how resolvers
 * drift. A node with no frequency yet (the timer has not reported one, or the seat is beyond the
 * configured pool) is just `Node 1` — the seat number is a position on the physical timer, not an
 * id standing in for a name, so it is the friendly name here.
 *
 * `frequency` should be the **live** heartbeat frequency when the snapshot carries one, falling
 * back to the registry's `available_channels[node]` — what the node is tuned to right now beats
 * what it was configured to.
 */
export function nodeTuneLabel(
  node: number,
  frequency: number | undefined,
  catalog: ChannelCatalogEntry[]
): string {
  const seat = `Node ${node + 1}`;
  if (frequency === undefined) return seat;
  return `${seat} · ${channelLabel(frequency, catalog)}`;
}

/**
 * How many node columns to show for a timer: what the signal snapshot actually reports, falling
 * back to the registry's `node_count` before the first poll lands (so the page lays out
 * immediately rather than popping in).
 */
export function nodeCountOf(signal: TimerSignal | undefined, declared: number): number {
  const reported = signal?.nodes.length ?? 0;
  return reported > 0 ? reported : Math.max(0, declared);
}

// ── The live plot adapter ───────────────────────────────────────────────────────────────────────

/**
 * Adapt one node's snapshot onto the `{ competitors: [CompetitorTrace] }` shape `RssiGraph`'s live
 * mode consumes (slice 1). The trace's `enter`/`exit` are the levels the **timer** holds — the page
 * overlays the operator's in-hand value through the graph's `tuned` prop, so the plot draws what
 * the RD is doing while the trace keeps saying what the hardware is doing.
 *
 * One trace per graph, deliberately: the layout is one column *per node*, and `RssiGraph`'s `tuned`
 * prop addresses a single competitor — a column that owns its own single-trace graph gets the
 * threshold handles wired to exactly one (node, threshold) state with no dispatch in between.
 */
export function nodeTraceOf(timer: TimerId, node: TimerSignalNode): CompetitorTrace {
  return {
    competitor: { adapter: timer, competitor: nodeRefOf(node.node) },
    from: node.from ?? 0,
    period_micros: node.period_micros > 0 ? node.period_micros : 1,
    samples: node.samples,
    enter: node.enter_at_level,
    exit: node.exit_at_level
  };
}

/** One labelled numeric readout under a node's plot. */
export interface Readout {
  /** The stable key/testid stem. */
  key: string;
  /** The human label, spelled as RotorHazard's own tuning page spells it. */
  label: string;
  /** The value, or `'—'` when the timer has not reported one yet. */
  value: string;
}

/**
 * The six readouts under a node's plot, in the order the RD listed them. **All but `RSSI` come from
 * RotorHazard's `node_data` frame, not the heartbeat** — `get_heartbeat_json` carries only
 * `current_rssi` / `frequency` / `loop_time` / `crossing_flag`, so a page that read the peaks off
 * the heartbeat would render six permanent dashes.
 */
export function readoutsOf(node: TimerSignalNode | undefined): Readout[] {
  const n = (v: number | undefined) => (v === undefined || v === null ? '—' : String(v));
  return [
    { key: 'rssi', label: 'RSSI', value: n(node?.current_rssi) },
    { key: 'node-peak', label: 'Node peak', value: n(node?.node_peak_rssi) },
    { key: 'node-nadir', label: 'Node nadir', value: n(node?.node_nadir_rssi) },
    { key: 'pass-peak', label: 'Pass peak', value: n(node?.pass_peak_rssi) },
    { key: 'pass-nadir', label: 'Pass nadir', value: n(node?.pass_nadir_rssi) },
    { key: 'pass-count', label: 'Passes', value: n(node?.debug_pass_count) }
  ];
}

// ── The write gate ──────────────────────────────────────────────────────────────────────────────

/** The answer to "may this adjustment be written to the timer right now?". */
export type WriteGate = { allowed: true } | { allowed: false; reason: string };

/** The one allowed gate. */
const ALLOWED: WriteGate = { allowed: true };

/**
 * Whether a calibration write may go to the timer **right now** (#398).
 *
 * Writing a threshold mid-heat rewrites what the gate counts as a lap, so it is refused while a
 * **competition** heat is on the timer. It is *allowed* during **open practice**: practice is
 * explicitly excluded from scoring, so there is no result to corrupt — and pilots in the air is the
 * natural moment to tune, which is the whole reason the page exists.
 *
 * With **no heat on the timer** (idle, scheduled, or finished) there is nothing to protect and the
 * write goes through: that is the ordinary case, an RD tuning a timer before an event exists.
 *
 * Checked **per write**, never once at page load: with no commit button every adjustment is a
 * write, and a heat that goes `Running` while the RD is at the gate has to start refusing mid-
 * session. (`heatKind` is the caller's join of the current heat → its round → open-practice or not;
 * `undefined` means "no heat on the timer".)
 */
export function writeGate(
  phase: HeatPhase | undefined,
  heatKind: 'practice' | 'competition' | undefined
): WriteGate {
  // Only a *running* heat is at risk. Staged/Armed are pre-race and Unofficial/Final are past it;
  // in every one of those the detector is not deciding laps that count right now.
  if (phase !== 'Running' || heatKind === undefined) return ALLOWED;
  if (heatKind === 'practice') return ALLOWED;
  return {
    allowed: false,
    reason:
      'A competition heat is running — changing a gate threshold now would change which laps it counts. Tuning resumes when the heat ends.'
  };
}

// ── The per-threshold write lifecycle ───────────────────────────────────────────────────────────

/**
 * Where one threshold stands relative to the hardware. There is no Apply button — an adjustment
 * goes to the timer the moment the interaction ends — so this **is** the confirmation the commit
 * step used to provide, and it has to be legible at a glance from arm's length.
 *
 *  - `confirmed` — a readback matched: the timer holds this value. The resting state.
 *  - `pending`   — being dragged/typed right now; deliberately not yet written (a drag emits dozens
 *                  of values a second, and each write costs a readback).
 *  - `sent`      — written, readback outstanding.
 *  - `mismatch`  — the readback came back holding a *different* level. The write did not take.
 *  - `failed`    — the write or its readback errored / never arrived.
 *  - `refused`   — the write was not attempted: {@link writeGate} said no.
 */
export type ThresholdPhase = 'confirmed' | 'pending' | 'sent' | 'mismatch' | 'failed' | 'refused';

/** Everything one (node, threshold) tracks. `value` is the single state all three editors edit. */
export interface ThresholdState {
  /** THE value. Every editor reads this and every editor writes it through {@link clampLevel}. */
  value: number;
  /** The last level a readback confirmed the timer holds, or `undefined` before the first poll. */
  confirmed?: number;
  /** Where this threshold stands against the hardware. */
  phase: ThresholdPhase;
  /** Why, when `phase` is `mismatch` / `failed` / `refused`. Rendered on the node, never swallowed. */
  detail?: string;
}

/**
 * A fresh threshold state seeded from the level the timer is currently reporting. Seeding only
 * happens once a level *has* been reported: a column with no levels yet renders as "waiting", never
 * as a control sitting on a made-up default the RD might drag away from without noticing.
 */
export function seedThreshold(level: number): ThresholdState {
  const value = clampLevel(level);
  return { value, confirmed: value, phase: 'confirmed' };
}

/**
 * Adopt a level the **timer** reports while this threshold is at rest — the RD tuned in
 * RotorHazard's own UI, a profile switched, or the timer reconnected holding something else. The
 * hardware is the truth when we are not mid-edit, so the value follows it.
 *
 * Deliberately a no-op for any non-`confirmed` phase: an adjustment in the RD's hand, a write in
 * flight, or a failure they have not read yet must not be overwritten by the next poll.
 */
export function adoptReported(state: ThresholdState, level: number): ThresholdState {
  if (state.phase !== 'confirmed' || state.confirmed === level) return state;
  const value = clampLevel(level);
  return { value, confirmed: value, phase: 'confirmed' };
}

/**
 * Fold a readback into a threshold's state: `confirmed` becomes what the hardware actually holds,
 * and the phase says whether the write took.
 *
 * `sent` is the level the page asked for. A readback that disagrees is a **mismatch**, not a
 * success with a surprise — RotorHazard does not echo `set_enter_at_level`, so this comparison is
 * the only evidence a write landed at all.
 *
 * There is deliberately no "previous value" kept: a threshold is not a destructive action, the
 * value is on screen and re-draggable, and an undo readout would be clutter on a column that
 * already carries three controls and six stats.
 */
export function foldReadback(
  state: ThresholdState,
  sent: number,
  readback: number
): ThresholdState {
  if (readback === sent) {
    return { value: readback, confirmed: readback, phase: 'confirmed' };
  }
  return {
    value: state.value,
    confirmed: readback,
    phase: 'mismatch',
    detail: `The timer reports ${readback}, not ${sent}. The change did not take.`
  };
}

/** A short arm's-length label for a phase, for the badge beside each threshold. */
export function phaseLabel(phase: ThresholdPhase): string {
  switch (phase) {
    case 'confirmed':
      return 'On timer';
    case 'pending':
      return 'Adjusting';
    case 'sent':
      return 'Sending…';
    case 'mismatch':
      return 'Not taken';
    case 'failed':
      return 'Failed';
    case 'refused':
      return 'Not sent';
  }
}

/** The `Badge`/pill tone a phase reads as: settled, in-flight, or wrong. */
export function phaseTone(phase: ThresholdPhase): 'success' | 'info' | 'warn' | 'danger' {
  switch (phase) {
    case 'confirmed':
      return 'success';
    case 'pending':
    case 'sent':
      return 'info';
    case 'refused':
      return 'warn';
    case 'mismatch':
    case 'failed':
      return 'danger';
  }
}
