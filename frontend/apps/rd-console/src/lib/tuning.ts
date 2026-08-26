/**
 * Timer **tuning** — the pure half of the Tune page (#355, slice 2b).
 *
 * The Tune page is where an RD sets a timer's per-node enter/exit RSSI thresholds while standing
 * at (or walking back from) the gate. Everything in this module is the part of that with no DOM and
 * no I/O: the single clamp/round rule every editor funnels through, the write gate, the adapter
 * from a signal snapshot onto the `CompetitorTrace` shape `RssiGraph`'s live mode consumes, and the
 * per-threshold write lifecycle.
 *
 * ## The wire shape is IMPORTED, never re-declared
 *
 * `TimerSignal` / `NodeSignal` come from `@gridfpv/types` — the ts-rs bindings generated from
 * `crates/server/src/timers.rs`. This module previously hand-declared its own guesses at them while
 * the telemetry slice was unmerged, and every field name was wrong; `tsc` was perfectly happy, and
 * every readout on the page would have rendered `undefined` against a live Director. A hand-written
 * copy of a wire type does not fail loudly, so there is no version of it that is safe.
 *
 * ## The one rule this module exists to hold: clamp ONCE, at the state
 *
 * A threshold has **three editors** — a numeric box, a slider, and a draggable handle on the graph
 * — and they are three views of ONE value, not three values that sync. If each editor clamped and
 * rounded for itself, the box could hold `90.4` while the slider sat at `90` and the graph drew a
 * third position. So every editor hands its raw input to {@link clampLevel} and writes the result
 * to the single per-(node, threshold) state; nothing downstream clamps again.
 *
 * ## Why the range is 1..=254, and not 0..=255
 *
 * Both ends of the obvious 8-bit range are traps, and both fail the same way: they look accepted.
 *
 * RotorHazard's `calibration.py` tests `enter_at_level` for *truthiness*, so a `0` is read as
 * "leave it alone and read the level back off the node" rather than as the level zero. And
 * `Node.is_valid_rssi` is `value > 0 and value < max_rssi_value` (255) — a **strict** `<` — so a
 * `255` is dropped before it reaches the detector while RH's profile row, which is what gets
 * broadcast back, happily reports it.
 *
 * Either one silently no-ops while every readout on this page says the write landed — the #403
 * failure class, and the one thing a tuning page must never do. {@link RSSI_MIN} and
 * {@link RSSI_MAX} clamp them away at the only place a value can enter the state.
 */
import type { CalibrationRequest, ChannelRequest } from '@gridfpv/protocol-client';
import type {
  ChannelCapability,
  ChannelCatalogEntry,
  CompetitorTrace,
  HeatPhase,
  NodeSignal,
  TimerId,
  TimerNodes,
  TimerSignal
} from '@gridfpv/types';

import {
  channelLabel,
  channelOptionLabel,
  entryOptionLabel,
  isCatalogChannel,
  offeredCatalog
} from './channels.js';

// ── The value domain ────────────────────────────────────────────────────────────────────────────

/**
 * The lowest threshold a tune write may carry. **Not 0**: RotorHazard treats `enter_at_level == 0`
 * as falsy and re-reads the level off the node instead of setting it, so a typed `0` silently
 * no-ops (`calibration.py`). One below the real floor is worth nothing; a silent no-op costs a
 * tuning session.
 */
export const RSSI_MIN = 1;

/**
 * The highest threshold a tune write may carry. **Not 255**, and this is the same trap as
 * {@link RSSI_MIN} wearing the other hat.
 *
 * RotorHazard's `Node.is_valid_rssi` is `value > 0 and value < max_rssi_value`, with
 * `max_rssi_value == 255` at node API level ≥ 18 (verified in RH source on v4.3.0 and v4.4.0) —
 * note the **strict** `<`. So a literal 255 writes RH's profile row happily, is then silently
 * dropped by `RHInterface.set_enter_at_level` and never reaches the detector, and — because
 * `RHUI.emit_enter_and_exit_at_levels` serialises the **profile** rather than the node — comes back
 * on the next poll *confirming a value the detector does not hold*.
 *
 * That is the worst outcome this page can produce: a threshold reading `On timer` that is not on
 * the timer. One count of headroom is worth nothing; a false confirmation costs the RD the session.
 *
 * (The Director's own clamp stays at 255, matching the agreed route contract. This ceiling is the
 * one that has to be right, because it is the one a value passes through first.)
 */
export const RSSI_MAX = 254;

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

// ── The signal wire shape (the GENERATED bindings, not a guess) ─────────────────────────────────

/**
 * `GET /timers/{id}/signal` is **leased**, not a plain read: the first call opens the Director's
 * stream and every call renews it for `SIGNAL_LEASE` (`crates/server/src/timers.rs`). Stop polling
 * and the stream stops itself — which is what makes a closed tab or a dead network safe.
 *
 * This is the number the page's cadence has to stay inside. It is duplicated from the Rust
 * constant rather than derived, because the wire carries the *remaining* lease
 * ([`TimerSignal.lease_ms_remaining`]), never its length.
 */
export const SIGNAL_LEASE_MS = 5_000;

/**
 * The Tune page's poll cadence. Two jobs at once, and the tighter of the two wins: it feeds a
 * rolling plot (so it wants to be fast) **and** it is the only thing renewing the lease (so it must
 * be much faster than {@link SIGNAL_LEASE_MS} — a cadence that merely fits inside the lease would
 * drop the stream the first time one poll is slow).
 */
export const SIGNAL_POLL_MS = 250;

/**
 * Whether a poll cadence actually **holds** the lease, with room for a dropped poll or two.
 *
 * The margin is the point. A 4 s cadence technically renews a 5 s lease, but one slow answer and
 * the Director has already torn the stream down under a page that is still on screen; the RD sees
 * a plot stop moving for no reason they can see.
 */
export function holdsLease(pollMs: number, leaseMs: number = SIGNAL_LEASE_MS): boolean {
  return pollMs > 0 && pollMs * 3 <= leaseMs;
}

/**
 * Push one node's threshold(s) at the timer: `POST /timers/{id}/calibration`.
 *
 * **Nothing comes back but an acknowledgement.** RotorHazard does not echo a level set; it
 * broadcasts `enter_and_exit_at_levels`, which reaches this page as
 * [`NodeSignal.enter_at`]/[`NodeSignal.exit_at`] on the *next* poll. So the write resolves when the
 * Director accepted it, and the **confirmation is {@link foldPolled}** — see
 * {@link ThresholdPhase}.
 */
export type ApplyLevels = (timer: TimerId, body: CalibrationRequest) => Promise<void>;

/**
 * The outbound calibration body, re-exported so the page has one import site for the whole tuning
 * vocabulary. The definition is the ts-rs binding generated from the Director's own route
 * (`@gridfpv/types`), reached here via `@gridfpv/protocol-client`, which re-exports it beside the
 * call that sends it — so the page and the Director cannot disagree about the shape.
 */
export type { CalibrationRequest };

/** Poll one snapshot of a timer's live signal (`GET /timers/{id}/signal`) — and renew its lease. */
export type FetchSignal = (timer: TimerId, opts: { signal: AbortSignal }) => Promise<TimerSignal>;

/** End the timer's tuning stream now (`POST /timers/{id}/signal/stop`) — see {@link SIGNAL_LEASE_MS}. */
export type StopSignal = (timer: TimerId) => Promise<void>;

// ── Node identity + display ─────────────────────────────────────────────────────────────────────
//
// The seat's own name (`Node 1 · Raceband R7`) lives in `channels.ts` as `nodeSeatLabel` (#416):
// Live control, the Rounds & Heats stage and this page all label the same seat, and three copies of
// "node plus channel" is exactly the drift that put `node-6` on one screen and `Node 7` on another.

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
 * Whether a node's rolling window may be **plotted** — i.e. whether RotorHazard has reported this
 * node at all ([`NodeSignal.seen`]).
 *
 * This is not fussiness. The Director samples every node on the same pass and fills an unreported
 * node's slot with `0.0`, so an unseated or dead node arrives carrying a full, perfectly plottable
 * ring of zeroes. Drawn, that is a flat trace along the floor — indistinguishable from a live node
 * with no craft near it, and the RD is on this page precisely because they are trying to tell those
 * two apart. Unseen nodes are included in the snapshot deliberately ("is this node even alive?" is
 * half the diagnostic); the page's job is to render them as **dead**, not as quiet.
 */
export function plottable(node: NodeSignal | undefined): node is NodeSignal {
  return node !== undefined && node.seen;
}

/**
 * Adapt one node's snapshot onto the `{ competitors: [CompetitorTrace] }` shape `RssiGraph`'s live
 * mode consumes (slice 1). The trace's `enter`/`exit` are the levels the **timer** holds — the page
 * overlays the operator's in-hand value through the graph's `tuned` prop, so the plot draws what
 * the RD is doing while the trace keeps saying what the hardware is doing.
 *
 * One trace per graph, deliberately: the layout is one column *per node*, and `RssiGraph`'s `tuned`
 * prop addresses a single competitor — a column that owns its own single-trace graph gets the
 * threshold handles wired to exactly one (node, threshold) state with no dispatch in between.
 *
 * The trace is keyed on the node's **own** [`NodeSignal.seat`], never a locally re-spelled
 * `node-{i}`: the seat is what the heat's registration binds a pilot to, so re-deriving it here is
 * exactly the drift the repo display rule exists to prevent — and it takes the whole snapshot
 * because the sample time base is shared across every node, not carried per node.
 */
export function nodeTraceOf(signal: TimerSignal, node: NodeSignal): CompetitorTrace {
  const times = signal.sample_micros;
  return {
    competitor: { adapter: signal.timer, competitor: node.seat },
    from: times[0] ?? 0,
    period_micros: signal.period_micros > 0 ? signal.period_micros : 1,
    samples: node.samples,
    // The time base is SHARED and explicit — one axis for every node, because every node is sampled
    // in the same pass. Handing it to the graph as `times` means the plot is drawn against the
    // instants the Director actually stamped, not a `from + i·period` grid reconstructed from them.
    // They agree while the cadence is steady; when it slips, the explicit axis is the true one.
    times: times.length === node.samples.length ? times : undefined,
    enter: node.enter_at,
    exit: node.exit_at
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
 * `rssi` / `frequency_mhz` / `loop_time_micros` / `crossing`, so a page that read the peaks off the
 * heartbeat would render six permanent dashes.
 */
export function readoutsOf(node: NodeSignal | undefined): Readout[] {
  // Every field is optional on the wire and that is load-bearing: a node RotorHazard has not
  // reported, a timer whose thresholds have not arrived, a build that omits a readout. A dash is
  // information; a zero standing in for "not reported" is a lie the RD would tune against.
  // (The counts cross the wire as `f32`. They are integer ADC counts, so they are rendered as
  // integers rather than as whatever a float round-trip leaves behind.)
  const n = (v: number | undefined | null) =>
    v === undefined || v === null || !Number.isFinite(v) ? '—' : String(Math.round(v));
  return [
    { key: 'rssi', label: 'RSSI', value: n(node?.rssi) },
    { key: 'node-peak', label: 'Node peak', value: n(node?.node_peak_rssi) },
    { key: 'node-nadir', label: 'Node nadir', value: n(node?.node_nadir_rssi) },
    { key: 'pass-peak', label: 'Pass peak', value: n(node?.pass_peak_rssi) },
    { key: 'pass-nadir', label: 'Pass nadir', value: n(node?.pass_nadir_rssi) },
    { key: 'pass-count', label: 'Passes', value: n(node?.pass_count) }
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
 * How long a `sent` threshold may go unconfirmed by the poll before the page calls it **not taken**.
 *
 * Sized off the round trip it is waiting on, not off a feeling: the Director has to reach
 * RotorHazard, RH has to broadcast `enter_and_exit_at_levels`, and the change then has to survive
 * to the next `GET /signal`. Generous enough to ride out a slow LAN and a missed poll; short enough
 * that an RD standing at the gate is not left reading `Sending…` at a threshold that never landed.
 */
export const CONFIRM_TIMEOUT_MS = 3_000;

/**
 * Where one threshold stands relative to the hardware. There is no Apply button — an adjustment
 * goes to the timer the moment the interaction ends — so this **is** the confirmation the commit
 * step used to provide, and it has to be legible at a glance from arm's length.
 *
 *  - `confirmed` — a poll showed the timer holding this value. The resting state.
 *  - `pending`   — being dragged/typed right now; deliberately not yet written (a drag emits dozens
 *                  of values a second).
 *  - `sent`      — accepted by the Director, not yet seen on the timer.
 *  - `mismatch`  — the polls kept showing a *different* level. The write did not take.
 *  - `failed`    — the write itself errored: refused, unauthorised, or never arrived.
 *  - `refused`   — the write was not attempted: {@link writeGate} said no.
 *
 * ## The confirmation is a POLL, not a response
 *
 * `POST /timers/{id}/calibration` answers "accepted", nothing more. RotorHazard does not echo a
 * level set synchronously — it broadcasts `enter_and_exit_at_levels`, which reaches this page as
 * [`NodeSignal.enter_at`]/[`NodeSignal.exit_at`] on a **later** `GET /signal`. So `sent` is not
 * "waiting for the response" (that already came back); it is waiting for the signal feed the page
 * is already polling to show the new level. {@link foldPolled} is where that happens, and
 * {@link CONFIRM_TIMEOUT_MS} is how long it waits before saying so.
 */
export type ThresholdPhase = 'confirmed' | 'pending' | 'sent' | 'mismatch' | 'failed' | 'refused';

/** Everything one (node, threshold) tracks. `value` is the single state all three editors edit. */
export interface ThresholdState {
  /** THE value. Every editor reads this and every editor writes it through {@link clampLevel}. */
  value: number;
  /** The last level a poll showed the timer holding, or `undefined` before the first one. */
  confirmed?: number;
  /** Where this threshold stands against the hardware. */
  phase: ThresholdPhase;
  /** Why, when `phase` is `mismatch` / `failed` / `refused`. Rendered on the node, never swallowed. */
  detail?: string;
  /** The level the in-flight write asked for. Only meaningful while `phase` is `sent`. */
  sent?: number;
  /** When that write was accepted (ms), the clock {@link CONFIRM_TIMEOUT_MS} runs against. */
  sentAt?: number;
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
  // Clamp BEFORE comparing: the wire carries the levels as floats, so `90` and a reported `90.0`
  // (or a `90.4` the detector is sitting on) are the same threshold and must not churn the state.
  const value = clampLevel(level);
  if (state.phase !== 'confirmed' || state.confirmed === value) return state;
  return { value, confirmed: value, phase: 'confirmed' };
}

/**
 * Mark a threshold as written: the Director accepted the level, and the page is now waiting for a
 * **poll** to show the timer holding it. `sent`/`sentAt` are the two things {@link foldPolled}
 * needs to answer "did it take?" — what was asked for, and how long ago.
 */
export function markSent(state: ThresholdState, sent: number, now: number): ThresholdState {
  return { ...state, value: sent, phase: 'sent', detail: undefined, sent, sentAt: now };
}

/**
 * Fold what a **poll** reports into a threshold's state. This is the whole confirmation mechanism.
 *
 * At rest it is {@link adoptReported}: the hardware is the truth when the RD is not mid-edit.
 *
 * While a write is in flight (`sent`) it is the answer to "did it take?". RotorHazard does not echo
 * a level set, so the *only* evidence a write landed is the timer subsequently reporting that
 * level — which is why this is checked here and not against a response body. A level that matches
 * settles to `confirmed`; one that keeps disagreeing past `timeoutMs` becomes a **mismatch**, said
 * loudly on the node, because a silent divergence leaves the RD tuning against a value the hardware
 * never held (#403's failure class). In between it stays `sent` — the change may simply still be in
 * flight, and a mismatch declared one poll too early is a false alarm.
 *
 * There is deliberately no "previous value" kept: a threshold is not a destructive action, the
 * value is on screen and re-draggable, and an undo readout would be clutter on a column that
 * already carries three controls and six stats.
 */
export function foldPolled(
  state: ThresholdState,
  reported: number | undefined,
  now: number,
  timeoutMs: number = CONFIRM_TIMEOUT_MS
): ThresholdState {
  if (state.phase !== 'sent') {
    return reported === undefined ? state : adoptReported(state, reported);
  }
  const sent = state.sent;
  if (sent === undefined) return state;
  if (reported !== undefined && clampLevel(reported) === sent) {
    return { value: sent, confirmed: sent, phase: 'confirmed' };
  }
  if (now - (state.sentAt ?? now) < timeoutMs) return state;
  const holding = reported === undefined ? undefined : clampLevel(reported);
  return {
    value: state.value,
    confirmed: holding,
    phase: 'mismatch',
    detail:
      holding === undefined
        ? 'The timer is not reporting this threshold at all. The change did not take.'
        : `The timer reports ${holding}, not ${sent}. The change did not take.`
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

// ── The channel a node is listening on (#413) ───────────────────────────────────────────────────

/**
 * Push one node's **channel** at the timer: `POST /timers/{id}/channel`.
 *
 * The twin of {@link ApplyLevels}, and it resolves the same way: the Director answers with what it
 * *dispatched*, never a readback. The confirmation is {@link foldPolledChannel} seeing
 * [`NodeSignal.frequency_mhz`] come back holding what was sent.
 *
 * The one thing the answer is worth reading for is `thresholds_tuned_on_another_channel` — whether
 * the levels this page is showing were tuned while the node was on a different channel. The page
 * cannot know that on its own; the Director holds the record.
 */
export type ApplyChannel = (
  timer: TimerId,
  body: ChannelRequest
) => Promise<{ thresholds_tuned_on_another_channel?: boolean } | undefined | void>;

/** The outbound channel body, re-exported so the page has one import site (see {@link ApplyLevels}). */
export type { ChannelRequest };

/** Read a timer's node set (`GET /timers/{id}/nodes`, #412) — which gates exist and may be used. */
export type FetchNodes = (timer: TimerId) => Promise<TimerNodes>;

/** One entry in a node's channel dropdown: what it tunes to, and what the RD reads. */
export interface ChannelOption {
  /** The centre frequency in raw MHz — the option's `value`, and a wire handle only. */
  mhz: number;
  /** The catalog band, when this is a catalog channel (`"Raceband"`). */
  band?: string;
  /** The catalog channel label, when this is a catalog channel (`"R7"`). */
  channel?: string;
  /**
   * What the RD sees: `"Raceband R7"`, or `"5885 MHz"` for a custom entry with no catalog name.
   *
   * **Band and channel, never the frequency.** CLAUDE.md: the option `value` may stay the raw MHz
   * (it is a wire handle), the visible label may not — a bare number reaching a dropdown label is
   * the rule violation this repo has re-fixed most often. A catalog option is named from its own
   * entry so a coincident frequency keeps the band the RD picked (`HDZero R7`, not `Raceband R7`);
   * a custom one falls back through `channels.ts`'s {@link channelLabel}, whose `"5885 MHz"` is a
   * measurement rather than a number standing in for a name.
   */
  label: string;
  /** Whether this option is a **custom** raw MHz the RD added, rather than a catalog channel. */
  custom: boolean;
}

/**
 * The channels a node's dropdown offers, for a timer's capability.
 *
 * ## `available_channels` is NOT the source, and that is the whole point of this function
 *
 * Measured on the bench: the Mock reports `available_channels=[5658 … 5917]`, and **both** real
 * RotorHazard timers (Docker RH 4.3.0, NuclearHazard) report `channel_capability: "Flexible"` with
 * `available_channels=[]`. That empty list means *"no restriction"* — it is the **pool** per-heat
 * assignment allocates from, which an RD tuning at the bench has usually never configured — and
 * **not** *"no channels available"*. A dropdown bound naively to it renders empty on every real
 * RotorHazard, which is precisely the timer this feature exists for.
 *
 * This is the same shape of trap as #355's `seen` / zero-RSSI one: an empty-or-zero value that
 * reads as legitimate data instead of as "not applicable". So the source is the **capability**:
 *
 * - `Fixed { channels }` → exactly those channels (a limited module offers only what it supports);
 * - `Flexible` → the **whole catalog** (`GET /channels`, 52 entries).
 *
 * `custom` then adds the RD's own non-catalog entries **alongside** the catalog — the raw MHz they
 * typed into the timer's channel config, which is the only place `available_channels` legitimately
 * contributes, and only for the entries the catalog does not already know. Custom channels are a
 * Flexible-only idea (a Fixed timer supports what it supports), so they are ignored for a Fixed one.
 *
 * `current` is what the node is tuned to right now: it is appended if nothing above already offers
 * it, so the dropdown can always show the node's actual channel rather than silently selecting some
 * other option. Catalog order is preserved; custom entries follow, ascending.
 */
export function channelOptions(
  capability: ChannelCapability | undefined,
  catalog: ChannelCatalogEntry[],
  custom: number[] = [],
  current?: number
): ChannelOption[] {
  // ONE ROW PER FREQUENCY. `5880` is Raceband R7 and Fatshark F8 — the same channel, reachable by
  // two names. Offering both as rows shows the RD the same frequency twice with nothing to say
  // which is "the" one; the first-listed band wins (the catalog leads with Raceband, the de-facto
  // racing default) and the other names ride along in the label's parentheses, so a pilot who
  // knows their VTX as F8 still finds it.
  const seenMhz = new Set<number>();
  const options: ChannelOption[] = [];
  for (const entry of offeredCatalog(capability, catalog)) {
    if (seenMhz.has(entry.mhz)) continue;
    seenMhz.add(entry.mhz);
    options.push({
      mhz: entry.mhz,
      band: entry.band,
      channel: entry.channel,
      label: entryOptionLabel(entry, catalog),
      custom: false
    });
  }
  // A Fixed timer offers its declared set and nothing else — a custom MHz it cannot tune to is not
  // an option, it is a refusal waiting to happen.
  const flexible = capabilityIsFlexible(capability);
  const extras = new Set<number>();
  if (flexible) {
    for (const mhz of custom) {
      if (!isCatalogChannel(mhz, catalog)) extras.add(mhz);
    }
  }
  // What the node is ON always appears, even if it is off-catalog and off-pool: a dropdown that
  // cannot show the current value would quietly display some other channel as if it were selected.
  if (current !== undefined && !options.some((o) => o.mhz === current) && !extras.has(current)) {
    if (flexible || capabilityAllows(capability, current)) extras.add(current);
  }
  for (const mhz of [...extras].sort((a, b) => a - b)) {
    options.push({
      mhz,
      label: channelOptionLabel(mhz, catalog),
      custom: !isCatalogChannel(mhz, catalog)
    });
  }
  return options;
}

/** Whether a capability is the permissive `Flexible` one (the default for an undeclared timer). */
function capabilityIsFlexible(capability: ChannelCapability | undefined): boolean {
  return !(capability && typeof capability === 'object' && 'Fixed' in capability);
}

/** Whether a capability permits `mhz` — anything for Flexible, the declared set for Fixed. */
function capabilityAllows(capability: ChannelCapability | undefined, mhz: number): boolean {
  if (capabilityIsFlexible(capability)) return true;
  const cap = capability as { Fixed: { channels: number[] } };
  return cap.Fixed.channels.includes(mhz);
}

/**
 * Which node indices a channel may be **offered** for, from the Director's own node view (#412).
 *
 * RotorHazard validates `0 <= node_index < num_nodes` and otherwise just writes a log line — so a
 * channel write to a node that does not exist looks accepted and lands nowhere. And a node the RD
 * has **disabled** is one no heat is ever seated on, so retuning it is at best pointless.
 *
 * **Fails closed.** With no node view yet (the read has not landed, or it failed) this is empty and
 * the page offers no channel control at all — better a control that appears a beat late than one
 * that offers a gate the hardware does not have. The Director refuses those writes anyway; this is
 * so the RD is never offered the choice in the first place.
 */
export function offerableNodes(view: TimerNodes | undefined): Set<number> {
  return new Set(view?.enabled ?? []);
}

/**
 * Where one node's channel stands relative to the hardware — the same six phases a threshold has
 * ({@link ThresholdPhase}), because it is the same kind of write with the same kind of proof.
 *
 * The confirmation is a **poll**, again: `POST /timers/{id}/channel` says "accepted", and the
 * channel itself comes back as [`NodeSignal.frequency_mhz`] on a later `GET /signal` (every
 * RotorHazard heartbeat carries it). A channel that never comes back holding the value the RD
 * picked is a write that did not take, and the node has to say so.
 */
export interface ChannelState {
  /** THE value — what the dropdown shows, and the only place it is held. */
  mhz: number;
  /** The last frequency a poll showed the node on, or `undefined` before the first one. */
  confirmed?: number;
  /** Where this channel stands against the hardware. */
  phase: ThresholdPhase;
  /** Why, when `phase` is `mismatch` / `failed` / `refused`. Rendered on the node, never swallowed. */
  detail?: string;
  /** The channel the in-flight write asked for. Only meaningful while `phase` is `sent`. */
  sent?: number;
  /** When that write was accepted (ms), the clock {@link CONFIRM_TIMEOUT_MS} runs against. */
  sentAt?: number;
  /**
   * The channel this node's enter/exit thresholds were tuned on, once a channel change has left
   * them behind. `undefined` while nothing has moved. See {@link staleThresholdNote}.
   */
  tunedOn?: number;
}

/** A fresh channel state seeded from what the node reports being tuned to. */
export function seedChannel(mhz: number): ChannelState {
  return { mhz, confirmed: mhz, phase: 'confirmed' };
}

/**
 * Mark a channel as written and record what the thresholds were tuned on, so the node can say the
 * levels are now stale. `tunedOn` is carried forward, not overwritten: two channel changes in a row
 * leave the thresholds tuned on whatever they were *first* tuned on, which is the honest answer.
 */
export function markChannelSent(
  state: ChannelState,
  sent: number,
  now: number,
  stale: boolean
): ChannelState {
  return {
    ...state,
    mhz: sent,
    phase: 'sent',
    detail: undefined,
    sent,
    sentAt: now,
    tunedOn: stale ? (state.tunedOn ?? state.confirmed ?? state.mhz) : state.tunedOn
  };
}

/**
 * Fold what a poll reports into a channel's state — {@link foldPolled}'s twin, and the whole
 * confirmation mechanism for a channel.
 *
 * At rest the hardware is the truth: a heat that starts **legitimately retunes every node** to its
 * assigned channel, and this page must follow that rather than fight it. In flight it is the answer
 * to "did it take?": the value coming back settles it, a different one that persists past
 * `timeoutMs` is a mismatch said loudly on the node, and in between it stays `sent`.
 *
 * `reported === undefined` is *"the node is tuned to nothing"* (RotorHazard reports `0`, which the
 * adapter turns into an absence rather than a 0 MHz channel) — never silently adopted as a value.
 */
export function foldPolledChannel(
  state: ChannelState,
  reported: number | undefined,
  now: number,
  catalog: ChannelCatalogEntry[],
  timeoutMs: number = CONFIRM_TIMEOUT_MS
): ChannelState {
  if (state.phase !== 'sent') {
    if (reported === undefined || state.phase !== 'confirmed' || state.confirmed === reported) {
      return state;
    }
    // The hardware moved under us — a heat's channel assignment, or the RD retuning in RH's own UI.
    return { ...state, mhz: reported, confirmed: reported, phase: 'confirmed', detail: undefined };
  }
  const sent = state.sent;
  if (sent === undefined) return state;
  if (reported === sent) {
    return { ...state, mhz: sent, confirmed: sent, phase: 'confirmed', sent: undefined };
  }
  if (now - (state.sentAt ?? now) < timeoutMs) return state;
  return {
    ...state,
    confirmed: reported,
    phase: 'mismatch',
    detail:
      reported === undefined
        ? 'The timer is not reporting a channel on this node at all. The change did not take.'
        : // Both channels named the way the RD reads them (CLAUDE.md) — a message about a channel
          // that spells it as a bare number is exactly the leak the display rule exists to stop.
          `The node is still on ${channelLabel(reported, catalog)}, not ${channelLabel(
            sent,
            catalog
          )}. The change did not take.`
  };
}

/**
 * What to say on a node whose channel just changed: **the thresholds were tuned on a different
 * channel**, and nothing else changed.
 *
 * RotorHazard's `on_set_frequency` writes the frequency into the **current profile** — the same row
 * that holds `enter_ats` / `exit_ats`. So moving a node's channel leaves its thresholds exactly
 * where they were, tuned for the frequency it just left, and nothing announces it: the levels read
 * unchanged and therefore fine, while the gate now detects on numbers never calibrated for the
 * channel it is on.
 *
 * Deliberately **factual, not alarming**. It is not necessarily wrong — thresholds often carry
 * across channels perfectly well — it is simply unverified, and the RD is the one standing at the
 * gate who can check it in ten seconds. (Recalling saved per-channel levels is #411; this states
 * the situation rather than pre-empting it.)
 */
export function staleThresholdNote(
  tunedOn: number,
  now: number,
  catalog: ChannelCatalogEntry[]
): string {
  return (
    `These enter and exit levels were tuned on ${channelLabel(tunedOn, catalog)}. ` +
    `This node is now on ${channelLabel(now, catalog)} and the levels are unchanged — ` +
    `fly a pass to check they still bracket it.`
  );
}

/**
 * The nodes sharing a channel with another node (#413) — a real mistake worth flagging.
 *
 * **Flagged, never blocked.** Two nodes on one frequency both see the same craft, which will
 * double-count a pass in a race; but it is also exactly what a bench swap looks like halfway
 * through, and refusing it would block the legitimate case to prevent a recoverable one.
 *
 * `byNode` maps each node index to the channel it is *effectively* on — the value the RD just
 * picked if a write is in flight, else what the timer reports. Nodes with no channel are ignored:
 * two nodes tuned to nothing are not a clash.
 */
export function duplicateChannelNodes(byNode: Map<number, number | undefined>): Set<number> {
  const seen = new Map<number, number[]>();
  for (const [node, mhz] of byNode) {
    if (mhz === undefined) continue;
    const nodes = seen.get(mhz);
    if (nodes) nodes.push(node);
    else seen.set(mhz, [node]);
  }
  const clashing = new Set<number>();
  for (const nodes of seen.values()) {
    if (nodes.length > 1) for (const node of nodes) clashing.add(node);
  }
  return clashing;
}

/** What a node says when it shares its channel with others — by their friendly, 1-based names. */
export function duplicateChannelNote(node: number, sharing: number[]): string {
  const others = sharing
    .filter((n) => n !== node)
    .map((n) => `Node ${n + 1}`)
    .join(', ');
  return (
    `${others} ${sharing.length > 2 ? 'are' : 'is'} on this channel too. ` +
    `Two gates on one frequency both see the same craft — fine while you are swapping, wrong for a race.`
  );
}

/**
 * The small print beside the channel control: **a heat will overwrite this, and that is correct.**
 *
 * Channel set here is a bench setting. Heat setup allocates channels and re-tunes every node when a
 * heat stages, which legitimately replaces whatever was picked on this page — so an RD who tunes
 * node 1 to R7 and then starts a heat must not be surprised, and the page must not try to make the
 * tune-page value win. Saying so is the whole fix.
 */
export const HEAT_OVERWRITES_CHANNEL =
  'Channel here is a bench setting. Starting a heat re-tunes every node to that heat’s assigned ' +
  'channel, which replaces what you pick here.';

/**
 * Whether a **channel** change may go to the timer right now — {@link writeGate}'s rule, said in
 * the words of what a channel change actually does.
 *
 * Deliberately delegates rather than restating the rule: one predicate decides when a Tune-page
 * write is refused, so the channel dropdown and the threshold sliders can never disagree about
 * whether a heat is protected. Only the reason differs, because retuning a receiver mid-race is a
 * different kind of wrong from moving a threshold — it takes the gate off the channel the pilot is
 * flying, rather than changing what counts as a lap.
 */
export function channelGate(
  phase: HeatPhase | undefined,
  heatKind: 'practice' | 'competition' | undefined
): WriteGate {
  const gate = writeGate(phase, heatKind);
  if (gate.allowed) return gate;
  return {
    allowed: false,
    reason:
      'A competition heat is on this timer — retuning a node now would take its gate off the channel the pilot is flying. Channel changes resume when the heat ends.'
  };
}
