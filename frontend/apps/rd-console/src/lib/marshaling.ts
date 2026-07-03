/**
 * Marshaling adjudication command builders (#55).
 *
 * Marshaling never mutates state directly: each correction *appends* the matching
 * adjudication event, which the projection re-folds into a fresh result (clients.html
 * §1, architecture.html §3). The console emits the corresponding `Command`; the
 * corrected projection arrives back on the read stream as a fresh value (the live
 * client folds it and pushes a new state — the screen re-renders, nothing local to
 * reconcile).
 *
 * `Command`'s five marshaling variants are pure functions of their inputs here, so the
 * screen builds them declaratively and the tests assert the exact wire shape. Targets
 * are `LogRef` (a u64 log offset, rendered as a TS `number`); times are `SourceTime`
 * (µs, rendered as a TS `number`).
 */

import type {
  AdapterId,
  Command,
  CompetitorRef,
  HeatId,
  LogRef,
  Penalty,
  ProtestOutcome,
  SourceTime
} from '@gridfpv/types';

/** Void a single logged detection (a bad pass, a phantom lap) by its log offset. */
export function voidDetectionCommand(target: LogRef): Command {
  return { VoidDetection: { target } };
}

/**
 * Insert a missed crossing for a competitor at a source-clock time. Carries the marshaled
 * `heat` so the server routes the insertion into THAT heat's scoring window — marshaling a
 * finished heat while a later heat is live must never attribute the lap to the live heat.
 */
export function insertLapCommand(
  adapter: AdapterId,
  competitor: CompetitorRef,
  at: SourceTime,
  heat: HeatId
): Command {
  return { InsertLap: { adapter, competitor, at, heat } };
}

/** Re-time an existing logged pass (identified by its log offset) to a new time. */
export function adjustLapCommand(target: LogRef, at: SourceTime): Command {
  return { AdjustLap: { target, at } };
}

/**
 * Split the over-long lap *ending* at `target` (a logged pass's offset) by inserting a
 * synthetic mid-lap crossing at `at` — the FPVTrackside "split" for a missed mid-lap detection.
 */
export function splitLapCommand(target: LogRef, at: SourceTime): Command {
  return { SplitLap: { target, at } };
}

/**
 * Throw out a single **valid** lap from a competitor's scored count (marshaling.html §3.3),
 * identified by the lap's end-pass log offset. Distinct from `voidDetectionCommand`: the lap stays
 * real in the lap list/audit — it is only excluded from scoring.
 */
export function throwOutLapCommand(target: LogRef): Command {
  return { ThrowOutLap: { target } };
}

/**
 * Reverse **any** prior ruling — a penalty (DQ / time / points), a lap throw-out, a protest
 * resolution, or a heat-void — identified by its log offset. The reversed ruling no longer affects
 * the result (generalized reversibility, marshaling.html §3.3).
 */
export function reverseRulingCommand(target: LogRef): Command {
  return { ReverseRuling: { target } };
}

/** Void a whole heat (it should never have counted). */
export function voidHeatCommand(heat: HeatId): Command {
  return { VoidHeat: { heat } };
}

/** Apply a penalty (DQ, added time, or points) to a competitor in a heat. */
export function applyPenaltyCommand(
  heat: HeatId,
  competitor: CompetitorRef,
  penalty: Penalty
): Command {
  return { ApplyPenalty: { heat, competitor, penalty } };
}

/**
 * Deduct **standings** points from a competitor (marshaling.html §3.3) — points affect the
 * season/event standings, not the per-heat lap result.
 */
export function deductPointsCommand(
  heat: HeatId,
  competitor: CompetitorRef,
  points: number
): Command {
  return { DeductPoints: { heat, competitor, points: Math.max(0, Math.round(points)) } };
}

/** File a protest against a competitor's result in a heat (the append-only filing fact). */
export function fileProtestCommand(heat: HeatId, competitor: CompetitorRef, note: string): Command {
  return { FileProtest: { heat, competitor, note } };
}

/** Resolve a filed protest (identified by the `ProtestFiled`'s log offset) with an outcome. */
export function resolveProtestCommand(target: LogRef, outcome: ProtestOutcome): Command {
  return { ResolveProtest: { target, outcome } };
}

/**
 * Build a `TimeAdded` penalty from a whole-second amount (the console's input unit). A penalty
 * only ever WORSENS a result: a non-positive amount clamps to 0µs (a no-op ruling) rather than
 * crediting time — a negative "penalty" silently improving a competitor's result is exactly the
 * kind of ruling defensible results must not allow.
 */
export function timeAddedPenalty(seconds: number): Penalty {
  return { TimeAdded: { micros: Math.max(0, Math.round(seconds * 1_000_000)) } };
}

/** Build a `PointsDeducted` standings penalty (season/event points, not per-heat). */
export function pointsDeductedPenalty(points: number): Penalty {
  return { PointsDeducted: { points: Math.max(0, Math.round(points)) } };
}

/**
 * Build a disqualification penalty, optionally carrying a reason. A reason-less DQ serialises as
 * the compact legacy form; a reason rides along when given.
 */
export function disqualifyPenalty(reason?: string): Penalty {
  const trimmed = reason?.trim();
  return { Disqualify: trimmed ? { reason: trimmed } : {} };
}

/** The bare disqualification penalty (no reason) — the common quick-DQ. */
export const DISQUALIFY: Penalty = { Disqualify: {} };

/** Convert the console's whole-second time input to `SourceTime` microseconds. */
export function secondsToSourceTime(seconds: number): SourceTime {
  return Math.round(seconds * 1_000_000);
}
