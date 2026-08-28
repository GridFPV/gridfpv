/**
 * Threshold re-detection (the RotorHazard-style "Recalculate", marshaling.html §5) — the pure
 * math behind the Marshaling screen's "Tune detection" panel.
 *
 * The marshal moves the enter/exit levels live; these helpers re-run the timer's own hysteresis
 * over the CAPTURED samples ({@link detectPasses}), diff the re-detected passes against the
 * competitor's current official ones ({@link diffPasses}), and derive the preview lap list
 * ({@link previewLaps}) — all **preview-only**. Nothing here talks to the Director: committing
 * the diff is the screen's job (a `VoidDetection` per removed pass + an `InsertLap` per added
 * one, the existing marshaling primitives), and pushing the tuned thresholds themselves back to
 * the timer is a separate, explicit action the screen offers alongside it (`applyTune.ts`, #470)
 * — never a side effect of a commit.
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
  /**
   * Re-detected crossings SUPPRESSED because the RD explicitly voided a pass there
   * (within tolerance). The RSSI trace genuinely shows the crossing, but the marshal already
   * ruled it out — so re-detection must never re-propose it as "a lap to add" (the removal
   * record and the tuner share the lap list's `voided` data). Never inserted by a commit.
   */
  suppressed: number[];
  /**
   * Re-detected crossings REFUSED by the round's **minimum-lap floor** (D26, #469) — they would
   * have been `added`, but minting them would put a lap under the floor. Never inserted by a
   * commit. See {@link diffPasses}'s `minLapMicros` for the rule and why the automated path is
   * held to it when a hand-added lap is not.
   */
  refused: number[];
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
 *
 * `minLapMicros` is the round's **minimum-lap floor** (D26, `RoundDef.min_lap_secs` in µs;
 * omitted / `0` = off). **An automated path may not mint a lap under the floor** (#469): a
 * commit inserts each `added` time as a marshal-created pass, and marshal-created passes are
 * *exempt* from the corrected-passes fold's floor — an explicit ruling outranks the floor. That
 * exemption is right for a human RD adding a lap by hand (their judgment, their call) and wrong
 * for threshold-replay math, which was borrowing it to mint sub-floor laps no fold could strip.
 * So the refusal happens here, in the math, before anything is proposed.
 *
 * The rule, evaluated against the chain a commit would actually leave behind (kept official
 * passes + the candidate adds, in time order): a candidate add is REFUSED when it lands closer
 * than `minLapMicros` to the previous surviving entry, or closer than `minLapMicros` to the next
 * *official* entry — the second half stops an add from squeezing in beside an existing pass and
 * pushing that pass under the floor instead of itself. Refusals land in `refused`, never
 * `added`. Only adds are ever refused: an official pass already in the record — including a lap
 * the RD hand-added under the floor — anchors the chain and is left exactly as it is.
 */
export function diffPasses(
  current: OfficialPass[],
  detected: number[],
  toleranceMicros: number = DEFAULT_MATCH_TOLERANCE_MICROS,
  voidedAt: number[] = [],
  minLapMicros: number = 0
): PassDiff {
  // Match FIRST, suppress SECOND. Suppression must only claim crossings that would otherwise
  // become ADDS: running it before the match let a voided instant steal an official pass's
  // nearest crossing (a double-detection the RD half-removed), flipping the SURVIVING lap
  // into `removed` — a commit would then void the lap the RD kept. Matching first also means
  // a lap the RD re-adds at a once-voided instant pairs with its crossing (kept) instead of
  // fighting the stale removal record forever.
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

  // THEN: of the unmatched crossings, suppress those the RD explicitly voided (within
  // tolerance). The trace still shows the crossing — the void must win here, or the tuner
  // keeps offering a removed lap back as an add.
  const mintable: number[] = [];
  const suppressed: number[] = [];
  for (let di = 0; di < detected.length; di++) {
    if (matchedDetected.has(di)) continue;
    const t = detected[di];
    if (voidedAt.some((v) => Math.abs(v - t) <= toleranceMicros)) suppressed.push(t);
    else mintable.push(t);
  }

  // FINALLY: hold the surviving candidates to the round's min-lap floor. A void is the RD's
  // ruling and outranks the floor, so it is applied first (above) and a voided crossing never
  // reaches this step at all.
  const { added, refused } = applyMinLapFloor(
    kept.map((k) => k.at),
    mintable,
    minLapMicros
  );

  return {
    kept,
    added,
    removed: current.filter((_, ci) => !matchedCurrent.has(ci)),
    suppressed,
    refused
  };
}

/**
 * Split re-detection's candidate new passes into the ones a commit may `add` and the ones the
 * round's minimum-lap floor `refused` (#469) — the rule spelled out on {@link diffPasses}.
 *
 * `official` are the times of the passes that survive the commit (they anchor the chain and are
 * never refused — a hand-added sub-floor lap is the RD's ruling, and the floor does not overturn
 * it). `candidates` are the would-be adds. A floor of `0` (or a non-finite one) is OFF and every
 * candidate is an add, exactly as before the floor existed.
 *
 * Earlier candidates win: walking the merged chain forward means a burst of reflections keeps its
 * FIRST crossing and refuses the rest, matching how the timer's own min-lap behaves and how the
 * corrected-passes fold suppresses a raw sub-floor pass rather than the pass before it.
 */
export function applyMinLapFloor(
  official: number[],
  candidates: number[],
  minLapMicros: number
): { added: number[]; refused: number[] } {
  if (!(minLapMicros > 0)) return { added: [...candidates], refused: [] };
  const officialSorted = [...official].sort((a, b) => a - b);
  const merged = [
    ...officialSorted.map((at) => ({ at, add: false })),
    ...candidates.map((at) => ({ at, add: true }))
  ].sort((a, b) => a.at - b.at || Number(a.add) - Number(b.add));

  const added: number[] = [];
  const refused: number[] = [];
  let prevSurviving: number | undefined;
  for (let i = 0; i < merged.length; i++) {
    const entry = merged[i];
    if (!entry.add) {
      // An official pass always survives and always anchors, floor or no floor.
      prevSurviving = entry.at;
      continue;
    }
    // The next OFFICIAL entry, which is guaranteed to survive — so an add cannot be accepted by
    // clearing the gap behind it only to shove the pass in front of it under the floor.
    const nextOfficial = merged.slice(i + 1).find((e) => !e.add)?.at;
    const tooCloseBehind = prevSurviving !== undefined && entry.at - prevSurviving < minLapMicros;
    const tooCloseAhead = nextOfficial !== undefined && nextOfficial - entry.at < minLapMicros;
    if (tooCloseBehind || tooCloseAhead) {
      refused.push(entry.at);
      continue;
    }
    added.push(entry.at);
    prevSurviving = entry.at;
  }
  return { added, refused };
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
    }
  | {
      status: 'voided';
      /** Source-clock time (µs) of a detected crossing the RD already voided — shown so the
       *  story is honest ("the trace sees it; you removed it"), never inserted by a commit. */
      at: number;
    }
  | {
      status: 'refused';
      /** Source-clock time (µs) of a detected crossing the round's min-lap floor refused
       *  (#469) — shown, not hidden, so the RD can see the trace found something and why it is
       *  not being minted. Never inserted by a commit; the RD may still add the lap by hand. */
      at: number;
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
  toleranceMicros: number = DEFAULT_MATCH_TOLERANCE_MICROS,
  voidedAt: number[] = [],
  minLapMicros: number = 0
): PreviewRow[] {
  const diff = diffPasses(current, detected, toleranceMicros, voidedAt, minLapMicros);
  const keptDetectedAt = new Set(diff.kept.map((k) => k.detectedAt));
  // The lap chain derives from the detected passes MINUS the crossings the commit will not
  // insert — the RD-suppressed ones and the ones the min-lap floor refused (#469). The
  // post-commit record will not contain them, so the previewed laps must not either.
  const dropped = new Set([...diff.suppressed, ...diff.refused]);
  const chain = detected.filter((t) => !dropped.has(t));
  const rows: PreviewRow[] = previewLaps(chain).map((lap) => ({
    status: keptDetectedAt.has(lap.at) ? 'kept' : 'added',
    number: lap.number,
    at: lap.at,
    durationMicros: lap.durationMicros
  }));
  for (const pass of diff.removed) rows.push({ status: 'removed', at: pass.at, ref: pass.ref });
  for (const at of diff.suppressed) rows.push({ status: 'voided', at });
  for (const at of diff.refused) rows.push({ status: 'refused', at });
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
