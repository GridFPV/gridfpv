/**
 * Round-robin derivations — fold a round-robin round's heats + results into the {@link RoundRobin}
 * view-model `RoundRobinView` renders (the builder lives in `standings.ts`).
 *
 * A round-robin round (format `round_robin`) is one round whose heats rotate the field so each pilot
 * meets a spread of opponents. Its heat ids follow the scheme `rr-r{N}-h{M}` — `N` is the **rotation**
 * number, `M` the heat within it — so the per-rotation grid is recovered by grouping on `N`. The
 * standings aggregate each pilot's points (the per-position `points` table summed over every finish)
 * and best lap, then render in the **canonical** order `session.roundRanking` returns.
 *
 * Kept framework-pure (no Svelte) so it unit-tests directly and the Rounds stage + Results share one
 * source of truth.
 */

import type {
  CompetitorRef,
  HeatId,
  HeatResult,
  HeatSummary,
  RankEntry,
  RoundDef
} from '@gridfpv/types';
import type { RoundRobin, RoundRobinRotation, RoundRobinStanding } from '@gridfpv/components';

/** The round-robin format key. */
export const ROUND_ROBIN = 'round_robin';

/**
 * The default **per-position points table** for round-robin placement — the same `[10, 6, 4, 3, 2, 1]`
 * the Head-to-Head / bracket editors seed with. A finish in position `p` scores
 * `table[p - 1]`; a position past the table scores `0`.
 */
export const RR_DEFAULT_POINTS: readonly number[] = [10, 6, 4, 3, 2, 1];

/** Whether `round` is a round-robin round (format `round_robin`). */
export function isRoundRobinRound(round: RoundDef): boolean {
  return round.format === ROUND_ROBIN;
}

/**
 * Parse a stored `points` CSV (e.g. "10,6,4" or "10, 6, 4") into a per-position table; falls back to
 * {@link RR_DEFAULT_POINTS} when empty/blank. Non-numeric / negative entries clamp to 0.
 */
export function parseRoundRobinPoints(csv: string | undefined): number[] {
  const parsed = (csv ?? '')
    .split(/[,\s]+/)
    .filter((s) => s.length > 0)
    .map((s) => Math.max(0, Math.round(Number(s) || 0)));
  return parsed.length > 0 ? parsed : [...RR_DEFAULT_POINTS];
}

/**
 * The 1-based **rotation** number of a `rr-r{N}-h{M}` heat id, or `undefined` for any id that does
 * not match the scheme (so a non-round-robin / hand-built heat is grouped separately).
 */
export function rotationNumberOf(heatId: string): number | undefined {
  const m = /^rr-r(\d+)-h\d+$/.exec(heatId);
  return m ? Number(m[1]) : undefined;
}

/** The 1-based **heat** number within a rotation, for ordering heats inside a rotation. */
function heatNumberOf(heatId: string): number {
  const m = /^rr-r\d+-h(\d+)$/.exec(heatId);
  return m ? Number(m[1]) : Number.MAX_SAFE_INTEGER;
}

/**
 * Group a round-robin round's `heats` into ordered rotations by their `rr-r{N}` number (heats whose
 * id doesn't match the scheme fall into a trailing "Other" rotation so they never vanish). Within a
 * rotation heats sort by their `h{M}` number; each heat's entries are in **finishing order** when the
 * heat is scored (from `resultByHeat`), else lineup order.
 *
 * @param heats the round's scheduled heats.
 * @param heatName resolve a heat to its friendly display name.
 * @param label resolve a competitor ref to its friendly callsign.
 * @param resultByHeat resolve a heat id to its scored {@link HeatResult}, if any.
 */
export function groupRoundRobinRotations(
  heats: HeatSummary[],
  heatName: (h: HeatSummary) => string,
  label: (ref: CompetitorRef) => string,
  resultByHeat: (heatId: HeatId) => HeatResult | undefined
): RoundRobinRotation[] {
  // Bucket heats by rotation number (undefined → a trailing "Other" bucket, key -1).
  const byRotation = new Map<number, HeatSummary[]>();
  for (const h of heats) {
    const n = rotationNumberOf(h.heat) ?? -1;
    const bucket = byRotation.get(n);
    if (bucket) bucket.push(h);
    else byRotation.set(n, [h]);
  }
  // Rotation order: real rotations ascending, the "Other" bucket (-1) last.
  const keys = [...byRotation.keys()].sort((a, b) => {
    if (a === -1) return 1;
    if (b === -1) return -1;
    return a - b;
  });

  return keys.map((key) => {
    const bucket = (byRotation.get(key) ?? [])
      .slice()
      .sort((a, b) => heatNumberOf(a.heat) - heatNumberOf(b.heat));
    return {
      name: key === -1 ? 'Other heats' : `Rotation ${key}`,
      heats: bucket.map((h) => {
        const res = resultByHeat(h.heat);
        const entries =
          res && res.places.length > 0
            ? res.places.map((p) => ({
                competitor: p.competitor.competitor,
                label: label(p.competitor.competitor),
                position: p.position
              }))
            : h.lineup.map((ref) => ({ competitor: ref, label: label(ref) }));
        return { heat: h.heat, name: heatName(h), entries };
      })
    };
  });
}

/**
 * The aggregate standings rows for a round-robin round, in the **canonical** order `ranking`
 * (`session.roundRanking`) returns. Each pilot's `points` is the `pointsTable` summed over their
 * finishes across every scored heat; `bestLapMicros` comes from `bestLapOf` — the dedicated round
 * standings endpoint, which **always** computes best lap regardless of the round's win condition (a
 * race-scored heat's result metric is the win-condition metric, e.g. `ReachedAt`, not a best lap, so
 * best lap can no longer be read off the heat-result metric). A pilot with no scored finish totals 0
 * points, and a pilot with no recorded lap shows no best lap, so a part-run round still renders in
 * ranking order.
 *
 * When `ranking` is empty (the round isn't scored yet) the rows fall back to the heats' lineup order
 * (deduped, first-seen) so the table still shows the field with zero points.
 *
 * @param bestLapOf resolve a competitor ref to their best single lap in micros (`null`/`undefined`
 *   when they have none) — sourced from `session.roundStandings`, never the heat-result metric.
 */
export function roundRobinStandings(
  ranking: RankEntry[],
  heats: HeatSummary[],
  pointsTable: number[],
  label: (ref: CompetitorRef) => string,
  resultByHeat: (heatId: HeatId) => HeatResult | undefined,
  bestLapOf: (ref: CompetitorRef) => number | null | undefined
): RoundRobinStanding[] {
  // Aggregate points per competitor across every scored heat (best lap comes from `bestLapOf`).
  const points = new Map<CompetitorRef, number>();
  for (const h of heats) {
    const res = resultByHeat(h.heat);
    if (!res) continue;
    for (const p of res.places) {
      const ref = p.competitor.competitor;
      const pts = pointsTable[p.position - 1] ?? 0;
      points.set(ref, (points.get(ref) ?? 0) + pts);
    }
  }

  const rowFor = (ref: CompetitorRef, position: number): RoundRobinStanding => ({
    competitor: ref,
    label: label(ref),
    position,
    points: points.get(ref) ?? 0,
    bestLapMicros: bestLapOf(ref) ?? undefined
  });

  if (ranking.length > 0) {
    return ranking.map((r) => rowFor(r.competitor, r.position));
  }

  // No ranking yet: fall back to the field in first-seen lineup order, zero points.
  const seen: CompetitorRef[] = [];
  for (const h of heats) for (const ref of h.lineup) if (!seen.includes(ref)) seen.push(ref);
  return seen.map((ref, i) => rowFor(ref, i + 1));
}

/**
 * Build the whole {@link RoundRobin} view-model for a round-robin round: the aggregate standings
 * (ordered by `ranking`) + the per-rotation heat grid. Pure + fetch-free — the caller supplies the
 * ranking, the round's heats, and the resolvers/result accessor.
 */
export function buildRoundRobinView(
  ranking: RankEntry[],
  heats: HeatSummary[],
  opts: {
    pointsTable: number[];
    label: (ref: CompetitorRef) => string;
    heatName: (h: HeatSummary) => string;
    resultByHeat: (heatId: HeatId) => HeatResult | undefined;
    /** Best single lap (micros) per competitor — from `session.roundStandings`, win-condition-agnostic. */
    bestLapOf: (ref: CompetitorRef) => number | null | undefined;
  }
): RoundRobin {
  return {
    standings: roundRobinStandings(
      ranking,
      heats,
      opts.pointsTable,
      opts.label,
      opts.resultByHeat,
      opts.bestLapOf
    ),
    rotations: groupRoundRobinRotations(heats, opts.heatName, opts.label, opts.resultByHeat)
  };
}
