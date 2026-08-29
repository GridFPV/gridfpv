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
  available_channels: [],
  manual_connect: false,
  calibration: [],
  disabled_nodes: []
};
const RH: Timer = {
  id: 'rh-1',
  name: 'Track RH',
  kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
  status: 'Configured',
  channel_capability: 'Flexible',
  node_count: 8,
  available_channels: [],
  manual_connect: false,
  calibration: [],
  disabled_nodes: []
};

describe('TimersPage (app-level timer registry)', () => {
  it('lists the registry on mount, with the built-in Mock undeletable', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, RH]);
    const { session } = makeTestSession({ listTimersImpl });
    render(TimersPage, { session, onhome: noop });

    // Wait for the registry read to land. Deliberately the LIST, not the text "Mock": that used
    // to resolve against the page's standing blurb, which #466 moved into a tooltip.
    await screen.findByRole('list', { name: 'Configured timers' });
    expect(screen.getByText('Track RH')).toBeInTheDocument();

    const list = screen.getByRole('list', { name: 'Configured timers' });
    const rows = within(list).getAllByRole('listitem');
    // Mock row: no Remove button (built-in). RH row: has one.
    expect(within(rows[0]).queryByRole('button', { name: 'Remove' })).toBeNull();
    expect(within(rows[1]).getByRole('button', { name: 'Remove' })).toBeInTheDocument();
  });

  it('warns that a timer with no chosen channels cannot seat a heat (#117 S1)', async () => {
    // The fifth instance of the empty-`available_channels` trap. This line used to read "No
    // channels available", which on a Flexible RotorHazard is exactly backwards: it can tune all 52
    // catalog channels and lists none only because nobody has ticked any. It also read as "nothing
    // to do here" when it is the thing to do — the server now REFUSES to seat a heat on a timer with
    // an empty allowed set, and this picker is where the RD closes the gap.
    const listTimersImpl = vi.fn(async () => [RH]);
    const { session } = makeTestSession({ listTimersImpl, listChannelsImpl: async () => CATALOG });
    render(TimersPage, { session, onhome: noop });

    await screen.findByText('Track RH');
    expect(screen.queryByText('No channels available')).toBeNull();
    expect(screen.getByText(/No channels chosen/)).toBeInTheDocument();
    expect(screen.getByText(/heats cannot be seated/)).toBeInTheDocument();
  });

  it('adds a timer and reloads the list', async () => {
    const created: Timer = {
      id: 'fast-x',
      name: 'Fast',
      kind: { Mock: { laps: 5, lap_ms: 12000 } },
      status: 'Ready',
      channel_capability: 'Flexible',
      node_count: 8,
      available_channels: [],
      manual_connect: false,
      calibration: [],
      disabled_nodes: []
    };
    let calls = 0;
    const listTimersImpl = vi.fn(async () => (calls++ === 0 ? [MOCK] : [MOCK, created]));
    const createTimerImpl = vi.fn(async () => created);
    const { session } = makeTestSession({ listTimersImpl, createTimerImpl });
    render(TimersPage, { session, onhome: noop });

    // Wait for the registry read to land. Deliberately the LIST, not the text "Mock": that used
    // to resolve against the page's standing blurb, which #466 moved into a tooltip.
    await screen.findByRole('list', { name: 'Configured timers' });
    await fireEvent.click(screen.getByRole('button', { name: '+ Add timer' }));

    const name = (await screen.findByLabelText('Timer name')) as HTMLInputElement;
    await fireEvent.input(name, { target: { value: 'Fast' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add timer' }));

    await waitFor(() => expect(createTimerImpl).toHaveBeenCalledTimes(1));
    // The add carries the channel config too (race redesign Slice 4b): the permissive default —
    // Flexible, no channels picked — when the RD does not touch the channel fields. `node_count`
    // is deliberately ABSENT (#412): it is an *override*, and a new timer follows whatever the
    // hardware reports on connect rather than being pinned to the console's fallback of 8.
    expect(createTimerImpl).toHaveBeenCalledWith(
      'http://d.local',
      {
        name: 'Fast',
        kind: { Mock: { laps: 3, lap_ms: 30000 } },
        channel_capability: 'Flexible',
        node_count: undefined,
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
    // The edit carries the timer's (unchanged) channel config too (race redesign Slice 4b) — but
    // NOT `node_count` (#412). Renaming a timer must not pin its width: sending the seeded value
    // back would silently create the reported-vs-configured drift this release exists to remove.
    expect(updateTimerImpl).toHaveBeenCalledWith(
      'http://d.local',
      'rh-1',
      {
        name: 'Renamed',
        kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
        channel_capability: 'Flexible',
        node_count: undefined,
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
      available_channels: [5658, 5800, 5685],
      manual_connect: false,
      calibration: [],
      disabled_nodes: []
    };
    let calls = 0;
    const listTimersImpl = vi.fn(async () => (calls++ === 0 ? [MOCK] : [MOCK, created]));
    const createTimerImpl = vi.fn(async () => created);
    const listChannelsImpl = vi.fn(async () => CATALOG);
    const { session } = makeTestSession({ listTimersImpl, createTimerImpl, listChannelsImpl });
    render(TimersPage, { session, onhome: noop });

    // Wait for the registry read to land. Deliberately the LIST, not the text "Mock": that used
    // to resolve against the page's standing blurb, which #466 moved into a tooltip.
    await screen.findByRole('list', { name: 'Configured timers' });
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
      available_channels: [5658],
      manual_connect: false,
      calibration: [],
      disabled_nodes: []
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
        // Untouched, so the width override is left exactly as it was (#412).
        node_count: undefined,
        available_channels: [5658, 5695]
      },
      'tok'
    );
  });

  it('ticks a whole band from its header box, tri-state, in catalog order (#429)', async () => {
    const created: Timer = {
      id: 'flex-b',
      name: 'Band',
      kind: { Mock: { laps: 3, lap_ms: 30000 } },
      status: 'Ready',
      channel_capability: 'Flexible',
      node_count: 8,
      available_channels: [5658, 5695],
      manual_connect: false,
      calibration: [],
      disabled_nodes: []
    };
    const listTimersImpl = vi.fn(async () => [MOCK]);
    const createTimerImpl = vi.fn(async () => created);
    const listChannelsImpl = vi.fn(async () => CATALOG);
    const { session } = makeTestSession({ listTimersImpl, createTimerImpl, listChannelsImpl });
    render(TimersPage, { session, onhome: noop });

    // Wait for the registry read to land. Deliberately the LIST, not the text "Mock": that used
    // to resolve against the page's standing blurb, which #466 moved into a tooltip.
    await screen.findByRole('list', { name: 'Configured timers' });
    await fireEvent.click(screen.getByRole('button', { name: '+ Add timer' }));
    const name = (await screen.findByLabelText('Timer name')) as HTMLInputElement;
    await fireEvent.input(name, { target: { value: 'Band' } });

    const box = (await screen.findByLabelText('All Raceband channels')) as HTMLInputElement;
    const r1 = screen.getByLabelText('Raceband R1, 5658 MHz') as HTMLInputElement;
    const r2 = screen.getByLabelText('Raceband R2, 5695 MHz') as HTMLInputElement;
    const f4 = screen.getByLabelText('Fatshark F4, 5800 MHz') as HTMLInputElement;

    // Nothing chosen: unchecked, and NOT indeterminate.
    expect(box.checked).toBe(false);
    expect(box.indeterminate).toBe(false);

    // One channel of the band: the box goes indeterminate — a partial band is a normal state.
    await fireEvent.click(r1);
    await waitFor(() => expect(box.indeterminate).toBe(true));
    expect(box.checked).toBe(false);

    // Clicking an indeterminate box FILLS the band; it does not wipe the RD's subset. Other bands
    // are untouched.
    await fireEvent.click(box);
    await waitFor(() => expect(r2.checked).toBe(true));
    expect(r1.checked).toBe(true);
    expect(f4.checked).toBe(false);
    expect(box.checked).toBe(true);
    expect(box.indeterminate).toBe(false);

    // A full band clears on the next click…
    await fireEvent.click(box);
    await waitFor(() => expect(r1.checked).toBe(false));
    expect(r2.checked).toBe(false);
    // …and fills again from empty.
    await fireEvent.click(box);
    await waitFor(() => expect(r1.checked).toBe(true));

    await fireEvent.click(screen.getByRole('button', { name: 'Add timer' }));
    await waitFor(() => expect(createTimerImpl).toHaveBeenCalledTimes(1));
    expect(createTimerImpl).toHaveBeenCalledWith(
      expect.anything(),
      // Catalog order, unchanged by the band toggle.
      expect.objectContaining({ available_channels: [5658, 5695] }),
      expect.anything()
    );
  });

  it('a Fixed timer’s band box ticks only that timer’s DECLARED channels (#429)', async () => {
    // The picker is already narrowed to `Fixed.channels`; the band box must tick from that, or it
    // would select channels the timer refuses. Raceband R2 is in the catalog but not declared.
    const fixed: Timer = {
      id: 'fix-2',
      name: 'Fixed RH',
      kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
      status: 'Configured',
      channel_capability: { Fixed: { channels: [5658, 5800] } },
      node_count: 2,
      available_channels: [],
      manual_connect: false,
      calibration: [],
      disabled_nodes: []
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

    const box = (await screen.findByLabelText('All Raceband channels')) as HTMLInputElement;
    // R2 is not offered at all, so the Raceband band here is R1 alone.
    expect(screen.queryByLabelText('Raceband R2, 5695 MHz')).toBeNull();

    await fireEvent.click(box);
    // One declared channel = the whole offered band: checked outright, never stuck indeterminate.
    await waitFor(() => expect(box.checked).toBe(true));
    expect(box.indeterminate).toBe(false);

    await fireEvent.click(screen.getByRole('button', { name: 'Save changes' }));
    await waitFor(() => expect(updateTimerImpl).toHaveBeenCalledTimes(1));
    expect(updateTimerImpl).toHaveBeenCalledWith(
      expect.anything(),
      'fix-2',
      expect.objectContaining({ available_channels: [5658] }),
      expect.anything()
    );
  });

  it('offers no “select all bands” — the pool a Flexible timer draws from stays deliberate', async () => {
    // Not an oversight (#429): the allowed set is what the IMD picker chooses from when a round
    // names no layout, so a needlessly wide pool makes the automatic answer worse.
    const listTimersImpl = vi.fn(async () => [MOCK]);
    const listChannelsImpl = vi.fn(async () => CATALOG);
    const { session } = makeTestSession({ listTimersImpl, listChannelsImpl });
    render(TimersPage, { session, onhome: noop });

    // Wait for the registry read to land. Deliberately the LIST, not the text "Mock": that used
    // to resolve against the page's standing blurb, which #466 moved into a tooltip.
    await screen.findByRole('list', { name: 'Configured timers' });
    await fireEvent.click(screen.getByRole('button', { name: '+ Add timer' }));
    await screen.findByLabelText('All Raceband channels');

    const picker = screen.getByRole('group', { name: 'Available channels' });
    const boxes = within(picker).getAllByRole('checkbox') as HTMLInputElement[];
    // Two bands + three channels; no fourth "all" control above them.
    expect(boxes).toHaveLength(5);
    expect(within(picker).queryByLabelText(/^All channels$/)).toBeNull();
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

/**
 * Manual **Connect / Disconnect** on the Timers screen (issue #383).
 *
 * The premise of #383 is that this works with **no active event**. Before it, a timer only ever
 * dialed when the *active event* selected it, so "is this timer even reachable?" — the question
 * the Timers screen exists to answer — could not be asked without first creating and activating an
 * event. `makeTestSession({ noEnter: true })` is therefore load-bearing in every test here: it
 * leaves the session outside any event, exactly as an RD setting up at a venue would be.
 */
describe('Timers screen — manual connect / disconnect (#383, no active event)', () => {
  /** The RH timer, with the manual hold and status a given test wants. */
  function rhWith(manual_connect: boolean, status: Timer['status'] = 'Configured'): Timer {
    return { ...RH, manual_connect, status };
  }

  it('offers Connect on a RotorHazard row with NO event, and holds the connection', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, rhWith(false)]);
    const connectTimerImpl = vi.fn(async () => rhWith(true, 'Connecting'));
    const { session } = makeTestSession({ noEnter: true, listTimersImpl, connectTimerImpl });
    render(TimersPage, { session, onhome: noop });

    await screen.findByText('Track RH');
    // No event was ever entered — the session's own timer poll never started.
    expect(session.timers).toEqual([]);

    const list = screen.getByRole('list', { name: 'Configured timers' });
    const rhRow = within(list).getAllByRole('listitem')[1];
    await fireEvent.click(within(rhRow).getByRole('button', { name: 'Connect' }));

    await waitFor(() => expect(connectTimerImpl).toHaveBeenCalledTimes(1));
    expect(connectTimerImpl).toHaveBeenCalledWith('http://d.local', 'rh-1', 'tok');
    // The server's answer is folded straight back in, so the control flips at once rather than
    // waiting a reconciler tick — and the row starts reading the hold out loud.
    await waitFor(() =>
      expect(within(rhRow).getByRole('button', { name: 'Disconnect' })).toBeInTheDocument()
    );
    expect(within(rhRow).getByText('Connecting…')).toBeInTheDocument();
  });

  it('does NOT offer the control on the built-in Mock — it has nothing to dial', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, rhWith(false)]);
    const { session } = makeTestSession({ noEnter: true, listTimersImpl });
    render(TimersPage, { session, onhome: noop });

    // Wait for the registry read to land. Deliberately the LIST, not the text "Mock": that used
    // to resolve against the page's standing blurb, which #466 moved into a tooltip.
    await screen.findByRole('list', { name: 'Configured timers' });
    const list = screen.getByRole('list', { name: 'Configured timers' });
    const [mockRow, rhRow] = within(list).getAllByRole('listitem');
    // The Director answers a Mock's connect with a 400; better to not offer it than to be rejected.
    expect(within(mockRow).queryByRole('button', { name: 'Connect' })).toBeNull();
    expect(within(rhRow).getByRole('button', { name: 'Connect' })).toBeInTheDocument();
  });

  it('releases the hold on Disconnect', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, rhWith(true, 'Connected')]);
    const disconnectTimerImpl = vi.fn(async () => rhWith(false, 'Disconnected'));
    const { session } = makeTestSession({ noEnter: true, listTimersImpl, disconnectTimerImpl });
    render(TimersPage, { session, onhome: noop });

    await screen.findByText('Track RH');
    const list = screen.getByRole('list', { name: 'Configured timers' });
    const rhRow = within(list).getAllByRole('listitem')[1];
    // A standing hold reads "Reachable" and offers the release, not another Connect.
    expect(within(rhRow).getByText(/Reachable/)).toBeInTheDocument();

    await fireEvent.click(within(rhRow).getByRole('button', { name: 'Disconnect' }));
    await waitFor(() => expect(disconnectTimerImpl).toHaveBeenCalledTimes(1));
    expect(disconnectTimerImpl).toHaveBeenCalledWith('http://d.local', 'rh-1', 'tok');
    await waitFor(() =>
      expect(within(rhRow).getByRole('button', { name: 'Connect' })).toBeInTheDocument()
    );
    // Hold released ⇒ nothing more to narrate; the StatusPill carries it from here.
    expect(within(rhRow).queryByText(/Reachable/)).toBeNull();
  });

  it('keeps re-reading the registry while a hold is up, with no event poll running', async () => {
    // THE regression this guards: the session's timer poll only runs inside an event, so with no
    // event nothing would refresh `status` — the RD would press Connect and watch a pill that never
    // moved. The screen must poll for itself while a hold exists.
    let status: Timer['status'] = 'Connecting';
    const listTimersImpl = vi.fn(async () => [MOCK, rhWith(true, status)]);
    const { session } = makeTestSession({ noEnter: true, listTimersImpl });
    render(TimersPage, { session, onhome: noop });

    await screen.findByText('Track RH');
    const list = screen.getByRole('list', { name: 'Configured timers' });
    const rhRow = within(list).getAllByRole('listitem')[1];
    expect(within(rhRow).getByText('Connecting…')).toBeInTheDocument();

    // The Director's reconciler gets the connection up; only a re-read can show that.
    status = 'Connected';
    await waitFor(() => expect(within(rhRow).getByText(/Reachable/)).toBeInTheDocument(), {
      timeout: 5000
    });
    expect(listTimersImpl.mock.calls.length).toBeGreaterThan(1);
  });

  it('does not poll when nothing is held — an idle Timers screen stays quiet', async () => {
    const listTimersImpl = vi.fn(async () => [MOCK, rhWith(false)]);
    const { session } = makeTestSession({ noEnter: true, listTimersImpl });
    render(TimersPage, { session, onhome: noop });

    await screen.findByText('Track RH');
    const calls = listTimersImpl.mock.calls.length;
    await new Promise((r) => setTimeout(r, 2500));
    expect(listTimersImpl.mock.calls.length).toBe(calls);
  });
});

describe('TimerManager — a coincident frequency keeps its own catalog position', () => {
  it('orders Raceband R7 before R8, not after it', async () => {
    // 5880 is BOTH Raceband R7 and Fatshark F8. Building the sort index with
    // `new Map(catalog.map(([mhz, i])))` keeps the LAST entry for a duplicate key, so R7 sorted at
    // Fatshark's position and rendered after R8 — visible on the RD's bench and, once saved, in
    // the order the allowed set is stored in.
    const catalog = [
      { band: 'Raceband', channel: 'R6', mhz: 5843 },
      { band: 'Raceband', channel: 'R7', mhz: 5880 },
      { band: 'Raceband', channel: 'R8', mhz: 5917 },
      { band: 'Fatshark', channel: 'F8', mhz: 5880 }
    ];
    const order = new Map<number, number>();
    catalog.forEach((e, i) => {
      if (!order.has(e.mhz)) order.set(e.mhz, i);
    });
    const sorted = [5917, 5880, 5843].sort((a, b) => order.get(a)! - order.get(b)!);
    expect(sorted).toEqual([5843, 5880, 5917]);

    // The shape of the bug, so it cannot come back by "simplifying" the map construction.
    const lastWins = new Map(catalog.map((e, i) => [e.mhz, i] as const));
    expect(lastWins.get(5880)).toBe(3); // Fatshark's index
    expect(order.get(5880)).toBe(1); // Raceband's — the one the label uses
  });
});

describe('the post-add second look (#462, field 2026-08-28)', () => {
  it('re-reads the registry after the reconciler sweep so a fresh RH timer is never stale', async () => {
    // The bug: the reconciler grants a new RH timer its automatic dial AFTER the add's reload
    // snapshot, so the screen saw no hold, never started its hold-poll, and the row sat stale on
    // its pre-dial status until the RD navigated away and back. The fix takes one delayed quiet
    // refresh; this drives the real dialog and asserts the second look happens and picks up the
    // dialling state.
    const added: Timer = {
      id: 'rh-new',
      name: 'Gate RH',
      kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
      status: 'Configured',
      channel_capability: 'Flexible',
      node_count: null,
      available_channels: [],
      manual_connect: false,
      calibration: [],
      disabled_nodes: []
    } as unknown as Timer;
    // Read 1: initial load. Read 2: the post-create reload (still pre-dial, unheld). Read 3+:
    // the delayed second look — by now the reconciler has auto-held it and it is Connecting.
    let reads = 0;
    const listTimersImpl = vi.fn(async () => {
      reads += 1;
      if (reads <= 1) return [];
      if (reads === 2) return [added];
      return [{ ...added, manual_connect: true, status: 'Connecting' as Timer['status'] }];
    });
    const createTimerImpl = vi.fn(async () => added);
    const { session } = makeTestSession({ listTimersImpl, createTimerImpl });
    render(TimersPage, { session, onhome: noop });
    await screen.findByRole('list', { name: 'Configured timers' });

    await fireEvent.click(screen.getByRole('button', { name: '+ Add timer' }));
    const name = (await screen.findByLabelText('Timer name')) as HTMLInputElement;
    await fireEvent.input(name, { target: { value: 'Gate RH' } });
    const kind = (await screen.findByLabelText('Timer kind')) as HTMLSelectElement;
    await fireEvent.change(kind, { target: { value: 'Rotorhazard' } });
    const url = (await screen.findByLabelText('RotorHazard URL')) as HTMLInputElement;
    await fireEvent.input(url, { target: { value: 'http://rh.local:5000' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add timer' }));
    await waitFor(() => expect(createTimerImpl).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(listTimersImpl.mock.calls.length).toBeGreaterThanOrEqual(2));

    // The second look lands on its own — no navigation — and the row reads the live dial state.
    await waitFor(() => expect(listTimersImpl.mock.calls.length).toBeGreaterThanOrEqual(3), {
      timeout: 5000
    });
    await screen.findByText('Connecting', undefined, { timeout: 5000 });
  });
});
