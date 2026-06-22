import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { ChannelCatalogEntry, Class, EventMeta, Pilot, Timer } from '@gridfpv/types';
import EventClassesRoster from '../src/screens/EventClassesRoster.svelte';
import { makeTestSession } from './support.js';

const ACE: Pilot = { id: 'p1', callsign: 'Ace', vtx_types: [], attributes: {} };
const BEE: Pilot = { id: 'p2', callsign: 'Bee', vtx_types: [], attributes: {} };

const OPEN: Class = { id: 'open', name: 'Open Class', source: 'Custom' };
const SPEC: Class = { id: 'spec', name: 'Spec', source: 'Custom' };

// A primary timer carrying two available channels — the pool the per-pilot dropdowns draw from.
const MOCK: Timer = {
  id: 'mock',
  name: 'Mock',
  kind: { Mock: { laps: 3, lap_ms: 30000 } },
  status: 'Ready',
  channel_capability: 'Flexible',
  node_count: 8,
  available_channels: [5658, 5695]
} as unknown as Timer;

const CATALOG: ChannelCatalogEntry[] = [
  { band: 'Raceband', channel: 'R1', mhz: 5658 },
  { band: 'Raceband', channel: 'R2', mhz: 5695 }
];

/** Inert directory reads + the primary-timer registry + the channel catalog. */
function impls(extra: Record<string, unknown> = {}) {
  return {
    listPilotsImpl: vi.fn(async () => [ACE, BEE]),
    listClassesImpl: vi.fn(async () => [OPEN, SPEC]),
    listTimersImpl: vi.fn(async () => [MOCK]),
    listChannelsImpl: vi.fn(async () => CATALOG),
    ...extra
  };
}

/** A single-class event that rosters both pilots and selects its one timer. */
const SINGLE: EventMeta = {
  id: 'e1',
  name: 'Friday',
  created_at: 0,
  persistent: true,
  timers: ['mock'],
  primary_timer: 'mock',
  roster: ['p1', 'p2'],
  classes: ['open']
};

describe('EventClassesRoster — single-class auto-fill', () => {
  it('auto-places every roster pilot into the lone class (no per-class checkboxes)', async () => {
    const { session } = makeTestSession({ ...impls(), event: SINGLE });
    render(EventClassesRoster, { session });

    // The placement section lists each present pilot under the single class — no "Place … in"
    // checkboxes (auto-filled), just a channel dropdown per pilot.
    const grid = await screen.findByRole('group', { name: 'Placement for Open Class' });
    expect(within(grid).getByText('Ace')).toBeInTheDocument();
    expect(within(grid).getByText('Bee')).toBeInTheDocument();
    expect(within(grid).getByText('all present pilots (single class)')).toBeInTheDocument();
    expect(screen.queryByLabelText('Place Ace in Open Class')).not.toBeInTheDocument();
    // A channel selector exists per pilot.
    expect(screen.getByLabelText('Channel for Ace')).toBeInTheDocument();
    expect(screen.getByLabelText('Channel for Bee')).toBeInTheDocument();
  });

  it('keeps the lone class in sync as the roster grows (live)', async () => {
    const { session } = makeTestSession({
      ...impls(),
      event: { ...SINGLE, roster: ['p1'] }
    });
    render(EventClassesRoster, { session });

    const grid = await screen.findByRole('group', { name: 'Placement for Open Class' });
    expect(within(grid).getByText('Ace')).toBeInTheDocument();
    expect(within(grid).queryByText('Bee')).not.toBeInTheDocument();

    // The active-event roster grows (e.g. the sim reconciler adds a player) — auto-fill follows.
    session.currentEvent = { ...SINGLE, roster: ['p1', 'p2'] };
    await waitFor(() =>
      expect(
        within(screen.getByRole('group', { name: 'Placement for Open Class' })).getByText('Bee')
      ).toBeInTheDocument()
    );
  });

  it('shows per-class placement checkboxes when ≥2 classes are selected', async () => {
    const { session } = makeTestSession({
      ...impls(),
      event: { ...SINGLE, classes: ['open', 'spec'] }
    });
    render(EventClassesRoster, { session });

    // Two classes → the per-class checkbox grid returns (no auto-fill).
    expect(await screen.findByLabelText('Place Ace in Open Class')).toBeInTheDocument();
    expect(screen.getByLabelText('Place Bee in Spec')).toBeInTheDocument();
    expect(screen.queryByText('all present pilots (single class)')).not.toBeInTheDocument();
  });
});

describe('EventClassesRoster — per-pilot channel (sourced from the primary timer)', () => {
  it('populates the channel dropdown from the primary timer and saves a MemberSlot', async () => {
    const setClassMembershipImpl = vi.fn(async () => ({
      ...SINGLE,
      classes_membership: [{ class: 'open', pilots: [{ pilot: 'p1', channel: 5658 }] }]
    }));
    const { session } = makeTestSession({
      ...impls({ setClassMembershipImpl }),
      event: SINGLE
    });
    render(EventClassesRoster, { session });

    // The dropdown offers the primary timer's two available channels, labelled via the catalog.
    const sel = (await screen.findByLabelText('Channel for Ace')) as HTMLSelectElement;
    const options = within(sel)
      .getAllByRole('option')
      .map((o) => o.textContent?.trim());
    expect(options).toContain('Raceband R1 · 5658 MHz');
    expect(options).toContain('Raceband R2 · 5695 MHz');
    expect(options).toContain('No channel');

    // Assign Ace a channel and save the placement → the wire carries a MemberSlot with the channel.
    await fireEvent.change(sel, { target: { value: '5658' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save placement' }));

    await waitFor(() => expect(setClassMembershipImpl).toHaveBeenCalledTimes(1));
    // The wire carries the class id + MemberSlots — both auto-filled pilots, Ace with his channel,
    // Bee channel-less (a single-class event auto-fills every roster pilot into the lone class).
    expect(setClassMembershipImpl).toHaveBeenCalledWith(
      'http://d.local',
      'e1',
      'open',
      [{ pilot: 'p1', channel: 5658 }, { pilot: 'p2' }],
      'tok'
    );
  });

  it('nudges to configure a timer’s channels when the primary has none', async () => {
    const { session } = makeTestSession({
      ...impls({
        listTimersImpl: vi.fn(async () => [{ ...MOCK, available_channels: [] }])
      }),
      event: SINGLE
    });
    render(EventClassesRoster, { session });

    await screen.findByRole('group', { name: 'Placement for Open Class' });
    expect(screen.getByText(/No channels to assign yet/i)).toBeInTheDocument();
  });
});
