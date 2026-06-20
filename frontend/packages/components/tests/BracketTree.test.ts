import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
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
});
