/**
 * Unit tests for the pure half of the Tune page (#355, slice 2b) — `src/lib/tuning.ts`.
 *
 * The component test (`TunePage.test.ts`) proves the three editors stay locked to one value through
 * the DOM; this pins the rules that guarantee they *can*: the single clamp, the write gate, the
 * readback fold, and the label that keeps a raw seat/frequency off the screen.
 */
import { describe, expect, it } from 'vitest';
import type { ChannelCatalogEntry } from '@gridfpv/types';
import {
  RSSI_MAX,
  RSSI_MIN,
  adoptReported,
  clampLevel,
  foldReadback,
  isParsableLevel,
  nodeCountOf,
  nodeRefOf,
  nodeTraceOf,
  nodeTuneLabel,
  phaseLabel,
  phaseTone,
  readoutsOf,
  seedThreshold,
  writeGate,
  type ThresholdState,
  type TimerSignalNode
} from '../src/lib/tuning.js';

const CATALOG: ChannelCatalogEntry[] = [
  { band: 'Raceband', channel: 'R1', mhz: 5658 },
  { band: 'Raceband', channel: 'R7', mhz: 5880 },
  { band: 'Fatshark', channel: 'F4', mhz: 5800 }
];

function node(over: Partial<TimerSignalNode> = {}): TimerSignalNode {
  return { node: 0, samples: [], period_micros: 200_000, ...over };
}

describe('clampLevel — the ONE clamp, at the state', () => {
  it('rounds to an integer (RSSI is an ADC count, not a fraction)', () => {
    // The whole reason this lives in one place: if each control rounded for itself the box could
    // hold 90.4 while the slider sat at 90 and the graph drew a third position.
    expect(clampLevel(90.4)).toBe(90);
    expect(clampLevel(90.5)).toBe(91);
    expect(clampLevel('90.6')).toBe(91);
  });

  it('clamps the minimum to 1, never 0', () => {
    // RotorHazard's calibration.py tests `enter_at_level` for truthiness, so a 0 is read as "go
    // read the level off the node" — a typed 0 would look accepted and silently no-op.
    expect(clampLevel(0)).toBe(RSSI_MIN);
    expect(clampLevel('0')).toBe(1);
    expect(clampLevel(-40)).toBe(1);
  });

  it('clamps the maximum to the 8-bit ceiling', () => {
    expect(clampLevel(999)).toBe(RSSI_MAX);
    expect(clampLevel(255)).toBe(255);
  });

  it('resolves un-parseable input to the fallback rather than NaN', () => {
    // A NaN in the state would poison all three views of it at once.
    expect(clampLevel('')).toBe(RSSI_MIN);
    expect(clampLevel('abc', 88)).toBe(88);
    expect(clampLevel(undefined, 42)).toBe(42);
    expect(clampLevel(NaN, 42)).toBe(42);
  });

  it('is idempotent — clamping a clamped value changes nothing', () => {
    for (const raw of [-5, 0, 0.4, 90.5, 254.6, 1e6]) {
      expect(clampLevel(clampLevel(raw))).toBe(clampLevel(raw));
    }
  });
});

describe('isParsableLevel', () => {
  it('rejects the half-typed / emptied box so the state is left alone', () => {
    expect(isParsableLevel('')).toBe(false);
    expect(isParsableLevel('   ')).toBe(false);
    expect(isParsableLevel('-')).toBe(false);
    expect(isParsableLevel('abc')).toBe(false);
  });

  it('accepts anything numeric', () => {
    expect(isParsableLevel('0')).toBe(true);
    expect(isParsableLevel('90')).toBe(true);
    expect(isParsableLevel(' 90.5 ')).toBe(true);
  });
});

describe('writeGate — practice only, checked per write', () => {
  it('allows a write with no heat on the timer (the ordinary case)', () => {
    expect(writeGate(undefined, undefined).allowed).toBe(true);
    expect(writeGate('Scheduled', undefined).allowed).toBe(true);
  });

  it('allows a write while an OPEN PRACTICE heat is running', () => {
    // Practice is excluded from scoring (#398) — there is no result to corrupt, and pilots in the
    // air is the natural moment to tune.
    expect(writeGate('Running', 'practice').allowed).toBe(true);
  });

  it('refuses a write while a COMPETITION heat is running, and says why', () => {
    const gate = writeGate('Running', 'competition');
    expect(gate.allowed).toBe(false);
    expect(gate.allowed === false && gate.reason).toMatch(/competition heat is running/i);
  });

  it('allows a write in every non-Running phase, even a competition heat', () => {
    // Staged/Armed are pre-race and Unofficial/Final are past it: in none of those is the detector
    // deciding laps that count right now.
    for (const phase of ['Scheduled', 'Staged', 'Armed', 'Unofficial', 'Final'] as const) {
      expect(writeGate(phase, 'competition').allowed).toBe(true);
    }
  });
});

describe('seedThreshold / adoptReported', () => {
  it('seeds at rest from the level the timer reports', () => {
    expect(seedThreshold(90)).toEqual({ value: 90, confirmed: 90, phase: 'confirmed' });
  });

  it('clamps a bad reported level like any other input', () => {
    expect(seedThreshold(0)).toEqual({ value: 1, confirmed: 1, phase: 'confirmed' });
  });

  it('follows the hardware when the threshold is at rest (the RD tuned in RH’s own UI)', () => {
    const atRest: ThresholdState = { value: 90, confirmed: 90, phase: 'confirmed' };
    expect(adoptReported(atRest, 104)).toEqual({ value: 104, confirmed: 104, phase: 'confirmed' });
  });

  it('leaves a threshold the RD is adjusting alone', () => {
    // The next poll must never yank a value out from under a drag, an in-flight write, or a
    // failure the RD has not read yet.
    for (const phase of ['pending', 'sent', 'mismatch', 'failed', 'refused'] as const) {
      const busy: ThresholdState = { value: 120, confirmed: 90, phase };
      expect(adoptReported(busy, 104)).toBe(busy);
    }
  });

  it('is a no-op when the hardware already agrees', () => {
    const state: ThresholdState = { value: 90, confirmed: 90, phase: 'confirmed' };
    expect(adoptReported(state, 90)).toBe(state);
  });
});

describe('foldReadback — the confirmation that replaced the Apply button', () => {
  it('confirms when the readback matches what was sent', () => {
    const sending: ThresholdState = { value: 104, confirmed: 90, phase: 'sent' };
    expect(foldReadback(sending, 104, 104)).toEqual({
      value: 104,
      confirmed: 104,
      phase: 'confirmed'
    });
  });

  it('flags a MISMATCH — the write did not take — and names both levels', () => {
    // `set_enter_at_level` does not echo, so this comparison is the only evidence a write landed.
    // A silent divergence means tuning against a value the hardware never took.
    const sending: ThresholdState = { value: 104, confirmed: 90, phase: 'sent' };
    const folded = foldReadback(sending, 104, 90);
    expect(folded.phase).toBe('mismatch');
    expect(folded.confirmed).toBe(90);
    expect(folded.detail).toContain('90');
    expect(folded.detail).toContain('104');
  });

  it('keeps no undo value — the number is on screen and re-draggable', () => {
    const folded = foldReadback({ value: 104, confirmed: 90, phase: 'sent' }, 104, 104);
    expect(Object.keys(folded).sort()).toEqual(['confirmed', 'phase', 'value']);
  });
});

describe('phase presentation', () => {
  it('labels every phase in plain language, readable at arm’s length', () => {
    expect(phaseLabel('confirmed')).toBe('On timer');
    expect(phaseLabel('pending')).toBe('Adjusting');
    expect(phaseLabel('sent')).toBe('Sending…');
    expect(phaseLabel('mismatch')).toBe('Not taken');
    expect(phaseLabel('failed')).toBe('Failed');
    expect(phaseLabel('refused')).toBe('Not sent');
  });

  it('tones the settled state apart from the wrong ones', () => {
    expect(phaseTone('confirmed')).toBe('success');
    expect(phaseTone('mismatch')).toBe('danger');
    expect(phaseTone('failed')).toBe('danger');
    expect(phaseTone('refused')).toBe('warn');
  });
});

describe('nodeTuneLabel — CLAUDE.md: never a bare frequency, never a raw seat', () => {
  it('resolves the frequency to band + channel through channels.ts', () => {
    expect(nodeTuneLabel(0, 5880, CATALOG)).toBe('Node 1 · Raceband R7');
    expect(nodeTuneLabel(3, 5658, CATALOG)).toBe('Node 4 · Raceband R1');
  });

  it('is the 1-based seat alone when the node has no frequency yet', () => {
    expect(nodeTuneLabel(0, undefined, CATALOG)).toBe('Node 1');
  });

  it('never renders a bare raw seat ref', () => {
    expect(nodeTuneLabel(0, 5880, CATALOG)).not.toContain('node-0');
  });
});

describe('nodeCountOf', () => {
  it('prefers what the timer actually reports', () => {
    expect(nodeCountOf({ timer: 't', nodes: [node(), node({ node: 1 })] }, 8)).toBe(2);
  });

  it('lays out from the registry before the first snapshot lands', () => {
    expect(nodeCountOf(undefined, 4)).toBe(4);
    expect(nodeCountOf({ timer: 't', nodes: [] }, 4)).toBe(4);
  });
});

describe('nodeTraceOf — the adapter onto RssiGraph’s live mode', () => {
  it('keys the trace on the node SEAT, which is what the resolver resolves', () => {
    const trace = nodeTraceOf('rh-1', node({ node: 2, samples: [10, 20] }));
    expect(trace.competitor).toEqual({ adapter: 'rh-1', competitor: nodeRefOf(2) });
    expect(trace.samples).toEqual([10, 20]);
  });

  it('carries the levels the TIMER holds — the page overlays its pending value via `tuned`', () => {
    const trace = nodeTraceOf('rh-1', node({ enter_at_level: 90, exit_at_level: 80 }));
    expect(trace.enter).toBe(90);
    expect(trace.exit).toBe(80);
  });

  it('never yields a zero period (a zero would divide the whole projection by nothing)', () => {
    expect(nodeTraceOf('rh-1', node({ period_micros: 0 })).period_micros).toBe(1);
  });
});

describe('readoutsOf — the six stats, all from node_data', () => {
  it('reads the peaks/nadirs/count off node_data, not the heartbeat', () => {
    // `get_heartbeat_json` carries only current_rssi / frequency / loop_time / crossing_flag; a
    // page that looked for the peaks there would render six permanent dashes.
    const out = readoutsOf(
      node({
        current_rssi: 48,
        node_peak_rssi: 132,
        node_nadir_rssi: 12,
        pass_peak_rssi: 118,
        pass_nadir_rssi: 41,
        debug_pass_count: 7
      })
    );
    expect(out.map((r) => `${r.label} ${r.value}`)).toEqual([
      'RSSI 48',
      'Node peak 132',
      'Node nadir 12',
      'Pass peak 118',
      'Pass nadir 41',
      'Passes 7'
    ]);
  });

  it('dashes an unreported field rather than rendering a misleading zero', () => {
    expect(readoutsOf(undefined).every((r) => r.value === '—')).toBe(true);
    expect(readoutsOf(node({ debug_pass_count: 0 })).at(-1)?.value).toBe('0');
  });
});
