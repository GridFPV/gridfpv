import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import Leaderboard from '../src/Leaderboard.svelte';
import { heatResult } from './fixtures.js';

describe('Leaderboard', () => {
  it('renders every competitor with position, laps, and metric', () => {
    render(Leaderboard, { result: heatResult, metricLabel: 'Best lap' });

    // All three pilots present.
    expect(screen.getByText('ALICE')).toBeInTheDocument();
    expect(screen.getByText('BOB')).toBeInTheDocument();
    expect(screen.getByText('CARMEN')).toBeInTheDocument();

    // The custom metric column heading.
    expect(screen.getByText('Best lap')).toBeInTheDocument();

    // ALICE's row: position 1, 3 laps, best lap 41.250.
    const aliceRow = screen.getByText('ALICE').closest('tr');
    expect(aliceRow).not.toBeNull();
    expect(aliceRow!).toHaveTextContent('1');
    expect(aliceRow!).toHaveTextContent('3');
    expect(aliceRow!).toHaveTextContent('41.250');

    // CARMEN has a null metric → em dash.
    const carmenRow = screen.getByText('CARMEN').closest('tr');
    expect(carmenRow!).toHaveTextContent('—');
  });

  it('marks the podium positions with medal data attributes', () => {
    const { container } = render(Leaderboard, { result: heatResult });
    expect(container.querySelector('tr[data-medal="gold"]')).not.toBeNull();
    expect(container.querySelector('tr[data-medal="silver"]')).not.toBeNull();
    expect(container.querySelector('tr[data-medal="bronze"]')).not.toBeNull();
  });
});
