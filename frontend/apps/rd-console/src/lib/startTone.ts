/**
 * The **start tone** (heat-lifecycle Slice 3) — the console's audible race-go cue, played the
 * moment a heat crosses `Armed → Running` so the RD (and anyone near the console) hears the start
 * synced to the race actually going live.
 *
 * It's a short oscillator burst over the **Web Audio API**: an `OscillatorNode` through a
 * `GainNode` with a tiny attack/release envelope so it reads as a clean beep, not a click. The
 * pitch/length come from the round's `StartProcedure.tone` config when present, else a sensible
 * default (an `880`Hz, `400`ms tone — a confident "go").
 *
 * ── Mute toggle (persisted) ──────────────────────────────────────────────────────────────────
 * The RD can mute the tone; the choice persists to `localStorage` (default **on** — a start cue is
 * wanted by default). {@link StartTonePlayer.muted} is a reactive getter so the toolbar toggle and
 * the player agree.
 *
 * ── Autoplay policy ──────────────────────────────────────────────────────────────────────────
 * Browsers suspend a freshly-created `AudioContext` until a user gesture. The player lazily creates
 * the context on first use and {@link StartTonePlayer.resume}s it; the console also calls `resume()`
 * from the first RD click (Stage/Start) so the context is unlocked well before race-go. If the
 * context can't be created (no Web Audio) every call is a silent no-op — the tone never blocks the UI.
 *
 * ── Testability ──────────────────────────────────────────────────────────────────────────────
 * The `AudioContext` constructor is **injected** ({@link StartTonePlayerOptions.audioContextFactory}),
 * so a unit test passes a mock and asserts an oscillator was started at race-go, that mute suppresses
 * it, and that the configured/default frequency is used — with no real audio hardware.
 */

/** The persisted mute-preference key (localStorage). Default unset ⇒ unmuted (tone on). */
const MUTE_STORAGE_KEY = 'gridfpv.startTone.muted';

/** Default tone when the round carries no `StartProcedure.tone` — a confident 880Hz "go". */
const DEFAULT_HZ = 880;
const DEFAULT_MS = 400;

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

/** The tone cue to play — the round's `StartProcedure.tone`, both fields optional (defaults fill). */
export interface ToneCue {
  hz?: number;
  ms?: number;
}

export interface StartTonePlayerOptions {
  /**
   * Inject the `AudioContext` constructor (defaults to the platform one). A test passes a mock so
   * `play()` is observable without real audio; production omits it. Returning a context that throws
   * on construction is handled as "no audio" (silent no-op).
   */
  audioContextFactory?: () => ToneAudioContext;
  /** Start muted regardless of storage (tests). Otherwise the persisted preference is read. */
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
 * The reactive start-tone player. One instance per console (created in the live screen). Holds the
 * lazily-created `AudioContext`, the persisted mute state, and the `play(cue)` that fires the beep.
 */
export class StartTonePlayer {
  /**
   * The RD's mute preference. A plain (non-rune) field so this player is unit-testable as a pure
   * class outside a component; the live screen mirrors it into its own `$state` for the toolbar
   * toggle's reactivity (re-reading {@link muted} after {@link toggleMuted}).
   */
  #muted = false;
  #factory: (() => ToneAudioContext) | undefined;
  #ctx: ToneAudioContext | undefined;

  constructor(opts?: StartTonePlayerOptions) {
    this.#factory = opts?.audioContextFactory ?? platformAudioContext();
    this.#muted = opts?.initialMuted ?? loadMuted();
  }

  /** Whether the tone is currently muted. */
  get muted(): boolean {
    return this.#muted;
  }

  /** Whether this environment can play a tone at all (Web Audio is available). */
  get available(): boolean {
    return this.#factory !== undefined;
  }

  /**
   * Whether the audio is currently **locked** — Web Audio is available but the context has not yet
   * reached the `running` state (the browser autoplay policy still has it suspended, or it hasn't
   * been created yet). Drives the toolbar's "audio enabled / locked" indicator so the RD can see at
   * a glance whether a race-go tone will actually sound, and prime it before the race if not. When
   * Web Audio is unavailable this reads `false` (nothing to unlock — the toolbar shows "no audio").
   */
  get locked(): boolean {
    if (!this.#factory) return false;
    return this.#ctx?.state !== 'running';
  }

  /** Set the mute preference and persist it. */
  setMuted(muted: boolean): void {
    this.#muted = muted;
    persistMuted(muted);
  }

  /** Flip the mute preference and persist it; returns the new state. */
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
   * a suspended context so the later race-go `play()` is audible. A no-op where audio is unavailable.
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
   * The explicit **"Enable sound / Test tone"** affordance: a definite user gesture that **unlocks
   * the context and plays one confirmation beep** so the RD can prime audio and *hear* that it works
   * before the race — the reliable escape hatch regardless of autoplay quirks. Unlike {@link play},
   * the confirmation beep is **not** gated by mute (the RD asked to test it; muting only suppresses
   * the automatic race-go tone). Resolves to `true` when the context is `running` afterwards (so the
   * caller can refresh its enabled/locked indicator). Never throws.
   */
  async enable(cue?: ToneCue): Promise<boolean> {
    await this.resume();
    const ctx = this.#context();
    if (!ctx) return false;
    if (ctx.state === 'running') this.#emit(ctx, cue);
    return ctx.state === 'running';
  }

  /**
   * Play the start tone now. Muted ⇒ no-op; no Web Audio ⇒ no-op. The cue's `hz`/`ms` fall back to
   * the {@link DEFAULT_HZ}/{@link DEFAULT_MS} defaults. Never throws.
   *
   * ── Why this resumes-then-schedules (the audible-tone fix) ──────────────────────────────────
   * Race-go (`Armed → Running`) is an **auto** transition the runtime drives — there's no click on
   * that edge — so the `AudioContext` may still be **suspended** by the browser autoplay policy. A
   * suspended context has a **frozen** `currentTime`; scheduling a note against that frozen clock and
   * only resuming afterwards means the note's start time is already in the past when audio actually
   * begins, so it never sounds (the original "no tone" bug). We therefore **`resume()` first and
   * schedule the note from the resumed clock**. The console also unlocks on the earlier `Start`
   * gesture ({@link resume}) so the context is usually already running and this path is instant.
   */
  play(cue?: ToneCue): void {
    if (this.#muted) return;
    const ctx = this.#context();
    if (!ctx) return;
    if (ctx.state === 'running') {
      this.#emit(ctx, cue);
      return;
    }
    // Suspended (autoplay policy): resume, then schedule against the *resumed* clock. Re-check mute
    // at fire time in case it was toggled during the (brief) resume.
    void ctx
      .resume()
      .then(() => {
        if (!this.#muted) this.#emit(ctx, cue);
      })
      .catch(() => {
        /* still locked (no gesture yet) — nothing to play; the next gesture unlocks it */
      });
  }

  /** Build and fire the oscillator → gain → destination burst on a running context. Never throws. */
  #emit(ctx: ToneAudioContext, cue?: ToneCue): void {
    const hz = cue?.hz ?? DEFAULT_HZ;
    const ms = cue?.ms ?? DEFAULT_MS;
    try {
      const now = ctx.currentTime;
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
  }
}
