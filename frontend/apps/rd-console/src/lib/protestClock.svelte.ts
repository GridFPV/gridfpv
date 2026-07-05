/**
 * The **auto-official countdown** for a heat in `Unofficial` whose round armed a protest window
 * (marshaling Slice 5, marshaling.html §3.3) — a sibling of the staging clock ({@link
 * useStagingClock}) built on the same phase-driven `$effect` pattern.
 *
 * When the runtime arms the protest window it logs the **deadline** (the server-clock instant it
 * will auto-finalize), which the live projection surfaces as
 * `lifecycle = Provisional { auto_official_at }`. This helper counts the wall clock **down to** that
 * instant so the console can render "Provisional — auto-official in M:SS". With no armed window
 * (`auto_official_at` absent) it is simply inactive — the heat is provisional until the RD finalizes
 * manually.
 *
 * Behaviour, keyed off the absolute deadline:
 *   • a deadline is set → tick `at − now` every 250ms, clamped at zero (the auto-finalize lands
 *                         from the runtime; the countdown never goes negative — once it hits zero the
 *                         stream will flip the heat to `Final` / `Official`).
 *   • no deadline       → inactive (`active === false`).
 *
 * Like the race / staging clocks this is the v1 *approximate* timer: it trusts the client and server
 * wall clocks to agree (the same assumption the elapsed clock makes). Sharing the phase-effect shape
 * keeps the family together.
 *
 * Usage (inside a component, so the `$effect` has an owner):
 *   const protest = useProtestClock(() => live?.lifecycle, () => session.serverNowMs());
 *   {#if protest.active}Provisional — auto-official in {formatProtest(protest.remainingMs)}{/if}
 */

import type { LifecycleState } from '@gridfpv/types';

/** A live auto-official countdown derived from the heat's lifecycle. */
export interface ProtestClockState {
  /** Whether a protest window is armed (the lifecycle is `Provisional` with a deadline). */
  readonly active: boolean;
  /** Milliseconds remaining until the auto-official deadline; clamped at `0`. */
  readonly remainingMs: number;
}

/** How often the countdown re-reads the wall clock (ms). 250ms reads smoothly at a 1s tick. */
const TICK_MS = 250;

/**
 * The auto-official deadline (epoch **milliseconds**) carried by a `Provisional` lifecycle, or
 * `undefined` when the lifecycle is absent, `Official`, or `Provisional` without an armed window.
 * The wire instant is in **microseconds**; this converts to ms for `Date.now()` comparison.
 */
function deadlineMs(lifecycle: LifecycleState | undefined | null): number | undefined {
  if (lifecycle && typeof lifecycle === 'object' && 'Provisional' in lifecycle) {
    const at = lifecycle.Provisional.auto_official_at;
    return at != null ? at / 1000 : undefined;
  }
  return undefined;
}

/**
 * Create a phase/lifecycle-driven auto-official countdown. `getLifecycle` is read reactively, so the
 * countdown arms when a `Provisional` lifecycle carries a deadline and goes inactive otherwise. Must
 * be called during component setup so its internal `$effect` is owned (torn down on unmount).
 */
export function useProtestClock(
  getLifecycle: () => LifecycleState | undefined | null,
  nowMs: () => number = () => Date.now()
): ProtestClockState {
  let remainingMs = $state(0);
  let active = $state(false);

  $effect(() => {
    const at = deadlineMs(getLifecycle());
    if (at === undefined) {
      active = false;
      remainingMs = 0;
      return;
    }
    active = true;
    const advance = () => {
      // Count DOWN to the absolute deadline, clamped at zero: the auto-finalize lands from the
      // runtime, and the stream then flips the heat to Final, so the countdown never shows negative.
      // `nowMs()` is the SERVER clock (offset-corrected) — the auto-official deadline is a server
      // instant, so a skewed RD device would otherwise mis-read the remaining time (clock-skew rule).
      remainingMs = Math.max(0, at - nowMs());
    };
    advance();
    const id = setInterval(advance, TICK_MS);
    return () => clearInterval(id);
  });

  return {
    get active() {
      return active;
    },
    get remainingMs() {
      return remainingMs;
    }
  };
}

/** Format an auto-official remainder in **milliseconds** as `M:SS` (e.g. `5:00`, `0:07`). */
export function formatProtest(remainingMs: number): string {
  const totalSecs = Math.max(0, Math.floor(remainingMs / 1000));
  const minutes = Math.floor(totalSecs / 60);
  const seconds = totalSecs % 60;
  return `${minutes}:${String(seconds).padStart(2, '0')}`;
}
