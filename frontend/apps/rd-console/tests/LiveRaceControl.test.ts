import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/dom';
import { tick } from 'svelte';
import type { LiveRaceState } from '@gridfpv/types';
import LiveRaceControl from '../src/screens/LiveRaceControl.svelte';
import { makeTestSession } from './support.js';
import { liveRunning, failAck } from './fixtures.js';

describe('LiveRaceControl', () => {
  it('enables only the phase-legal transitions (Running → Finish/Abort/Restart)', () => {
    const { session } = makeTestSession({ live: liveRunning });
    render(LiveRaceControl, { session });

    const btn = (label: string) => screen.getByRole('button', { name: label }) as HTMLButtonElement;
    // Forward step + off-ramps legal in Running.
    expect(btn('Finish').disabled).toBe(false);
    expect(btn('Abort').disabled).toBe(false);
    expect(btn('Restart').disabled).toBe(false);
    // Illegal in Running.
    expect(btn('Stage').disabled).toBe(true);
    expect(btn('Arm').disabled).toBe(true);
    expect(btn('Finalize').disabled).toBe(true);
    expect(btn('Advance').disabled).toBe(true);
    expect(btn('Revert').disabled).toBe(true);
    expect(btn('Discard').disabled).toBe(true);
  });

  it('fires the matching Command for a forward transition', async () => {
    const { session, sendSpy } = makeTestSession({ live: liveRunning });
    render(LiveRaceControl, { session });

    await fireEvent.click(screen.getByRole('button', { name: 'Finish' }));
    expect(sendSpy).toHaveBeenCalledWith({ Finish: { heat: 'heat-1' } });
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

    await fireEvent.click(screen.getByRole('button', { name: 'Finish' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('illegal transition');
  });

  it('renders the live leaderboard from the running order', () => {
    const { session } = makeTestSession({ live: liveRunning });
    render(LiveRaceControl, { session });
    // Heat sheet + live standing both list the lineup.
    expect(screen.getAllByText('ALICE').length).toBeGreaterThan(0);
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
