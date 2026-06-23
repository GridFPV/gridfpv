import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { waitFor } from '@testing-library/dom';
import type { Class, ClassStandings, EventMeta, Pilot } from '@gridfpv/types';
import Results from '../src/screens/Results.svelte';
import { heatResult, standings, eventOutcome } from './fixtures.js';
import { makeTestSession } from './support.js';

const OPEN: Class = { id: 'c1', name: 'Open', source: 'MultiGP' };
const ACE: Pilot = { id: 'p1', callsign: 'AceOne', vtx_types: [] };
const BOLT: Pilot = { id: 'p2', callsign: 'Bolt', vtx_types: [] };

const EVENT: EventMeta = {
  id: 'e1',
  name: 'Friday',
  created_at: 0,
  persistent: true,
  timers: ['mock'],
  roster: ['p1', 'p2'],
  classes: ['c1']
};

const STANDINGS: ClassStandings = {
  class: 'c1',
  standings: [
    {
      competitor: 'p1',
      position: 1,
      points: 6,
      best_lap_micros: 41_250_000, // → "41.250"
      total_laps: 9,
      rounds_entered: 2
    },
    {
      competitor: 'p2',
      position: 2,
      points: 3,
      best_lap_micros: null, // → "—"
      total_laps: 4,
      rounds_entered: 2
    }
  ]
};

describe('Results — per-class standings (race redesign Slice 5/6b)', () => {
  it('renders a class standings table with callsign, points, best lap (µs→s.mmm), laps, and rounds', async () => {
    const { session } = makeTestSession({
      event: EVENT,
      listClassesImpl: vi.fn(async () => [OPEN]),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT]),
      classStandingsImpl: vi.fn(async () => STANDINGS)
    });
    render(Results, { session });

    const table = (await screen.findByLabelText(/Open standings/i)) as HTMLElement;
    // Pilot 1's row: resolved callsign, points, best lap formatted µs → "41.250", laps, rounds.
    const aceCell = within(table).getByText('AceOne');
    const aceRow = aceCell.closest('tr') as HTMLElement;
    expect(within(aceRow).getByText('6')).toBeInTheDocument();
    expect(within(aceRow).getByText('41.250')).toBeInTheDocument();
    expect(within(aceRow).getByText('9')).toBeInTheDocument();
    // Pilot 2 has no best lap → renders a dash.
    const boltRow = within(table).getByText('Bolt').closest('tr') as HTMLElement;
    expect(within(boltRow).getByText('—')).toBeInTheDocument();
  });

  it('shows an empty state for a class with no scored rounds yet', async () => {
    const { session } = makeTestSession({
      event: EVENT,
      listClassesImpl: vi.fn(async () => [OPEN]),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT]),
      classStandingsImpl: vi.fn(async () => ({ class: 'c1', standings: [] }))
    });
    render(Results, { session });
    await waitFor(() => expect(screen.getByText(/Nothing scored/i)).toBeInTheDocument());
  });

  it('shows a no-classes message when the event selects none', async () => {
    const { session } = makeTestSession({
      event: { ...EVENT, classes: [] },
      listClassesImpl: vi.fn(async () => [OPEN]),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT])
    });
    render(Results, { session });
    expect(await screen.findByText(/selects no classes yet/i)).toBeInTheDocument();
  });
});

describe('Results — event-level projections (kept from #56)', () => {
  it('renders a heat result, ranking, and bracket from typed fixtures', () => {
    render(Results, { heatResult, standings, outcome: eventOutcome });
    expect(screen.getAllByText('ALICE').length).toBeGreaterThan(0);
    expect(screen.getByText('Final')).toBeInTheDocument();
    expect(screen.getByText('Semifinals')).toBeInTheDocument();
  });

  it('shows an empty state when nothing is scored yet and no session', () => {
    render(Results, {});
    expect(screen.getByText(/No results yet/i)).toBeInTheDocument();
  });

  it('offers an export action', () => {
    render(Results, { heatResult });
    expect(screen.getByRole('button', { name: 'Export JSON' })).toBeInTheDocument();
  });
});
