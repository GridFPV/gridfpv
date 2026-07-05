/**
 * Spoken **lap callouts** — the voice half of the informational audio layer (`raceAudio.ts` has
 * the beeps). On each recorded lap the console speaks "‹callsign›, lap ‹N›, ‹M.SS›" through the
 * Web Speech API (`speechSynthesis`), so the RD hears who crossed without looking down.
 *
 * ── Queue with per-pilot supersede (pack finishes) ───────────────────────────────────────────
 * Speech is slow (~1–2s per utterance) while crossings can arrive in a burst (a pack crossing the
 * gate together). Crossing *beeps* fire immediately per lap (raceAudio); the *speech* here is
 * serialized — one utterance at a time, in crossing order. The lap TIME is the payload of a
 * callout, so a backlog never strips it (the old "short form" coalescing spoke "‹callsign›,
 * lap ‹N›" with no time — which read as the voice being cut off). Instead, when a pilot crosses
 * again while their previous lap is still waiting, the stale entry is **superseded**: it is
 * dropped and the new lap queues at the back (announcing lap 3 when lap 4 already happened is
 * narrating history). That bounds the backlog at one entry per pilot, so the voice stays close to
 * live without ever swallowing a time. {@link CalloutQueue.cancel} drops the backlog and stops
 * the current utterance — called when the RD mutes the callouts or a *new* heat starts staging;
 * a natural race end lets the queue drain (the final laps are what everyone is waiting to hear).
 *
 * ── Testability ──────────────────────────────────────────────────────────────────────────────
 * jsdom has no `speechSynthesis`/`SpeechSynthesisUtterance`, so the platform calls sit behind an
 * injected {@link CalloutSpeech} seam; unit tests drive a fake synth's `onend` and assert the
 * FIFO/supersede/cancel behaviour. Where the platform has no speech at all, every call is a silent
 * no-op — callouts never block the UI.
 */

/** The minimal utterance surface the queue drives (a `SpeechSynthesisUtterance` in production). */
export interface SpeechUtteranceLike {
  text: string;
  onend: (() => void) | null;
  onerror: (() => void) | null;
}

/** The minimal `speechSynthesis` surface the queue drives. */
export interface SpeechSynthesisLike {
  speak(utterance: SpeechUtteranceLike): void;
  cancel(): void;
}

/** The injectable speech seam: the synth plus its utterance factory. */
export interface CalloutSpeech {
  synth: SpeechSynthesisLike;
  makeUtterance(text: string): SpeechUtteranceLike;
}

/**
 * The per-utterance WATCHDOG: if neither `onend` nor `onerror` fires within this window the
 * utterance is declared dead and the queue advances. Chrome's speech engine is known to
 * swallow an utterance silently — worst on its very first use after page load, while the TTS
 * backend spins up — and with a serialized queue one dead utterance used to dam every
 * subsequent callout (observed in the field as ~13s of race with no voice). A real lap
 * callout speaks in 1–2s; 6s is comfortably past any legitimate utterance.
 */
export const UTTERANCE_WATCHDOG_MS = 6_000;

/** One queued callout: the spoken text plus an optional supersede key (see {@link CalloutQueue}). */
export interface Callout {
  text: string;
  /**
   * The supersede key — the competitor ref for a lap callout. A newer enqueue with the same key
   * drops the older *waiting* entry (the currently-speaking utterance is never interrupted): the
   * pilot crossed again, so their previous lap is history. Omit for a callout that must never be
   * superseded.
   */
  key?: string;
}

/** Read the platform speech seam, or `undefined` where the Web Speech API is unavailable. */
function platformSpeech(): CalloutSpeech | undefined {
  const g = globalThis as unknown as {
    speechSynthesis?: SpeechSynthesisLike;
    SpeechSynthesisUtterance?: new (text: string) => SpeechUtteranceLike;
  };
  if (!g.speechSynthesis || !g.SpeechSynthesisUtterance) return undefined;
  const Utterance = g.SpeechSynthesisUtterance;
  const synth = g.speechSynthesis;
  return { synth, makeUtterance: (text) => new Utterance(text) };
}

/**
 * The serialized lap-callout speech queue. One instance per console (created in the live screen).
 * `enqueue` is safe to call from any crossing; utterances play one at a time in arrival order,
 * with a same-key entry superseding its stale waiting predecessor (see the module docs).
 */
export class CalloutQueue {
  #speech: CalloutSpeech | undefined;
  #queue: Callout[] = [];
  /** The utterance currently being spoken — the serialization token (also guards a stale `onend`
   * from a cancelled utterance pumping a second, concurrent chain). */
  #current: SpeechUtteranceLike | undefined;
  /** The current utterance's dead-engine watchdog timer (see {@link UTTERANCE_WATCHDOG_MS}). */
  #watchdog: ReturnType<typeof setTimeout> | undefined;

  constructor(speech?: CalloutSpeech) {
    this.#speech = speech ?? platformSpeech();
  }

  /** Whether this environment can speak at all (the Web Speech API is available). */
  get available(): boolean {
    return this.#speech !== undefined;
  }

  /** Waiting entries (excluding the utterance currently speaking). Exposed for tests/telemetry. */
  get depth(): number {
    return this.#queue.length;
  }

  /**
   * Queue one callout. A keyed entry supersedes a same-key entry still waiting — the stale one is
   * removed and the new one joins the back (crossing order). A silent no-op where speech is
   * unavailable. Never throws.
   */
  enqueue(callout: Callout): void {
    if (!this.#speech) return;
    if (callout.key !== undefined) {
      this.#queue = this.#queue.filter((c) => c.key !== callout.key);
    }
    this.#queue.push(callout);
    this.#pump();
  }

  /**
   * Drop the backlog and stop the current utterance — the RD muted the callouts, a new heat is
   * staging, or the screen unmounted; nothing queued is worth saying any more. Idempotent; never
   * throws.
   */
  cancel(): void {
    this.#queue = [];
    this.#current = undefined;
    if (this.#watchdog !== undefined) clearTimeout(this.#watchdog);
    this.#watchdog = undefined;
    try {
      this.#speech?.synth.cancel();
    } catch {
      /* a flaky speech backend must never break the live screen */
    }
  }

  /** Speak the next queued entry unless one is already speaking. */
  #pump(): void {
    if (!this.#speech || this.#current) return;
    const next = this.#queue.shift();
    if (!next) return;
    try {
      const utterance = this.#speech.makeUtterance(next.text);
      const done = () => {
        // A cancel() (or a newer utterance) may have superseded this one — a stale onend must not
        // pump a second, concurrent chain.
        if (this.#current !== utterance) return;
        if (this.#watchdog !== undefined) clearTimeout(this.#watchdog);
        this.#watchdog = undefined;
        this.#current = undefined;
        this.#pump();
      };
      utterance.onend = done;
      utterance.onerror = done;
      this.#current = utterance;
      // The watchdog: a cold/buggy TTS engine can swallow an utterance without EVER calling
      // back — declare it dead after the window, cancel the engine (unsticks Chrome), advance.
      if (this.#watchdog !== undefined) clearTimeout(this.#watchdog);
      this.#watchdog = setTimeout(() => {
        if (this.#current !== utterance) return;
        this.#watchdog = undefined;
        this.#current = undefined;
        try {
          this.#speech?.synth.cancel();
        } catch {
          /* unsticking a dead engine must never throw */
        }
        this.#pump();
      }, UTTERANCE_WATCHDOG_MS);
      this.#speech.synth.speak(utterance);
    } catch {
      // The utterance never started — clear the token so the queue isn't wedged.
      this.#current = undefined;
    }
  }

  /**
   * WARM UP the speech engine: speak a single space at volume 0 so the TTS backend loads its
   * voices and spins up BEFORE the first real callout (the engine's first-ever utterance can
   * take seconds — or die silently — while it initializes; see the watchdog). Called from the
   * console's one-time audio-unlock gesture. A no-op where speech is unavailable; never throws.
   */
  warmUp(): void {
    if (!this.#speech || this.#current) return;
    try {
      const u = this.#speech.makeUtterance(' ');
      (u as { volume?: number }).volume = 0;
      this.#speech.synth.speak(u);
    } catch {
      /* warm-up is best-effort */
    }
  }
}

/** Format a lap duration (µs) the way it is spoken: seconds to the hundredth, e.g. `"21.47"`. */
export function formatLapSeconds(micros: number): string {
  return (micros / 1_000_000).toFixed(2);
}

/**
 * Build the spoken text for one lap crossing: "‹callsign›, lap ‹N›, ‹M.SS›".
 *
 * `callsign` is the **resolved friendly name** (the shared competitor-name resolver) — pass
 * `undefined` when the resolver fell back to the raw ref, and the callout skips the name rather
 * than speaking an id (the friendly-names rule). `lastLapMicros` absent (no completed lap time)
 * skips the time.
 */
export function lapCalloutText(
  callsign: string | undefined,
  lap: number,
  lastLapMicros?: number
): string {
  const name = callsign ? `${callsign}, ` : '';
  const base = `${name}lap ${lap}`;
  return lastLapMicros != null ? `${base}, ${formatLapSeconds(lastLapMicros)}` : base;
}
