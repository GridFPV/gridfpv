/**
 * The **leased** timer-signal subscription, in one place (#415).
 *
 * `GET /timers/{id}/signal` is not a read, it is a subscription: the Director streams a timer's
 * telemetry only while somebody is looking, every `GET` renews a ~5 s lease, and the stream stops
 * by itself once the calls stop. That makes the poll cadence load-bearing — it is the thing keeping
 * the feed alive, which is why it sits an order of magnitude inside {@link SIGNAL_LEASE_MS} — and
 * it makes the release obligatory: `POST /signal/stop` on every path that ends the watch (unmount,
 * a route change, and the tab being hidden), because "the RD walked to the gate with the phone in
 * their pocket" must not leave a timer parsing telemetry into nobody's screen. The lease is the
 * backstop for a stop that never lands (a killed tab, a dead network); it is not the plan.
 *
 * Both watchers run the same bargain — the Tune page (#355) and Race control's read-only gate strip
 * (#415) — so it is implemented once here rather than copied. Two copies of "hold the lease, then
 * give it back" is exactly the shape of bug where one of them quietly stops releasing.
 */
import type { TimerId, TimerSignal } from '@gridfpv/types';

import { SIGNAL_POLL_MS, type FetchSignal, type StopSignal } from './tuning.js';

/** A live view of one timer's signal subscription. */
export interface SignalFeed {
  /** The newest snapshot, or `undefined` before the first one lands. */
  readonly snapshot: TimerSignal | undefined;
  /** The last poll failure — the DIRECTOR did not answer, so nothing on screen is current. */
  readonly error: string | undefined;
  /** Whether a first snapshot has ever landed — distinguishes "connecting" from "no nodes". */
  readonly everLoaded: boolean;
  /**
   * Whether a live connection is actually feeding the snapshot. `false` with a perfectly valid
   * snapshot is **no link** (the timer is not connected, or just dropped) as against a live feed
   * over a quiet gate — opposite faults, opposite fixes.
   */
  readonly streaming: boolean;
}

/** What a caller has to supply to hold a subscription. */
export interface SignalFeedOptions {
  /** The timer to watch. `undefined` watches nothing (and releases anything already held). */
  timer: () => TimerId | undefined;
  /** The poll — defaults to the session's `GET /timers/{id}/signal`; a test swaps it here. */
  read: () => FetchSignal;
  /** The release — defaults to the session's `POST /timers/{id}/signal/stop`. */
  release: () => StopSignal;
  /** Poll cadence (ms). Defaults to {@link SIGNAL_POLL_MS}, which holds the lease with margin. */
  pollMs?: () => number;
  /**
   * Fold each snapshot into the caller's own state, in poll order. The Tune page confirms its
   * in-flight writes here; the gate strip needs nothing and omits it.
   */
  onsnapshot?: (snap: TimerSignal) => void;
}

/**
 * Subscribe to a timer's live signal for as long as the calling component is mounted **and the tab
 * is visible**, and release it the moment either stops being true.
 *
 * Must be called during component initialisation — it owns an `$effect` for the poll cadence and
 * the `visibilitychange` listener, and its teardown is what fires the release.
 */
export function useSignalFeed(opts: SignalFeedOptions): SignalFeed {
  let snapshot = $state.raw<TimerSignal | undefined>(undefined);
  let error = $state<string | undefined>(undefined);
  let everLoaded = $state(false);

  let poll: ReturnType<typeof setInterval> | undefined;
  let inflight: AbortController | undefined;

  async function pollOnce(id: TimerId): Promise<void> {
    inflight?.abort();
    const ctl = new AbortController();
    inflight = ctl;
    try {
      const snap = await opts.read()(id, { signal: ctl.signal });
      if (ctl.signal.aborted) return;
      snapshot = snap;
      opts.onsnapshot?.(snap);
      error = undefined;
      everLoaded = true;
    } catch (e) {
      if (ctl.signal.aborted) return;
      error = e instanceof Error ? e.message : String(e);
    } finally {
      if (inflight === ctl) inflight = undefined;
    }
  }

  function startPolling(id: TimerId, every: number): void {
    if (poll !== undefined) return;
    void pollOnce(id);
    poll = setInterval(() => void pollOnce(id), every);
  }

  /**
   * Stop watching `id`: end the cadence, abandon anything in flight, and **tell the Director** so
   * the stream stops now rather than when the lease runs out.
   *
   * Fire-and-forget on purpose. It runs from teardown, where there is nobody left to show an error
   * to and nothing useful to do about one — and the lease already guarantees the outcome if it
   * never arrives. Not firing it at all is the thing that would be wrong.
   */
  function stopWatching(id: TimerId): void {
    if (poll !== undefined) {
      clearInterval(poll);
      poll = undefined;
    }
    inflight?.abort();
    inflight = undefined;
    void opts
      .release()(id)
      .catch(() => {});
  }

  $effect(() => {
    const id = opts.timer();
    if (id === undefined) return;
    const every = opts.pollMs?.() ?? SIGNAL_POLL_MS;
    const doc = typeof document === 'undefined' ? undefined : document;
    const sync = () => {
      if (doc?.visibilityState === 'hidden') stopWatching(id);
      else startPolling(id, every);
    };
    sync();
    doc?.addEventListener('visibilitychange', sync);
    // Unmount is also how the ROUTE leaves — the shell swaps the screen out on a hash change — so
    // this cleanup is the one that has to release the feed when the RD navigates away.
    return () => {
      doc?.removeEventListener('visibilitychange', sync);
      stopWatching(id);
    };
  });

  return {
    get snapshot() {
      return snapshot;
    },
    get error() {
      return error;
    },
    get everLoaded() {
      return everLoaded;
    },
    get streaming() {
      return snapshot?.streaming ?? false;
    }
  };
}
