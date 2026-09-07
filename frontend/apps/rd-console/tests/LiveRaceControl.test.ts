import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor, within } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/dom';
import { tick } from 'svelte';
import type {
  ChannelCatalogEntry,
  ChannelLayout,
  EventMeta,
  HeatSummary,
  LiveCrossing,
  LiveRaceState,
  RoundDef,
  Timer
} from '@gridfpv/types';
import LiveRaceControl from '../src/screens/LiveRaceControl.svelte';
import { makeTestSession } from './support.js';
import AudioHost from './AudioHost.svelte';
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
  name: 'Qualifying R1 Heat 1',
  lineup: ['ALICE', 'BOB'],
  round: 'r1',
  class: 'c1',
  frequencies: [],
  phase: 'Staged',
  is_current: true
};
const liveAt = (phase: LiveRaceState['phase'], heat: string | undefined = 'heat-1') =>
  ({ current_heat: heat, phase }) as LiveRaceState;

// A stub platform AudioContext the screen's RaceAudioPlayer picks up, recording each started
// oscillator's FREQUENCY (so a test can tell the start tone 880 / countdown pip 880 / race-end
// buzzer 440 / crossing pip 1760 apart) plus resume()/createOscillator counts — no real audio.
// `calloutsMuted` seeds the persisted callouts-mute pref (the informational-layer toggle).
function installAudioStub(state = 'running', opts?: { calloutsMuted?: boolean }) {
  const started: number[] = [];
  let resumes = 0;
  let oscillators = 0;
  class MockAudioContext {
    currentTime = 0;
    state = state;
    destination = {};
    createOscillator() {
      oscillators++;
      let freq = 0;
      return {
        type: 'square',
        frequency: {
          setValueAtTime(value: number) {
            freq = value;
          }
        },
        connect() {},
        start() {
          started.push(freq);
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
  // Seed the callouts-mute pref: unmuted by default; 'true' asserts the mute SCOPE (informational
  // layer only — the procedure tones must ignore it).
  vi.stubGlobal('localStorage', {
    getItem: (key: string) =>
      key === 'gridfpv.callouts.muted' && opts?.calloutsMuted ? 'true' : null,
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

// A stub Web Speech API (jsdom has neither speechSynthesis nor SpeechSynthesisUtterance): records
// every spoken utterance so the callout texts + the cancel-on-heat-end path are observable.
function installSpeechStub() {
  const utterances: Array<{ text: string; onend: (() => void) | null }> = [];
  const cancelSpy = vi.fn();
  vi.stubGlobal('speechSynthesis', {
    speak: (u: { text: string; onend: (() => void) | null }) => utterances.push(u),
    cancel: cancelSpy
  });
  class MockUtterance {
    text: string;
    onend: (() => void) | null = null;
    onerror: (() => void) | null = null;
    constructor(text: string) {
      this.text = text;
    }
  }
  vi.stubGlobal('SpeechSynthesisUtterance', MockUtterance);
  return { utterances, cancelSpy };
}

describe('LiveRaceControl', () => {
  it('enables only the phase-legal transitions (Running → ForceEnd/Abort/Restart)', () => {
    const { session } = makeTestSession({ live: liveRunning });
    render(LiveRaceControl, { session });

    const btn = (label: string) => screen.getByRole('button', { name: label }) as HTMLButtonElement;
    // Stop (the ForceEnd command) + off-ramps legal in Running (the manual Finish is gone —
    // the clock auto-completes; Stop is the plain manual end).
    expect(btn('Stop').disabled).toBe(false);
    expect(btn('Abort').disabled).toBe(false);
    expect(btn('Restart').disabled).toBe(false);
    // Illegal in Running.
    expect(btn('Stage').disabled).toBe(true);
    expect(btn('Start').disabled).toBe(true);
    expect(btn('Finalize').disabled).toBe(true);
    expect(btn('Advance').disabled).toBe(true);
    expect(btn('Revert').disabled).toBe(true);
    expect(btn('Discard').disabled).toBe(true);
    // SkipCountdown is retired from the console entirely — no button renders for it.
    expect(screen.queryByRole('button', { name: /Skip/ })).toBeNull();
  });

  it('Stop fires the ForceEnd command (the wire name is unchanged)', async () => {
    const { session, sendSpy } = makeTestSession({ live: liveRunning });
    render(LiveRaceControl, { session });

    await fireEvent.click(screen.getByRole('button', { name: 'Stop' }));
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

    await fireEvent.click(screen.getByRole('button', { name: 'Stop' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('illegal transition');
  });

  it('renders the live leaderboard from the running order', () => {
    const { session } = makeTestSession({ live: liveRunning });
    render(LiveRaceControl, { session });
    // Heat sheet + live standing both list the lineup.
    expect(screen.getAllByText('ALICE').length).toBeGreaterThan(0);
  });

  it('Armed exposes only the off-ramps — the countdown runs itself, no Skip, no override tag', () => {
    const { session } = makeTestSession({ live: liveAt('Armed') });
    render(LiveRaceControl, { session });
    const btn = (label: string) => screen.getByRole('button', { name: label }) as HTMLButtonElement;
    expect(btn('Abort').disabled).toBe(false);
    expect(btn('Restart').disabled).toBe(false);
    // Forward steps are illegal in Armed (the runtime clock drives Armed → Running).
    expect(btn('Stage').disabled).toBe(true);
    expect(btn('Start').disabled).toBe(true);
    expect(btn('Stop').disabled).toBe(true);
    expect(btn('Finalize').disabled).toBe(true);
    // The whole override concept is retired: no Skip button, no "override" tag anywhere.
    expect(screen.queryByRole('button', { name: /Skip/ })).toBeNull();
    expect(screen.queryByText('override')).toBeNull();
  });

  describe('heat picker (manual current-heat selection)', () => {
    // Two heats in the same round: heat-1 (current) and heat-2 (filled, on deck).
    const HEAT_2: HeatSummary = {
      heat: 'heat-2',
      name: 'Qualifying R1 Heat 2',
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
    it('shows the generic "arming… stand by" state in Armed when no tone_at is known', async () => {
      // No `tone_at` on the live state (e.g. an untimed/older log): fall back to the generic copy
      // with no precise countdown.
      const { session } = makeTestSession({ live: liveAt('Armed') });
      render(LiveRaceControl, { session });
      await tick();
      const arming = await screen.findByRole('status', { name: 'Arming' });
      expect(arming).toHaveTextContent(/Arming… stand by/);
      expect(screen.queryByTestId('arming-countdown')).toBeNull();
    });

    it('shows the RD-only "Tone in S.s" countdown while Armed when tone_at is known', async () => {
      // RD-console-only: the start delay is intentionally random to PILOTS, so a controlling (RD)
      // session sees a live countdown to the start tone. tone_at is server-time µs; with the system
      // clock at 0 a tone_at of 3.2s out reads "3.2s" and ticks down.
      vi.useFakeTimers();
      vi.setSystemTime(0);
      const { session } = makeTestSession({
        live: { ...liveAt('Armed'), tone_at: 3_200_000 } as LiveRaceState
      });
      render(LiveRaceControl, { session });
      await vi.advanceTimersByTimeAsync(0);
      await tick();

      const arming = await screen.findByRole('status', { name: 'Arming' });
      const countdown = () => screen.getByTestId('arming-countdown').textContent?.trim();
      expect(arming).toHaveTextContent(/Tone in/);
      await waitFor(() => expect(countdown()).toBe('3.2s'));

      // It counts down: ~2s later it reads ~1.2s.
      await vi.advanceTimersByTimeAsync(2_000);
      expect(countdown()).toBe('1.2s');

      // It clamps at zero (the runtime fires the tone + flips to Running and clears tone_at).
      await vi.advanceTimersByTimeAsync(2_000);
      expect(countdown()).toBe('0.0s');
      vi.useRealTimers();
    });

    it('hides the tone countdown from a read-only / pilot session (random to pilots)', async () => {
      // The whole point: pilots must NOT see the countdown. Even with tone_at present, a read-only
      // session falls back to the generic "stand by" — no precise countdown is fed or rendered.
      const { session } = makeTestSession({
        live: { ...liveAt('Armed'), tone_at: 3_200_000 } as LiveRaceState,
        role: 'readonly'
      });
      render(LiveRaceControl, { session });
      await tick();
      const arming = await screen.findByRole('status', { name: 'Arming' });
      expect(arming).toHaveTextContent(/Arming… stand by/);
      expect(screen.queryByTestId('arming-countdown')).toBeNull();
      expect(arming).not.toHaveTextContent(/Tone in/);
    });

    it('plays the start tone when the heat enters Running (from Armed)', async () => {
      const { started } = installAudioStub('running');

      const { session, pushLive } = makeTestSession({ live: liveAt('Armed') });
      const { container } = render(AudioHost, { session });
      render(LiveRaceControl, { session });
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
      render(AudioHost, { session });
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
      render(AudioHost, { session });
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
      render(AudioHost, { session });
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
      render(AudioHost, { session });
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
      render(AudioHost, { session });
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

    it('the old Tone toggle is GONE; the audio toolbar holds only the renamed Callouts toggle', async () => {
      installAudioStub('suspended');

      const { session } = makeTestSession({ live: liveAt('Staged') });
      render(LiveRaceControl, { session });
      await tick();
      // The test-tone affordance and the procedure-tone mute are gone; the one remaining audio
      // control is the informational-layer "Callouts" toggle.
      expect(screen.queryByRole('button', { name: /Enable sound/ })).not.toBeInTheDocument();
      expect(screen.queryByRole('button', { name: /Test tone/ })).not.toBeInTheDocument();
      expect(screen.queryByRole('button', { name: /Tone on|Tone off/ })).not.toBeInTheDocument();
      expect(screen.getByRole('button', { name: /Callouts on/ })).toBeInTheDocument();

      vi.unstubAllGlobals();
    });

    it('plays the start tone EVEN WHILE the callouts are muted (procedure tones are always-on)', async () => {
      const { started } = installAudioStub('running', { calloutsMuted: true });

      const { session, pushLive } = makeTestSession({ live: liveAt('Staged') });
      render(AudioHost, { session });
      render(LiveRaceControl, { session });
      await tick();
      // The toggle reads muted — and the race-go tone still fires.
      expect(screen.getByRole('button', { name: /Callouts off/ })).toBeInTheDocument();
      pushLive(liveAt('Running'));
      await tick();
      expect(started).toEqual([880]);

      vi.unstubAllGlobals();
    });
  });

  describe('end-of-race countdown + buzzer (Timed heats only)', () => {
    afterEach(() => {
      vi.useRealTimers();
      vi.unstubAllGlobals();
    });

    // A Running live state carrying the server race-go anchor (µs). The ROUND above is Timed with
    // a 120s window, so the fixed end is race_started_at + 120s.
    const runningAt = (startedAtMicros: number): LiveRaceState =>
      ({ ...liveAt('Running'), race_started_at: startedAtMicros }) as LiveRaceState;

    it('pips at remaining 5..1s and fires the LOWER race-end buzzer at 0 — once each', async () => {
      vi.useFakeTimers();
      vi.setSystemTime(0);
      const { started } = installAudioStub('running');
      const { session, pushLive } = makeTestSession({
        event: EVENT_WITH_ROUND,
        live: liveAt('Staged'),
        listHeatsImpl: vi.fn(async () => [HEAT_IN_ROUND])
      });
      render(AudioHost, { session });
      render(LiveRaceControl, { session });
      // Let the heats/round directory settle so the Timed window resolves.
      await vi.advanceTimersByTimeAsync(0);
      await tick();

      // Race-go at t=0 (server anchor 0µs): the start tone fires; the countdown is far off.
      pushLive(runningAt(0));
      await tick();
      expect(started).toEqual([880]);

      // …114s in (remaining 6s): still quiet.
      await vi.advanceTimersByTimeAsync(114_000);
      expect(started).toEqual([880]);

      // Remaining 5s → the first pip (start-tone pitch family).
      await vi.advanceTimersByTimeAsync(1_000);
      expect(started).toEqual([880, 880]);

      // 4,3,2,1 land each second on the way to remaining 1s.
      await vi.advanceTimersByTimeAsync(4_000);
      expect(started).toEqual([880, 880, 880, 880, 880, 880]);

      // Remaining 0 → the race-end buzzer: LOWER (440), fired once.
      await vi.advanceTimersByTimeAsync(1_000);
      expect(started).toEqual([880, 880, 880, 880, 880, 880, 440]);

      // The grace window keeps the heat Running past the buzzer — nothing replays.
      await vi.advanceTimersByTimeAsync(5_000);
      expect(started).toEqual([880, 880, 880, 880, 880, 880, 440]);
    });

    it('no countdown for a First-to-N heat (no known fixed end)', async () => {
      vi.useFakeTimers();
      vi.setSystemTime(0);
      const { started } = installAudioStub('running');
      const lapsRound: RoundDef = {
        ...ROUND,
        win_condition: { FirstToLaps: { n: 3 } }
      };
      const { session, pushLive } = makeTestSession({
        event: { ...EVENT_WITH_ROUND, rounds: [lapsRound] },
        live: liveAt('Staged'),
        listHeatsImpl: vi.fn(async () => [HEAT_IN_ROUND])
      });
      render(AudioHost, { session });
      render(LiveRaceControl, { session });
      await vi.advanceTimersByTimeAsync(0);
      await tick();

      pushLive(runningAt(0));
      await tick();
      // A long while later: only the start tone ever sounded — no pips, no buzzer.
      await vi.advanceTimersByTimeAsync(300_000);
      expect(started).toEqual([880]);
    });

    it('the countdown pips are NOT silenced by the callouts mute (procedure tones)', async () => {
      vi.useFakeTimers();
      vi.setSystemTime(0);
      const { started } = installAudioStub('running', { calloutsMuted: true });
      const { session, pushLive } = makeTestSession({
        event: EVENT_WITH_ROUND,
        live: liveAt('Staged'),
        listHeatsImpl: vi.fn(async () => [HEAT_IN_ROUND])
      });
      render(AudioHost, { session });
      render(LiveRaceControl, { session });
      await vi.advanceTimersByTimeAsync(0);
      await tick();

      pushLive(runningAt(0));
      await tick();
      await vi.advanceTimersByTimeAsync(120_000);
      // Muted callouts — yet start tone + all five pips + the buzzer all sounded.
      expect(started).toEqual([880, 880, 880, 880, 880, 880, 440]);
    });
  });

  describe('lap callouts (informational layer — crossing pip + spoken callout)', () => {
    afterEach(() => vi.unstubAllGlobals());

    // A roster-seeded lineup so the callsign resolves through the shared resolver: the competitor
    // ref IS the pilot id, looked up in the pilots directory.
    const CO_PILOTS = [{ id: 'maverick-4d9rp8', callsign: 'Maverick', vtx_types: [] }];
    /** One entry of the live crossing feed (#397) — the TONE's source, keyed on `pass_ref`. */
    const cross = (
      passRef: number,
      disposition: LiveCrossing['disposition'],
      lapNumber?: number,
      competitor = 'maverick-4d9rp8'
    ): LiveCrossing => ({
      pass_ref: passRef,
      competitor,
      at: passRef * 1_000_000,
      disposition,
      lap_number: lapNumber
    });
    const coLive = (
      laps: number,
      lastLapMicros?: number,
      phase = 'Running',
      crossings: LiveCrossing[] = []
    ): LiveRaceState =>
      ({
        current_heat: 'heat-1',
        phase,
        race_started_at: 1_000,
        active_pilots: ['maverick-4d9rp8'],
        progress: [
          {
            competitor: 'maverick-4d9rp8',
            laps_completed: laps,
            ...(lastLapMicros != null ? { last_lap_micros: lastLapMicros } : {})
          }
        ],
        running_order: ['maverick-4d9rp8'],
        crossings
      }) as LiveRaceState;

    function renderCallouts(opts?: { calloutsMuted?: boolean }) {
      const audioStub = installAudioStub('running', opts);
      const speech = installSpeechStub();
      const madeSession = makeTestSession({
        event: EVENT_WITH_ROUND,
        live: coLive(0),
        listHeatsImpl: vi.fn(async () => [{ ...HEAT_IN_ROUND, lineup: ['maverick-4d9rp8'] }]),
        listPilotsImpl: vi.fn(async () => CO_PILOTS as unknown as never)
      });
      render(AudioHost, { session: madeSession.session });
      render(LiveRaceControl, { session: madeSession.session });
      return { ...madeSession, ...audioStub, ...speech };
    }

    it('a lap pips per CROSSING (holeshot too) and speaks "<callsign>, lap N, M.SS" once', async () => {
      const { pushLive, started, utterances } = renderCallouts();
      // Let the pilots directory settle so the callsign resolves before the crossing.
      await waitFor(() => expect(screen.getAllByText('Maverick').length).toBeGreaterThan(0));

      // Lap 1 is TWO crossings: the holeshot that opened it and the pass that closed it. Both pip
      // (#397 — the holeshot used to be silent because it derives no lap); only the closing one
      // has a lap number and a time, so only it is spoken.
      pushLive(coLive(1, 21_470_000, 'Running', [cross(1, 'Holeshot'), cross(2, 'Counted', 1)]));
      await tick();
      // The crossing pip is the distinct high/short voice (1760), not a procedure tone.
      expect(started).toEqual([1760, 1760]);
      // The lap time is spoken to the hundredth — once, not once per crossing.
      expect(utterances.map((u) => u.text)).toEqual(['Maverick, lap 1, 21.47']);
    });

    it('a crossing REJECTED under the min-lap floor pips with NOTHING spoken (#397)', async () => {
      const { pushLive, started, utterances } = renderCallouts();
      await waitFor(() => expect(screen.getAllByText('Maverick').length).toBeGreaterThan(0));

      // A too-short pass records no lap, so `progress` never moves — the case that was pure
      // silence before, and the one that tells an RD their gate is double-triggering.
      pushLive(
        coLive(0, undefined, 'Running', [cross(1, 'Holeshot'), cross(2, 'RejectedTooShort')])
      );
      await tick();
      expect(started).toEqual([1760, 1760]);
      expect(utterances).toEqual([]);
    });

    it('a RE-PUSHED identical live state pips nothing — identity is pass_ref, not the frame', async () => {
      const { pushLive, started } = renderCallouts();
      await waitFor(() => expect(screen.getAllByText('Maverick').length).toBeGreaterThan(0));

      const feed = [cross(1, 'Holeshot'), cross(2, 'Counted', 1)];
      pushLive(coLive(1, 21_400_000, 'Running', feed));
      await tick();
      expect(started).toEqual([1760, 1760]);

      // The stream re-pushes the same state (a wake-up, a re-snapshot, a resubscribe).
      pushLive(coLive(1, 21_400_000, 'Running', feed));
      await tick();
      pushLive(coLive(1, 21_400_000, 'Running', feed));
      await tick();
      expect(started).toEqual([1760, 1760]);
    });

    it('the callouts mute silences BOTH the crossing pip and the speech', async () => {
      const { pushLive, started, utterances } = renderCallouts({ calloutsMuted: true });
      await waitFor(() => expect(screen.getAllByText('Maverick').length).toBeGreaterThan(0));

      pushLive(coLive(1, 21_400_000, 'Running', [cross(1, 'Holeshot'), cross(2, 'Counted', 1)]));
      await tick();
      expect(started).toEqual([]);
      expect(utterances).toEqual([]);
    });

    it('no ghost callouts from a non-Running fold (corrections on a finished heat)', async () => {
      const { pushLive, started, utterances } = renderCallouts();
      await waitFor(() => expect(screen.getAllByText('Maverick').length).toBeGreaterThan(0));

      // The heat finishes; a marshaling-style fold bumps the count (and appends a crossing) on
      // the finished heat. Neither the tone nor the voice may fire.
      pushLive(coLive(0, undefined, 'Unofficial'));
      await tick();
      pushLive(coLive(1, 20_000_000, 'Unofficial', [cross(1, 'Holeshot'), cross(2, 'Counted', 1)]));
      await tick();
      expect(started).toEqual([]);
      expect(utterances).toEqual([]);
    });

    it('lets the speech DRAIN at race end; cancels when the next run stages', async () => {
      const { pushLive, cancelSpy } = renderCallouts();
      await waitFor(() => expect(screen.getAllByText('Maverick').length).toBeGreaterThan(0));

      pushLive(coLive(1, 21_400_000, 'Running', [cross(1, 'Holeshot'), cross(2, 'Counted', 1)]));
      await tick();
      // A natural finish (Running → Unofficial) must NOT cancel — the final laps' times are what
      // everyone is waiting to hear (cancelling here chopped the last callout mid-word).
      pushLive(coLive(0, undefined, 'Unofficial'));
      await tick();
      expect(cancelSpy).not.toHaveBeenCalled();

      // A new run taking the stage (a pre-run phase) drops the stale backlog.
      pushLive(coLive(0, undefined, 'Staged'));
      await tick();
      expect(cancelSpy).toHaveBeenCalled();
    });

    it('muting mid-race cancels the queued speech immediately', async () => {
      const { pushLive, cancelSpy, utterances } = renderCallouts();
      await waitFor(() => expect(screen.getAllByText('Maverick').length).toBeGreaterThan(0));

      pushLive(coLive(1, 21_400_000, 'Running', [cross(1, 'Holeshot'), cross(2, 'Counted', 1)]));
      await tick();
      expect(utterances).toHaveLength(1);

      await fireEvent.click(screen.getByRole('button', { name: /Callouts on/ }));
      expect(cancelSpy).toHaveBeenCalled();
      expect(screen.getByRole('button', { name: /Callouts off/ })).toBeInTheDocument();

      // Further crossings while muted stay silent.
      pushLive(
        coLive(2, 20_000_000, 'Running', [
          cross(1, 'Holeshot'),
          cross(2, 'Counted', 1),
          cross(3, 'Counted', 2)
        ])
      );
      await tick();
      expect(utterances).toHaveLength(1);
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

    it('counts DOWN for a Timed round, running negative (danger) through the grace window', async () => {
      vi.useFakeTimers();
      vi.setSystemTime(0);
      // The heat's round is Timed with a 120s window → the HUD clock counts down from 2:00.
      const { session, pushLive } = makeTestSession({
        event: EVENT_WITH_ROUND,
        live: liveAt('Running', { startedAtMs: 0 }),
        listHeatsImpl: vi.fn(async () => [{ ...HEAT_IN_ROUND, phase: 'Running' as const }])
      });
      const { container } = render(LiveRaceControl, { session });
      // Let the heats/round directory settle so the Timed window resolves.
      await vi.advanceTimersByTimeAsync(0);
      await tick();
      // Two clocks in countdown mode: the big remaining readout + the small companion elapsed.
      const remainingText = () =>
        screen.getByRole('timer', { name: /^Time remaining/ }).textContent?.trim();
      const elapsedText = () => screen.getByRole('timer', { name: /^Elapsed/ }).textContent?.trim();

      // 1.25s in: remaining = 120s − 1.25s counting DOWN, while the companion counts UP from 0
      // (lap times are elapsed-from-zero quantities — the RD reads them off this one).
      await vi.advanceTimersByTimeAsync(1_250);
      expect(remainingText()).toBe('1:58.750');
      expect(elapsedText()).toBe('0:01.250');

      // Past the window end (inside the grace period): NEGATIVE, danger-styled — late crossings
      // still score, but the readout shows the heat running down its grace.
      await vi.advanceTimersByTimeAsync(120_000); // t = 121.25s → remaining −1.25s
      expect(remainingText()).toBe('-0:01.250');
      expect(elapsedText()).toBe('2:01.250');
      expect(container.querySelector('[data-urgency="over"]')).not.toBeNull();

      // The race-end freeze keeps both frames: window − exact duration, and the exact duration.
      pushLive(liveAt('Unofficial', { startedAtMs: 0, endedAtMs: 123_000 }));
      await tick();
      expect(remainingText()).toBe('-0:03.000');
      expect(elapsedText()).toBe('2:03.000');
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
  available_channels: [5658, 5800],
  manual_connect: false,
  calibration: [],
  disabled_nodes: []
};
const OP_CATALOG: ChannelCatalogEntry[] = [
  { band: 'Raceband', channel: 'R1', mhz: 5658 },
  { band: 'Fatshark', channel: 'F4', mhz: 5800 }
];
// #117 S3 / #402: the event's channel layout — the `node → channel` mapping a practice seat's
// channel resolves through. Before layouts existed there was no such mapping anywhere: a practice
// heat's `frequencies` are empty by construction, and the console had nothing but the live signal
// to fall back on (which only two screens carry). That was #402.
const OP_LAYOUT: ChannelLayout = {
  id: 'practice-a',
  name: 'Practice A',
  nodes: [
    { node: 0, channel: 5658 },
    { node: 1, channel: 5800 }
  ]
};
const OP_ROUND: RoundDef = {
  id: 'rp',
  label: 'Open Practice',
  classes: [],
  format: 'open_practice',
  params: {},
  win_condition: 'BestLap',
  seeding: { ActiveNodes: { nodes: [0, 1] } },
  layouts: ['practice-a'],
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
  rounds: [OP_ROUND],
  channel_layouts: [OP_LAYOUT]
};
// Deliberately still carrying EMPTY `frequencies`, and still naming every seat's channel: the
// board resolves them through the heat's **layout**, which is the source that replaced
// `available_channels[node]` (#117 S3). Isolating it this way is the #402 regression test — a
// practice seat's channel now has a real source in the event's own config, not just in whatever
// the hardware happens to report to the two screens that hold a signal subscription.
const OP_HEAT: HeatSummary = {
  heat: 'practice-1',
  name: 'Practice Heat',
  lineup: ['node-0', 'node-1'],
  round: 'rp',
  class: undefined,
  frequencies: [],
  layout: 'practice-a',
  phase: 'Running',
  is_current: true
};
// A live open-practice state: two channels, node-0 with 3 laps (last 28.0s, best 26.0s), node-1
// quiet. Last and best are DIFFERENT values on purpose — the board must render the served
// `best_lap_micros` rather than the last lap it happens to be holding (#425).
const opLive: LiveRaceState = {
  current_heat: 'practice-1',
  phase: 'Running',
  active_pilots: ['node-0', 'node-1'],
  progress: [
    {
      competitor: 'node-0',
      laps_completed: 3,
      last_lap_micros: 28_000_000,
      best_lap_micros: 26_000_000
    },
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
    const r1 = await screen.findByLabelText('Channel Node 1 · Raceband R1');
    expect(r1).toBeInTheDocument();
    expect(screen.getByLabelText('Channel Node 2 · Fatshark F4')).toBeInTheDocument();

    // node-0 shows 3 laps, its last lap (28.0s) and the SERVED best (26.0s) — two distinct values.
    expect(within(r1).getByText('3')).toBeInTheDocument();
    expect(within(r1).getByText('28.000')).toBeInTheDocument();
    expect(within(r1).getByText('26.000')).toBeInTheDocument();
  });

  it('renders the served best lap and accumulates nothing of its own (#425)', async () => {
    const { session, pushLive } = renderBoard();
    render(LiveRaceControl, { session });
    const r1 = await screen.findByLabelText('Channel Node 1 · Raceband R1');
    await within(r1).findByText('26.000');

    // A faster lap: the server's fold has already taken the `min`, so the board just renders what
    // it is handed.
    pushLive({
      ...opLive,
      progress: [
        {
          competitor: 'node-0',
          laps_completed: 4,
          last_lap_micros: 25_000_000,
          best_lap_micros: 25_000_000
        },
        { competitor: 'node-1', laps_completed: 0 }
      ]
    });
    await waitFor(() => expect(within(r1).getAllByText('25.000')).toHaveLength(2));

    // A slower lap: last becomes 30.0s and best stays 25.0s — because the SERVER says so, not
    // because the screen remembered the earlier frame.
    pushLive({
      ...opLive,
      progress: [
        {
          competitor: 'node-0',
          laps_completed: 5,
          last_lap_micros: 30_000_000,
          best_lap_micros: 25_000_000
        },
        { competitor: 'node-1', laps_completed: 0 }
      ]
    });
    await within(r1).findByText('30.000');
    expect(within(r1).getByText('25.000')).toBeInTheDocument();
  });

  it('takes a re-snapshot at its word rather than holding a stale minimum (#425)', async () => {
    // The bug this replaced: the screen kept a running `min` over the `last_lap_micros` of the
    // frames it observed, so a value it had seen could outlive the truth. A re-snapshot — a
    // reconnect, a scope change, a heat re-windowed by "Run again" — is authoritative. If the
    // server now says the best is 31.0s, the board must say 31.0s, not the 26.0s it used to hold.
    const { session, pushLive } = renderBoard();
    render(LiveRaceControl, { session });
    const r1 = await screen.findByLabelText('Channel Node 1 · Raceband R1');
    await within(r1).findByText('26.000');

    pushLive({
      ...opLive,
      progress: [
        {
          competitor: 'node-0',
          laps_completed: 1,
          last_lap_micros: 31_000_000,
          best_lap_micros: 31_000_000
        },
        { competitor: 'node-1', laps_completed: 0 }
      ]
    });
    await waitFor(() => expect(within(r1).getAllByText('31.000')).toHaveLength(2));
    expect(within(r1).queryByText('26.000')).toBeNull();
  });

  it('carries no second "new run" control — Run again is the one way to go again (#393)', () => {
    // The board used to offer a fill-based "New run · clear board". An open-practice round has
    // exactly ONE heat ever (`OpenPractice::next` completes after it), so once the run had ended
    // that fill scheduled nothing and still acked ok — a button that claimed success and cleared
    // nothing. Re-running is the transition row's Run again.
    const { session } = renderBoard();
    render(LiveRaceControl, { session });
    expect(screen.queryByRole('button', { name: /New run/ })).toBeNull();
  });

  it('does not show the pilot-keyed panels for an open-practice heat', async () => {
    const { session } = renderBoard();
    render(LiveRaceControl, { session });
    await screen.findByLabelText('Channel Node 1 · Raceband R1');
    // The normal Heat sheet / Live standing panels are replaced by the practice board.
    expect(screen.queryByText('Heat sheet')).not.toBeInTheDocument();
    expect(screen.queryByText('Live standing')).not.toBeInTheDocument();
  });
});

// ── Practice ends with "Run again", not competition ceremony (#393) ────────────────────────────

describe('LiveRaceControl — practice runs again instead of being adjudicated (#393)', () => {
  const opLiveAt = (phase: LiveRaceState['phase']) => ({ ...opLive, phase }) as LiveRaceState;

  function renderPractice(phase: LiveRaceState['phase']) {
    return makeTestSession({
      event: OP_EVENT,
      live: opLiveAt(phase),
      listHeatsImpl: vi.fn(async () => [{ ...OP_HEAT, phase }]),
      listChannelsImpl: vi.fn(async () => OP_CATALOG),
      listTimersImpl: vi.fn(async () => [OP_TIMER])
    });
  }

  it('offers Run again at the end of a practice run and NONE of the ceremony verbs', async () => {
    const { session } = renderPractice('Unofficial');
    render(LiveRaceControl, { session });

    // The one obvious action, enabled. (`Restart` under a name that describes practice.)
    const again = (await screen.findByRole('button', { name: 'Run again' })) as HTMLButtonElement;
    expect(again.disabled).toBe(false);
    // Practice has no result to make official — the verbs are ABSENT, not merely disabled.
    for (const ceremony of ['Finalize', 'Advance', 'Revert']) {
      expect(screen.queryByRole('button', { name: ceremony })).toBeNull();
    }
    // `Restart` is never spelled that way for practice.
    expect(screen.queryByRole('button', { name: 'Restart' })).toBeNull();
    // Discard stays — abandoning the session is still a real thing to want.
    expect((screen.getByRole('button', { name: 'Discard' }) as HTMLButtonElement).disabled).toBe(
      false
    );
  });

  it('Run again fires the Restart command (same transition, practice name)', async () => {
    const { session, sendSpy } = renderPractice('Unofficial');
    render(LiveRaceControl, { session });

    // Restart is destructive — it throws the run away — so it still confirms once.
    await fireEvent.click(await screen.findByRole('button', { name: 'Run again' }));
    expect(sendSpy).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
    expect(sendSpy).toHaveBeenCalledWith({ Restart: { heat: 'practice-1' } });
  });

  it('replaces the provisional/official lifecycle copy with the practice run one', async () => {
    const { session } = renderPractice('Unofficial');
    render(LiveRaceControl, { session });

    expect(await screen.findByText('Run complete')).toBeInTheDocument();
    // No adjudication language: nothing is provisional and nothing becomes official.
    expect(screen.queryByText('Provisional')).toBeNull();
    expect(screen.queryByText('Official')).toBeNull();
  });

  it('never strands a practice heat at Final: Run again re-opens it, then resets', async () => {
    // Reachable when the round carries an armed protest window (the runtime auto-finalizes).
    const { session, sendSpy } = renderPractice('Final');
    render(LiveRaceControl, { session });

    await fireEvent.click(await screen.findByRole('button', { name: 'Run again' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
    await waitFor(() => expect(sendSpy).toHaveBeenCalledTimes(2));
    expect(sendSpy.mock.calls.map((c) => c[0])).toEqual([
      { Revert: { heat: 'practice-1' } },
      { Restart: { heat: 'practice-1' } }
    ]);
  });

  it("leaves a NON-practice heat's actions exactly as they were", () => {
    const { session } = makeTestSession({ live: liveAt('Unofficial') });
    render(LiveRaceControl, { session });
    const btn = (label: string) => screen.getByRole('button', { name: label }) as HTMLButtonElement;
    // The competition lifecycle is untouched: Finalize primary, Restart still called Restart.
    expect(btn('Finalize').disabled).toBe(false);
    expect(btn('Restart').disabled).toBe(false);
    expect(btn('Discard').disabled).toBe(false);
    expect(btn('Advance').disabled).toBe(true);
    expect(btn('Revert').disabled).toBe(true);
    expect(screen.queryByRole('button', { name: 'Run again' })).toBeNull();
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
    name: 'Qualifying R1 Heat 1',
    lineup: ['node-0', 'node-1'],
    round: 'r1',
    class: 'c1',
    frequencies: [],
    phase: 'Running',
    is_current: true
  };
  const HEAT_2: HeatSummary = {
    heat: 'p2-deadbeef-heat',
    name: 'Qualifying R1 Heat 2',
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
    available_channels: [5658, 5800],
    manual_connect: false,
    calibration: [],
    disabled_nodes: []
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

  // #340: a FAILED pilots/heats read used to swallow into empty arrays, so the raw refs rendered
  // with no error state. It must surface a visible "Couldn't load — retry" state instead.
  it('surfaces a visible retry state when the pilot/heat directory reads fail (#340)', async () => {
    let fail = true;
    const { session } = makeTestSession({
      event: FN_EVENT,
      live: fnLive,
      listHeatsImpl: vi.fn(async () => {
        if (fail) throw new Error('boom');
        return [HEAT_1_FREQ, HEAT_2];
      }),
      listPilotsImpl: vi.fn(async () => {
        if (fail) throw new Error('boom');
        return PILOTS as unknown as never;
      }),
      listChannelsImpl: vi.fn(async () => FN_CATALOG),
      listTimersImpl: vi.fn(async () => [FN_TIMER])
    });
    render(LiveRaceControl, { session });

    // The failure is visible — no more silently-empty directory + raw refs.
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(/Couldn.t load the pilot\/heat directory/);

    // Retry with the reads healthy again: the error clears and the names resolve.
    fail = false;
    await fireEvent.click(within(alert).getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
    const title = document.querySelector('.heat-id .value') as HTMLElement;
    await waitFor(() => expect(title.textContent?.trim()).toBe('Qualifying R1 Heat 1'));
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
    name: 'Qualifying R1 Heat 1',
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
