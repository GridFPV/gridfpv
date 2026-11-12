import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { waitFor } from '@testing-library/dom';
import type { Pilot } from '@gridfpv/types';
import PilotsPage from '../src/screens/PilotsPage.svelte';
import { makeTestSession } from './support.js';

const PILOTS: Pilot[] = [
  { id: 'p1', callsign: 'Ace', name: 'Alice', country: 'US', vtx_types: [] },
  { id: 'p2', callsign: 'Bee', vtx_types: [] }
];

describe('PilotsPage (#74) — hosts the shared PilotManager', () => {
  it('lists the directory and exposes Add / Edit / Remove controls', async () => {
    const listPilotsImpl = vi.fn(async () => PILOTS);
    const { session } = makeTestSession({ noEnter: true, listPilotsImpl });
    render(PilotsPage, { session });

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
    render(PilotsPage, { session });

    await waitFor(() => expect(screen.getByText(/HTTP 503/)).toBeInTheDocument());
    expect(screen.getByRole('button', { name: 'Try again' })).toBeInTheDocument();
  });

  it('sits beside Classes in one tab row, and is the current tab', () => {
    const { session } = makeTestSession({ noEnter: true });
    render(PilotsPage, { session });
    const tabs = within(
      screen.getByRole('navigation', { name: 'Pilots and classes' })
    ).getAllByRole('link');
    expect(
      tabs.map((tab) => [
        tab.textContent,
        tab.getAttribute('href'),
        tab.getAttribute('aria-current')
      ])
    ).toEqual([
      ['Pilots', '#/pilots', 'page'],
      ['Classes', '#/classes', null]
    ]);
    expect(screen.getByRole('link', { name: 'Help for Pilots' })).toHaveAttribute(
      'href',
      '#/help/pilots'
    );
  });
});
