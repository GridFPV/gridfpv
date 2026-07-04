import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync } from 'svelte';
import { useEndOfRaceTones } from '../src/lib/endTones.svelte.js';

/**
 * Unit tests for the end-of-race tone scheduler: countdown pips at remaining 5…1s and the
 * race-end buzzer at 0, for heats with a KNOWN fixed end only. Driven inside an `$effect.root`
 * (the raceClock test pattern) with reactive `$state` for phase/heat/anchor/window; fake timers
 * drive both the helper's `setInterval` and the anchored clock. Pins:
 *   • the full 5,4,3,2,1 + end sequence, each mark once per run;
 *   • nothing for a heat with no fixed end (First-to-N: window `undefined`);
 *   • stream re-renders with the same run don't replay;
 *   • a re-mount mid-race pre-marks the already-past pips silently (no replay burst);
 *   • a Restart (new `race_started_at`) counts down afresh;
 *   • leaving `Running` stops the schedule.
 */
describe('useEndOfRaceTones', () => {
  const T0 = 1_000_000_000_000; // ms — the fake wall-clock anchor

  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(T0);
  });
  afterEach(() => vi.useRealTimers());

  /** Run inside an owned effect root with reactive inputs; returns a handle + the fired log. */
  function harness(initial?: {
    phase?: string;
    heat?: string | undefined;
    startedAtMicros?: number | null;
    windowMicros?: number;
  }) {
    let phase = $state<string | undefined>(initial?.phase ?? 'Scheduled');
    let heat = $state<string | undefined>('heat' in (initial ?? {}) ? initial?.heat : 'heat-1');
    let startedAt = $state<number | null | undefined>(initial?.startedAtMicros ?? null);
    let windowMicros = $state<number | undefined>(initial?.windowMicros);
    // A dummy rev the getters touch, so a test can force an effect re-run with UNCHANGED values —
    // simulating the live stream re-pushing the same state (new object, same content).
    let rev = $state(0);
    const fired: Array<number | 'end'> = [];
    const cleanup = $effect.root(() => {
      useEndOfRaceTones(
        () => {
          void rev;
          return phase;
        },
        () => heat,
        () => startedAt,
        () => windowMicros,
        () => Date.now(), // the tests' stand-in for session.serverNowMs()
        {
          onCountdown: (n) => fired.push(n),
          onRaceEnd: () => fired.push('end')
        }
      );
    });
    flushSync();
    return {
      fired,
      set(next: {
        phase?: string;
        heat?: string | undefined;
        startedAtMicros?: number | null;
        windowMicros?: number;
      }) {
        if ('phase' in next) phase = next.phase;
        if ('heat' in next) heat = next.heat;
        if ('startedAtMicros' in next) startedAt = next.startedAtMicros;
        if ('windowMicros' in next) windowMicros = next.windowMicros;
        flushSync();
      },
      /** Re-run the effect with unchanged values (a same-content live-stream push). */
      poke() {
        rev += 1;
        flushSync();
      },
      cleanup
    };
  }

  it('fires 5,4,3,2,1 then the race-end buzzer, each exactly once, for a Timed run', () => {
    // A 30s window starting at T0.
    const h = harness({ phase: 'Running', startedAtMicros: T0 * 1000, windowMicros: 30_000_000 });
    vi.advanceTimersByTime(24_000); // remaining 6s — quiet so far
    expect(h.fired).toEqual([]);

    vi.advanceTimersByTime(1_000); // remaining 5s
    expect(h.fired).toEqual([5]);
    vi.advanceTimersByTime(4_000); // remaining 1s — 4,3,2,1 landed on the way
    expect(h.fired).toEqual([5, 4, 3, 2, 1]);
    vi.advanceTimersByTime(1_000); // remaining 0 — the buzzer
    expect(h.fired).toEqual([5, 4, 3, 2, 1, 'end']);

    // Past the end (the grace window keeps the heat Running): nothing re-fires.
    vi.advanceTimersByTime(5_000);
    expect(h.fired).toEqual([5, 4, 3, 2, 1, 'end']);
    h.cleanup();
  });

  it('fires NOTHING for a heat with no fixed end (First-to-N: window undefined)', () => {
    const h = harness({ phase: 'Running', startedAtMicros: T0 * 1000, windowMicros: undefined });
    vi.advanceTimersByTime(300_000);
    expect(h.fired).toEqual([]);
    h.cleanup();
  });

  it('does not replay marks when the live stream re-pushes the same run (effect re-runs)', () => {
    const h = harness({ phase: 'Running', startedAtMicros: T0 * 1000, windowMicros: 10_000_000 });
    vi.advanceTimersByTime(7_000); // remaining 3s → 5,4,3 fired
    expect(h.fired).toEqual([5, 4, 3]);

    // Same-content pushes re-run the $effect; the fired set survives — no replay burst.
    h.poke();
    h.poke();
    expect(h.fired).toEqual([5, 4, 3]);

    // …and the remaining marks still land on time.
    vi.advanceTimersByTime(3_000);
    expect(h.fired).toEqual([5, 4, 3, 2, 1, 'end']);
    h.cleanup();
  });

  it('a re-mount mid-race does NOT replay the already-past pips (silent pre-mark)', () => {
    // First mount rides the run down to remaining ~3.5s, then unmounts (navigation away).
    const startedAtMicros = T0 * 1000;
    const h1 = harness({ phase: 'Running', startedAtMicros, windowMicros: 20_000_000 });
    vi.advanceTimersByTime(16_500); // remaining 3.5s → 5,4 fired
    expect(h1.fired).toEqual([5, 4]);
    h1.cleanup();

    // A fresh mount onto the SAME run at remaining 3.5s: the 5s/4s pips are already past — they
    // pre-mark silently; only 3,2,1 + the buzzer fire from here.
    const h2 = harness({ phase: 'Running', startedAtMicros, windowMicros: 20_000_000 });
    expect(h2.fired).toEqual([]);
    vi.advanceTimersByTime(3_500);
    expect(h2.fired).toEqual([3, 2, 1, 'end']);
    h2.cleanup();
  });

  it('mounting after the window end is fully silent (everything pre-marked)', () => {
    const startedAtMicros = (T0 - 60_000) * 1000; // the window ended 30s ago
    const h = harness({ phase: 'Running', startedAtMicros, windowMicros: 30_000_000 });
    vi.advanceTimersByTime(10_000);
    expect(h.fired).toEqual([]);
    h.cleanup();
  });

  it('a Restart (new race_started_at) counts the fresh run down again', () => {
    const h = harness({ phase: 'Running', startedAtMicros: T0 * 1000, windowMicros: 8_000_000 });
    vi.advanceTimersByTime(8_000);
    expect(h.fired).toEqual([5, 4, 3, 2, 1, 'end']);

    // Abort/Restart: back through Scheduled, then a NEW run anchor for the same heat.
    h.set({ phase: 'Scheduled', startedAtMicros: null });
    const t1 = T0 + 20_000;
    vi.setSystemTime(t1);
    h.set({ phase: 'Running', startedAtMicros: t1 * 1000 });
    vi.advanceTimersByTime(8_000);
    expect(h.fired).toEqual([5, 4, 3, 2, 1, 'end', 5, 4, 3, 2, 1, 'end']);
    h.cleanup();
  });

  it('stops scheduling once the heat leaves Running (early ForceEnd → no buzzer)', () => {
    const h = harness({ phase: 'Running', startedAtMicros: T0 * 1000, windowMicros: 10_000_000 });
    vi.advanceTimersByTime(6_000); // remaining 4s → 5,4 fired
    expect(h.fired).toEqual([5, 4]);

    // The RD force-ends early: the heat folds to Unofficial before the window closes.
    h.set({ phase: 'Unofficial' });
    vi.advanceTimersByTime(10_000);
    expect(h.fired).toEqual([5, 4]); // no 3,2,1, and crucially no end buzzer
    h.cleanup();
  });

  it('holds quiet while Running before the server race-start has propagated', () => {
    const h = harness({ phase: 'Running', startedAtMicros: null, windowMicros: 4_000_000 });
    vi.advanceTimersByTime(2_000);
    expect(h.fired).toEqual([]);
    // The anchor lands (race began at T0): remaining is 2s — the 5s/4s/3s marks pre-mark
    // silently (already past), and 2,1 + the buzzer fire on time.
    h.set({ startedAtMicros: T0 * 1000 });
    vi.advanceTimersByTime(2_000);
    expect(h.fired).toEqual([2, 1, 'end']);
    h.cleanup();
  });
});
