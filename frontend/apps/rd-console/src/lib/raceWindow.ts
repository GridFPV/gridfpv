/**
 * The **known fixed race end** of a round — shared by everything that renders or sounds the race
 * window: the end-of-race tones (`endTones.svelte.ts`), the Live HUD's countdown clock, and the
 * persistent header's clock. One derivation so they all agree on *whether* a heat has a fixed end
 * and *how long* its window is (lifted out of `LiveRaceControl`, mirroring how `useRaceClock` was
 * lifted for #85).
 */
import type { RoundDef } from '@gridfpv/types';

import { OPEN_PRACTICE } from './formats.js';

/**
 * The fixed window length (µs) of `round`, or `undefined` when the heat has no known fixed end.
 *
 * Only two configurations fix the end instant at race-go:
 *   • a **Timed** win-condition round — its `window_micros`, measured from race-go;
 *   • a **Practice** run with a `time_limit_secs` — the limit bounds the run.
 * First-to-N / BestLap rounds have no fixed end. NOTE an open-practice round stores an inert
 * *default* win condition that is never consulted (see RoundDef) — only its `time_limit_secs`
 * bounds the run, so a limitless practice has no fixed end either.
 */
export function fixedEndWindowMicros(round: RoundDef | undefined): number | undefined {
  if (!round) return undefined;
  if (round.format === OPEN_PRACTICE) {
    return round.time_limit_secs != null ? round.time_limit_secs * 1_000_000 : undefined;
  }
  const wc = round.win_condition;
  if (typeof wc === 'object' && wc !== null && 'Timed' in wc) return wc.Timed.window_micros;
  return undefined;
}
