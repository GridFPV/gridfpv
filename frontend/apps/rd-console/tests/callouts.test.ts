/**
 * Unit tests for the spoken lap-callout queue + text builder. jsdom has no
 * `speechSynthesis`/`SpeechSynthesisUtterance`, so a **fake speech seam** is injected; the tests
 * drive its `onend` to walk the queue and assert:
 *   • serialization — one utterance at a time, FIFO;
 *   • coalescing — a backlog deeper than 3 falls back to the short ("<callsign>, lap N") form;
 *   • cancel — drops the backlog, stops the synth, and a stale in-flight `onend` cannot restart
 *     a second speaking chain;
 *   • the spoken texts — resolved callsign + lap + seconds, name-skipped when unresolved.
 */
import { describe, expect, it, vi } from 'vitest';
import {
  CalloutQueue,
  formatLapSeconds,
  lapCalloutTexts,
  type CalloutSpeech,
  type SpeechUtteranceLike
} from '../src/lib/callouts.js';

/** A fake speech seam recording spoken texts; `finish()` fires the current utterance's onend. */
function makeFakeSpeech() {
  const spoken: string[] = [];
  const pending: SpeechUtteranceLike[] = [];
  const cancelSpy = vi.fn();
  const speech: CalloutSpeech = {
    synth: {
      speak(u) {
        spoken.push(u.text);
        pending.push(u);
      },
      cancel: cancelSpy
    },
    makeUtterance: (text) => ({ text, onend: null, onerror: null })
  };
  /** Complete the oldest still-speaking utterance (fires its onend). */
  const finish = () => pending.shift()?.onend?.();
  return { speech, spoken, finish, cancelSpy, pending };
}

describe('CalloutQueue', () => {
  it('speaks FIFO, one utterance at a time (the next only after the current ends)', () => {
    const { speech, spoken, finish } = makeFakeSpeech();
    const q = new CalloutQueue(speech);

    q.enqueue({ full: 'Maverick, lap 1, 21.4', short: 'Maverick, lap 1' });
    q.enqueue({ full: 'Goose, lap 1, 22.0', short: 'Goose, lap 1' });

    // Only the first has been handed to the synth; the second waits its turn.
    expect(spoken).toEqual(['Maverick, lap 1, 21.4']);
    finish();
    expect(spoken).toEqual(['Maverick, lap 1, 21.4', 'Goose, lap 1, 22.0']);
    finish();
    expect(spoken).toHaveLength(2);
  });

  it('coalesces to the short form once the backlog runs deeper than 3 (pack finish)', () => {
    const { speech, spoken, finish } = makeFakeSpeech();
    const q = new CalloutQueue(speech);

    // Six crossings land in a burst: #1 starts speaking immediately, #2..#6 queue up (depth 5).
    for (let n = 1; n <= 6; n++) {
      q.enqueue({ full: `pilot ${n} full`, short: `pilot ${n} short` });
    }
    for (let i = 0; i < 6; i++) finish();

    // #1 spoke full (nothing queued yet); #2/#3 dequeue over a >3 backlog → short (catch up);
    // by #4 the backlog is back to 3 → full again.
    expect(spoken).toEqual([
      'pilot 1 full',
      'pilot 2 short',
      'pilot 3 short',
      'pilot 4 full',
      'pilot 5 full',
      'pilot 6 full'
    ]);
  });

  it('cancel drops the backlog and stops the synth', () => {
    const { speech, spoken, finish, cancelSpy } = makeFakeSpeech();
    const q = new CalloutQueue(speech);

    q.enqueue({ full: 'a full', short: 'a' });
    q.enqueue({ full: 'b full', short: 'b' });
    q.enqueue({ full: 'c full', short: 'c' });
    expect(spoken).toEqual(['a full']);

    q.cancel();
    expect(cancelSpy).toHaveBeenCalled();
    expect(q.depth).toBe(0);

    // The cancelled utterance's late onend must not resurrect the dropped backlog.
    finish();
    expect(spoken).toEqual(['a full']);
  });

  it('a stale onend from a cancelled utterance cannot start a second, concurrent chain', () => {
    const { speech, spoken, finish } = makeFakeSpeech();
    const q = new CalloutQueue(speech);

    q.enqueue({ full: 'old full', short: 'old' });
    q.cancel();
    // A fresh callout starts a new chain after the cancel…
    q.enqueue({ full: 'new full', short: 'new' });
    q.enqueue({ full: 'newer full', short: 'newer' });
    expect(spoken).toEqual(['old full', 'new full']);

    // …then the OLD utterance's onend finally fires (real synths do this on cancel). It must be
    // ignored — 'newer' only speaks when 'new' itself ends.
    finish(); // old — stale, ignored
    expect(spoken).toEqual(['old full', 'new full']);
    finish(); // new — pumps newer
    expect(spoken).toEqual(['old full', 'new full', 'newer full']);
  });

  it('pumps past an utterance error (onerror) rather than wedging the queue', () => {
    const { speech, spoken, pending } = makeFakeSpeech();
    const q = new CalloutQueue(speech);
    q.enqueue({ full: 'a full', short: 'a' });
    q.enqueue({ full: 'b full', short: 'b' });

    pending.shift()?.onerror?.(); // 'a' errors mid-speech
    expect(spoken).toEqual(['a full', 'b full']);
  });

  it('is a silent no-op where the Web Speech API is unavailable', () => {
    const q = new CalloutQueue(undefined);
    expect(q.available).toBe(false);
    expect(() => q.enqueue({ full: 'x', short: 'x' })).not.toThrow();
    expect(() => q.cancel()).not.toThrow();
  });
});

describe('lapCalloutTexts / formatLapSeconds', () => {
  it('speaks the formatted seconds to one decimal ("21.4")', () => {
    expect(formatLapSeconds(21_400_000)).toBe('21.4');
    expect(formatLapSeconds(21_449_000)).toBe('21.4');
    expect(formatLapSeconds(9_960_000)).toBe('10.0');
  });

  it('builds "<callsign>, lap N, M.S" with the short "<callsign>, lap N" fallback', () => {
    expect(lapCalloutTexts('Maverick', 3, 21_400_000)).toEqual({
      full: 'Maverick, lap 3, 21.4',
      short: 'Maverick, lap 3'
    });
  });

  it('skips the name when the resolver fell back (never speaks a raw ref)', () => {
    expect(lapCalloutTexts(undefined, 2, 30_000_000)).toEqual({
      full: 'lap 2, 30.0',
      short: 'lap 2'
    });
  });

  it('degrades to the short form when no lap time is carried', () => {
    expect(lapCalloutTexts('Goose', 1, undefined)).toEqual({
      full: 'Goose, lap 1',
      short: 'Goose, lap 1'
    });
  });
});
