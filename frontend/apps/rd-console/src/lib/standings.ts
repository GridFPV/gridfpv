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
 * The selectable **bracket sizes** for a field of `fieldSize` — the powers of two from 2 up to the
 * largest that **fits** (`2, 4, 8, …` with `2^k ≤ fieldSize`). This is what the Advance-to-bracket
 * modal offers, so the RD can never advance **more pilots than the round holds** (the size cap), and
 * every size is a clean power-of-two bracket (no byes). A field under 2 yields no options (you need
 * at least two to bracket).
 */
export function bracketSizeOptions(fieldSize: number): number[] {
  const n = Math.floor(fieldSize);
  const out: number[] = [];
  for (let size = 2; size <= n; size *= 2) out.push(size);
  return out;
}

/**
 * The overall **final** of a bracket: how the last level is decided. Omit (single race = the bracket
 * format) or `{ format: 'chase_the_ace', winsToWin }` for a multi-race Chase-the-Ace final. Shared by
 * {@link advanceRoundReq} (a 2-up bracket whose only level *is* the final) and {@link advanceLevelReq}.
 */
export interface BracketFinal {
  format: string;
  winsToWin?: number;
}

/** Apply a {@link BracketFinal} to a level request's `format` + `params` (chase carries wins_to_win). */
function withFinal(
  base: { format: string; params: Record<string, string> },
  final: BracketFinal | undefined
): { format: string; params: Record<string, string> } {
  if (!final) return base;
  if (final.format === 'chase_the_ace') {
    return {
      format: 'chase_the_ace',
      params: { wins_to_win: String(Math.max(1, Math.round(final.winsToWin ?? 2))) }
    };
  }
  return { format: final.format, params: base.params };
}

/**
 * Assemble the request for the **bracket round** that advancing `source` produces: a `single_elim`
 * round over the source round's same eligible classes, carrying the source round's win condition,
 * seeded `FromRanking` from the source round's ranking, top-`topN`. The RD picks the `label` and may
 * override `topN` in the confirm. The created round's heats are then filled (`fillRound`) into the
 * seeded bracket matchups — editable thereafter like any manually-built round.
 */
export function advanceRoundReq(
  source: RoundDef,
  topN: number,
  label: string,
  winCondition: WinCondition,
  final?: BracketFinal
): NewRoundReq {
  // `final` applies only when this round IS the final (a 2-up bracket's single level); otherwise it
  // is a normal single_elim level.
  const { format, params } = withFinal({ format: 'single_elim', params: {} }, final);
  return {
    label,
    classes: [...source.classes],
    format,
    params,
    // The bracket's chosen win condition decides every heat (First-to-N / Most-laps-in-N-min), not the
    // qual's ranking metric. Both self-terminate, so no separate race time is needed.
    win_condition: winCondition,
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
 * Assemble the **first bracket level** seeded straight from a **class roster** — a tournament built
 * without a qualifying round (Build tournament, seed source = the pilot list). The level is a
 * `single_elim` round over `classId`, seeded `FromRoster` (the class's members, in roster/seed order),
 * carrying the bracket's chosen win condition. The generator bracket-pairs the field and byes any odd
 * seed, so it works for any roster size. `final` applies only when this single level *is* the final
 * (a 2-pilot roster). The next levels chain off it with {@link advanceLevelReq}.
 */
export function rosterRoundReq(
  classId: string,
  label: string,
  winCondition: WinCondition,
  final?: BracketFinal
): NewRoundReq {
  const { format, params } = withFinal({ format: 'single_elim', params: {} }, final);
  return {
    label,
    classes: [classId],
    format,
    params,
    win_condition: winCondition,
    seeding: 'FromRoster'
  };
}

/**
 * Assemble the request for the **next bracket level** advancing `level` produces (#217, decisions
 * D13: one round per level). Unlike {@link advanceRoundReq} (quali → level 1, seeded `FromRanking`),
 * a level → next-level advance reuses the same bracket format and is seeded
 * `FromHeatWinners { source_round: <level> }` — the per-level generator pairs the level's heat
 * winners into the next level's heats. Carries the level's classes + win condition unchanged; the RD
 * picks the `label` (e.g. "Semifinals").
 *
 * `finalFormat` overrides the next level's format for the **final** (the final-format selector): omit
 * (the default) to reuse the level's bracket format — a single decisive race; pass
 * `{ format: 'chase_the_ace', winsToWin }` for a multi-race Chase-the-Ace final. A Chase-the-Ace
 * level carries only its `wins_to_win` param (its field is the seeded finalists, not heat-sized).
 */
export function advanceLevelReq(
  level: RoundDef,
  label: string,
  winCondition: WinCondition,
  final?: BracketFinal
): NewRoundReq {
  const { format, params } = withFinal(
    { format: level.format, params: { ...(level.params ?? {}) } },
    final
  );
  return {
    label,
    classes: [...level.classes],
    format,
    params,
    // Every bracket level shares the bracket's win condition (set once in the builder); it
    // self-terminates, so no race time is carried.
    win_condition: winCondition,
    seeding: { FromHeatWinners: { source_round: level.id } },
    channel_mode: level.channel_mode
  };
}
