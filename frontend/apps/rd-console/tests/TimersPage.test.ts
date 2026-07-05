import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { ChannelCatalogEntry, Timer } from '@gridfpv/types';
import TimersPage from '../src/screens/TimersPage.svelte';
import { makeTestSession } from './support.js';

const CATALOG: ChannelCatalogEntry[] = [
  { band: 'Raceband', channel: 'R1', mhz: 5658 },
  { band: 'Raceband', channel: 'R2', mhz: 5695 },
  { band: 'Fatshark', channel: 'F4', mhz: 5800 }
];

/** The page hosts the shared TimerManager; tests render it with a no-op `onhome`. */
const noop = () => {};

const MOCK: Timer = {
  id: 'mock',
  name: 'Mock',
  kind: { Mock: { laps: 3, lap_ms: 30000 } },
  status: 'Ready',
  channel_capability: 'Flexible',
  node_count: 8,
  available_channels: []
};
const RH: Timer = {
  id: 'rh-1',
  name: 'Track RH',
  kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
  status: 'Configured',
  channel_capability: 'Flexible',
  node_count: 8,
  available_channels: []
};

describe('TimersPage (app-level timer registry)', () => {
  it('lists the registry on mount, with the built-in Mock undeletable', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, RH]);
    const { session } = makeTestSession({ listTimersImpl });
    render(TimersPage, { session, onhome: noop });

    await screen.findByText('Mock');
    expect(screen.getByText('Track RH')).toBeInTheDocument();

    const list = screen.getByRole('list', { name: 'Configured timers' });
    const rows = within(list).getAllByRole('listitem');
    // Mock row: no Remove button (built-in). RH row: has one.
    expect(within(rows[0]).queryByRole('button', { name: 'Remove' })).toBeNull();
    expect(within(rows[1]).getByRole('button', { name: 'Remove' })).toBeInTheDocument();
  });

  it('adds a timer and reloads the list', async () => {
    const created: Timer = {
      id: 'fast-x',
      name: 'Fast',
      kind: { Mock: { laps: 5, lap_ms: 12000 } },
      status: 'Ready',
      channel_capability: 'Flexible',
      node_count: 8,
      available_channels: []
    };
    let calls = 0;
    const listTimersImpl = vi.fn(async () => (calls++ === 0 ? [MOCK] : [MOCK, created]));
    const createTimerImpl = vi.fn(async () => created);
    const { session } = makeTestSession({ listTimersImpl, createTimerImpl });
    render(TimersPage, { session, onhome: noop });

    await screen.findByText('Mock');
    await fireEvent.click(screen.getByRole('button', { name: '+ Add timer' }));

    const name = (await screen.findByLabelText('Timer name')) as HTMLInputElement;
    await fireEvent.input(name, { target: { value: 'Fast' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add timer' }));

    await waitFor(() => expect(createTimerImpl).toHaveBeenCalledTimes(1));
    // The add carries the channel config too (race redesign Slice 4b): the permissive default —
    // Flexible, 8 nodes, no channels picked — when the RD does not touch the channel fields.
    expect(createTimerImpl).toHaveBeenCalledWith(
      'http://d.local',
      {
        name: 'Fast',
        kind: { Mock: { laps: 3, lap_ms: 30000 } },
        channel_capability: 'Flexible',
        node_count: 8,
        available_channels: []
      },
      'tok'
    );
    // The list reloaded and shows the new timer.
    await screen.findByText('Fast');
  });

  it('edits a timer through the same dialog', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, RH]);
    const updateTimerImpl = vi.fn(async () => ({ ...RH, name: 'Renamed' }));
    const { session } = makeTestSession({ listTimersImpl, updateTimerImpl });
    render(TimersPage, { session, onhome: noop });

    await screen.findByText('Track RH');
    const list = screen.getByRole('list', { name: 'Configured timers' });
    const rhRow = within(list).getAllByRole('listitem')[1];
    await fireEvent.click(within(rhRow).getByRole('button', { name: 'Edit' }));

    const name = (await screen.findByLabelText('Timer name')) as HTMLInputElement;
    expect(name.value).toBe('Track RH');
    await fireEvent.input(name, { target: { value: 'Renamed' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => expect(updateTimerImpl).toHaveBeenCalledTimes(1));
    // The edit carries the timer's (unchanged) channel config too (race redesign Slice 4b).
    expect(updateTimerImpl).toHaveBeenCalledWith(
      'http://d.local',
      'rh-1',
      {
        name: 'Renamed',
        kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
        channel_capability: 'Flexible',
        node_count: 8,
        available_channels: []
      },
      'tok'
    );
  });

  it('configures a Flexible timer’s channels from the catalog + a custom MHz (Slice 4b)', async () => {
    const created: Timer = {
      id: 'flex-x',
      name: 'Flex',
      kind: { Mock: { laps: 3, lap_ms: 30000 } },
      status: 'Ready',
      channel_capability: 'Flexible',
      node_count: 6,
      available_channels: [5658, 5800, 5685]
    };
    let calls = 0;
    const listTimersImpl = vi.fn(async () => (calls++ === 0 ? [MOCK] : [MOCK, created]));
    const createTimerImpl = vi.fn(async () => created);
    const listChannelsImpl = vi.fn(async () => CATALOG);
    const { session } = makeTestSession({ listTimersImpl, createTimerImpl, listChannelsImpl });
    render(TimersPage, { session, onhome: noop });

    await screen.findByText('Mock');
    await fireEvent.click(screen.getByRole('button', { name: '+ Add timer' }));

    const name = (await screen.findByLabelText('Timer name')) as HTMLInputElement;
    await fireEvent.input(name, { target: { value: 'Flex' } });

    // Node count (the heat-size cap) is editable.
    const nodes = screen.getByLabelText('Node count') as HTMLInputElement;
    await fireEvent.input(nodes, { target: { value: '6' } });

    // The catalog picker renders grouped by band; pick two channels (Raceband R1, Fatshark F4).
    await waitFor(() => expect(screen.getByLabelText('Raceband R1, 5658 MHz')).toBeInTheDocument());
    await fireEvent.click(screen.getByLabelText('Raceband R1, 5658 MHz'));
    await fireEvent.click(screen.getByLabelText('Fatshark F4, 5800 MHz'));

    // Add a custom raw-MHz channel (Flexible only).
    const custom = screen.getByLabelText('Custom channel MHz') as HTMLInputElement;
    await fireEvent.input(custom, { target: { value: '5685' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add' }));

    await fireEvent.click(screen.getByRole('button', { name: 'Add timer' }));

    await waitFor(() => expect(createTimerImpl).toHaveBeenCalledTimes(1));
    expect(createTimerImpl).toHaveBeenCalledWith(
      'http://d.local',
      {
        name: 'Flex',
        kind: { Mock: { laps: 3, lap_ms: 30000 } },
        channel_capability: 'Flexible',
        node_count: 6,
        // Catalog channels in catalog order, then the custom MHz.
        available_channels: [5658, 5800, 5685]
      },
      'tok'
    );
  });

  it('limits a Fixed timer to its built-in allowed set (no custom) and builds Fixed (Slice 4b)', async () => {
    // A timer whose Fixed allowed set is just two Raceband channels — the picker offers only those,
    // and the custom-MHz row is hidden.
    const fixed: Timer = {
      id: 'fix-1',
      name: 'Fixed RH',
      kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
      status: 'Configured',
      channel_capability: { Fixed: { channels: [5658, 5695] } },
      node_count: 2,
      available_channels: [5658]
    };
    const listTimersImpl = vi.fn(async () => [MOCK, fixed]);
    const updateTimerImpl = vi.fn(async () => fixed);
    const listChannelsImpl = vi.fn(async () => CATALOG);
    const { session } = makeTestSession({ listTimersImpl, updateTimerImpl, listChannelsImpl });
    render(TimersPage, { session, onhome: noop });

    await screen.findByText('Fixed RH');
    const list = screen.getByRole('list', { name: 'Configured timers' });
    const row = within(list).getAllByRole('listitem')[1];
    await fireEvent.click(within(row).getByRole('button', { name: 'Edit' }));

    // Only the Fixed allowed set is offered; Fatshark F4 (not allowed) is absent.
    await waitFor(() => expect(screen.getByLabelText('Raceband R1, 5658 MHz')).toBeInTheDocument());
    expect(screen.getByLabelText('Raceband R2, 5695 MHz')).toBeInTheDocument();
    expect(screen.queryByLabelText('Fatshark F4, 5800 MHz')).toBeNull();
    // A Fixed timer offers no custom-MHz entry.
    expect(screen.queryByLabelText('Custom channel MHz')).toBeNull();

    // Make the second allowed channel available too, then save.
    await fireEvent.click(screen.getByLabelText('Raceband R2, 5695 MHz'));
    await fireEvent.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => expect(updateTimerImpl).toHaveBeenCalledTimes(1));
    expect(updateTimerImpl).toHaveBeenCalledWith(
      'http://d.local',
      'fix-1',
      {
        name: 'Fixed RH',
        kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
        channel_capability: { Fixed: { channels: [5658, 5695] } },
        node_count: 2,
        available_channels: [5658, 5695]
      },
      'tok'
    );
  });

  it('removes a non-built-in timer', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, RH]);
    const deleteTimerImpl = vi.fn(async () => undefined as unknown as void);
    const { session } = makeTestSession({ listTimersImpl, deleteTimerImpl });
    render(TimersPage, { session, onhome: noop });

    await screen.findByText('Track RH');
    const list = screen.getByRole('list', { name: 'Configured timers' });
    const rhRow = within(list).getAllByRole('listitem')[1];
    await fireEvent.click(within(rhRow).getByRole('button', { name: 'Remove' }));

    await waitFor(() => expect(deleteTimerImpl).toHaveBeenCalledTimes(1));
    expect(deleteTimerImpl).toHaveBeenCalledWith('http://d.local', 'rh-1', 'tok');
  });
});
