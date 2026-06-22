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

    // There is no Save button — placement auto-saves. Assign Ace a channel; the debounced
    // `setClassMembership` lands on its own carrying a MemberSlot with the channel.
    expect(screen.queryByRole('button', { name: 'Save placement' })).toBeNull();
    await fireEvent.change(sel, { target: { value: '5658' } });

    // The latest wire call carries the class id + MemberSlots — both auto-filled pilots, Ace with
    // his channel, Bee channel-less (a single-class event auto-fills every roster pilot).
    await waitFor(() =>
      expect(setClassMembershipImpl).toHaveBeenCalledWith(
        'http://d.local',
        'e1',
        'open',
        [{ pilot: 'p1', channel: 5658 }, { pilot: 'p2' }],
        'tok'
      )
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

describe('EventClassesRoster — select all / unselect all pilots', () => {
  // An event with neither pilot rostered yet, so Select all has work to do.
  const EMPTY_ROSTER: EventMeta = { ...SINGLE, roster: [] };

  it('select-all ticks every directory pilot, unselect-all clears the roster selection', async () => {
    const { session } = makeTestSession({ ...impls(), event: EMPTY_ROSTER });
    render(EventClassesRoster, { session });

    // Both roster checkboxes start unchecked (nobody present yet).
    const ace = (await screen.findByLabelText('Roster Ace')) as HTMLInputElement;
    const bee = screen.getByLabelText('Roster Bee') as HTMLInputElement;
    expect(ace.checked).toBe(false);
    expect(bee.checked).toBe(false);

    // Select all → both become checked.
    await fireEvent.click(screen.getByRole('button', { name: 'Select all' }));
    await waitFor(() => expect(ace.checked).toBe(true));
    expect(bee.checked).toBe(true);

    // Unselect all → both clear again.
    await fireEvent.click(screen.getByRole('button', { name: 'Unselect all' }));
    await waitFor(() => expect(ace.checked).toBe(false));
    expect(bee.checked).toBe(false);
  });
});

describe('EventClassesRoster — auto-save (no Save buttons)', () => {
  // A two-class event so toggling a class is a non-empty selection both ways.
  const TWO_CLASS: EventMeta = { ...SINGLE, classes: ['open'] };

  it('toggling a class auto-saves the full selection (debounced), no Save click', async () => {
    const setEventClassesImpl = vi.fn(async () => ({ ...TWO_CLASS, classes: ['open', 'spec'] }));
    const { session } = makeTestSession({
      ...impls({ setEventClassesImpl }),
      event: TWO_CLASS
    });
    render(EventClassesRoster, { session });

    // No Save buttons for the selection boxes.
    const spec = (await screen.findByLabelText('Select Spec')) as HTMLInputElement;
    expect(screen.queryByRole('button', { name: 'Save classes' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Save roster' })).toBeNull();

    // Tick Spec → optimistic flip, then a debounced wholesale save of the full class selection.
    await fireEvent.click(spec);
    expect(spec.checked).toBe(true);
    await waitFor(() =>
      expect(setEventClassesImpl).toHaveBeenCalledWith(
        'http://d.local',
        'e1',
        ['open', 'spec'],
        'tok'
      )
    );
  });

  it('reverts a roster toggle optimistically when the save fails', async () => {
    const EMPTY_ROSTER: EventMeta = { ...SINGLE, roster: [] };
    const setEventRosterImpl = vi.fn(async () => {
      throw new Error('boom');
    });
    const { session } = makeTestSession({
      ...impls({ setEventRosterImpl }),
      event: EMPTY_ROSTER
    });
    render(EventClassesRoster, { session });

    const ace = (await screen.findByLabelText('Roster Ace')) as HTMLInputElement;
    await fireEvent.click(ace); // optimistically present
    expect(ace.checked).toBe(true);

    // The failed save reverts the roster to the last-saved (empty) state.
    await waitFor(() => expect(setEventRosterImpl).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect((screen.getByLabelText('Roster Ace') as HTMLInputElement).checked).toBe(false)
    );
  });
});

describe('EventClassesRoster — auto-assign channels', () => {
  it('fills every placed pilot from the channel pool (round-robin) and saves each class', async () => {
    const setClassMembershipImpl = vi.fn(async () => SINGLE);
    const { session } = makeTestSession({
      ...impls({ setClassMembershipImpl }),
      event: SINGLE
    });
    render(EventClassesRoster, { session });

    // Both pilots auto-placed in the single class, channels unset to start.
    const aceSel = (await screen.findByLabelText('Channel for Ace')) as HTMLSelectElement;
    const beeSel = screen.getByLabelText('Channel for Bee') as HTMLSelectElement;
    expect(aceSel.value).toBe('');
    expect(beeSel.value).toBe('');

    // Auto-assign → Ace gets pool[0] (5658), Bee gets pool[1] (5695); each select reflects it.
    await fireEvent.click(screen.getByRole('button', { name: 'Auto-assign channels' }));
    await waitFor(() => expect(aceSel.value).toBe('5658'));
    expect(beeSel.value).toBe('5695');

    // Membership saved for the lone class, both MemberSlots carrying their assigned channel.
    await waitFor(() =>
      expect(setClassMembershipImpl).toHaveBeenLastCalledWith(
        'http://d.local',
        'e1',
        'open',
        [
          { pilot: 'p1', channel: 5658 },
          { pilot: 'p2', channel: 5695 }
        ],
        'tok'
      )
    );
  });

  it('disables auto-assign when the primary timer has no channels', async () => {
    const { session } = makeTestSession({
      ...impls({
        listTimersImpl: vi.fn(async () => [{ ...MOCK, available_channels: [] }])
      }),
      event: SINGLE
    });
    render(EventClassesRoster, { session });

    const btn = (await screen.findByRole('button', {
      name: 'Auto-assign channels'
    })) as HTMLButtonElement;
    expect(btn.disabled).toBe(true);
  });
});
