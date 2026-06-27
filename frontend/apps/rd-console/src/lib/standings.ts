/**
 * Results / standings + advance-to-bracket derivations (race redesign Slice 5/6b).
 *
 * Two pure helpers shared by the Results screen (per-class standings) and the Rounds stage
 * (per-round ranking → "Advance to bracket"):
 *
 *  - {@link bracketTopNDefault} — the default bracket size when advancing a round to a single-elim
 *    bracket: the largest power-of-two **≤ the round's field size** (8 pilots → 8, 9 → 8, 6 → 4),
 *    so a 9-up qualifier cuts to a clean 8-seed bracket. The RD can override it in the confirm.
 *  - {@link advanceRoundReq} — assembles the `single_elim` round request seeded `FromRanking` from a
 *    source round's ranking, top-N. The payload the bracket round is created with.
 *
 * Kept framework-pure (no Svelte) so they unit-test directly and the screen + stage share one
 * source of truth.
 */

import type { NewRoundReq, RoundDef, WinCondition } from '@gridfpv/types';

/**
 * The largest power-of-two **≤ `fieldSize`** — the default top-N for a bracket cut. A bracket runs
 * on a power-of-two seed count (8, 16, …); the sensible default is to take as many seeds as fit
 * under the field without padding byes, so a 9-pilot qualifier defaults to a clean 8-seed bracket.
 * A field of 0 or 1 floors at 1 (a degenerate single-entry bracket the RD can still grow).
 */
export function bracketTopNDefault(fieldSize: number): number {
  const n = Math.floor(fieldSize);
  if (n <= 1) return 1;
  // Largest power of two ≤ n: 2 ^ floor(log2(n)).
  return 2 ** Math.floor(Math.log2(n));
}

/**
 * Assemble the request for the **bracket round** that advancing `source` produces: a `single_elim`
 * round over the source round's same eligible classes, carrying the source round's win condition,
 * seeded `FromRanking` from the source round's ranking, top-`topN`. The RD picks the `label` and may
 * override `topN` in the confirm. The created round's heats are then filled (`fillRound`) into the
 * seeded bracket matchups — editable thereafter like any manually-built round.
 */
export function advanceRoundReq(source: RoundDef, topN: number, label: string): NewRoundReq {
  const win_condition: WinCondition = source.win_condition;
  return {
    label,
    classes: [...source.classes],
    format: 'single_elim',
    params: {},
    win_condition,
    // Carry the source's race time too: a bracket inherits the qual's win condition, and a
    // ranking-only condition (Best Lap / Best N Consecutive) needs a time limit to end its heats —
    // without it the server rejects the round (a scored round must be able to end).
    time_limit_secs: source.time_limit_secs,
    seeding: {
      // Advancing one round seeds the bracket from that single round (issue #51: `source_rounds` is
      // a list — a one-element list here; the Rounds form lets the RD add more sources after).
      FromRanking: { source_rounds: [source.id], top_n: Math.max(1, Math.round(topN)) }
    }
  };
}

/** A sensible default label for the bracket round advancing `source` produces. */
export function advanceRoundLabel(source: RoundDef): string {
  return `${source.label} — Bracket`;
}

/**
 * Assemble the request for the **next bracket level** advancing `level` produces (#217, decisions
 * D13: one round per level). Unlike {@link advanceRoundReq} (quali → level 1, seeded `FromRanking`),
 * a level → next-level advance reuses the same bracket format and is seeded
 * `FromHeatWinners { source_round: <level> }` — the per-level generator pairs the level's heat
 * winners into the next level's heats. Carries the level's classes + win condition unchanged; the RD
 * picks the `label` (e.g. "Semifinals").
 */
export function advanceLevelReq(level: RoundDef, label: string): NewRoundReq {
  return {
    label,
    classes: [...level.classes],
    format: level.format,
    params: { ...(level.params ?? {}) },
    win_condition: level.win_condition,
    // Carry the level's race time forward too, so a ranking-only win condition still ends its
    // heats (a scored round must be able to end — server validation).
    time_limit_secs: level.time_limit_secs,
    seeding: { FromHeatWinners: { source_round: level.id } },
    channel_mode: level.channel_mode
  };
}
