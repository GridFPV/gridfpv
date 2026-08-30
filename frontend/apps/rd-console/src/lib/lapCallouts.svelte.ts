/**
 * The live stream's two audio **detectors**, both pure (the caller decides what anything sounds
 * like — see `raceDayAudio.svelte.ts`):
 *
 *   • {@link useLapCallouts} — newly closed **laps**, driving the SPOKEN callout;
 *   • {@link useCrossingTones} — every **crossing**, driving the TONE.
 *
 * Both read the live `crossings` feed and both key on the same thing — a crossing's `pass_ref`,
 * the only identity on the wire that survives a re-fold. They stay two detectors because a lap and
 * a crossing are not the same event: a lap is derived from a *pair* of crossings, so most
 * crossings never close one. Splitting them is what lets the holeshot and a floor-rejected pass
 * tone without inventing a lap to speak about.
 *
 * ── {@link useLapCallouts} ───────────────────────────────────────────────────────────────────
 * **New-lap detection** for the live audio callouts — fires `onLap` once per newly closed lap of
 * the CURRENT, RUNNING heat.
 *
 * ── Fire on the IDENTITY of the pass that closed the lap (#417) ──────────────────────────────
 * This detector used to diff `progress.laps_completed` and announce every increase, resyncing
 * silently on a decrease. That is correct only while the count is monotonic, and it is not: a live
 * re-fold can hand back a LOWER count (the min-lap floor rejecting a pass that briefly folded as a
 * lap; a marshal voiding one). The silent resync did exactly what it claimed — but a count
 * *returning* to a value already announced is, to a count-keyed detector, indistinguishable from a
 * new lap *reaching* that value, so `lap N` → dip to `N-1` → back to `N` spoke lap N twice. **A
 * count is not an identity.**
 *
 * (What dipped on the bench was NOT the min-lap floor — that is applied on every live fold, per
 * prefix, and #409's conformance proof covers all three live scopes. It was a WS reconnect
 * replaying intermediate folds from behind the true tail, #422. Which is the point: a consumer
 * keyed on identity does not need to know why a count moved.)
 *
 * The identity is the pass that **closed** the lap. Every `LiveCrossing` carries `pass_ref` — its
 * global append offset, stable across every re-fold, re-push, resubscribe and scope change — and
 * the `lap_number` it closed. One high-water mark over `pass_ref` and each lap is announced
 * exactly once, by the pass that closed it: a count recovering to an announced value announces
 * nothing, because no new pass closed anything. (The projection's `Lap` carries that offset as
 * `end_ref`, but `PilotProgress` does not — it reports counts only. The crossings feed is where
 * the closing pass's offset reaches a live consumer.)
 *
 * Crossings that close no lap are silent here: the **holeshot** (it opens the first lap), a pass
 * the round's min-lap **floor rejected**, and a **marshal-voided** one all carry no `lap_number`
 * and have no lap number or lap time to speak. They still tone — see {@link useCrossingTones}.
 *
 * ── `progress` is still read, for the lap TIME alone ─────────────────────────────────────────
 * A `LiveCrossing` carries no duration, and a callout without one is thinner. `last_lap_micros` is
 * the pilot's MOST RECENT lap, so it is *this* lap's time exactly when that pilot's
 * `laps_completed` is the lap just closed; when they disagree (two of one pilot's laps closing in
 * a single frame — only the newer's time is on offer) the callout speaks the number alone rather
 * than another lap's time. Presence in `progress` also scopes the VOICE to the lineup: a crossing
 * on a seat nobody is flying tones, which is #397's whole point, but is not narrated.
 *
 * ── Only genuine live crossings — no ghost callouts ──────────────────────────────────────────
 * Everything else stays silent:
 *   • **not** marshaling corrections / historical folds — those flow through the heat-scoped
 *     projections (`heatLiveState` / `lapList`), not this live stream; and once a heat leaves
 *     `Running` this detector is off entirely, so editing a finished heat can never call out.
 *     A crossing whose *disposition* changes keeps its `pass_ref`, so a re-labelled pass is never
 *     a new lap — that is the same rule {@link useCrossingTones} states, applied to laps.
 *   • **not** non-current heats — the live stream only carries the current heat's crossings, and
 *     a heat swap re-baselines (below) rather than carrying a mark across heats.
 *   • **not** a late join / re-mount mid-race: the first sight of a run RETIRES the whole feed
 *     silently (mirroring how the start tone suppresses navigation replays), so mounting onto lap
 *     7 doesn't narrate laps 1–7. Runs are keyed `heat + race_started_at` — the same run anchor
 *     the end-of-race tones use — so a Restart's fresh run baselines afresh.
 */
import type { CompetitorRef, LiveCrossing, PilotProgress } from '@gridfpv/types';

/** One newly closed lap, as handed to `onLap`. */
export interface LapCrossing {
  /** The competitor ref that crossed (resolve to a callsign via the shared resolver). */
  ref: CompetitorRef;
  /** The 1-based lap this crossing closed. */
  lap: number;
  /** The recorded lap duration (µs), when `progress` confirms it is this lap's. */
  lastLapMicros: number | undefined;
}

/**
 * Watch the live crossings for newly closed laps. All getters are read reactively; `onLap` fires
 * once per closing `pass_ref` per run. Must be called during component setup so its internal
 * `$effect` is owned (torn down on unmount).
 */
export function useLapCallouts(
  getPhase: () => string | undefined,
  getHeat: () => string | undefined,
  getStartedAtMicros: () => number | null | undefined,
  getCrossings: () => readonly LiveCrossing[] | undefined,
  getProgress: () => readonly PilotProgress[] | undefined,
  onLap: (crossing: LapCrossing) => void
): void {
  // The run anchor and its high-water mark, OUTSIDE the $effect: stream pushes re-run the effect
  // with the same run key, and a mark that reset on every push would re-announce the whole feed
  // on every frame — the exact failure the mark exists to make impossible.
  let runKey: string | undefined;
  let watermark = -1;

  $effect(() => {
    const phase = getPhase();
    const heat = getHeat();
    // Read every input on every run so the effect's dependency set does not depend on which
    // branch it takes.
    const crossings = getCrossings() ?? [];
    const progress = getProgress() ?? [];
    if (phase !== 'Running' || heat === undefined) {
      // Off while not Running (a finished heat being marshaled can never call out). Drop the run
      // so the next Running run baselines afresh.
      runKey = undefined;
      watermark = -1;
      return;
    }
    // The run anchor: heat + server race-go instant (a Restart mints a new one). `race_started_at`
    // can lag the Running flip by a tick — key that brief window as 'pending' and re-baseline once
    // the anchor lands (the feed is still ~empty that early, so nothing is lost).
    const key = `${heat}@${getStartedAtMicros() ?? 'pending'}`;
    if (key !== runKey) {
      // First sight of this run (fresh race-go: an empty feed; late join / re-mount: up to the
      // feed's whole bound). Retire everything on offer and announce NONE of it — this is what
      // stops a reconnect narrating a whole race. RESETTING the mark rather than keeping it is
      // also what makes an event switch safe: append offsets restart per event log, and a mark
      // carried across would swallow the new event's entire race.
      runKey = key;
      watermark = crossings.reduce((mark, c) => (c.pass_ref > mark ? c.pass_ref : mark), -1);
      return;
    }
    const last = new Map(progress.map((p) => [p.competitor, p]));
    // Snapshot the mark before the sweep so an (unexpectedly) unsorted feed cannot let an earlier
    // entry retire a later one's chance to speak. The feed is documented ascending; this costs
    // nothing and makes the sweep independent of that.
    const previous = watermark;
    let next = watermark;
    for (const c of crossings) {
      if (c.pass_ref <= previous) continue;
      if (c.pass_ref > next) next = c.pass_ref;
      // Holeshot / floor-rejected / marshal-voided: closed no lap, so there is nothing to speak.
      const lap = c.lap_number;
      if (lap == null) continue;
      // The voice is the lineup's; a phantom seat tones but is not narrated.
      const p = last.get(c.competitor);
      if (p === undefined) continue;
      onLap({
        ref: c.competitor,
        lap,
        lastLapMicros: p.laps_completed === lap ? (p.last_lap_micros ?? undefined) : undefined
      });
    }
    watermark = next;
  });
}

/**
 * How long after a competitor's SOUNDED tone their further crossings are absorbed (#503), in
 * source-clock µs. Well under any real lap (the min-lap floor's field default is 10s) and well
 * over a reflection burst's spread (tens to hundreds of ms), so it can only ever collapse a
 * multi-detection of one physical pass, never silence a genuine next lap.
 */
export const CROSSING_TONE_COOLDOWN_MICROS = 1_000_000;

/**
 * **New-crossing detection** for the per-crossing tone (#397) — the sibling of
 * {@link useLapCallouts} above, and the reason this module is no longer only about laps.
 *
 * ── Why a second detector at all ─────────────────────────────────────────────────────────────
 * `useLapCallouts` speaks **laps**, and a lap is derived from a pair of crossings
 * (`passes.windows(2)`), so a lap-driven consumer is structurally deaf to most crossings: the
 * **holeshot** opens the first lap and closes none, and a pass the round's min-lap floor
 * **rejected** closes none either. Both are silent there — since #417 the two detectors read the
 * same feed, but only this one has anything to say about a crossing that closed no lap. The feed
 * (`LiveRaceState.crossings`) carries the *observations*, so this detector fires on every
 * crossing whatever became of it — including a phantom on a seat nobody is flying. **That is the
 * feature**: a tone for a crossing that should not have happened is how an RD notices a
 * too-sensitive gate, and it is exactly what a table of laps will never show them. Nothing here
 * filters toward "meaningful" crossings — but repeats are rate-limited, see the cooldown below.
 *
 * ── The cooldown: one physical pass is ONE tone (#503) ───────────────────────────────────────
 * A quad sitting in the gate's near field fires the detector several times per pass (antenna
 * reflections milliseconds apart), and on the field that rendered as a pip storm per lap — the
 * tone stopped answering "did the gate see me?" and started drowning the RD. So: a crossing by
 * the SAME competitor within {@link CROSSING_TONE_COOLDOWN_MICROS} of the last one *sounded* for
 * them is absorbed. Measured from the last sounded tone (an absorbed crossing does not extend
 * the window), keyed **per competitor** — two pilots crossing near-simultaneously must both
 * tone, that is the gate telling the RD it saw both. The window is source-clock (`at`), the axis
 * bursts are adjacent on; a crossing carrying an *older* source time than the last sounded one
 * (a marshal insert) is never absorbed. Absorbed crossings still advance the watermark — they
 * are seen, just not sounded — and the min-lap floor still voids them on its own axis; this
 * cooldown only de-duplicates the NOISE of them.
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
  // The source time (µs) of the last crossing SOUNDED per competitor — the cooldown's anchor
  // (#503). Absorbed crossings never land here, so the window measures from the last tone.
  const lastTonedAt = new Map<CompetitorRef, number>();

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
      lastTonedAt.clear();
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
      if (!announce) continue;
      // The cooldown (#503): absorb a same-competitor crossing inside the window of their last
      // SOUNDED tone. A negative delta (a marshal insert carrying an older source time) is a
      // different case entirely and always tones; exactly-at-the-boundary tones too.
      const last = lastTonedAt.get(crossing.competitor);
      if (last !== undefined) {
        const since = crossing.at - last;
        if (since >= 0 && since < CROSSING_TONE_COOLDOWN_MICROS) continue;
      }
      lastTonedAt.set(crossing.competitor, crossing.at);
      onCrossing(crossing);
    }
    watermark = next;
    baselined = true;
  });
}
