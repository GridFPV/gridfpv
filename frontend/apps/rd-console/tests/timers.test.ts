import { describe, expect, it } from 'vitest';
import type { Timer, TimerStatus } from '@gridfpv/types';
import { isTimerConnected } from '../src/lib/timers.js';

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
