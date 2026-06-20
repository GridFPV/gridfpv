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
  SourceTime
} from '@gridfpv/types';

/** Void a single logged detection (a bad pass, a phantom lap) by its log offset. */
export function voidDetectionCommand(target: LogRef): Command {
  return { VoidDetection: { target } };
}

/** Insert a missed crossing for a competitor at a source-clock time. */
export function insertLapCommand(
  adapter: AdapterId,
  competitor: CompetitorRef,
  at: SourceTime
): Command {
  return { InsertLap: { adapter, competitor, at } };
}

/** Re-time an existing logged pass (identified by its log offset) to a new time. */
export function adjustLapCommand(target: LogRef, at: SourceTime): Command {
  return { AdjustLap: { target, at } };
}

/** Void a whole heat (it should never have counted). */
export function voidHeatCommand(heat: HeatId): Command {
  return { VoidHeat: { heat } };
}

/** Apply a penalty (disqualification or added time) to a competitor in a heat. */
export function applyPenaltyCommand(
  heat: HeatId,
  competitor: CompetitorRef,
  penalty: Penalty
): Command {
  return { ApplyPenalty: { heat, competitor, penalty } };
}

/** Build a `TimeAdded` penalty from a whole-second amount (the console's input unit). */
export function timeAddedPenalty(seconds: number): Penalty {
  return { TimeAdded: { micros: Math.round(seconds * 1_000_000) } };
}

/** The disqualification penalty. */
export const DISQUALIFY: Penalty = 'Disqualify';

/** Convert the console's whole-second time input to `SourceTime` microseconds. */
export function secondsToSourceTime(seconds: number): SourceTime {
  return Math.round(seconds * 1_000_000);
}
