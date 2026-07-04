import { describe, expect, it } from 'vitest';
import { flushSync } from 'svelte';
import type { PilotProgress } from '@gridfpv/types';
import { useLapCallouts, type LapCrossing } from '../src/lib/lapCallouts.svelte.js';

/**
 * Unit tests for the new-lap detector behind the audio callouts. Driven inside an `$effect.root`
 * (the raceClock test pattern) with reactive `$state` for the live inputs. Pins:
 *   • a lap-count increase on the current Running heat fires once, with ref + lap + last-lap;
 *   • the first sight of a run BASELINES silently (late join / re-mount: no ghost narration);
 *   • nothing fires outside Running (a finished heat being corrected can never call out);
 *   • a count decrease (a live correction fold) resyncs silently;
 *   • a heat/run change re-baselines rather than diffing across heats.
 */
describe('useLapCallouts', () => {
  const progressRow = (
    competitor: string,
    laps: number,
    lastLapMicros?: number
  ): PilotProgress => ({ competitor, laps_completed: laps, last_lap_micros: lastLapMicros });

  function harness(initial?: {
    phase?: string;
    heat?: string | undefined;
    startedAtMicros?: number | null;
    progress?: PilotProgress[];
  }) {
    let phase = $state<string | undefined>(initial?.phase ?? 'Running');
    let heat = $state<string | undefined>('heat' in (initial ?? {}) ? initial?.heat : 'heat-1');
    let startedAt = $state<number | null | undefined>(initial?.startedAtMicros ?? 1_000_000);
    let progress = $state<PilotProgress[]>(initial?.progress ?? []);
    const crossings: LapCrossing[] = [];
    const cleanup = $effect.root(() => {
      useLapCallouts(
        () => phase,
        () => heat,
        () => startedAt,
        () => progress,
        (c) => crossings.push(c)
      );
    });
    flushSync();
    return {
      crossings,
      set(next: {
        phase?: string;
        heat?: string | undefined;
        startedAtMicros?: number | null;
        progress?: PilotProgress[];
      }) {
        if ('phase' in next) phase = next.phase;
        if ('heat' in next) heat = next.heat;
        if ('startedAtMicros' in next) startedAt = next.startedAtMicros;
        if ('progress' in next) progress = next.progress ?? [];
        flushSync();
      },
      cleanup
    };
  }

  it('fires once per new lap on the current Running heat, carrying ref + lap + last-lap', () => {
    const h = harness({ progress: [progressRow('maverick-1', 0), progressRow('goose-2', 0)] });
    expect(h.crossings).toEqual([]);

    h.set({ progress: [progressRow('maverick-1', 1, 21_400_000), progressRow('goose-2', 0)] });
    expect(h.crossings).toEqual([{ ref: 'maverick-1', lap: 1, lastLapMicros: 21_400_000 }]);

    // A same-content re-push (the stream re-emitting) fires nothing.
    h.set({ progress: [progressRow('maverick-1', 1, 21_400_000), progressRow('goose-2', 0)] });
    expect(h.crossings).toHaveLength(1);

    // Both cross: one crossing each, in row order.
    h.set({
      progress: [progressRow('maverick-1', 2, 20_900_000), progressRow('goose-2', 1, 22_000_000)]
    });
    expect(h.crossings).toEqual([
      { ref: 'maverick-1', lap: 1, lastLapMicros: 21_400_000 },
      { ref: 'maverick-1', lap: 2, lastLapMicros: 20_900_000 },
      { ref: 'goose-2', lap: 1, lastLapMicros: 22_000_000 }
    ]);
    h.cleanup();
  });

  it('BASELINES silently on first sight of a run (late join / re-mount mid-race)', () => {
    // Mounting onto a heat already at laps 7/6 must not narrate history.
    const h = harness({
      progress: [progressRow('maverick-1', 7, 20_000_000), progressRow('goose-2', 6, 21_000_000)]
    });
    expect(h.crossings).toEqual([]);

    // The NEXT lap after the baseline calls out normally.
    h.set({
      progress: [progressRow('maverick-1', 8, 19_800_000), progressRow('goose-2', 6, 21_000_000)]
    });
    expect(h.crossings).toEqual([{ ref: 'maverick-1', lap: 8, lastLapMicros: 19_800_000 }]);
    h.cleanup();
  });

  it('fires nothing outside Running — corrections on a finished heat can never call out', () => {
    const h = harness({
      phase: 'Unofficial',
      progress: [progressRow('maverick-1', 3, 20_000_000)]
    });
    // A marshaling correction bumps the count on the (finished) heat's fold: silent.
    h.set({ progress: [progressRow('maverick-1', 4, 20_000_000)] });
    expect(h.crossings).toEqual([]);
    h.cleanup();
  });

  it('re-baselines when the heat returns to Running (post-fold counts are not "new laps")', () => {
    const h = harness({ progress: [progressRow('maverick-1', 0)] });
    h.set({ progress: [progressRow('maverick-1', 1, 25_000_000)] });
    expect(h.crossings).toHaveLength(1);

    // The heat finishes, then a NEW run of the same heat starts (Restart → new anchor): the
    // fresh run baselines at its own first snapshot; no callouts for pre-existing counts.
    h.set({ phase: 'Unofficial' });
    h.set({
      phase: 'Running',
      startedAtMicros: 2_000_000,
      progress: [progressRow('maverick-1', 0)]
    });
    expect(h.crossings).toHaveLength(1);
    h.set({ progress: [progressRow('maverick-1', 1, 24_000_000)] });
    expect(h.crossings).toHaveLength(2);
    expect(h.crossings[1]).toEqual({ ref: 'maverick-1', lap: 1, lastLapMicros: 24_000_000 });
    h.cleanup();
  });

  it('a count DECREASE (a live correction fold) resyncs silently', () => {
    const h = harness({ progress: [progressRow('maverick-1', 0)] });
    h.set({ progress: [progressRow('maverick-1', 2, 20_000_000)] });
    expect(h.crossings).toHaveLength(1); // one increase event → one callout (lap 2)

    // A correction folds the count back down: no callout.
    h.set({ progress: [progressRow('maverick-1', 1, 20_000_000)] });
    expect(h.crossings).toHaveLength(1);

    // The next genuine crossing (back up to 2) announces lap 2 again — correct after the fold.
    h.set({ progress: [progressRow('maverick-1', 2, 19_000_000)] });
    expect(h.crossings).toHaveLength(2);
    expect(h.crossings[1]).toEqual({ ref: 'maverick-1', lap: 2, lastLapMicros: 19_000_000 });
    h.cleanup();
  });

  it('a heat SWAP re-baselines — the new heat\'s existing counts are not "new laps"', () => {
    const h = harness({ heat: 'heat-1', progress: [progressRow('maverick-1', 0)] });
    // The stream swaps to a different, already-running heat carrying non-zero counts.
    h.set({
      heat: 'heat-2',
      startedAtMicros: 5_000_000,
      progress: [progressRow('carla-3', 4, 23_000_000)]
    });
    expect(h.crossings).toEqual([]);

    // Its next lap calls out.
    h.set({ progress: [progressRow('carla-3', 5, 22_500_000)] });
    expect(h.crossings).toEqual([{ ref: 'carla-3', lap: 5, lastLapMicros: 22_500_000 }]);
    h.cleanup();
  });

  it('fires nothing while there is no current heat', () => {
    const h = harness({ heat: undefined, progress: [progressRow('maverick-1', 0)] });
    h.set({ progress: [progressRow('maverick-1', 1, 20_000_000)] });
    expect(h.crossings).toEqual([]);
    h.cleanup();
  });
});
