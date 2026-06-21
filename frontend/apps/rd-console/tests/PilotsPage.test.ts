import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { waitFor } from '@testing-library/dom';
import type { Pilot } from '@gridfpv/types';
import PilotsPage from '../src/screens/PilotsPage.svelte';
import { makeTestSession } from './support.js';

const noop = () => {};

const PILOTS: Pilot[] = [
  { id: 'p1', callsign: 'Ace', name: 'Alice', attributes: {} },
  { id: 'p2', callsign: 'Bee', attributes: {} }
];

describe('PilotsPage (placeholder for #74)', () => {
  it('lists the directory callsigns read-only and notes the registration UI is coming', async () => {
    const listPilotsImpl = vi.fn(async () => PILOTS);
    const { session } = makeTestSession({ noEnter: true, listPilotsImpl });
    render(PilotsPage, { session, onhome: noop });

    // The read-only callsigns appear.
    await screen.findByText('Ace');
    expect(screen.getByText('Bee')).toBeInTheDocument();
    // The "coming" note seam for the next slice.
    expect(screen.getByText(/registration UI/i)).toBeInTheDocument();
    // No management controls in this slice.
    expect(screen.queryByRole('button', { name: /Add pilot/i })).toBeNull();
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
