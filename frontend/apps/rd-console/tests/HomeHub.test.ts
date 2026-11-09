import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { Class, EventMeta, Pilot } from '@gridfpv/types';
import HomeHub from '../src/screens/HomeHub.svelte';
import { makeTestSession } from './support.js';

const PILOTS: Pilot[] = [
  { id: 'p1', callsign: 'Ace', vtx_types: [] },
  { id: 'p2', callsign: 'Bee', vtx_types: [] }
];
const CLASSES: Class[] = [
  { id: 'c1', name: 'Open', source: 'MultiGP' },
  { id: 'c2', name: 'Spec', source: 'MultiGP' },
  { id: 'c3', name: 'House', source: 'Custom' }
];
const EVENTS: EventMeta[] = [
  {
    id: 'practice-ab12',
    name: 'Practice',
    created_at: 0,
    persistent: true
  },
  {
    id: 'e1',
    name: 'Friday',
    created_at: 1,
    persistent: true
  }
];
describe('HomeHub (app-level landing, #118)', () => {
  function setup() {
    // The hub is the no-event landing, so render with no event open.
    const session = makeTestSession({
      noEnter: true,
      listPilotsImpl: vi.fn(async () => PILOTS),
      listClassesImpl: vi.fn(async () => CLASSES),
      listEventsImpl: vi.fn(async () => EVENTS)
    }).session;
    return { session };
  }

  it('renders the three cards with their summary counts', async () => {
    const { session } = setup();
    render(HomeHub, { session, onpilots: vi.fn(), onclasses: vi.fn(), onevents: vi.fn() });

    // Summaries settle per card (count + unit are separate spans): 2 pilots, 3 classes, 2 events.
    const pilotsCard = screen.getByRole('heading', { name: 'Pilots' }).closest('button')!;
    const classesCard = screen.getByRole('heading', { name: 'Classes' }).closest('button')!;
    const eventsCard = screen.getByRole('heading', { name: 'Events' }).closest('button')!;
    await waitFor(() => expect(within(pilotsCard).getByText('2')).toBeInTheDocument());
    expect(within(pilotsCard).getByText('pilots')).toBeInTheDocument();
    await waitFor(() => expect(within(classesCard).getByText('3')).toBeInTheDocument());
    expect(within(classesCard).getByText('classes')).toBeInTheDocument();
    await waitFor(() => expect(within(eventsCard).getByText('2')).toBeInTheDocument());
    expect(within(eventsCard).getByText('events')).toBeInTheDocument();
  });

  it('navigates to each page on card click', async () => {
    const { session } = setup();
    const onpilots = vi.fn();
    const onclasses = vi.fn();
    const onevents = vi.fn();
    render(HomeHub, { session, onpilots, onclasses, onevents });

    await fireEvent.click(screen.getByRole('heading', { name: 'Pilots' }).closest('button')!);
    await fireEvent.click(screen.getByRole('heading', { name: 'Classes' }).closest('button')!);
    await fireEvent.click(screen.getByRole('heading', { name: 'Events' }).closest('button')!);

    expect(onpilots).toHaveBeenCalledTimes(1);
    expect(onclasses).toHaveBeenCalledTimes(1);
    expect(onevents).toHaveBeenCalledTimes(1);
  });

  it('shows a dash when a summary read fails', async () => {
    const session = makeTestSession({
      noEnter: true,
      listPilotsImpl: vi.fn(async () => {
        throw new Error('unreachable');
      }),
      listClassesImpl: vi.fn(async () => CLASSES),
      listEventsImpl: vi.fn(async () => EVENTS)
    }).session;
    render(HomeHub, { session, onpilots: vi.fn(), onclasses: vi.fn(), onevents: vi.fn() });

    const pilotsCard = screen.getByRole('heading', { name: 'Pilots' }).closest('button')!;
    await waitFor(() => expect(within(pilotsCard).getByText('—')).toBeInTheDocument());
  });
});
