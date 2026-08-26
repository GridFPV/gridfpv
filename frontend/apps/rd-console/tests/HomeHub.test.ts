import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { Class, EventMeta, Pilot, Timer } from '@gridfpv/types';
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
  {
    id: 'mock',
    name: 'Mock',
    kind: { Mock: { laps: 3, lap_ms: 30000 } },
    status: 'Ready',
    channel_capability: 'Flexible',
    node_count: 8,
    available_channels: [],
    manual_connect: false,
    calibration: [],
    disabled_nodes: []
  },
  {
    id: 'rh-1',
    name: 'Track RH',
    kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
    // A live RH that's up: Connected counts toward "connected" (not just Mock's Ready).
    status: 'Connected',
    channel_capability: 'Flexible',
    node_count: 8,
    available_channels: [],
    manual_connect: false,
    calibration: [],
    disabled_nodes: []
  },
  {
    id: 'rh-2',
    name: 'Spare RH',
    kind: { Rotorhazard: { url: 'http://rh2.local:5000' } },
    // Configured but not dialed in yet: does NOT count.
    status: 'Configured',
    channel_capability: 'Flexible',
    node_count: 8,
    available_channels: [],
    manual_connect: false,
    calibration: [],
    disabled_nodes: []
  }
];

describe('HomeHub (app-level landing, #118)', () => {
  function setup() {
    const listPilotsImpl = vi.fn(async () => PILOTS);
    const listClassesImpl = vi.fn(async () => CLASSES);
    const listEventsImpl = vi.fn(async () => EVENTS);
    const listTimersImpl = vi.fn(async () => TIMERS);
    // The hub is the no-event landing, so render with no event entered.
    const session = makeTestSession({
      noEnter: true,
      listTimersImpl,
      listPilotsImpl,
      listClassesImpl
    }).session;
    // listEvents isn't a constructor seam, so stub it directly.
    vi.spyOn(session, 'listEvents').mockImplementation(listEventsImpl);
    return { session };
  }

  it('renders the four cards with their summary counts', async () => {
    const { session } = setup();
    const onpilots = vi.fn();
    const onclasses = vi.fn();
    const onevents = vi.fn();
    const ontimers = vi.fn();
    render(HomeHub, { session, onpilots, onclasses, onevents, ontimers });

    // Four navigable cards.
    expect(screen.getByRole('heading', { name: 'Pilots' })).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Classes' })).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Events' })).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Timers' })).toBeInTheDocument();

    // Summaries settle per card (count + unit are separate spans): 2 pilots, 3 classes, 2 events,
    // 3 timers · 2 connected (Mock is Ready + the live RH is Connected; the Configured RH is excluded).
    const pilotsCard = screen.getByRole('heading', { name: 'Pilots' }).closest('button')!;
    const classesCard = screen.getByRole('heading', { name: 'Classes' }).closest('button')!;
    const eventsCard = screen.getByRole('heading', { name: 'Events' }).closest('button')!;
    const timersCard = screen.getByRole('heading', { name: 'Timers' }).closest('button')!;
    await waitFor(() => expect(within(pilotsCard).getByText('2')).toBeInTheDocument());
    expect(within(pilotsCard).getByText('pilots')).toBeInTheDocument();
    await waitFor(() => expect(within(classesCard).getByText('3')).toBeInTheDocument());
    expect(within(classesCard).getByText('classes')).toBeInTheDocument();
    await waitFor(() => expect(within(eventsCard).getByText('2')).toBeInTheDocument());
    expect(within(eventsCard).getByText('events')).toBeInTheDocument();
    await waitFor(() => expect(within(timersCard).getByText('timers')).toBeInTheDocument());
    expect(within(timersCard).getByText(/2 connected/)).toBeInTheDocument();
  });

  it('navigates to each page on card click', async () => {
    const { session } = setup();
    const onpilots = vi.fn();
    const onclasses = vi.fn();
    const onevents = vi.fn();
    const ontimers = vi.fn();
    render(HomeHub, { session, onpilots, onclasses, onevents, ontimers });

    await fireEvent.click(screen.getByRole('heading', { name: 'Pilots' }).closest('button')!);
    await fireEvent.click(screen.getByRole('heading', { name: 'Classes' }).closest('button')!);
    await fireEvent.click(screen.getByRole('heading', { name: 'Events' }).closest('button')!);
    await fireEvent.click(screen.getByRole('heading', { name: 'Timers' }).closest('button')!);

    expect(onpilots).toHaveBeenCalledTimes(1);
    expect(onclasses).toHaveBeenCalledTimes(1);
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
    vi.spyOn(session, 'listClasses').mockResolvedValue(CLASSES);
    render(HomeHub, {
      session,
      onpilots: vi.fn(),
      onclasses: vi.fn(),
      onevents: vi.fn(),
      ontimers: vi.fn()
    });

    const timersCard = screen.getByRole('heading', { name: 'Timers' }).closest('button')!;
    await waitFor(() => expect(within(timersCard).getByText('—')).toBeInTheDocument());
  });
});
