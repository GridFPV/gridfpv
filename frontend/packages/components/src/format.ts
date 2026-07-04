/**
 * Pure presentational formatters shared across the component library.
 *
 * Durations on the wire are microseconds (a plain `number`, source clock — see
 * `@bindings/SourceTime` / `Lap.duration_micros`); these turn them into the
 * compact strings the widgets show. Kept framework-pure so components and tests
 * share one source of truth for "how a lap time reads".
 */

import type { Metric } from '@gridfpv/types';

/**
 * Format a clock duration in **milliseconds** as `M:SS.mmm` (e.g. `1:23.456`). A negative value —
 * a countdown past its zero (a timed heat inside its grace window) — renders sign-prefixed
 * (`-0:03.250`).
 */
export function formatClock(ms: number): string {
  const totalMs = Math.floor(Math.abs(ms));
  const minutes = Math.floor(totalMs / 60000);
  const seconds = Math.floor((totalMs % 60000) / 1000);
  const millis = totalMs % 1000;
  const sign = ms < 0 ? '-' : '';
  return `${sign}${minutes}:${String(seconds).padStart(2, '0')}.${String(millis).padStart(3, '0')}`;
}

/**
 * Format a lap/split duration given in **microseconds** (the wire unit) as
 * `S.mmm` seconds, or minutes:seconds when ≥ 60s. `null`/`undefined` → `'—'`.
 */
export function formatMicros(micros: number | null | undefined): string {
  if (micros === null || micros === undefined) return '—';
  // Micros are a plain `number` now (the i64/u64 wire types render as TS `number`,
  // matching serde's JSON-number output), so the bigint/number mismatch is resolved at
  // the type level. The `Number()` coercion stays as belt-and-suspenders against any
  // stray non-number value reaching a render.
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
