/**
 * Unit tests for the spoken lap-callout queue + text builder. jsdom has no
 * `speechSynthesis`/`SpeechSynthesisUtterance`, so a **fake speech seam** is injected; the tests
 * drive its `onend` to walk the queue and assert:
 *   • serialization — one utterance at a time, FIFO;
 *   • per-pilot supersede — a pilot's newer crossing replaces their still-waiting entry (the lap
 *     time is never stripped to "catch up"; other pilots' entries are untouched);
 *   • cancel — drops the backlog, stops the synth, and a stale in-flight `onend` cannot restart
 *     a second speaking chain;
 *   • the spoken text — resolved callsign + lap + hundredth-second time, name-skipped when
 *     unresolved.
 */
import { describe, expect, it, vi } from 'vitest';
import {
  CalloutQueue,
  formatLapSeconds,
  lapCalloutText,
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

    q.enqueue({ text: 'Maverick, lap 1, 21.40', key: 'mav' });
    q.enqueue({ text: 'Goose, lap 1, 22.05', key: 'goose' });

    // Only the first has been handed to the synth; the second waits its turn.
    expect(spoken).toEqual(['Maverick, lap 1, 21.40']);
    finish();
    expect(spoken).toEqual(['Maverick, lap 1, 21.40', 'Goose, lap 1, 22.05']);
    finish();
    expect(spoken).toHaveLength(2);
  });

  it('a newer same-key crossing supersedes the waiting entry (never the speaking one)', () => {
    const { speech, spoken, finish } = makeFakeSpeech();
    const q = new CalloutQueue(speech);

    // Maverick lap 1 starts speaking immediately; lap 2 queues behind Goose.
    q.enqueue({ text: 'Maverick, lap 1, 21.40', key: 'mav' });
    q.enqueue({ text: 'Goose, lap 1, 22.05', key: 'goose' });
    q.enqueue({ text: 'Maverick, lap 2, 20.90', key: 'mav' });
    // Maverick crosses AGAIN while lap 2 is still waiting: lap 2 is history — replaced by lap 3
    // at the back (crossing order), Goose untouched, the in-flight lap 1 not interrupted.
    q.enqueue({ text: 'Maverick, lap 3, 20.10', key: 'mav' });
    expect(q.depth).toBe(2);

    for (let i = 0; i < 4; i++) finish();
    expect(spoken).toEqual([
      'Maverick, lap 1, 21.40',
      'Goose, lap 1, 22.05',
      'Maverick, lap 3, 20.10'
    ]);
  });

  it('an unkeyed callout is never superseded', () => {
    const { speech, spoken, finish } = makeFakeSpeech();
    const q = new CalloutQueue(speech);

    q.enqueue({ text: 'first' });
    q.enqueue({ text: 'second' });
    q.enqueue({ text: 'third' });
    for (let i = 0; i < 3; i++) finish();
    expect(spoken).toEqual(['first', 'second', 'third']);
  });

  it('cancel drops the backlog and stops the synth', () => {
    const { speech, spoken, finish, cancelSpy } = makeFakeSpeech();
    const q = new CalloutQueue(speech);

    q.enqueue({ text: 'a full', key: 'a' });
    q.enqueue({ text: 'b full', key: 'b' });
    q.enqueue({ text: 'c full', key: 'c' });
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

    q.enqueue({ text: 'old full', key: 'old' });
    q.cancel();
    // A fresh callout starts a new chain after the cancel…
    q.enqueue({ text: 'new full', key: 'new' });
    q.enqueue({ text: 'newer full', key: 'newer' });
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
    q.enqueue({ text: 'a full', key: 'a' });
    q.enqueue({ text: 'b full', key: 'b' });

    pending.shift()?.onerror?.(); // 'a' errors mid-speech
    expect(spoken).toEqual(['a full', 'b full']);
  });

  it('is a silent no-op where the Web Speech API is unavailable', () => {
    const q = new CalloutQueue(undefined);
    expect(q.available).toBe(false);
    expect(() => q.enqueue({ text: 'x', key: 'x' })).not.toThrow();
    expect(() => q.cancel()).not.toThrow();
  });
});

describe('lapCalloutText / formatLapSeconds', () => {
  it('speaks the formatted seconds to the hundredth ("21.47")', () => {
    expect(formatLapSeconds(21_470_000)).toBe('21.47');
    expect(formatLapSeconds(21_474_900)).toBe('21.47');
    expect(formatLapSeconds(9_996_000)).toBe('10.00');
  });

  it('builds "<callsign>, lap N, M.SS"', () => {
    expect(lapCalloutText('Maverick', 3, 21_470_000)).toBe('Maverick, lap 3, 21.47');
  });

  it('skips the name when the resolver fell back (never speaks a raw ref)', () => {
    expect(lapCalloutText(undefined, 2, 30_000_000)).toBe('lap 2, 30.00');
  });

  it('skips the time when no lap time is carried', () => {
    expect(lapCalloutText('Goose', 1, undefined)).toBe('Goose, lap 1');
  });
});
