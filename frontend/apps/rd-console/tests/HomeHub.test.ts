import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { EventMeta, Pilot, Timer } from '@gridfpv/types';
import HomeHub from '../src/screens/HomeHub.svelte';
import { makeTestSession } from './support.js';

const PILOTS: Pilot[] = [
  { id: 'p1', callsign: 'Ace', vtx_types: [], attributes: {} },
  { id: 'p2', callsign: 'Bee', vtx_types: [], attributes: {} }
];
const EVENTS: EventMeta[] = [
  {
    id: 'practice',
    name: 'Practice',
    created_at: 0,
    persistent: false,
    timers: ['mock'],
    roster: [],
    classes: []
  },
  {
    id: 'e1',
    name: 'Friday',
    created_at: 1,
    persistent: true,
    timers: ['mock'],
    roster: [],
    classes: []
  }
];
const TIMERS: Timer[] = [
  { id: 'mock', name: 'Mock', kind: { Mock: { laps: 3, lap_ms: 30000 } }, status: 'Ready' },
  {
    id: 'rh-1',
    name: 'Track RH',
    kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
    status: 'Configured'
  }
];

describe('HomeHub (app-level landing, #118)', () => {
  function setup() {
    const listPilotsImpl = vi.fn(async () => PILOTS);
    const listEventsImpl = vi.fn(async () => EVENTS);
    const listTimersImpl = vi.fn(async () => TIMERS);
    // The hub is the no-event landing, so render with no event entered.
    const session = makeTestSession({ noEnter: true, listTimersImpl, listPilotsImpl }).session;
    // listEvents isn't a constructor seam, so stub it directly.
    vi.spyOn(session, 'listEvents').mockImplementation(listEventsImpl);
    return { session };
  }

  it('renders the three cards with their summary counts', async () => {
    const { session } = setup();
    const onpilots = vi.fn();
    const onevents = vi.fn();
    const ontimers = vi.fn();
    render(HomeHub, { session, onpilots, onevents, ontimers });

    // Three navigable cards.
    expect(screen.getByRole('heading', { name: 'Pilots' })).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Events' })).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Timers' })).toBeInTheDocument();

    // Summaries settle per card (count + unit are separate spans): 2 pilots, 2 events,
    // 2 timers · 1 connected (only Mock is Ready).
    const pilotsCard = screen.getByRole('heading', { name: 'Pilots' }).closest('button')!;
    const eventsCard = screen.getByRole('heading', { name: 'Events' }).closest('button')!;
    const timersCard = screen.getByRole('heading', { name: 'Timers' }).closest('button')!;
    await waitFor(() => expect(within(pilotsCard).getByText('2')).toBeInTheDocument());
    expect(within(pilotsCard).getByText('pilots')).toBeInTheDocument();
    await waitFor(() => expect(within(eventsCard).getByText('2')).toBeInTheDocument());
    expect(within(eventsCard).getByText('events')).toBeInTheDocument();
    await waitFor(() => expect(within(timersCard).getByText('timers')).toBeInTheDocument());
    expect(within(timersCard).getByText(/1 connected/)).toBeInTheDocument();
  });

  it('navigates to each page on card click', async () => {
    const { session } = setup();
    const onpilots = vi.fn();
    const onevents = vi.fn();
    const ontimers = vi.fn();
    render(HomeHub, { session, onpilots, onevents, ontimers });

    await fireEvent.click(screen.getByRole('heading', { name: 'Pilots' }).closest('button')!);
    await fireEvent.click(screen.getByRole('heading', { name: 'Events' }).closest('button')!);
    await fireEvent.click(screen.getByRole('heading', { name: 'Timers' }).closest('button')!);

    expect(onpilots).toHaveBeenCalledTimes(1);
    expect(onevents).toHaveBeenCalledTimes(1);
    expect(ontimers).toHaveBeenCalledTimes(1);
  });

  it('shows a dash when a summary read fails', async () => {
    const listTimersImpl = vi.fn(async () => {
      throw new Error('unreachable');
    });
    const session = makeTestSession({ noEnter: true, listTimersImpl }).session;
    vi.spyOn(session, 'listEvents').mockResolvedValue(EVENTS);
    vi.spyOn(session, 'listPilots').mockResolvedValue(PILOTS);
    render(HomeHub, { session, onpilots: vi.fn(), onevents: vi.fn(), ontimers: vi.fn() });

    const timersCard = screen.getByRole('heading', { name: 'Timers' }).closest('button')!;
    await waitFor(() => expect(within(timersCard).getByText('—')).toBeInTheDocument());
  });
});
