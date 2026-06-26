/**
 * The **start-tone countdown** for a heat in `Armed` — a sibling of the protest / staging clocks
 * ({@link useProtestClock} / {@link useStagingClock}) built on the same phase-driven `$effect`
 * pattern.
 *
 * When a heat arms, the runtime chooses a randomized start delay and logs `HeatStarting { delay_ms }`,
 * then auto-advances `Armed → Running` (the start tone) that many ms later. The live projection
 * surfaces the resulting absolute instant as `tone_at` (server wall-clock µs) **while the heat is
 * Armed**, clearing it once the heat is Running. This helper counts the wall clock **down to** that
 * instant so the RD can read "Tone in 0:03".
 *
 * **RD-console-only:** the delay is *intentionally random to pilots*, so the consuming banner is
 * gated to a controlling (RD) session — a read-only / pilot view never feeds this clock a `tone_at`
 * worth showing, and never renders the countdown. This helper is purely the timer; the gating lives
 * at the call site.
 *
 * Behaviour, keyed off the absolute instant:
 *   • a `tone_at` is set → tick `at − now` every 100ms, clamped at zero (the tone + Running flip land
 *                          from the runtime; the countdown never shows negative — once it hits zero
 *                          the stream flips the heat to `Running` and `tone_at` clears).
 *   • no `tone_at`       → inactive (`active === false`).
 *
 * Ticks faster than the protest clock (100ms vs 250ms): the start hold is *short* (seconds), so a
 * sub-second `S.s` readout needs a tighter cadence to read smoothly. Like the other clocks this is
 * the v1 *approximate* timer — it trusts client and server wall clocks to agree (the same assumption
 * the elapsed / protest clocks make).
 *
 * Usage (inside a component, so the `$effect` has an owner):
 *   const arming = useArmingClock(() => canControl ? live?.tone_at : undefined);
 *   {#if arming.active}Tone in {formatArming(arming.remainingMs)}{/if}
 */

/** A live start-tone countdown derived from the heat's `tone_at`. */
export interface ArmingClockState {
  /** Whether a start-tone instant is set (the heat is `Armed` with a known tone time). */
  readonly active: boolean;
  /** Milliseconds remaining until the start tone fires; clamped at `0`. */
  readonly remainingMs: number;
}

/** How often the countdown re-reads the wall clock (ms). 100ms reads smoothly at a sub-second tick. */
const TICK_MS = 100;

/**
 * The start-tone instant (epoch **milliseconds**), or `undefined` when absent. The wire instant is
 * in **microseconds**; this converts to ms for `Date.now()` comparison.
 */
function toneMs(toneAt: number | undefined | null): number | undefined {
  return toneAt != null ? toneAt / 1000 : undefined;
}

/**
 * Create a `tone_at`-driven start-tone countdown. `getToneAt` is read reactively, so the countdown
 * arms when a `tone_at` is present and goes inactive otherwise. The caller is responsible for the
 * RD-only gating: pass `undefined` for a read-only / pilot session so the countdown never arms.
 * Must be called during component setup so its internal `$effect` is owned (torn down on unmount).
 */
export function useArmingClock(getToneAt: () => number | undefined | null): ArmingClockState {
  let remainingMs = $state(0);
  let active = $state(false);

  $effect(() => {
    const at = toneMs(getToneAt());
    if (at === undefined) {
      active = false;
      remainingMs = 0;
      return;
    }
    active = true;
    const advance = () => {
      // Count DOWN to the absolute tone instant, clamped at zero: the Running flip lands from the
      // runtime and clears `tone_at`, so the countdown never shows negative.
      remainingMs = Math.max(0, at - Date.now());
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

/**
 * Format a start-tone remainder in **milliseconds** for the RD. The hold is short (seconds), so
 * under a minute it reads as tenths (`3.2s`, `0.4s`) — a precise, fast-moving cue — and only the
 * (rare) case of a minute or more falls back to `M:SS` (e.g. `1:05`). Clamped at zero.
 */
export function formatArming(remainingMs: number): string {
  const ms = Math.max(0, remainingMs);
  if (ms >= 60_000) {
    const totalSecs = Math.floor(ms / 1000);
    const minutes = Math.floor(totalSecs / 60);
    const seconds = totalSecs % 60;
    return `${minutes}:${String(seconds).padStart(2, '0')}`;
  }
  // Tenths of a second: floor so the readout never shows the next-higher tenth (a stage clock, not
  // a round-to-nearest stopwatch).
  const tenths = Math.floor(ms / 100) / 10;
  return `${tenths.toFixed(1)}s`;
}
