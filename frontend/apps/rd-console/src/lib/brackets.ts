/**
 * Bracket **level-chain** derivations (#217 — one round per bracket level, decisions D13).
 *
 * Under D13 a single-elimination bracket is no longer one internally-stateful round: it is a
 * **chain of rounds**, one per level. Level 1 is the round advancing-to-brackets created (seeded
 * `FromRanking` from the quali cut); each subsequent level is a round seeded
 * `FromHeatWinners { source_round: <prior level> }`, whose heats pair the prior level's heat winners.
 * The chain runs to a single-heat final.
 *
 * This module stitches that chain back together for the UI from data the Rounds & Heats stage
 * already holds — the event's `rounds` and the scheduled `heats` (with their lineups + phases) —
 * with no extra per-heat result fetch:
 *
 *  - {@link bracketChainRounds} walks the `FromHeatWinners` links forward from a level-1 round to
 *    return the ordered level rounds (Quarters → Semis → Final).
 *  - {@link buildBracketView} folds those level rounds + their heats into the component-local
 *    {@link Bracket} view-model `BracketTree` renders. A heat's **winner** is inferred structurally:
 *    the competitor in this heat who reappears in the *next* level's heats advanced from it. The
 *    final level has no next level, so its winner is supplied (the champion) when known.
 *  - {@link isLevelComplete} reports whether a level's heats are all scored (every one `Final`),
 *    the gate the "Advance bracket" action opens on.
 *
 * Kept framework-pure (no Svelte) so it unit-tests directly and the screen + tests share one source
 * of truth.
 */

import type { Bracket, BracketMatch, BracketRound } from '@gridfpv/components';
import type { CompetitorRef, HeatSummary, RoundDef, RoundId, SeedingRule } from '@gridfpv/types';

import { isBracketFormat } from './formats.js';

/** The `source_round` a `FromHeatWinners` seeding points at, or `undefined` for any other rule. */
export function heatWinnersSource(seeding: SeedingRule): RoundId | undefined {
  if (typeof seeding === 'string') return undefined;
  if ('FromHeatWinners' in seeding) return seeding.FromHeatWinners.source_round;
  return undefined;
}

/**
 * Whether `round` is a **bracket level** — a bracket-format round that is either the chain's first
 * level (seeded `FromRanking`, the advance-to-brackets entry) or a later level (seeded
 * `FromHeatWinners`). A bracket round seeded straight from the roster is degenerate but still counts.
 */
export function isBracketLevel(round: RoundDef): boolean {
  return isBracketFormat(round.format);
}

/**
 * Whether `round` is the **first level** of a bracket chain — a bracket round that is *not* itself
 * seeded from another level's heat winners (so nothing chains into it). This is the round
 * advance-to-brackets creates; every other level chains off it via `FromHeatWinners`.
 */
export function isBracketRoot(round: RoundDef): boolean {
  return isBracketLevel(round) && heatWinnersSource(round.seeding) === undefined;
}

/**
 * The ordered level rounds of the bracket chain rooted at `root`, earliest level first
 * (`[root, level2, level3, …]`). Walks the `FromHeatWinners` links forward: the next level is the
 * round whose seeding names the current level as its `source_round`. Stops at the final level (no
 * round chains off it) and guards against a cycle so a malformed chain can't loop forever.
 */
export function bracketChainRounds(root: RoundDef, rounds: RoundDef[]): RoundDef[] {
  const chain: RoundDef[] = [root];
  const seen = new Set<RoundId>([root.id]);
  let current = root;
  for (;;) {
    const next = rounds.find(
      (r) => isBracketLevel(r) && heatWinnersSource(r.seeding) === current.id && !seen.has(r.id)
    );
    if (!next) break;
    chain.push(next);
    seen.add(next.id);
    current = next;
  }
  return chain;
}

/** A round's heats, in list (generation) order. */
function heatsOf(roundId: RoundId, heats: HeatSummary[]): HeatSummary[] {
  return heats.filter((h) => h.round === roundId);
}

/** Whether every one of `round`'s heats is scored (`Final`) — and there is at least one. */
export function isLevelComplete(roundId: RoundId, heats: HeatSummary[]): boolean {
  const hs = heatsOf(roundId, heats);
  return hs.length > 0 && hs.every((h) => h.phase === 'Final');
}

/**
 * Build the {@link Bracket} view-model for the chain rooted at `root`.
 *
 * Each level becomes a {@link BracketRound} (named by its round label); each of the level's heats a
 * {@link BracketMatch} whose slots are the heat's lineup, resolved to a display label via `label`.
 * The advancing seat is inferred without a result fetch: a heat's winner is the lineup competitor
 * who appears in the **next** level's combined lineups (they were seeded forward from this heat).
 * For the final level there is no next level, so the optional `champion` marks its winner when the
 * final is scored.
 *
 * @param root the chain's first-level round (see {@link isBracketRoot}).
 * @param rounds the event's rounds (to resolve the chain).
 * @param heats the scheduled heats (lineups + phases).
 * @param label resolve a `CompetitorRef` to a display string (callsign).
 * @param champion the overall bracket winner, marked on the final's heat when known.
 */
export function buildBracketView(
  root: RoundDef,
  rounds: RoundDef[],
  heats: HeatSummary[],
  label: (ref: CompetitorRef) => string,
  champion?: CompetitorRef
): Bracket {
  const chain = bracketChainRounds(root, rounds);
  const levels = chain.map((r) => ({ round: r, heats: heatsOf(r.id, heats) }));

  const bracketRounds: BracketRound[] = levels.map((level, li) => {
    // The set of competitors seated in the NEXT level — anyone here who is there advanced.
    const nextLineups = new Set<CompetitorRef>(
      (levels[li + 1]?.heats ?? []).flatMap((h) => h.lineup)
    );
    const isFinalLevel = li === levels.length - 1;

    const matches: BracketMatch[] = level.heats.map((h) => ({
      heat: h.heat,
      slots: h.lineup.map((ref) => ({
        competitor: ref,
        label: label(ref),
        winner: isFinalLevel ? champion !== undefined && ref === champion : nextLineups.has(ref)
      }))
    }));

    return { name: level.round.label, matches };
  });

  return { rounds: bracketRounds };
}

/**
 * A sensible **next-level label** when advancing a bracket. Single-elim levels read best by their
 * size: a 1-heat next level is the *Final*, a 2-heat level the *Semifinals*, a 4-heat level the
 * *Quarterfinals*, else a *Round of N*. `nextHeatCount` is how many heats the next level will hold
 * (half the current level's, since winners pair up). Falls back to "<root> — Round k" when the size
 * is unknown (0). The RD can always override the offered label.
 */
export function nextLevelLabel(
  rootLabel: string,
  nextHeatCount: number,
  levelIndex: number
): string {
  switch (nextHeatCount) {
    case 1:
      return 'Final';
    case 2:
      return 'Semifinals';
    case 4:
      return 'Quarterfinals';
    case 8:
      return 'Round of 16';
    default:
      return nextHeatCount > 0
        ? `Round of ${nextHeatCount * 2}`
        : `${rootLabel} — Round ${levelIndex + 1}`;
  }
}
