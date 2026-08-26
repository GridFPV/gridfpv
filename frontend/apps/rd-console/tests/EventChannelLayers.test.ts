/**
 * The in-event **channel layer** editor (#117 S2).
 *
 * What is actually being defended here:
 *
 *  - **no raw id reaches the screen** — a node renders as `"Node 3"`, a channel as `"Raceband R3"`,
 *    a layer as its name. The dropdown's option *values* stay raw MHz (a wire handle); its labels
 *    may not;
 *  - the **global→event seam** — adding a layer with nothing picked sends no `nodes` at all, which
 *    is the Director's seed path (the allowed set laid onto the enabled nodes);
 *  - **only allowed channels, only enabled nodes** are offered, so the editor cannot even build the
 *    two refusals the Director exists to make;
 *  - a duplicate channel **blocks Save**, and a cross-layer overlap **does not** — it is a notice.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import { toasts } from '@gridfpv/components';
import type { createChannelLayer, updateChannelLayer } from '@gridfpv/protocol-client';
import type {
  ChannelCatalogEntry,
  ChannelLayers,
  EventMeta,
  Timer,
  TimerNodes
} from '@gridfpv/types';
import EventChannelLayers from '../src/screens/EventChannelLayers.svelte';
import { makeTestSession } from './support.js';

/** A four-node timer allowing Raceband R1–R4 — the bracket-strategy shape. */
const TIMER: Timer = {
  id: 'rh-1',
  name: 'Track RH',
  kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
  status: 'Connected',
  channel_capability: 'Flexible',
  node_count: 4,
  available_channels: [5658, 5695, 5732, 5769],
  manual_connect: false,
  calibration: [],
  disabled_nodes: []
} as unknown as Timer;

/** The same timer with nothing ticked — the empty-allowed-set trap. */
const UNCONFIGURED: Timer = { ...TIMER, available_channels: [] };

const CATALOG: ChannelCatalogEntry[] = [
  { band: 'Raceband', channel: 'R1', mhz: 5658 },
  { band: 'Raceband', channel: 'R2', mhz: 5695 },
  { band: 'Raceband', channel: 'R3', mhz: 5732 },
  { band: 'Raceband', channel: 'R4', mhz: 5769 },
  { band: 'Fatshark', channel: 'F4', mhz: 5800 }
];

/** Node index 2 disabled, so the enabled set is `[0, 1, 3]` — a set with a hole. */
const NODES: TimerNodes = {
  timer: 'rh-1',
  width: 4,
  nodes: [0, 1, 2, 3].map((n) => ({
    node: n,
    label: `Node ${n + 1}`,
    seat: `node-${n}`,
    enabled: n !== 2,
    reported: true
  })),
  enabled: [0, 1, 3]
};

const EVENT: EventMeta = {
  id: 'e1',
  name: 'Friday',
  created_at: 0,
  persistent: true,
  timers: ['rh-1'],
  primary_timer: 'rh-1',
  roster: [],
  classes: []
};

const BRACKET_A: ChannelLayers = {
  layers: [
    {
      id: 'bracket-a-k3f9',
      name: 'Bracket A',
      nodes: [
        { node: 0, channel: 5658 },
        { node: 1, channel: 5695 },
        { node: 3, channel: 5769 }
      ]
    }
  ],
  overlaps: []
};

/**
 * The reads every test needs: the catalog, the node view and the layer list.
 *
 * The layer read is **stateful** on purpose. Every layer write re-homes `currentEvent`, which is
 * what keeps the console's cached meta honest — and that re-home makes the editor re-read. A
 * fixed-`[]` read would answer the re-read with "no layers" and hide the write, which the real
 * Director never would. `seed` is the store the writes below mutate, exactly as the server's is.
 */
function impls(seed: { view: ChannelLayers }, extra: Record<string, unknown> = {}) {
  return {
    listChannelsImpl: vi.fn(async () => CATALOG),
    timerNodesImpl: vi.fn(async () => NODES),
    listChannelLayersImpl: vi.fn(async () => seed.view),
    ...extra
  };
}

/** A store starting from `view`, shared by the stateful read and the writes in one test. */
function store(view: ChannelLayers = { layers: [], overlaps: [] }): { view: ChannelLayers } {
  return { view };
}

beforeEach(() => toasts.clear());

describe('EventChannelLayers — what the RD reads', () => {
  it('renders each node with its channel, never an index or a bare MHz', async () => {
    const { session } = makeTestSession({
      ...impls(store(BRACKET_A)),
      event: EVENT
    });
    render(EventChannelLayers, { session, timer: TIMER });

    const list = await screen.findByRole('list', { name: 'Channel layers' });
    const row = within(list).getByText('Bracket A').closest('li') as HTMLElement;
    expect(row.textContent).toContain('Node 1 · Raceband R1');
    expect(row.textContent).toContain('Node 4 · Raceband R4');
    // The disabled node is not tuned, and no raw value leaks.
    expect(row.textContent).not.toContain('node-0');
    expect(row.textContent).not.toContain('5658');
    expect(row.textContent).not.toContain('bracket-a-k3f9');
  });

  it('offers only the ALLOWED channels, labelled by band, and only the ENABLED nodes', async () => {
    const { session } = makeTestSession({ ...impls(store()), event: EVENT });
    render(EventChannelLayers, { session, timer: TIMER });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add layer' }));

    // Node index 2 is disabled — it must not be offered at all (#412).
    expect(await screen.findByLabelText('Channel for Node 1')).toBeTruthy();
    expect(screen.getByLabelText('Channel for Node 2')).toBeTruthy();
    expect(screen.queryByLabelText('Channel for Node 3')).toBeNull();
    expect(screen.getByLabelText('Channel for Node 4')).toBeTruthy();

    const select = screen.getByLabelText('Channel for Node 1') as HTMLSelectElement;
    const labels = [...select.options].map((o) => o.textContent?.trim());
    // Raceband R1–R4 (what the RD ticked) and nothing else — Fatshark F4 is in the catalog but is
    // not allowed on this timer, so it is not an option the Director would then refuse.
    expect(labels).toEqual(['Not set', 'Raceband R1', 'Raceband R2', 'Raceband R3', 'Raceband R4']);
    // The option VALUE stays the raw wire handle.
    expect([...select.options].map((o) => o.value)).toEqual(['', '5658', '5695', '5732', '5769']);
  });

  it('says the timer is unconfigured rather than showing an empty dropdown', async () => {
    const { session } = makeTestSession({ ...impls(store()), event: EVENT });
    render(EventChannelLayers, { session, timer: UNCONFIGURED });

    const banner = await screen.findByRole('alert');
    expect(banner.textContent).toContain('Track RH');
    expect(banner.textContent).toContain('Timers page');
    // And there is nothing to add until that is fixed.
    expect(
      (screen.getByRole('button', { name: '+ Add layer' }) as HTMLButtonElement).disabled
    ).toBe(true);
  });
});

describe('EventChannelLayers — defining a layer', () => {
  it('seeds from the global allowed set: adding with nothing picked sends no nodes', async () => {
    const seed = store();
    const createChannelLayerImpl = vi.fn<typeof createChannelLayer>(
      async () => (seed.view = BRACKET_A)
    );
    const { session } = makeTestSession({
      ...impls(seed, { createChannelLayerImpl }),
      event: EVENT
    });
    render(EventChannelLayers, { session, timer: TIMER });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add layer' }));
    // The seed note is what tells the RD this is not an empty form.
    expect(screen.getByText(/starts from the channels/)).toBeTruthy();
    await fireEvent.click(screen.getByRole('button', { name: 'Add layer' }));

    await waitFor(() => expect(createChannelLayerImpl).toHaveBeenCalled());
    const request = createChannelLayerImpl.mock.calls[0][2];
    expect(request.name).toBe('Layer A');
    // The seam: no `nodes` at all, so the Director lays the allowed set onto the enabled nodes.
    expect(request.nodes).toBeUndefined();
    // And the stored layer comes back rendered by name.
    expect(await screen.findByText('Bracket A')).toBeTruthy();
  });

  it('sends the explicit mapping once the RD picks channels', async () => {
    const seed = store();
    const createChannelLayerImpl = vi.fn<typeof createChannelLayer>(
      async () => (seed.view = BRACKET_A)
    );
    const { session } = makeTestSession({
      ...impls(seed, { createChannelLayerImpl }),
      event: EVENT
    });
    render(EventChannelLayers, { session, timer: TIMER });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add layer' }));
    await fireEvent.change(await screen.findByLabelText('Channel for Node 1'), {
      target: { value: '5658' }
    });
    await fireEvent.change(screen.getByLabelText('Channel for Node 2'), {
      target: { value: '5695' }
    });
    await fireEvent.change(screen.getByLabelText('Channel for Node 4'), {
      target: { value: '5769' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add layer' }));

    await waitFor(() => expect(createChannelLayerImpl).toHaveBeenCalled());
    expect(createChannelLayerImpl.mock.calls[0][2]).toEqual({
      name: 'Layer A',
      nodes: [
        { node: 0, channel: 5658 },
        { node: 1, channel: 5695 },
        { node: 3, channel: 5769 }
      ]
    });
  });

  it('refuses to save two nodes on one channel, naming both and the channel', async () => {
    const createChannelLayerImpl = vi.fn<typeof createChannelLayer>(async () => BRACKET_A);
    const { session } = makeTestSession({
      ...impls(store(), { createChannelLayerImpl }),
      event: EVENT
    });
    render(EventChannelLayers, { session, timer: TIMER });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add layer' }));
    await fireEvent.change(await screen.findByLabelText('Channel for Node 1'), {
      target: { value: '5658' }
    });
    await fireEvent.change(screen.getByLabelText('Channel for Node 2'), {
      target: { value: '5658' }
    });

    const blocker = await screen.findByText(/cannot share a frequency/);
    expect(blocker.textContent).toContain('Node 1');
    expect(blocker.textContent).toContain('Node 2');
    expect(blocker.textContent).toContain('Raceband R1');
    expect(blocker.textContent).not.toContain('5658');

    expect((screen.getByRole('button', { name: 'Add layer' }) as HTMLButtonElement).disabled).toBe(
      true
    );
    expect(createChannelLayerImpl).not.toHaveBeenCalled();
  });

  it('will not save a half-tuned layer — a layer tunes every enabled node', async () => {
    const { session } = makeTestSession({ ...impls(store()), event: EVENT });
    render(EventChannelLayers, { session, timer: TIMER });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add layer' }));
    await fireEvent.change(await screen.findByLabelText('Channel for Node 1'), {
      target: { value: '5658' }
    });

    const blocker = await screen.findByText(/tunes every enabled node/);
    expect(blocker.textContent).toContain('Node 2');
    expect(blocker.textContent).toContain('Node 4');
    expect((screen.getByRole('button', { name: 'Add layer' }) as HTMLButtonElement).disabled).toBe(
      true
    );
  });

  it('surfaces the Director’s own refusal sentence verbatim', async () => {
    const createChannelLayerImpl = vi.fn<typeof createChannelLayer>(async () => {
      throw new Error('Node 2 and Node 3 are both on Raceband R1 in this layer.');
    });
    const { session } = makeTestSession({
      ...impls(store(), { createChannelLayerImpl }),
      event: EVENT
    });
    render(EventChannelLayers, { session, timer: TIMER });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add layer' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Add layer' }));

    // Surfaced VERBATIM — the Director's message already names the nodes and the channel by their
    // friendly names, so re-wording it could only make it worse.
    await waitFor(() => expect(toasts.items).toHaveLength(1));
    expect(toasts.items[0].tone).toBe('danger');
    expect(toasts.items[0].message).toBe(
      'Node 2 and Node 3 are both on Raceband R1 in this layer.'
    );
  });
});

describe('EventChannelLayers — cross-layer overlap', () => {
  it('shows the overlap as a notice, with both layers named and nothing blocked', async () => {
    const overlapping: ChannelLayers = {
      layers: [
        BRACKET_A.layers[0],
        {
          id: 'pack-b-z1x8',
          name: 'Pack B',
          nodes: [
            { node: 0, channel: 5658 },
            { node: 1, channel: 5732 },
            { node: 3, channel: 5769 }
          ]
        }
      ],
      overlaps: [{ layer: 'bracket-a-k3f9', other: 'pack-b-z1x8', channels: [5658, 5769] }]
    };
    const { session } = makeTestSession({
      ...impls(store(overlapping)),
      event: EVENT
    });
    render(EventChannelLayers, { session, timer: TIMER });

    const notice = await screen.findByText(/both use/);
    expect(notice.textContent).toContain('Bracket A');
    expect(notice.textContent).toContain('Pack B');
    expect(notice.textContent).toContain('Raceband R1 and Raceband R4');
    // Never a raw id and never a bare MHz.
    expect(notice.textContent).not.toContain('pack-b-z1x8');
    expect(notice.textContent).not.toContain('5658');
    // Both layers are listed and editable — the warning blocks nothing.
    const list = screen.getByRole('list', { name: 'Channel layers' });
    expect(within(list).getAllByRole('listitem')).toHaveLength(2);
    expect(
      (screen.getByRole('button', { name: '+ Add layer' }) as HTMLButtonElement).disabled
    ).toBe(false);
  });
});

describe('EventChannelLayers — editing an existing layer', () => {
  it('loads the layer’s tuning into the editor and replaces it wholesale', async () => {
    const seed = store(BRACKET_A);
    const updateChannelLayerImpl = vi.fn<typeof updateChannelLayer>(async () => seed.view);
    const { session } = makeTestSession({
      ...impls(seed, { updateChannelLayerImpl }),
      event: EVENT
    });
    render(EventChannelLayers, { session, timer: TIMER });

    await fireEvent.click(await screen.findByRole('button', { name: 'Edit' }));
    expect((await screen.findByLabelText('Layer name')).getAttribute('value') ?? '').toBeDefined();
    // The stored mapping is what the dropdowns show.
    expect((screen.getByLabelText('Channel for Node 1') as HTMLSelectElement).value).toBe('5658');
    expect((screen.getByLabelText('Channel for Node 4') as HTMLSelectElement).value).toBe('5769');

    await fireEvent.change(screen.getByLabelText('Channel for Node 4'), {
      target: { value: '5732' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Save layer' }));

    await waitFor(() => expect(updateChannelLayerImpl).toHaveBeenCalled());
    expect(updateChannelLayerImpl.mock.calls[0][2]).toBe('bracket-a-k3f9');
    expect(updateChannelLayerImpl.mock.calls[0][3]).toEqual({
      name: 'Bracket A',
      nodes: [
        { node: 0, channel: 5658 },
        { node: 1, channel: 5695 },
        { node: 3, channel: 5732 }
      ]
    });
  });
});
