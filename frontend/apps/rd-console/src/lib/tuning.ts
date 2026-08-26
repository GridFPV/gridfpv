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
import type { CalibrationRequest } from '@gridfpv/protocol-client';
import type { CompetitorTrace, HeatPhase, NodeSignal, TimerId, TimerSignal } from '@gridfpv/types';

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
