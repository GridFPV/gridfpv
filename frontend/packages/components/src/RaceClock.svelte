<script lang="ts">
  import { formatClock } from './format.js';

  /**
   * RaceClock — pure presentational elapsed/remaining display.
   *
   * Takes a time in milliseconds and renders it as `M:SS.mmm`. It does not tick
   * on its own (no timers, no protocol dependency); the caller feeds it the
   * authoritative time so the same component serves a live overlay and a static
   * results readout. Pass `remainingMs` instead of `elapsedMs` for a countdown;
   * if both are given, `remainingMs` wins.
   *
   * A countdown carries **urgency** styling (readable at a glance in sunlight):
   * normal text color while comfortable, **warn** (yellow) inside the closing
   * {@link CLOSING_MS} seconds, **danger** (red) once past zero — the negative,
   * sign-prefixed readout of a timed heat running down its grace window.
   */
  let {
    elapsedMs = 0,
    remainingMs,
    /** Accessible label prefix announced with the value. */
    label = 'Race time'
  }: { elapsedMs?: number; remainingMs?: number; label?: string } = $props();

  /** A countdown turns warn-colored inside this window (matches the 5s end-tone pips' order of
   * magnitude — the visual pre-warning starts a bit earlier than the audible one). */
  const CLOSING_MS = 10_000;

  let ms = $derived(remainingMs ?? elapsedMs);
  let mode = $derived(remainingMs !== undefined ? 'remaining' : 'elapsed');
  let urgency = $derived.by(() => {
    if (remainingMs === undefined) return undefined;
    if (remainingMs < 0) return 'over';
    return remainingMs <= CLOSING_MS ? 'closing' : 'ok';
  });
  let display = $derived(formatClock(ms));
</script>

<time
  class="gridfpv-race-clock"
  data-mode={mode}
  data-urgency={urgency}
  role="timer"
  aria-label={`${label}: ${display}`}>{display}</time
>

<style>
  .gridfpv-race-clock {
    display: inline-block;
    color: var(--gf-text);
    font-family: var(--gf-font-mono);
    font-size: var(--gf-font-size-2xl);
    font-weight: var(--gf-font-weight-bold);
    font-variant-numeric: tabular-nums;
    letter-spacing: -0.02em;
    line-height: 1;
    font-feature-settings: 'tnum' 1;
  }
  .gridfpv-race-clock[data-urgency='closing'] {
    color: var(--gf-warn);
  }
  .gridfpv-race-clock[data-urgency='over'] {
    color: var(--gf-danger);
  }
</style>
