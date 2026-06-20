import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { EventMeta, Timer } from '@gridfpv/types';
import EventTimers from '../src/screens/EventTimers.svelte';
import { makeTestSession } from './support.js';

const MOCK: Timer = {
  id: 'mock',
  name: 'Mock',
  kind: { Mock: { laps: 3, lap_ms: 30000 } },
  status: 'Ready'
};
const RH: Timer = {
  id: 'rh-1',
  name: 'Track RH',
  kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
  status: 'Disconnected'
};

/** A created event selecting only the Mock timer (so its checkbox seeds checked). */
const EVENT: EventMeta = {
  id: 'e1',
  name: 'Friday',
  created_at: 0,
  persistent: true,
  timers: ['mock']
};

describe('EventTimers (in-event CRUD + selection)', () => {
  it('seeds the checkboxes from the event selection', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, RH]);
    const { session } = makeTestSession({ listTimersImpl, event: EVENT });
    render(EventTimers, { session });

    const mockBox = (await screen.findByLabelText('Use Mock')) as HTMLInputElement;
    const rhBox = screen.getByLabelText('Use Track RH') as HTMLInputElement;
    expect(mockBox.checked).toBe(true);
    expect(rhBox.checked).toBe(false);
    // No change yet → Save disabled.
    expect(
      (screen.getByRole('button', { name: 'Save selection' }) as HTMLButtonElement).disabled
    ).toBe(true);
  });

  it('saves the working selection in the registry-listed order on change', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, RH]);
    const setEventTimersImpl = vi.fn(async () => ({ ...EVENT, timers: ['mock', 'rh-1'] }));
    const { session } = makeTestSession({ listTimersImpl, setEventTimersImpl, event: EVENT });
    render(EventTimers, { session });

    const rhBox = (await screen.findByLabelText('Use Track RH')) as HTMLInputElement;
    await fireEvent.click(rhBox);

    const save = screen.getByRole('button', { name: 'Save selection' }) as HTMLButtonElement;
    expect(save.disabled).toBe(false);
    await fireEvent.click(save);

    await waitFor(() => expect(setEventTimersImpl).toHaveBeenCalledTimes(1));
    expect(setEventTimersImpl).toHaveBeenCalledWith(
      'http://d.local',
      'e1',
      ['mock', 'rh-1'],
      'tok'
    );
  });

  it('blocks saving an empty selection', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, RH]);
    const setEventTimersImpl = vi.fn(async () => EVENT);
    const { session } = makeTestSession({ listTimersImpl, setEventTimersImpl, event: EVENT });
    render(EventTimers, { session });

    const mockBox = (await screen.findByLabelText('Use Mock')) as HTMLInputElement;
    await fireEvent.click(mockBox); // unselect the only selected → empty
    await fireEvent.click(screen.getByRole('button', { name: 'Save selection' }));

    // The empty selection is rejected client-side; no protocol call goes out.
    expect(setEventTimersImpl).not.toHaveBeenCalled();
  });

  it('adds a timer from inside the event (shared management)', async () => {
    const created: Timer = {
      id: 'fast-x',
      name: 'Fast',
      kind: { Mock: { laps: 5, lap_ms: 12000 } },
      status: 'Ready'
    };
    let calls = 0;
    const listTimersImpl = vi.fn(async () => (calls++ === 0 ? [MOCK] : [MOCK, created]));
    const createTimerImpl = vi.fn(async () => created);
    const { session } = makeTestSession({ listTimersImpl, createTimerImpl, event: EVENT });
    render(EventTimers, { session });

    await screen.findByLabelText('Use Mock');
    await fireEvent.click(screen.getByRole('button', { name: '+ Add timer' }));

    const name = (await screen.findByLabelText('Timer name')) as HTMLInputElement;
    await fireEvent.input(name, { target: { value: 'Fast' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add timer' }));

    await waitFor(() => expect(createTimerImpl).toHaveBeenCalledTimes(1));
    // The new timer becomes available to select (a fresh, unchecked row).
    const newBox = (await screen.findByLabelText('Use Fast')) as HTMLInputElement;
    expect(newBox.checked).toBe(false);
  });

  it('shows per-row edit, and hides Remove for the built-in Mock', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, RH]);
    const { session } = makeTestSession({ listTimersImpl, event: EVENT });
    render(EventTimers, { session });

    const list = await screen.findByRole('list', { name: 'Configured timers' });
    const rows = within(list).getAllByRole('listitem');
    // Both rows have Edit; only the RH row has Remove.
    expect(within(rows[0]).getByRole('button', { name: 'Edit' })).toBeInTheDocument();
    expect(within(rows[0]).queryByRole('button', { name: 'Remove' })).toBeNull();
    expect(within(rows[1]).getByRole('button', { name: 'Remove' })).toBeInTheDocument();
  });
});
