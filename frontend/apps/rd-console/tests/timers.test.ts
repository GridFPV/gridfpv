import { describe, expect, it } from 'vitest';
import type { Timer, TimerKind, TimerStatus } from '@gridfpv/types';
import { isTimerConnected, kindLabel, kindSummary, kindTag, kindTone } from '../src/lib/timers.js';

/** Build a Timer with the given status (the only field `isTimerConnected` reads). */
function timerWith(status: TimerStatus): Timer {
  return {
    id: 't',
    name: 'T',
    kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
    status,
    channel_capability: 'Flexible',
    node_count: 8,
    available_channels: []
  } as Timer;
}

describe('isTimerConnected', () => {
  it('counts a Ready (Mock) timer as connected', () => {
    expect(isTimerConnected(timerWith('Ready'))).toBe(true);
  });

  it('counts a Connected (live RotorHazard) timer as connected', () => {
    expect(isTimerConnected(timerWith('Connected'))).toBe(true);
  });

  it('does NOT count a Configured (not-yet-dialed-in) timer', () => {
    expect(isTimerConnected(timerWith('Configured'))).toBe(false);
  });

  it('does NOT count Connecting / Disconnected / Error', () => {
    expect(isTimerConnected(timerWith('Connecting'))).toBe(false);
    expect(isTimerConnected(timerWith('Disconnected'))).toBe(false);
    expect(isTimerConnected(timerWith('Error'))).toBe(false);
  });
});

describe('version skew: an unmodeled timer kind renders labeled, never crashes', () => {
  // A NEWER Director may ship a kind this console build doesn't know (the RH-plugin pivot
  // makes this likely). It must not mislabel as RotorHazard — and field access on
  // `kind.Rotorhazard` must never throw.
  const future = { RhPlugin: { url: 'http://rig:5055' } } as unknown as TimerKind;
  it('tags, labels and tones it as unknown', () => {
    expect(kindTag(future)).toBe('Unknown');
    expect(kindLabel(future)).toBe('RhPlugin');
    expect(kindTone(future)).toBe('neutral');
  });
  it('summarizes it with an update nudge instead of crashing', () => {
    expect(kindSummary(future)).toContain('update the console');
  });
});
