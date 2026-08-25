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
  status: 'Ready',
  channel_capability: 'Flexible',
  node_count: 8,
  available_channels: [],
  manual_connect: false,
  calibration: []
};
/**
 * A **healthy** RotorHazard timer: connected, and its GridFPV plugin probed `Present`. Since #405
 * that is the only state in which an event may select an RH timer, so it is what the ordinary
 * selection tests use; the gate's own tests build the unhealthy variants from it.
 */
const RH: Timer = {
  id: 'rh-1',
  name: 'Track RH',
  kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
  status: 'Connected',
  channel_capability: 'Flexible',
  node_count: 8,
  available_channels: [],
  manual_connect: false,
  calibration: [],
  plugin: { Present: { plugin_version: '0.1.0', rhapi_version: '1.4', capabilities: ['hello'] } }
};

/** The same RH timer with a given plugin presence — the #405 gate's three refusal cases. */
function rhWithPlugin(plugin: Timer['plugin']): Timer {
  return { ...RH, plugin };
}

/** A created event selecting only the Mock timer (so its checkbox seeds checked). */
const EVENT: EventMeta = {
  id: 'e1',
  name: 'Friday',
  created_at: 0,
  persistent: true,
  timers: ['mock'],
  roster: [],
  classes: []
};

/** A redundant-gate event with two selected timers, no explicit primary (first = effective). */
const EVENT_2: EventMeta = {
  id: 'e2',
  name: 'Redundant',
  created_at: 0,
  persistent: true,
  timers: ['mock', 'rh-1'],
  roster: [],
  classes: []
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
    // Selection auto-saves — there is no explicit Save button.
    expect(screen.queryByRole('button', { name: 'Save selection' })).toBeNull();
    expect(screen.getByText(/saved automatically/)).toBeInTheDocument();
  });

  it('auto-saves the working selection (debounced) in registry-listed order on a toggle', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, RH]);
    const setEventTimersImpl = vi.fn(async () => ({ ...EVENT, timers: ['mock', 'rh-1'] }));
    const { session } = makeTestSession({ listTimersImpl, setEventTimersImpl, event: EVENT });
    render(EventTimers, { session });

    const rhBox = (await screen.findByLabelText('Use Track RH')) as HTMLInputElement;
    // Optimistic: the box flips instantly, no Save click.
    await fireEvent.click(rhBox);
    expect(rhBox.checked).toBe(true);

    // The debounced save lands on its own with the FULL selection in registry order.
    await waitFor(() => expect(setEventTimersImpl).toHaveBeenCalledTimes(1));
    expect(setEventTimersImpl).toHaveBeenCalledWith(
      'http://d.local',
      'e1',
      ['mock', 'rh-1'],
      'tok'
    );
  });

  it('coalesces rapid toggles into one trailing save carrying the latest selection', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, RH]);
    const setEventTimersImpl = vi.fn(async () => EVENT);
    const { session } = makeTestSession({ listTimersImpl, setEventTimersImpl, event: EVENT });
    render(EventTimers, { session });

    const rhBox = (await screen.findByLabelText('Use Track RH')) as HTMLInputElement;
    // Tick then untick within the debounce window → one save, back at {mock}.
    await fireEvent.click(rhBox);
    await fireEvent.click(rhBox);

    await waitFor(() => expect(setEventTimersImpl).toHaveBeenCalledTimes(1));
    expect(setEventTimersImpl).toHaveBeenCalledWith('http://d.local', 'e1', ['mock'], 'tok');
  });

  it('clears the last timer with no snap-back (skips the empty auto-save)', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, RH]);
    const setEventTimersImpl = vi.fn(async () => EVENT);
    const { session } = makeTestSession({ listTimersImpl, setEventTimersImpl, event: EVENT });
    render(EventTimers, { session });

    const mockBox = (await screen.findByLabelText('Use Mock')) as HTMLInputElement;
    await fireEvent.click(mockBox); // unselect the only selected → selection empties

    // No snap-back: the box stays unchecked, the count reflects an empty selection, and (because an
    // empty selection is kept local, not persisted) no wholesale save goes out.
    expect(mockBox.checked).toBe(false);
    expect(screen.getByText(/0 selected/)).toBeInTheDocument();
    await new Promise((r) => setTimeout(r, 400));
    expect(setEventTimersImpl).not.toHaveBeenCalled();

    // Re-ticking it resumes the wholesale save with the restored selection.
    await fireEvent.click(mockBox);
    expect(mockBox.checked).toBe(true);
    await waitFor(() => expect(setEventTimersImpl).toHaveBeenCalledTimes(1));
    expect(setEventTimersImpl).toHaveBeenCalledWith('http://d.local', 'e1', ['mock'], 'tok');
  });

  it('reverts the optimistic toggle when the save fails', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, RH]);
    const setEventTimersImpl = vi.fn(async () => {
      throw new Error('boom');
    });
    const { session } = makeTestSession({ listTimersImpl, setEventTimersImpl, event: EVENT });
    render(EventTimers, { session });

    const rhBox = (await screen.findByLabelText('Use Track RH')) as HTMLInputElement;
    await fireEvent.click(rhBox); // optimistically checked
    expect(rhBox.checked).toBe(true);

    // The failed save reverts the selection to the last-saved state (RH unchecked again).
    await waitFor(() => expect(setEventTimersImpl).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect((screen.getByLabelText('Use Track RH') as HTMLInputElement).checked).toBe(false)
    );
  });

  it('adds a timer from inside the event (shared management)', async () => {
    const created: Timer = {
      id: 'fast-x',
      name: 'Fast',
      kind: { Mock: { laps: 5, lap_ms: 12000 } },
      status: 'Ready',
      channel_capability: 'Flexible',
      node_count: 8,
      available_channels: [],
      manual_connect: false,
      calibration: []
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

  it('hides timer roles when only one timer is selected', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, RH]);
    const { session } = makeTestSession({ listTimersImpl, event: EVENT });
    render(EventTimers, { session });

    await screen.findByLabelText('Use Mock');
    expect(screen.queryByRole('list', { name: 'Timer roles' })).toBeNull();
  });

  it('shows roles for 2+ timers: the first selected is the effective primary', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, RH]);
    const { session } = makeTestSession({ listTimersImpl, event: EVENT_2 });
    render(EventTimers, { session });

    await screen.findByRole('list', { name: 'Timer roles' });
    // No explicit primary → the first selected ('mock') is primary; the rest are alternates.
    const primaryRadio = screen.getByLabelText('Make Mock the primary timer') as HTMLInputElement;
    const altRadio = screen.getByLabelText('Make Track RH the primary timer') as HTMLInputElement;
    expect(primaryRadio.checked).toBe(true);
    expect(altRadio.checked).toBe(false);
    // The role labels are legible.
    const roles = screen.getByRole('list', { name: 'Timer roles' });
    expect(within(roles).getByText('Primary')).toBeInTheDocument();
    expect(within(roles).getByText('Alternate')).toBeInTheDocument();
  });

  it('reflects an explicit primary_timer as the current selection', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, RH]);
    const { session } = makeTestSession({
      listTimersImpl,
      event: { ...EVENT_2, primary_timer: 'rh-1' }
    });
    render(EventTimers, { session });

    await screen.findByRole('list', { name: 'Timer roles' });
    expect(
      (screen.getByLabelText('Make Track RH the primary timer') as HTMLInputElement).checked
    ).toBe(true);
    expect((screen.getByLabelText('Make Mock the primary timer') as HTMLInputElement).checked).toBe(
      false
    );
  });

  it('designating a new primary calls setPrimaryTimer with the chosen id', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, RH]);
    const setPrimaryTimerImpl = vi.fn(async () => ({ ...EVENT_2, primary_timer: 'rh-1' }));
    const { session } = makeTestSession({ listTimersImpl, setPrimaryTimerImpl, event: EVENT_2 });
    render(EventTimers, { session });

    await screen.findByRole('list', { name: 'Timer roles' });
    await fireEvent.click(screen.getByLabelText('Make Track RH the primary timer'));

    await waitFor(() => expect(setPrimaryTimerImpl).toHaveBeenCalledTimes(1));
    expect(setPrimaryTimerImpl).toHaveBeenCalledWith('http://d.local', 'e2', 'rh-1', 'tok');
    // The radio reflects the re-homed event's new primary.
    await waitFor(() =>
      expect(
        (screen.getByLabelText('Make Track RH the primary timer') as HTMLInputElement).checked
      ).toBe(true)
    );
  });
});

/**
 * The **GridFPV-plugin selection gate** (#405): a RotorHazard timer without a loaded, compatible
 * plugin cannot be selected for an event. The Director enforces the same rule on
 * `PUT /events/{id}/timers`; these cover the console half — which must render the *reason* on the
 * unselectable row, not merely disable it, because a dead end with no explanation is the failure
 * this gate exists to prevent.
 */
describe('EventTimers — the GridFPV-plugin selection gate (#405)', () => {
  it('lets a Present-plugin RotorHazard timer be selected normally', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, RH]);
    const setEventTimersImpl = vi.fn(async () => ({ ...EVENT, timers: ['mock', 'rh-1'] }));
    const { session } = makeTestSession({ listTimersImpl, setEventTimersImpl, event: EVENT });
    render(EventTimers, { session });

    const rhBox = (await screen.findByLabelText('Use Track RH')) as HTMLInputElement;
    expect(rhBox.disabled).toBe(false);
    await fireEvent.click(rhBox);
    await waitFor(() => expect(setEventTimersImpl).toHaveBeenCalledTimes(1));
  });

  it('refuses a never-probed timer with “connect it first”, not “plugin missing”', async () => {
    // `plugin: undefined` is a different problem with a different fix: presence is only knowable
    // over a live socket, so this is the normal state of a freshly added timer.
    const listTimersImpl = vi.fn(async () => [MOCK, rhWithPlugin(undefined)]);
    const setEventTimersImpl = vi.fn(async () => EVENT);
    const { session } = makeTestSession({ listTimersImpl, setEventTimersImpl, event: EVENT });
    render(EventTimers, { session });

    const rhBox = (await screen.findByLabelText('Use Track RH')) as HTMLInputElement;
    expect(rhBox.disabled).toBe(true);
    // The reason is ON the row, naming the timer, and pointing at connecting — not installing.
    const reason = screen.getByText(/hasn’t been connected yet/);
    expect(reason.textContent).toContain('Track RH');
    expect(reason.textContent).toContain('Connect it');
    expect(reason.textContent).not.toContain('rh-1');
    expect(screen.queryByText(/which Grid requires to race a RotorHazard timer/)).toBeNull();

    // Even a forced click never reaches the wire.
    await fireEvent.click(rhBox);
    await new Promise((r) => setTimeout(r, 50));
    expect(setEventTimersImpl).not.toHaveBeenCalled();
  });

  it('refuses a Missing plugin with the install instruction', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, rhWithPlugin('Missing')]);
    const { session } = makeTestSession({ listTimersImpl, event: EVENT });
    render(EventTimers, { session });

    expect(((await screen.findByLabelText('Use Track RH')) as HTMLInputElement).disabled).toBe(
      true
    );
    const reason = screen.getByText(/which Grid requires to race a RotorHazard timer/);
    expect(reason.textContent).toContain('Track RH');
    expect(reason.textContent).toContain('Install it');
  });

  it('refuses an Incompatible plugin with the update instruction', async () => {
    const listTimersImpl = vi.fn(async () => [
      MOCK,
      rhWithPlugin({
        Incompatible: { plugin_version: '0.0.1', protocol_version: 99, reason: 'protocol 99' }
      })
    ]);
    const { session } = makeTestSession({ listTimersImpl, event: EVENT });
    render(EventTimers, { session });

    expect(((await screen.findByLabelText('Use Track RH')) as HTMLInputElement).disabled).toBe(
      true
    );
    const reason = screen.getByText(/speaks a protocol this Director doesn’t/);
    expect(reason.textContent).toContain('Track RH');
    expect(reason.textContent).toContain('Update it');
  });

  it('never gates a Mock timer', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, rhWithPlugin('Missing')]);
    const { session } = makeTestSession({ listTimersImpl, event: EVENT });
    render(EventTimers, { session });
    expect(((await screen.findByLabelText('Use Mock')) as HTMLInputElement).disabled).toBe(false);
  });

  it('keeps an ALREADY-selected plugin-less timer tickable, and warns on its row', async () => {
    // A pre-#405 event may already select a plugin-less RH timer, and a plugin can go away after a
    // valid selection. The Director grandfathers the existing selection, so the box must stay live
    // — the RD has to be able to untick it — and the row surfaces the presence change instead.
    const listTimersImpl = vi.fn(async () => [MOCK, rhWithPlugin('Missing')]);
    const setEventTimersImpl = vi.fn(async () => EVENT_2);
    const { session } = makeTestSession({ listTimersImpl, setEventTimersImpl, event: EVENT_2 });
    render(EventTimers, { session });

    const rhBox = (await screen.findByLabelText('Use Track RH')) as HTMLInputElement;
    expect(rhBox.checked).toBe(true);
    expect(rhBox.disabled).toBe(false);
    const warning = screen.getByText(/its GridFPV plugin has gone away/);
    expect(warning.textContent).toContain('Track RH');
    expect(warning.textContent).toContain('arming a heat is refused');

    // Unticking it saves the reduced selection — the event stays editable.
    await fireEvent.click(rhBox);
    await waitFor(() => expect(setEventTimersImpl).toHaveBeenCalledTimes(1));
    expect(setEventTimersImpl).toHaveBeenCalledWith('http://d.local', 'e2', ['mock'], 'tok');
  });
});
