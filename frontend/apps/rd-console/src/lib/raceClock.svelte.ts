/**
 * The shared race clock (#62), lifted out of {@link LiveRaceControl} so the persistent
 * {@link ContextHeader} and the live screen drive the **same** clock from one place (#85).
 *
 * Behaviour, keyed off the live `phase`:
 *   • Running                → tick a wall-clock timer (`Date.now() - start`) every 50ms.
 *   • Finished / Scored      → freeze the clock at its last ticked value (stop ticking).
 *   • Scheduled/Staged/Armed → reset to 0 (a fresh/idle heat, or no heat at all).
 * The off-ramps Abort/Restart/Discard aren't phases of their own — they fold back onto one
 * of the above (typically Scheduled), so they fall out of these same rules.
 *
 * This is the v1, *approximate* clock: on a late join (the heat is already Running when this
 * mounts, or after a reconnect) it counts from "now", not the real heat start, so the time can
 * read short. The fuller fix (#62 follow-up) is a server-authoritative start time in
 * `LiveRaceState`. Sharing the logic here means that fix lands in exactly one place.
 *
 * Usage (inside a component, so the `$effect` has an owner):
 *   const clock = useRaceClock(() => session.liveState?.phase ?? 'Scheduled');
 *   <RaceClock elapsedMs={clock.elapsedMs} />
 */

/** A live, ticking elapsed-millis reading whose behaviour follows the heat `phase`. */
export interface RaceClockState {
  /** Milliseconds elapsed in the current Running window; frozen/zeroed per the rules above. */
  readonly elapsedMs: number;
}

/**
 * Create a phase-driven race clock. `getPhase` is read reactively, so the timer starts,
 * freezes, and resets as the live phase changes. Must be called during component setup so
 * its internal `$effect` is owned (and torn down on unmount).
 */
export function useRaceClock(getPhase: () => string | undefined): RaceClockState {
  let elapsedMs = $state(0);

  $effect(() => {
    // `phase` is the only reactive read in this effect, so it re-runs *only* on a phase
    // change — not on every render — which keeps the clock from restarting spuriously.
    const phase = getPhase();
    if (phase === 'Running') {
      const startedAt = Date.now();
      const advance = () => {
        elapsedMs = Date.now() - startedAt;
      };
      advance();
      const id = setInterval(advance, 50);
      // Teardown: stop ticking when the phase leaves Running (or on unmount). The next
      // effect run applies the freeze (Finished/Scored) or reset (everything else).
      return () => clearInterval(id);
    }
    if (phase === 'Finished' || phase === 'Scored') {
      // Freeze: leave `elapsedMs` at its last Running value.
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
