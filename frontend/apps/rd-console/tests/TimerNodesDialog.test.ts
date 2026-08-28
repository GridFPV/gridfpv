/**
 * The node-configuration dialog (#412) — the screen the bench bug had no answer for.
 *
 * The live case these drive: a real 4-node NuclearHazard configured as 8. The RD must be able to
 * *see* that (drift, with the phantom nodes named), fix it in one click ("follow the timer"), or
 * disable an individual dead node — and must never be shown a raw node index or a `node-{i}` seat
 * ref, because an off-by-one here puts a pilot on a dead gate.
 */
import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { HeatSummary, Timer, TimerNode, TimerNodes } from '@gridfpv/types';
import TimersPage from '../src/screens/TimersPage.svelte';
import { makeTestSession } from './support.js';

const noop = () => {};

/** The RD's bench timer: reports 4, configured 8 — the state #412 was filed for. */
const BENCH: Timer = {
  id: 'rh-1',
  name: 'Track RH',
  kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
  status: 'Connected',
  channel_capability: 'Flexible',
  node_count: 8,
  reported_nodes: 4,
  available_channels: [],
  manual_connect: false,
  calibration: [],
  disabled_nodes: []
};

function node(index: number, opts: { enabled?: boolean; reported?: boolean } = {}): TimerNode {
  return {
    node: index,
    label: `Node ${index + 1}`,
    seat: `node-${index}`,
    enabled: opts.enabled ?? true,
    reported: opts.reported ?? true
  };
}

/** `GET /timers/rh-1/nodes` for the bench timer: 8 wide, 8 enabled, only 4 of them real. */
const BENCH_VIEW: TimerNodes = {
  timer: 'rh-1',
  reported: 4,
  configured: 8,
  width: 8,
  nodes: [0, 1, 2, 3, 4, 5, 6, 7].map((i) => node(i, { reported: i < 4 })),
  enabled: [0, 1, 2, 3, 4, 5, 6, 7],
  drift: { reported: 4, configured: 8, enabled_beyond_reported: [4, 5, 6, 7] }
};

/** The repaired timer: following the hardware, four real nodes, all enabled. */
const FOLLOWED_VIEW: TimerNodes = {
  timer: 'rh-1',
  reported: 4,
  configured: undefined,
  width: 4,
  nodes: [0, 1, 2, 3].map((i) => node(i)),
  enabled: [0, 1, 2, 3],
  drift: undefined
};

/** Render the Timers page and open the node dialog on the bench timer. */
async function openNodes(opts: {
  timerNodesImpl?: ReturnType<typeof vi.fn>;
  setTimerNodesImpl?: ReturnType<typeof vi.fn>;
  listHeatsImpl?: ReturnType<typeof vi.fn>;
  timer?: Timer;
}) {
  const timer = opts.timer ?? BENCH;
  const listTimersImpl = vi.fn(async () => [timer]);
  const { session } = makeTestSession({
    listTimersImpl,
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    timerNodesImpl: opts.timerNodesImpl as any,
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    setTimerNodesImpl: opts.setTimerNodesImpl as any,
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    listHeatsImpl: (opts.listHeatsImpl ?? vi.fn(async () => [] as HeatSummary[])) as any
  });
  render(TimersPage, { session, onhome: noop });
  await screen.findByText('Track RH');
  const list = screen.getByRole('list', { name: 'Configured timers' });
  const row = within(list).getAllByRole('listitem')[0];
  await fireEvent.click(within(row).getByRole('button', { name: /nodes/i }));
  return { session, row };
}

describe('TimerNodesDialog (#412)', () => {
  it('shows the drift on the row itself, using the seat count and not a raw index', async () => {
    const listTimersImpl = vi.fn(async () => [BENCH]);
    const { session } = makeTestSession({ listTimersImpl });
    render(TimersPage, { session, onhome: noop });

    await screen.findByText('Track RH');
    const list = screen.getByRole('list', { name: 'Configured timers' });
    const row = within(list).getAllByRole('listitem')[0];
    // The row reads the configured width (the heat-size cap) and flags what the timer reported.
    expect(within(row).getByRole('button', { name: /8 nodes/ })).toBeInTheDocument();
    expect(within(row).getByText('Timer reports 4')).toBeInTheDocument();
  });

  it('shows NO drift badge on a timer the Director has never asked (#445)', async () => {
    // `reported_nodes` is `Option<u32>` with no `skip_serializing_if`, so a Mock — or any timer
    // that has not connected yet — arrives as `"reported_nodes": null`, not as an absent key. That
    // put a danger badge reading "Timer reports " (with nothing after it, `null` renders empty) on
    // every such row: an alarm about a disagreement between a number and nothing at all.
    //
    // Cast for the same reason the unit fixtures do: `#[ts(optional)]` types the field
    // `reported_nodes?: number`, which cannot express the `null` the wire actually sends.
    const neverAsked = { ...BENCH, reported_nodes: null } as unknown as Timer;
    const listTimersImpl = vi.fn(async () => [neverAsked]);
    const { session } = makeTestSession({ listTimersImpl });
    render(TimersPage, { session, onhome: noop });

    await screen.findByText('Track RH');
    const list = screen.getByRole('list', { name: 'Configured timers' });
    const row = within(list).getAllByRole('listitem')[0];
    expect(within(row).queryByText(/Timer reports/)).toBeNull();
    // The row still reads the width the RD pinned — the badge is the only thing that goes.
    expect(within(row).getByRole('button', { name: /8 nodes/ })).toBeInTheDocument();
  });

  it('renders reported alongside configured, and names the phantom nodes 1-based', async () => {
    const timerNodesImpl = vi.fn(async () => BENCH_VIEW);
    await openNodes({ timerNodesImpl });

    await waitFor(() => expect(timerNodesImpl).toHaveBeenCalledTimes(1));
    expect(timerNodesImpl).toHaveBeenCalledWith('http://d.local', 'rh-1', expect.anything());

    // Both values, side by side.
    expect(await screen.findByTestId('reported')).toHaveTextContent('4 nodes');
    expect(screen.getByTestId('configured')).toHaveTextContent('8 nodes');

    // The drift notice names the seats that would record nothing — 1-based, plainly.
    const notice = await screen.findByText(
      /This timer reports 4 nodes; GridFPV is configured for 8\./
    );
    expect(notice).toBeInTheDocument();
    expect(
      screen.getByText(/Node 5, Node 6, Node 7 and Node 8 are enabled but do not exist/)
    ).toBeInTheDocument();
    expect(screen.getByText(/record nothing/)).toBeInTheDocument();
  });

  it('labels every node 1-based and never leaks a raw index or seat ref', async () => {
    const timerNodesImpl = vi.fn(async () => BENCH_VIEW);
    await openNodes({ timerNodesImpl });

    const group = await screen.findByRole('group', { name: 'Enabled nodes' });
    // Node index 0 reads "Node 1"; index 7 reads "Node 8". There is no "Node 0".
    expect(within(group).getByLabelText('Node 1')).toBeInTheDocument();
    expect(within(group).getByLabelText('Node 8')).toBeInTheDocument();
    expect(within(group).queryByLabelText('Node 0')).toBeNull();
    expect(within(group).queryByLabelText('Node 9')).toBeNull();

    // Nothing anywhere on the screen prints a `node-{i}` seat ref or a bare 0-based index.
    const text = document.body.textContent ?? '';
    expect(text).not.toMatch(/node-\d/);
    expect(text).not.toMatch(/\bNode 0\b/);
  });

  it('“follow the timer” sends node_count: null and clears the drift', async () => {
    const timerNodesImpl = vi.fn(async () => BENCH_VIEW);
    const setTimerNodesImpl = vi.fn(async () => FOLLOWED_VIEW);
    await openNodes({ timerNodesImpl, setTimerNodesImpl });

    const follow = await screen.findByRole('button', { name: 'Follow the timer' });
    await fireEvent.click(follow);

    await waitFor(() => expect(setTimerNodesImpl).toHaveBeenCalledTimes(1));
    expect(setTimerNodesImpl).toHaveBeenCalledWith(
      'http://d.local',
      'rh-1',
      { node_count: null },
      'tok'
    );

    // The repaired view replaces the drift notice, and the control has nothing left to clear.
    await waitFor(() => expect(screen.getByTestId('configured')).toHaveTextContent('4 nodes'));
    expect(screen.getByTestId('configured')).toHaveTextContent('following the timer');
    expect(screen.queryByText(/GridFPV is configured for 8/)).toBeNull();
    expect(screen.queryByRole('button', { name: 'Follow the timer' })).toBeNull();
  });

  it('disabling one node sends the remaining enabled set — a hole, not a shorter prefix', async () => {
    // "reported is 4 but node 3 is busted, I need to use nodes 1, 2 and 4."
    const timerNodesImpl = vi.fn(async () => FOLLOWED_VIEW);
    const saved: TimerNodes = {
      ...FOLLOWED_VIEW,
      nodes: [0, 1, 2, 3].map((i) => node(i, { enabled: i !== 2 })),
      enabled: [0, 1, 3]
    };
    const setTimerNodesImpl = vi.fn(async () => saved);
    await openNodes({ timerNodesImpl, setTimerNodesImpl });

    // The RD unticks the node they call "Node 3" — wire index 2.
    const nodeThree = await screen.findByLabelText('Node 3');
    await fireEvent.click(nodeThree);

    await fireEvent.click(screen.getByRole('button', { name: 'Save nodes' }));
    await waitFor(() => expect(setTimerNodesImpl).toHaveBeenCalledTimes(1));
    expect(setTimerNodesImpl).toHaveBeenCalledWith(
      'http://d.local',
      'rh-1',
      { enabled: [0, 1, 3] },
      'tok'
    );

    await waitFor(() =>
      expect(screen.getByTestId('seat-summary')).toHaveTextContent(
        '3 pilots per heat (1 of 4 nodes disabled)'
      )
    );
  });

  it('surfaces the Director’s refusal verbatim instead of an HTTP line', async () => {
    const timerNodesImpl = vi.fn(async () => FOLLOWED_VIEW);
    const setTimerNodesImpl = vi.fn(async () => {
      throw new Error(
        'at least one node must stay enabled (a timer with none caps every heat to no pilots)'
      );
    });
    await openNodes({ timerNodesImpl, setTimerNodesImpl });

    // Untick three of four, leaving one — a legal edit the Director happens to refuse here.
    await fireEvent.click(await screen.findByLabelText('Node 2'));
    await fireEvent.click(screen.getByLabelText('Node 3'));
    await fireEvent.click(screen.getByRole('button', { name: 'Save nodes' }));

    await waitFor(() => expect(setTimerNodesImpl).toHaveBeenCalledTimes(1));
    expect(
      await screen.findByText(
        'at least one node must stay enabled (a timer with none caps every heat to no pilots)'
      )
    ).toBeInTheDocument();
  });

  it('refuses to send an edit that would leave no node enabled at all', async () => {
    const timerNodesImpl = vi.fn(async () => FOLLOWED_VIEW);
    const setTimerNodesImpl = vi.fn(async () => FOLLOWED_VIEW);
    await openNodes({ timerNodesImpl, setTimerNodesImpl });

    for (const label of ['Node 1', 'Node 2', 'Node 3', 'Node 4']) {
      await fireEvent.click(await screen.findByLabelText(label));
    }
    expect(await screen.findByText(/At least one node must stay enabled/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save nodes' })).toBeDisabled();
    expect(setTimerNodesImpl).not.toHaveBeenCalled();
  });

  it('warns when a scheduled heat would exceed the enabled set', async () => {
    const timerNodesImpl = vi.fn(async () => FOLLOWED_VIEW);
    const heats: HeatSummary[] = [
      {
        heat: 'h1',
        name: 'h1',
        lineup: ['p1', 'p2', 'p3', 'p4'],
        phase: 'Scheduled',
        is_current: false
      }
    ];
    const listHeatsImpl = vi.fn(async () => heats);
    await openNodes({ timerNodesImpl, listHeatsImpl });

    // Four enabled nodes, a four-pilot heat: it fits, so nothing is said.
    await screen.findByLabelText('Node 1');
    await waitFor(() => expect(listHeatsImpl).toHaveBeenCalled());
    expect(screen.queryByText(/would record nothing/)).toBeNull();

    // Untick one and the heat no longer fits — the warning reads against the PENDING set, before
    // the RD saves it, which is the only useful moment to say so.
    await fireEvent.click(screen.getByLabelText('Node 3'));
    expect(
      await screen.findByText(
        /1 scheduled heat is built for more pilots than this timer can time.*1 pilot in that heat would record nothing\./
      )
    ).toBeInTheDocument();
  });

  it('surfaces a failed read rather than rendering an empty node list', async () => {
    const timerNodesImpl = vi.fn(async () => {
      throw new Error('no timer with that id');
    });
    await openNodes({ timerNodesImpl });

    expect(await screen.findByText('no timer with that id')).toBeInTheDocument();
    expect(screen.queryByRole('group', { name: 'Enabled nodes' })).toBeNull();
  });
});
