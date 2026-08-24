import { describe, expect, it } from 'vitest';
import type { Timer, TimerKind, TimerStatus } from '@gridfpv/types';
import {
  connectActionLabel,
  connectionHint,
  isConnectable,
  isManuallyHeld,
  isTimerConnected,
  kindLabel,
  kindSummary,
  kindTag,
  kindTone
} from '../src/lib/timers.js';

/** Build a Timer with the given status (the only field `isTimerConnected` reads). */
function timerWith(status: TimerStatus): Timer {
  return {
    id: 't',
    name: 'T',
    kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
    status,
    channel_capability: 'Flexible',
    node_count: 8,
    available_channels: [],
    manual_connect: false
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

describe('manual connect hold (#383)', () => {
  /** Build a RotorHazard timer with the given hold + status. */
  function rh(manual_connect: boolean, status: TimerStatus = 'Configured'): Timer {
    return { ...timerWith(status), manual_connect };
  }
  const mock = {
    ...timerWith('Ready'),
    id: 'mock',
    name: 'Mock',
    kind: { Mock: { laps: 3, lap_ms: 30000 } }
  } as Timer;

  it('offers the control for a RotorHazard timer only — a Mock has nothing to dial', () => {
    expect(isConnectable(rh(false))).toBe(true);
    // The Director answers a Mock's connect with a 400; the control is not offered at all.
    expect(isConnectable(mock)).toBe(false);
  });

  it('does not offer the control for an unmodeled (newer-Director) kind', () => {
    const future = {
      ...timerWith('Configured'),
      kind: { RhPlugin: { url: 'http://rig:5055' } } as unknown as TimerKind
    } as Timer;
    expect(isConnectable(future)).toBe(false);
    expect(isManuallyHeld({ ...future, manual_connect: true })).toBe(false);
  });

  it('labels the button from the HOLD, not the status — so it cannot flicker mid-retry', () => {
    // The dialer oscillates Connecting → Error while retrying a bad URL. The button must stay
    // "Disconnect" throughout, because the RD's intent (the hold) has not changed.
    for (const status of ['Connecting', 'Error', 'Disconnected', 'Connected'] as TimerStatus[]) {
      expect(connectActionLabel(rh(true, status))).toBe('Disconnect');
      expect(connectActionLabel(rh(false, status))).toBe('Connect');
    }
  });

  it('reads a held timer’s status back in the RD’s vocabulary', () => {
    expect(connectionHint(rh(true, 'Connected'))).toContain('Reachable');
    // The failure case names what to check — the whole point of the control at a venue.
    expect(connectionHint(rh(true, 'Error'))).toContain('Check the URL');
    expect(connectionHint(rh(true, 'Connecting'))).toContain('Connecting');
    // A just-held timer still reads its resting `Configured` until the reconciler's next tick.
    expect(connectionHint(rh(true, 'Configured'))).toContain('Connecting');
    expect(connectionHint(rh(true, 'Disconnected'))).toContain('dropped');
  });

  it('says nothing when there is no hold — the pill already carries the state', () => {
    expect(connectionHint(rh(false, 'Error'))).toBeUndefined();
    expect(connectionHint({ ...mock, manual_connect: true })).toBeUndefined();
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
