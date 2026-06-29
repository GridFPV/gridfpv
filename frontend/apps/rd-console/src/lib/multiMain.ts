/**
 * Multi-main derivations — fold a multi-main round's heats + results + ranking into the
 * {@link MultiMain} view-model `MultiMainView` renders (the builder lives in `standings.ts`).
 *
 * A multi-main round (format `multi_main`) is one finals round whose heats are the tiered mains —
 * `main-A`, `main-B`, `main-C`, … (best tier first) — and whose canonical `session.roundRanking`
 * concatenates the mains' results tier-by-tier into the full-field standing. The view is
 * standings-only (the mains themselves live in the normal Heats area), with a **Tier** column so the
 * structure is visible: each pilot's tier is derived from which `main-X` heat their competitor appears
 * in (its scored result, else its lineup), mapping the heat's tier index via {@link mainTierName}.
 *
 * Kept framework-pure (no Svelte) so it unit-tests directly and the Rounds stage + Results share one
 * source of truth.
 */

import type { CompetitorRef, HeatId, HeatResult, HeatSummary, RankEntry } from '@gridfpv/types';
import type { MultiMain, MultiMainStanding } from '@gridfpv/components';

// Re-export the shared multi-main helpers so consumers import all multi-main concerns from one place
// (the round predicate lives in heats.ts with the other round predicates — no duplication).
export { isMultiMainRound, mainTierName } from './heats.js';

/** The multi-main format key. */
export const MULTI_MAIN = 'multi_main';

/**
 * The 0-based **tier index** of a `main-X` heat id (`main-A` → 0, `main-B` → 1, …), matching the
 * engine's `MultiMain::main_id` scheme; `main-26`, `main-27`, … (the >26-mains fallback) parse off
 * their numeric suffix. Returns `undefined` for any id that does not match the scheme (so a
 * non-multi-main / hand-built heat is ignored for tiering).
 */
export function mainTierIndexOf(heatId: string): number | undefined {
  const letter = /^main-([A-Z])$/.exec(heatId);
  if (letter) return letter[1].charCodeAt(0) - 65;
  const numeric = /^main-(\d+)$/.exec(heatId);
  if (numeric) return Number(numeric[1]);
  return undefined;
}

/**
 * The tier index each competitor raced in, across the round's `heats`. A competitor is placed by the
 * `main-X` heat they appear in — its scored result (authoritative finishers) when available, else its
 * lineup. The **best** (lowest) tier index wins, so a bump-ladder pilot who fought up from a lower
 * main shows the highest main they reached (matching how the engine ranks them once, in that main).
 */
function tierIndexByCompetitor(
  heats: HeatSummary[],
  resultByHeat: (heatId: HeatId) => HeatResult | undefined
): Map<CompetitorRef, number> {
  const tiers = new Map<CompetitorRef, number>();
  const note = (ref: CompetitorRef, index: number) => {
    const prev = tiers.get(ref);
    if (prev === undefined || index < prev) tiers.set(ref, index);
  };
  for (const h of heats) {
    const index = mainTierIndexOf(h.heat);
    if (index === undefined) continue;
    const res = resultByHeat(h.heat);
    if (res && res.places.length > 0) {
      for (const p of res.places) note(p.competitor.competitor, index);
    } else {
      for (const ref of h.lineup) note(ref, index);
    }
  }
  return tiers;
}

/**
 * Build the {@link MultiMain} view-model for a multi-main round: the aggregate standings in the
 * **canonical** order `ranking` (`session.roundRanking`) returns, each row tagged with the pilot's
 * tier (from the `main-X` heat they raced in, via {@link mainTierName}). Pure + fetch-free — the
 * caller supplies the ranking, the round's heats, and the resolvers/result accessor.
 *
 * When `ranking` is empty (the round isn't scored yet) the rows fall back to the heats' lineup order
 * (deduped, first-seen) so the table still shows the field with its tiers.
 *
 * @param opts.label resolve a competitor ref → its friendly callsign.
 * @param opts.tierNameOf resolve a 0-based tier index → its friendly tier name (pass `mainTierName`).
 * @param opts.resultByHeat resolve a heat id → its scored {@link HeatResult}, if any (for tiering).
 */
export function buildMultiMainView(
  ranking: RankEntry[],
  heats: HeatSummary[],
  opts: {
    label: (ref: CompetitorRef) => string;
    tierNameOf: (index: number) => string;
    resultByHeat?: (heatId: HeatId) => HeatResult | undefined;
  }
): MultiMain {
  const tiers = tierIndexByCompetitor(heats, opts.resultByHeat ?? (() => undefined));

  const rowFor = (ref: CompetitorRef, position: number): MultiMainStanding => {
    const index = tiers.get(ref);
    return {
      competitor: ref,
      label: opts.label(ref),
      position,
      tierName: index !== undefined ? opts.tierNameOf(index) : undefined
    };
  };

  if (ranking.length > 0) {
    return { standings: ranking.map((r) => rowFor(r.competitor, r.position)) };
  }

  // No ranking yet: fall back to the field in first-seen lineup order.
  const seen: CompetitorRef[] = [];
  for (const h of heats) for (const ref of h.lineup) if (!seen.includes(ref)) seen.push(ref);
  return { standings: seen.map((ref, i) => rowFor(ref, i + 1)) };
}
