import { describe, expect, it } from 'vitest';
import type { HeatResult, HeatSummary, RoundDef } from '@gridfpv/types';
import {
  bracketChainRounds,
  buildBracketView,
  chaseWinTally,
  groupRoundsForDisplay,
  heatWinnersSource,
  isBracketRoot,
  isLevelComplete,
  nextLevelLabel,
  splitBracketLabel
} from '../src/lib/brackets.js';

/** A minimal RoundDef builder — only the fields the bracket-chain logic reads matter. */
function round(over: Partial<RoundDef> & Pick<RoundDef, 'id' | 'label' | 'seeding'>): RoundDef {
  return {
    classes: ['c1'],
    format: 'single_elim',
    params: {},
    win_condition: 'BestLap',
    channel_mode: 'PerHeat',
    staging_timer_secs: 300,
    start_procedure: { mode: 'randomized-delay', min_delay_ms: 2000, max_delay_ms: 5000 },
    grace_window: { Duration: { micros: 3_000_000 } },
    protest_window: 'Off',
    ...over
  };
}

/** A minimal HeatSummary — lineup + round + phase are what the chain view reads. */
function heat(
  id: string,
  roundId: string,
  lineup: string[],
  phase: HeatSummary['phase'] = 'Final'
): HeatSummary {
  return { heat: id, lineup, round: roundId, phase, is_current: false };
}

// A 4-pilot single-elim: a 2-heat semis level (root) → a 1-heat final.
const SEMIS = round({
  id: 'semis',
  label: 'Semifinals',
  seeding: { FromRanking: { source_rounds: ['q'], top_n: 4 } }
});
const FINAL = round({
  id: 'final',
  label: 'Final',
  seeding: { FromHeatWinners: { source_round: 'semis' } }
});
const QUAL = round({ id: 'q', label: 'Qualifying', format: 'timed_qual', seeding: 'FromRoster' });

describe('heatWinnersSource / isBracketRoot', () => {
  it('reads the source_round of a FromHeatWinners seeding, else undefined', () => {
    expect(heatWinnersSource(FINAL.seeding)).toBe('semis');
    expect(heatWinnersSource(SEMIS.seeding)).toBeUndefined();
    expect(heatWinnersSource('FromRoster')).toBeUndefined();
  });

  it('a FromRanking-seeded bracket round is the chain root; a FromHeatWinners one is not', () => {
    expect(isBracketRoot(SEMIS)).toBe(true);
    expect(isBracketRoot(FINAL)).toBe(false);
    // A qualifying round is not a bracket level at all.
    expect(isBracketRoot(QUAL)).toBe(false);
  });
});

describe('bracketChainRounds — walks FromHeatWinners forward', () => {
  it('returns the ordered levels from the root to the final', () => {
    const chain = bracketChainRounds(SEMIS, [QUAL, FINAL, SEMIS]);
    expect(chain.map((r) => r.id)).toEqual(['semis', 'final']);
  });

  it('stops at the final (nothing chains off it) and is a single-element chain for a lone level', () => {
    expect(bracketChainRounds(FINAL, [FINAL]).map((r) => r.id)).toEqual(['final']);
  });
});

describe('isLevelComplete — every heat Final', () => {
  it('is true only when the level has heats and all are Final', () => {
    const heats = [heat('sf-1', 'semis', ['A', 'D']), heat('sf-2', 'semis', ['B', 'C'])];
    expect(isLevelComplete('semis', heats)).toBe(true);
    expect(isLevelComplete('semis', [...heats, heat('sf-3', 'semis', ['x'], 'Running')])).toBe(
      false
    );
    expect(isLevelComplete('semis', [])).toBe(false);
  });
});

describe('buildBracketView — stitches the level chain, infers winners', () => {
  // Semis: A beats D, B beats C → Final seats A vs B. A is champion.
  const heats = [
    heat('sf-1', 'semis', ['A', 'D']),
    heat('sf-2', 'semis', ['B', 'C']),
    heat('f-1', 'final', ['A', 'B'])
  ];

  it('lays out one column per level, named by the round label', () => {
    const view = buildBracketView(SEMIS, [QUAL, FINAL, SEMIS], heats, (r) => r, undefined, 4, 2);
    expect(view.rounds.map((r) => r.name)).toEqual(['Semifinals', 'Final']);
    // Two semis matches, one final.
    expect(view.rounds[0].matches.length).toBe(2);
    expect(view.rounds[1].matches.length).toBe(1);
  });

  it('infers a semifinal heat winner as the seat who reappears in the final', () => {
    const view = buildBracketView(SEMIS, [QUAL, FINAL, SEMIS], heats, (r) => r, undefined, 4, 2);
    const sf1 = view.rounds[0].matches[0];
    const winners = sf1.slots.filter((s) => s.winner).map((s) => s.competitor);
    expect(winners).toEqual(['A']); // A advanced to the final, D did not
  });

  it('marks the final winner from the supplied champion, and resolves labels', () => {
    const view = buildBracketView(
      SEMIS,
      [QUAL, FINAL, SEMIS],
      heats,
      (r) => `Pilot-${r}`,
      'A',
      4,
      2
    );
    const final = view.rounds[1].matches[0];
    expect(final.slots.find((s) => s.competitor === 'A')?.winner).toBe(true);
    expect(final.slots.find((s) => s.competitor === 'B')?.winner).toBe(false);
    expect(final.slots[0].label).toBe('Pilot-A');
  });

  it('leaves the final unmarked when no champion is known yet', () => {
    const view = buildBracketView(SEMIS, [QUAL, FINAL, SEMIS], heats, (r) => r, undefined, 4, 2);
    const final = view.rounds[1].matches[0];
    expect(final.slots.every((s) => !s.winner)).toBe(true);
  });
});

describe('buildBracketView — built-ahead levels render with TBD placeholder matches', () => {
  it('a level with no heats yet still shows the expected match count, all seats TBD', () => {
    // Level 1 (semis) has its two heats; the final has none yet (built ahead). With a 4-seed field +
    // 2-up heats the geometry is [4, 2] → 2 semis matches, 1 final match (placeholder).
    const heats = [heat('sf-1', 'semis', ['A', 'D']), heat('sf-2', 'semis', ['B', 'C'])];
    const view = buildBracketView(SEMIS, [QUAL, FINAL, SEMIS], heats, (r) => r, undefined, 4, 2);
    expect(view.rounds.map((r) => r.name)).toEqual(['Semifinals', 'Final']);
    expect(view.rounds[0].matches.length).toBe(2); // real heats
    expect(view.rounds[1].matches.length).toBe(1); // placeholder final
    const final = view.rounds[1].matches[0];
    expect(final.heat).toBeUndefined();
    expect(final.slots.length).toBe(2);
    // Empty slots carry no competitor/label → BracketTree renders them as "TBD".
    expect(final.slots.every((s) => s.competitor === undefined && s.label === undefined)).toBe(
      true
    );
  });

  it('a from-scratch root with no heats yields every level as TBD matches', () => {
    const view = buildBracketView(SEMIS, [QUAL, FINAL, SEMIS], [], (r) => r, undefined, 4, 2);
    // Both levels render fully from the geometry: 2 TBD semis matches, 1 TBD final.
    expect(view.rounds[0].matches.length).toBe(2);
    expect(view.rounds[1].matches.length).toBe(1);
    expect(view.rounds.flatMap((r) => r.matches).every((m) => m.heat === undefined)).toBe(true);
    expect(
      view.rounds
        .flatMap((r) => r.matches)
        .flatMap((m) => m.slots)
        .every((s) => s.competitor === undefined)
    ).toBe(true);
  });

  it('larger heats (top half advance) build the right per-level match counts', () => {
    // 16-seed, 4-up, default advance (2): geometry [16, 8, 4] → ceil/4 = 4, 2, 1 matches; 4 slots each.
    const view = buildBracketView(SEMIS, [QUAL, FINAL, SEMIS], [], (r) => r, undefined, 16, 4);
    // The chain only has two rounds (semis, final), so only the first two geometry levels render.
    expect(view.rounds[0].matches.length).toBe(4);
    expect(view.rounds[1].matches.length).toBe(2);
    expect(view.rounds[0].matches[0].slots.length).toBe(4);
  });

  it('an explicit advance-per-heat reshapes the placeholder geometry', () => {
    // 16-seed, 4-up, advance=1: geometry [16, 4] → ceil/4 = 4 then 1 match (the final is reached one
    // level sooner than the default advance=2 above).
    const view = buildBracketView(SEMIS, [QUAL, FINAL, SEMIS], [], (r) => r, undefined, 16, 4, 1);
    expect(view.rounds[0].matches.length).toBe(4);
    expect(view.rounds[1].matches.length).toBe(1);
  });
});

describe('chaseWinTally — counts race winners, finds the champion', () => {
  /** A scored race result from `[competitor, position]` rows. */
  function res(rows: [string, number][]): HeatResult {
    return {
      places: rows.map(([competitor, position]) => ({
        competitor: { adapter: 'rh-1', competitor },
        position,
        laps: 3,
        metric: { BestLapMicros: 1 }
      }))
    };
  }

  it('tallies wins per finalist and crowns the first to the target', () => {
    // A wins r0, B wins r1 (1-1), A wins r2 (2-1) → A champion at race 2.
    const t = chaseWinTally(
      [
        res([
          ['A', 1],
          ['B', 2]
        ]),
        res([
          ['B', 1],
          ['A', 2]
        ]),
        res([
          ['A', 1],
          ['B', 2]
        ])
      ],
      2
    );
    expect(t.wins).toEqual({ A: 2, B: 1 });
    expect(t.racesRun).toBe(3);
    expect(t.target).toBe(2);
    expect(t.champion).toBe('A');
  });

  it('leaves the champion undefined while no finalist has reached the target', () => {
    const t = chaseWinTally(
      [
        res([
          ['A', 1],
          ['B', 2]
        ]),
        res([
          ['B', 1],
          ['A', 2]
        ])
      ],
      2
    );
    expect(t.wins).toEqual({ A: 1, B: 1 });
    expect(t.champion).toBeUndefined();
  });

  it('infers the target (most wins) when none is given — the Results-page path', () => {
    const t = chaseWinTally([
      res([
        ['A', 1],
        ['B', 2]
      ]),
      res([
        ['A', 1],
        ['B', 2]
      ])
    ]);
    // No explicit target → inferred as the max win count (2); champion is the one who reached it.
    expect(t.target).toBe(2);
    expect(t.champion).toBe('A');
  });

  it('returns an empty, champion-less tally for no races', () => {
    const t = chaseWinTally([], 2);
    expect(t).toEqual({ wins: {}, target: 2, racesRun: 0, champion: undefined });
  });
});

describe('buildBracketView — collapses a Chase-the-Ace final into one match', () => {
  // A 2-up chase final seeded straight from a ranking (a single-level chain): it is BOTH the root and
  // the final level. Three race heats (cta-r0..r2), all flying the same field in seed order.
  const CHASE = round({
    id: 'cta',
    label: 'Aces — Final',
    format: 'chase_the_ace',
    params: { wins_to_win: '2' },
    seeding: { FromRanking: { source_rounds: ['q'], top_n: 2 } }
  });
  const chaseHeats = [
    heat('cta-r0', 'cta', ['A', 'B']),
    heat('cta-r1', 'cta', ['A', 'B']),
    heat('cta-r2', 'cta', ['A', 'B'])
  ];

  it('renders ONE final match with per-finalist scores, a note, and the champion marked', () => {
    const tally = { wins: { A: 2, B: 1 }, target: 2, racesRun: 3, champion: 'A' };
    const view = buildBracketView(
      CHASE,
      [QUAL, CHASE],
      chaseHeats,
      (r) => `P-${r}`,
      tally.champion,
      2,
      2,
      1,
      tally
    );
    // A single-level chain → one column named by its level ("Final"), with exactly one match.
    expect(view.rounds.map((r) => r.name)).toEqual(['Final']);
    expect(view.rounds[0].matches).toHaveLength(1);
    const match = view.rounds[0].matches[0];
    // The race counter caption: best-of-(2N-1) over the races run.
    expect(match.note).toBe('Best of 3 · 3 races');
    expect(match.slots).toHaveLength(2);
    const a = match.slots.find((s) => s.competitor === 'A')!;
    const b = match.slots.find((s) => s.competitor === 'B')!;
    // Series scores ride on the slots; the 2-win finalist is the winner, labels resolve.
    expect(a.score).toBe(2);
    expect(a.winner).toBe(true);
    expect(a.label).toBe('P-A');
    expect(b.score).toBe(1);
    expect(b.winner).toBe(false);
  });

  it('shows a "Series" note and zero scores before any race tally exists', () => {
    const view = buildBracketView(CHASE, [QUAL, CHASE], chaseHeats, (r) => r, undefined, 2, 2, 1);
    const match = view.rounds[0].matches[0];
    expect(match.note).toBe('Series');
    expect(match.slots.every((s) => s.score === 0 && !s.winner)).toBe(true);
  });

  it('renders a single placeholder match for a built-ahead chase final with no races yet', () => {
    const view = buildBracketView(CHASE, [QUAL, CHASE], [], (r) => r, undefined, 2, 2, 1);
    expect(view.rounds[0].matches).toHaveLength(1);
    const match = view.rounds[0].matches[0];
    expect(match.note).toBe('Series');
    expect(match.slots.every((s) => s.competitor === undefined)).toBe(true);
  });

  it('does NOT collapse a non-chase (single-elim) final — it renders one match per heat', () => {
    // The plain single-elim FINAL keeps its per-heat match (no note, no scores).
    const heats = [
      heat('sf-1', 'semis', ['A', 'D']),
      heat('sf-2', 'semis', ['B', 'C']),
      heat('f-1', 'final', ['A', 'B'])
    ];
    const view = buildBracketView(SEMIS, [QUAL, FINAL, SEMIS], heats, (r) => r, 'A', 4, 2);
    const finalMatch = view.rounds[1].matches[0];
    expect(finalMatch.note).toBeUndefined();
    expect(finalMatch.slots.every((s) => s.score === undefined)).toBe(true);
  });
});

describe('groupRoundsForDisplay — folds a bracket chain into one group', () => {
  it('emits standalone rounds and one bracket group (root + folded levels) in order', () => {
    const groups = groupRoundsForDisplay([QUAL, SEMIS, FINAL]);
    expect(groups.map((g) => (g.kind === 'bracket' ? `bracket:${g.root.id}` : g.round.id))).toEqual(
      ['q', 'bracket:semis']
    );
    const bracket = groups.find((g) => g.kind === 'bracket');
    expect(bracket?.kind === 'bracket' && bracket.levels.map((r) => r.id)).toEqual([
      'semis',
      'final'
    ]);
  });

  it('never emits a chained level on its own (the root carries it)', () => {
    const ids = groupRoundsForDisplay([QUAL, SEMIS, FINAL]).flatMap((g) =>
      g.kind === 'round' ? [g.round.id] : []
    );
    expect(ids).not.toContain('final');
    expect(ids).not.toContain('semis');
  });

  it('keeps a non-bracket round standalone, and falls an orphan level back to standalone', () => {
    // FINAL names source_round 'semis', but no root chain in this list reaches it → standalone.
    const groups = groupRoundsForDisplay([QUAL, FINAL]);
    expect(groups.map((g) => (g.kind === 'bracket' ? 'bracket' : g.round.id))).toEqual([
      'q',
      'final'
    ]);
  });
});

describe('splitBracketLabel — bracket name + level name', () => {
  it('splits "‹name› — ‹level›" on the last separator', () => {
    expect(splitBracketLabel('Pro — Quarterfinals')).toEqual({
      name: 'Pro',
      level: 'Quarterfinals'
    });
    // A bracket name that itself contains the separator survives (split on the LAST one).
    expect(splitBracketLabel('A — B — Final')).toEqual({ name: 'A — B', level: 'Final' });
    // No separator → no prefix; the whole label is the level.
    expect(splitBracketLabel('Final')).toEqual({ name: '', level: 'Final' });
  });
});

describe('nextLevelLabel — size-driven default', () => {
  it('names the next level by its heat count', () => {
    expect(nextLevelLabel('Bracket', 1, 1)).toBe('Final');
    expect(nextLevelLabel('Bracket', 2, 0)).toBe('Semifinals');
    expect(nextLevelLabel('Bracket', 4, 0)).toBe('Quarterfinals');
    expect(nextLevelLabel('Bracket', 8, 0)).toBe('Round of 16');
    expect(nextLevelLabel('Bracket', 16, 0)).toBe('Round of 32');
  });
});
