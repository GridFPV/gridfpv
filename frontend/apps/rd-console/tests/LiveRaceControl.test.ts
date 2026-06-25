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
  grace_window: { Duration: { micros: 3_000_000 } },
  protest_window: 'Off'
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

  describe('heat picker (manual current-heat selection)', () => {
    // Two heats in the same round: heat-1 (current) and heat-2 (filled, on deck).
    const HEAT_2: HeatSummary = {
      heat: 'heat-2',
      lineup: ['CARLA', 'DAN'],
      round: 'r1',
      class: 'c1',
      frequencies: [],
      phase: 'Scheduled',
      is_current: false
    };

    it('lists the round heats by their shared display name, marking the current one', async () => {
      const { session } = makeTestSession({
        event: EVENT_WITH_ROUND,
        live: liveAt('Staged', 'heat-1'),
        listHeatsImpl: vi.fn(async () => [HEAT_IN_ROUND, HEAT_2])
      });
      render(LiveRaceControl, { session });

      const select = (await screen.findByRole('combobox', {
        name: 'Select current heat'
      })) as HTMLSelectElement;
      const labels = Array.from(select.options).map((o) => o.textContent?.trim());
      // "<Round> Heat N" names from the shared helper; the current heat is flagged.
      expect(labels).toContain('Qualifying R1 Heat 1 (current)');
      expect(labels).toContain('Qualifying R1 Heat 2');
      // The select tracks the live current heat.
      expect(select.value).toBe('heat-1');
    });

    it('sends SetCurrentHeat for the picked heat (when the current heat is Scheduled)', async () => {
      const { session, sendSpy } = makeTestSession({
        event: EVENT_WITH_ROUND,
        live: liveAt('Scheduled', 'heat-1'),
        listHeatsImpl: vi.fn(async () => [
          { ...HEAT_IN_ROUND, phase: 'Scheduled' as const },
          HEAT_2
        ])
      });
      render(LiveRaceControl, { session });

      const select = (await screen.findByRole('combobox', {
        name: 'Select current heat'
      })) as HTMLSelectElement;
      expect(select.disabled).toBe(false);
      await fireEvent.change(select, { target: { value: 'heat-2' } });
      expect(sendSpy).toHaveBeenCalledWith({ SetCurrentHeat: { heat: 'heat-2' } });
    });

    // The picker is LOCKED once the current heat is mid-commit (Staged/Armed/Running): after
    // Stage you're committed to that race, so the only way to switch is to abort it back to
    // Scheduled or finish to Unofficial/Final. Mirrors the backend's authoritative rejection.
    for (const phase of ['Staged', 'Armed', 'Running'] as const) {
      it(`disables the picker and shows the lock hint while the current heat is ${phase}`, async () => {
        const { session, sendSpy } = makeTestSession({
          event: EVENT_WITH_ROUND,
          live: liveAt(phase, 'heat-1'),
          listHeatsImpl: vi.fn(async () => [{ ...HEAT_IN_ROUND, phase }, HEAT_2])
        });
        render(LiveRaceControl, { session });

        const select = (await screen.findByRole('combobox', {
          name: 'Select current heat'
        })) as HTMLSelectElement;
        expect(select.disabled).toBe(true);
        // The inline hint explains why and how to switch.
        expect(screen.getByTestId('heat-pick-lock-hint')).toHaveTextContent(
          /Locked while a heat is staged\/running/
        );
        // A guarded change does not send (belt-and-suspenders with the disabled attribute).
        await fireEvent.change(select, { target: { value: 'heat-2' } });
        expect(sendSpy).not.toHaveBeenCalled();
      });
    }

    // The drift/defer-apply bug (#…): while locked, forcing the select's value to a different heat
    // must (a) never send SetCurrentHeat, and (b) snap the displayed value straight back to the
    // current heat — so nothing stale survives to apply once the picker unlocks (e.g. after Abort).
    it('snaps the value back to the current heat when a change is attempted while locked', async () => {
      const { session, sendSpy } = makeTestSession({
        event: EVENT_WITH_ROUND,
        live: liveAt('Staged', 'heat-1'),
        listHeatsImpl: vi.fn(async () => [HEAT_IN_ROUND, HEAT_2])
      });
      render(LiveRaceControl, { session });

      const select = (await screen.findByRole('combobox', {
        name: 'Select current heat'
      })) as HTMLSelectElement;
      expect(select.disabled).toBe(true);
      expect(select.value).toBe('heat-1');

      // Force the value to the other heat (as a stuck-drift would) and fire change.
      select.value = 'heat-2';
      await fireEvent.change(select, { target: { value: 'heat-2' } });
      await tick();

      // It snapped back to the current heat and never sent — no drifted selection lingers.
      expect(select.value).toBe('heat-1');
      expect(sendSpy).not.toHaveBeenCalled();
    });

    for (const phase of ['Scheduled', 'Unofficial', 'Final'] as const) {
      it(`enables the picker (no lock hint) while the current heat is ${phase}`, async () => {
        const { session } = makeTestSession({
          event: EVENT_WITH_ROUND,
          live: liveAt(phase, 'heat-1'),
          listHeatsImpl: vi.fn(async () => [{ ...HEAT_IN_ROUND, phase }, HEAT_2])
        });
        render(LiveRaceControl, { session });

        const select = (await screen.findByRole('combobox', {
          name: 'Select current heat'
        })) as HTMLSelectElement;
        expect(select.disabled).toBe(false);
        expect(screen.queryByTestId('heat-pick-lock-hint')).not.toBeInTheDocument();
      });
    }
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
  grace_window: { Duration: { micros: 3_000_000 } },
  protest_window: 'Off'
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
    // Open practice single-steps — the New run fills its one channel heat (mode 'Next', #216).
    expect(sendSpy.mock.calls.find((c) => 'FillRound' in c[0])![0]).toEqual({
      FillRound: { round: 'rp', mode: 'Next' }
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

// ── Friendly names everywhere (heat names, on-deck, pilot callsigns) ───────────────────────────

describe('LiveRaceControl — friendly names (no raw ids/refs)', () => {
  // A round + two of its heats, so the heat id resolves to "<Round> Heat N".
  const FN_ROUND: RoundDef = {
    id: 'r1',
    label: 'Qualifying R1',
    classes: ['c1'],
    format: 'timed_qual',
    params: {},
    win_condition: { Timed: { window_micros: 120_000_000 } },
    seeding: 'FromRoster',
    channel_mode: 'Static',
    staging_timer_secs: 300,
    start_procedure: { mode: 'randomized-delay', min_delay_ms: 2000, max_delay_ms: 5000 },
    grace_window: { Duration: { micros: 3_000_000 } },
    protest_window: 'Off'
  };
  const FN_EVENT: EventMeta = {
    id: 'e1',
    name: 'Friday',
    created_at: 0,
    persistent: true,
    timers: ['mock'],
    roster: [],
    classes: ['c1'],
    rounds: [FN_ROUND]
  };
  const HEAT_1: HeatSummary = {
    heat: 'p1-939hvr-heat',
    lineup: ['node-0', 'node-1'],
    round: 'r1',
    class: 'c1',
    frequencies: [],
    phase: 'Running',
    is_current: true
  };
  const HEAT_2: HeatSummary = {
    heat: 'p2-deadbeef-heat',
    lineup: ['node-0'],
    round: 'r1',
    class: 'c1',
    frequencies: [],
    phase: 'Scheduled',
    is_current: false
  };

  // The pilots directory: one bound pilot. node-0 is bound to it; node-1 is an unbound seat.
  const PILOTS = [{ id: 'pilot-1', callsign: 'Maverick', vtx_types: [] }];

  // A live heat: node-0 bound to pilot-1 (renders "Maverick"); node-1 unbound (renders its channel
  // label, NOT "node-1"). The heat is in a round so the title resolves to "Qualifying R1 Heat 1",
  // and an on-deck heat resolves to its own friendly name.
  const fnLive: LiveRaceState = {
    current_heat: 'p1-939hvr-heat',
    phase: 'Running',
    active_pilots: ['node-0', 'node-1'],
    progress: [
      { competitor: 'node-0', pilot: 'pilot-1', laps_completed: 2, last_lap_micros: 40_000_000 },
      { competitor: 'node-1', laps_completed: 1, last_lap_micros: 42_000_000 }
    ],
    running_order: ['node-0', 'node-1'],
    on_deck: 'p2-deadbeef-heat'
  };
  // A timer + catalog so the unbound node-1 seat resolves to a channel label (not "node-1"). The
  // heat carries per-seat frequencies so the channels panel + the seat name resolve to the band.
  const FN_TIMER: Timer = {
    id: 'mock',
    name: 'Mock',
    kind: { Mock: { laps: 3, lap_ms: 30000 } },
    status: 'Ready',
    channel_capability: 'Flexible',
    node_count: 2,
    available_channels: [5658, 5800]
  };
  const FN_CATALOG: ChannelCatalogEntry[] = [
    { band: 'Raceband', channel: 'R1', mhz: 5658 },
    { band: 'Fatshark', channel: 'F4', mhz: 5800 }
  ];
  // The current heat's per-seat frequencies (node-0 → R1, node-1 → F4), so the channel-label
  // fallback has something to resolve for the unbound node-1 seat.
  const HEAT_1_FREQ: HeatSummary = {
    ...HEAT_1,
    frequencies: [
      ['node-0', 5658],
      ['node-1', 5800]
    ]
  };

  function renderFN() {
    return makeTestSession({
      event: FN_EVENT,
      live: fnLive,
      listHeatsImpl: vi.fn(async () => [HEAT_1_FREQ, HEAT_2]),
      listChannelsImpl: vi.fn(async () => FN_CATALOG),
      listTimersImpl: vi.fn(async () => [FN_TIMER]),
      listPilotsImpl: vi.fn(async () => PILOTS as unknown as never)
    });
  }

  it('renders the current-heat title as its "<Round> Heat N" name, not the raw heat id', async () => {
    const { session } = renderFN();
    render(LiveRaceControl, { session });
    const title = document.querySelector('.heat-id .value') as HTMLElement;
    await waitFor(() => expect(title.textContent?.trim()).toBe('Qualifying R1 Heat 1'));
    expect(title.textContent).not.toContain('p1-939hvr-heat');
  });

  it('renders the on-deck heat by its friendly name, not the raw id', async () => {
    const { session } = renderFN();
    render(LiveRaceControl, { session });
    const ondeck = await screen.findByText('On deck');
    const value = ondeck.parentElement?.querySelector('.value') as HTMLElement;
    await waitFor(() => expect(value.textContent?.trim()).toBe('Qualifying R1 Heat 2'));
  });

  it('renders the HeatSheet heading as the friendly heat name', async () => {
    const { session } = renderFN();
    render(LiveRaceControl, { session });
    const sheet = screen.getByRole('region', { name: 'Heat sheet' });
    await waitFor(() =>
      expect(within(sheet).getByRole('heading')).toHaveTextContent('Qualifying R1 Heat 1')
    );
  });

  it('renders a bound seat by its pilot callsign in the heat sheet (not the ref)', async () => {
    const { session } = renderFN();
    render(LiveRaceControl, { session });
    const sheet = screen.getByRole('region', { name: 'Heat sheet' });
    await waitFor(() => expect(within(sheet).getByText('Maverick')).toBeInTheDocument());
    // The raw ref never shows for the bound seat.
    expect(within(sheet).queryByText('node-0')).not.toBeInTheDocument();
  });

  it('renders an unbound seat by its channel label, never "node-1"', async () => {
    const { session } = renderFN();
    render(LiveRaceControl, { session });
    const sheet = screen.getByRole('region', { name: 'Heat sheet' });
    // node-1 is unbound → its channel label (Fatshark F4 · 5800) shows; the raw seat ref does not.
    await waitFor(() => expect(within(sheet).getByText(/Fatshark F4/)).toBeInTheDocument());
    expect(within(sheet).queryByText('node-1')).not.toBeInTheDocument();
  });

  it('renders the live standing rows by callsign / channel label, not refs', async () => {
    const { session } = renderFN();
    render(LiveRaceControl, { session });
    const standing = screen.getByRole('table', { name: 'Heat leaderboard' });
    await waitFor(() => expect(within(standing).getByText('Maverick')).toBeInTheDocument());
    expect(within(standing).getByText(/Fatshark F4/)).toBeInTheDocument();
    expect(within(standing).queryByText('node-0')).not.toBeInTheDocument();
    expect(within(standing).queryByText('node-1')).not.toBeInTheDocument();
  });
});

// ── Roster-seeded pilot callsigns resolve from the roster binding, BEFORE the heat runs ──────────
//
// The bug #212's e2e missed: a normal `FromRoster` heat seeds each competitor ref to the **pilot id
// itself** and emits NO `CompetitorRegistered` event, so `progress[].pilot` is `null` in *every*
// phase (confirmed on a throwaway Director — Scheduled, Staged, Armed, AND Running all carry a
// null `pilot`). #212 resolved the callsign only from `progress[].pilot`, so it fell back to the raw
// id (`maverick-4d9rp8`) everywhere a pilot appears — heat sheet, leaderboard, channels panel. The
// callsign must instead resolve from the **always-available roster binding**: the ref *is* the
// pilot id, looked up in the `/pilots` directory — independent of the race phase / progress.
describe('LiveRaceControl — roster-seeded callsigns (resolve pre-race, no progress binding)', () => {
  const RS_ROUND: RoundDef = {
    id: 'r1',
    label: 'Qualifying R1',
    classes: ['c1'],
    format: 'timed_qual',
    params: {},
    win_condition: { Timed: { window_micros: 120_000_000 } },
    seeding: 'FromRoster',
    channel_mode: 'Static',
    staging_timer_secs: 300,
    start_procedure: { mode: 'randomized-delay', min_delay_ms: 2000, max_delay_ms: 5000 },
    grace_window: { Duration: { micros: 3_000_000 } },
    protest_window: 'Off'
  };
  const RS_EVENT: EventMeta = {
    id: 'e1',
    name: 'Friday',
    created_at: 0,
    persistent: true,
    timers: ['mock'],
    roster: ['maverick-4d9rp8', 'goose-yla6dp'],
    classes: ['c1'],
    rounds: [RS_ROUND]
  };
  // The directory: callsigns keyed by the pilot id (which IS the competitor ref for a roster heat).
  const RS_PILOTS = [
    { id: 'maverick-4d9rp8', callsign: 'Maverick', vtx_types: [] },
    { id: 'goose-yla6dp', callsign: 'Goose', vtx_types: [] }
  ];
  // The wire shape a real roster-seeded heat produces: refs are the pilot ids, frequencies are
  // assigned, and crucially `progress[].pilot` is ABSENT (no registration event was ever emitted).
  const rsHeat = (phase: HeatSummary['phase']): HeatSummary => ({
    heat: 'qualifying-r1-tj8x88-r1-h1',
    lineup: ['maverick-4d9rp8', 'goose-yla6dp'],
    round: 'r1',
    class: 'c1',
    frequencies: [
      ['maverick-4d9rp8', 5658],
      ['goose-yla6dp', 5695]
    ],
    phase,
    is_current: true
  });
  // A live state with NO `pilot` binding on its progress rows — exactly what the backend emits for a
  // roster-seeded heat (verified on a throwaway). Defaults to the not-yet-running Scheduled phase.
  const rsLive = (phase: LiveRaceState['phase'] = 'Scheduled'): LiveRaceState => ({
    current_heat: 'qualifying-r1-tj8x88-r1-h1',
    phase,
    active_pilots: ['maverick-4d9rp8', 'goose-yla6dp'],
    progress: [
      { competitor: 'maverick-4d9rp8', laps_completed: 0 },
      { competitor: 'goose-yla6dp', laps_completed: 0 }
    ],
    running_order: ['maverick-4d9rp8', 'goose-yla6dp']
  });
  const RS_CATALOG: ChannelCatalogEntry[] = [
    { band: 'Raceband', channel: 'R1', mhz: 5658 },
    { band: 'Raceband', channel: 'R2', mhz: 5695 }
  ];
  function renderRS(phase: LiveRaceState['phase'] = 'Scheduled') {
    return makeTestSession({
      event: RS_EVENT,
      live: rsLive(phase),
      listHeatsImpl: vi.fn(async () => [rsHeat(phase as HeatSummary['phase'])]),
      listChannelsImpl: vi.fn(async () => RS_CATALOG),
      listPilotsImpl: vi.fn(async () => RS_PILOTS as unknown as never)
    });
  }

  // The KEY regression: a Scheduled (NOT running) heat of rostered pilots renders callsigns.
  it('renders callsigns in the heat sheet of a NOT-running (Scheduled) heat', async () => {
    const { session } = renderRS('Scheduled');
    render(LiveRaceControl, { session });
    const sheet = screen.getByRole('region', { name: 'Heat sheet' });
    await waitFor(() => expect(within(sheet).getByText('Maverick')).toBeInTheDocument());
    expect(within(sheet).getByText('Goose')).toBeInTheDocument();
    // The raw pilot-id refs never show.
    expect(within(sheet).queryByText('maverick-4d9rp8')).not.toBeInTheDocument();
    expect(within(sheet).queryByText('goose-yla6dp')).not.toBeInTheDocument();
  });

  it('renders callsigns in the channels panel of a NOT-running (Scheduled) heat', async () => {
    const { session } = renderRS('Scheduled');
    render(LiveRaceControl, { session });
    const channels = await screen.findByRole('list', { name: 'Heat channels' });
    await waitFor(() => expect(within(channels).getByText('Maverick')).toBeInTheDocument());
    expect(within(channels).getByText('Goose')).toBeInTheDocument();
    expect(within(channels).queryByText('maverick-4d9rp8')).not.toBeInTheDocument();
  });

  // Staged (still not running) resolves the same way.
  it('renders callsigns in the heat sheet of a Staged heat', async () => {
    const { session } = renderRS('Staged');
    render(LiveRaceControl, { session });
    const sheet = screen.getByRole('region', { name: 'Heat sheet' });
    await waitFor(() => expect(within(sheet).getByText('Maverick')).toBeInTheDocument());
    expect(within(sheet).queryByText('maverick-4d9rp8')).not.toBeInTheDocument();
  });

  // The running case (which #212's progress-only resolver also could not satisfy for a real roster
  // heat, since progress.pilot is null) stays green via the same roster binding.
  it('renders callsigns when the roster heat IS running (progress still carries no pilot)', async () => {
    const { session } = makeTestSession({
      event: RS_EVENT,
      // Running, with laps banked but STILL no `pilot` binding on the progress rows.
      live: {
        ...rsLive('Running'),
        progress: [
          { competitor: 'maverick-4d9rp8', laps_completed: 2, last_lap_micros: 2_500_000 },
          { competitor: 'goose-yla6dp', laps_completed: 2, last_lap_micros: 2_600_000 }
        ]
      },
      listHeatsImpl: vi.fn(async () => [rsHeat('Running')]),
      listChannelsImpl: vi.fn(async () => RS_CATALOG),
      listPilotsImpl: vi.fn(async () => RS_PILOTS as unknown as never)
    });
    render(LiveRaceControl, { session });
    const sheet = screen.getByRole('region', { name: 'Heat sheet' });
    await waitFor(() => expect(within(sheet).getByText('Maverick')).toBeInTheDocument());
    const standing = screen.getByRole('table', { name: 'Heat leaderboard' });
    expect(within(standing).getByText('Goose')).toBeInTheDocument();
    expect(within(standing).queryByText('goose-yla6dp')).not.toBeInTheDocument();
  });
});
