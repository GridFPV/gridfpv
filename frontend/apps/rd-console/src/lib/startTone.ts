/**
 * The start tone, played in the browser.
 *
 * The Director only says when the tone is due (`tone_at`). The console owns the sound:
 * three short pips in the last three seconds, then one longer tone at the start. Each
 * sound is scheduled on the Web Audio clock up front, so it lands on time even when the
 * page is busy.
 */

/** How long before the start each pip plays, in seconds. */
const PIP_OFFSETS_S = [3, 2, 1];
const PIP_HZ = 660;
const PIP_LENGTH_S = 0.12;
const TONE_HZ = 990;
const TONE_LENGTH_S = 0.6;
const VOLUME = 0.25;

type AudioContextConstructor = new () => AudioContext;

let context: AudioContext | undefined;

/** The shared audio context, or `undefined` where there is no Web Audio (tests, old browsers). */
function audio(): AudioContext | undefined {
  if (context) return context;
  const scope = globalThis as {
    AudioContext?: AudioContextConstructor;
    webkitAudioContext?: AudioContextConstructor;
  };
  const Constructor = scope.AudioContext ?? scope.webkitAudioContext;
  if (!Constructor) return undefined;
  try {
    context = new Constructor();
  } catch {
    return undefined;
  }
  return context;
}

/**
 * Wake the audio. Browsers keep sound off until the person clicks something, so call this
 * from a click (the Start button does). Safe to call any number of times.
 */
export function unlockAudio(): void {
  const ctx = audio();
  if (ctx?.state === 'suspended') void ctx.resume().catch(() => undefined);
}

/** Schedule one beep `inSeconds` from now. Returns the oscillator so it can be stopped. */
function beep(ctx: AudioContext, inSeconds: number, hz: number, length: number): OscillatorNode {
  const at = ctx.currentTime + inSeconds;
  const oscillator = ctx.createOscillator();
  const gain = ctx.createGain();
  oscillator.frequency.value = hz;
  // A short fade at each end, so the beep does not click.
  gain.gain.setValueAtTime(0, at);
  gain.gain.linearRampToValueAtTime(VOLUME, at + 0.01);
  gain.gain.setValueAtTime(VOLUME, at + length - 0.02);
  gain.gain.linearRampToValueAtTime(0, at + length);
  oscillator.connect(gain).connect(ctx.destination);
  oscillator.start(at);
  oscillator.stop(at + length);
  return oscillator;
}

/**
 * Schedule the countdown pips and the start tone for a start `msUntilTone` from now.
 * Anything already in the past is skipped, so a page opened mid-countdown plays what is
 * left. Returns a function that silences whatever has not played yet (the race was called
 * off, or the page moved on).
 */
export function scheduleStartTone(msUntilTone: number): () => void {
  const ctx = audio();
  if (!ctx) return () => undefined;
  unlockAudio();
  const untilTone = msUntilTone / 1000;
  const scheduled: OscillatorNode[] = [];
  for (const before of PIP_OFFSETS_S) {
    const inSeconds = untilTone - before;
    if (inSeconds >= 0) scheduled.push(beep(ctx, inSeconds, PIP_HZ, PIP_LENGTH_S));
  }
  // A tone that is already more than a moment late would only confuse, so it is skipped.
  if (untilTone > -0.25) scheduled.push(beep(ctx, Math.max(0, untilTone), TONE_HZ, TONE_LENGTH_S));
  return () => {
    for (const oscillator of scheduled) {
      try {
        oscillator.stop();
        oscillator.disconnect();
      } catch {
        // It already finished.
      }
    }
  };
}

/** Where a race is, as far as the tone cares. */
export type CuePhase = 'counting' | 'started' | 'off';

/**
 * Keeps the start tone in step with one race as the snapshot changes.
 *
 * - `counting`: the countdown is on; the pips and the tone are scheduled once per `tone_at`.
 * - `started`: the race began. Nothing is cancelled, so the tone rings out in full.
 * - `off`: there is no countdown (the race was called off, or there is no race). Anything
 *   still to play is silenced.
 */
export class StartCue {
  #armedFor: number | undefined;
  #cancel: () => void = () => undefined;
  #schedule: (msUntilTone: number) => () => void;

  constructor(schedule: (msUntilTone: number) => () => void = scheduleStartTone) {
    this.#schedule = schedule;
  }

  /**
   * Tell the cue where the race is. `toneAt` and `now` are on the Director's clock, in
   * microseconds; they are only read while counting.
   */
  update(phase: CuePhase, toneAt?: number, now?: number): void {
    if (phase === 'counting' && toneAt !== undefined && now !== undefined) {
      if (this.#armedFor === toneAt) return;
      this.#cancel();
      this.#armedFor = toneAt;
      this.#cancel = this.#schedule((toneAt - now) / 1000);
      return;
    }
    if (phase === 'off') this.#cancel();
    this.#armedFor = undefined;
    this.#cancel = () => undefined;
  }

  /** Silence anything still to play (the page is going away). */
  dispose(): void {
    this.update('off');
  }
}
