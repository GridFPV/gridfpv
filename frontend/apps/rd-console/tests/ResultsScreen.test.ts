import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import Results from '../src/screens/Results.svelte';
import { heatResult, standings, eventOutcome } from './fixtures.js';

describe('Results', () => {
  it('renders a heat result, standings, and bracket from typed fixtures', () => {
    render(Results, { heatResult, standings, outcome: eventOutcome });
    // Leaderboard + standings list competitors.
    expect(screen.getAllByText('ALICE').length).toBeGreaterThan(0);
    // Bracket round names from the derivation.
    expect(screen.getByText('Final')).toBeInTheDocument();
    expect(screen.getByText('Semifinals')).toBeInTheDocument();
  });

  it('shows an empty state when nothing is scored yet', () => {
    render(Results, {});
    expect(screen.getByText(/No results yet/i)).toBeInTheDocument();
  });

  it('offers an export action', () => {
    render(Results, { heatResult });
    expect(screen.getByRole('button', { name: 'Export JSON' })).toBeInTheDocument();
  });
});
