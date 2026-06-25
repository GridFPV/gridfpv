import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import type { Bracket } from '../src/bracket.js';
import BracketTree from '../src/BracketTree.svelte';
import { bracket } from './fixtures.js';

describe('BracketTree', () => {
  it('renders each round, its matches, and the seated competitors', () => {
    const { container } = render(BracketTree, { bracket });

    expect(screen.getByText('Semifinals')).toBeInTheDocument();
    expect(screen.getByText('Final')).toBeInTheDocument();

    // Competitors across the rounds.
    expect(screen.getByText('DANA')).toBeInTheDocument();
    expect(screen.getAllByText('ALICE').length).toBeGreaterThan(0);

    // Three matches total (2 semis + 1 final).
    expect(container.querySelectorAll('.match').length).toBe(3);
  });

  it('marks the advancing seat in each match', () => {
    const { container } = render(BracketTree, { bracket });
    const winners = container.querySelectorAll('.slot.winner');
    // One winner per match.
    expect(winners.length).toBe(3);
    winners.forEach((w) => expect(w).toHaveAttribute('aria-selected', 'true'));
  });

  it('renders a multi-level single-elim chain as ordered level columns (#217, D13)', () => {
    // A bracket authored one round per level: Quarterfinals → Semifinals → Final.
    const chain: Bracket = {
      rounds: [
        {
          name: 'Quarterfinals',
          matches: [
            { heat: 'qf-1', slots: [{ competitor: 'ALICE', winner: true }, { competitor: 'EVE' }] },
            { heat: 'qf-2', slots: [{ competitor: 'BOB', winner: true }, { competitor: 'FAY' }] },
            {
              heat: 'qf-3',
              slots: [{ competitor: 'CARMEN', winner: true }, { competitor: 'GIL' }]
            },
            { heat: 'qf-4', slots: [{ competitor: 'DANA', winner: true }, { competitor: 'HAL' }] }
          ]
        },
        {
          name: 'Semifinals',
          matches: [
            {
              heat: 'sf-1',
              slots: [{ competitor: 'ALICE', winner: true }, { competitor: 'DANA' }]
            },
            { heat: 'sf-2', slots: [{ competitor: 'BOB', winner: true }, { competitor: 'CARMEN' }] }
          ]
        },
        {
          name: 'Final',
          matches: [
            { heat: 'final', slots: [{ competitor: 'ALICE', winner: true }, { competitor: 'BOB' }] }
          ]
        }
      ]
    };

    const { container } = render(BracketTree, { bracket: chain });

    // The level columns render in chain order, left to right.
    const columns = [...container.querySelectorAll('.round-name')].map((n) => n.textContent);
    expect(columns).toEqual(['Quarterfinals', 'Semifinals', 'Final']);
    // 4 quarter + 2 semi + 1 final = 7 matches; one advancing seat marked per match.
    expect(container.querySelectorAll('.match').length).toBe(7);
    expect(container.querySelectorAll('.slot.winner').length).toBe(7);
  });
});
