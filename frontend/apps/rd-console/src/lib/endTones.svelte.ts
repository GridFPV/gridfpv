/**
 * The **end-of-race tone scheduler** — a sibling of the race / arming clocks
 * ({@link useRaceClock} / {@link useArmingClock}) built on the same phase-driven `$effect`
 * pattern, deciding *when* the countdown pips and the race-end buzzer fire (the sounds themselves
 * live in `raceAudio.ts`).
 *
 * Only heats with a **known fixed end** get a countdown: the caller derives the window length —
 * a `Timed` win-condition round's `window_micros`, or a Practice run's `time_limit_secs` — and
 * passes it here (anything else, e.g. First-to-N, passes `undefined` and nothing ever fires).
 * While the heat is `Running` with a server `race_started_at`, the end instant is
 * `race_started_at + window`; at remaining **5, 4, 3, 2, 1 s** it fires `onCountdown(n)` and at
 * remaining **0** it fires `onRaceEnd()`. The buzzer marks the *window* end — the grace window
 * keeps the heat `Running` for late crossings after it, and scoring is untouched.
 *
 * ── Server-anchored, like the heat clock ─────────────────────────────────────────────────────
 * The schedule derives from the SAME anchor the heat-time readout uses: the server's
 * `race_started_at` (µs) compared against `nowMs()` — the offset-corrected server clock
 * (`session.serverNowMs()`), never a raw `Date.now()` anchor. The local `setInterval` only paces
 * the re-check (each tick re-reads the anchored clock), so timer drift between ticks cannot skew
 * the schedule — the standing clock rule for a console on a separate device.
 *
 * ── Once per heat RUN — no replays ───────────────────────────────────────────────────────────
 * Each mark fires at most once per run, keyed by `heat + race_started_at` (the run anchor — a
 * Restart mints a new `race_started_at`, so the next run counts down afresh; mirrors how the
 * start tone keys off the heat). Two replay guards:
 *   • live-stream re-renders re-run the `$effect` with the same run key — the fired-marks set
 *     lives *outside* the effect, so nothing re-fires;
 *   • a re-mount mid-race (navigating away and back) starts a fresh instance — on first sight of
 *     a run, every mark **already due** is pre-marked *silently*, so mounting at remaining 3.4s
 *     doesn't machine-gun the 5s and 4s pips; the still-future marks fire on time.
 */

/** The countdown marks (ms remaining) that get a pip; `0` (the buzzer) is handled separately. */
const COUNTDOWN_MARKS_MS = [5000, 4000, 3000, 2000, 1000];

/** How often the schedule re-checks the anchored clock (ms). Pips land within a tick of true. */
const TICK_MS = 100;

export interface EndOfRaceToneCues {
  /** A countdown pip is due: `secondsRemaining` ∈ 5…1. */
  onCountdown(secondsRemaining: number): void;
  /** The race window just ended (remaining 0): the race-end buzzer. */
  onRaceEnd(): void;
}

/**
 * Schedule the end-of-race tones for the live heat. All getters are read reactively:
 *   • `getPhase` / `getHeat` — the live phase + current heat (tones only while `Running`);
 *   • `getStartedAtMicros` — the server race-go instant (`race_started_at`, µs);
 *   • `getWindowMicros` — the fixed window length (µs), or `undefined` for a heat with no known
 *     fixed end (no countdown, ever);
 *   • `nowMs` — the **server-anchored** clock (`session.serverNowMs()`).
 * Must be called during component setup so its internal `$effect` is owned (torn down on
 * unmount).
 */
export function useEndOfRaceTones(
  getPhase: () => string | undefined,
  getHeat: () => string | undefined,
  getStartedAtMicros: () => number | null | undefined,
  getWindowMicros: () => number | undefined,
  nowMs: () => number,
  cues: EndOfRaceToneCues
): void {
  // The once-per-run guard, OUTSIDE the $effect: stream pushes re-run the effect with the same
  // run key, and the fired set must survive those re-runs (a per-run set inside the effect would
  // replay every mark on every push).
  let runKey: string | undefined;
  let fired = new Set<number>(); // COUNTDOWN_MARKS_MS entries + 0 (the buzzer)

  $effect(() => {
    const phase = getPhase();
    const heat = getHeat();
    const startedAtMicros = getStartedAtMicros();
    const windowMicros = getWindowMicros();
    // No fixed end / not running / no server anchor yet → nothing scheduled. (Leaving `Running`
    // clears the interval via the effect cleanup; an early ForceEnd simply never reaches 0 here.)
    if (phase !== 'Running' || heat === undefined || startedAtMicros == null) return;
    if (windowMicros === undefined) return;

    const endMs = (startedAtMicros + windowMicros) / 1000;
    const key = `${heat}@${startedAtMicros}`;
    if (key !== runKey) {
      // First sight of this run (a fresh race-go, a Restart's new anchor, or a re-mount mid-race).
      // Pre-mark every mark ALREADY due, silently — a genuine race-go has the full window ahead so
      // nothing pre-marks; a mid-race re-mount skips the pips that already sounded.
      runKey = key;
      fired = new Set();
      // STRICTLY past marks pre-mark (a mark due exactly now hasn't sounded anywhere yet — let
      // the first advance() below fire it).
      const remaining = endMs - nowMs();
      for (const mark of COUNTDOWN_MARKS_MS) if (remaining < mark) fired.add(mark);
      if (remaining < 0) fired.add(0);
    }

    const advance = () => {
      // Re-read the ANCHORED clock every tick — the interval only paces the check, so local timer
      // drift between ticks cannot skew when a mark fires.
      const remaining = endMs - nowMs();
      for (const mark of COUNTDOWN_MARKS_MS) {
        if (remaining <= mark && !fired.has(mark)) {
          fired.add(mark);
          cues.onCountdown(mark / 1000);
        }
      }
      if (remaining <= 0 && !fired.has(0)) {
        fired.add(0);
        cues.onRaceEnd();
      }
    };
    advance();
    const id = setInterval(advance, TICK_MS);
    return () => clearInterval(id);
  });
}
