/**
 * The **staging countdown** for a heat in `Staged` (heat-lifecycle Slice 3), a sibling of the
 * race clock ({@link useRaceClock}) built on the same phase-driven `$effect` pattern.
 *
 * When a heat enters `Staged` the round's `staging_timer_secs` (default 300 = 5:00) starts
 * counting **down**. It is *informational only* — there is **no** auto-advance out of `Staged`
 * (race-engine.html §2: the staging timer never fires a transition); the RD reads it to know how
 * long the field has been on the line, and crucially it keeps counting **past zero into the
 * negative** so a field that isn't ready shows up as over-time (a "−0:14" the console renders red).
 *
 * Behaviour, keyed off the live `phase`:
 *   • Staged                       → tick a wall-clock countdown from `secs` every 250ms; goes
 *                                    negative past zero (over-time), never auto-advances.
 *   • anything else / no heat      → reset to `secs` (idle; the next Staged re-arms it).
 *
 * **Server-anchored** (#62 family): the countdown anchors to the live state's `staged_at` — the
 * server's `Scheduled → Staged` instant — read against `nowMs()` (the offset-corrected
 * `session.serverNowMs()`), so every console (and a reload / late join) reads the SAME window.
 * The old per-mount `Date.now()` anchor made each console count its own staging window (the RD
 * read overtime while a fresh console read 5:00), and a late-resolving round config silently
 * re-anchored a running countdown. Before the server anchor lands (a tick-old stream) it falls
 * back to the mount-time anchor, converging on the next push.
 *
 * Usage (inside a component, so the `$effect` has an owner):
 *   const staging = useStagingClock(
 *     () => phase,
 *     () => round?.staging_timer_secs ?? 300,
 *     () => live?.staged_at,
 *     () => session.serverNowMs()
 *   );
 *   {staging.remainingMs}  // ms left; negative once over-time
 */

/** A live staging countdown whose behaviour follows the heat `phase`. */
export interface StagingClockState {
  /** Milliseconds remaining in the staging window; **negative once over-time** (past zero). */
  readonly remainingMs: number;
  /** Whether the window has elapsed (`remainingMs <= 0`) — the console renders this red. */
  readonly overtime: boolean;
}

/** How often the staging countdown re-reads the wall clock (ms). 250ms reads smoothly at 1s tick. */
const TICK_MS = 250;

/**
 * Create a phase-driven staging countdown. `getPhase` and `getSecs` are read reactively, so the
 * countdown arms when the phase becomes `Staged`, counts down (going negative past zero), and
 * resets when the phase leaves `Staged`. Must be called during component setup so its internal
 * `$effect` is owned (and torn down on unmount).
 */
export function useStagingClock(
  getPhase: () => string | undefined,
  getSecs: () => number,
  getStagedAtMicros: () => number | null | undefined = () => undefined,
  nowMs: () => number = () => Date.now()
): StagingClockState {
  let remainingMs = $state(0);

  $effect(() => {
    const phase = getPhase();
    const totalMs = Math.max(0, Math.round(getSecs() * 1000));
    if (phase === 'Staged') {
      // Anchor to the SERVER's staging instant when the stream carries it (µs → ms); fall back
      // to the observation instant only until it lands. Both are read against the SAME clock
      // (`nowMs()` — offset-corrected server time), the standing skew rule.
      const stagedAtMicros = getStagedAtMicros();
      const anchorMs = stagedAtMicros != null ? stagedAtMicros / 1000 : nowMs();
      const advance = () => {
        // Count DOWN from the full window; allow it to pass below zero (over-time) so the RD
        // sees a field that has overstayed its staging slot.
        remainingMs = totalMs - (nowMs() - anchorMs);
      };
      advance();
      const id = setInterval(advance, TICK_MS);
      return () => clearInterval(id);
    }
    // Not staging (or no heat): re-arm to the full window for the next Staged.
    remainingMs = totalMs;
  });

  return {
    get remainingMs() {
      return remainingMs;
    },
    get overtime() {
      return remainingMs <= 0;
    }
  };
}

/**
 * Format a (possibly negative) staging remainder in **milliseconds** as `M:SS`, prefixed with `−`
 * when over-time (e.g. `5:00`, `0:07`, `−0:14`). Floors to whole seconds, mirroring a stage clock.
 */
export function formatStaging(remainingMs: number): string {
  const neg = remainingMs < 0;
  const totalSecs = Math.floor(Math.abs(remainingMs) / 1000);
  const minutes = Math.floor(totalSecs / 60);
  const seconds = totalSecs % 60;
  const body = `${minutes}:${String(seconds).padStart(2, '0')}`;
  return neg ? `−${body}` : body;
}
