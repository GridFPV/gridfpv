import { describe, expect, it } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import type { RoundRobin } from '../src/roundRobin.js';
import RoundRobinView from '../src/RoundRobinView.svelte';

const VIEW: RoundRobin = {
  standings: [
    { competitor: 'p1', label: 'AceOne', position: 1, points: 20, bestLapMicros: 28_000_000 },
    { competitor: 'p2', label: 'Bolt', position: 2, points: 12, bestLapMicros: null }
  ],
  rotations: [
    {
      name: 'Rotation 1',
      heats: [
        {
          heat: 'rr-r1-h1',
          name: 'Round Robin Heat 1',
          entries: [
            { competitor: 'p1', label: 'AceOne', position: 1 },
            { competitor: 'p2', label: 'Bolt', position: 2 }
          ]
        }
      ]
    },
    {
      name: 'Rotation 2',
      heats: [
        {
          heat: 'rr-r2-h1',
          name: 'Round Robin Heat 2',
          entries: [
            { competitor: 'p2', label: 'Bolt' },
            { competitor: 'p1', label: 'AceOne' }
          ]
        }
      ]
    }
  ]
};

describe('RoundRobinView', () => {
  it('renders the standings with a Points column and resolved callsigns', () => {
    render(RoundRobinView, { view: VIEW });
    const table = screen.getByRole('table', { name: 'Round-robin standings' });
    expect(within(table).getByText('Points')).toBeInTheDocument();
    expect(within(table).getByText('AceOne')).toBeInTheDocument();
    expect(within(table).getByText('Bolt')).toBeInTheDocument();
    // The summed points show.
    expect(within(table).getByText('20')).toBeInTheDocument();
    expect(within(table).getByText('12')).toBeInTheDocument();
    // The Best-lap column shows the formatted time for a pilot who has one (28.000)…
    expect(within(table).getByText('28.000')).toBeInTheDocument();
    // …and the em dash only for the pilot whose best lap is genuinely absent (null).
    expect(within(table).getByText('—')).toBeInTheDocument();
  });

  it('shows only the standings — no per-rotation grid (heats live in the Heats area)', () => {
    const { container } = render(RoundRobinView, { view: VIEW });
    expect(screen.queryByText('Rotation 1')).not.toBeInTheDocument();
    expect(screen.queryByText('Round Robin Heat 1')).not.toBeInTheDocument();
    expect(container.querySelectorAll('.rr-heat').length).toBe(0);
  });

  it('shows an empty standings note when there are no rows yet', () => {
    render(RoundRobinView, { view: { standings: [], rotations: [] } });
    expect(
      screen.getByText(/Standings appear as the round's heats are scored/i)
    ).toBeInTheDocument();
  });
});
