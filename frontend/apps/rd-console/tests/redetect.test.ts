import { describe, expect, it } from 'vitest';
import type { CompetitorTrace, Lap } from '@gridfpv/types';
import {
  DEFAULT_MATCH_TOLERANCE_MICROS,
  defaultThresholds,
  detectPasses,
  diffPasses,
  officialPasses,
  previewLaps,
  previewRows
} from '../src/lib/redetect.js';

/** A uniform-grid trace: sample `i` at `i·period` (1s cadence by default). */
function trace(samples: number[], opts?: Partial<CompetitorTrace>): CompetitorTrace {
  return {
    competitor: { adapter: 'rh-1', competitor: 'ALICE' },
    from: 0,
    period_micros: 1_000_000,
    samples,
    ...opts
  };
}

const S = 1_000_000; // one second in µs

describe('detectPasses (RH-semantics hysteresis)', () => {
  it('detects one pass per enter→exit crossing, timed at the window PEAK', () => {
    //             0   1    2    3   4   5    6    7   8
    const t = trace([70, 120, 150, 90, 70, 100, 160, 80, 70]);
    // enter=110/exit=95: window 1 opens at i=1, peaks at i=2 (150), closes at i=3 (90).
    // window 2 opens at i=6 (160 ≥ 110 — 100 at i=5 does NOT open), closes at i=7 (80).
    expect(detectPasses(t, 110, 95)).toEqual([2 * S, 6 * S]);
  });

  it('a noisy double-peak within ONE open window is ONE pass at the higher peak', () => {
    // The signal dips between the two humps but never to/below exit — still one crossing.
    //             0   1    2    3    4    5   6
    const t = trace([70, 120, 100, 140, 100, 80, 70]);
    // Opens at i=1; the dip to 100 stays above exit=95; higher peak 140 at i=3; closes at i=5.
    expect(detectPasses(t, 110, 95)).toEqual([3 * S]);
  });

  it('a tie between peaks keeps the FIRST peak sample time', () => {
    const t = trace([70, 130, 100, 130, 80]);
    expect(detectPasses(t, 110, 95)).toEqual([1 * S]);
  });

  it('a window still open at trace end emits NO pass (incomplete crossing)', () => {
    const t = trace([70, 120, 150, 130]); // never falls back to/below exit
    expect(detectPasses(t, 110, 95)).toEqual([]);
    // …but a completed earlier crossing still counts.
    const t2 = trace([70, 120, 80, 70, 125, 140]);
    expect(detectPasses(t2, 110, 95)).toEqual([1 * S]);
  });

  it('equal or inverted thresholds detect nothing (enter must exceed exit)', () => {
    const t = trace([70, 120, 150, 90, 70]);
    expect(detectPasses(t, 100, 100)).toEqual([]);
    expect(detectPasses(t, 95, 110)).toEqual([]);
  });

  it('boundary samples open/close the window inclusively (≥ enter, ≤ exit)', () => {
    const t = trace([70, 110, 95, 70]); // opens exactly AT enter, closes exactly AT exit
    expect(detectPasses(t, 110, 95)).toEqual([1 * S]);
  });

  it('uses the dense per-sample `times` when present (bursty marshal history)', () => {
    const t = trace([70, 120, 150, 90, 70], {
      // Non-uniform: the peak sample really sits at 12.34s, far off the 1s grid.
      times: [0, 12_000_000, 12_340_000, 12_900_000, 30_000_000]
    });
    expect(detectPasses(t, 110, 95)).toEqual([12_340_000]);
  });

  it('falls back to the uniform from + i·period grid without `times`', () => {
    const t = trace([70, 120, 80], { from: 5_000_000, period_micros: 500_000 });
    expect(detectPasses(t, 110, 95)).toEqual([5_500_000]);
  });

  it('an empty trace detects nothing', () => {
    expect(detectPasses(trace([]), 110, 95)).toEqual([]);
  });
});

describe('diffPasses (greedy nearest-match)', () => {
  const current = [
    { at: 1 * S, ref: 10 },
    { at: 41 * S, ref: 12 },
    { at: 81 * S, ref: 14 }
  ];

  it('an identical re-detection keeps everything (empty diff)', () => {
    const d = diffPasses(current, [1 * S, 41 * S, 81 * S]);
    expect(d.added).toEqual([]);
    expect(d.removed).toEqual([]);
    expect(d.kept.map((k) => k.ref)).toEqual([10, 12, 14]);
  });

  it('matches within tolerance INCLUSIVE; just-over-tolerance is an add + a remove', () => {
    // 41.5s is exactly 500ms from the official 41.0s pass — still the same pass.
    const atTol = diffPasses(current, [1 * S, 41 * S + DEFAULT_MATCH_TOLERANCE_MICROS, 81 * S]);
    expect(atTol.added).toEqual([]);
    expect(atTol.removed).toEqual([]);
    expect(atTol.kept.find((k) => k.ref === 12)?.detectedAt).toBe(41_500_000);

    // One µs beyond tolerance: no longer the same pass — the official one is removed, the
    // detected one added.
    const over = diffPasses(current, [1 * S, 41 * S + DEFAULT_MATCH_TOLERANCE_MICROS + 1, 81 * S]);
    expect(over.added).toEqual([41 * S + DEFAULT_MATCH_TOLERANCE_MICROS + 1]);
    expect(over.removed).toEqual([{ at: 41 * S, ref: 12 }]);
  });

  it('each official pass matches at most ONE detected pass (nearest wins)', () => {
    // Two detected passes both within tolerance of the 41s official pass: the nearer one keeps
    // it; the other is a genuine add.
    const d = diffPasses([{ at: 41 * S, ref: 12 }], [41 * S - 100_000, 41 * S + 300_000]);
    expect(d.kept).toEqual([{ at: 41 * S, ref: 12, detectedAt: 41 * S - 100_000 }]);
    expect(d.added).toEqual([41 * S + 300_000]);
    expect(d.removed).toEqual([]);
  });

  it('officially-present passes the re-detection no longer sees are removed', () => {
    const d = diffPasses(current, [1 * S]);
    expect(d.removed).toEqual([
      { at: 41 * S, ref: 12 },
      { at: 81 * S, ref: 14 }
    ]);
    expect(d.added).toEqual([]);
  });

  it('a custom tolerance is honored', () => {
    const d = diffPasses([{ at: 10 * S, ref: 5 }], [10 * S + 900_000], 1_000_000);
    expect(d.kept).toHaveLength(1);
    expect(d.added).toEqual([]);
  });
});

describe('previewLaps (consecutive-pass lap derivation)', () => {
  it('the first pass is the holeshot: k passes → k−1 laps', () => {
    expect(previewLaps([1 * S, 41 * S, 81 * S])).toEqual([
      { number: 1, at: 41 * S, durationMicros: 40 * S },
      { number: 2, at: 81 * S, durationMicros: 40 * S }
    ]);
  });

  it('zero or one pass implies no laps', () => {
    expect(previewLaps([])).toEqual([]);
    expect(previewLaps([5 * S])).toEqual([]);
  });
});

describe('void suppression (the removal record and the tuner share data)', () => {
  it('a detected crossing at an RD-voided time is SUPPRESSED, never added', () => {
    // Blaze's repro: a real crossing that was not a full lap — the RD voided it, but the
    // trace still shows it, so re-detection kept proposing it back as "a lap to add".
    const current: { at: number; ref: number }[] = [
      { at: 1 * S, ref: 10 },
      { at: 81 * S, ref: 12 }
    ];
    const detected = [1 * S, 41 * S, 81 * S]; // the trace still sees the voided crossing at 41s
    const d = diffPasses(current, detected, DEFAULT_MATCH_TOLERANCE_MICROS, [41 * S + 100_000]);
    expect(d.added).toEqual([]); // NOT re-proposed
    expect(d.suppressed).toEqual([41 * S]);
    expect(d.kept.map((k) => k.ref)).toEqual([10, 12]);
    expect(d.removed).toEqual([]);
  });

  it('a genuinely new crossing away from any void is still added', () => {
    const d = diffPasses(
      [{ at: 1 * S, ref: 10 }],
      [1 * S, 60 * S],
      DEFAULT_MATCH_TOLERANCE_MICROS,
      [41 * S]
    );
    expect(d.added).toEqual([60 * S]);
    expect(d.suppressed).toEqual([]);
  });

  it('previewRows carries the suppressed crossing as a voided row and drops it from the lap chain', () => {
    const rows = previewRows(
      [
        { at: 1 * S, ref: 10 },
        { at: 81 * S, ref: 12 }
      ],
      [1 * S, 41 * S, 81 * S],
      DEFAULT_MATCH_TOLERANCE_MICROS,
      [41 * S]
    );
    // ONE kept lap spanning 1s -> 81s (the suppressed crossing is not in the chain), plus the
    // voided marker row at its instant.
    expect(rows).toEqual([
      { status: 'voided', at: 41 * S },
      { status: 'kept', number: 1, at: 81 * S, durationMicros: 80 * S }
    ]);
  });
});

describe('previewRows (the unified re-detection preview)', () => {
  // The canonical official chain: holeshot at 1s, gate passes at 41s + 81s (two 40s laps).
  const current = [
    { at: 1 * S, ref: 10 },
    { at: 41 * S, ref: 12 },
    { at: 81 * S, ref: 14 }
  ];

  it('classifies each preview lap by its CLOSING pass: matched official → kept, new → added', () => {
    // The re-detection keeps the official chain and finds one NEW pass at 60s: the 41→60s lap
    // closes on the new pass (added); the 1→41s and 60→81s laps close on matched passes (kept).
    const rows = previewRows(current, [1 * S, 41 * S, 60 * S, 81 * S]);
    expect(rows).toEqual([
      { status: 'kept', number: 1, at: 41 * S, durationMicros: 40 * S },
      { status: 'added', number: 2, at: 60 * S, durationMicros: 19 * S },
      { status: 'kept', number: 3, at: 81 * S, durationMicros: 21 * S }
    ]);
  });

  it('interleaves dropped official passes as `removed` rows in time order (no lap number)', () => {
    // The re-detection no longer sees the 41s pass: one long 1→81s lap remains (closing on the
    // matched 81s pass → kept), and the dropped 41s pass interleaves BEFORE it at its own time.
    const rows = previewRows(current, [1 * S, 81 * S]);
    expect(rows).toEqual([
      { status: 'removed', at: 41 * S, ref: 12 },
      { status: 'kept', number: 1, at: 81 * S, durationMicros: 80 * S }
    ]);
  });

  it('a pure-kept diff yields all-kept rows (nothing added, nothing removed)', () => {
    const rows = previewRows(current, [1 * S, 41 * S, 81 * S]);
    expect(rows).toEqual([
      { status: 'kept', number: 1, at: 41 * S, durationMicros: 40 * S },
      { status: 'kept', number: 2, at: 81 * S, durationMicros: 40 * S }
    ]);
    expect(rows.every((r) => r.status === 'kept')).toBe(true);
  });

  it('matches within tolerance like diffPasses — a re-timed pass still reads as kept', () => {
    // The 41s pass re-detects 300ms later (within the 500ms default tolerance): same pass, so
    // the lap closing on it stays `kept` (durations follow the DETECTED times, like previewLaps).
    const rows = previewRows(current, [1 * S, 41 * S + 300_000, 81 * S]);
    expect(rows.map((r) => r.status)).toEqual(['kept', 'kept']);
    expect(rows[0]).toMatchObject({ at: 41 * S + 300_000, durationMicros: 40 * S + 300_000 });
  });

  it('detecting nothing turns every official pass into a removed row (and no laps)', () => {
    expect(previewRows(current, [])).toEqual([
      { status: 'removed', at: 1 * S, ref: 10 },
      { status: 'removed', at: 41 * S, ref: 12 },
      { status: 'removed', at: 81 * S, ref: 14 }
    ]);
  });
});

describe('officialPasses (lap list → boundary passes)', () => {
  it('reconstructs lap 1’s opening pass plus every closing pass', () => {
    const laps: Lap[] = [
      { number: 1, duration_micros: 40 * S, at: 41 * S, start_ref: 10, end_ref: 12 },
      { number: 2, duration_micros: 40 * S, at: 81 * S, start_ref: 12, end_ref: 14 }
    ];
    expect(officialPasses(laps)).toEqual([
      { at: 1 * S, ref: 10 }, // the holeshot: lap 1's at − duration
      { at: 41 * S, ref: 12 },
      { at: 81 * S, ref: 14 }
    ]);
  });

  it('no laps → no passes', () => {
    expect(officialPasses([])).toEqual([]);
  });
});

describe('defaultThresholds (unset-trace fallback)', () => {
  it('derives enter at the 75th and exit at the 25th sample percentile', () => {
    const t = trace([10, 20, 30, 40, 50, 60, 70, 80]);
    expect(defaultThresholds(t)).toEqual({ enter: 70, exit: 30 });
  });

  it('is undefined for an empty trace', () => {
    expect(defaultThresholds(trace([]))).toBeUndefined();
  });
});
