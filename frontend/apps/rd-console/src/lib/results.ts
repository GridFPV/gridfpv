/**
 * Results-screen derivations (#56).
 *
 * The results screen renders heat results (`Leaderboard`), rankings/standings
 * (`StandingsTable`), and a bracket (`BracketTree`) from typed projection data. Two of
 * those map straight to a wire type; the bracket does not (the wire has no bracket
 * projection — see `@gridfpv/components`'s `bracket.ts`), so we derive the component's
 * `Bracket` view-model from an `EventOutcome`'s completed bracket heats. This is the
 * caller-side derivation that `bracket.ts` describes.
 *
 * Export is a JSON download of the typed projection — good enough for v0.4 (#56) and
 * lossless, since it is the exact wire value.
 */

import type { Bracket, BracketMatch, BracketRound, BracketSlot } from '@gridfpv/components';
import type {
  CompetitorRef,
  CompletedHeat,
  EventOutcome,
  HeatResult,
  Placement
} from '@gridfpv/types';

import { chaseWinTally } from './brackets.js';

/** The race index of a Chase-the-Ace heat id (`cta-r{n}`), for ordering races; else +∞ (sorts last). */
function chaseRaceIndex(heatId: string): number {
  const m = /^cta-r(\d+)$/.exec(heatId);
  return m ? Number(m[1]) : Number.MAX_SAFE_INTEGER;
}

/** Whether a heat id belongs to a Chase-the-Ace final (race ids are `cta-r{n}`). */
function isChaseHeatId(heatId: string): boolean {
  return heatId.startsWith('cta-');
}

/** The top (winning) placement's competitor ref, if the heat has any places. */
function winnerOf(result: HeatResult): CompetitorRef | undefined {
  const top: Placement | undefined = result.places[0];
  return top?.competitor.competitor;
}

/**
 * Derive a `Bracket` view-model from an `EventOutcome`'s completed bracket heats.
 *
 * Without a named round structure on the wire we lay each completed bracket heat out
 * as its own match and group sequential heats into rounds by halving: the last heat is
 * the final, the two before it the semis, and so on. This mirrors a single-elim shape
 * well enough for display; when the server grows a real bracket projection this is
 * replaced wholesale.
 */
export function bracketFromOutcome(outcome: EventOutcome): Bracket {
  const allHeats = outcome.bracket_heats;
  if (allHeats.length === 0) return { rounds: [] };

  // A Chase-the-Ace final's race heats (`cta-r{n}`) collapse into ONE final match (see below); the
  // remaining heats group into single-elim levels by halving as before.
  const chaseHeats = allHeats.filter((h) => isChaseHeatId(h.heat));
  const normalHeats = allHeats.filter((h) => !isChaseHeatId(h.heat));

  const matches: BracketMatch[] = normalHeats.map((h: CompletedHeat) => {
    const winner = winnerOf(h.result);
    return {
      heat: h.heat,
      slots: h.result.places.map((p) => ({
        competitor: p.competitor.competitor,
        winner: p.competitor.competitor === winner
      }))
    };
  });

  // Group from the end: final (1), then doubling round sizes backward (…, 4, 2, 1).
  const rounds: BracketRound[] = [];
  let remaining = matches.slice();
  let size = 1;
  while (remaining.length > 0) {
    const take = Math.min(size, remaining.length);
    const roundMatches = remaining.slice(remaining.length - take);
    remaining = remaining.slice(0, remaining.length - take);
    rounds.unshift({ name: roundNameFor(size, roundMatches.length), matches: roundMatches });
    size *= 2;
  }

  // Append the collapsed Chase-the-Ace final as the last round when present.
  if (chaseHeats.length > 0) rounds.push(chaseFinalRound(chaseHeats));
  return { rounds };
}

/**
 * Collapse a Chase-the-Ace final's race heats into ONE `Final` round with a single match: the
 * finalists as slots carrying their series score (race-win count), the champion (first to the
 * winning count) marked, and a `"Best of N · races"` caption. The wins target is inferred from the
 * results (the outcome may not carry `wins_to_win`) by {@link chaseWinTally}.
 */
function chaseFinalRound(chaseHeats: CompletedHeat[]): BracketRound {
  const ordered = chaseHeats
    .slice()
    .sort((a, b) => chaseRaceIndex(a.heat) - chaseRaceIndex(b.heat));
  const tally = chaseWinTally(
    ordered.map((h) => h.result),
    undefined
  );
  // The finalists in seed order: the first race's field, then any others (each race flies them all).
  const finalists: CompetitorRef[] = [];
  for (const h of ordered) {
    for (const p of h.result.places) {
      const ref = p.competitor.competitor;
      if (!finalists.includes(ref)) finalists.push(ref);
    }
  }
  const slots: BracketSlot[] = finalists.map((ref) => ({
    competitor: ref,
    score: tally.wins[ref] ?? 0,
    winner: tally.champion !== undefined && ref === tally.champion
  }));
  const note = `Best of ${tally.target * 2 - 1} · ${tally.racesRun} race${tally.racesRun === 1 ? '' : 's'}`;
  return { name: 'Final', matches: [{ heat: undefined, note, slots }] };
}

function roundNameFor(size: number, count: number): string {
  if (size === 1 && count === 1) return 'Final';
  if (size === 2) return 'Semifinals';
  if (size === 4) return 'Quarterfinals';
  return `Round of ${size * 2}`;
}

/** Serialize a value to pretty JSON; the bigint replacer is a defensive no-op now
 * that wire numerics are plain `number`s. */
export function toExportJson(value: unknown): string {
  return JSON.stringify(value, (_k, v) => (typeof v === 'bigint' ? Number(v) : v), 2);
}

/**
 * Trigger a browser download of `json` as `filename`. No-op outside a DOM (tests call
 * `toExportJson` directly). Returns whether a download was initiated.
 */
export function downloadJson(filename: string, json: string): boolean {
  if (typeof document === 'undefined' || typeof URL?.createObjectURL !== 'function') return false;
  const blob = new Blob([json], { type: 'application/json' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
  return true;
}
