import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import HeatSheet from '../src/HeatSheet.svelte';
import { liveState } from './fixtures.js';

describe('HeatSheet', () => {
  it('renders the heat id, phase, and each pilot with live laps', () => {
    render(HeatSheet, { state: liveState });

    expect(screen.getByText('heat-1')).toBeInTheDocument();
    expect(screen.getByText('Running')).toBeInTheDocument();

    expect(screen.getByText('ALICE')).toBeInTheDocument();
    expect(screen.getByText('BOB')).toBeInTheDocument();
    expect(screen.getByText('CARMEN')).toBeInTheDocument();

    // ALICE leads the running order with 3 laps.
    const aliceRow = screen.getByText('ALICE').closest('li');
    expect(aliceRow!).toHaveTextContent('3 laps');

    // CARMEN has no completed last lap → em dash.
    const carmenRow = screen.getByText('CARMEN').closest('li');
    expect(carmenRow!).toHaveTextContent('—');
  });

  it('maps competitor refs to display names when provided', () => {
    render(HeatSheet, { state: liveState, names: { ALICE: 'Alice A.' } });
    expect(screen.getByText('Alice A.')).toBeInTheDocument();
  });

  it('uses the friendly heatName for the heading when provided (id otherwise)', () => {
    render(HeatSheet, { state: liveState, heatName: 'Qualifying Heat 1' });
    expect(screen.getByRole('heading')).toHaveTextContent('Qualifying Heat 1');
    // The raw id is no longer the heading.
    expect(screen.queryByRole('heading', { name: 'heat-1' })).not.toBeInTheDocument();
  });

  it('falls back to the raw heat id when no heatName is given (backward-compatible)', () => {
    render(HeatSheet, { state: liveState });
    expect(screen.getByRole('heading')).toHaveTextContent('heat-1');
  });
});
