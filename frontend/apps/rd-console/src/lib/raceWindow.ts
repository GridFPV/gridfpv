/**
 * The **known fixed race end** of a round — shared by everything that renders or sounds the race
 * window: the end-of-race tones (`endTones.svelte.ts`), the Live HUD's countdown clock, and the
 * persistent header's clock. One derivation so they all agree on *whether* a heat has a fixed end
 * and *how long* its window is (lifted out of `LiveRaceControl`, mirroring how `useRaceClock` was
 * lifted for #85).
 */
import type { RoundDef } from '@gridfpv/types';

/**
 * The fixed window length (µs) of `round`, or `undefined` when the heat has no known fixed end.
 *
 * Two configurations fix the end instant at race-go:
 *   • a **time limit** (`time_limit_secs`) — the completion driver's auto-end, and it is
 *     format-blind: a Time Trial stores its race duration here (Best-of-N only *ranks*, it never
 *     ends a heat — the limit is what does), and a practice its optional duration. Checked FIRST,
 *     mirroring the driver's unconditional time-limit branch (#504 — this used to be honoured
 *     only for open practice, so a time trial ran with no countdown, no pips and no buzzer while
 *     the backend ended it on schedule anyway);
 *   • a **Timed** win-condition round — its `window_micros`, measured from race-go.
 * First-to-N rounds (no limit set) have no fixed end.
 */
export function fixedEndWindowMicros(round: RoundDef | undefined): number | undefined {
  if (!round) return undefined;
  if (round.time_limit_secs != null) return round.time_limit_secs * 1_000_000;
  const wc = round.win_condition;
  if (typeof wc === 'object' && wc !== null && 'Timed' in wc) return wc.Timed.window_micros;
  return undefined;
}
