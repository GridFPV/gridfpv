import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { waitFor } from '@testing-library/dom';
import type { Pilot } from '@gridfpv/types';
import PilotsPage from '../src/screens/PilotsPage.svelte';
import { makeTestSession } from './support.js';

const noop = () => {};

const PILOTS: Pilot[] = [
  { id: 'p1', callsign: 'Ace', name: 'Alice', country: 'US', vtx_types: [] },
  { id: 'p2', callsign: 'Bee', vtx_types: [] }
];

describe('PilotsPage (#74) — hosts the shared PilotManager', () => {
  it('lists the directory and exposes Add / Edit / Remove controls', async () => {
    const listPilotsImpl = vi.fn(async () => PILOTS);
    const { session } = makeTestSession({ noEnter: true, listPilotsImpl });
    render(PilotsPage, { session, onhome: noop });

    await screen.findByText('Ace');
    expect(screen.getByText('Bee')).toBeInTheDocument();
    // The page's primary action opens the add form.
    expect(screen.getByRole('button', { name: '+ Add pilot' })).toBeInTheDocument();
    // Per-row management controls exist.
    expect(screen.getAllByRole('button', { name: 'Edit' })).toHaveLength(2);
    expect(screen.getAllByRole('button', { name: 'Remove' })).toHaveLength(2);
  });

  it('surfaces a read error with a retry', async () => {
    const listPilotsImpl = vi.fn(async () => {
      throw new Error('GET /pilots failed: HTTP 503');
    });
    const { session } = makeTestSession({ noEnter: true, listPilotsImpl });
    render(PilotsPage, { session, onhome: noop });

    await waitFor(() => expect(screen.getByText(/HTTP 503/)).toBeInTheDocument());
    expect(screen.getByRole('button', { name: 'Try again' })).toBeInTheDocument();
  });
});
