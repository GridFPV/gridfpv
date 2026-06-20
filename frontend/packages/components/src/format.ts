/**
 * Pure presentational formatters shared across the component library.
 *
 * Durations on the wire are microseconds (`bigint`, source clock — see
 * `@bindings/SourceTime` / `Lap.duration_micros`); these turn them into the
 * compact strings the widgets show. Kept framework-pure so components and tests
 * share one source of truth for "how a lap time reads".
 */

import type { Metric } from '@gridfpv/types';

/** Format a clock duration in **milliseconds** as `M:SS.mmm` (e.g. `1:23.456`). */
export function formatClock(ms: number): string {
  const totalMs = Math.max(0, Math.floor(ms));
  const minutes = Math.floor(totalMs / 60000);
  const seconds = Math.floor((totalMs % 60000) / 1000);
  const millis = totalMs % 1000;
  return `${minutes}:${String(seconds).padStart(2, '0')}.${String(millis).padStart(3, '0')}`;
}

/**
 * Format a lap/split duration given in **microseconds** (the wire unit) as
 * `S.mmm` seconds, or minutes:seconds when ≥ 60s. `null`/`undefined` → `'—'`.
 */
export function formatMicros(micros: bigint | number | null | undefined): string {
  if (micros === null || micros === undefined) return '—';
  // ts-rs types i64 micros as `bigint`, but serde sends i64 as a JSON *number*, so the
  // runtime value is usually a `number`. Coerce to Number BEFORE any arithmetic — mixing
  // a bigint and a number (e.g. `250000 / 1000n`) throws "Cannot mix BigInt and other types"
  // and, thrown mid-render, aborts the update. (The wire-vs-type i64/bigint mismatch is a
  // class to address systematically in the contract review.)
  const totalMs = Math.floor(Number(micros) / 1000);
  if (totalMs >= 60000) return formatClock(totalMs);
  const seconds = Math.floor(totalMs / 1000);
  const millis = totalMs % 1000;
  return `${seconds}.${String(millis).padStart(3, '0')}`;
}

/**
 * Render a scoring [`Metric`] for display — the condition-specific deciding
 * value carried on a `Placement`. Durations become `S.mmm`; the time-of-event
 * metrics (`LastLapAt`, `ReachedAt`) carry a source timestamp we surface as a
 * coarse marker rather than inventing a wall-clock format here.
 */
export function formatMetric(metric: Metric): string {
  if ('BestLapMicros' in metric) return formatMicros(metric.BestLapMicros);
  if ('BestConsecutiveMicros' in metric) return formatMicros(metric.BestConsecutiveMicros);
  if ('LastLapAt' in metric) return metric.LastLapAt === null ? '—' : 'banked';
  if ('ReachedAt' in metric) return metric.ReachedAt === null ? '—' : 'reached';
  return '—';
}

/** The medal token name for a 1-based finishing position, or `null` past 3rd. */
export function medalFor(position: number): 'gold' | 'silver' | 'bronze' | null {
  if (position === 1) return 'gold';
  if (position === 2) return 'silver';
  if (position === 3) return 'bronze';
  return null;
}
