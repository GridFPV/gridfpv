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

import type { Bracket, BracketMatch, BracketRound, BracketSlot } from '@gridfpv/components';
import type {
  CompetitorRef,
  HeatResult,
  HeatSummary,
  RankEntry,
  RoundDef,
  RoundId,
  SeedingRule
} from '@gridfpv/types';

import { isBracketFormat, isChaseTheAceFormat } from './formats.js';
import { bracketLevelFields } from './standings.js';

/**
 * The replayed state of a **Chase-the-Ace** final — the per-finalist race-win tally a chase series
 * carries, computed by {@link chaseWinTally} from the final round's completed-race results. The
 * frontend counts race winners itself because the engine ranking exposes only an overall placement,
 * not a per-finalist win count; this is the "series score" the collapsed final node renders.
 */
export interface ChaseFinalTally {
  /** Race-wins per finalist (`competitor ref → wins`); finalists with no win yet may be absent. */
  wins: Record<CompetitorRef, number>;
  /** The race-wins needed to take the final (`wins_to_win`, ≥ 1). */
  target: number;
  /** How many of the final's races have been scored. */
  racesRun: number;
  /** The champion — the first finalist to reach {@link target} race-wins — once one exists. */
  champion?: CompetitorRef;
}

/** The race winner of one scored race: its position-1 placement (else the first listed). */
function raceWinner(result: HeatResult): CompetitorRef | undefined {
  const top = result.places.find((p) => p.position === 1) ?? result.places[0];
  return top?.competitor.competitor;
}

/**
 * Count a Chase-the-Ace final's race winners into a {@link ChaseFinalTally}. `results` must be in
 * **race order** (`cta-r0`, `cta-r1`, …) so the champion — the first finalist to reach `target`
 * race-wins — is found correctly. When `target` is omitted (e.g. the Results-page path, where the
 * outcome may not carry `wins_to_win`) it is **inferred** as the most wins any finalist holds (≥ 1),
 * so the champion is the finalist who first reached that winning count.
 */
export function chaseWinTally(results: HeatResult[], target?: number): ChaseFinalTally {
  const wins: Record<CompetitorRef, number> = {};
  const winners: CompetitorRef[] = [];
  for (const r of results) {
    const w = raceWinner(r);
    if (w === undefined) continue;
    wins[w] = (wins[w] ?? 0) + 1;
    winners.push(w);
  }
  const maxWins = Object.values(wins).reduce((m, v) => Math.max(m, v), 0);
  const effectiveTarget = Math.max(1, target ?? maxWins);
  // The champion is the first finalist to reach the target, walking the races in order.
  let champion: CompetitorRef | undefined;
  const running: Record<CompetitorRef, number> = {};
  for (const w of winners) {
    running[w] = (running[w] ?? 0) + 1;
    if (running[w] >= effectiveTarget) {
      champion = w;
      break;
    }
  }
  return { wins, target: effectiveTarget, racesRun: results.length, champion };
}

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

/**
 * The default **per-position points table** for tournament placement — the same `[10, 6, 4, 3, 2, 1]`
 * shape the Head-to-Head round's points editor seeds with. A finish in position `p` scores
 * `DEFAULT_BRACKET_POINTS[p - 1]`; any position past the table (a deep heat) scores `0`. Used as the
 * within-tier tiebreak in {@link tournamentPlacement}: pilots eliminated from the *same* multi-pilot
 * heat tie on placement tier, so points order them, with seed only breaking a points tie.
 */
export const DEFAULT_BRACKET_POINTS: readonly number[] = [10, 6, 4, 3, 2, 1];

/**
 * The **full-field tournament standing** of a bracket chain — every pilot who raced any level, ranked
 * by how deep they got and, within a depth tier, by **points then seed**.
 *
 * Unlike the final level's `roundRanking` (only the finalists), this ranks the *whole* field. A
 * pilot's standing is their **placement tier** — how far they advanced:
 *
 *  - **champion** → tier 0;
 *  - a pilot whose deepest level is the **final** but who isn't the champion (the runner-up / other
 *    finalists) → tier 1;
 *  - a pilot eliminated at level `i` → `finalIndex - i + 1` (so the level before the final = tier 2,
 *    the one before that = tier 3, … — deeper is always a better, lower tier).
 *
 * Within a tier, **seed alone can't order pilots eliminated from the *same* 4–8-pilot heat**, so the
 * tier is ranked by **points descending**, falling back to **seed ascending** (lower = better) only
 * when points tie. For today's 2-up brackets the two SF losers tie on points (each 2nd in their
 * head-to-head), so seed still breaks them — the higher seed taking 3rd; for a multi-pilot heat the
 * pilots' distinct finishes give distinct points that order the tier. Positions are 1-based and
 * tie-aware (competition-ranking `1, 2, 2, 4` — see {@link RankEntry}): a true tie is the same tier
 * AND points AND seed.
 *
 * Pure + fetch-free: the caller supplies the chain rounds, a per-level heats accessor (the level's
 * heat lineups), the champion ref (may be `undefined` for an in-progress bracket — then tier 0 is
 * empty and the finalists lead as tier 1), `seedOf(ref) => number` (lower = better; a non-finite
 * result — an unknown seed — sorts last), and `pointsOf(ref) => number` (higher = better; a pilot
 * with no scored finish in the chain is `0`, so an in-progress / partial bracket falls back to seed).
 *
 * @param chain the bracket chain rounds, earliest level first (see {@link bracketChainRounds}).
 * @param heatsByLevel resolve a level round's id to its scheduled heats (lineups).
 * @param champion the overall bracket winner, or `undefined` while the final is undecided.
 * @param seedOf a competitor's bracket seed (lower = better); non-finite sorts the pilot last.
 * @param pointsOf a competitor's points across the chain's scored heats (higher = better; default 0).
 */
export function tournamentPlacement(
  chain: RoundDef[],
  heatsByLevel: (roundId: RoundId) => HeatSummary[],
  champion: CompetitorRef | undefined,
  seedOf: (ref: CompetitorRef) => number,
  pointsOf: (ref: CompetitorRef) => number
): RankEntry[] {
  const finalIndex = chain.length - 1;
  if (finalIndex < 0) return [];

  // The deepest level index each competitor appears in (across the chain's heat lineups).
  const deepest = new Map<CompetitorRef, number>();
  chain.forEach((round, li) => {
    for (const h of heatsByLevel(round.id)) {
      for (const ref of h.lineup) {
        const prev = deepest.get(ref);
        if (prev === undefined || li > prev) deepest.set(ref, li);
      }
    }
  });
  // The champion always counts as having reached the final, even if the final's heats aren't read.
  if (champion !== undefined && !deepest.has(champion)) deepest.set(champion, finalIndex);

  const tierOf = (ref: CompetitorRef, depth: number): number =>
    champion !== undefined && ref === champion ? 0 : finalIndex - depth + 1;

  const normSeed = (ref: CompetitorRef): number => {
    const s = seedOf(ref);
    return Number.isFinite(s) ? s : Number.POSITIVE_INFINITY;
  };
  const normPoints = (ref: CompetitorRef): number => {
    const p = pointsOf(ref);
    return Number.isFinite(p) ? p : 0;
  };

  const ranked = [...deepest.entries()]
    .map(([competitor, depth]) => ({
      competitor,
      tier: tierOf(competitor, depth),
      points: normPoints(competitor),
      seed: normSeed(competitor)
    }))
    // Within a tier: points descending, then seed ascending (lower = better).
    .sort((a, b) => a.tier - b.tier || b.points - a.points || a.seed - b.seed);

  // 1-based, tie-aware (competition-ranking) positions: a tie shares the leader's position and the
  // next distinct entry skips past (1, 2, 2, 4). A tie is the same tier AND points AND seed.
  const out: RankEntry[] = [];
  let position = 0;
  let prevTier: number | undefined;
  let prevPoints: number | undefined;
  let prevSeed: number | undefined;
  ranked.forEach((entry, k) => {
    if (entry.tier !== prevTier || entry.points !== prevPoints || entry.seed !== prevSeed)
      position = k + 1;
    prevTier = entry.tier;
    prevPoints = entry.points;
    prevSeed = entry.seed;
    out.push({ competitor: entry.competitor, position });
  });
  return out;
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
 * @param levelOneField the seed count entering level 1 (the bracket size / roster size). With
 *   `heatSize` this derives the expected match count per level, so a built-ahead bracket renders
 *   every level — empty seats reading as "TBD" — not just the levels whose heats already exist.
 * @param heatSize the pilots per heat the bracket groups on (`single_elim`'s `heat_size`).
 * @param advance how many advance out of each heat (`single_elim`'s `advance`); shapes the geometry.
 * @param chaseFinal the replayed {@link ChaseFinalTally} when the final level is a Chase-the-Ace
 *   round — its N race heats collapse into ONE final match showing the per-finalist series score
 *   (a slot `score`) + a race-counter caption (the match `note`), the champion marked the winner.
 */
export function buildBracketView(
  root: RoundDef,
  rounds: RoundDef[],
  heats: HeatSummary[],
  label: (ref: CompetitorRef) => string,
  champion: CompetitorRef | undefined,
  levelOneField: number,
  heatSize: number,
  advance: number = Math.max(1, Math.floor(heatSize / 2)),
  chaseFinal?: ChaseFinalTally
): Bracket {
  const chain = bracketChainRounds(root, rounds);
  const levels = chain.map((r) => ({ round: r, heats: heatsOf(r.id, heats) }));
  // The geometry of the whole bracket: the field entering each level, so a level with no (or fewer)
  // heats yet still renders the right number of (TBD) matches.
  const fields = bracketLevelFields(levelOneField, heatSize, advance);
  const slotsPerMatch = Math.max(2, Math.floor(heatSize));

  const bracketRounds: BracketRound[] = levels.map((level, li) => {
    // The set of competitors seated in the NEXT level — anyone here who is there advanced.
    const nextLineups = new Set<CompetitorRef>(
      (levels[li + 1]?.heats ?? []).flatMap((h) => h.lineup)
    );
    const isFinalLevel = li === levels.length - 1;

    // A **Chase-the-Ace** final collapses its N race heats into ONE match (the whole field flies
    // every race, so the per-race heats are not distinct matches): the finalists are the slots, each
    // carrying its series score (race-win count), with the champion marked and a race-counter note.
    if (isFinalLevel && isChaseTheAceFormat(level.round.format)) {
      const finalists: CompetitorRef[] = [];
      for (const h of level.heats) {
        for (const ref of h.lineup) if (!finalists.includes(ref)) finalists.push(ref);
      }
      const target =
        chaseFinal?.target ?? Math.max(1, Math.round(Number(level.round.params?.wins_to_win ?? 2)));
      const racesRun = chaseFinal?.racesRun ?? 0;
      const note =
        chaseFinal && racesRun > 0
          ? `Best of ${target * 2 - 1} · ${racesRun} race${racesRun === 1 ? '' : 's'}`
          : 'Series';
      const champ = chaseFinal?.champion;
      const slots: BracketSlot[] = finalists.length
        ? finalists.map((ref) => ({
            competitor: ref,
            label: label(ref),
            score: chaseFinal?.wins[ref] ?? 0,
            winner: champ !== undefined && ref === champ
          }))
        : // No race heats yet (built-ahead final): one placeholder match of TBD finalist seats.
          Array.from({ length: Math.max(2, Math.round(fields[li] ?? slotsPerMatch)) }, () => ({}));
      return {
        name: splitBracketLabel(level.round.label).level,
        matches: [{ heat: undefined, note, slots }]
      };
    }

    // The real heats this level already has, with winners inferred as before.
    const realMatches: BracketMatch[] = level.heats.map((h) => ({
      heat: h.heat,
      slots: h.lineup.map((ref) => ({
        competitor: ref,
        label: label(ref),
        winner: isFinalLevel ? champion !== undefined && ref === champion : nextLineups.has(ref)
      }))
    }));

    // How many matches this level should ultimately hold (from the bracket geometry). Falls back to
    // the real heat count when the geometry doesn't reach this level (an unexpected extra level).
    const expectedMatches =
      fields[li] !== undefined ? Math.ceil((fields[li] ?? 0) / slotsPerMatch) : realMatches.length;

    // Pad with placeholder (TBD) matches up to the expected count: a level with no/fewer heats yet
    // still shows its shape, each empty slot ({}) rendering as "TBD".
    const matches: BracketMatch[] = [...realMatches];
    for (let m = matches.length; m < expectedMatches; m++) {
      matches.push({ heat: undefined, slots: Array.from({ length: slotsPerMatch }, () => ({})) });
    }

    // The tree column shows just the level name (e.g. "Quarterfinals"), stripping the bracket-name
    // prefix that the level's round label carries ("‹Bracket› — ‹Level›") — the container header
    // already shows the bracket name.
    return { name: splitBracketLabel(level.round.label).level, matches };
  });

  return { rounds: bracketRounds };
}

/**
 * Split a bracket level's round label into its **bracket name** + **level name**. The builder labels
 * each level `"‹Bracket name› — ‹Level›"` (e.g. `"Pro — Quarterfinals"`) so multiple brackets in one
 * event are distinguishable; this recovers the parts for display (the container header shows `name`,
 * the tree column shows `level`). A label with no ` — ` separator has no prefix — the whole label is
 * the level name. Splits on the **last** separator, so a bracket name that itself contains ` — `
 * survives.
 */
export function splitBracketLabel(label: string): { name: string; level: string } {
  const sep = ' — ';
  const i = label.lastIndexOf(sep);
  if (i < 0) return { name: '', level: label };
  return { name: label.slice(0, i), level: label.slice(i + sep.length) };
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

/** One entry in the Rounds-stage display list: a whole bracket chain, or a standalone round. */
export type RoundGroup =
  | { kind: 'bracket'; root: RoundDef; levels: RoundDef[] }
  | { kind: 'round'; round: RoundDef };

/**
 * Group the event's `rounds` for display: a single-elim bracket's level rounds collapse into ONE
 * `bracket` group (so the Rounds stage shows the chain as a container, not three loose rounds),
 * every other round stays a standalone `round`. Order is preserved by each chain's **root** position
 * — the bracket group appears where its first level sits in `rounds`, and the chain's later levels
 * are folded into it (never emitted on their own).
 *
 * A bracket level that names a `FromHeatWinners` source which isn't actually a root chain in this
 * event (an orphan — its root was deleted) falls back to a standalone `round` so it never vanishes.
 * Keeps the level rounds first-class (each is still its own `RoundDef`); this is a pure view grouping.
 */
export function groupRoundsForDisplay(rounds: RoundDef[]): RoundGroup[] {
  // Every round reachable from some root's chain — these are folded into their bracket group and are
  // never emitted standalone (the root carries them).
  const chained = new Set<RoundId>();
  for (const r of rounds) {
    if (isBracketRoot(r)) for (const c of bracketChainRounds(r, rounds)) chained.add(c.id);
  }

  const consumed = new Set<RoundId>();
  const groups: RoundGroup[] = [];
  for (const round of rounds) {
    if (consumed.has(round.id)) continue;
    if (isBracketRoot(round)) {
      const levels = bracketChainRounds(round, rounds);
      levels.forEach((l) => consumed.add(l.id));
      groups.push({ kind: 'bracket', root: round, levels });
    } else if (chained.has(round.id)) {
      // A non-root level its root chain already folds in — skip; the root group emits it.
      continue;
    } else {
      consumed.add(round.id);
      groups.push({ kind: 'round', round });
    }
  }
  return groups;
}
