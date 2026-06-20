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

import type { Bracket, BracketMatch, BracketRound } from '@gridfpv/components';
import type {
  CompetitorRef,
  CompletedHeat,
  EventOutcome,
  HeatResult,
  Placement
} from '@gridfpv/types';

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
  const heats = outcome.bracket_heats;
  if (heats.length === 0) return { rounds: [] };

  const matches: BracketMatch[] = heats.map((h: CompletedHeat) => {
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
  return { rounds };
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
