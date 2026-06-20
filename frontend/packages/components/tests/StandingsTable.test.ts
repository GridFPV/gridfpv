import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import StandingsTable from '../src/StandingsTable.svelte';
import { standings } from './fixtures.js';

describe('StandingsTable', () => {
  it('renders each ranking entry with its tie-aware position', () => {
    render(StandingsTable, { entries: standings, caption: 'Open — overall' });

    expect(screen.getByText('Open — overall')).toBeInTheDocument();
    expect(screen.getByText('ALICE')).toBeInTheDocument();

    // The two pilots tied at position 2 both show "2".
    const bobRow = screen.getByText('BOB').closest('tr');
    const bob2Row = screen.getByText('BOB2').closest('tr');
    expect(bobRow!).toHaveTextContent('2');
    expect(bob2Row!).toHaveTextContent('2');

    // The next distinct entry skips to 4 (competition ranking).
    const carmenRow = screen.getByText('CARMEN').closest('tr');
    expect(carmenRow!).toHaveTextContent('4');
  });
});
