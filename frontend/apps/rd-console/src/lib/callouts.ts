/**
 * Spoken **lap callouts** — the voice half of the informational audio layer (`raceAudio.ts` has
 * the beeps). On each recorded lap the console speaks "‹callsign›, lap ‹N›, ‹M.S›" through the
 * Web Speech API (`speechSynthesis`), so the RD hears who crossed without looking down.
 *
 * ── Queue with coalescing (pack finishes) ────────────────────────────────────────────────────
 * Speech is slow (~1–2s per utterance) while crossings can arrive in a burst (a pack crossing the
 * gate together). Crossing *beeps* fire immediately per lap (raceAudio); the *speech* here is
 * serialized — one utterance at a time, FIFO — and when the backlog exceeds
 * {@link COALESCE_DEPTH} the queued entries fall back to their **short** form ("‹callsign›,
 * lap ‹N›", no lap time) so the voice catches back up instead of narrating history.
 * {@link CalloutQueue.cancel} drops the backlog and stops the current utterance — called when the
 * heat ends or the RD mutes the callouts.
 *
 * ── Testability ──────────────────────────────────────────────────────────────────────────────
 * jsdom has no `speechSynthesis`/`SpeechSynthesisUtterance`, so the platform calls sit behind an
 * injected {@link CalloutSpeech} seam; unit tests drive a fake synth's `onend` and assert the
 * FIFO/coalesce/cancel behaviour. Where the platform has no speech at all, every call is a silent
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

/** One queued callout: the full text, and the short form used when the queue is backed up. */
export interface CalloutTexts {
  full: string;
  short: string;
}

/**
 * Beyond this many waiting entries the queue speaks **short** forms to catch up (pack finishes
 * would otherwise leave the voice narrating laps long since crossed).
 */
const COALESCE_DEPTH = 3;

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
 * `enqueue` is safe to call from any crossing; utterances play one at a time in arrival order.
 */
export class CalloutQueue {
  #speech: CalloutSpeech | undefined;
  #queue: CalloutTexts[] = [];
  /** The utterance currently being spoken — the serialization token (also guards a stale `onend`
   * from a cancelled utterance pumping a second, concurrent chain). */
  #current: SpeechUtteranceLike | undefined;

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

  /** Queue one callout (FIFO). A silent no-op where speech is unavailable. Never throws. */
  enqueue(texts: CalloutTexts): void {
    if (!this.#speech) return;
    this.#queue.push(texts);
    this.#pump();
  }

  /**
   * Drop the backlog and stop the current utterance — the heat ended or the RD muted the
   * callouts; nothing queued is worth saying any more. Idempotent; never throws.
   */
  cancel(): void {
    this.#queue = [];
    this.#current = undefined;
    try {
      this.#speech?.synth.cancel();
    } catch {
      /* a flaky speech backend must never break the live screen */
    }
  }

  /** Speak the next queued entry unless one is already speaking. */
  #pump(): void {
    if (!this.#speech || this.#current) return;
    // Backed up beyond the coalesce depth (a pack finish): speak the short form to catch up.
    // Measured BEFORE dequeuing — "more than COALESCE_DEPTH waiting" is the backlog signal.
    const backlogged = this.#queue.length > COALESCE_DEPTH;
    const next = this.#queue.shift();
    if (!next) return;
    const text = backlogged ? next.short : next.full;
    try {
      const utterance = this.#speech.makeUtterance(text);
      const done = () => {
        // A cancel() (or a newer utterance) may have superseded this one — a stale onend must not
        // pump a second, concurrent chain.
        if (this.#current !== utterance) return;
        this.#current = undefined;
        this.#pump();
      };
      utterance.onend = done;
      utterance.onerror = done;
      this.#current = utterance;
      this.#speech.synth.speak(utterance);
    } catch {
      // The utterance never started — clear the token so the queue isn't wedged.
      this.#current = undefined;
    }
  }
}

/** Format a lap duration (µs) the way it is spoken: whole-plus-tenth seconds, e.g. `"21.4"`. */
export function formatLapSeconds(micros: number): string {
  return (micros / 1_000_000).toFixed(1);
}

/**
 * Build the spoken texts for one lap crossing: the full "‹callsign›, lap ‹N›, ‹M.S›" and the
 * short (coalesced) "‹callsign›, lap ‹N›".
 *
 * `callsign` is the **resolved friendly name** (the shared competitor-name resolver) — pass
 * `undefined` when the resolver fell back to the raw ref, and the callout skips the name rather
 * than speaking an id (the friendly-names rule). `lastLapMicros` absent (no completed lap time)
 * also degrades to the short form.
 */
export function lapCalloutTexts(
  callsign: string | undefined,
  lap: number,
  lastLapMicros?: number
): CalloutTexts {
  const name = callsign ? `${callsign}, ` : '';
  const short = `${name}lap ${lap}`;
  const full = lastLapMicros != null ? `${short}, ${formatLapSeconds(lastLapMicros)}` : short;
  return { full, short };
}
