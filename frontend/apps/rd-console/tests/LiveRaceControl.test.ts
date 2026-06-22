import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor, within } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/dom';
import { tick } from 'svelte';
import type {
  ChannelCatalogEntry,
  EventMeta,
  HeatSummary,
  LiveRaceState,
  RoundDef,
  Timer
} from '@gridfpv/types';
import LiveRaceControl from '../src/screens/LiveRaceControl.svelte';
import { makeTestSession } from './support.js';
import { liveRunning, failAck } from './fixtures.js';

// A round with a short staging window so the over-time path is reachable in a fake-timer test, and
// a heat tagged with it so the screen resolves the round from the live current heat.
const ROUND: RoundDef = {
  id: 'r1',
  label: 'Qualifying R1',
  classes: ['c1'],
  format: 'timed_qual',
  params: {},
  win_condition: { Timed: { window_micros: 120_000_000 } },
  seeding: 'FromRoster',
  channel_mode: 'Static',
  staging_timer_secs: 5, // 0:05 so the test can run it over-time quickly
  start_procedure: { mode: 'randomized-delay', min_delay_ms: 2000, max_delay_ms: 5000 },
  grace_window: { Duration: { micros: 3_000_000 } }
};
const EVENT_WITH_ROUND: EventMeta = {
  id: 'e1',
  name: 'Friday',
  created_at: 0,
  persistent: true,
  timers: ['mock'],
  roster: [],
  classes: ['c1'],
  rounds: [ROUND]
};
const HEAT_IN_ROUND: HeatSummary = {
  heat: 'heat-1',
  lineup: ['ALICE', 'BOB'],
  round: 'r1',
  class: 'c1',
  frequencies: [],
  phase: 'Staged',
  is_current: true
};
const liveAt = (phase: LiveRaceState['phase'], heat: string | undefined = 'heat-1') =>
  ({ current_heat: heat, phase }) as LiveRaceState;

describe('LiveRaceControl', () => {
  it('enables only the phase-legal transitions (Running → ForceEnd/Abort/Restart)', () => {
    const { session } = makeTestSession({ live: liveRunning });
    render(LiveRaceControl, { session });

    const btn = (label: string) => screen.getByRole('button', { name: label }) as HTMLButtonElement;
    // The runtime-clock override + off-ramps legal in Running (the manual Finish is gone — the
    // clock auto-completes; ForceEnd is the override).
    expect(btn('ForceEnd').disabled).toBe(false);
    expect(btn('Abort').disabled).toBe(false);
    expect(btn('Restart').disabled).toBe(false);
    // Illegal in Running.
    expect(btn('Stage').disabled).toBe(true);
    expect(btn('Start').disabled).toBe(true);
    expect(btn('SkipCountdown').disabled).toBe(true);
    expect(btn('Finalize').disabled).toBe(true);
    expect(btn('Advance').disabled).toBe(true);
    expect(btn('Revert').disabled).toBe(true);
    expect(btn('Discard').disabled).toBe(true);
  });

  it('fires the matching Command for the runtime-clock override', async () => {
    const { session, sendSpy } = makeTestSession({ live: liveRunning });
    render(LiveRaceControl, { session });

    await fireEvent.click(screen.getByRole('button', { name: 'ForceEnd' }));
    expect(sendSpy).toHaveBeenCalledWith({ ForceEnd: { heat: 'heat-1' } });
  });

  it('requires a confirm before a destructive off-ramp, then fires it', async () => {
    const { session, sendSpy } = makeTestSession({ live: liveRunning });
    render(LiveRaceControl, { session });

    // First click arms the confirm (does NOT send).
    await fireEvent.click(screen.getByRole('button', { name: 'Abort' }));
    expect(sendSpy).not.toHaveBeenCalled();

    // Confirm fires the Abort command.
    await fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
    expect(sendSpy).toHaveBeenCalledWith({ Abort: { heat: 'heat-1' } });
  });

  it('surfaces a failed CommandAck error to the RD', async () => {
    const { session } = makeTestSession({ live: liveRunning, ack: failAck });
    render(LiveRaceControl, { session });

    await fireEvent.click(screen.getByRole('button', { name: 'ForceEnd' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('illegal transition');
  });

  it('renders the live leaderboard from the running order', () => {
    const { session } = makeTestSession({ live: liveRunning });
    render(LiveRaceControl, { session });
    // Heat sheet + live standing both list the lineup.
    expect(screen.getAllByText('ALICE').length).toBeGreaterThan(0);
  });

  it('styles the runtime-clock overrides as secondary "override" buttons (legality intact)', () => {
    // In Armed the legal actions are the override SkipCountdown + the off-ramps Abort/Restart.
    const { session } = makeTestSession({ live: liveAt('Armed') });
    render(LiveRaceControl, { session });
    const btn = (label: string) => screen.getByRole('button', { name: label }) as HTMLButtonElement;
    expect(btn('SkipCountdown').disabled).toBe(false);
    expect(btn('Abort').disabled).toBe(false);
    expect(btn('Restart').disabled).toBe(false);
    // Forward steps are illegal in Armed (the runtime clock drives Armed → Running).
    expect(btn('Stage').disabled).toBe(true);
    expect(btn('Start').disabled).toBe(true);
    expect(btn('ForceEnd').disabled).toBe(true);
    expect(btn('Finalize').disabled).toBe(true);
    // Both clock overrides (SkipCountdown + ForceEnd) carry the "override" tag that distinguishes
    // them from forward/off-ramp buttons; the forward/off-ramp buttons do not.
    const tags = screen.getAllByText('override');
    const taggedButtons = tags.map((t) => t.closest('button'));
    expect(taggedButtons).toContain(btn('SkipCountdown'));
    expect(taggedButtons).toContain(btn('ForceEnd'));
    expect(taggedButtons).not.toContain(btn('Abort'));
    expect(taggedButtons).not.toContain(btn('Start'));
  });

  describe('staging countdown (Slice 3)', () => {
    const stagingClock = () => screen.getByLabelText('Staging time remaining').textContent?.trim();

    afterEach(() => vi.useRealTimers());

    it('counts down from the round staging window while Staged, then goes over-time (red)', async () => {
      vi.useFakeTimers();
      vi.setSystemTime(0);
      const { session } = makeTestSession({
        event: EVENT_WITH_ROUND,
        live: liveAt('Staged'),
        listHeatsImpl: vi.fn(async () => [HEAT_IN_ROUND])
      });
      render(LiveRaceControl, { session });
      // The heats list (round resolution) is fetched async; let it settle.
      await vi.advanceTimersByTimeAsync(0);
      await tick();

      // The countdown is shown and starts near the 0:05 window.
      const region = await screen.findByRole('status', { name: 'Staging countdown' });
      expect(region).toBeInTheDocument();
      await waitFor(() => expect(stagingClock()).toBe('0:05'));

      // After ~3s it has counted down…
      await vi.advanceTimersByTimeAsync(3_000);
      expect(stagingClock()).toBe('0:02');

      // …and past zero it goes over-time: negative reading + the over-time (red) styling.
      await vi.advanceTimersByTimeAsync(3_000);
      expect(stagingClock()).toMatch(/^−0:0[01]$/);
      expect(region.className).toContain('overtime');
    });

    it('shows no staging countdown once the heat leaves Staged', async () => {
      const { session } = makeTestSession({
        event: EVENT_WITH_ROUND,
        live: liveAt('Running'),
        listHeatsImpl: vi.fn(async () => [{ ...HEAT_IN_ROUND, phase: 'Running' as const }])
      });
      render(LiveRaceControl, { session });
      await tick();
      expect(screen.queryByRole('status', { name: 'Staging countdown' })).not.toBeInTheDocument();
    });
  });

  describe('start-procedure UX + tone (Slice 3)', () => {
    it('shows the generic "arming… stand by" state in Armed (no precise countdown)', async () => {
      const { session } = makeTestSession({ live: liveAt('Armed') });
      render(LiveRaceControl, { session });
      await tick();
      const arming = await screen.findByRole('status', { name: 'Arming' });
      expect(arming).toHaveTextContent(/Arming… stand by/);
      // The randomness is hidden: no precise ms/seconds countdown is rendered.
      expect(arming).not.toHaveTextContent(/\d+\s*ms/);
    });

    // A stub platform AudioContext the screen's StartTonePlayer picks up, recording oscillator
    // starts + resume()/createOscillator calls so we can assert the tone path with no real audio.
    function installAudioStub(state = 'running') {
      const started: number[] = [];
      let resumes = 0;
      let oscillators = 0;
      class MockAudioContext {
        currentTime = 0;
        state = state;
        destination = {};
        createOscillator() {
          oscillators++;
          return {
            type: 'square',
            frequency: { setValueAtTime() {} },
            connect() {},
            start() {
              started.push(1);
            },
            stop() {}
          };
        }
        createGain() {
          return {
            gain: { setValueAtTime() {}, linearRampToValueAtTime() {} },
            connect() {}
          };
        }
        async resume() {
          resumes++;
          this.state = 'running';
        }
        async close() {}
      }
      vi.stubGlobal('AudioContext', MockAudioContext);
      // Ensure the mute pref reads unmuted regardless of any leaked storage.
      vi.stubGlobal('localStorage', {
        getItem: () => null,
        setItem: () => {},
        removeItem: () => {},
        clear: () => {},
        key: () => null,
        length: 0
      } as unknown as Storage);
      return {
        started,
        calls: () => ({ resumes, oscillators })
      };
    }

    it('plays the start tone when the heat enters Running (from Armed)', async () => {
      const { started } = installAudioStub('running');

      const { session, pushLive } = makeTestSession({ live: liveAt('Armed') });
      const { container } = render(LiveRaceControl, { session });
      await tick();
      expect(started).toHaveLength(0); // nothing plays while merely Armed

      pushLive(liveAt('Running'));
      await tick();
      // The tone fired once; the arming panel is gone and the race clock has taken over.
      expect(started).toHaveLength(1);
      expect(container.querySelector('.arming')).toBeNull();
      expect(screen.getByRole('timer')).toBeInTheDocument();

      vi.unstubAllGlobals();
    });

    it('does NOT fire when Running is the FIRST observed phase (late join / navigating to a running heat)', async () => {
      // The late-join bug: the RD navigates to the Live page while a heat is already Running. The
      // first phase the console observes for that heat is Running, with no pre-Running phase seen —
      // this is NOT a race-go the RD watched, so the tone must stay silent (no oscillator built).
      const { started } = installAudioStub('running');

      const { session, pushLive } = makeTestSession({ live: liveAt('Running') });
      render(LiveRaceControl, { session });
      await tick();
      expect(started).toHaveLength(0);

      // Repeated Running snapshots (progress updates) for the same already-running heat stay silent.
      pushLive(liveAt('Running'));
      await tick();
      expect(started).toHaveLength(0);

      vi.unstubAllGlobals();
    });

    it('fires once when Running is reached from a non-Armed prior phase (Staged → Running)', async () => {
      const { started } = installAudioStub('running');

      const { session, pushLive } = makeTestSession({ live: liveAt('Staged') });
      render(LiveRaceControl, { session });
      await tick();
      expect(started).toHaveLength(0);

      // Skip straight from Staged to Running (Armed snapshot never arrives).
      pushLive(liveAt('Running'));
      await tick();
      expect(started).toHaveLength(1);

      vi.unstubAllGlobals();
    });

    it('does not re-fire on repeated Running snapshots for the same heat', async () => {
      const { started } = installAudioStub('running');

      // Start Staged (a pre-Running phase observed first) so the genuine race-go arms + fires once.
      const { session, pushLive } = makeTestSession({ live: liveAt('Staged') });
      render(LiveRaceControl, { session });
      await tick();
      pushLive(liveAt('Running'));
      await tick();
      expect(started).toHaveLength(1);

      // Progress updates re-push the same Running heat — must NOT re-fire the tone.
      pushLive(liveAt('Running'));
      await tick();
      pushLive(liveAt('Running'));
      await tick();
      expect(started).toHaveLength(1);

      vi.unstubAllGlobals();
    });

    it('fires again for the NEXT heat (flag resets on heat change)', async () => {
      const { started } = installAudioStub('running');

      // heat-1 is watched through a genuine race-go (Staged → Running): its tone fires once.
      const { session, pushLive } = makeTestSession({ live: liveAt('Staged', 'heat-1') });
      render(LiveRaceControl, { session });
      await tick();
      pushLive(liveAt('Running', 'heat-1'));
      await tick();
      expect(started).toHaveLength(1);

      // The heat is swapped out (Scheduled, then a new heat goes Running) — a fresh tone fires (the
      // new heat had a pre-Running phase observed, so it's a genuine race-go, not a late join).
      pushLive(liveAt('Scheduled', 'heat-2'));
      await tick();
      pushLive(liveAt('Running', 'heat-2'));
      await tick();
      expect(started).toHaveLength(2);

      vi.unstubAllGlobals();
    });

    it('does not play the start tone when navigating to a NEW heat already Running (late join)', async () => {
      // A late join to a *different* heat than the one observed before: the new heat's first phase
      // is Running, with no pre-Running phase seen for it, so the tone stays silent.
      const { started } = installAudioStub('running');

      // heat-1 watched Staged → Running (fires once).
      const { session, pushLive } = makeTestSession({ live: liveAt('Staged', 'heat-1') });
      render(LiveRaceControl, { session });
      await tick();
      pushLive(liveAt('Running', 'heat-1'));
      await tick();
      expect(started).toHaveLength(1);

      // heat-2 appears already Running (never seen pre-Running) — suppressed.
      pushLive(liveAt('Running', 'heat-2'));
      await tick();
      expect(started).toHaveLength(1);

      vi.unstubAllGlobals();
    });

    it('does not render an inline Enable/Test-tone button (removed)', async () => {
      installAudioStub('suspended');

      const { session } = makeTestSession({ live: liveAt('Staged') });
      render(LiveRaceControl, { session });
      await tick();
      // The test-tone affordance is gone; only the mute toggle remains in the audio toolbar.
      expect(screen.queryByRole('button', { name: /Enable sound/ })).not.toBeInTheDocument();
      expect(screen.queryByRole('button', { name: /Test tone/ })).not.toBeInTheDocument();
      expect(screen.getByRole('button', { name: /Tone on|Tone off/ })).toBeInTheDocument();

      vi.unstubAllGlobals();
    });
  });

  describe('race clock (#62)', () => {
    // The pure `RaceClock` renders `M:SS.mmm` into a `role="timer"`; we read that text to
    // assert the SERVER-TIME-ANCHORED clock (#62 follow-up): the elapsed counts from the live
    // state's `race_started_at` (µs) and freezes at `race_ended_at - race_started_at`.
    const clockText = () => screen.getByRole('timer').textContent?.trim();

    // A `LiveRaceState` at the given phase, carrying server timing in **microseconds** (the wire
    // unit). `startedAtMs` / `endedAtMs` are the server's race-go / race-end instants in ms.
    const liveAt = (
      phase: LiveRaceState['phase'],
      opts: {
        heat?: string | undefined;
        startedAtMs?: number | null;
        endedAtMs?: number | null;
      } = {}
    ) => {
      const heat = 'heat' in opts ? opts.heat : 'heat-1';
      return {
        current_heat: heat,
        phase,
        race_started_at: opts.startedAtMs == null ? undefined : opts.startedAtMs * 1000,
        race_ended_at: opts.endedAtMs == null ? undefined : opts.endedAtMs * 1000
      } as LiveRaceState;
    };

    afterEach(() => {
      vi.useRealTimers();
    });

    it('starts ticking when the phase becomes Running, anchored to the server race-start', async () => {
      vi.useFakeTimers();
      vi.setSystemTime(0);
      const { session, pushLive } = makeTestSession({ live: liveAt('Armed') });
      render(LiveRaceControl, { session });
      // Idle/pre-race: clock sits at zero.
      expect(clockText()).toBe('0:00.000');

      // The server stamps race-go at t=0 (the current wall time).
      pushLive(liveAt('Running', { startedAtMs: 0 }));
      await tick();
      // Advance wall-clock + the tick interval; the clock reflects now - race_started_at. The
      // display only updates on a 50ms tick, so we advance by exact tick multiples.
      await vi.advanceTimersByTimeAsync(1_250);
      expect(clockText()).toBe('0:01.250');

      await vi.advanceTimersByTimeAsync(60_000);
      expect(clockText()).toBe('1:01.250');
    });

    it('reads the real elapsed when Running is observed AFTER race-go (late join)', async () => {
      // The bug: navigating to Live mid-race used to count from arrival (0), lagging the header.
      // Anchored to the server `race_started_at`, the first Running snapshot already reads the
      // real elapsed regardless of when this screen mounted.
      vi.useFakeTimers();
      vi.setSystemTime(7_000); // the race started 7s ago (race_started_at = 0)
      const { session, pushLive } = makeTestSession({ live: liveAt('Armed') });
      render(LiveRaceControl, { session });

      pushLive(liveAt('Running', { startedAtMs: 0 }));
      await tick();
      await vi.advanceTimersByTimeAsync(0);
      expect(clockText()).toBe('0:07.000');
    });

    it('freezes at the EXACT server duration on Unofficial, and stops ticking', async () => {
      vi.useFakeTimers();
      vi.setSystemTime(0);
      const { session, pushLive } = makeTestSession({
        live: liveAt('Running', { startedAtMs: 0 })
      });
      render(LiveRaceControl, { session });
      await tick();

      await vi.advanceTimersByTimeAsync(2_500);
      expect(clockText()).toBe('0:02.500');

      // The server closed the race at exactly 2.500s — freeze at race_ended_at - race_started_at.
      pushLive(liveAt('Unofficial', { startedAtMs: 0, endedAtMs: 2_500 }));
      await tick();
      expect(clockText()).toBe('0:02.500');

      // …and the interval is gone: more wall-clock time does not move the clock.
      await vi.advanceTimersByTimeAsync(5_000);
      expect(clockText()).toBe('0:02.500');
    });

    it('keeps the exact frozen value through Final', async () => {
      vi.useFakeTimers();
      vi.setSystemTime(0);
      const { session, pushLive } = makeTestSession({
        live: liveAt('Running', { startedAtMs: 0 })
      });
      render(LiveRaceControl, { session });
      await tick();
      await vi.advanceTimersByTimeAsync(3_000);

      pushLive(liveAt('Unofficial', { startedAtMs: 0, endedAtMs: 3_000 }));
      await tick();
      pushLive(liveAt('Final', { startedAtMs: 0, endedAtMs: 3_000 }));
      await tick();
      await vi.advanceTimersByTimeAsync(4_000);
      expect(clockText()).toBe('0:03.000');
    });

    it('resets the clock to zero when the heat goes back to Scheduled (e.g. after an abort)', async () => {
      vi.useFakeTimers();
      vi.setSystemTime(0);
      const { session, pushLive } = makeTestSession({
        live: liveAt('Running', { startedAtMs: 0 })
      });
      render(LiveRaceControl, { session });
      await tick();
      await vi.advanceTimersByTimeAsync(2_000);
      expect(clockText()).toBe('0:02.000');

      // An Abort/Restart folds the phase back to Scheduled (timing cleared) → reset to zero.
      pushLive(liveAt('Scheduled', { startedAtMs: null }));
      await tick();
      expect(clockText()).toBe('0:00.000');
    });

    it('resets to zero when there is no heat on the timer', async () => {
      vi.useFakeTimers();
      vi.setSystemTime(0);
      const { session, pushLive } = makeTestSession({
        live: liveAt('Running', { startedAtMs: 0 })
      });
      render(LiveRaceControl, { session });
      await tick();
      await vi.advanceTimersByTimeAsync(1_500);

      // No current heat → phase defaults to Scheduled → reset.
      pushLive(liveAt('Scheduled', { heat: undefined, startedAtMs: null }));
      await tick();
      expect(clockText()).toBe('0:00.000');
    });

    it('does not restart the clock on a repeated Running push (rapid same-phase flips)', async () => {
      vi.useFakeTimers();
      vi.setSystemTime(0);
      const { session, pushLive } = makeTestSession({
        live: liveAt('Running', { startedAtMs: 0 })
      });
      render(LiveRaceControl, { session });
      await tick();
      await vi.advanceTimersByTimeAsync(2_000);
      expect(clockText()).toBe('0:02.000');

      // Another Running snapshot (same server anchor) must not reset the start.
      pushLive(liveAt('Running', { startedAtMs: 0 }));
      await tick();
      await vi.advanceTimersByTimeAsync(1_000);
      expect(clockText()).toBe('0:03.000');
    });
  });
});

// ── Open-practice per-channel board + reset (open-practice Slice 2) ────────────────────────────

const OP_TIMER: Timer = {
  id: 'mock',
  name: 'Mock',
  kind: { Mock: { laps: 3, lap_ms: 30000 } },
  status: 'Ready',
  channel_capability: 'Flexible',
  node_count: 2,
  available_channels: [5658, 5800]
};
const OP_CATALOG: ChannelCatalogEntry[] = [
  { band: 'Raceband', channel: 'R1', mhz: 5658 },
  { band: 'Fatshark', channel: 'F4', mhz: 5800 }
];
const OP_ROUND: RoundDef = {
  id: 'rp',
  label: 'Open Practice',
  classes: [],
  format: 'open_practice',
  params: {},
  win_condition: 'BestLap',
  seeding: { AllChannels: { channels: [0, 1] } },
  channel_mode: 'Static',
  staging_timer_secs: 300,
  start_procedure: { mode: 'randomized-delay', min_delay_ms: 2000, max_delay_ms: 5000 },
  grace_window: { Duration: { micros: 3_000_000 } }
};
const OP_EVENT: EventMeta = {
  id: 'e1',
  name: 'Friday',
  created_at: 0,
  persistent: true,
  timers: ['mock'],
  roster: [],
  classes: [],
  rounds: [OP_ROUND]
};
const OP_HEAT: HeatSummary = {
  heat: 'practice-1',
  lineup: ['node-0', 'node-1'],
  round: 'rp',
  class: undefined,
  frequencies: [],
  phase: 'Running',
  is_current: true
};
// A live open-practice state: two channels, node-0 with 3 laps (last 28.0s), node-1 quiet.
const opLive: LiveRaceState = {
  current_heat: 'practice-1',
  phase: 'Running',
  active_pilots: ['node-0', 'node-1'],
  progress: [
    { competitor: 'node-0', laps_completed: 3, last_lap_micros: 28_000_000 },
    { competitor: 'node-1', laps_completed: 0 }
  ],
  running_order: ['node-0', 'node-1']
};

describe('LiveRaceControl — open-practice per-channel board', () => {
  function renderBoard(extra?: Parameters<typeof makeTestSession>[0]) {
    return makeTestSession({
      event: OP_EVENT,
      live: opLive,
      listHeatsImpl: vi.fn(async () => [OP_HEAT]),
      listChannelsImpl: vi.fn(async () => OP_CATALOG),
      listTimersImpl: vi.fn(async () => [OP_TIMER]),
      ...extra
    });
  }

  it('renders a per-channel board labelling each node by its timer channel, with laps + best lap', async () => {
    const { session } = renderBoard();
    render(LiveRaceControl, { session });

    // The board replaces the pilot-keyed panels; rows are keyed by channel.
    const r1 = await screen.findByLabelText('Channel Raceband R1 · 5658');
    expect(r1).toBeInTheDocument();
    expect(screen.getByLabelText('Channel Fatshark F4 · 5800')).toBeInTheDocument();

    // node-0 shows 3 laps and a best lap of 28.0s (formatMicros), tracked from the last lap.
    expect(within(r1).getByText('3')).toBeInTheDocument();
    expect(within(r1).getAllByText('28.000').length).toBeGreaterThan(0);
  });

  it('tracks best lap as the min last-lap across the run', async () => {
    const { session, pushLive } = renderBoard();
    render(LiveRaceControl, { session });
    const r1 = await screen.findByLabelText('Channel Raceband R1 · 5658');
    // Both Last and Best read 28.0s on the first snapshot (best seeds from the only lap).
    await within(r1).findAllByText('28.000');

    // A faster lap arrives → best updates to 25.0s while last shows 25.0s too.
    pushLive({
      ...opLive,
      progress: [
        { competitor: 'node-0', laps_completed: 4, last_lap_micros: 25_000_000 },
        { competitor: 'node-1', laps_completed: 0 }
      ]
    });
    // Last + Best both now read 25.0s.
    await waitFor(() => expect(within(r1).getAllByText('25.000')).toHaveLength(2));

    // A slower lap must NOT regress the best (still 25.0s), though last becomes 30.0s.
    pushLive({
      ...opLive,
      progress: [
        { competitor: 'node-0', laps_completed: 5, last_lap_micros: 30_000_000 },
        { competitor: 'node-1', laps_completed: 0 }
      ]
    });
    await within(r1).findByText('30.000');
    // Best lap (25.0s) is still present in the row.
    expect(within(r1).getByText('25.000')).toBeInTheDocument();
  });

  it('the New run control fills the open-practice round to clear the board', async () => {
    const { session, sendSpy } = renderBoard();
    render(LiveRaceControl, { session });

    const reset = await screen.findByRole('button', { name: /New run/ });
    await fireEvent.click(reset);

    await waitFor(() => expect(sendSpy.mock.calls.some((c) => 'FillRound' in c[0])).toBe(true));
    expect(sendSpy.mock.calls.find((c) => 'FillRound' in c[0])![0]).toEqual({
      FillRound: { round: 'rp' }
    });
  });

  it('does not show the pilot-keyed panels for an open-practice heat', async () => {
    const { session } = renderBoard();
    render(LiveRaceControl, { session });
    await screen.findByLabelText('Channel Raceband R1 · 5658');
    // The normal Heat sheet / Live standing panels are replaced by the practice board.
    expect(screen.queryByText('Heat sheet')).not.toBeInTheDocument();
    expect(screen.queryByText('Live standing')).not.toBeInTheDocument();
  });
});
