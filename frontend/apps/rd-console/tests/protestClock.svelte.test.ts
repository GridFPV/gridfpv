import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync } from 'svelte';
import type { LifecycleState } from '@gridfpv/types';
import { useProtestClock, formatProtest } from '../src/lib/protestClock.svelte.js';

/**
 * Unit tests for the auto-official protest countdown.
 *
 * The reactive `useProtestClock` rune helper takes an injected `nowMs` clock — the auto-official
 * deadline is a SERVER instant, so the console feeds it `() => session.serverNowMs()` (the
 * offset-corrected clock) rather than raw `Date.now()` (the clock-skew rule). These tests pin that
 * the countdown is measured against the INJECTED clock, not the device wall clock, plus the pure
 * `formatProtest` mapping.
 *
 * The rune helper owns an internal `$effect`, driven inside an `$effect.root` with reactive `$state`
 * for the lifecycle, and `flushSync()` to settle after each change. Fake timers drive the helper's
 * `setInterval`; the injected `nowMs` is an explicit, skewed clock so the test can prove it is used.
 */
describe('useProtestClock (injected server clock)', () => {
  const T0 = 1_000_000_000_000; // ms — the device wall clock anchor

  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(T0);
  });
  afterEach(() => vi.useRealTimers());

  /** A `Provisional` lifecycle whose auto-official deadline is `atMs` (stored as µs on the wire). */
  function provisional(atMs: number): LifecycleState {
    return { Provisional: { auto_official_at: atMs * 1000 } } as LifecycleState;
  }

  /** Run the helper inside an owned effect root with a reactive lifecycle + injected clock. */
  function harness(initial: LifecycleState | undefined, nowMs: () => number) {
    let lifecycle = $state<LifecycleState | undefined>(initial);
    let clock!: ReturnType<typeof useProtestClock>;
    const cleanup = $effect.root(() => {
      clock = useProtestClock(() => lifecycle, nowMs);
    });
    flushSync();
    return {
      get active() {
        return clock.active;
      },
      get remainingMs() {
        return clock.remainingMs;
      },
      set(next: LifecycleState | undefined) {
        lifecycle = next;
        flushSync();
      },
      cleanup
    };
  }

  it('measures the remainder against the INJECTED clock, not Date.now()', () => {
    // The injected (server) clock is 5s AHEAD of the device wall clock. The deadline is 10s ahead
    // of the SERVER clock, so the remainder must read ~10s — using Date.now() it would read ~15s.
    const serverNow = () => Date.now() + 5_000;
    const deadlineMs = serverNow() + 10_000;
    const h = harness(provisional(deadlineMs), serverNow);
    expect(h.active).toBe(true);
    expect(h.remainingMs).toBe(10_000);
    h.cleanup();
  });

  it('ticks down against the injected clock', () => {
    const serverNow = () => Date.now() + 5_000;
    const deadlineMs = serverNow() + 10_000;
    const h = harness(provisional(deadlineMs), serverNow);
    vi.advanceTimersByTime(4_000);
    expect(h.remainingMs).toBe(6_000);
    h.cleanup();
  });

  it('clamps at zero once the deadline passes (never negative)', () => {
    const serverNow = () => Date.now() + 5_000;
    const deadlineMs = serverNow() + 2_000;
    const h = harness(provisional(deadlineMs), serverNow);
    vi.advanceTimersByTime(10_000);
    expect(h.remainingMs).toBe(0);
    h.cleanup();
  });

  it('is inactive with no armed window (Official / absent lifecycle)', () => {
    const serverNow = () => Date.now();
    const h = harness('Official' as LifecycleState, serverNow);
    expect(h.active).toBe(false);
    expect(h.remainingMs).toBe(0);
    h.set(undefined);
    expect(h.active).toBe(false);
    h.cleanup();
  });

  it('defaults to Date.now() when no clock is injected', () => {
    const lifecycle = $state<LifecycleState | undefined>(provisional(Date.now() + 8_000));
    let clock!: ReturnType<typeof useProtestClock>;
    const cleanup = $effect.root(() => {
      clock = useProtestClock(() => lifecycle);
    });
    flushSync();
    expect(clock.remainingMs).toBe(8_000);
    cleanup();
  });
});

describe('formatProtest', () => {
  it('formats a remainder as M:SS', () => {
    expect(formatProtest(300_000)).toBe('5:00');
    expect(formatProtest(7_000)).toBe('0:07');
    expect(formatProtest(0)).toBe('0:00');
  });

  it('clamps a negative remainder at zero', () => {
    expect(formatProtest(-1_000)).toBe('0:00');
  });
});
