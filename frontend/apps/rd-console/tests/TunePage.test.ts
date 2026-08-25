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
 *  • **The readback is the confirmation.** `set_enter_at_level` does not echo, so a write that the
 *    hardware did not take must be visible on that node, never silent (#403's failure class).
 *  • **The practice-only gate is checked per write**, not once at load — a heat going Running while
 *    the RD is at the gate has to start refusing mid-tune.
 *  • **No raw seat, no bare frequency** on screen (CLAUDE.md).
 *  • **The poll stops** on unmount and when the tab is hidden — the endpoint holds a TTL lease.
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
  Pilot,
  RoundDef,
  Timer
} from '@gridfpv/types';
import TunePage from '../src/screens/TunePage.svelte';
import type { CalibrationReadback, TimerSignal } from '../src/lib/tuning.js';
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
  manual_connect: true
};

/** Two nodes at rest: node 0 on Raceband R7 with 90/80, node 1 on R2 with 95/85. */
function snapshot(over: Partial<TimerSignal['nodes'][number]> = {}): TimerSignal {
  return {
    timer: 'rh-1',
    nodes: [
      {
        node: 0,
        frequency: 5880,
        current_rssi: 48,
        crossing_flag: false,
        enter_at_level: 90,
        exit_at_level: 80,
        node_peak_rssi: 132,
        node_nadir_rssi: 12,
        pass_peak_rssi: 118,
        pass_nadir_rssi: 41,
        debug_pass_count: 7,
        samples: [40, 42, 44, 46, 48],
        from: 0,
        period_micros: 200_000,
        ...over
      },
      {
        node: 1,
        frequency: 5695,
        current_rssi: 51,
        crossing_flag: false,
        enter_at_level: 95,
        exit_at_level: 85,
        samples: [50, 51, 52],
        from: 0,
        period_micros: 200_000
      }
    ]
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
  unmount: () => void;
}

/** Render the page over a stubbed signal feed + calibration write. */
async function renderTune(
  opts: {
    signal?: TimerSignal;
    readback?: (
      node: number,
      body: { enter_at_level?: number; exit_at_level?: number }
    ) => CalibrationReadback;
    applyRejects?: Error;
    live?: LiveRaceState;
    event?: EventMeta;
    heats?: HeatSummary[];
    pilots?: Pilot[];
  } = {}
): Promise<Harness> {
  const snap = opts.signal ?? snapshot();
  const fetchSignal = vi.fn(async () => snap);
  const applyLevels = vi.fn(
    async (
      _timer: string,
      node: number,
      body: { enter_at_level?: number; exit_at_level?: number }
    ) => {
      if (opts.applyRejects) throw opts.applyRejects;
      return (
        opts.readback?.(node, body) ?? {
          node,
          // Default: the hardware took exactly what it was sent.
          enter_at_level: body.enter_at_level ?? 90,
          exit_at_level: body.exit_at_level ?? 80
        }
      );
    }
  );

  const { session } = makeTestSession({
    live: opts.live,
    event: opts.event,
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
    // One poll on mount; these tests never want a second one firing mid-assertion.
    pollMs: 10 * 60 * 1000
  });

  // Wait for the first snapshot to seed the per-(node, threshold) state.
  await screen.findByLabelText('Enter at level for Node 1 · Raceband R7');
  return { applyLevels, fetchSignal, unmount };
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
    expect(box().value).toBe('255');
    expect(slider().value).toBe('255');
    expect(graphValue('Enter')).toBe(255);
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
    expect(h.applyLevels).toHaveBeenCalledWith('rh-1', 0, { enter_at_level: 110 });
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
    expect(h.applyLevels).toHaveBeenCalledWith('rh-1', 0, { enter_at_level: 92 });
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
    expect(h.applyLevels).toHaveBeenCalledWith('rh-1', 0, { enter_at_level: 112 });
    h.unmount();
  });

  it('sends only the threshold that changed', async () => {
    const h = await renderTune();
    await fireEvent.input(box(1, 'Exit at'), { target: { value: '70' } });
    await fireEvent.blur(box(1, 'Exit at'));
    await waitFor(() => expect(h.applyLevels).toHaveBeenCalledTimes(1));
    expect(h.applyLevels).toHaveBeenCalledWith('rh-1', 0, { exit_at_level: 70 });
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
    expect(h.applyLevels).toHaveBeenCalledWith('rh-1', 0, { enter_at_level: 111 });
    h.unmount();
  });
});

describe('TunePage — the readback is the confirmation', () => {
  it('shows Adjusting → On timer once the readback matches', async () => {
    const h = await renderTune();
    const threshold = () => screen.getByTestId('threshold-0-enter');
    expect(within(threshold()).getByText('On timer')).toBeInTheDocument();

    await fireEvent.input(slider(), { target: { value: '110' } });
    await tick();
    expect(within(threshold()).getByText('Adjusting')).toBeInTheDocument();

    await fireEvent.pointerUp(slider());
    await waitFor(() => expect(within(threshold()).getByText('On timer')).toBeInTheDocument());
    h.unmount();
  });

  it('says so on the node when the hardware did NOT take the value', async () => {
    // RotorHazard does not echo the set, so a silent divergence would leave the RD tuning against
    // a level the timer never held — the #403 failure class.
    const h = await renderTune({
      readback: (node) => ({ node, enter_at_level: 90, exit_at_level: 80 })
    });
    await fireEvent.input(slider(), { target: { value: '110' } });
    await fireEvent.pointerUp(slider());

    const threshold = () => screen.getByTestId('threshold-0-enter');
    await waitFor(() => expect(within(threshold()).getByText('Not taken')).toBeInTheDocument());
    expect(within(threshold()).getByText(/reports 90, not 110/)).toBeInTheDocument();
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

describe('TunePage — the poll holds a TTL lease, so it must stop', () => {
  it('polls once on mount', async () => {
    const h = await renderTune();
    expect(h.fetchSignal).toHaveBeenCalledTimes(1);
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
      pollMs: 10 * 60 * 1000
    });
    await waitFor(() => expect(seen).toBeDefined());
    expect(seen!.aborted).toBe(false);
    unmount();
    expect(seen!.aborted).toBe(true);
  });

  it('stops polling when the tab is hidden and resumes when it comes back', async () => {
    const h = await renderTune();
    expect(h.fetchSignal).toHaveBeenCalledTimes(1);

    const spy = vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden');
    document.dispatchEvent(new Event('visibilitychange'));
    await tick();
    expect(h.fetchSignal).toHaveBeenCalledTimes(1); // still one — nothing new while hidden

    spy.mockReturnValue('visible');
    document.dispatchEvent(new Event('visibilitychange'));
    await waitFor(() => expect(h.fetchSignal).toHaveBeenCalledTimes(2));
    spy.mockRestore();
    h.unmount();
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
