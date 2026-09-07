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
    manual_connect: false,
    calibration: [],
    disabled_nodes: []
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

  it('tells the truth about a timer GridFPV has stopped dialling (#462)', () => {
    // `Unreachable` is a RESTING state, not a failing one: nothing is in flight and nothing will
    // be. Reading it as "retrying" (the `Disconnected` sentence) or leaving it with no sentence at
    // all would both promise an attempt that is not happening — so it has to end in the thing to
    // press, because pressing it is the only thing that will change anything.
    const hint = connectionHint(rh(true, 'Unreachable'));
    expect(hint).toContain('stopped trying');
    expect(hint).toContain('Connect');
    expect(hint).not.toContain('Retrying');
  });

  it('keeps the button on Disconnect while a timer rests, like every other status', () => {
    // The hold is the RD's intent and it has not changed just because GridFPV gave up dialling.
    expect(connectActionLabel(rh(true, 'Unreachable'))).toBe('Disconnect');
    expect(isTimerConnected(rh(true, 'Unreachable'))).toBe(false);
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

describe('the Simulator identity (#491)', () => {
  const SIM: Timer['kind'] = { Mock: { laps: 3, lap_ms: 2500 } };
  it('labels a Mock as Simulator with a warn tone, and the summary says what selecting it does', () => {
    expect(kindLabel(SIM)).toBe('Simulator');
    expect(kindTone(SIM)).toBe('warn');
    // The behavior IS the summary: an RD scanning the row must learn heats will race themselves.
    expect(kindSummary(SIM)).toMatch(/fly themselves/);
    expect(kindSummary(SIM)).toContain('3 laps');
  });
});

describe('the VelociDrone timer kind (#484)', () => {
  const VD: Timer['kind'] = { Velocidrone: { url: 'ws://192.168.1.20:60003/velocidrone' } };

  function vdTimer(status: TimerStatus, held = true): Timer {
    return { ...timerWith(status), kind: VD, manual_connect: held } as Timer;
  }

  it('tags, labels and tones it as a real external source — not as the Simulator', () => {
    expect(kindTag(VD)).toBe('Velocidrone');
    // The brand spelling, capital D. And NOT "Simulator": that is the built-in Mock's name here
    // (#491), and a VelociDrone timer is a real source whose heats do NOT fly themselves.
    expect(kindLabel(VD)).toBe('VelociDrone');
    expect(kindTone(VD)).toBe('info');
  });

  it('summarizes with its URL, and says so plainly when there is none', () => {
    expect(kindSummary(VD)).toBe('ws://192.168.1.20:60003/velocidrone');
    expect(kindSummary({ Velocidrone: { url: '' } } as TimerKind)).toBe('No URL set');
  });

  it('is connectable and holdable — it dials something, so Connect is a real question', () => {
    expect(isConnectable(vdTimer('Configured'))).toBe(true);
    expect(isManuallyHeld(vdTimer('Configured'))).toBe(true);
    expect(connectActionLabel(vdTimer('Configured'))).toBe('Disconnect');
    expect(connectActionLabel(vdTimer('Configured', false))).toBe('Connect');
  });

  it('counts as connected once its socket is up', () => {
    expect(isTimerConnected(vdTimer('Connected'))).toBe(true);
    expect(isTimerConnected(vdTimer('Configured'))).toBe(false);
  });

  // The three things that are actually wrong when a VelociDrone will not answer, none of which
  // an RD can guess from "could not reach this timer" — above all the loopback trap, which looks
  // like it must work when the sim is on the same machine.
  it('names the VelociDrone-specific things to check when it cannot be reached', () => {
    const hint = connectionHint(vdTimer('Error')) ?? '';
    expect(hint).toContain('Websocket Communication');
    expect(hint).toContain('LAN IP');
    expect(hint).toContain('127.0.0.1');
    expect(hint).not.toContain('RotorHazard');
  });

  it('says GridFPV has stopped trying, and ends on the button to press', () => {
    const hint = connectionHint(vdTimer('Unreachable')) ?? '';
    expect(hint).toContain('stopped trying');
    expect(hint).toContain('Websocket Communication');
    expect(hint).toMatch(/press Connect to try again\.$/);
  });

  it('still tells a RotorHazard RD to check RotorHazard, not the sim', () => {
    const rh = { ...timerWith('Error'), manual_connect: true } as Timer;
    const hint = connectionHint(rh) ?? '';
    expect(hint).toContain('RotorHazard is running');
    expect(hint).not.toContain('Websocket Communication');
  });
});
