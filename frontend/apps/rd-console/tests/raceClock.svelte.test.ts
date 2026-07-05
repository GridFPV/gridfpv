import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync } from 'svelte';
import { useRaceClock } from '../src/lib/raceClock.svelte.js';

/**
 * Unit tests for the shared, **server-time-authoritative** race clock (#62, #85, #62 follow-up).
 *
 * The clock no longer counts from a per-instance wall-clock start; it counts from the server's
 * `race_started_at` / `race_ended_at` (µs) carried on `LiveRaceState`. These tests pin:
 *   • the same `race_started_at` yields the same elapsed regardless of when the clock mounts
 *     (a mount *after* race-go still reads the real elapsed, not 0) — the header↔HUD agreement;
 *   • the frozen value equals exactly `race_ended_at - race_started_at` (no poll-late overshoot);
 *   • Running ticks, Unofficial/Final freeze, Scheduled/idle reset.
 *
 * The rune helper owns an internal `$effect`, driven inside an `$effect.root` with reactive
 * `$state` for phase + the two server instants, and `flushSync()` to settle after each change.
 * Fake timers drive both the helper's `setInterval` and `Date.now()`.
 */
describe('useRaceClock (server-time-authoritative)', () => {
  // Anchor the fake clock at a fixed wall time so `Date.now()` and the µs anchors are comparable.
  const T0 = 1_000_000_000_000; // ms

  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(T0);
  });
  afterEach(() => vi.useRealTimers());

  /** Run inside an owned effect root with reactive phase + server instants; returns a handle. */
  function harness(initial?: {
    phase?: string;
    startedAtMicros?: number | null;
    endedAtMicros?: number | null;
  }) {
    let phase = $state<string | undefined>(initial?.phase ?? 'Scheduled');
    let startedAt = $state<number | null | undefined>(initial?.startedAtMicros ?? null);
    let endedAt = $state<number | null | undefined>(initial?.endedAtMicros ?? null);
    let clock!: ReturnType<typeof useRaceClock>;
    const cleanup = $effect.root(() => {
      clock = useRaceClock(
        () => phase,
        () => startedAt,
        () => endedAt
      );
    });
    flushSync();
    return {
      get elapsedMs() {
        return clock.elapsedMs;
      },
      set(next: {
        phase?: string;
        startedAtMicros?: number | null;
        endedAtMicros?: number | null;
      }) {
        if ('phase' in next) phase = next.phase;
        if ('startedAtMicros' in next) startedAt = next.startedAtMicros;
        if ('endedAtMicros' in next) endedAt = next.endedAtMicros;
        flushSync();
      },
      cleanup
    };
  }

  it('ticks up while Running, anchored to the server race-start', () => {
    const h = harness({ phase: 'Running', startedAtMicros: T0 * 1000 });
    expect(h.elapsedMs).toBe(0);
    vi.advanceTimersByTime(3000);
    expect(h.elapsedMs).toBeGreaterThanOrEqual(2900);
    expect(h.elapsedMs).toBeLessThanOrEqual(3100);
    h.cleanup();
  });

  it('mounting AFTER race-go reads the real elapsed, not 0 (header == HUD)', () => {
    // The race started 7s before this clock mounts (the bug: a late-joining live page used to
    // count from arrival and disagree with the always-mounted header). Both now anchor to the
    // same server `race_started_at`, so a fresh mount immediately reads ~7s.
    const startedAtMicros = (T0 - 7000) * 1000; // 7s ago
    const h = harness({ phase: 'Running', startedAtMicros });
    expect(h.elapsedMs).toBeGreaterThanOrEqual(6900);
    expect(h.elapsedMs).toBeLessThanOrEqual(7100);

    // A second clock mounting at the same instant with the same anchor reads the SAME value.
    const h2 = harness({ phase: 'Running', startedAtMicros });
    expect(h2.elapsedMs).toBe(h.elapsedMs);
    h.cleanup();
    h2.cleanup();
  });

  it('freezes at EXACTLY race_ended_at - race_started_at on Unofficial', () => {
    const startedAtMicros = T0 * 1000;
    // A clean 60.000s race window from the server.
    const endedAtMicros = (T0 + 60_000) * 1000;
    const h = harness({ phase: 'Running', startedAtMicros });
    vi.advanceTimersByTime(60_100); // poll a tenth late, as the real bug did

    h.set({ phase: 'Unofficial', endedAtMicros });
    // Exactly 60_000 ms — not 60_100 (no overshoot), not a short late-join value.
    expect(h.elapsedMs).toBe(60_000);

    // The frozen value must not advance as the wall clock keeps moving.
    vi.advanceTimersByTime(10_000);
    expect(h.elapsedMs).toBe(60_000);
    h.cleanup();
  });

  it('stays frozen at the exact duration through Final', () => {
    const startedAtMicros = T0 * 1000;
    const endedAtMicros = (T0 + 42_000) * 1000;
    const h = harness({ phase: 'Running', startedAtMicros });
    vi.advanceTimersByTime(42_000);
    h.set({ phase: 'Unofficial', endedAtMicros });
    expect(h.elapsedMs).toBe(42_000);

    h.set({ phase: 'Final' });
    vi.advanceTimersByTime(5000);
    expect(h.elapsedMs).toBe(42_000);
    h.cleanup();
  });

  it('resets to 0 on a new/idle heat (Scheduled) after a frozen race', () => {
    const startedAtMicros = T0 * 1000;
    const endedAtMicros = (T0 + 30_000) * 1000;
    const h = harness({ phase: 'Running', startedAtMicros });
    vi.advanceTimersByTime(30_000);
    h.set({ phase: 'Unofficial', endedAtMicros });
    expect(h.elapsedMs).toBe(30_000);

    // A fresh heat: phase Scheduled, server timing cleared → reset.
    h.set({ phase: 'Scheduled', startedAtMicros: null, endedAtMicros: null });
    expect(h.elapsedMs).toBe(0);
    h.cleanup();
  });

  it('resets to 0 when the heat goes idle (no heat / undefined phase)', () => {
    const h = harness({ phase: 'Running', startedAtMicros: T0 * 1000 });
    vi.advanceTimersByTime(2000);
    h.set({ phase: undefined, startedAtMicros: null });
    expect(h.elapsedMs).toBe(0);
    h.cleanup();
  });

  it('holds at 0 while Running before the server race-start has propagated', () => {
    // A brief window: phase is Running but `race_started_at` has not arrived yet. Holding at 0
    // is correct — counting from an unknown anchor would be wrong.
    const h = harness({ phase: 'Running', startedAtMicros: null });
    expect(h.elapsedMs).toBe(0);
    vi.advanceTimersByTime(1000);
    expect(h.elapsedMs).toBe(0);
    // Once the anchor lands, it counts.
    h.set({ startedAtMicros: T0 * 1000 });
    vi.advanceTimersByTime(2000);
    expect(h.elapsedMs).toBeGreaterThanOrEqual(1900);
    h.cleanup();
  });
});
