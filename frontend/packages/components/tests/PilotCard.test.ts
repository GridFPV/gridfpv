import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import PilotCard from '../src/PilotCard.svelte';
import { pilotProgress } from './fixtures.js';

describe('PilotCard', () => {
  it('shows identity, lap count, and last lap from PilotProgress', () => {
    render(PilotCard, { progress: pilotProgress, position: 1 });

    expect(screen.getByText('ALICE')).toBeInTheDocument();
    expect(screen.getByText('2')).toBeInTheDocument(); // laps_completed
    expect(screen.getByText('41.250')).toBeInTheDocument(); // last lap (µs → s)
    expect(screen.getByLabelText('Position 1')).toBeInTheDocument();
  });

  it('prefers an explicit display name over the source-local ref', () => {
    render(PilotCard, { progress: pilotProgress, name: 'Alice A.' });
    expect(screen.getByText('Alice A.')).toBeInTheDocument();
    expect(screen.queryByText('ALICE')).toBeNull();
  });

  it('renders an em dash when no lap has been completed', () => {
    render(PilotCard, {
      progress: { competitor: 'NEW', laps_completed: 0, last_lap_micros: undefined }
    });
    expect(screen.getByText('—')).toBeInTheDocument();
  });
});
