/**
 * The live stream's two audio **detectors**, both pure (the caller decides what anything sounds
 * like — see `raceDayAudio.svelte.ts`):
 *
 *   • {@link useLapCallouts} — new **laps**, off `progress`, driving the SPOKEN callout;
 *   • {@link useCrossingTones} — new **crossings**, off the `crossings` feed, driving the TONE.
 *
 * They are deliberately separate because a lap and a crossing are not the same event: a lap is
 * derived from a *pair* of crossings, so most crossings never become one. Splitting them is what
 * lets the holeshot and a floor-rejected pass tone without inventing a lap to speak about.
 *
 * ── {@link useLapCallouts} ───────────────────────────────────────────────────────────────────
 * **New-lap detection** for the live audio callouts — watches the live stream's per-pilot
 * `progress` for the CURRENT, RUNNING heat and fires `onLap` once per newly recorded lap.
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
import type { CompetitorRef, LiveCrossing, PilotProgress } from '@gridfpv/types';

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

/**
 * **New-crossing detection** for the per-crossing tone (#397) — the sibling of
 * {@link useLapCallouts} above, and the reason this module is no longer only about laps.
 *
 * ── Why a second detector at all ─────────────────────────────────────────────────────────────
 * `useLapCallouts` watches `progress`, and `progress` reports **laps**. Laps are derived from
 * consecutive pairs of crossings (`passes.windows(2)`), so a lap-derived consumer is structurally
 * deaf to most crossings: the **holeshot** opens the first lap and closes none, and a pass the
 * round's min-lap floor **rejected** closes none either. Both are silent today. The live crossing
 * feed (`LiveRaceState.crossings`) carries the *observations*, so this detector fires on every
 * crossing whatever became of it — including a phantom on a seat nobody is flying. **That is the
 * feature**: a tone for a crossing that should not have happened is how an RD notices a
 * too-sensitive gate, and it is exactly what a table of laps will never show them. Nothing here
 * filters toward "meaningful" crossings.
 *
 * ── The watermark: fire on IDENTITY, never on a frame arriving ───────────────────────────────
 * Every `LiveCrossing` carries `pass_ref` — its **global append offset**, stable across every
 * re-fold, re-push, resubscribe and scope change. The detector holds ONE high-water mark over it
 * and announces `pass_ref > watermark`. Receipt of a `LiveRaceState` means nothing at all: a
 * re-pushed identical state, a fresh snapshot after a reconnect, or a stream wake-up re-sending
 * the same frame all announce exactly nothing. (Structural, not a workaround for #396 — that a
 * console must never assume a frame implies novelty is the lesson, and it outlives the bug.) It
 * is also why nothing here keys on `at`: a marshal-inserted pass carries an OLD source time under
 * a NEW offset, and time-ordering would file a genuinely new crossing among the seen ones. A
 * crossing whose *disposition* later changes (a marshal voids a counted lap) keeps its `pass_ref`,
 * so it is correctly not re-announced — a re-labelled crossing is not a new one.
 *
 * ── Baseline silently on first sight ─────────────────────────────────────────────────────────
 * The feed is bounded (the most recent 64 crossings, tail kept). A mid-heat mount or a reconnect
 * legitimately arrives carrying up to all 64 unseen — announcing them would be a machine-gun of
 * history. So the FIRST frame in a scope only sets the watermark and plays nothing, mirroring how
 * {@link useLapCallouts} baselines a late join and how the start tone suppresses replays.
 *
 * ── Scope: the event, because offsets are per-event-log ──────────────────────────────────────
 * Each event has its own log, so append offsets **restart at zero** in a different event. A
 * watermark carried across an event switch would swallow the new event's whole race. The scope
 * key (the event id) re-baselines the watermark — silently, like any first sight.
 *
 * ── While the heat is ARMED or RUNNING ───────────────────────────────────────────────────────
 * `Armed` is included deliberately, and it is arguably the most valuable window of the two: with
 * pilots on the line and nobody flying, *nothing* should trigger the gate — so a pip in that
 * silence is an unambiguous false crossing, which is the whole reason this feature exists. A
 * legitimate pre-race gate check (waving a quad through to confirm the gate lives) pips too, which
 * is also wanted.
 *
 * Still excluded: idle and tuning (#355's Tune page owns the bench case, and owns it better —
 * visual, per-node, with the thresholds in view). Outside those phases the watermark still
 * advances **silently**, so arming or starting can never dump a backlog as a burst.
 *
 * Must be called during component setup so its internal `$effect` is owned (torn down on unmount).
 */
export function useCrossingTones(
  getScope: () => string | undefined,
  getPhase: () => string | undefined,
  getCrossings: () => readonly LiveCrossing[] | undefined,
  onCrossing: (crossing: LiveCrossing) => void
): void {
  // All three live OUTSIDE the $effect: stream pushes re-run it, and a watermark that reset on
  // every push would re-announce the whole feed — the exact failure this detector exists to make
  // impossible.
  let scopeKey: string | undefined;
  let baselined = false;
  let watermark = -1;

  $effect(() => {
    const scope = getScope();
    const phase = getPhase();
    // Armed as well as Running — see the header: a crossing while everyone is on the line is the
    // clearest false positive there is.
    const audible = phase === 'Armed' || phase === 'Running';
    const crossings = getCrossings() ?? [];
    if (scope !== scopeKey) {
      // A different event log ⇒ a different offset space. Re-baseline on its first frame.
      scopeKey = scope;
      baselined = false;
      watermark = -1;
    }
    // First sight of this scope BASELINES: retire everything on offer, announce none of it.
    const announce = baselined && audible;
    // Snapshot the mark before the sweep so an (unexpectedly) unsorted feed cannot let an earlier
    // entry retire a later one's chance to speak. The feed is documented ascending; this costs
    // nothing and makes the sweep independent of that.
    const previous = watermark;
    let next = watermark;
    for (const crossing of crossings) {
      if (crossing.pass_ref <= previous) continue;
      if (crossing.pass_ref > next) next = crossing.pass_ref;
      if (announce) onCrossing(crossing);
    }
    watermark = next;
    baselined = true;
  });
}
