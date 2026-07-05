/**
 * The shared race clock (#62), lifted out of {@link LiveRaceControl} so the persistent
 * {@link ContextHeader} and the live screen drive the **same** clock from one place (#85).
 *
 * **Server-time-authoritative (#62 follow-up).** The elapsed is no longer a per-component
 * wall-clock that starts when *this* instance first sees `Running` — that made the header and
 * the live HUD drift (each anchored to whenever it happened to mount) and freeze a tick past
 * the real end. Instead it counts from the server's authoritative race timing carried on
 * `LiveRaceState`:
 *   • `race_started_at` — the µs instant the heat went `Running` (the real race-go).
 *   • `race_ended_at`   — the µs instant it went `Unofficial` (the race-end), once ended.
 *
 * Behaviour, keyed off the live `phase` and that timing:
 *   • Running                → `elapsed = now - race_started_at`, ticked every 50ms. Every
 *     instance shares the one server anchor, so the header and the HUD **agree** and read
 *     accurately from the real race-go regardless of when each mounted (a late join / reload
 *     no longer counts from arrival).
 *   • Unofficial / Final     → freeze at the **exact** server duration
 *     `race_ended_at - race_started_at` — killing the ~0.1s overshoot a poll-late freeze left
 *     (a 60s limit reads 1:00.000, not 1:00.100).
 *   • Scheduled/Staged/Armed → reset to 0 (a fresh/idle heat, or no heat at all).
 * The off-ramps Abort/Restart/Discard aren't phases of their own — they fold back onto one
 * of the above (typically Scheduled), so they fall out of these same rules.
 *
 * Skew note: `now` is the client clock and `race_started_at` the server's. On loopback/LAN the
 * skew is small and — crucially — *shared* by every instance, so the two clocks are consistent
 * (the bug being fixed). The frozen value uses only server instants, so it is exact.
 *
 * Usage (inside a component, so the `$effect` has an owner):
 *   const clock = useRaceClock(
 *     () => session.liveState?.phase ?? 'Scheduled',
 *     () => session.liveState?.race_started_at,
 *     () => session.liveState?.race_ended_at
 *   );
 *   <RaceClock elapsedMs={clock.elapsedMs} />
 */

/** A live, ticking elapsed-millis reading whose behaviour follows the heat `phase`. */
export interface RaceClockState {
  /** Milliseconds elapsed in the current Running window; frozen/zeroed per the rules above. */
  readonly elapsedMs: number;
}

/**
 * Create a phase-driven, **server-time-anchored** race clock. `getPhase` selects the rule;
 * `getStartedAt` / `getEndedAt` supply the server's `race_started_at` / `race_ended_at`
 * (microseconds, from `LiveRaceState`). All are read reactively, so the clock starts, freezes,
 * and resets as the live state changes. Must be called during component setup so its internal
 * `$effect` is owned (and torn down on unmount).
 */
export function useRaceClock(
  getPhase: () => string | undefined,
  getStartedAt: () => number | null | undefined,
  getEndedAt: () => number | null | undefined,
  nowMs: () => number = () => Date.now()
): RaceClockState {
  let elapsedMs = $state(0);

  $effect(() => {
    const phase = getPhase();
    const startedAtMicros = getStartedAt();
    const endedAtMicros = getEndedAt();

    if (phase === 'Running') {
      // No server start yet (a brief window before the `Running` transition's timestamp has
      // propagated): hold at 0 rather than counting from an unknown anchor.
      if (startedAtMicros == null) {
        elapsedMs = 0;
        return;
      }
      const startedAtMs = startedAtMicros / 1000;
      // All instances count from the SAME server anchor → header and HUD agree, accurate from
      // the real race-go regardless of when this clock mounted.
      const advance = () => {
        // `nowMs()` is the SERVER clock (offset-corrected) so a skewed RD device counts from the real
        // race-go, not ~1s off (`startedAtMs` is a server instant).
        elapsedMs = Math.max(0, nowMs() - startedAtMs);
      };
      advance();
      const id = setInterval(advance, 50);
      return () => clearInterval(id);
    }

    if (phase === 'Unofficial' || phase === 'Final') {
      // Freeze at the EXACT server-side race duration. Falls back to the last ticked value only
      // if the server timing is somehow absent (an older log), preserving the prior behaviour.
      if (startedAtMicros != null && endedAtMicros != null) {
        elapsedMs = Math.max(0, (endedAtMicros - startedAtMicros) / 1000);
      }
      return;
    }

    // Scheduled / Staged / Armed / no heat (incl. where Abort/Restart/Discard land): reset.
    elapsedMs = 0;
  });

  return {
    get elapsedMs() {
      return elapsedMs;
    }
  };
}
