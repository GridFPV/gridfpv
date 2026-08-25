/**
 * The console's **race-day audio player** (grown out of the heat-lifecycle Slice 3 start tone) —
 * every Web-Audio cue the RD console emits, in two clearly-scoped layers:
 *
 * ── Procedure tones — ALWAYS ON, never muted ─────────────────────────────────────────────────
 * The audible race procedure. These are the on-course signals a race runs by, so they play
 * unconditionally — there is **no** toggle for them (the old "Tone on/off" switch was a testing
 * convenience and is gone):
 *   • {@link RaceAudioPlayer.playStartTone} — race-go (`Armed → Running`): an 880Hz square burst,
 *     ~800ms (long enough to read as the "go" over field noise). Pitch/length still honour the
 *     round's `StartProcedure.tone` cue when configured.
 *   • {@link RaceAudioPlayer.playCountdownBeep} — the end-of-race countdown pips at remaining
 *     5…1s: same pitch family as the start (880Hz), short (~150ms).
 *   • {@link RaceAudioPlayer.playRaceEndTone} — remaining 0: the race-end buzzer — LOWER (half
 *     the countdown pitch, 440Hz) and longer (~600ms). It marks the **window** end; grace-window
 *     laps still count after it (scoring is untouched).
 *
 * ── Informational layer — mute-scoped ("Callouts") ───────────────────────────────────────────
 * The per-crossing chatter. Useful, but optional — the RD can silence it without losing the
 * procedure tones:
 *   • {@link RaceAudioPlayer.playCrossingBeep} — a very short, higher pip (1760Hz, ~60ms) per
 *     gate **crossing** (#397), distinct from the procedure family. One pip per crossing —
 *     holeshot, counted lap, and a pass the min-lap floor rejected alike — because "the gate saw
 *     nothing" and "the gate saw something that didn't count" must stop being the same silence.
 *     A phantom crossing on an unseated node pips too: that is the point, not noise.
 *     Consecutive pips are **staggered** on the audio clock (see {@link CROSSING_SPACING_MS}) so a
 *     pack crossing together reads as N pips rather than one stacked blip.
 * The spoken lap callouts (Web Speech) live in `callouts.ts` and are gated by the same
 * {@link RaceAudioPlayer.muted} preference at the call site. The preference persists to
 * `localStorage` under a **new** key ({@link MUTE_STORAGE_KEY}) — the old start-tone mute pref is
 * deliberately ignored, since its meaning changed (it used to silence the procedure tones).
 *
 * ── Autoplay policy ──────────────────────────────────────────────────────────────────────────
 * Browsers suspend a freshly-created `AudioContext` until a user gesture. The player lazily
 * creates the context on first use and {@link RaceAudioPlayer.resume}s it; the console calls
 * `resume()` from the RD's control clicks (Stage/Start), the callouts toggle, **and a one-time
 * first-gesture listener** (with the old tone toggle gone, any first pointerdown unlocks audio so
 * the procedure tones are never blocked). Play calls on a still-suspended context resume first and
 * schedule against the *resumed* clock (the suspended-clock fix). If the context can't be created
 * (no Web Audio) every call is a silent no-op — audio never blocks the UI.
 *
 * ── Testability ──────────────────────────────────────────────────────────────────────────────
 * The `AudioContext` constructor is **injected** ({@link RaceAudioPlayerOptions.audioContextFactory})
 * — jsdom has no Web Audio — so a unit test passes a mock and asserts which tones fired, at what
 * pitch/length, and that mute gates exactly the informational layer.
 */

/**
 * The persisted callouts-mute preference key (localStorage). Default unset ⇒ unmuted (callouts
 * on). Deliberately NOT the old `gridfpv.startTone.muted` key: that pref silenced the *procedure*
 * tones, which are now always-on — carrying it over would silently mute the new callouts layer for
 * anyone who once muted the test tone.
 */
const MUTE_STORAGE_KEY = 'gridfpv.callouts.muted';

/** Default race-go tone when the round carries no `StartProcedure.tone` — a confident 880Hz "go".
 * 800ms (doubled from the original 400ms) so it carries over field noise as a proper start horn. */
const START_HZ = 880;
const START_MS = 800;
/** The end-of-race countdown pips (remaining 5…1s): the start-tone pitch family, short. */
const COUNTDOWN_HZ = 880;
const COUNTDOWN_MS = 150;
/** The race-end buzzer (remaining 0): half the countdown pitch, longer — unmistakably "time". */
const RACE_END_HZ = COUNTDOWN_HZ / 2;
const RACE_END_MS = 600;
/** The per-crossing pip (informational, mute-scoped): higher + very short, a distinct voice. */
const CROSSING_HZ = 1760;
const CROSSING_MS = 60;
/**
 * The minimum gap between two crossing pips on the audio clock. Eight seats can cross within a
 * second, and Web Audio bursts scheduled at the *same* `currentTime` sum into ONE louder blip
 * instead of eight countable pips (they also stack toward clipping). So each pip claims the next
 * free slot at least this far after the previous one. 80ms > the 60ms burst, so the pips are
 * separated by silence and stay individually countable.
 */
const CROSSING_SPACING_MS = 80;
/**
 * How far ahead the pip queue may run before further pips are DROPPED. A pip is awareness — "the
 * gate just saw someone" — and awareness has a shelf life: a pip arriving a second after the
 * crossing tells the RD nothing and, worse, a long trailing string turns a burst into a
 * machine-gun the RD has to work through. 600ms of lookahead admits ~8 pips (one full heat's
 * simultaneous crossings) and drops the rest of a pile-up on the floor.
 */
const CROSSING_MAX_LOOKAHEAD_MS = 600;

/** The minimal `AudioContext` surface the player drives (so a test mock implements just this). */
export interface ToneAudioContext {
  readonly currentTime: number;
  readonly state: string;
  createOscillator(): {
    type: OscillatorType;
    frequency: { setValueAtTime(value: number, atTime: number): void };
    connect(destination: unknown): void;
    start(when?: number): void;
    stop(when?: number): void;
  };
  createGain(): {
    gain: {
      setValueAtTime(value: number, atTime: number): void;
      linearRampToValueAtTime(value: number, atTime: number): void;
    };
    connect(destination: unknown): void;
  };
  readonly destination: unknown;
  resume(): Promise<void>;
  close(): Promise<void>;
}

/** The start-tone cue — the round's `StartProcedure.tone`, both fields optional (defaults fill). */
export interface ToneCue {
  hz?: number;
  ms?: number;
}

export interface RaceAudioPlayerOptions {
  /**
   * Inject the `AudioContext` constructor (defaults to the platform one). A test passes a mock so
   * the tones are observable without real audio; production omits it. Returning a context that
   * throws on construction is handled as "no audio" (silent no-op).
   */
  audioContextFactory?: () => ToneAudioContext;
  /** Start with callouts muted regardless of storage (tests). Otherwise the persisted pref reads. */
  initialMuted?: boolean;
}

/** Read the platform `AudioContext` constructor, or `undefined` where Web Audio is unavailable. */
function platformAudioContext(): (() => ToneAudioContext) | undefined {
  const Ctor =
    (globalThis as unknown as { AudioContext?: new () => ToneAudioContext }).AudioContext ??
    (globalThis as unknown as { webkitAudioContext?: new () => ToneAudioContext })
      .webkitAudioContext;
  if (!Ctor) return undefined;
  return () => new Ctor();
}

function loadMuted(): boolean {
  try {
    return globalThis.localStorage?.getItem(MUTE_STORAGE_KEY) === 'true';
  } catch {
    return false;
  }
}

function persistMuted(muted: boolean): void {
  try {
    globalThis.localStorage?.setItem(MUTE_STORAGE_KEY, muted ? 'true' : 'false');
  } catch {
    /* storage unavailable — in-memory still works */
  }
}

/**
 * The race-day audio player. One instance per console (created in the live screen). Holds the
 * lazily-created `AudioContext`, the persisted callouts-mute state, and the tone methods above.
 */
export class RaceAudioPlayer {
  /**
   * The RD's callouts-mute preference — scoped to the **informational layer only** (the crossing
   * pip here + the spoken callouts at the call site). Procedure tones ignore it. A plain
   * (non-rune) field so this player is unit-testable as a pure class outside a component; the live
   * screen mirrors it into its own `$state` for the toolbar toggle's reactivity.
   */
  #muted = false;
  #factory: (() => ToneAudioContext) | undefined;
  #ctx: ToneAudioContext | undefined;
  /**
   * The next free slot on the audio clock for a **crossing pip** (seconds, in the context's own
   * time base). Staggering is per-player, not per-call, which is what makes a burst countable —
   * see {@link CROSSING_SPACING_MS}. Always clamped forward to `currentTime`, so it can never
   * schedule in the past after a quiet stretch.
   */
  #nextPipAt = 0;

  constructor(opts?: RaceAudioPlayerOptions) {
    this.#factory = opts?.audioContextFactory ?? platformAudioContext();
    this.#muted = opts?.initialMuted ?? loadMuted();
  }

  /** Whether the informational layer (crossing pips + callouts) is currently muted. */
  get muted(): boolean {
    return this.#muted;
  }

  /** Whether this environment can play a tone at all (Web Audio is available). */
  get available(): boolean {
    return this.#factory !== undefined;
  }

  /** Set the callouts-mute preference and persist it. */
  setMuted(muted: boolean): void {
    this.#muted = muted;
    persistMuted(muted);
  }

  /** Flip the callouts-mute preference and persist it; returns the new state. */
  toggleMuted(): boolean {
    this.setMuted(!this.#muted);
    return this.#muted;
  }

  /** Lazily create the `AudioContext` (once), or `undefined` if unavailable / construction fails. */
  #context(): ToneAudioContext | undefined {
    if (this.#ctx) return this.#ctx;
    if (!this.#factory) return undefined;
    try {
      this.#ctx = this.#factory();
    } catch {
      this.#factory = undefined; // don't retry a broken constructor
      this.#ctx = undefined;
    }
    return this.#ctx;
  }

  /**
   * Unlock the audio context on a user gesture (autoplay policy). Safe to call repeatedly; resumes
   * a suspended context so a later procedure tone is audible. A no-op where audio is unavailable.
   */
  async resume(): Promise<void> {
    const ctx = this.#context();
    if (!ctx) return;
    try {
      if (ctx.state !== 'running') await ctx.resume();
    } catch {
      /* resume can reject before a gesture — the next gesture retries */
    }
  }

  /**
   * The race-go tone — a **procedure tone**, plays unconditionally (never muted). The cue's
   * `hz`/`ms` fall back to the {@link START_HZ}/{@link START_MS} defaults. Never throws.
   */
  playStartTone(cue?: ToneCue): void {
    this.#play(cue?.hz ?? START_HZ, cue?.ms ?? START_MS, false);
  }

  /** One end-of-race countdown pip (remaining 5…1s) — a **procedure tone**, never muted. */
  playCountdownBeep(): void {
    this.#play(COUNTDOWN_HZ, COUNTDOWN_MS, false);
  }

  /** The race-end buzzer (remaining 0; lower + longer) — a **procedure tone**, never muted. */
  playRaceEndTone(): void {
    this.#play(RACE_END_HZ, RACE_END_MS, false);
  }

  /**
   * The per-**crossing** pip — **informational**, silenced by the callouts mute. Fires once per
   * gate crossing (holeshot / counted / rejected-too-short / voided alike); the caller keys
   * novelty on the crossing's `pass_ref`, never on a frame arriving. Staggered against the other
   * pips in flight so a pack reads as separate pips; a pip that would land beyond
   * {@link CROSSING_MAX_LOOKAHEAD_MS} is dropped rather than trailing the race.
   */
  playCrossingBeep(): void {
    this.#play(CROSSING_HZ, CROSSING_MS, true, true);
  }

  /**
   * Play one tone now. No Web Audio ⇒ no-op; `gatedByMute` tones are dropped while muted. Never
   * throws.
   *
   * ── Why this resumes-then-schedules (the audible-tone fix) ──────────────────────────────────
   * Race-go (`Armed → Running`) is an **auto** transition the runtime drives — there's no click on
   * that edge — so the `AudioContext` may still be **suspended** by the browser autoplay policy. A
   * suspended context has a **frozen** `currentTime`; scheduling a note against that frozen clock
   * and only resuming afterwards means the note's start time is already in the past when audio
   * actually begins, so it never sounds (the original "no tone" bug). We therefore **`resume()`
   * first and schedule the note from the resumed clock**. The console also unlocks on the earlier
   * Start gesture / first pointerdown ({@link resume}) so this path is usually instant.
   */
  #play(hz: number, ms: number, gatedByMute: boolean, stagger = false): void {
    if (gatedByMute && this.#muted) return;
    const ctx = this.#context();
    if (!ctx) return;
    if (ctx.state === 'running') {
      this.#schedule(ctx, hz, ms, stagger);
      return;
    }
    // Suspended (autoplay policy): resume, then schedule against the *resumed* clock. A mute-gated
    // tone re-checks the mute at fire time in case it was toggled during the (brief) resume.
    void ctx
      .resume()
      .then(() => {
        if (!gatedByMute || !this.#muted) this.#schedule(ctx, hz, ms, stagger);
      })
      .catch(() => {
        /* still locked (no gesture yet) — nothing to play; the next gesture unlocks it */
      });
  }

  /**
   * Pick the burst's start time on the context clock and emit it. A **procedure** tone plays
   * NOW — it is the race signal and must never be delayed behind chatter. A **staggered** tone
   * (the crossing pip) takes the next free slot so simultaneous pips do not stack into one blip;
   * if the queue has already run past {@link CROSSING_MAX_LOOKAHEAD_MS} the pip is dropped (a
   * stale pip is noise, not awareness).
   */
  #schedule(ctx: ToneAudioContext, hz: number, ms: number, stagger: boolean): void {
    const now = ctx.currentTime;
    if (!stagger) {
      this.#emit(ctx, hz, ms, now);
      return;
    }
    const at = Math.max(now, this.#nextPipAt);
    if (at - now > CROSSING_MAX_LOOKAHEAD_MS / 1000) return;
    this.#nextPipAt = at + CROSSING_SPACING_MS / 1000;
    this.#emit(ctx, hz, ms, at);
  }

  /** Build and fire the oscillator → gain → destination burst on a running context. Never throws. */
  #emit(ctx: ToneAudioContext, hz: number, ms: number, at: number): void {
    try {
      const now = at;
      const dur = Math.max(0.05, ms / 1000);
      const osc = ctx.createOscillator();
      const gain = ctx.createGain();
      osc.type = 'square';
      osc.frequency.setValueAtTime(hz, now);
      // A short attack + release so the burst reads as a clean beep, not a click. The audible
      // sustain level is 0.25 (non-zero — the bug would be a zero envelope or a missing connect).
      gain.gain.setValueAtTime(0.0001, now);
      gain.gain.linearRampToValueAtTime(0.25, now + 0.01);
      gain.gain.linearRampToValueAtTime(0.0001, now + dur);
      osc.connect(gain);
      gain.connect(ctx.destination);
      osc.start(now);
      osc.stop(now + dur + 0.02);
    } catch {
      /* a flaky audio backend must never break the live screen */
    }
  }

  /** Tear down the audio context (on unmount). Idempotent; never throws. */
  dispose(): void {
    try {
      void this.#ctx?.close();
    } catch {
      /* ignore */
    }
    this.#ctx = undefined;
    this.#nextPipAt = 0;
  }
}
