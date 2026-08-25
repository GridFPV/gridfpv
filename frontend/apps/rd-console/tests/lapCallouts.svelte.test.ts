import { describe, expect, it } from 'vitest';
import { flushSync } from 'svelte';
import type { LiveCrossing, PilotProgress } from '@gridfpv/types';
import {
  useCrossingTones,
  useLapCallouts,
  type LapCrossing
} from '../src/lib/lapCallouts.svelte.js';

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

/**
 * Unit tests for the per-CROSSING detector behind the crossing tone (#397). The point of the whole
 * feature is that this fires where the lap detector above cannot — the holeshot and a pass the
 * min-lap floor rejected close no lap, and a crossing on a seat nobody is flying belongs to no
 * lineup at all. Pins:
 *   • a tone per crossing whatever its disposition — holeshot, counted, rejected, marshal-voided;
 *   • an unseated seat's crossing tones too (the false-crossing case, which is the FEATURE);
 *   • a re-pushed identical state fires NOTHING (identity is `pass_ref`, not frame arrival);
 *   • the first frame BASELINES silently even carrying a full 64-deep feed (mid-heat mount);
 *   • nothing outside Running, and the watermark still advances there (no burst on race-go);
 *   • an event switch re-baselines — append offsets restart per event log.
 */
describe('useCrossingTones', () => {
  const crossing = (
    passRef: number,
    competitor: string,
    disposition: LiveCrossing['disposition'],
    lapNumber?: number
  ): LiveCrossing => ({
    pass_ref: passRef,
    competitor,
    at: passRef * 1_000_000,
    disposition,
    lap_number: lapNumber
  });

  function harness(initial?: {
    scope?: string | undefined;
    phase?: string;
    crossings?: LiveCrossing[];
  }) {
    let scope = $state<string | undefined>('scope' in (initial ?? {}) ? initial?.scope : 'event-1');
    let phase = $state<string | undefined>(initial?.phase ?? 'Running');
    let feed = $state<LiveCrossing[] | undefined>(initial?.crossings ?? []);
    const toned: LiveCrossing[] = [];
    const cleanup = $effect.root(() => {
      useCrossingTones(
        () => scope,
        () => phase,
        () => feed,
        (c) => toned.push(c)
      );
    });
    flushSync();
    return {
      toned,
      set(next: { scope?: string | undefined; phase?: string; crossings?: LiveCrossing[] }) {
        if ('scope' in next) scope = next.scope;
        if ('phase' in next) phase = next.phase;
        // A fresh array every push, exactly as a `$state.raw` live-state replacement delivers it —
        // so re-push idempotency is proved against a NEW reference, not a reused one.
        if ('crossings' in next) feed = [...(next.crossings ?? [])];
        flushSync();
      },
      cleanup
    };
  }

  it('tones on EVERY crossing — holeshot, counted, rejected-too-short and marshal-voided alike', () => {
    const h = harness();
    const holeshot = crossing(10, 'maverick-1', 'Holeshot');
    const counted = crossing(11, 'maverick-1', 'Counted', 1);
    const rejected = crossing(12, 'maverick-1', 'RejectedTooShort');
    const voided = crossing(13, 'goose-2', 'VoidedByMarshal');

    h.set({ crossings: [holeshot] });
    h.set({ crossings: [holeshot, counted] });
    h.set({ crossings: [holeshot, counted, rejected] });
    h.set({ crossings: [holeshot, counted, rejected, voided] });

    expect(h.toned).toEqual([holeshot, counted, rejected, voided]);
    expect(h.toned.map((c) => c.disposition)).toEqual([
      'Holeshot',
      'Counted',
      'RejectedTooShort',
      'VoidedByMarshal'
    ]);
    h.cleanup();
  });

  it('tones a crossing on an UNSEATED seat — a phantom detection is the feature, not noise', () => {
    const h = harness();
    // `node-7` is in no lineup and has no pilot binding: the feed reports it anyway, and an RD
    // hearing a pip with nobody on course is exactly how a too-sensitive gate gets noticed.
    const phantom = crossing(20, 'node-7', 'Holeshot');
    h.set({ crossings: [phantom] });
    expect(h.toned).toEqual([phantom]);
    h.cleanup();
  });

  it('a RE-PUSHED identical state fires nothing — novelty is pass_ref, never frame arrival', () => {
    const h = harness();
    const feed = [crossing(30, 'maverick-1', 'Holeshot'), crossing(31, 'maverick-1', 'Counted', 1)];
    h.set({ crossings: feed });
    expect(h.toned).toHaveLength(2);

    // Three more pushes of the same crossings (a stream wake-up, a re-snapshot, a resubscribe).
    h.set({ crossings: feed });
    h.set({ crossings: feed });
    h.set({ crossings: feed });
    expect(h.toned).toHaveLength(2);

    // A RE-LABELLED crossing is not a new crossing: same pass_ref, marshal-changed disposition.
    h.set({
      crossings: [
        crossing(30, 'maverick-1', 'Holeshot'),
        crossing(31, 'maverick-1', 'VoidedByMarshal')
      ]
    });
    expect(h.toned).toHaveLength(2);

    // The next genuinely new offset still tones.
    const next = crossing(32, 'goose-2', 'Counted', 1);
    h.set({ crossings: [...feed, next] });
    expect(h.toned).toHaveLength(3);
    expect(h.toned[2]).toEqual(next);
    h.cleanup();
  });

  it('BASELINES silently on the first frame, even carrying a full feed (mid-heat mount)', () => {
    // A mid-heat mount or a reconnect arrives with up to the feed's whole 64-entry bound unseen.
    const history = Array.from({ length: 64 }, (_, i) =>
      crossing(100 + i, `seat-${i % 8}`, 'Counted', i)
    );
    const h = harness({ crossings: history });
    expect(h.toned).toEqual([]);

    // Only what arrives AFTER the baseline tones.
    const fresh = crossing(164, 'seat-0', 'Counted', 9);
    h.set({ crossings: [...history.slice(1), fresh] });
    expect(h.toned).toEqual([fresh]);
    h.cleanup();
  });

  it('fires nothing outside Running, and advances the watermark there (no burst on race-go)', () => {
    const h = harness({ phase: 'Armed', crossings: [] });
    const armedPhantom = crossing(40, 'node-3', 'Holeshot');
    h.set({ crossings: [armedPhantom] });
    expect(h.toned).toEqual([]);

    // Race-go: the crossings already retired while Armed must NOT all fire at once.
    h.set({ phase: 'Running' });
    expect(h.toned).toEqual([]);

    const live = crossing(41, 'maverick-1', 'Counted', 1);
    h.set({ crossings: [armedPhantom, live] });
    expect(h.toned).toEqual([live]);

    // The heat finishes; a late marshaling fold on the finished heat is silent again.
    h.set({ phase: 'Unofficial' });
    h.set({ crossings: [armedPhantom, live, crossing(42, 'maverick-1', 'Counted', 2)] });
    expect(h.toned).toEqual([live]);
    h.cleanup();
  });

  it('an EVENT switch re-baselines — append offsets restart in a different log', () => {
    const h = harness();
    h.set({ crossings: [crossing(500, 'maverick-1', 'Counted', 1)] });
    expect(h.toned).toHaveLength(1);

    // A different event's log starts its offsets at zero. Without a scope reset the high watermark
    // (500) would swallow that event's entire race.
    h.set({ scope: 'event-2', crossings: [crossing(3, 'carla-3', 'Holeshot')] });
    expect(h.toned).toHaveLength(1); // the new scope's first frame baselines, silently

    const fresh = crossing(4, 'carla-3', 'Counted', 1);
    h.set({ crossings: [crossing(3, 'carla-3', 'Holeshot'), fresh] });
    expect(h.toned).toHaveLength(2);
    expect(h.toned[1]).toEqual(fresh);
    h.cleanup();
  });

  it('tones each of eight near-simultaneous crossings exactly once, in offset order', () => {
    const h = harness();
    h.set({ crossings: [crossing(60, 'seat-0', 'Holeshot')] });
    expect(h.toned).toHaveLength(1);

    // A whole pack lands in ONE frame (the stream coalesces): eight crossings, eight tones.
    const pack = Array.from({ length: 8 }, (_, i) => crossing(61 + i, `seat-${i}`, 'Counted', 1));
    h.set({ crossings: [crossing(60, 'seat-0', 'Holeshot'), ...pack] });
    expect(h.toned.slice(1)).toEqual(pack);
    expect(h.toned.slice(1).map((c) => c.pass_ref)).toEqual([61, 62, 63, 64, 65, 66, 67, 68]);
    h.cleanup();
  });

  it('never re-fires when a marshal-inserted pass arrives with an OLDER source time', () => {
    const h = harness();
    const first = crossing(70, 'maverick-1', 'Holeshot');
    const second = crossing(71, 'maverick-1', 'Counted', 1);
    h.set({ crossings: [first, second] });
    expect(h.toned).toHaveLength(2);

    // A marshal inserts a missed pass: NEW offset (72), OLD source time (between 70 and 71). The
    // feed stays ordered by offset, so it is one new tone — and the two seen ones stay silent.
    const inserted: LiveCrossing = { ...crossing(72, 'maverick-1', 'Counted', 2), at: 70_500_000 };
    h.set({ crossings: [first, second, inserted] });
    expect(h.toned).toHaveLength(3);
    expect(h.toned[2]).toEqual(inserted);
    h.cleanup();
  });
});
