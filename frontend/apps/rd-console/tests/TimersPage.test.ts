import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { Timer } from '@gridfpv/types';
import TimersPage from '../src/screens/TimersPage.svelte';
import { makeTestSession } from './support.js';

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
    expect(createTimerImpl).toHaveBeenCalledWith(
      'http://d.local',
      { name: 'Fast', kind: { Mock: { laps: 3, lap_ms: 30000 } } },
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
    expect(updateTimerImpl).toHaveBeenCalledWith(
      'http://d.local',
      'rh-1',
      { name: 'Renamed', kind: { Rotorhazard: { url: 'http://rh.local:5000' } } },
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
