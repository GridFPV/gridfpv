/**
 * The in-event **channel layout** editor (#117 S2).
 *
 * What is actually being defended here:
 *
 *  - **no raw id reaches the screen** — a node renders as `"Node 3"`, a channel as `"Raceband R3"`,
 *    a layout as its name. The dropdown's option *values* stay raw MHz (a wire handle); its labels
 *    may not;
 *  - the **global→event seam** — adding a layout with nothing picked sends no `nodes` at all, which
 *    is the Director's seed path (the allowed set laid onto the enabled nodes);
 *  - **only allowed channels, only enabled nodes** are offered, so the editor cannot even build the
 *    two refusals the Director exists to make;
 *  - a duplicate channel **blocks Save**, and a cross-layout overlap **does not** — it is a notice.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import { toasts } from '@gridfpv/components';
import type {
  createChannelLayout,
  rateChannels,
  updateChannelLayout
} from '@gridfpv/protocol-client';
import type {
  ChannelCatalogEntry,
  ChannelLayouts,
  EventMeta,
  ImdReading,
  Timer,
  TimerNodes
} from '@gridfpv/types';
import EventChannelLayouts from '../src/screens/EventChannelLayouts.svelte';
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

const BRACKET_A: ChannelLayouts = {
  layouts: [
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
 * The reads every test needs: the catalog, the node view and the layout list.
 *
 * The layout read is **stateful** on purpose. Every layout write re-homes `currentEvent`, which is
 * what keeps the console's cached meta honest — and that re-home makes the editor re-read. A
 * fixed-`[]` read would answer the re-read with "no layouts" and hide the write, which the real
 * Director never would. `seed` is the store the writes below mutate, exactly as the server's is.
 */
function impls(seed: { view: ChannelLayouts }, extra: Record<string, unknown> = {}) {
  return {
    listChannelsImpl: vi.fn(async () => CATALOG),
    timerNodesImpl: vi.fn(async () => NODES),
    listChannelLayoutsImpl: vi.fn(async () => seed.view),
    ...extra
  };
}

/** A store starting from `view`, shared by the stateful read and the writes in one test. */
function store(view: ChannelLayouts = { layouts: [], overlaps: [] }): { view: ChannelLayouts } {
  return { view };
}

beforeEach(() => toasts.clear());

describe('EventChannelLayouts — what the RD reads', () => {
  it('renders each node with its channel, never an index or a bare MHz', async () => {
    const { session } = makeTestSession({
      ...impls(store(BRACKET_A)),
      event: EVENT
    });
    render(EventChannelLayouts, { session, timer: TIMER });

    const list = await screen.findByRole('list', { name: 'Channel layouts' });
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
    render(EventChannelLayouts, { session, timer: TIMER });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add layout' }));

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
    render(EventChannelLayouts, { session, timer: UNCONFIGURED });

    const banner = await screen.findByRole('alert');
    expect(banner.textContent).toContain('Track RH');
    expect(banner.textContent).toContain('Timers page');
    // And there is nothing to add until that is fixed.
    expect(
      (screen.getByRole('button', { name: '+ Add layout' }) as HTMLButtonElement).disabled
    ).toBe(true);
  });
});

describe('EventChannelLayouts — defining a layout', () => {
  it('seeds from the global allowed set: adding with nothing picked sends no nodes', async () => {
    const seed = store();
    const createChannelLayoutImpl = vi.fn<typeof createChannelLayout>(
      async () => (seed.view = BRACKET_A)
    );
    const { session } = makeTestSession({
      ...impls(seed, { createChannelLayoutImpl }),
      event: EVENT
    });
    render(EventChannelLayouts, { session, timer: TIMER });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add layout' }));
    // The seed note is what tells the RD this is not an empty form.
    expect(screen.getByText(/starts from the channels/)).toBeTruthy();
    await fireEvent.click(screen.getByRole('button', { name: 'Add layout' }));

    await waitFor(() => expect(createChannelLayoutImpl).toHaveBeenCalled());
    const request = createChannelLayoutImpl.mock.calls[0][2];
    expect(request.name).toBe('Layout A');
    // The seam: no `nodes` at all, so the Director lays the allowed set onto the enabled nodes.
    expect(request.nodes).toBeUndefined();
    // And the stored layout comes back rendered by name.
    expect(await screen.findByText('Bracket A')).toBeTruthy();
  });

  it('sends the explicit mapping once the RD picks channels', async () => {
    const seed = store();
    const createChannelLayoutImpl = vi.fn<typeof createChannelLayout>(
      async () => (seed.view = BRACKET_A)
    );
    const { session } = makeTestSession({
      ...impls(seed, { createChannelLayoutImpl }),
      event: EVENT
    });
    render(EventChannelLayouts, { session, timer: TIMER });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add layout' }));
    await fireEvent.change(await screen.findByLabelText('Channel for Node 1'), {
      target: { value: '5658' }
    });
    await fireEvent.change(screen.getByLabelText('Channel for Node 2'), {
      target: { value: '5695' }
    });
    await fireEvent.change(screen.getByLabelText('Channel for Node 4'), {
      target: { value: '5769' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add layout' }));

    await waitFor(() => expect(createChannelLayoutImpl).toHaveBeenCalled());
    expect(createChannelLayoutImpl.mock.calls[0][2]).toEqual({
      name: 'Layout A',
      nodes: [
        { node: 0, channel: 5658 },
        { node: 1, channel: 5695 },
        { node: 3, channel: 5769 }
      ]
    });
  });

  it('refuses to save two nodes on one channel, naming both and the channel', async () => {
    const createChannelLayoutImpl = vi.fn<typeof createChannelLayout>(async () => BRACKET_A);
    const { session } = makeTestSession({
      ...impls(store(), { createChannelLayoutImpl }),
      event: EVENT
    });
    render(EventChannelLayouts, { session, timer: TIMER });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add layout' }));
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

    expect((screen.getByRole('button', { name: 'Add layout' }) as HTMLButtonElement).disabled).toBe(
      true
    );
    expect(createChannelLayoutImpl).not.toHaveBeenCalled();
  });

  it('will not save a half-tuned layout — a layout tunes every enabled node', async () => {
    const { session } = makeTestSession({ ...impls(store()), event: EVENT });
    render(EventChannelLayouts, { session, timer: TIMER });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add layout' }));
    await fireEvent.change(await screen.findByLabelText('Channel for Node 1'), {
      target: { value: '5658' }
    });

    const blocker = await screen.findByText(/tunes every enabled node/);
    expect(blocker.textContent).toContain('Node 2');
    expect(blocker.textContent).toContain('Node 4');
    expect((screen.getByRole('button', { name: 'Add layout' }) as HTMLButtonElement).disabled).toBe(
      true
    );
  });

  it('surfaces the Director’s own refusal sentence verbatim', async () => {
    const createChannelLayoutImpl = vi.fn<typeof createChannelLayout>(async () => {
      throw new Error('Node 2 and Node 3 are both on Raceband R1 in this layout.');
    });
    const { session } = makeTestSession({
      ...impls(store(), { createChannelLayoutImpl }),
      event: EVENT
    });
    render(EventChannelLayouts, { session, timer: TIMER });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add layout' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Add layout' }));

    // Surfaced VERBATIM — the Director's message already names the nodes and the channel by their
    // friendly names, so re-wording it could only make it worse.
    await waitFor(() => expect(toasts.items).toHaveLength(1));
    expect(toasts.items[0].tone).toBe('danger');
    expect(toasts.items[0].message).toBe(
      'Node 2 and Node 3 are both on Raceband R1 in this layout.'
    );
  });
});

describe('EventChannelLayouts — cross-layout overlap', () => {
  it('shows the overlap as a notice, with both layouts named and nothing blocked', async () => {
    const overlapping: ChannelLayouts = {
      layouts: [
        BRACKET_A.layouts[0],
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
      overlaps: [{ layout: 'bracket-a-k3f9', other: 'pack-b-z1x8', channels: [5658, 5769] }]
    };
    const { session } = makeTestSession({
      ...impls(store(overlapping)),
      event: EVENT
    });
    render(EventChannelLayouts, { session, timer: TIMER });

    const notice = await screen.findByText(/both use/);
    expect(notice.textContent).toContain('Bracket A');
    expect(notice.textContent).toContain('Pack B');
    expect(notice.textContent).toContain('Raceband R1 and Raceband R4');
    // Never a raw id and never a bare MHz.
    expect(notice.textContent).not.toContain('pack-b-z1x8');
    expect(notice.textContent).not.toContain('5658');
    // Both layouts are listed and editable — the warning blocks nothing.
    const list = screen.getByRole('list', { name: 'Channel layouts' });
    expect(within(list).getAllByRole('listitem')).toHaveLength(2);
    expect(
      (screen.getByRole('button', { name: '+ Add layout' }) as HTMLButtonElement).disabled
    ).toBe(false);
  });
});

describe('EventChannelLayouts — editing an existing layout', () => {
  it('loads the layout’s tuning into the editor and replaces it wholesale', async () => {
    const seed = store(BRACKET_A);
    const updateChannelLayoutImpl = vi.fn<typeof updateChannelLayout>(async () => seed.view);
    const { session } = makeTestSession({
      ...impls(seed, { updateChannelLayoutImpl }),
      event: EVENT
    });
    render(EventChannelLayouts, { session, timer: TIMER });

    await fireEvent.click(await screen.findByRole('button', { name: 'Edit' }));
    expect((await screen.findByLabelText('Layout name')).getAttribute('value') ?? '').toBeDefined();
    // The stored mapping is what the dropdowns show.
    expect((screen.getByLabelText('Channel for Node 1') as HTMLSelectElement).value).toBe('5658');
    expect((screen.getByLabelText('Channel for Node 4') as HTMLSelectElement).value).toBe('5769');

    await fireEvent.change(screen.getByLabelText('Channel for Node 4'), {
      target: { value: '5732' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Save layout' }));

    await waitFor(() => expect(updateChannelLayoutImpl).toHaveBeenCalled());
    expect(updateChannelLayoutImpl.mock.calls[0][2]).toBe('bracket-a-k3f9');
    expect(updateChannelLayoutImpl.mock.calls[0][3]).toEqual({
      name: 'Bracket A',
      nodes: [
        { node: 0, channel: 5658 },
        { node: 1, channel: 5695 },
        { node: 3, channel: 5732 }
      ]
    });
  });
});

describe('EventChannelLayouts — the IMD reading (#117 S4)', () => {
  /** RotorHazard's IMD6C reading, as the Director computes it. */
  const IMD6C: ImdReading = {
    rating: 29,
    worst: { doubled: 5732, subtracted: 5695, product: 5769, lands_on: 5769, gap_mhz: 0 }
  };
  /** Racebnd4 — nothing within 35 MHz, so nothing to name. */
  const CLEAN: ImdReading = { rating: 100 };

  /** Bracket A with a reading attached, the way the Director sends it back. */
  const RATED: ChannelLayouts = {
    ...BRACKET_A,
    ratings: [{ layout: 'bracket-a-k3f9', imd: IMD6C }]
  };

  it('shows a saved layout its rating and the worst offender, every channel named', async () => {
    const { session } = makeTestSession({ ...impls(store(RATED)), event: EVENT });
    render(EventChannelLayouts, { session, timer: TIMER });

    const list = await screen.findByRole('list', { name: 'Channel layouts' });
    const row = within(list).getByText('Bracket A').closest('li') as HTMLElement;
    await waitFor(() => expect(row.textContent).toContain('IMD 29'));
    // The two channels that mix and the one they land on, all by name.
    expect(row.textContent).toContain('Raceband R3');
    expect(row.textContent).toContain('Raceband R2');
    expect(row.textContent).toContain('lands on Raceband R4');
    // No raw frequency for any of them — only the product, which is arithmetic.
    expect(row.textContent).not.toContain('5732');
    expect(row.textContent).not.toContain('5695');
    expect(row.textContent).toContain('5769 MHz');
  });

  // ── The green/amber/red scale (#474) ───────────────────────────────────────────────────────

  it('colours the RATING and leaves the worst-offender sentence exactly as it was', async () => {
    const { session } = makeTestSession({ ...impls(store(RATED)), event: EVENT });
    render(EventChannelLayouts, { session, timer: TIMER });

    const list = await screen.findByRole('list', { name: 'Channel layouts' });
    const row = within(list).getByText('Bracket A').closest('li') as HTMLElement;
    await waitFor(() => expect(row.textContent).toContain('IMD 29'));

    // Bracket A flies three channels; 29 is well under what three pilots can reach, so it reads
    // red. The tone is a token name, never a colour — the stylesheet owns the palette.
    const reading = row.querySelector('.layout-imd') as HTMLElement;
    expect(reading.dataset.tone).toBe('danger');

    // The offender half carries no tone of its own: it names three real channels, and tinting it
    // would read as a verdict on those channels rather than on the set.
    const offender = reading.querySelector('.imd-offender') as HTMLElement;
    expect(offender.dataset.tone).toBeUndefined();
    expect(offender.textContent).toContain('worst offender:');
    expect(offender.textContent).toContain('lands on Raceband R4');

    // And the whole line still reads exactly as it did before the colour existed.
    expect(reading.textContent?.replace(/\s+/g, ' ').trim()).toBe(
      'IMD 29 · worst offender: 2 × Raceband R3 − Raceband R2 = 5769 MHz — lands on Raceband R4'
    );
  });

  it('explains the colour as guidance, against the layout’s own pilot count', async () => {
    const { session } = makeTestSession({ ...impls(store(RATED)), event: EVENT });
    render(EventChannelLayouts, { session, timer: TIMER });

    const list = await screen.findByRole('list', { name: 'Channel layouts' });
    const row = within(list).getByText('Bracket A').closest('li') as HTMLElement;
    await waitFor(() => expect(row.textContent).toContain('IMD 29'));

    const hint = (row.querySelector('.layout-imd') as HTMLElement).title;
    expect(hint).toMatch(/guidance only/i);
    // Bracket A tunes three nodes, and that is what it was judged against — "is 29 good?" has no
    // answer without a pilot count.
    expect(hint).toContain('3 channels flying at once');
    // Never a refusal: the reading has never blocked a save and the tooltip must not imply it does.
    expect(hint).toMatch(/blocks saving/i);
  });

  it('tones the LIVE reading too, as the RD picks', async () => {
    const rateChannelsImpl = vi.fn<typeof rateChannels>(async () => ({ rating: 67 }));
    const { session } = makeTestSession({
      ...impls(store(), { rateChannelsImpl }),
      event: EVENT
    });
    render(EventChannelLayouts, { session, timer: TIMER });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add layout' }));
    await fireEvent.change(await screen.findByLabelText('Channel for Node 1'), {
      target: { value: '5695' }
    });
    await fireEvent.change(screen.getByLabelText('Channel for Node 2'), {
      target: { value: '5732' }
    });

    const strip = screen.getByLabelText('IMD reading');
    await waitFor(() => expect(strip.textContent).toContain('IMD 67'));
    // Two channels can reach the ceiling easily, so 67 is "cleaner sets exist" rather than clean.
    const line = strip.querySelector('.imd-line') as HTMLElement;
    expect(line.dataset.tone).toBe('warn');
    expect(line.title).toMatch(/guidance only/i);
    // The caption that stops the number being read as a pass mark is still there beside it.
    expect(strip.textContent).toContain('What is achievable falls as you use more nodes');
  });

  it('reads live as the RD picks, asking the Director about the channel SET', async () => {
    const rateChannelsImpl = vi.fn<typeof rateChannels>(async () => IMD6C);
    const { session } = makeTestSession({
      ...impls(store(), { rateChannelsImpl }),
      event: EVENT
    });
    render(EventChannelLayouts, { session, timer: TIMER });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add layout' }));
    // One channel cannot interfere with anything, so nothing is asked yet.
    await fireEvent.change(await screen.findByLabelText('Channel for Node 1'), {
      target: { value: '5695' }
    });
    expect(rateChannelsImpl).not.toHaveBeenCalled();
    expect(screen.getByText(/Pick a second/)).toBeTruthy();

    // A second channel makes it a set worth asking about.
    await fireEvent.change(screen.getByLabelText('Channel for Node 2'), {
      target: { value: '5732' }
    });
    await waitFor(() => expect(rateChannelsImpl).toHaveBeenCalled());
    expect(rateChannelsImpl.mock.calls[0][1]).toEqual([5695, 5732]);

    const strip = screen.getByLabelText('IMD reading');
    await waitFor(() => expect(strip.textContent).toContain('IMD 29'));
    expect(strip.textContent).toContain('lands on Raceband R4');
    // The caption that stops the number being misread as a pass mark.
    expect(strip.textContent).toContain('What is achievable falls as you use more nodes');
  });

  it('never blocks a save on a poor rating, and never shows a verdict word', async () => {
    const seed = store();
    const rateChannelsImpl = vi.fn<typeof rateChannels>(async () => ({
      rating: -635,
      worst: { doubled: 5695, subtracted: 5658, product: 5732, lands_on: 5732, gap_mhz: 0 }
    }));
    const createChannelLayoutImpl = vi.fn<typeof createChannelLayout>(
      async () => (seed.view = RATED)
    );
    const { session } = makeTestSession({
      ...impls(seed, { rateChannelsImpl, createChannelLayoutImpl }),
      event: EVENT
    });
    render(EventChannelLayouts, { session, timer: TIMER });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add layout' }));
    for (const [node, mhz] of [
      ['Node 1', '5658'],
      ['Node 2', '5695'],
      ['Node 4', '5732']
    ] as const) {
      await fireEvent.change(screen.getByLabelText(`Channel for ${node}`), {
        target: { value: mhz }
      });
    }
    const strip = screen.getByLabelText('IMD reading');
    await waitFor(() => expect(strip.textContent).toContain('IMD \u2212635'));
    // The worst rating in FPV, and Save is still live: the RD may have no better option.
    const save = screen.getByRole('button', { name: 'Add layout' }) as HTMLButtonElement;
    expect(save.disabled).toBe(false);
    await fireEvent.click(save);
    await waitFor(() => expect(createChannelLayoutImpl).toHaveBeenCalled());
    // And it states rather than judges.
    for (const verdict of ['Poor', 'Marginal', 'Clean', 'Bad', 'Unusable']) {
      expect(strip.textContent).not.toContain(verdict);
    }
  });

  it('stops reading while two nodes share a channel — that is a different layout', async () => {
    const rateChannelsImpl = vi.fn<typeof rateChannels>(async () => CLEAN);
    const { session } = makeTestSession({
      ...impls(store(), { rateChannelsImpl }),
      event: EVENT
    });
    render(EventChannelLayouts, { session, timer: TIMER });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add layout' }));
    await fireEvent.change(await screen.findByLabelText('Channel for Node 1'), {
      target: { value: '5658' }
    });
    await fireEvent.change(screen.getByLabelText('Channel for Node 2'), {
      target: { value: '5658' }
    });
    const strip = screen.getByLabelText('IMD reading');
    await waitFor(() => expect(strip.textContent).toContain('Two nodes are on one channel'));
    // Rating the collapsed set would answer about a layout that is not on screen.
    expect(rateChannelsImpl).not.toHaveBeenCalled();
  });

  it('shows nothing when the reading cannot be had, and still lets the layout save', async () => {
    const seed = store();
    const rateChannelsImpl = vi.fn<typeof rateChannels>(async () => {
      throw new Error('GET /channels/imd failed: HTTP 503');
    });
    const createChannelLayoutImpl = vi.fn<typeof createChannelLayout>(
      async () => (seed.view = BRACKET_A)
    );
    const { session } = makeTestSession({
      ...impls(seed, { rateChannelsImpl, createChannelLayoutImpl }),
      event: EVENT
    });
    render(EventChannelLayouts, { session, timer: TIMER });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add layout' }));
    await fireEvent.change(await screen.findByLabelText('Channel for Node 1'), {
      target: { value: '5658' }
    });
    await fireEvent.change(screen.getByLabelText('Channel for Node 2'), {
      target: { value: '5695' }
    });
    const strip = screen.getByLabelText('IMD reading');
    await waitFor(() => expect(strip.textContent).toContain('does not affect saving'));

    await fireEvent.change(screen.getByLabelText('Channel for Node 4'), {
      target: { value: '5732' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add layout' }));
    await waitFor(() => expect(createChannelLayoutImpl).toHaveBeenCalled());
  });
});
