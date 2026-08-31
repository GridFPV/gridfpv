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
 *   • a newly closed lap fires once, with ref + lap + last-lap;
 *   • #417: a count DIP and RECOVERY announces nothing — a lap is spoken by the pass that closed
 *     it, never by a count that moved; a genuinely new closing pass still announces;
 *   • a holeshot / floor-rejected / marshal-voided crossing closes no lap and is silent;
 *   • the first sight of a run BASELINES silently (late join / re-mount: no ghost narration), and
 *     a reconnect re-pushing the whole feed re-announces nothing;
 *   • nothing fires outside Running (a finished heat being corrected can never call out);
 *   • a heat/run change re-baselines rather than carrying a watermark across heats.
 */
describe('useLapCallouts', () => {
  const progressRow = (
    competitor: string,
    laps: number,
    lastLapMicros?: number
  ): PilotProgress => ({ competitor, laps_completed: laps, last_lap_micros: lastLapMicros });

  const pass = (
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
  /** The crossing that CLOSES a lap — the only kind this detector speaks for. */
  const closes = (passRef: number, competitor: string, lapNumber: number): LiveCrossing =>
    pass(passRef, competitor, 'Counted', lapNumber);

  function harness(initial?: {
    phase?: string;
    heat?: string | undefined;
    startedAtMicros?: number | null;
    crossings?: LiveCrossing[];
    progress?: PilotProgress[];
  }) {
    let phase = $state<string | undefined>(initial?.phase ?? 'Running');
    let heat = $state<string | undefined>('heat' in (initial ?? {}) ? initial?.heat : 'heat-1');
    let startedAt = $state<number | null | undefined>(initial?.startedAtMicros ?? 1_000_000);
    let feed = $state<LiveCrossing[]>(initial?.crossings ?? []);
    let progress = $state<PilotProgress[]>(initial?.progress ?? []);
    const laps: LapCrossing[] = [];
    const cleanup = $effect.root(() => {
      useLapCallouts(
        () => phase,
        () => heat,
        () => startedAt,
        () => feed,
        () => progress,
        (c) => laps.push(c)
      );
    });
    flushSync();
    return {
      laps,
      set(next: {
        phase?: string;
        heat?: string | undefined;
        startedAtMicros?: number | null;
        crossings?: LiveCrossing[];
        progress?: PilotProgress[];
      }) {
        if ('phase' in next) phase = next.phase;
        if ('heat' in next) heat = next.heat;
        if ('startedAtMicros' in next) startedAt = next.startedAtMicros;
        // A fresh array every push, exactly as a `$state.raw` live-state replacement delivers it —
        // so re-push idempotency is proved against a NEW reference, not a reused one.
        if ('crossings' in next) feed = [...(next.crossings ?? [])];
        if ('progress' in next) progress = [...(next.progress ?? [])];
        flushSync();
      },
      cleanup
    };
  }

  it('fires once per newly closed lap, carrying ref + lap + last-lap', () => {
    const h = harness({ progress: [progressRow('maverick-1', 0), progressRow('goose-2', 0)] });
    expect(h.laps).toEqual([]);

    // The holeshot OPENS the first lap and closes none: it tones (#397), it does not speak.
    const holeshot = pass(10, 'maverick-1', 'Holeshot');
    h.set({ crossings: [holeshot] });
    expect(h.laps).toEqual([]);

    const lap1 = closes(11, 'maverick-1', 1);
    h.set({
      crossings: [holeshot, lap1],
      progress: [progressRow('maverick-1', 1, 21_400_000), progressRow('goose-2', 0)]
    });
    expect(h.laps).toEqual([{ ref: 'maverick-1', lap: 1, lastLapMicros: 21_400_000 }]);

    // A same-content re-push (the stream re-emitting) fires nothing.
    h.set({ crossings: [holeshot, lap1] });
    expect(h.laps).toHaveLength(1);

    // Both pilots close a lap in the same frame: one callout each, in feed order.
    const lap2 = closes(12, 'maverick-1', 2);
    const gooseLap1 = closes(13, 'goose-2', 1);
    h.set({
      crossings: [holeshot, lap1, lap2, gooseLap1],
      progress: [progressRow('maverick-1', 2, 20_900_000), progressRow('goose-2', 1, 22_000_000)]
    });
    expect(h.laps).toEqual([
      { ref: 'maverick-1', lap: 1, lastLapMicros: 21_400_000 },
      { ref: 'maverick-1', lap: 2, lastLapMicros: 20_900_000 },
      { ref: 'goose-2', lap: 1, lastLapMicros: 22_000_000 }
    ]);
    h.cleanup();
  });

  it('a count DIP and RECOVERY announces nothing; a new closing pass still does (#417)', () => {
    // The exact bench sequence the RD heard: announce N, the live count dips to N-1, it recovers
    // to N — and the old count-diff detector read that recovery as an increase and said "lap N" a
    // second time. Nothing new closed a lap, so nothing is said.
    const holeshot = pass(10, 'maverick-1', 'Holeshot');
    const lap1 = closes(11, 'maverick-1', 1);
    const lap2 = closes(12, 'maverick-1', 2);
    const h = harness({ progress: [progressRow('maverick-1', 0)] });

    h.set({ crossings: [holeshot, lap1], progress: [progressRow('maverick-1', 1, 21_400_000)] });
    h.set({
      crossings: [holeshot, lap1, lap2],
      progress: [progressRow('maverick-1', 2, 20_900_000)]
    });
    expect(h.laps).toEqual([
      { ref: 'maverick-1', lap: 1, lastLapMicros: 21_400_000 },
      { ref: 'maverick-1', lap: 2, lastLapMicros: 20_900_000 }
    ]);

    // DIP: a re-fold hands the count back DOWN to 1 (the closing pass goes with it).
    h.set({ crossings: [holeshot, lap1], progress: [progressRow('maverick-1', 1, 21_400_000)] });
    expect(h.laps).toHaveLength(2);

    // RECOVERY to the same value, on the SAME closing pass — the repeat, and now silent.
    h.set({
      crossings: [holeshot, lap1, lap2],
      progress: [progressRow('maverick-1', 2, 20_900_000)]
    });
    expect(h.laps).toHaveLength(2);

    // A GENUINELY new lap — a closing pass never seen before — still announces.
    h.set({
      crossings: [holeshot, lap1, lap2, closes(13, 'maverick-1', 3)],
      progress: [progressRow('maverick-1', 3, 19_800_000)]
    });
    expect(h.laps).toHaveLength(3);
    expect(h.laps[2]).toEqual({ ref: 'maverick-1', lap: 3, lastLapMicros: 19_800_000 });
    h.cleanup();
  });

  it('a pass the min-lap floor REJECTED closes no lap — it tones, it never speaks', () => {
    // The RD's round has `min_lap_secs: 5`, so a quick second crossing is auto-suppressed on every
    // live fold: it reaches this detector already dispositioned, carrying no lap number.
    const holeshot = pass(20, 'maverick-1', 'Holeshot');
    const lap1 = closes(21, 'maverick-1', 1);
    const h = harness({ progress: [progressRow('maverick-1', 0)] });
    h.set({ crossings: [holeshot, lap1], progress: [progressRow('maverick-1', 1, 21_000_000)] });
    expect(h.laps).toEqual([{ ref: 'maverick-1', lap: 1, lastLapMicros: 21_000_000 }]);

    const quick = pass(22, 'maverick-1', 'RejectedTooShort');
    h.set({ crossings: [holeshot, lap1, quick] });
    expect(h.laps).toHaveLength(1);

    // And the genuine next lap speaks "lap 2" exactly once.
    h.set({
      crossings: [holeshot, lap1, quick, closes(23, 'maverick-1', 2)],
      progress: [progressRow('maverick-1', 2, 20_000_000)]
    });
    expect(h.laps).toHaveLength(2);
    expect(h.laps[1]).toEqual({ ref: 'maverick-1', lap: 2, lastLapMicros: 20_000_000 });
    h.cleanup();
  });

  it('a RE-LABELLED crossing is not a new lap — a marshal voiding one re-announces nothing', () => {
    const holeshot = pass(30, 'maverick-1', 'Holeshot');
    const lap1 = closes(31, 'maverick-1', 1);
    const h = harness({ progress: [progressRow('maverick-1', 0)] });
    h.set({ crossings: [holeshot, lap1], progress: [progressRow('maverick-1', 1, 21_000_000)] });
    expect(h.laps).toHaveLength(1);

    // A marshal voids it, then the void is undone: same `pass_ref` throughout, so the lap is never
    // spoken twice however its disposition is re-derived.
    h.set({
      crossings: [holeshot, pass(31, 'maverick-1', 'VoidedByMarshal')],
      progress: [progressRow('maverick-1', 0)]
    });
    h.set({ crossings: [holeshot, lap1], progress: [progressRow('maverick-1', 1, 21_000_000)] });
    expect(h.laps).toHaveLength(1);
    h.cleanup();
  });

  it('BASELINES silently on first sight of a run (late join / re-mount mid-race)', () => {
    // Mounting onto a heat already at laps 7/6 must not narrate history — the feed legitimately
    // arrives carrying up to its whole bound.
    const history = [
      pass(100, 'maverick-1', 'Holeshot'),
      ...Array.from({ length: 7 }, (_, i) => closes(101 + i, 'maverick-1', i + 1)),
      pass(110, 'goose-2', 'Holeshot'),
      ...Array.from({ length: 6 }, (_, i) => closes(111 + i, 'goose-2', i + 1))
    ];
    const h = harness({
      crossings: history,
      progress: [progressRow('maverick-1', 7, 20_000_000), progressRow('goose-2', 6, 21_000_000)]
    });
    expect(h.laps).toEqual([]);

    // The NEXT lap after the baseline calls out normally.
    h.set({
      crossings: [...history, closes(120, 'maverick-1', 8)],
      progress: [progressRow('maverick-1', 8, 19_800_000), progressRow('goose-2', 6, 21_000_000)]
    });
    expect(h.laps).toEqual([{ ref: 'maverick-1', lap: 8, lastLapMicros: 19_800_000 }]);
    h.cleanup();
  });

  it('a RECONNECT re-pushing the whole feed re-announces nothing', () => {
    const holeshot = pass(40, 'maverick-1', 'Holeshot');
    const lap1 = closes(41, 'maverick-1', 1);
    const h = harness({
      crossings: [holeshot],
      progress: [progressRow('maverick-1', 0)]
    });
    h.set({ crossings: [holeshot, lap1], progress: [progressRow('maverick-1', 1, 21_000_000)] });
    expect(h.laps).toHaveLength(1);

    // The socket drops; a fresh snapshot + a resubscribe re-deliver the same feed, twice over.
    h.set({ crossings: [holeshot, lap1] });
    h.set({ crossings: [holeshot, lap1] });
    expect(h.laps).toHaveLength(1);
    h.cleanup();
  });

  it('fires nothing outside Running — corrections on a finished heat can never call out', () => {
    const h = harness({
      phase: 'Unofficial',
      crossings: [pass(50, 'maverick-1', 'Holeshot')],
      progress: [progressRow('maverick-1', 3, 20_000_000)]
    });
    // A marshaling correction appends a pass on the (finished) heat's fold: silent.
    h.set({
      crossings: [pass(50, 'maverick-1', 'Holeshot'), closes(51, 'maverick-1', 4)],
      progress: [progressRow('maverick-1', 4, 20_000_000)]
    });
    expect(h.laps).toEqual([]);
    h.cleanup();
  });

  it('re-baselines when the heat returns to Running (a Restart is a fresh run)', () => {
    const holeshot = pass(60, 'maverick-1', 'Holeshot');
    const lap1 = closes(61, 'maverick-1', 1);
    const h = harness({ progress: [progressRow('maverick-1', 0)] });
    h.set({ crossings: [holeshot, lap1], progress: [progressRow('maverick-1', 1, 25_000_000)] });
    expect(h.laps).toHaveLength(1);

    // The heat finishes, then a NEW run of the same heat starts (Restart → new anchor). The old
    // run's crossings are still on the feed; the fresh run retires them silently.
    h.set({ phase: 'Unofficial' });
    h.set({
      phase: 'Running',
      startedAtMicros: 2_000_000,
      crossings: [holeshot, lap1],
      progress: [progressRow('maverick-1', 0)]
    });
    expect(h.laps).toHaveLength(1);

    h.set({
      crossings: [holeshot, lap1, pass(70, 'maverick-1', 'Holeshot'), closes(71, 'maverick-1', 1)],
      progress: [progressRow('maverick-1', 1, 24_000_000)]
    });
    expect(h.laps).toHaveLength(2);
    expect(h.laps[1]).toEqual({ ref: 'maverick-1', lap: 1, lastLapMicros: 24_000_000 });
    h.cleanup();
  });

  it("a heat SWAP re-baselines — the new heat's feed is not a backlog of new laps", () => {
    const h = harness({ heat: 'heat-1', progress: [progressRow('maverick-1', 0)] });
    // The stream swaps to a different, already-running heat whose feed carries higher offsets.
    const backlog = [
      pass(200, 'carla-3', 'Holeshot'),
      ...Array.from({ length: 4 }, (_, i) => closes(201 + i, 'carla-3', i + 1))
    ];
    h.set({
      heat: 'heat-2',
      startedAtMicros: 5_000_000,
      crossings: backlog,
      progress: [progressRow('carla-3', 4, 23_000_000)]
    });
    expect(h.laps).toEqual([]);

    // Its next lap calls out.
    h.set({
      crossings: [...backlog, closes(210, 'carla-3', 5)],
      progress: [progressRow('carla-3', 5, 22_500_000)]
    });
    expect(h.laps).toEqual([{ ref: 'carla-3', lap: 5, lastLapMicros: 22_500_000 }]);
    h.cleanup();
  });

  it('fires nothing while there is no current heat', () => {
    const h = harness({ heat: undefined, progress: [progressRow('maverick-1', 0)] });
    h.set({
      crossings: [pass(80, 'maverick-1', 'Holeshot'), closes(81, 'maverick-1', 1)],
      progress: [progressRow('maverick-1', 1, 20_000_000)]
    });
    expect(h.laps).toEqual([]);
    h.cleanup();
  });

  it("does not narrate a seat nobody is flying — the voice is the lineup's, the tone is not", () => {
    const h = harness({ progress: [progressRow('maverick-1', 0)] });
    // `node-7` is in no lineup, so it has no `progress` row. Its crossings still tone (#397).
    h.set({
      crossings: [pass(90, 'node-7', 'Holeshot'), closes(91, 'node-7', 1)],
      progress: [progressRow('maverick-1', 0)]
    });
    expect(h.laps).toEqual([]);
    h.cleanup();
  });

  it('speaks the lap number alone when the last-lap time belongs to another lap', () => {
    // Two of one pilot's laps close in a single frame: `last_lap_micros` is the NEWER one's, so
    // the older lap is spoken without a time rather than with a time that is not its own.
    const holeshot = pass(300, 'maverick-1', 'Holeshot');
    const h = harness({ progress: [progressRow('maverick-1', 0)] });
    h.set({ crossings: [holeshot] });
    h.set({
      crossings: [holeshot, closes(301, 'maverick-1', 1), closes(302, 'maverick-1', 2)],
      progress: [progressRow('maverick-1', 2, 20_900_000)]
    });
    expect(h.laps).toEqual([
      { ref: 'maverick-1', lap: 1, lastLapMicros: undefined },
      { ref: 'maverick-1', lap: 2, lastLapMicros: 20_900_000 }
    ]);
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

  it('tones while ARMED — a crossing with everyone on the line is the clearest false positive', () => {
    const h = harness({ phase: 'Armed', crossings: [] });
    // Nobody is flying yet. Anything the gate reports here is either a phantom or a deliberate
    // pre-race gate check, and the RD wants to hear both.
    const armedPhantom = crossing(40, 'node-3', 'Holeshot');
    h.set({ crossings: [armedPhantom] });
    expect(h.toned).toEqual([armedPhantom]);

    // Race-go does not re-announce what already toned while Armed.
    h.set({ phase: 'Running' });
    expect(h.toned).toEqual([armedPhantom]);

    const live = crossing(41, 'maverick-1', 'Counted', 1);
    h.set({ crossings: [armedPhantom, live] });
    expect(h.toned).toEqual([armedPhantom, live]);
    h.cleanup();
  });

  it('is silent outside Armed/Running, and advances the watermark there (no burst on arming)', () => {
    const h = harness({ phase: 'Staged', crossings: [] });
    // Staged: the countdown is running, frequencies are assigned, nobody is on the gate yet.
    const stagedPhantom = crossing(40, 'node-3', 'Holeshot');
    h.set({ crossings: [stagedPhantom] });
    expect(h.toned).toEqual([]);

    // Arming must NOT dump what was retired silently while Staged.
    h.set({ phase: 'Armed' });
    expect(h.toned).toEqual([]);

    const live = crossing(41, 'maverick-1', 'Counted', 1);
    h.set({ phase: 'Running', crossings: [stagedPhantom, live] });
    expect(h.toned).toEqual([live]);

    // The heat finishes; a late marshaling fold on the finished heat is silent again.
    h.set({ phase: 'Unofficial' });
    h.set({ crossings: [stagedPhantom, live, crossing(42, 'maverick-1', 'Counted', 2)] });
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

  it('absorbs a same-competitor reflection burst into ONE tone (#503)', () => {
    // One physical pass, five detections milliseconds apart (a quad in the gate's near field).
    // The field rendered this as a pip storm per lap; the tone must answer "did the gate see
    // me?" once. Explicit `at` values — the cooldown runs on the source clock, not offsets.
    const h = harness();
    const burst = [
      { ...crossing(80, 'maverick-1', 'Counted', 1), at: 10_000_000 },
      { ...crossing(81, 'maverick-1', 'RejectedTooShort'), at: 10_061_000 },
      { ...crossing(82, 'maverick-1', 'RejectedTooShort'), at: 10_193_000 },
      { ...crossing(83, 'maverick-1', 'RejectedTooShort'), at: 10_254_000 },
      { ...crossing(84, 'maverick-1', 'RejectedTooShort'), at: 10_487_000 }
    ];
    h.set({ crossings: burst });
    expect(h.toned).toEqual([burst[0]]);

    // The next genuine lap — 17s later, far past the cooldown — tones again.
    const nextLap = { ...crossing(85, 'maverick-1', 'Counted', 2), at: 27_500_000 };
    h.set({ crossings: [...burst, nextLap] });
    expect(h.toned).toEqual([burst[0], nextLap]);
    h.cleanup();
  });

  it('measures the cooldown from the last SOUNDED tone — an absorbed crossing does not extend it', () => {
    const h = harness();
    const t0 = { ...crossing(90, 'maverick-1', 'Counted', 1), at: 10_000_000 };
    const absorbed = { ...crossing(91, 'maverick-1', 'RejectedTooShort'), at: 10_800_000 };
    // 1.2s after t0 but only 0.4s after the absorbed crossing: the window anchors on the TONE,
    // so this fires. (A storm that never pauses must not silence the gate indefinitely.)
    const clear = { ...crossing(92, 'maverick-1', 'RejectedTooShort'), at: 11_200_000 };
    h.set({ crossings: [t0] });
    h.set({ crossings: [t0, absorbed] });
    h.set({ crossings: [t0, absorbed, clear] });
    expect(h.toned).toEqual([t0, clear]);
    h.cleanup();
  });

  it('cools down PER COMPETITOR — two pilots crossing near-simultaneously both tone', () => {
    // The gate telling the RD it saw both pilots is the point of the feature; only repeats of
    // the SAME competitor are noise.
    const h = harness();
    const mav = { ...crossing(95, 'maverick-1', 'Counted', 1), at: 10_000_000 };
    const goose = { ...crossing(96, 'goose-2', 'Counted', 1), at: 10_050_000 };
    h.set({ crossings: [mav, goose] });
    expect(h.toned).toEqual([mav, goose]);
    h.cleanup();
  });
});
