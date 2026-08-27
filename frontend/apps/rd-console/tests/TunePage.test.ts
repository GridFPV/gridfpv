/**
 * TunePage (#355, slice 2b) — the per-timer gate-tuning page.
 *
 * What these prove, in the order the decisions were made:
 *
 *  • **One value, three editors.** The numeric box, the slider, and the draggable handle on the
 *    graph are three views of ONE number, in every direction. The user was emphatic about this, and
 *    the failure it guards against is subtle: three controls syncing pairwise drift, and a clamp
 *    applied per-control leaves the box on `90.4`, the slider on `90`, and the graph on a third
 *    position.
 *  • **No Apply button, but no write storm either.** An adjustment reaches the timer on its own;
 *    a drag emits dozens of values a second and must produce exactly ONE write, on release.
 *  • **The POLL is the confirmation.** `POST /calibration` only says "accepted"; RotorHazard does
 *    not echo a level set, so a write is confirmed by a later `GET /signal` showing the new level —
 *    and a write the hardware did not take must be visible on that node, never silent (#403).
 *  • **The practice-only gate is checked per write**, not once at load — a heat going Running while
 *    the RD is at the gate has to start refusing mid-tune.
 *  • **No raw seat, no bare frequency** on screen (CLAUDE.md).
 *  • **The feed is leased**, so the page holds it while it is on screen and gives it back —
 *    `signal/stop` — when it leaves: unmount, route change, or a hidden tab.
 *  • **`streaming: false` is not "no signal"**, and an unseen node is not a quiet one.
 *
 * ## The fixtures are the GENERATED wire shape, not a plausible one
 *
 * `snapshot()` below is typed as `TimerSignal` from `@gridfpv/types` with no casts, so every field
 * name is checked against the ts-rs bindings. The previous fixture was a hand-written guess in
 * which every field was misnamed, and this whole file passed green against it — on a page that
 * would have shown `undefined` at every readout in front of a live Director. Structural agreement
 * with the generated type is the only thing that makes these tests mean anything.
 */
import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import { tick } from 'svelte';
import type {
  ChannelCatalogEntry,
  ChannelDispatch,
  EventMeta,
  HeatSummary,
  LiveRaceState,
  NodeSignal,
  Pilot,
  RoundDef,
  Timer,
  TimerNodes,
  TimerSignal
} from '@gridfpv/types';
import TunePage from '../src/screens/TunePage.svelte';
import type { CalibrationRequest, CaptureDispatch, CaptureRequest } from '@gridfpv/protocol-client';
import type { SessionRole } from '../src/lib/session.svelte.js';
import { makeTestSession } from './support.js';

const CATALOG: ChannelCatalogEntry[] = [
  { band: 'Raceband', channel: 'R7', mhz: 5880 },
  { band: 'Raceband', channel: 'R2', mhz: 5695 },
  // Deliberately a channel that is in NEITHER the timer's `available_channels` pool nor on any
  // node: the channel dropdown must offer it anyway, because the pool is not the option source.
  { band: 'Fatshark', channel: 'F4', mhz: 5800 }
];

const TIMER: Timer = {
  id: 'rh-1',
  name: 'Track RH',
  kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
  status: 'Connected',
  channel_capability: 'Flexible',
  node_count: 2,
  available_channels: [5880, 5695],
  manual_connect: true,
  calibration: [],
  disabled_nodes: []
};

/**
 * Two nodes at rest: node 0 on Raceband R7 with 90/80, node 1 on R2 with 95/85.
 *
 * `sample_micros` is on the SNAPSHOT, once — the shared time base every node is sampled against —
 * and the per-node `samples` line up with it index for index. `streaming` and `lease_ms_remaining`
 * are the feed's own state, distinct from any node's.
 */
function snapshot(
  over: Partial<NodeSignal> = {},
  signalOver: Partial<TimerSignal> = {}
): TimerSignal {
  return {
    timer: 'rh-1',
    streaming: true,
    lease_ms_remaining: 5_000,
    period_micros: 200_000,
    sample_micros: [0, 200_000, 400_000, 600_000, 800_000],
    nodes: [
      {
        node: 0,
        seat: 'node-0',
        seen: true,
        frequency_mhz: 5880,
        rssi: 48,
        crossing: false,
        crossed_recently: false,
        enter_at: 90,
        exit_at: 80,
        node_peak_rssi: 132,
        node_nadir_rssi: 12,
        pass_peak_rssi: 118,
        pass_nadir_rssi: 41,
        pass_count: 7,
        samples: [40, 42, 44, 46, 48],
        ...over
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
      }
    ],
    ...signalOver
  };
}

const PRACTICE_ROUND: RoundDef = {
  id: 'r-practice',
  label: 'Practice',
  classes: [],
  format: 'open_practice',
  params: {},
  win_condition: { Timed: { window_micros: 120_000_000 } },
  seeding: 'FromRoster',
  channel_mode: 'Static',
  staging_timer_secs: 300,
  start_procedure: { mode: 'randomized-delay', min_delay_ms: 2000, max_delay_ms: 5000 },
  grace_window: { Duration: { micros: 3_000_000 } },
  protest_window: 'Off'
};
const QUAL_ROUND: RoundDef = {
  ...PRACTICE_ROUND,
  id: 'r-qual',
  label: 'Qualifying',
  format: 'timed_qual'
};

function eventWith(rounds: RoundDef[]): EventMeta {
  return {
    id: 'e1',
    name: 'Friday',
    created_at: 0,
    persistent: true,
    timers: ['rh-1'],
    roster: [],
    classes: [],
    rounds
  };
}

function heatOn(round: RoundDef): HeatSummary[] {
  return [
    {
      heat: 'heat-1',
      lineup: ['node-0', 'node-1'],
      round: round.id,
      phase: 'Running',
      is_current: true
    }
  ];
}

const RUNNING: LiveRaceState = { current_heat: 'heat-1', phase: 'Running' };

interface Harness {
  applyLevels: ReturnType<typeof vi.fn>;
  startCapture: ReturnType<typeof vi.fn>;
  applyChannel: ReturnType<typeof vi.fn>;
  fetchSignal: ReturnType<typeof vi.fn>;
  stopSignal: ReturnType<typeof vi.fn>;
  unmount: () => void;
}

/**
 * The Director's node view (#412): both nodes exist and are enabled unless a test says otherwise.
 *
 * Supplied explicitly because the channel control **fails closed** without it — RotorHazard drops
 * an out-of-range seat index with nothing but a log line, so a dropdown offered for a node that
 * does not exist would produce a write that looks accepted and lands nowhere.
 */
function nodeView(enabled: number[] = [0, 1]): TimerNodes {
  return {
    timer: 'rh-1',
    width: 2,
    enabled,
    nodes: [0, 1].map((node) => ({
      node,
      label: `Node ${node + 1}`,
      seat: `node-${node}`,
      enabled: enabled.includes(node),
      reported: true
    }))
  };
}

/**
 * Render the page over a stubbed signal feed + calibration write.
 *
 * The feed is a **mutable** snapshot the write path mutates, because that is how the real thing
 * works: `POST /calibration` answers "accepted" and the level itself only reappears on a later
 * `GET /signal`. `opts.takes` decides what the timer ends up holding — default, exactly what it was
 * sent; return `{}` to model a write RotorHazard silently ignored.
 *
 * `pollMs` defaults to ten minutes so the mount poll is the only one; a test about the confirmation
 * lowers it, because a second poll is precisely what it is testing.
 */
async function renderTune(
  opts: {
    signal?: TimerSignal;
    takes?: (body: CalibrationRequest) => { enter_at?: number; exit_at?: number };
    applyRejects?: Error;
    live?: LiveRaceState;
    event?: EventMeta;
    heats?: HeatSummary[];
    pilots?: Pilot[];
    role?: SessionRole;
    pollMs?: number;
    /** First poll answers as an opening subscription (empty, not yet streaming). */
    warmup?: boolean;
    confirmMs?: number;
    /** The Director's node view (#412); `null` models the read never landing (fails closed). */
    nodes?: TimerNodes | null;
    /** What the timer ends up tuned to after a channel write — default, exactly what it was sent. */
    tunes?: (mhz: number) => number | undefined;
    /** The Director's verdict on whether the node's thresholds are now stale. */
    staleThresholds?: boolean;
    channelRejects?: Error;
    /**
     * Holds the channel `POST` open until the test resolves it — the window where the write is
     * literally on the wire, as opposed to the far longer one where it is merely unconfirmed. The
     * default mock resolves immediately, which is the *unconfirmed* case and cannot exercise this.
     */
    holdChannelWrite?: Promise<void>;
    /** Override the timer under test (its capability / channel pool). */
    timer?: Timer;
    /**
     * What a **capture** (#355) ends up measuring, per threshold — the level the timer starts
     * reporting once its sampling window closes. `undefined` (the default) models a capture that
     * produced nothing: RotorHazard refuses one in complete silence, so "the level never changed"
     * is the only evidence of that there is.
     */
    captures?: (body: CaptureRequest) => number | undefined;
    /** The capture request itself is refused by the Director. */
    captureRejects?: Error;
  } = {}
): Promise<Harness> {
  const feed: TimerSignal = opts.signal ?? snapshot();
  // A fresh object per poll: the page holds the snapshot in `$state.raw`, so handing back the same
  // reference would be indistinguishable from no new poll at all.
  // `warmup` reproduces a REAL fresh subscription: the first GET *opens* the lease, so the Director
  // legitimately answers `streaming: false` with an empty ring, and the data only arrives on a
  // later poll. The harness used to hand back a populated snapshot on every call, which is why
  // "the page needs a manual refresh to show anything" could ship green.
  let polls = 0;
  const fetchSignal = vi.fn(async () => {
    polls += 1;
    if (opts.warmup && polls === 1) {
      return { ...structuredClone(feed), streaming: false, sample_micros: [], nodes: [] };
    }
    return structuredClone(feed);
  });
  const applyLevels = vi.fn(async (_timer: string, body: CalibrationRequest) => {
    if (opts.applyRejects) throw opts.applyRejects;
    const took = opts.takes?.(body) ?? { enter_at: body.enter_at, exit_at: body.exit_at };
    const target = feed.nodes.find((n) => n.node === body.node);
    if (target) {
      if (took.enter_at !== undefined) target.enter_at = took.enter_at;
      if (took.exit_at !== undefined) target.exit_at = took.exit_at;
    }
  });
  const stopSignal = vi.fn(async () => {});
  // The capture behaves like the real one: the Director answers with a DISPATCH carrying the window
  // it just opened, and the measured level (if there is one) only appears on a LATER poll — fed in
  // reality by RotorHazard's end-of-capture `node_enter_at_level` broadcast and the readback behind
  // it. The window/grace are milliseconds here for the same reason `confirmMs` is: the behaviour
  // under test is the sequence, not RotorHazard's three seconds.
  const startCapture = vi.fn(async (_timer: string, body: CaptureRequest) => {
    if (opts.captureRejects) throw opts.captureRejects;
    const target = feed.nodes.find((n) => n.node === body.node);
    const previous = target
      ? body.threshold === 'enter'
        ? target.enter_at
        : target.exit_at
      : undefined;
    const measured = opts.captures?.(body);
    if (target && measured !== undefined) {
      // Applied on a delay so the window is genuinely open for a moment — a capture that resolved
      // the instant it was pressed would never exercise the "fly the pass now" state at all.
      setTimeout(() => {
        if (body.threshold === 'enter') target.enter_at = measured;
        else target.exit_at = measured;
      }, 20);
    }
    return {
      timer: 'rh-1',
      node: body.node,
      threshold: body.threshold,
      window_ms: 40,
      settle_ms: 60,
      previous
    } satisfies CaptureDispatch;
  });
  // The channel write behaves like the real one: the Director answers with a DISPATCH, and the
  // channel itself only reappears on a later poll (RotorHazard's heartbeat carries it).
  const applyChannel = vi.fn(async (_timer: string, body: { node: number; mhz: number }) => {
    if (opts.holdChannelWrite) await opts.holdChannelWrite;
    if (opts.channelRejects) throw opts.channelRejects;
    const took = opts.tunes ? opts.tunes(body.mhz) : body.mhz;
    const target = feed.nodes.find((n) => n.node === body.node);
    if (target && took !== undefined) target.frequency_mhz = took;
    return {
      timer: 'rh-1',
      node: body.node,
      mhz: body.mhz,
      thresholds_tuned_on_another_channel: opts.staleThresholds ?? false
    } satisfies ChannelDispatch;
  });

  const { session } = makeTestSession({
    live: opts.live,
    event: opts.event,
    role: opts.role,
    listChannelsImpl: async () => CATALOG,
    listPilotsImpl: async () => opts.pilots ?? [],
    listHeatsImpl: async () => opts.heats ?? []
  });

  const view = opts.nodes === undefined ? nodeView() : opts.nodes;
  const { unmount } = render(TunePage, {
    session,
    timer: opts.timer ?? TIMER,
    onhome: () => {},
    ontimers: () => {},
    fetchSignal,
    applyLevels,
    startCapture,
    applyChannel,
    fetchNodes: async () => {
      if (view === null) throw new Error('the node view is unavailable');
      return view;
    },
    stopSignal,
    pollMs: opts.pollMs ?? 10 * 60 * 1000,
    confirmMs: opts.confirmMs ?? 150
  });

  // Wait for the first snapshot to seed the per-(node, threshold) state. Matched on the NODE only:
  // a seat's label carries its channel just when something knows it, and #117 S3 removed the
  // `available_channels[node]` fabrication that used to make one up — so a node whose heartbeat has
  // not reported a frequency is now honestly labelled "Node 1" alone.
  await screen.findByLabelText(/^Enter at level for Node 1/);
  return { applyLevels, startCapture, applyChannel, fetchSignal, stopSignal, unmount };
}

const box = (node = 1, th = 'Enter at') =>
  screen.getByLabelText(
    `${th} level for Node ${node} · Raceband ${node === 1 ? 'R7' : 'R2'}`
  ) as HTMLInputElement;
const slider = (node = 1, th = 'Enter at') =>
  screen.getByLabelText(
    `${th} slider for Node ${node} · Raceband ${node === 1 ? 'R7' : 'R2'}`
  ) as HTMLInputElement;
/** The graph's draggable handle for a threshold — `aria-valuenow` is the graph's view of the value. */
const handle = (which: 'Enter' | 'Exit', name = 'Node 1 · Raceband R7') =>
  screen.getByLabelText(`${which} threshold for ${name}`);
const graphValue = (which: 'Enter' | 'Exit', name?: string) =>
  Number(handle(which, name).getAttribute('aria-valuenow'));

describe('TunePage — one value, three editors', () => {
  it('seeds all three editors from the level the timer reports', async () => {
    const h = await renderTune();
    expect(box().value).toBe('90');
    expect(slider().value).toBe('90');
    expect(graphValue('Enter')).toBe(90);
    h.unmount();
  });

  it('typing in the box moves the slider AND the graph handle', async () => {
    const h = await renderTune();
    await fireEvent.input(box(), { target: { value: '120' } });
    await tick();
    expect(slider().value).toBe('120');
    expect(graphValue('Enter')).toBe(120);
    h.unmount();
  });

  it('moving the slider moves the box AND the graph handle', async () => {
    const h = await renderTune();
    await fireEvent.input(slider(), { target: { value: '133' } });
    await tick();
    expect(box().value).toBe('133');
    expect(graphValue('Enter')).toBe(133);
    h.unmount();
  });

  it('nudging the graph handle moves the box AND the slider', async () => {
    // The graph's keyboard nudge is the same emit path a pointer drag uses — one value out, one
    // state in. Driving it by key avoids mocking SVG geometry to prove the binding.
    const h = await renderTune();
    await fireEvent.keyDown(handle('Enter'), { key: 'ArrowUp' });
    await tick();
    expect(box().value).toBe('91');
    expect(slider().value).toBe('91');
    expect(graphValue('Enter')).toBe(91);
    h.unmount();
  });

  it('keeps enter and exit independent, and keeps the nodes independent', async () => {
    const h = await renderTune();
    await fireEvent.input(box(1, 'Enter at'), { target: { value: '120' } });
    await tick();
    // The other threshold on the same node, and the same threshold on the other node, are untouched.
    expect(box(1, 'Exit at').value).toBe('80');
    expect(box(2, 'Enter at').value).toBe('95');
    expect(graphValue('Exit')).toBe(80);
    expect(graphValue('Enter', 'Node 2 · Raceband R2')).toBe(95);
    h.unmount();
  });

  it('clamps ONCE, at the state — all three views land on the same clamped number', async () => {
    const h = await renderTune();
    await fireEvent.input(box(), { target: { value: '0' } });
    await tick();
    // RH reads a 0 as "read it back off the node", so it must never reach the wire.
    expect(box().value).toBe('1');
    expect(slider().value).toBe('1');
    expect(graphValue('Enter')).toBe(1);

    await fireEvent.input(box(), { target: { value: '9999' } });
    await tick();
    // 254, not 255: RH's `is_valid_rssi` is a STRICT `< 255`, so a literal 255 is dropped before
    // the detector and then confirmed anyway off the profile row — "On timer" for a value that is
    // not on the timer. It must never reach the wire either.
    expect(box().value).toBe('254');
    expect(slider().value).toBe('254');
    expect(graphValue('Enter')).toBe(254);
    h.unmount();
  });

  it('leaves the state alone while the box is empty mid-typing, and re-syncs on blur', async () => {
    const h = await renderTune();
    await fireEvent.input(box(), { target: { value: '' } });
    await tick();
    // An emptied box must not slam the shared value to the minimum under the RD's fingers…
    expect(slider().value).toBe('90');
    expect(graphValue('Enter')).toBe(90);
    // …and the box must not be left as a lingering third view either.
    await fireEvent.blur(box());
    await tick();
    expect(box().value).toBe('90');
    h.unmount();
  });
});

describe('TunePage — write cadence (no Apply button, no write storm)', () => {
  it('writes NOTHING while the slider is being dragged, then exactly once on release', async () => {
    const h = await renderTune();
    await fireEvent.pointerDown(slider());
    for (const v of ['95', '100', '105', '110']) {
      await fireEvent.input(slider(), { target: { value: v } });
    }
    await tick();
    // Each write also costs a readback, on the socket that carries lap ingest. Mid-drag writes buy
    // nothing: the crossing band the RD is watching is drawn client-side from the pending value.
    expect(h.applyLevels).not.toHaveBeenCalled();

    await fireEvent.pointerUp(slider());
    await waitFor(() => expect(h.applyLevels).toHaveBeenCalledTimes(1));
    expect(h.applyLevels).toHaveBeenCalledWith('rh-1', { node: 0, enter_at: 110 });
    h.unmount();
  });

  it('writes nothing while the graph handle is nudged, then once when the key is released', async () => {
    const h = await renderTune();
    const knob = handle('Enter');
    await fireEvent.keyDown(knob, { key: 'ArrowUp' });
    await fireEvent.keyDown(knob, { key: 'ArrowUp' });
    await tick();
    expect(h.applyLevels).not.toHaveBeenCalled();

    await fireEvent.keyUp(knob, { key: 'ArrowUp' });
    await waitFor(() => expect(h.applyLevels).toHaveBeenCalledTimes(1));
    expect(h.applyLevels).toHaveBeenCalledWith('rh-1', { node: 0, enter_at: 92 });
    h.unmount();
  });

  it('does not let a PAUSED drag slip a write past the pointerup rule', async () => {
    // The typing-idle net (~300ms) exists for the numeric box. A drag says when it is finished, so
    // while the pointer is down the net stays disarmed — a slow drag that pauses to look at the
    // gate must not fire one write mid-drag and another on release.
    const h = await renderTune();
    await fireEvent.pointerDown(slider());
    await fireEvent.input(slider(), { target: { value: '110' } });
    await new Promise((r) => setTimeout(r, 400));
    expect(h.applyLevels).not.toHaveBeenCalled();

    await fireEvent.pointerUp(slider());
    await waitFor(() => expect(h.applyLevels).toHaveBeenCalledTimes(1));
    h.unmount();
  });

  it('writes after a short pause when the RD types a value and looks away without leaving the box', async () => {
    const h = await renderTune();
    await fireEvent.input(box(), { target: { value: '112' } });
    await waitFor(() => expect(h.applyLevels).toHaveBeenCalledTimes(1), { timeout: 2000 });
    expect(h.applyLevels).toHaveBeenCalledWith('rh-1', { node: 0, enter_at: 112 });
    h.unmount();
  });

  it('sends only the threshold that changed', async () => {
    const h = await renderTune();
    await fireEvent.input(box(1, 'Exit at'), { target: { value: '70' } });
    await fireEvent.blur(box(1, 'Exit at'));
    await waitFor(() => expect(h.applyLevels).toHaveBeenCalledTimes(1));
    expect(h.applyLevels).toHaveBeenCalledWith('rh-1', { node: 0, exit_at: 70 });
    h.unmount();
  });

  it('does not write when the value ends up back where the timer already had it', async () => {
    const h = await renderTune();
    await fireEvent.pointerDown(slider());
    await fireEvent.input(slider(), { target: { value: '120' } });
    await fireEvent.input(slider(), { target: { value: '90' } });
    await fireEvent.pointerUp(slider());
    await tick();
    expect(h.applyLevels).not.toHaveBeenCalled();
    h.unmount();
  });

  it('writes on Enter without waiting for a blur', async () => {
    const h = await renderTune();
    await fireEvent.input(box(), { target: { value: '111' } });
    await fireEvent.keyDown(box(), { key: 'Enter' });
    await waitFor(() => expect(h.applyLevels).toHaveBeenCalledTimes(1));
    expect(h.applyLevels).toHaveBeenCalledWith('rh-1', { node: 0, enter_at: 111 });
    h.unmount();
  });
});

describe('TunePage — the POLL is the confirmation', () => {
  it('shows Adjusting → Sending… → On timer, settling only when a POLL sees the level', async () => {
    // `POST /calibration` answers "accepted" and nothing else. RotorHazard broadcasts
    // `enter_and_exit_at_levels`, which comes back as `NodeSignal.enter_at` on a LATER `GET
    // /signal` — so `Sending…` is not waiting on the response, it is waiting on the feed.
    const h = await renderTune({ pollMs: 15 });
    const threshold = () => screen.getByTestId('threshold-0-enter');
    expect(within(threshold()).getByText('On timer')).toBeInTheDocument();

    await fireEvent.input(slider(), { target: { value: '110' } });
    await tick();
    expect(within(threshold()).getByText('Adjusting')).toBeInTheDocument();

    await fireEvent.pointerUp(slider());
    await waitFor(() => expect(h.applyLevels).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(within(threshold()).getByText('On timer')).toBeInTheDocument());
    h.unmount();
  });

  it('stays Sending… while the write is accepted but the polls have not shown it yet', async () => {
    // The write is accepted immediately; the feed keeps reporting the OLD level. Until the timeout
    // that is not a failure — the change may still be in flight — so it must not read as settled.
    const h = await renderTune({ pollMs: 15, confirmMs: 5_000, takes: () => ({}) });
    await fireEvent.input(slider(), { target: { value: '110' } });
    await fireEvent.pointerUp(slider());

    const threshold = () => screen.getByTestId('threshold-0-enter');
    await waitFor(() => expect(within(threshold()).getByText('Sending…')).toBeInTheDocument());
    await new Promise((r) => setTimeout(r, 60));
    expect(within(threshold()).getByText('Sending…')).toBeInTheDocument();
    h.unmount();
  });

  it('says so on the node when the polls keep showing the OLD level', async () => {
    // RotorHazard does not echo the set, so a silent divergence would leave the RD tuning against
    // a level the timer never held — the #403 failure class.
    const h = await renderTune({ pollMs: 15, takes: () => ({}) });
    await fireEvent.input(slider(), { target: { value: '110' } });
    await fireEvent.pointerUp(slider());

    const threshold = () => screen.getByTestId('threshold-0-enter');
    await waitFor(() => expect(within(threshold()).getByText('Not taken')).toBeInTheDocument());
    expect(within(threshold()).getByText(/reports 90, not 110/)).toBeInTheDocument();
    h.unmount();
  });

  it('gives a verdict even when the poll itself has stopped answering', async () => {
    // The backstop: with no further polls there is no confirmation coming, and a threshold left
    // reading `Sending…` for ever is the silent failure this page exists to prevent.
    const h = await renderTune({ pollMs: 10 * 60 * 1000, takes: () => ({}) });
    await fireEvent.input(slider(), { target: { value: '110' } });
    await fireEvent.pointerUp(slider());

    const threshold = () => screen.getByTestId('threshold-0-enter');
    await waitFor(() => expect(within(threshold()).getByText('Not taken')).toBeInTheDocument());
    expect(h.fetchSignal).toHaveBeenCalledTimes(1);
    h.unmount();
  });

  it('says so on the node when the write never lands at all', async () => {
    const h = await renderTune({ applyRejects: new Error('The timer refused the change (503).') });
    await fireEvent.input(slider(), { target: { value: '110' } });
    await fireEvent.pointerUp(slider());

    const threshold = () => screen.getByTestId('threshold-0-enter');
    await waitFor(() => expect(within(threshold()).getByText('Failed')).toBeInTheDocument());
    expect(within(threshold()).getByText(/503/)).toBeInTheDocument();
    h.unmount();
  });

  it('lets the hardware reclaim a threshold at rest — the RD tuned in RotorHazard’s own UI', async () => {
    const feed = snapshot();
    const h = await renderTune({ signal: feed, pollMs: 15 });
    expect(box().value).toBe('90');
    feed.nodes[0].enter_at = 104;
    await waitFor(() => expect(box().value).toBe('104'));
    h.unmount();
  });
});

describe('TunePage — the practice-only gate, per write', () => {
  it('allows a write while an OPEN PRACTICE heat is running', async () => {
    const h = await renderTune({
      live: RUNNING,
      event: eventWith([PRACTICE_ROUND]),
      heats: heatOn(PRACTICE_ROUND)
    });
    await fireEvent.input(slider(), { target: { value: '110' } });
    await fireEvent.pointerUp(slider());
    await waitFor(() => expect(h.applyLevels).toHaveBeenCalledTimes(1));
    h.unmount();
  });

  it('REFUSES a write while a competition heat is running, and says why', async () => {
    const h = await renderTune({
      live: RUNNING,
      event: eventWith([QUAL_ROUND]),
      heats: heatOn(QUAL_ROUND)
    });
    // The gate is stated up front as well as enforced per write.
    await waitFor(() =>
      expect(screen.getByText(/competition heat is running/i)).toBeInTheDocument()
    );

    await fireEvent.input(slider(), { target: { value: '110' } });
    await fireEvent.pointerUp(slider());
    await tick();
    expect(h.applyLevels).not.toHaveBeenCalled();

    const threshold = screen.getByTestId('threshold-0-enter');
    await waitFor(() => expect(within(threshold).getByText('Not sent')).toBeInTheDocument());
    h.unmount();
  });

  it('fails closed: a running heat whose round cannot be resolved is treated as competition', async () => {
    const h = await renderTune({ live: RUNNING, event: eventWith([]), heats: [] });
    await fireEvent.input(slider(), { target: { value: '110' } });
    await fireEvent.pointerUp(slider());
    await tick();
    expect(h.applyLevels).not.toHaveBeenCalled();
    h.unmount();
  });
});

describe('TunePage — names (CLAUDE.md)', () => {
  it('labels each node "Node N · <band+channel>" and never leaks a raw seat or bare frequency', async () => {
    const h = await renderTune();
    expect(screen.getByRole('heading', { name: 'Node 1 · Raceband R7' })).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Node 2 · Raceband R2' })).toBeInTheDocument();
    expect(screen.queryByText(/node-0/)).toBeNull();
    expect(screen.queryByText(/node-1/)).toBeNull();
    // A frequency may appear in the channel PICKER — see below — but never as a heading, where it
    // would be the raw handle standing in for the name.
    for (const heading of screen.getAllByRole('heading')) {
      expect(heading.textContent ?? '').not.toMatch(/\b5\d{3}\b/);
    }
    h.unmount();
  });

  it('resolves a staged seat to its pilot callsign through the shared resolver', async () => {
    const h = await renderTune({
      live: {
        current_heat: 'heat-1',
        phase: 'Staged',
        progress: [{ competitor: 'node-0', pilot: 'p1', laps_completed: 0 }]
      },
      pilots: [{ id: 'p1', callsign: 'Maverick', vtx_types: [] }]
    });
    // Twice: the column header and the graph's own trace label, both through the same resolver.
    await waitFor(() => expect(screen.getAllByText('Maverick')).toHaveLength(2));
    // The seat's own identity stays on screen too — the RD is tuning a gate, not a pilot.
    expect(screen.getByRole('heading', { name: 'Node 1 · Raceband R7' })).toBeInTheDocument();
    h.unmount();
  });
});

describe('TunePage — readouts', () => {
  it('renders the six node_data stats per node', async () => {
    const h = await renderTune();
    expect(within(screen.getByTestId('readout-0-rssi')).getByText('48')).toBeInTheDocument();
    expect(within(screen.getByTestId('readout-0-node-peak')).getByText('132')).toBeInTheDocument();
    expect(within(screen.getByTestId('readout-0-node-nadir')).getByText('12')).toBeInTheDocument();
    expect(within(screen.getByTestId('readout-0-pass-peak')).getByText('118')).toBeInTheDocument();
    expect(within(screen.getByTestId('readout-0-pass-nadir')).getByText('41')).toBeInTheDocument();
    expect(within(screen.getByTestId('readout-0-pass-count')).getByText('7')).toBeInTheDocument();
    h.unmount();
  });

  it('dashes a node whose stats the timer has not reported', async () => {
    // Node 1 in the fixture carries no node_data — six dashes beats six misleading zeroes.
    const h = await renderTune();
    expect(within(screen.getByTestId('readout-1-node-peak')).getByText('—')).toBeInTheDocument();
    h.unmount();
  });
});

describe('TunePage — an unseen node is DEAD, not quiet', () => {
  /** A snapshot whose node 1 RotorHazard has never reported — zero-filled samples and all. */
  const withDeadNode = () => {
    const feed = snapshot();
    feed.nodes[1] = {
      node: 1,
      seat: 'node-1',
      seen: false,
      crossing: false,
      crossed_recently: false,
      // The Director samples every node on the same pass and fills an unreported one's slot with
      // 0.0 — so a dead node arrives carrying a full, perfectly plottable ring of zeroes.
      samples: [0, 0, 0, 0, 0]
    };
    return feed;
  };

  it('says the node is not reporting, instead of drawing its zeroes as a trace', async () => {
    // Plotted, that ring is a flat line along the floor — indistinguishable from a live node over
    // an empty gate. Telling those two apart is the entire reason an RD opens this page.
    const h = await renderTune({ signal: withDeadNode() });
    expect(screen.getByTestId('node-dead-1')).toBeInTheDocument();
    expect(screen.getByText('Not reporting')).toBeInTheDocument();
    // Node 0 still plots; only the dead one loses its graph.
    const graphs = document.querySelectorAll('[aria-label="RSSI signal graph"]');
    expect(graphs.length).toBe(1);
    h.unmount();
  });

  it('offers no thresholds to write to a node that is not there', async () => {
    const h = await renderTune({ signal: withDeadNode() });
    expect(screen.queryByTestId('threshold-1-enter')).toBeNull();
    expect(screen.getByTestId('threshold-0-enter')).toBeInTheDocument();
    h.unmount();
  });

  it('still keeps the node in the layout — "is this node even alive?" needs an answer', async () => {
    // Unseated nodes are in the snapshot deliberately: filtering them out would answer the RD's
    // question with silence for exactly the node they are checking.
    const h = await renderTune({ signal: withDeadNode() });
    // Labelled by the node alone: a node that is not reporting has not said what it is tuned to,
    // and #117 S3 stopped inventing an answer from the timer's allowed set. Unknown, not "none".
    expect(screen.getByRole('heading', { name: 'Node 2' })).toBeInTheDocument();
    expect(within(screen.getByTestId('readout-1-rssi')).getByText('—')).toBeInTheDocument();
    h.unmount();
  });
});

describe('TunePage — "no signal" is not "no link"', () => {
  it('reports a live feed as live', async () => {
    const h = await renderTune();
    expect(within(screen.getByTestId('feed-status')).getByText('Feed live')).toBeInTheDocument();
    expect(screen.queryByText(/No link to this timer/)).toBeNull();
    h.unmount();
  });

  it('says NO LINK when the Director answers but nothing is feeding it', async () => {
    // `streaming: false` on a perfectly valid snapshot means the timer is not connected (or has
    // just dropped) — a different fault from a live feed over a quiet gate, with a different fix.
    const h = await renderTune({ signal: snapshot({}, { streaming: false }) });
    await waitFor(() => expect(screen.getByText(/No link to this timer/)).toBeInTheDocument());
    expect(within(screen.getByTestId('feed-status')).getByText('No link')).toBeInTheDocument();
    h.unmount();
  });

  it('distinguishes that from a feed that failed outright', async () => {
    // The Director itself did not answer: nothing on the page is current, which is a louder thing.
    const { session } = makeTestSession({
      listChannelsImpl: async () => CATALOG,
      listPilotsImpl: async () => [],
      listHeatsImpl: async () => []
    });
    const { unmount } = render(TunePage, {
      session,
      timer: TIMER,
      onhome: () => {},
      ontimers: () => {},
      fetchSignal: vi.fn(async () => {
        throw new Error('Track RH is not answering.');
      }),
      applyLevels: vi.fn(),
      stopSignal: vi.fn(async () => {}),
      pollMs: 10 * 60 * 1000
    });
    await waitFor(() =>
      expect(screen.getByText(/Lost the timer's signal feed/)).toBeInTheDocument()
    );
    expect(screen.queryByText(/No link to this timer/)).toBeNull();
    unmount();
  });
});

describe('TunePage — control authority', () => {
  it('disables every editor for a read-only session, and says why', async () => {
    // `POST /calibration` is ControlAuth-gated: a read-only session's every adjustment would come
    // back 401, which is a different thing from a broken gate. Better to disable up front than to
    // let the RD drag a slider that cannot possibly apply.
    const h = await renderTune({ role: 'readonly' });
    expect(screen.getByText(/Read-only session/)).toBeInTheDocument();
    expect(box().disabled).toBe(true);
    expect(slider().disabled).toBe(true);
    h.unmount();
  });

  it('writes nothing even if an adjustment gets through anyway', async () => {
    const h = await renderTune({ role: 'readonly' });
    await fireEvent.input(slider(), { target: { value: '110' } });
    await fireEvent.pointerUp(slider());
    await tick();
    expect(h.applyLevels).not.toHaveBeenCalled();
    h.unmount();
  });
});

describe('TunePage — the feed is leased: hold it, then give it back', () => {
  it('polls once on mount — the call IS the subscription', async () => {
    const h = await renderTune();
    expect(h.fetchSignal).toHaveBeenCalledTimes(1);
    h.unmount();
  });

  it('keeps renewing the lease while the page is on screen', async () => {
    // Every GET resets the Director's lease; stop calling and it tears the stream down. So the
    // cadence is not a refresh rate, it is the thing holding the feed open.
    const h = await renderTune({ pollMs: 15 });
    await waitFor(() => expect(h.fetchSignal.mock.calls.length).toBeGreaterThan(2));
    h.unmount();
  });

  it('STOPS the stream when the page unmounts — which is also how the route leaves', async () => {
    // The shell swaps TunePage out on a hash change, so this cleanup is what releases the feed when
    // the RD navigates away. Without it the timer keeps streaming to nobody until the lease lapses.
    const h = await renderTune();
    expect(h.stopSignal).not.toHaveBeenCalled();
    h.unmount();
    expect(h.stopSignal).toHaveBeenCalledWith('rh-1');
  });

  it('stops the stream when the tab is hidden, and re-subscribes when it comes back', async () => {
    // The RD walks to the gate with the phone in a pocket. A backgrounded tab must not leave a
    // timer parsing telemetry into a screen nobody is looking at.
    const h = await renderTune();
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

  it('abandons an in-flight read on unmount rather than letting it land in a dead component', async () => {
    let seen: AbortSignal | undefined;
    // A read that never settles — the only way to observe the abort, and the realistic case: the
    // RD closes the tab while the Director is mid-answer.
    const fetchSignal = vi.fn((_id: string, o: { signal: AbortSignal }) => {
      seen = o.signal;
      return new Promise<TimerSignal>(() => {});
    });
    const stopSignal = vi.fn(async () => {});
    const { session } = makeTestSession({
      listChannelsImpl: async () => CATALOG,
      listPilotsImpl: async () => [],
      listHeatsImpl: async () => []
    });
    const { unmount } = render(TunePage, {
      session,
      timer: TIMER,
      onhome: () => {},
      ontimers: () => {},
      fetchSignal,
      applyLevels: vi.fn(),
      stopSignal,
      pollMs: 10 * 60 * 1000
    });
    await waitFor(() => expect(seen).toBeDefined());
    expect(seen!.aborted).toBe(false);
    unmount();
    expect(seen!.aborted).toBe(true);
    // …and the Director is told, rather than left to time the lease out on its own.
    expect(stopSignal).toHaveBeenCalledWith('rh-1');
  });
});

/**
 * Which layer am I editing? (#411's first open question.)
 *
 * The scope comes from the ROUTE, so the shell hands the page an event or it does not, and the page
 * has to say which — by the event's friendly NAME (CLAUDE.md), never its id — and take its back
 * crumb to the matching place. Profiles do not exist, so the page names the *event*; and both
 * scopes write the same calibration today, so the note must not imply an isolation that isn't there.
 */
describe('TunePage — the scope it states (#411)', () => {
  /** Render the page in one scope or the other, over an inert feed. */
  async function renderScoped(scope: { scopeEvent?: EventMeta } = {}) {
    const onhome = vi.fn();
    const ontimers = vi.fn();
    const onevent = vi.fn();
    const { session } = makeTestSession({
      listChannelsImpl: async () => CATALOG,
      listPilotsImpl: async () => [],
      listHeatsImpl: async () => []
    });
    const { unmount } = render(TunePage, {
      session,
      timer: TIMER,
      ...scope,
      onhome,
      ontimers,
      onevent,
      fetchSignal: vi.fn(async () => snapshot()),
      applyLevels: vi.fn(async () => {}),
      stopSignal: vi.fn(async () => {}),
      pollMs: 10 * 60 * 1000
    });
    await screen.findByLabelText('Enter at level for Node 1 · Raceband R7');
    return { onhome, ontimers, onevent, unmount };
  }

  it('names the TIMER when there is no event in scope', async () => {
    const h = await renderScoped();
    const scope = screen.getByTestId('tune-scope');
    expect(scope).toHaveTextContent('editing:');
    expect(scope).toHaveTextContent('Track RH');
    expect(scope).toHaveTextContent(/No event in scope/);
    h.unmount();
  });

  it('names the EVENT and the timer when opened from inside an event', async () => {
    const h = await renderScoped({ scopeEvent: eventWith([]) });
    const scope = screen.getByTestId('tune-scope');
    expect(scope).toHaveTextContent('Friday · Track RH');
    // The event's NAME, never its id (CLAUDE.md) — 'e1' must not reach the screen.
    expect(scope.textContent).not.toContain('e1');
    // …and it must not claim an isolation that does not exist yet: both scopes write the same
    // calibration today, so the note says the levels are still the timer's own.
    expect(scope).toHaveTextContent(/timer's own levels/);
    h.unmount();
  });

  it('takes the back crumb to the Timers page on the timer scope', async () => {
    const h = await renderScoped();
    expect(screen.queryByRole('button', { name: 'Friday' })).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Timers' }));
    expect(h.ontimers).toHaveBeenCalledTimes(1);
    expect(h.onevent).not.toHaveBeenCalled();
    h.unmount();
  });

  it('takes the back crumb into the EVENT on the event scope', async () => {
    // The RD's requirement: "tuning from in the event would be ideal, as long as when we click back
    // we are back in the event". So the middle crumb is the event, and it does not go to Timers.
    const h = await renderScoped({ scopeEvent: eventWith([]) });
    expect(screen.queryByRole('button', { name: 'Timers' })).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Friday' }));
    expect(h.onevent).toHaveBeenCalledTimes(1);
    expect(h.ontimers).not.toHaveBeenCalled();
    h.unmount();
  });

  it('keeps Home reachable in both scopes', async () => {
    for (const scope of [{}, { scopeEvent: eventWith([]) }]) {
      const h = await renderScoped(scope);
      await fireEvent.click(screen.getByRole('button', { name: 'Home' }));
      expect(h.onhome).toHaveBeenCalledTimes(1);
      h.unmount();
    }
  });
});

describe('TunePage — layout', () => {
  it('offers a stacked variant of the same columns, without forking the markup', async () => {
    const h = await renderTune();
    const columns = document.querySelector('[data-layout]') as HTMLElement;
    expect(columns.dataset.layout).toBe('columns');
    const before = columns.querySelectorAll('section').length;

    await fireEvent.click(screen.getByRole('button', { name: 'Stacked' }));
    await tick();
    const after = document.querySelector('[data-layout]') as HTMLElement;
    expect(after.dataset.layout).toBe('stacked');
    // Same nodes, same sections — a class, not a second component.
    expect(after.querySelectorAll('section').length).toBe(before);
    h.unmount();
  });
});

describe('TunePage — the channel is settable, not just shown (#413)', () => {
  /** The channel dropdown for a node, by the node's friendly name (CLAUDE.md). */
  const channelSelect = (name = 'Node 1 · Raceband R7') =>
    screen.getByLabelText(`Channel for ${name}`) as HTMLSelectElement;

  it('offers the WHOLE catalog on a Flexible timer, not the timer\u2019s channel pool', async () => {
    // The trap #413 was filed with a warning about. Both real RotorHazard timers on the bench
    // report `Flexible` with an EMPTY `available_channels` — which means "no restriction", not "no
    // channels" — so a dropdown bound to the pool renders empty on exactly the timer this is for.
    // Here the pool is [5880, 5695] and the catalog also holds 5800: the dropdown must offer all
    // three, and the extra one proves the source is the capability rather than the pool.
    const h = await renderTune();
    const labels = [...channelSelect().options].map((o) => o.textContent?.trim());
    expect(labels).toEqual(['Raceband R7 — 5880', 'Raceband R2 — 5695', 'Fatshark F4 — 5800']);
    h.unmount();
  });

  it('still offers the full catalog when the pool is empty — the bench case exactly', async () => {
    const h = await renderTune({ timer: { ...TIMER, available_channels: [] } });
    expect(channelSelect().options).toHaveLength(CATALOG.length);
    h.unmount();
  });

  it("adds the RD's custom raw-MHz channels alongside the catalog", async () => {
    // The one thing `available_channels` legitimately contributes: the custom entries the RD typed
    // into the timer's channel config, which they asked to see beside the catalog.
    const h = await renderTune({
      timer: { ...TIMER, available_channels: [5880, 5891] }
    });
    const labels = [...channelSelect().options].map((o) => o.textContent?.trim());
    // A frequency the catalog does not know is marked as such — the RD can see at a glance that it
    // is theirs, not a standard channel.
    expect(labels).toContain('Custom — 5891');
    // …after the catalog, not instead of it.
    expect(labels.slice(0, CATALOG.length)).toEqual([
      'Raceband R7 — 5880',
      'Raceband R2 — 5695',
      'Fatshark F4 — 5800'
    ]);
    h.unmount();
  });

  it('limits a Fixed timer to the channels it declares', async () => {
    const h = await renderTune({
      timer: { ...TIMER, channel_capability: { Fixed: { channels: [5880, 5695] } } }
    });
    expect([...channelSelect().options].map((o) => o.textContent?.trim())).toEqual([
      'Raceband R7 — 5880',
      'Raceband R2 — 5695'
    ]);
    h.unmount();
  });

  it('shows a Fixed timer’s node on a channel outside the declared set (#449)', async () => {
    // Node 0 reports 5880, and the RD has since narrowed this timer to R2/F4. With 5880 missing
    // from the options the `<select value={chan.mhz}>` matched nothing, and the browser fell back
    // to rendering the FIRST option — so the page showed Raceband R2 on a gate sitting on R7, as
    // though the RD had picked it. The one thing that must never happen on this control.
    const h = await renderTune({
      timer: { ...TIMER, channel_capability: { Fixed: { channels: [5695, 5800] } } }
    });
    const select = screen.getByTestId('channel-0').querySelector('select') as HTMLSelectElement;
    expect(select.value).toBe('5880');
    // Present, named, and last — the declared set still leads.
    expect([...select.options].map((o) => o.textContent?.trim())).toEqual([
      'Raceband R2 — 5695',
      'Fatshark F4 — 5800',
      'Raceband R7 — 5880'
    ]);
    h.unmount();
  });

  it('offers a Fixed timer a declared channel the catalog cannot name (#449)', async () => {
    // `offeredCatalog` filtered the catalog, so a declared frequency with no catalog entry was
    // dropped before it could become an option — a timer on a non-standard grid could never be
    // offered the channels it supports. Node 0's own 5880 rides along beside them, as ever.
    const h = await renderTune({
      timer: { ...TIMER, channel_capability: { Fixed: { channels: [5695, 5891] } } }
    });
    const select = screen.getByTestId('channel-0').querySelector('select') as HTMLSelectElement;
    const labels = [...select.options].map((o) => o.textContent?.trim());
    // Named from the raw MHz through `channels.ts` — never a bare number on its own.
    expect(labels).toContain('Custom — 5891');
    expect(labels).toEqual(['Raceband R2 — 5695', 'Custom — 5891', 'Raceband R7 — 5880']);

    // And it is genuinely selectable: the write goes out with no band/channel invented for it.
    await fireEvent.change(select, { target: { value: '5891' } });
    await waitFor(() => expect(h.applyChannel).toHaveBeenCalledTimes(1));
    expect(h.applyChannel).toHaveBeenCalledWith('rh-1', { node: 0, mhz: 5891 });
    h.unmount();
  });

  it('sends the BAND AND CHANNEL, not just the frequency', async () => {
    // RotorHazard's `on_set_frequency` stores band/channel on its active profile, and the RD
    // validates this work by refreshing RotorHazard's own page — where a bare number with no `R7`
    // beside it reads as "it half worked".
    const h = await renderTune();
    await fireEvent.change(channelSelect(), { target: { value: '5800' } });
    await waitFor(() => expect(h.applyChannel).toHaveBeenCalledTimes(1));
    expect(h.applyChannel).toHaveBeenCalledWith('rh-1', {
      node: 0,
      mhz: 5800,
      band: 'Fatshark',
      channel: 'F4'
    });
    h.unmount();
  });

  it('shows the channel as `Sending…` until a POLL brings it back', async () => {
    // Same rule as a threshold: `POST /channel` only says "accepted". The channel comes back on the
    // heartbeat, so the feed the page is already polling is the confirmation.
    const h = await renderTune({ pollMs: 20 });
    await fireEvent.change(channelSelect(), { target: { value: '5800' } });
    await waitFor(() => expect(h.applyChannel).toHaveBeenCalled());
    // The node's own name follows the channel it is now on, so it is addressed by the new one.
    await waitFor(() =>
      expect(within(screen.getByTestId('channel-0')).getByText('On timer')).toBeInTheDocument()
    );
    h.unmount();
  });

  // #442, defect 2 — the honest seam is HERE, not in `tuning.ts`: the guard that drops the pick is
  // `commitChannel`'s own `if (!held || held.phase === 'sent') return;` (TunePage.svelte:745).
  // `ChannelState` has no queue and no refusal for this case, so there is nothing in the state
  // machine to exercise — the component is where the input is accepted and where it is lost.
  //
  // The dropdown is NOT disabled while a channel write is unconfirmed (up to CONFIRM_TIMEOUT_MS,
  // 3 s), so the RD's second pick is taken by the control, dropped by the handler, and then erased
  // from the screen by the one-way `value={chan.mhz}` snapping the select back to the first pick.
  // No write, no error, no `refused` state, no toast: the page ends up on a channel nobody asked
  // for while reading as though it were the RD's own choice.
  //
  // Deliberately accepts EITHER honest fix — queue/apply the pick, or refuse it out loud — because
  // both are defensible and only the silence is not.
  //
  // Fixed the first way: waiting on a poll is not a reason to refuse a pick (the POST is already
  // back and the wire is free), so the second pick is written immediately and `channelSeq` retires
  // the first write's answer. The narrower window where the POST itself is still open is queued
  // instead — the two tests below own that half.
  it('never silently drops a channel pick made while the previous write is unconfirmed', async () => {
    // No second poll (the default 10-minute cadence) and a confirm backstop far out of the way, so
    // the first write simply stays `sent` — exactly the window this is about.
    const h = await renderTune({ confirmMs: 5_000 });
    const panel = () => screen.getByTestId('channel-0');
    // Addressed through the panel's own testid rather than the node's accessible name, because that
    // name follows the channel and is mid-change for the whole of this test.
    const select = () => panel().querySelector('select') as HTMLSelectElement;

    await fireEvent.change(select(), { target: { value: '5800' } });
    await waitFor(() => expect(h.applyChannel).toHaveBeenCalledTimes(1));
    // The write is out and unconfirmed — and the control is still live, so the RD may pick again.
    expect(within(panel()).getByText('Sending…')).toBeInTheDocument();
    expect(select()).not.toBeDisabled();

    // The RD changes their mind inside the round trip.
    await fireEvent.change(select(), { target: { value: '5695' } });

    await waitFor(
      () => {
        const outcome = {
          // Either the pick reached the timer (immediately, or queued behind the first write)…
          wroteThePick: h.applyChannel.mock.calls.some((call) => call[1].mhz === 5695),
          // …or the node says out loud that it was not sent. `phaseLabel('refused')` is 'Not sent'.
          toldTheRD: within(panel()).queryByText('Not sent') !== null
        };
        expect(outcome).not.toEqual({ wroteThePick: false, toldTheRD: false });
      },
      { timeout: 300 }
    );
    h.unmount();
  });

  it('shows the RD their own pick, not the one it superseded (#442)', async () => {
    // The half of the bug that is invisible in a call log: `value={chan.mhz}` is one-way, so a pick
    // the handler drops is also ERASED from the control — the select snaps back to the previous
    // channel and reads as though the RD had chosen it.
    const h = await renderTune({ confirmMs: 5_000 });
    const select = () =>
      screen.getByTestId('channel-0').querySelector('select') as HTMLSelectElement;

    await fireEvent.change(select(), { target: { value: '5800' } });
    await waitFor(() => expect(h.applyChannel).toHaveBeenCalledTimes(1));
    await fireEvent.change(select(), { target: { value: '5695' } });

    await waitFor(() => expect(select().value).toBe('5695'));
    h.unmount();
  });

  it('QUEUES a pick made while the write is literally on the wire, and sends only the newest', async () => {
    // The narrow window the immediate write must not be used for: `on_set_frequency` is a
    // fire-and-forget socket emit with no ack and no ordering guarantee (CLAUDE.md), so two
    // overlapping writes to one node can land in either order — and the loser could be the newer.
    let release = () => {};
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    const h = await renderTune({ confirmMs: 5_000, holdChannelWrite: held });
    const panel = () => screen.getByTestId('channel-0');
    const select = () => panel().querySelector('select') as HTMLSelectElement;

    await fireEvent.change(select(), { target: { value: '5800' } });
    await waitFor(() => expect(h.applyChannel).toHaveBeenCalledTimes(1));

    // Two more picks while the first POST is still open. Nothing new goes out…
    await fireEvent.change(select(), { target: { value: '5880' } });
    await fireEvent.change(select(), { target: { value: '5695' } });
    expect(h.applyChannel).toHaveBeenCalledTimes(1);
    // …but the RD's latest choice is on screen and reads as in-hand, not as landed.
    expect(select().value).toBe('5695');
    expect(within(panel()).getByText('Adjusting')).toBeInTheDocument();

    release();

    // Exactly one follow-up write, carrying the LAST pick. The 5880 the RD clicked through on the
    // way is not a write anybody asked for.
    await waitFor(() => expect(h.applyChannel).toHaveBeenCalledTimes(2));
    expect(h.applyChannel.mock.calls[1][1]).toMatchObject({ node: 0, mhz: 5695 });
    expect(h.applyChannel.mock.calls.some((c) => c[1].mhz === 5880)).toBe(false);
    h.unmount();
  });

  it('flushes a queued pick even when the write it was queued behind FAILED', async () => {
    // The failure belongs to the channel the RD moved off. Swallowing their newer choice with it
    // would leave the node on neither, silently — the same drop, one path over.
    let release = () => {};
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    const h = await renderTune({
      confirmMs: 5_000,
      holdChannelWrite: held,
      channelRejects: new Error('the timer said no')
    });
    const select = () =>
      screen.getByTestId('channel-0').querySelector('select') as HTMLSelectElement;

    await fireEvent.change(select(), { target: { value: '5800' } });
    await waitFor(() => expect(h.applyChannel).toHaveBeenCalledTimes(1));
    await fireEvent.change(select(), { target: { value: '5695' } });
    release();

    await waitFor(() => expect(h.applyChannel).toHaveBeenCalledTimes(2));
    expect(h.applyChannel.mock.calls[1][1]).toMatchObject({ mhz: 5695 });
    h.unmount();
  });

  it('does not flush a queued pick into a page the RD has already left', async () => {
    // The write would go out with nothing on screen waiting for it or reporting on it — the same
    // reason every timer on this page is cleared with the component.
    let release = () => {};
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    const h = await renderTune({ confirmMs: 5_000, holdChannelWrite: held });
    const select = () =>
      screen.getByTestId('channel-0').querySelector('select') as HTMLSelectElement;

    await fireEvent.change(select(), { target: { value: '5800' } });
    await waitFor(() => expect(h.applyChannel).toHaveBeenCalledTimes(1));
    await fireEvent.change(select(), { target: { value: '5695' } });

    h.unmount();
    release();
    await new Promise((r) => setTimeout(r, 20));
    expect(h.applyChannel).toHaveBeenCalledTimes(1);
  });

  it('says NOT TAKEN when the timer never comes back on the new channel', async () => {
    // The #403 failure class: a write that reports dispatched and never lands. Silence here would
    // leave the RD tuning a gate that is on a different channel from the one on screen.
    const h = await renderTune({ pollMs: 20, confirmMs: 40, tunes: () => undefined });
    await fireEvent.change(channelSelect(), { target: { value: '5800' } });
    await waitFor(() =>
      expect(within(screen.getByTestId('channel-0')).getByText('Not taken')).toBeInTheDocument()
    );
    h.unmount();
  });

  it('says the thresholds were tuned on a different channel, factually', async () => {
    // `on_set_frequency` writes the frequency into the SAME profile row that holds the thresholds,
    // so they came through the change untouched — tuned for the channel the node just left. Nothing
    // else announces that: the levels look unchanged and therefore fine.
    const h = await renderTune({ staleThresholds: true });
    await fireEvent.change(channelSelect(), { target: { value: '5800' } });
    const note = await screen.findByTestId('channel-stale-0');
    expect(note.textContent).toContain('Raceband R7');
    expect(note.textContent).toContain('Fatshark F4');
    // Factual, not alarming — and never a bare frequency (CLAUDE.md).
    expect(note.textContent).not.toMatch(/wrong|broken|error/i);
    expect(note.textContent).not.toMatch(/\b5880\b/);
    h.unmount();
  });

  it('says nothing about stale thresholds when the Director says there are none', async () => {
    const h = await renderTune({ staleThresholds: false });
    await fireEvent.change(channelSelect(), { target: { value: '5800' } });
    await waitFor(() => expect(h.applyChannel).toHaveBeenCalled());
    expect(screen.queryByTestId('channel-stale-0')).toBeNull();
    h.unmount();
  });

  it('flags two nodes on one channel — but does not block it', async () => {
    // A real mistake worth flagging, and also exactly what a bench swap looks like halfway through.
    const h = await renderTune();
    // Node 1 (index 0) is on R7; move node 2 (index 1) onto R7 as well.
    await fireEvent.change(screen.getByLabelText('Channel for Node 2 · Raceband R2'), {
      target: { value: '5880' }
    });
    await waitFor(() => expect(h.applyChannel).toHaveBeenCalledTimes(1));
    const note = await screen.findByTestId('channel-clash-1');
    // Named the way the RD names them — 1-based, never a raw index.
    expect(note.textContent).toContain('Node 1');
    // The write went through regardless: flagged, not refused.
    expect(h.applyChannel).toHaveBeenCalledWith(
      'rh-1',
      expect.objectContaining({ node: 1, mhz: 5880 })
    );
    h.unmount();
  });

  it('states that a heat will overwrite the channel, rather than trying to win', async () => {
    // Channel here is a bench setting; heat setup legitimately reassigns. An RD who tunes node 1 to
    // R7 and then starts a heat must not be surprised.
    const h = await renderTune();
    const panel = screen.getByTestId('channel-0');
    expect(panel.textContent).toMatch(/bench setting/i);
    expect(panel.textContent).toMatch(/heat/i);
    h.unmount();
  });

  it('never offers a channel for a node the RD has DISABLED', async () => {
    // #412: RotorHazard validates `0 <= node < num_nodes` and otherwise writes nothing but a log
    // line, and a disabled node seats no pilot — so the RD is never offered the choice.
    const h = await renderTune({ nodes: nodeView([0]) });
    expect(screen.getByLabelText('Channel for Node 1 · Raceband R7')).toBeInTheDocument();
    expect(screen.queryByTestId('channel-1')).toBeNull();
    h.unmount();
  });

  it('says why rather than silently omitting the dropdown on a channel-less node', async () => {
    // The node exists and is enabled, but the timer has not said what it is tuned to. A dropdown
    // resting on a fabricated default is one the RD can change away from without ever having seen
    // the real one — so there is none, and the gap is explained rather than left blank.
    const feed = snapshot();
    feed.nodes[0].frequency_mhz = undefined;
    const h = await renderTune({ signal: feed });
    expect(screen.queryByTestId('channel-0')).toBeNull();
    expect(screen.getByTestId('channel-waiting-0').textContent).toMatch(/not reported a channel/i);
    h.unmount();
  });

  it('offers no channel at all until the Director says which nodes exist', async () => {
    // Fails closed. Better a control that appears a beat late than one that offers a gate the
    // hardware does not have.
    const h = await renderTune({ nodes: null });
    expect(screen.queryByTestId('channel-0')).toBeNull();
    expect(screen.queryByTestId('channel-1')).toBeNull();
    h.unmount();
  });

  it('refuses a channel change while a COMPETITION heat is running', async () => {
    const h = await renderTune({
      live: RUNNING,
      event: eventWith([QUAL_ROUND]),
      heats: heatOn(QUAL_ROUND)
    });
    expect(channelSelect()).toBeDisabled();
    // And the page says why, in the words of what a retune actually does.
    const banner = await screen.findByTestId('channel-gate');
    expect(banner.textContent).toMatch(/channel/i);
    expect(h.applyChannel).not.toHaveBeenCalled();
    h.unmount();
  });

  it('ALLOWS a channel change during open practice', async () => {
    // #398: practice is excluded from scoring, and pilots in the air is exactly when an RD is
    // checking whether the gate is even on the right channel.
    const h = await renderTune({
      live: RUNNING,
      event: eventWith([PRACTICE_ROUND]),
      heats: heatOn(PRACTICE_ROUND)
    });
    expect(channelSelect()).not.toBeDisabled();
    await fireEvent.change(channelSelect(), { target: { value: '5800' } });
    await waitFor(() => expect(h.applyChannel).toHaveBeenCalledTimes(1));
    h.unmount();
  });

  it('disables the dropdown for a read-only session', async () => {
    const h = await renderTune({ role: 'readonly' });
    expect(channelSelect()).toBeDisabled();
    h.unmount();
  });

  it('surfaces the Director\u2019s refusal verbatim on the node', async () => {
    // The Director's message already names the timer, the node and the channel by their friendly
    // names; replacing it with a status code would put a raw id or a bare number on screen.
    const h = await renderTune({
      channelRejects: new Error('Node 3 is disabled on "Track RH" — enable it first')
    });
    await fireEvent.change(channelSelect(), { target: { value: '5800' } });
    await waitFor(() =>
      expect(
        within(screen.getByTestId('channel-0')).getByText(/Node 3 is disabled/)
      ).toBeInTheDocument()
    );
    h.unmount();
  });

  it('shows band, channel AND frequency in the channel picker, and marks a custom one', async () => {
    // The RD picks a channel by matching it against a VTX, a printed sheet, or RotorHazard's own
    // screen — and those speak in MHz. Here the number is EXTRA information beside the friendly
    // name, never a substitute for it, so the band and channel still lead.
    const h = await renderTune();
    const select = await screen.findByLabelText('Channel for Node 1 · Raceband R7');
    const labels = Array.from(select.querySelectorAll('option')).map((o) => o.textContent?.trim());
    expect(labels).toContain('Raceband R7 — 5880');
    expect(labels).toContain('Raceband R2 — 5695');
    h.unmount();
  });
});

describe('TunePage — a fresh subscription fills in without a manual refresh', () => {
  it('renders the nodes once a later poll carries them', async () => {
    // The RD hit this on the bench: open Tune, get "Reading this node's channel…" and
    // "Waiting for this node to report its levels…" forever, refresh the page, and everything
    // appears. A refresh worked because by then the lease was already warm — so the very first
    // GET of the NEW subscription answered with data. The page must not need that.
    const h = await renderTune({ warmup: true, pollMs: 5 });

    await waitFor(
      () =>
        expect(screen.getByRole('heading', { name: 'Node 1 · Raceband R7' })).toBeInTheDocument(),
      { timeout: 2000 }
    );
    // And the placeholders are gone — not merely joined by real content.
    expect(screen.queryByText(/Reading this node/)).toBeNull();
    expect(screen.queryByText(/Waiting for this node to report its levels/)).toBeNull();
    expect(h.fetchSignal.mock.calls.length).toBeGreaterThan(1);
    h.unmount();
  });
});

describe('TunePage — Capture: let the timer MEASURE the level (#355)', () => {
  const captureBtn = (node = 0, th: 'enter' | 'exit' = 'enter') =>
    within(screen.getByTestId(`capture-${node}-${th}`)).getByRole('button');

  it('says what it will do BEFORE it is pressed — not a bare verb, and not an icon', async () => {
    // The RD's own requirement, verbatim: *"the RD had never seen this in RH and did not know it
    // existed, so it cannot be a bare icon or an unexplained verb."* Three facts have to be on
    // screen before the press, and each of them was read off RotorHazard's source rather than
    // assumed: the timer does the measuring, the window starts at the press, and the RD flies the
    // pass INTO it. "Fly a lap, then capture" would send them to the gate three seconds too late.
    const h = await renderTune();
    const btn = captureBtn();
    expect(btn).toHaveTextContent(/Capture Enter at from a pass/i);
    // The accessible name carries the same explanation, and names the node by its FRIENDLY name.
    expect(btn.getAttribute('aria-label')).toMatch(/Node 1 · Raceband R7/);
    expect(btn.getAttribute('aria-label')).toMatch(/three seconds from the moment you press/i);
    // And the explainer beside it says the mechanism plainly, including the honest ending.
    expect(
      screen.getAllByText(/watches this gate for three seconds starting the moment you press/i)
        .length
    ).toBeGreaterThan(0);
    expect(
      screen.getAllByText(/Nothing is recorded unless a new level comes back/i).length
    ).toBeGreaterThan(0);
    h.unmount();
  });

  it('tells the RD to fly the pass NOW, and counts the window down', async () => {
    // The countdown is the instruction. RotorHazard's window opens at the emit, so an RD who does
    // not know how long they have is an RD whose pass lands outside it.
    const h = await renderTune({ pollMs: 15 });
    await fireEvent.click(captureBtn());
    await waitFor(() =>
      expect(h.startCapture).toHaveBeenCalledWith('rh-1', { node: 0, threshold: 'enter' })
    );
    await waitFor(() =>
      expect(
        within(screen.getByTestId('capture-0-enter')).getByText(/Fly the pass now/)
      ).toBeInTheDocument()
    );
    // While the timer is watching, pressing again must not start a second capture — RotorHazard
    // refuses that in silence, so a second press would look started and do nothing.
    await fireEvent.click(captureBtn());
    expect(h.startCapture).toHaveBeenCalledTimes(1);
    h.unmount();
  });

  it('confirms the captured level BY POLL, and it lands in all three editors', async () => {
    // Same evidence a typed level is confirmed by, and it has to be: the RD cannot know the number
    // in advance, so the only proof the capture landed is the timer reporting a level it was not
    // reporting before.
    const h = await renderTune({ pollMs: 15, captures: () => 118 });
    await fireEvent.click(captureBtn());
    await waitFor(() =>
      expect(
        within(screen.getByTestId('capture-0-enter')).getByText('Captured 118')
      ).toBeInTheDocument()
    );
    // The captured level is now THE value — every editor shows it, exactly as if the RD had typed
    // it, because from here on it is GridFPV's value (D27) and the Director has recorded it.
    expect(box().value).toBe('118');
    expect(slider().value).toBe('118');
    expect(graphValue('Enter')).toBe(118);
    // And it did NOT write the level back at the timer: the timer already has it, and a second
    // write would be GridFPV changing something nobody asked it to change.
    expect(h.applyLevels).not.toHaveBeenCalled();
    h.unmount();
  });

  it('reports a capture that did not land, rather than showing it as a success', async () => {
    // RotorHazard refuses a capture — a node that is not answering, or one already capturing — with
    // no reply of any kind: `start_capture_enter_at_level` returns False and the handler emits
    // nothing. So "the level never changed" is the ONLY evidence of that refusal, and it must read
    // as a failure. This is the #423 failure class (a write that returns success and does nothing).
    const h = await renderTune({ pollMs: 15 });
    await fireEvent.click(captureBtn());
    const panel = () => screen.getByTestId('capture-0-enter');
    await waitFor(() => expect(within(panel()).getByText('Nothing captured')).toBeInTheDocument());
    expect(screen.getByTestId('capture-detail-0-enter')).toHaveTextContent(
      /still reporting 90.*Nothing was captured and nothing was recorded/s
    );
    // The threshold itself is untouched — nothing was invented to fill the gap.
    expect(box().value).toBe('90');
    h.unmount();
  });

  it('gives a verdict even when the poll itself has stopped answering', async () => {
    // The backstop, exactly as for a write: with no further polls no confirmation is coming, and a
    // capture left reading "Reading the level…" for ever is the silent failure in another costume.
    const h = await renderTune({ pollMs: 10 * 60 * 1000 });
    await fireEvent.click(captureBtn());
    await waitFor(() =>
      expect(
        within(screen.getByTestId('capture-0-enter')).getByText('Nothing captured')
      ).toBeInTheDocument()
    );
    expect(h.fetchSignal).toHaveBeenCalledTimes(1);
    h.unmount();
  });

  it('surfaces the Director’s refusal verbatim when the capture never starts', async () => {
    const h = await renderTune({
      captureRejects: new Error(
        'Node 1 is already capturing its Enter at level — wait for that capture to finish'
      )
    });
    await fireEvent.click(captureBtn());
    await waitFor(() =>
      expect(
        within(screen.getByTestId('capture-0-enter')).getByText('Capture failed')
      ).toBeInTheDocument()
    );
    expect(screen.getByTestId('capture-detail-0-enter')).toHaveTextContent(/already capturing/);
    h.unmount();
  });

  it('is never offered for a node the RD has DISABLED (#412)', async () => {
    // A capture on a disabled node would sample hardware no heat is ever seated on, and RotorHazard
    // drops an out-of-range seat index with nothing but a log line. Same rule, same reason, as the
    // channel dropdown's.
    const h = await renderTune({ nodes: nodeView([0]) });
    expect(screen.queryByTestId('capture-0-enter')).toBeInTheDocument();
    expect(screen.queryByTestId('capture-1-enter')).not.toBeInTheDocument();
    expect(screen.queryByTestId('capture-1-exit')).not.toBeInTheDocument();
    h.unmount();
  });

  it('is refused while a SCORED heat is running, and says why', async () => {
    // A capture ends by SETTING the threshold, so it changes what counts as a lap under a scored
    // heat just as surely as a typed level does. Same gate, checked per press.
    const h = await renderTune({
      live: RUNNING,
      event: eventWith([QUAL_ROUND]),
      heats: heatOn(QUAL_ROUND)
    });
    expect(captureBtn()).toBeDisabled();
    h.unmount();
  });

  it('is ALLOWED while an open-practice heat is running (#398)', async () => {
    // Practice is excluded from scoring, so there is no result to corrupt — and a practice heat is
    // the natural moment to capture, because the pass a capture needs is one a pilot is flying.
    const h = await renderTune({
      live: RUNNING,
      event: eventWith([PRACTICE_ROUND]),
      heats: heatOn(PRACTICE_ROUND),
      captures: () => 118
    });
    expect(captureBtn()).not.toBeDisabled();
    await fireEvent.click(captureBtn());
    await waitFor(() => expect(h.startCapture).toHaveBeenCalledTimes(1));
    h.unmount();
  });

  it('is not offered to a read-only session with no control authority', async () => {
    const h = await renderTune({ role: 'readonly' });
    expect(captureBtn()).toBeDisabled();
    h.unmount();
  });
});
