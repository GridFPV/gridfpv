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
  EventMeta,
  HeatSummary,
  LiveRaceState,
  NodeSignal,
  Pilot,
  RoundDef,
  Timer,
  TimerSignal
} from '@gridfpv/types';
import TunePage from '../src/screens/TunePage.svelte';
import type { CalibrationRequest } from '@gridfpv/protocol-client';
import type { SessionRole } from '../src/lib/session.svelte.js';
import { makeTestSession } from './support.js';

const CATALOG: ChannelCatalogEntry[] = [
  { band: 'Raceband', channel: 'R7', mhz: 5880 },
  { band: 'Raceband', channel: 'R2', mhz: 5695 }
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
  fetchSignal: ReturnType<typeof vi.fn>;
  stopSignal: ReturnType<typeof vi.fn>;
  unmount: () => void;
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
    confirmMs?: number;
  } = {}
): Promise<Harness> {
  const feed: TimerSignal = opts.signal ?? snapshot();
  // A fresh object per poll: the page holds the snapshot in `$state.raw`, so handing back the same
  // reference would be indistinguishable from no new poll at all.
  const fetchSignal = vi.fn(async () => structuredClone(feed));
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

  const { session } = makeTestSession({
    live: opts.live,
    event: opts.event,
    role: opts.role,
    listChannelsImpl: async () => CATALOG,
    listPilotsImpl: async () => opts.pilots ?? [],
    listHeatsImpl: async () => opts.heats ?? []
  });

  const { unmount } = render(TunePage, {
    session,
    timer: TIMER,
    onhome: () => {},
    ontimers: () => {},
    fetchSignal,
    applyLevels,
    stopSignal,
    pollMs: opts.pollMs ?? 10 * 60 * 1000,
    confirmMs: opts.confirmMs ?? 150
  });

  // Wait for the first snapshot to seed the per-(node, threshold) state.
  await screen.findByLabelText('Enter at level for Node 1 · Raceband R7');
  return { applyLevels, fetchSignal, stopSignal, unmount };
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
    expect(screen.queryByText(/5880/)).toBeNull();
    expect(screen.queryByText(/5695/)).toBeNull();
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
    expect(screen.getByRole('heading', { name: 'Node 2 · Raceband R2' })).toBeInTheDocument();
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
