/**
 * Threshold re-detection (the RotorHazard-style "Recalculate", marshaling.html §5) — the pure
 * math behind the Marshaling screen's "Tune detection" panel.
 *
 * The marshal moves the enter/exit levels live; these helpers re-run the timer's own hysteresis
 * over the CAPTURED samples ({@link detectPasses}), diff the re-detected passes against the
 * competitor's current official ones ({@link diffPasses}), and derive the preview lap list
 * ({@link previewLaps}) — all **preview-only**. Nothing here talks to the Director: committing
 * the diff is the screen's job (a `VoidDetection` per removed pass + an `InsertLap` per added
 * one, the existing marshaling primitives), and the tuned thresholds themselves are never
 * written anywhere — pushing calibration back to RotorHazard via the plugin is a separate,
 * future feature.
 *
 * Everything is a pure function of its inputs so the semantics are unit-testable without a DOM.
 */

import type { CompetitorTrace, Lap } from '@gridfpv/types';

/**
 * How close (µs) a re-detected pass must land to a current official pass to count as the SAME
 * pass in {@link diffPasses}. Real crossings are seconds apart, so half a second comfortably
 * separates "the same gate pass, re-timed to the sample peak" from a genuinely new/lost one.
 */
export const DEFAULT_MATCH_TOLERANCE_MICROS = 500_000;

/** A current official pass: its source-clock time and the log offset a correction targets. */
export interface OfficialPass {
  /** Source-clock time (µs) of the pass. */
  at: number;
  /** The global log offset (`LogRef`) — the `VoidDetection` target if this pass is removed. */
  ref: number;
}

/** The re-detected-vs-official diff {@link diffPasses} produces. */
export interface PassDiff {
  /** Official passes a re-detected pass matched (within tolerance) — unchanged by a commit. */
  kept: { at: number; ref: number; detectedAt: number }[];
  /** Re-detected pass times (µs) with no official counterpart — each becomes an `InsertLap`. */
  added: number[];
  /** Official passes the re-detection no longer sees — each becomes a `VoidDetection`. */
  removed: OfficialPass[];
}

/** A preview lap derived from consecutive re-detected passes (see {@link previewLaps}). */
export interface PreviewLap {
  /** 1-based lap number. */
  number: number;
  /** Source-clock time (µs) of the pass that closes this lap. */
  at: number;
  /** Lap duration (µs). */
  durationMicros: number;
}

/**
 * The source-clock time (µs) of sample `i`: the dense per-sample `times` when the trace carries
 * them (RH's marshal history is bursty — the uniform grid badly misplaces it), else the uniform
 * `from + i·period_micros` grid. Mirrors the RssiGraph's placement so a re-detected pass lands
 * exactly where the graph draws that sample.
 */
export function sampleTimeOf(trace: CompetitorTrace, i: number): number {
  return trace.times?.[i] ?? (trace.from ?? 0) + i * trace.period_micros;
}

/**
 * Re-run RotorHazard-semantics enter/exit hysteresis over a trace's samples and return the
 * detected gate-pass times (µs, source clock), oldest first.
 *
 * A crossing OPENS at the first sample at/above `enter`, tracks the PEAK sample within the open
 * window (ties keep the first peak), and CLOSES at the first subsequent sample at/below `exit`;
 * the pass time is the peak sample's time. A window still open at the end of the trace emits
 * NO pass (an incomplete crossing — the quad may still be at the gate).
 *
 * Detection requires `enter > exit` (the hysteresis gap): equal or inverted thresholds detect
 * nothing — the UI flags that state rather than guessing.
 */
export function detectPasses(trace: CompetitorTrace, enter: number, exit: number): number[] {
  if (!(enter > exit)) return [];
  const passes: number[] = [];
  let open = false;
  let peakValue = -Infinity;
  let peakTime = 0;
  for (let i = 0; i < trace.samples.length; i++) {
    const v = trace.samples[i];
    if (!open) {
      if (v >= enter) {
        open = true;
        peakValue = v;
        peakTime = sampleTimeOf(trace, i);
      }
    } else if (v <= exit) {
      passes.push(peakTime);
      open = false;
      peakValue = -Infinity;
    } else if (v > peakValue) {
      peakValue = v;
      peakTime = sampleTimeOf(trace, i);
    }
  }
  // An open window at trace end is an incomplete crossing — deliberately NOT a pass.
  return passes;
}

/**
 * Diff re-detected pass times against the current official passes: a greedy nearest-first
 * match within `toleranceMicros` (inclusive), each official pass matching at most one detected
 * pass and vice versa. Unmatched detected times are `added`; unmatched official passes are
 * `removed` — together, the exact command batch a commit sends.
 */
export function diffPasses(
  current: OfficialPass[],
  detected: number[],
  toleranceMicros: number = DEFAULT_MATCH_TOLERANCE_MICROS
): PassDiff {
  // Every candidate pairing within tolerance, nearest first — the greedy order.
  const candidates: { ci: number; di: number; dist: number }[] = [];
  for (let ci = 0; ci < current.length; ci++) {
    for (let di = 0; di < detected.length; di++) {
      const dist = Math.abs(detected[di] - current[ci].at);
      if (dist <= toleranceMicros) candidates.push({ ci, di, dist });
    }
  }
  candidates.sort((a, b) => a.dist - b.dist);

  const matchedCurrent = new Set<number>();
  const matchedDetected = new Set<number>();
  const kept: PassDiff['kept'] = [];
  for (const { ci, di } of candidates) {
    if (matchedCurrent.has(ci) || matchedDetected.has(di)) continue;
    matchedCurrent.add(ci);
    matchedDetected.add(di);
    kept.push({ at: current[ci].at, ref: current[ci].ref, detectedAt: detected[di] });
  }
  kept.sort((a, b) => a.at - b.at);

  return {
    kept,
    added: detected.filter((_, di) => !matchedDetected.has(di)),
    removed: current.filter((_, ci) => !matchedCurrent.has(ci))
  };
}

/**
 * The lap list a set of gate-pass times implies: the FIRST pass is the holeshot (it opens lap 1,
 * closing no lap), and every consecutive pair of passes is a lap. `k` passes → `k − 1` laps.
 */
export function previewLaps(passTimes: number[]): PreviewLap[] {
  const laps: PreviewLap[] = [];
  for (let i = 1; i < passTimes.length; i++) {
    laps.push({
      number: i,
      at: passTimes[i],
      durationMicros: passTimes[i] - passTimes[i - 1]
    });
  }
  return laps;
}

/**
 * One row of the UNIFIED re-detection preview (see {@link previewRows}): a single chronological
 * list that tells the whole story — the laps the tuned thresholds would produce (`kept` when the
 * lap's closing pass matches a current official one, `added` when it's new), interleaved with the
 * official passes the re-detection drops (`removed` — passes leaving the record, so they carry no
 * lap number).
 */
export type PreviewRow =
  | {
      status: 'kept' | 'added';
      /** 1-based lap number in the previewed (post-commit) lap list. */
      number: number;
      /** Source-clock time (µs) of the pass that closes this lap. */
      at: number;
      /** Lap duration (µs). */
      durationMicros: number;
    }
  | {
      status: 'removed';
      /** Source-clock time (µs) of the official pass the re-detection drops. */
      at: number;
      /** The pass's log offset — the `VoidDetection` target a commit sends. */
      ref: number;
    };

/**
 * The unified re-detection preview: ONE chronological row list combining the surviving lap chain
 * with the passes that would be removed. Laps derive from the detected passes exactly like
 * {@link previewLaps}; each lap is `kept` when its CLOSING pass matched an official pass (within
 * `toleranceMicros`, per {@link diffPasses}) and `added` otherwise. Every official pass the
 * re-detection no longer sees interleaves as a `removed` row at its own time. Preview-only —
 * nothing here sends commands.
 */
export function previewRows(
  current: OfficialPass[],
  detected: number[],
  toleranceMicros: number = DEFAULT_MATCH_TOLERANCE_MICROS
): PreviewRow[] {
  const diff = diffPasses(current, detected, toleranceMicros);
  const keptDetectedAt = new Set(diff.kept.map((k) => k.detectedAt));
  const rows: PreviewRow[] = previewLaps(detected).map((lap) => ({
    status: keptDetectedAt.has(lap.at) ? 'kept' : 'added',
    number: lap.number,
    at: lap.at,
    durationMicros: lap.durationMicros
  }));
  for (const pass of diff.removed) rows.push({ status: 'removed', at: pass.at, ref: pass.ref });
  // Chronological (stable, so same-instant rows keep lap-then-removed order).
  rows.sort((a, b) => a.at - b.at);
  return rows;
}

/**
 * A competitor's current OFFICIAL passes, reconstructed from their (marshaling-corrected) lap
 * list: lap 1's opening pass (`start_ref`, at `lap1.at − lap1.duration`) plus every lap's
 * closing pass (`end_ref` at `lap.at`). These are the refs a re-detection commit voids when the
 * new thresholds no longer see them.
 */
export function officialPasses(laps: Lap[]): OfficialPass[] {
  if (laps.length === 0) return [];
  const first = laps[0];
  const passes: OfficialPass[] = [{ at: first.at - first.duration_micros, ref: first.start_ref }];
  for (const lap of laps) passes.push({ at: lap.at, ref: lap.end_ref });
  return passes;
}

/**
 * Sensible starting thresholds for a trace that recorded none: enter at the 75th percentile of
 * the samples, exit at the 25th (the recorded `enter`/`exit` are always preferred when present).
 * Returns `undefined` for an empty trace.
 */
export function defaultThresholds(
  trace: CompetitorTrace
): { enter: number; exit: number } | undefined {
  if (trace.samples.length === 0) return undefined;
  const sorted = [...trace.samples].sort((a, b) => a - b);
  const at = (q: number) => sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))];
  return { enter: at(0.75), exit: at(0.25) };
}
