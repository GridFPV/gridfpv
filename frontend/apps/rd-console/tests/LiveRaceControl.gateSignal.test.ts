/**
 * Race control's read-only gate signal (#415).
 *
 * The RD's problem: mid-race is exactly when they cannot tune and most need to know what the gate
 * is seeing. A lap that does not register looks identical on the board whether the craft is
 * producing no signal at all, crossing under the enter threshold, or not crossing — and the
 * information only surfaces at marshaling, after the heat is over.
 *
 * What these prove:
 *
 *  • **The feed is a LEASE, and this screen gives it back** — on unmount (which is also how the
 *    route leaves) and on `visibilitychange → hidden`. The RD walks to the gate with the phone in
 *    their pocket; a backgrounded tab must not leave a timer streaming to nobody.
 *  • **An unseen node renders as DEAD, not quiet.** It arrives with a full ring of zeroes, which
 *    plots as a flat trace along the floor — the exact picture of a live node over a quiet gate,
 *    which is one of the three states this screen exists to tell apart. It gets no plot.
 *  • **Crossing marks come from the STICKY flag.** `crossed_recently` survives the Director's
 *    decimation, so a fast pass between two samples still lights the mark; `crossing` alone misses
 *    exactly the passes an RD is squinting for.
 *  • **No raw seat ref reaches the screen** (CLAUDE.md) — a gate reads as a callsign, or as
 *    `Node 3 · Raceband R7`, never as `node-2`.
 *  • **Nothing offers a tuning control.** `RssiGraph` grows its draggable threshold handles only
 *    when a parent supplies `onthresholds`; this surface never does. #355 already refuses threshold
 *    writes during a scored heat and Race control must not imply otherwise.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import { tick } from 'svelte';
import type {
  ChannelCatalogEntry,
  EventMeta,
  HeatSummary,
  LiveRaceState,
  NodeSignal,
  Pilot,
  Timer,
  TimerSignal
} from '@gridfpv/types';

import LiveRaceControl from '../src/screens/LiveRaceControl.svelte';
import { makeTestSession } from './support.js';

const CATALOG: ChannelCatalogEntry[] = [
  { band: 'Raceband', channel: 'R7', mhz: 5880 },
  { band: 'Raceband', channel: 'R2', mhz: 5695 }
];

/** Collapse the whitespace a wrapped template introduces, so copy can be asserted as one line. */
const flat = (text: string | null): string => (text ?? '').replace(/\s+/g, ' ').trim();

const TIMER: Timer = {
  id: 'rh-1',
  name: 'Track RH',
  kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
  status: 'Connected',
  channel_capability: 'Flexible',
  node_count: 3,
  // Empty on every Flexible RotorHazard timer — the channels come from the heat and the heartbeat.
  available_channels: [],
  manual_connect: true,
  calibration: [],
  disabled_nodes: []
};

const EVENT: EventMeta = {
  id: 'e1',
  name: 'Friday',
  created_at: 0,
  persistent: true,
  timers: ['rh-1'],
  roster: ['alice', 'bob'],
  classes: []
};

const PILOTS: Pilot[] = [
  { id: 'alice', callsign: 'ACRO', vtx_types: [] },
  { id: 'bob', callsign: 'BOLT', vtx_types: [] }
];

/** A roster-seeded competition heat: the competitor ref IS the pilot id, and carries a channel. */
const HEAT: HeatSummary = {
  heat: 'heat-1',
  lineup: ['alice', 'bob'],
  frequencies: [
    ['alice', 5695],
    ['bob', 5880]
  ],
  phase: 'Running',
  is_current: true
};

const RUNNING: LiveRaceState = {
  current_heat: 'heat-1',
  phase: 'Running',
  active_pilots: ['alice', 'bob'],
  running_order: ['alice', 'bob'],
  progress: [
    { competitor: 'alice', laps_completed: 2, last_lap_micros: 21_000_000 },
    { competitor: 'bob', laps_completed: 2, last_lap_micros: 22_000_000 }
  ]
} as LiveRaceState;

/**
 * Three nodes: ACRO's gate (node 1, R2) quiet, BOLT's gate (node 0, R7) mid-pass by the STICKY
 * flag only, and node 2 never reported at all — arriving, as the Director really sends it, with a
 * full ring of ZEROES rather than an absent one.
 */
function snapshot(over: Partial<TimerSignal> = {}): TimerSignal {
  const nodes: NodeSignal[] = [
    {
      node: 0,
      seat: 'node-0',
      seen: true,
      frequency_mhz: 5880,
      rssi: 120,
      crossing: false,
      // The sticky flag: the craft was through the gate BETWEEN two samples.
      crossed_recently: true,
      enter_at: 90,
      exit_at: 80,
      samples: [40, 42, 44, 46, 48]
    },
    {
      node: 1,
      seat: 'node-1',
      seen: true,
      frequency_mhz: 5695,
      rssi: 51,
      crossing: false,
      crossed_recently: false,
      enter_at: 95,
      exit_at: 85,
      samples: [50, 51, 52, 53, 54]
    },
    {
      node: 2,
      seat: 'node-2',
      seen: false,
      crossing: false,
      crossed_recently: false,
      samples: [0, 0, 0, 0, 0]
    }
  ];
  return {
    timer: 'rh-1',
    streaming: true,
    lease_ms_remaining: 5_000,
    period_micros: 200_000,
    sample_micros: [0, 200_000, 400_000, 600_000, 800_000],
    nodes,
    ...over
  };
}

interface Harness {
  fetchSignal: ReturnType<typeof vi.fn>;
  stopSignal: ReturnType<typeof vi.fn>;
  session: ReturnType<typeof makeTestSession>['session'];
  unmount: () => void;
}

/**
 * Render Race control over a stubbed signal feed.
 *
 * `pollMs` defaults to ten minutes so the mount poll is the only one — a test about the lease
 * cadence lowers it, because a second poll is precisely what it is testing.
 */
async function renderControl(
  opts: {
    signal?: TimerSignal | null;
    pollMs?: number;
    live?: LiveRaceState;
    /** Whether to wait for the gate→pilot pairing before returning. */
    attributed?: boolean;
  } = {}
): Promise<Harness> {
  const feed = opts.signal === undefined ? snapshot() : opts.signal;
  // A fresh object per poll: the screen holds the snapshot in `$state.raw`, so handing back the
  // same reference would be indistinguishable from no new poll at all.
  const fetchSignal = vi.fn(async () => {
    if (feed === null) throw new Error('the Director did not answer');
    return structuredClone(feed);
  });
  const stopSignal = vi.fn(async () => {});
  const { session } = makeTestSession({
    live: opts.live ?? RUNNING,
    event: EVENT,
    listTimersImpl: async () => [TIMER],
    listChannelsImpl: async () => CATALOG,
    listPilotsImpl: async () => PILOTS,
    listHeatsImpl: async () => [HEAT]
  });
  const { unmount } = render(LiveRaceControl, {
    session,
    fetchSignal,
    stopSignal,
    signalPollMs: opts.pollMs ?? 10 * 60 * 1000
  });
  await waitFor(() => expect(fetchSignal).toHaveBeenCalled());
  // The pilot/heat/channel directory reads are async, and the gate→pilot pairing needs all three.
  // Waiting for the chips to name a pilot is waiting for the screen to be the one under test.
  if (opts.attributed ?? true) {
    await waitFor(() => expect(screen.getByTestId('gate-chip-1').textContent).toMatch(/ACRO/));
  }
  return { fetchSignal, stopSignal, session, unmount };
}

/**
 * Open a disclosure if it is not already open. Idempotent on purpose: the strip's open state is
 * PERSISTED per event, so a blind click would close a section a previous test left open.
 */
async function expand(name: RegExp): Promise<void> {
  const toggle = await screen.findByRole('button', { name });
  if (toggle.getAttribute('aria-expanded') !== 'true') await fireEvent.click(toggle);
  await tick();
}

/** Open the strip (collapsed by default) and, optionally, the secondary "other nodes" group. */
async function openStrip(others = false): Promise<void> {
  await expand(/Gate signal/);
  if (others) await expand(/Other nodes on this timer/);
}

beforeEach(() => {
  // The open/closed choice is persisted per event; each test starts from the shipped default.
  globalThis.localStorage?.clear();
});

afterEach(() => {
  vi.unstubAllGlobals();
  globalThis.localStorage?.clear();
});

describe('Race control gate signal — the feed is leased: hold it, then give it back', () => {
  it('subscribes on mount — the GET IS the subscription', async () => {
    const h = await renderControl();
    expect(h.fetchSignal).toHaveBeenCalledTimes(1);
    expect(h.fetchSignal.mock.calls[0][0]).toBe('rh-1');
    h.unmount();
  });

  it('keeps renewing the lease while Race control is on screen', async () => {
    // Every GET resets the Director's lease; stop calling and it tears the stream down. So the
    // cadence is not a refresh rate, it is the thing holding the feed open.
    const h = await renderControl({ pollMs: 15 });
    await waitFor(() => expect(h.fetchSignal.mock.calls.length).toBeGreaterThan(2));
    h.unmount();
  });

  it('RELEASES the lease on unmount — which is also how the route leaves Race control', async () => {
    const h = await renderControl();
    expect(h.stopSignal).not.toHaveBeenCalled();
    h.unmount();
    expect(h.stopSignal).toHaveBeenCalledWith('rh-1');
  });

  it('releases it when the tab is hidden, and re-subscribes when it comes back', async () => {
    // The RD walks to the gate with the phone in a pocket. A backgrounded tab must not leave a
    // timer parsing telemetry into a screen nobody is looking at.
    const h = await renderControl();
    expect(h.fetchSignal).toHaveBeenCalledTimes(1);

    const spy = vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden');
    document.dispatchEvent(new Event('visibilitychange'));
    await tick();
    expect(h.fetchSignal).toHaveBeenCalledTimes(1); // still one — nothing new while hidden
    expect(h.stopSignal).toHaveBeenCalledWith('rh-1');

    spy.mockReturnValue('visible');
    document.dispatchEvent(new Event('visibilitychange'));
    await waitFor(() => expect(h.fetchSignal).toHaveBeenCalledTimes(2));
    spy.mockRestore();
    h.unmount();
  });

  it('holds no lease for a READ-ONLY session — the Director would refuse it anyway', async () => {
    // `GET /timers/{id}/signal` is ControlAuth-gated. Subscribing from a pilot/read-only session
    // would earn a 401 rendered as "lost the timer's signal feed" — a false statement about the
    // hardware, in front of somebody who could not act on it either way.
    const fetchSignal = vi.fn(async () => snapshot());
    const { session } = makeTestSession({
      live: RUNNING,
      event: EVENT,
      role: 'readonly',
      listTimersImpl: async () => [TIMER],
      listChannelsImpl: async () => CATALOG,
      listPilotsImpl: async () => PILOTS,
      listHeatsImpl: async () => [HEAT]
    });
    render(LiveRaceControl, { session, fetchSignal, stopSignal: vi.fn() });
    await tick();
    await tick();
    expect(fetchSignal).not.toHaveBeenCalled();
    expect(screen.queryByRole('button', { name: /Gate signal/ })).toBeNull();
  });

  it('holds no lease at all when the event has no timer — nothing to watch', async () => {
    const fetchSignal = vi.fn(async () => snapshot());
    const { session } = makeTestSession({
      live: RUNNING,
      event: EVENT,
      // The registry never answers, so there is no primary timer to subscribe to.
      listTimersImpl: async () => [],
      listChannelsImpl: async () => CATALOG,
      listPilotsImpl: async () => PILOTS,
      listHeatsImpl: async () => [HEAT]
    });
    render(LiveRaceControl, { session, fetchSignal, stopSignal: vi.fn() });
    await tick();
    await tick();
    expect(fetchSignal).not.toHaveBeenCalled();
    expect(screen.queryByRole('button', { name: /Gate signal/ })).toBeNull();
  });
});

describe('Race control gate signal — an unseen node is dead, not quiet', () => {
  it('gives a node the timer has never heard from NO plot, and says why', async () => {
    const h = await renderControl();
    await openStrip(true);

    // Node 2 is reported with a full ring of zeroes. Plotting it would draw a flat live-looking
    // trace along the floor — the exact picture of a live node over a quiet gate.
    const dead = flat(screen.getByTestId('gate-dead-2').textContent);
    expect(dead).toMatch(/never heard from this node/i);
    expect(dead).toMatch(/Not a quiet gate — there is nothing there to be quiet/i);
    expect(within(screen.getByTestId('gate-2')).queryByRole('img')).toBeNull();

    // …and it is called out as such, not left to the flat line to imply.
    expect(within(screen.getByTestId('gate-2')).getByText('Not reporting')).toBeTruthy();
    h.unmount();
  });

  it('counts the unreported nodes in the collapsed header, before anything is opened', async () => {
    const h = await renderControl();
    expect(flat(screen.getByTestId('gate-chips-others-dead').textContent)).toMatch(
      /1 other node not reporting/
    );
    h.unmount();
  });

  it('still plots a node that IS reporting, quiet gate and all', async () => {
    const h = await renderControl();
    await openStrip();
    // ACRO's gate is quiet — a real trace, drawn, and distinguishable from the dead one above.
    expect(within(screen.getByTestId('gate-1')).getByRole('img')).toBeTruthy();
    expect(screen.queryByTestId('gate-dead-1')).toBeNull();
    h.unmount();
  });
});

describe('Race control gate signal — crossings come from the sticky flag', () => {
  it('marks a pass that happened BETWEEN samples (`crossed_recently`, not `crossing`)', async () => {
    const h = await renderControl();
    await openStrip();
    // node 0 has `crossing: false` at both sampled instants; only the sticky flag saw the pass.
    expect(within(screen.getByTestId('gate-0')).getByText('Crossed')).toBeTruthy();
    expect(screen.getByTestId('gate-chip-0').dataset.state).toBe('crossing');
    // node 1 saw nothing, and must not claim it did.
    expect(within(screen.getByTestId('gate-1')).queryByText('Crossed')).toBeNull();
    expect(screen.getByTestId('gate-chip-1').dataset.state).toBe('live');
    h.unmount();
  });

  it('says "In gate" while the craft is still inside it', async () => {
    const feed = snapshot();
    feed.nodes[0].crossing = true;
    const h = await renderControl({ signal: feed });
    await openStrip();
    expect(within(screen.getByTestId('gate-0')).getByText('In gate')).toBeTruthy();
    h.unmount();
  });
});

describe('Race control gate signal — friendly names only, and nothing that writes', () => {
  it('labels each gate with the CALLSIGN of the pilot it is timing', async () => {
    const h = await renderControl();
    await openStrip(true);
    // The heat's channel assignment against what each node reports it is tuned to: ACRO is on R2
    // (node 1), BOLT on R7 (node 0). Both through the ONE shared resolver (#416).
    expect(within(screen.getByTestId('gate-1')).getByRole('heading').textContent).toBe('ACRO');
    expect(within(screen.getByTestId('gate-0')).getByRole('heading').textContent).toBe('BOLT');
    // An unattributed node reads as its seat, never as a raw ref.
    expect(within(screen.getByTestId('gate-2')).getByRole('heading').textContent).toBe('Node 3');
    h.unmount();
  });

  it('never lets a raw `node-{i}` seat ref reach the screen', async () => {
    const h = await renderControl();
    await openStrip(true);
    expect(document.body.textContent ?? '').not.toMatch(/node-\d/);
    h.unmount();
  });

  it('offers NO tuning control — the threshold lines are drawn and untouchable', async () => {
    const h = await renderControl();
    await openStrip(true);
    const strip = screen.getByTestId('gate-grid').closest('section') as HTMLElement;
    // `RssiGraph` grows its draggable handles (role="slider") only when a parent supplies
    // `onthresholds`. This surface never does — that IS the read-only guarantee.
    expect(within(strip).queryAllByRole('slider')).toHaveLength(0);
    expect(within(strip).queryAllByRole('textbox')).toHaveLength(0);
    expect(within(strip).queryAllByRole('spinbutton')).toHaveLength(0);
    expect(within(strip).queryAllByRole('combobox')).toHaveLength(0);
    // …and it says so, rather than leaving the RD to discover it by trying.
    expect(screen.getByTestId('gate-readonly').textContent).toMatch(/never writes a threshold/i);
    h.unmount();
  });

  it('shows just the graph — none of the Tune page readout stack', async () => {
    const h = await renderControl();
    await openStrip(true);
    const strip = screen.getByTestId('gate-grid').closest('section') as HTMLElement;
    const text = strip.textContent ?? '';
    for (const readout of ['Node peak', 'Node nadir', 'Pass peak', 'Pass nadir', 'Passes']) {
      expect(text).not.toMatch(new RegExp(readout, 'i'));
    }
    h.unmount();
  });
});

describe('Race control gate signal — a toggle whose closed state still answers', () => {
  it('starts collapsed, with a live chip per gate and no plots mounted', async () => {
    const h = await renderControl();
    // Collapsed: the chips are the at-a-glance layer — is the timer hearing this gate at all?
    expect(screen.getByTestId('gate-chip-0')).toBeTruthy();
    expect(screen.getByTestId('gate-chip-1')).toBeTruthy();
    // …but eight live SVGs re-rendering behind a `hidden` region is work nobody can see.
    expect(within(screen.getByTestId('gate-0')).queryByRole('img')).toBeNull();
    h.unmount();
  });

  it('mounts the plots when the RD opens it', async () => {
    const h = await renderControl();
    await openStrip();
    expect(within(screen.getByTestId('gate-0')).getByRole('img')).toBeTruthy();
    h.unmount();
  });

  it('remembers the choice — an RD who wants it open gets it open on the next heat', async () => {
    // Persisted per event (`collapseStore`), which is what makes "collapsed by default" a default
    // rather than a fight: open it once at the start of the meeting and it stays open.
    const first = await renderControl();
    await openStrip();
    first.unmount();

    const second = await renderControl();
    expect(
      (await screen.findByRole('button', { name: /Gate signal/ })).getAttribute('aria-expanded')
    ).toBe('true');
    second.unmount();
  });

  it('keeps the leaderboard out of it — the graphs are not in the standing rows', async () => {
    // Decided by the RD: Race control is the highest-stakes screen and the leaderboard is what an
    // RD actually reads during a heat, so the graphs live in their own strip.
    const h = await renderControl();
    await openStrip();
    const standing = screen.getByText('Live standing').closest('section, article, div');
    expect(within(standing as HTMLElement).queryByTestId('gate-grid')).toBeNull();
    h.unmount();
  });
});

describe('Race control gate signal — no link is not no signal', () => {
  it('says NO LINK when the Director answers but nothing is feeding it', async () => {
    const h = await renderControl({ signal: snapshot({ streaming: false }) });
    await openStrip();
    expect(screen.getByText(/No link to this timer/i)).toBeTruthy();
    expect(flat(document.body.textContent)).toMatch(/nothing is arriving from Track RH/i);
    h.unmount();
  });

  it('says the FEED failed when the Director itself did not answer', async () => {
    // No snapshot at all, so nothing is attributed — there is nothing to attribute it to.
    const h = await renderControl({ signal: null, attributed: false });
    await openStrip();
    expect(flat(document.body.textContent)).toMatch(/Lost the timer.s signal feed/i);
    expect(flat(document.body.textContent)).toMatch(/the Director did not answer/i);
    h.unmount();
  });
});

describe('the lease survives the session re-polling its timer list', () => {
  // `session.timers` is re-read every TIMER_POLL_MS and assigned a FRESH array. `primaryTimer` is a
  // lookup over it, so the timer OBJECT changes identity on every poll while the id does not.
  //
  // The feed's `$effect` used to call `opts.timer()` inside itself, which made it depend on
  // everything that getter touched — so each poll re-ran the effect, fired its cleanup
  // (`POST /signal/stop`) and resubscribed. On the bench that read as the gate signal flapping
  // "connected → no link → connected" every 2.5 s with nothing actually wrong.
  it('does not release and resubscribe when only the timer object identity changes', async () => {
    const { stopSignal, session, unmount } = await renderControl();
    expect(stopSignal).not.toHaveBeenCalled();

    // Exactly what a poll does: same timer, same id, brand-new array and object.
    for (let i = 0; i < 3; i++) {
      session.timers = session.timers.map((t) => ({ ...t }));
      await tick();
    }

    expect(stopSignal).not.toHaveBeenCalled();
    unmount();
    // The release still happens when the watch genuinely ends.
    await waitFor(() => expect(stopSignal).toHaveBeenCalled());
  });
});
