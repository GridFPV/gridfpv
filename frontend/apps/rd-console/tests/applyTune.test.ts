import { describe, expect, it, vi } from 'vitest';
import type { CompetitorRef, Timer, TimerSignal } from '@gridfpv/types';
import {
  CONFIRM_POLL_MS,
  applySummary,
  applyTuneGate,
  calibrationFor,
  confirmCalibration,
  type ApplyGateInput
} from '../src/lib/applyTune.js';

/**
 * "Apply a discovered tune to the timer" (#470) — the gate and the readback.
 *
 * Two things this must never do, and they are what most of these tests are about:
 *
 *  1. **Write to the wrong gate.** The node index is recovered from the marshaled heat's lineup
 *     position; if it cannot be, the answer is a refusal, never a guess. Calibrating the node the
 *     pilot next to them flew is worse than doing nothing.
 *  2. **Claim a write landed when it did not.** `POST /calibration` answers "accepted" and
 *     RotorHazard never echoes a level set, so `confirmed` may only ever come from the timer
 *     REPORTING the levels back (#403's failure class).
 *
 * And one it must always do: say WHY when it refuses (#405) — a disabled control with no reason is
 * the dead end that gate exists to prevent, so every `allowed: false` carries copy.
 */

const ALICE = 'ALICE' as CompetitorRef;
const BOB = 'BOB' as CompetitorRef;

function rhTimer(over: Partial<Timer> = {}): Timer {
  return {
    id: 'rh-1',
    name: 'Gate timer',
    kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
    status: 'Connected',
    channel_capability: 'Flexible',
    node_count: 8,
    available_channels: [],
    manual_connect: false,
    calibration: [],
    disabled_nodes: [],
    ...over
  } as Timer;
}

function input(over: Partial<ApplyGateInput> = {}): ApplyGateInput {
  return {
    canControl: true,
    timer: rhTimer(),
    lineup: [BOB, ALICE], // ALICE flew seat index 1 → "Node 2"
    competitor: ALICE,
    enter: 110,
    exit: 90,
    livePhase: undefined,
    liveHeatKind: undefined,
    ...over
  };
}

describe('applyTuneGate — which node, and may we write it', () => {
  it('resolves the node from the marshaled heat’s LINEUP POSITION', () => {
    // The Director re-attributes a node-seat trace to `lineup[n]`, so the competitor's index in
    // the lineup is the seat index the calibration request addresses. This inversion is the whole
    // reason a competition heat (whose trace is keyed on a pilot, not a seat) can be applied at all.
    const gate = applyTuneGate(input());
    expect(gate.allowed).toBe(true);
    if (!gate.allowed) return;
    expect(gate.target.node).toBe(1);
    expect(gate.target.timer).toBe('rh-1');
    // Named, never numbered (CLAUDE.md) — and 1-based, the way the RD reads it.
    expect(gate.target.nodeName).toBe('Node 2');
  });

  it('names the node with its channel when one is known', () => {
    const gate = applyTuneGate(
      input({
        nodeMhz: 5880,
        catalog: [{ band: 'Raceband', channel: 'R7', mhz: 5880 }] as ApplyGateInput['catalog']
      })
    );
    expect(gate.allowed && gate.target.nodeName).toContain('Node 2');
    expect(gate.allowed && gate.target.nodeName).toContain('Raceband R7');
  });

  it('refuses — with a reason — a read-only session', () => {
    const gate = applyTuneGate(input({ canControl: false }));
    expect(gate.allowed).toBe(false);
    expect(!gate.allowed && gate.reason).toMatch(/read-only/i);
  });

  it('refuses levels that detect nothing (enter must be above exit)', () => {
    for (const [enter, exit] of [
      [90, 90],
      [80, 120]
    ]) {
      const gate = applyTuneGate(input({ enter, exit }));
      expect(gate.allowed).toBe(false);
      expect(!gate.allowed && gate.reason).toMatch(/Enter must be above exit/);
    }
  });

  it('refuses when the event has no primary timer', () => {
    const gate = applyTuneGate(input({ timer: undefined }));
    expect(!gate.allowed && gate.reason).toMatch(/no primary timer/i);
  });

  it('refuses a Mock — there is no gate behind it', () => {
    const gate = applyTuneGate(
      input({ timer: rhTimer({ kind: { Mock: { laps: 3, lap_ms: 30_000 } }, status: 'Ready' }) })
    );
    expect(gate.allowed).toBe(false);
    expect(!gate.allowed && gate.reason).toMatch(/Mock/);
  });

  it('refuses a timer that is not connected, and names the state it is in', () => {
    for (const status of ['Disconnected', 'Configured', 'Error', 'Connecting'] as const) {
      const gate = applyTuneGate(input({ timer: rhTimer({ status }) }));
      expect(gate.allowed).toBe(false);
      expect(!gate.allowed && gate.reason).toContain(status.toLowerCase());
    }
  });

  it('refuses rather than guessing when the competitor is not in the lineup', () => {
    // THE dangerous one: no lineup entry means no way back to a node. Picking one anyway would
    // calibrate a gate some other pilot flew.
    expect(applyTuneGate(input({ lineup: [BOB] })).allowed).toBe(false);
    expect(applyTuneGate(input({ lineup: undefined })).allowed).toBe(false);
    expect(applyTuneGate(input({ lineup: [] })).allowed).toBe(false);
    const gate = applyTuneGate(input({ lineup: undefined }));
    expect(!gate.allowed && gate.reason).toMatch(/can’t tell which node/i);
  });

  it('refuses a seat the timer does not have', () => {
    // A lineup longer than the timer is wide: the Director would reject the write, and RotorHazard
    // would log-and-ignore it, so the RD must never be offered the button.
    const gate = applyTuneGate(
      input({ timer: rhTimer({ node_count: 1, reported_nodes: undefined }) })
    );
    expect(gate.allowed).toBe(false);
    expect(!gate.allowed && gate.reason).toMatch(/does not report a node/i);
  });

  it('falls back to the reported width when the RD set no override, and fails closed with neither', () => {
    expect(
      applyTuneGate(input({ timer: rhTimer({ node_count: undefined, reported_nodes: 4 }) })).allowed
    ).toBe(true);
    expect(
      applyTuneGate(input({ timer: rhTimer({ node_count: undefined, reported_nodes: undefined }) }))
        .allowed
    ).toBe(false);
  });

  it('refuses a node the RD has disabled', () => {
    const gate = applyTuneGate(input({ timer: rhTimer({ disabled_nodes: [1] }) }));
    expect(gate.allowed).toBe(false);
    expect(!gate.allowed && gate.reason).toMatch(/disabled/i);
  });

  it('refuses while a COMPETITION heat is running on the timer, and allows during practice', () => {
    // The shared `writeGate` rule, not a restatement of it — the Tune page and this panel must
    // never disagree about when a threshold write is refused.
    const running = applyTuneGate(input({ livePhase: 'Running', liveHeatKind: 'competition' }));
    expect(running.allowed).toBe(false);
    expect(!running.allowed && running.reason).toMatch(/competition heat is running/i);

    expect(applyTuneGate(input({ livePhase: 'Running', liveHeatKind: 'practice' })).allowed).toBe(
      true
    );
    // Not running = nothing to protect, which is the ordinary marshaling case.
    expect(
      applyTuneGate(input({ livePhase: 'Unofficial', liveHeatKind: 'competition' })).allowed
    ).toBe(true);
  });

  it('reports the most fundamental blocker first', () => {
    // A read-only session on a disconnected Mock with inverted levels is told about the thing that
    // makes every other fix pointless, not whichever check ran first.
    const gate = applyTuneGate(
      input({
        canControl: false,
        enter: 10,
        exit: 200,
        timer: rhTimer({ status: 'Disconnected', kind: { Mock: { laps: 1, lap_ms: 1 } } })
      })
    );
    expect(!gate.allowed && gate.reason).toMatch(/read-only/i);
  });
});

describe('calibrationFor', () => {
  it('sends BOTH levels — a re-detection produces a pair, not a single slider move', () => {
    expect(calibrationFor(2, 110, 90)).toEqual({ node: 2, enter_at: 110, exit_at: 90 });
  });

  it('clamps through the one rule every threshold editor uses (never 0, never 255)', () => {
    // Both ends of the naive 8-bit range silently no-op on RotorHazard — see tuning.ts.
    expect(calibrationFor(0, 0, -5)).toEqual({ node: 0, enter_at: 1, exit_at: 1 });
    expect(calibrationFor(0, 255, 300)).toEqual({ node: 0, enter_at: 254, exit_at: 254 });
  });

  it('rounds a fractional level from a drag', () => {
    expect(calibrationFor(1, 110.6, 89.4)).toEqual({ node: 1, enter_at: 111, exit_at: 89 });
  });
});

describe('confirmCalibration — the readback', () => {
  const signal = (node: number, enter?: number, exit?: number): TimerSignal =>
    ({
      timer: 'rh-1',
      streaming: true,
      lease_ms_remaining: 5000,
      period_micros: 200_000,
      sample_micros: [],
      nodes: [
        {
          node,
          seat: `node-${node}`,
          seen: true,
          crossing: false,
          crossed_recently: false,
          samples: [],
          enter_at: enter,
          exit_at: exit
        }
      ]
    }) as unknown as TimerSignal;

  /** A deps stub whose clock only advances when `sleep` is awaited. */
  function deps(responses: (() => Promise<TimerSignal>)[]) {
    let t = 0;
    let i = 0;
    return {
      now: () => t,
      sleep: vi.fn(async (ms: number) => {
        t += ms;
      }),
      fetchSignal: vi.fn(() =>
        (responses[Math.min(i++, responses.length - 1)] ?? (() => Promise.reject(new Error('x'))))()
      ),
      get elapsed() {
        return t;
      }
    };
  }

  it('confirms only once the TIMER reports both levels back', async () => {
    const d = deps([
      () => Promise.resolve(signal(1, 100, 80)), // still the old levels
      () => Promise.resolve(signal(1, 110, 90)) // …now it took
    ]);
    const out = await confirmCalibration(d, 1, 110, 90);
    expect(out.phase).toBe('confirmed');
    expect(d.fetchSignal).toHaveBeenCalledTimes(2);
    expect(d.sleep).toHaveBeenCalledWith(CONFIRM_POLL_MS);
  });

  it('does NOT confirm on a half-applied write', async () => {
    // Enter took, exit did not. Reporting success here is exactly the lie the readback exists for.
    const d = deps([() => Promise.resolve(signal(1, 110, 80))]);
    const out = await confirmCalibration(d, 1, 110, 90, 500);
    expect(out.phase).toBe('mismatch');
    expect(out.detail).toContain('exit 80');
  });

  it('calls it a mismatch — naming what the timer actually holds — when it never takes', async () => {
    const d = deps([() => Promise.resolve(signal(1, 100, 80))]);
    const out = await confirmCalibration(d, 1, 110, 90, 500);
    expect(out.phase).toBe('mismatch');
    expect(out.detail).toContain('enter 100');
    expect(out.detail).toContain('not 110 / 90');
  });

  it('says so plainly when the node reports no thresholds at all', async () => {
    const d = deps([() => Promise.resolve(signal(1, undefined, undefined))]);
    const out = await confirmCalibration(d, 1, 110, 90, 500);
    expect(out.phase).toBe('mismatch');
    expect(out.detail).toMatch(/not reporting/i);
  });

  it('rides out a dropped poll instead of calling it a failure', async () => {
    // One lost HTTP request proves nothing about the hardware — only the deadline decides.
    const d = deps([
      () => Promise.reject(new Error('network')),
      () => Promise.resolve(signal(1, 110, 90))
    ]);
    const out = await confirmCalibration(d, 1, 110, 90);
    expect(out.phase).toBe('confirmed');
  });

  it('reads the level off the RIGHT node', async () => {
    const other: TimerSignal = {
      ...signal(1, 110, 90),
      nodes: [...signal(0, 1, 1).nodes, ...signal(1, 110, 90).nodes]
    } as TimerSignal;
    const d = deps([() => Promise.resolve(other)]);
    expect((await confirmCalibration(d, 1, 110, 90)).phase).toBe('confirmed');
    const d2 = deps([() => Promise.resolve(other)]);
    expect((await confirmCalibration(d2, 0, 110, 90, 500)).phase).toBe('mismatch');
  });

  it('gives up at the timeout rather than polling forever', async () => {
    const d = deps([() => Promise.resolve(signal(1, 1, 1))]);
    const out = await confirmCalibration(d, 1, 110, 90, 1000);
    expect(out.phase).toBe('mismatch');
    expect(d.elapsed).toBeGreaterThanOrEqual(1000);
    // 1000ms / 250ms poll — bounded, not unbounded.
    expect(d.fetchSignal.mock.calls.length).toBeLessThanOrEqual(6);
  });

  it('treats a float readback as the same integer level (the wire carries floats)', async () => {
    const d = deps([() => Promise.resolve(signal(1, 110.0, 90.0))]);
    expect((await confirmCalibration(d, 1, 110, 90)).phase).toBe('confirmed');
  });
});

describe('applySummary', () => {
  it('says what happened, by friendly node name and callsign', () => {
    expect(applySummary({ phase: 'confirmed' }, 'Node 2', 'Maverick')).toBe(
      'Node 2 is now tuned to Maverick’s re-detected levels.'
    );
    expect(applySummary({ phase: 'mismatch' }, 'Node 2', 'Maverick')).toMatch(/did not take/);
    expect(applySummary({ phase: 'failed' }, 'Node 2', 'Maverick')).toMatch(/Couldn’t write/);
  });
});
