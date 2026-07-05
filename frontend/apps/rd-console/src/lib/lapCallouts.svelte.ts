/**
 * **New-lap detection** for the live audio callouts — watches the live stream's per-pilot
 * `progress` for the CURRENT, RUNNING heat and fires `onLap` once per newly recorded lap. Pure
 * detection: the caller decides what a lap sounds like (crossing pip + spoken callout, both
 * mute-scoped) — see `LiveRaceControl`.
 *
 * ── Only genuine live crossings — no ghost callouts ──────────────────────────────────────────
 * Fires ONLY on a lap-count *increase* observed on the live stream while the heat is `Running`.
 * Everything else stays silent:
 *   • **not** marshaling corrections / historical folds — those flow through the heat-scoped
 *     projections (`heatLiveState` / `lapList`), not this live stream; and once a heat leaves
 *     `Running` this detector is off entirely, so editing a finished heat can never call out.
 *     (A count *decrease* seen live — a correction folding down — just resyncs the baseline.)
 *   • **not** non-current heats — the live stream only carries the current heat's progress, and
 *     a heat swap re-baselines (below) rather than diffing across heats.
 *   • **not** a late join / re-mount mid-race: the first sight of a run BASELINES the current
 *     counts silently (mirroring how the start tone suppresses navigation replays), so mounting
 *     onto lap 7 doesn't narrate laps 1–7. Runs are keyed `heat + race_started_at` — the same run
 *     anchor the end-of-race tones use — so a Restart's fresh run baselines afresh (at zero).
 */
import type { CompetitorRef, PilotProgress } from '@gridfpv/types';

/** One newly recorded lap, as handed to `onLap`. */
export interface LapCrossing {
  /** The competitor ref that crossed (resolve to a callsign via the shared resolver). */
  ref: CompetitorRef;
  /** The lap number just completed (the new `laps_completed`). */
  lap: number;
  /** The recorded lap duration (µs), when the stream carries it. */
  lastLapMicros: number | undefined;
}

/**
 * Watch the live progress for new laps. All getters are read reactively; `onLap` fires once per
 * count-increase per run. Must be called during component setup so its internal `$effect` is
 * owned (torn down on unmount).
 */
export function useLapCallouts(
  getPhase: () => string | undefined,
  getHeat: () => string | undefined,
  getStartedAtMicros: () => number | null | undefined,
  getProgress: () => readonly PilotProgress[] | undefined,
  onLap: (crossing: LapCrossing) => void
): void {
  // The per-run lap baseline, OUTSIDE the $effect: stream pushes re-run the effect with the same
  // run key, and the seen-counts must survive those re-runs (state inside the effect would
  // re-baseline — or worse, re-announce — every push).
  let runKey: string | undefined;
  let seen = new Map<CompetitorRef, number>();

  $effect(() => {
    const phase = getPhase();
    const heat = getHeat();
    const progress = getProgress();
    if (phase !== 'Running' || heat === undefined) {
      // Off while not Running (a finished heat being marshaled can never call out). Drop the run
      // so the next Running run baselines afresh.
      runKey = undefined;
      seen = new Map();
      return;
    }
    // The run anchor: heat + server race-go instant (a Restart mints a new one). `race_started_at`
    // can lag the Running flip by a tick — key that brief window as 'pending' and re-baseline once
    // the anchor lands (counts are still ~0 that early, so nothing is lost).
    const startedAt = getStartedAtMicros();
    const key = `${heat}@${startedAt ?? 'pending'}`;
    if (key !== runKey) {
      // First sight of this run (fresh race-go: all zeros; late join / re-mount: the current
      // counts). Baseline SILENTLY — only increases observed from here on call out.
      runKey = key;
      seen = new Map((progress ?? []).map((p) => [p.competitor, p.laps_completed]));
      return;
    }
    for (const p of progress ?? []) {
      const prev = seen.get(p.competitor) ?? 0;
      if (p.laps_completed > prev) {
        seen.set(p.competitor, p.laps_completed);
        onLap({
          ref: p.competitor,
          lap: p.laps_completed,
          lastLapMicros: p.last_lap_micros ?? undefined
        });
      } else if (p.laps_completed < prev) {
        // A live correction folded the count DOWN: resync silently (never a callout), so the next
        // genuine crossing announces the right lap number.
        seen.set(p.competitor, p.laps_completed);
      }
    }
  });
}
