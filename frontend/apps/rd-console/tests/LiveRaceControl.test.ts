import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/dom';
import { tick } from 'svelte';
import type { EventMeta, HeatSummary, LiveRaceState, RoundDef } from '@gridfpv/types';
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

    it('plays the start tone exactly on the Armed → Running edge', async () => {
      // Stub the platform AudioContext so the screen's StartTonePlayer picks it up and we can
      // observe an oscillator start at race-go (no real audio). Default-unmuted (no stored pref).
      const started: number[] = [];
      class MockAudioContext {
        currentTime = 0;
        state = 'running';
        destination = {};
        createOscillator() {
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
        async resume() {}
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

      const { session, pushLive } = makeTestSession({ live: liveAt('Armed') });
      const { container } = render(LiveRaceControl, { session });
      await tick();
      expect(started).toHaveLength(0); // nothing plays while merely Armed

      pushLive(liveAt('Running'));
      await tick();
      // The tone fired once on the edge; the arming panel is gone and the race clock has taken over.
      expect(started).toHaveLength(1);
      expect(container.querySelector('.arming')).toBeNull();
      expect(screen.getByRole('timer')).toBeInTheDocument();

      vi.unstubAllGlobals();
    });
  });

  describe('race clock (#62)', () => {
    // The pure `RaceClock` renders `M:SS.mmm` into a `role="timer"`; we read that text to
    // assert the client-side ticking driven from the live `phase` in LiveRaceControl.
    const clockText = () => screen.getByRole('timer').textContent?.trim();

    // A bare `LiveRaceState` at the given phase (with/without a heat on the timer).
    const liveAt = (phase: LiveRaceState['phase'], heat: string | undefined = 'heat-1') =>
      ({ current_heat: heat, phase }) as LiveRaceState;

    afterEach(() => {
      vi.useRealTimers();
    });

    it('starts ticking when the phase becomes Running', async () => {
      vi.useFakeTimers();
      vi.setSystemTime(0);
      const { session, pushLive } = makeTestSession({ live: liveAt('Armed') });
      render(LiveRaceControl, { session });
      // Idle/pre-race: clock sits at zero.
      expect(clockText()).toBe('0:00.000');

      pushLive(liveAt('Running'));
      await tick();
      // Advance wall-clock + the tick interval; the clock reflects the elapsed time. The
      // display only updates on a 50ms tick, so we advance by exact tick multiples.
      await vi.advanceTimersByTimeAsync(1_250);
      expect(clockText()).toBe('0:01.250');

      await vi.advanceTimersByTimeAsync(60_000);
      expect(clockText()).toBe('1:01.250');
    });

    it('freezes the clock when the phase becomes Unofficial, and stops ticking', async () => {
      vi.useFakeTimers();
      vi.setSystemTime(0);
      const { session, pushLive } = makeTestSession({ live: liveAt('Running') });
      render(LiveRaceControl, { session });
      await tick();

      await vi.advanceTimersByTimeAsync(2_500);
      expect(clockText()).toBe('0:02.500');

      // Finishing freezes the displayed value…
      pushLive(liveAt('Unofficial'));
      await tick();
      expect(clockText()).toBe('0:02.500');

      // …and the interval is gone: more wall-clock time does not move the clock.
      await vi.advanceTimersByTimeAsync(5_000);
      expect(clockText()).toBe('0:02.500');
    });

    it('keeps the frozen value through Final', async () => {
      vi.useFakeTimers();
      vi.setSystemTime(0);
      const { session, pushLive } = makeTestSession({ live: liveAt('Running') });
      render(LiveRaceControl, { session });
      await tick();
      await vi.advanceTimersByTimeAsync(3_000);

      pushLive(liveAt('Unofficial'));
      await tick();
      pushLive(liveAt('Final'));
      await tick();
      await vi.advanceTimersByTimeAsync(4_000);
      expect(clockText()).toBe('0:03.000');
    });

    it('resets the clock to zero when the heat goes back to Scheduled (e.g. after an abort)', async () => {
      vi.useFakeTimers();
      vi.setSystemTime(0);
      const { session, pushLive } = makeTestSession({ live: liveAt('Running') });
      render(LiveRaceControl, { session });
      await tick();
      await vi.advanceTimersByTimeAsync(2_000);
      expect(clockText()).toBe('0:02.000');

      // An Abort/Restart folds the phase back to Scheduled → reset to zero.
      pushLive(liveAt('Scheduled'));
      await tick();
      expect(clockText()).toBe('0:00.000');
    });

    it('resets to zero when there is no heat on the timer', async () => {
      vi.useFakeTimers();
      vi.setSystemTime(0);
      const { session, pushLive } = makeTestSession({ live: liveAt('Running') });
      render(LiveRaceControl, { session });
      await tick();
      await vi.advanceTimersByTimeAsync(1_500);

      // No current heat → phase defaults to Scheduled → reset.
      pushLive(liveAt('Scheduled', undefined));
      await tick();
      expect(clockText()).toBe('0:00.000');
    });

    it('does not restart the clock on a repeated Running push (rapid same-phase flips)', async () => {
      vi.useFakeTimers();
      vi.setSystemTime(0);
      const { session, pushLive } = makeTestSession({ live: liveAt('Running') });
      render(LiveRaceControl, { session });
      await tick();
      await vi.advanceTimersByTimeAsync(2_000);
      expect(clockText()).toBe('0:02.000');

      // Another Running snapshot (e.g. progress update) must not reset the start.
      pushLive(liveAt('Running'));
      await tick();
      await vi.advanceTimersByTimeAsync(1_000);
      expect(clockText()).toBe('0:03.000');
    });
  });
});
