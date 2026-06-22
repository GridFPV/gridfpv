import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync } from 'svelte';
import { useRaceClock } from '../src/lib/raceClock.svelte.js';

/**
 * Unit tests for the shared phase-driven race clock (#62, #85). Pins the freeze regression
 * the Finished→Unofficial rename introduced: the clock must FREEZE on `Unofficial` (the race
 * has ended; only the result isn't finalized yet) and stay frozen through `Final`, ticking only
 * while `Running` and resetting to 0 on a fresh/idle heat or a heat change.
 *
 * The rune helper owns an internal `$effect`, so it's driven inside an `$effect.root` with a
 * reactive `$state` phase and `flushSync()` to settle the effect after each phase change. Fake
 * timers advance the wall clock the helper reads.
 */
describe('useRaceClock', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  /** Run `fn` with a reactive phase setter inside an owned effect root; returns a cleanup. */
  function harness() {
    let phase = $state<string | undefined>('Scheduled');
    let clock!: ReturnType<typeof useRaceClock>;
    const cleanup = $effect.root(() => {
      clock = useRaceClock(() => phase);
    });
    flushSync();
    return {
      get elapsedMs() {
        return clock.elapsedMs;
      },
      setPhase(p: string | undefined) {
        phase = p;
        flushSync();
      },
      cleanup
    };
  }

  it('ticks up while Running', () => {
    const h = harness();
    h.setPhase('Running');
    expect(h.elapsedMs).toBe(0);
    vi.advanceTimersByTime(3000);
    expect(h.elapsedMs).toBeGreaterThanOrEqual(2900);
    h.cleanup();
  });

  it('freezes at the race-end value on Unofficial and does not keep counting', () => {
    const h = harness();
    h.setPhase('Running');
    vi.advanceTimersByTime(5000);
    const atEnd = h.elapsedMs;
    expect(atEnd).toBeGreaterThanOrEqual(4900);

    // The time limit fires → Running transitions to Unofficial. The clock must STOP.
    h.setPhase('Unofficial');
    const frozen = h.elapsedMs;
    expect(frozen).toBe(atEnd);

    // Wall clock keeps moving, but the frozen reading must not advance (the bug: it free-ran).
    vi.advanceTimersByTime(10_000);
    expect(h.elapsedMs).toBe(frozen);
    h.cleanup();
  });

  it('stays frozen through Final (finalize does not restart the clock)', () => {
    const h = harness();
    h.setPhase('Running');
    vi.advanceTimersByTime(4000);
    h.setPhase('Unofficial');
    const frozen = h.elapsedMs;

    h.setPhase('Final');
    vi.advanceTimersByTime(5000);
    expect(h.elapsedMs).toBe(frozen);
    h.cleanup();
  });

  it('resets to 0 on a new/idle heat (Scheduled) after a frozen race', () => {
    const h = harness();
    h.setPhase('Running');
    vi.advanceTimersByTime(4000);
    h.setPhase('Unofficial');
    expect(h.elapsedMs).toBeGreaterThan(0);

    // A fresh heat is Scheduled again → reset.
    h.setPhase('Scheduled');
    expect(h.elapsedMs).toBe(0);
    h.cleanup();
  });

  it('resets to 0 when the heat goes idle (no heat / undefined phase)', () => {
    const h = harness();
    h.setPhase('Running');
    vi.advanceTimersByTime(2000);
    h.setPhase(undefined);
    expect(h.elapsedMs).toBe(0);
    h.cleanup();
  });
});
