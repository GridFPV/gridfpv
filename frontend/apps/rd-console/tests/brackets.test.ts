import { describe, expect, it } from 'vitest';
import type { HeatSummary, RoundDef } from '@gridfpv/types';
import {
  bracketChainRounds,
  buildBracketView,
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
    const view = buildBracketView(SEMIS, [QUAL, FINAL, SEMIS], heats, (r) => r);
    expect(view.rounds.map((r) => r.name)).toEqual(['Semifinals', 'Final']);
    // Two semis matches, one final.
    expect(view.rounds[0].matches.length).toBe(2);
    expect(view.rounds[1].matches.length).toBe(1);
  });

  it('infers a semifinal heat winner as the seat who reappears in the final', () => {
    const view = buildBracketView(SEMIS, [QUAL, FINAL, SEMIS], heats, (r) => r);
    const sf1 = view.rounds[0].matches[0];
    const winners = sf1.slots.filter((s) => s.winner).map((s) => s.competitor);
    expect(winners).toEqual(['A']); // A advanced to the final, D did not
  });

  it('marks the final winner from the supplied champion, and resolves labels', () => {
    const view = buildBracketView(SEMIS, [QUAL, FINAL, SEMIS], heats, (r) => `Pilot-${r}`, 'A');
    const final = view.rounds[1].matches[0];
    expect(final.slots.find((s) => s.competitor === 'A')?.winner).toBe(true);
    expect(final.slots.find((s) => s.competitor === 'B')?.winner).toBe(false);
    expect(final.slots[0].label).toBe('Pilot-A');
  });

  it('leaves the final unmarked when no champion is known yet', () => {
    const view = buildBracketView(SEMIS, [QUAL, FINAL, SEMIS], heats, (r) => r);
    const final = view.rounds[1].matches[0];
    expect(final.slots.every((s) => !s.winner)).toBe(true);
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
