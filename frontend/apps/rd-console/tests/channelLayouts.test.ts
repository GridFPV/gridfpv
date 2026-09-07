/**
 * Channel-layout readings (#117 S2) — the pure half.
 *
 * These are the sentences an RD reads while laying channels onto nodes, so they are asserted as
 * **text**. Two things are load-bearing here and are asserted in both directions:
 *
 *  - no raw id, raw node index or bare MHz reaches the screen — a node is `"Node 3"`, a channel is
 *    `"Raceband R7"`, a layout is `"Bracket A"` (CLAUDE.md's display rule);
 *  - cross-layout channel reuse produces a **warning sentence**, never a blocker — that split is the
 *    RD's own decision and is what makes the bracket and the GQ strategies share one mechanism.
 */
import { describe, expect, it } from 'vitest';
import type {
  ChannelCatalogEntry,
  ChannelLayout,
  ImdReading,
  TimerNode,
  TimerNodes
} from '@gridfpv/types';
import {
  allowedChannels,
  draftBlocker,
  draftChannelSet,
  draftNodes,
  duplicateNodes,
  IMD_BANDS,
  imdOffenderMessage,
  imdRatingLabel,
  imdTail,
  imdTone,
  imdToneHint,
  layoutName,
  layoutPilotCount,
  layoutRating,
  layerNodes,
  layerNodeLabel,
  layerSummary,
  overlapMessage,
  unconfiguredTimerMessage,
  untunedNodes,
  type LayerDraft
} from '../src/lib/channelLayouts.js';

const CATALOG: ChannelCatalogEntry[] = [
  { band: 'Raceband', channel: 'R1', mhz: 5658 },
  { band: 'Raceband', channel: 'R2', mhz: 5695 },
  { band: 'Raceband', channel: 'R3', mhz: 5732 },
  { band: 'Raceband', channel: 'R4', mhz: 5769 },
  { band: 'Fatshark', channel: 'F4', mhz: 5800 }
];

/** One node the way the Director sends it: 0-based index, 1-based label. */
function node(index: number, enabled = true): TimerNode {
  return {
    node: index,
    label: `Node ${index + 1}`,
    seat: `node-${index}`,
    enabled,
    reported: true
  };
}

/** A four-node timer with node index 2 disabled — an enabled set with a hole in it. */
function holedView(): TimerNodes {
  return {
    timer: 'rh-1',
    width: 4,
    nodes: [node(0), node(1), node(2, false), node(3)],
    enabled: [0, 1, 3]
  };
}

function draft(name: string, pairs: [number, number][]): LayerDraft {
  return { name, channels: new Map(pairs) };
}

const LAYERS: ChannelLayout[] = [
  {
    id: 'bracket-a-k3f9',
    name: 'Bracket A',
    nodes: [
      { node: 0, channel: 5658 },
      { node: 1, channel: 5695 }
    ]
  },
  {
    id: 'pack-b-z1x8',
    name: 'Pack B',
    nodes: [
      { node: 0, channel: 5658 },
      { node: 1, channel: 5800 }
    ]
  }
];

describe('the timer a layout draws from', () => {
  it('offers the ALLOWED set, in the RD’s own order — not the catalog', () => {
    // The Tune page offers everything the hardware can tune; a layout offers what the RD said this
    // timer may use. Offering more would offer a choice the Director then refuses.
    const timer = { available_channels: [5769, 5658] } as never;
    expect(allowedChannels(timer)).toEqual([5769, 5658]);
  });

  it('reads an empty allowed set as "not configured yet", never as "no channels"', () => {
    const timer = { name: 'NuclearHazard', available_channels: [] } as never;
    const message = unconfiguredTimerMessage(timer);
    expect(message).toContain('NuclearHazard');
    expect(message).toContain('Timers page');
  });

  it('says so when the event has no timer at all', () => {
    expect(unconfiguredTimerMessage(undefined)).toContain('no timer selected');
  });

  it('is quiet once channels are ticked', () => {
    const timer = { name: 'Mock', available_channels: [5658] } as never;
    expect(unconfiguredTimerMessage(timer)).toBeUndefined();
  });
});

describe('the nodes a layout tunes', () => {
  it('is the ENABLED set, holes and all (#412)', () => {
    // Node index 2 is disabled, so a layout must not offer to tune it — a disabled node seats
    // nobody, and pretending to tune it hides a dead gate.
    expect(layerNodes(holedView())).toEqual([0, 1, 3]);
  });

  it('is empty until the node view lands (fails closed)', () => {
    expect(layerNodes(undefined)).toEqual([]);
  });
});

describe('the draft', () => {
  it('goes on the wire ascending by node, with unset nodes simply absent', () => {
    expect(
      draftNodes(
        draft('A', [
          [3, 5769],
          [0, 5658]
        ])
      )
    ).toEqual([
      { node: 0, channel: 5658 },
      { node: 3, channel: 5769 }
    ]);
  });

  it('flags every node sharing a channel — the one hard rule inside a layout', () => {
    const clashing = draft('A', [
      [0, 5658],
      [1, 5695],
      [3, 5658]
    ]);
    expect([...duplicateNodes(clashing)].sort()).toEqual([0, 3]);
  });

  it('lists the nodes still untuned — a layout is a complete tuning', () => {
    expect(untunedNodes(draft('A', [[0, 5658]]), [0, 1, 3])).toEqual([1, 3]);
  });
});

describe('why Save is blocked', () => {
  it('asks for a name first — it is what a heat picks the layout by', () => {
    expect(draftBlocker(draft('  ', []), [0], holedView(), CATALOG)).toContain('Name this layout');
  });

  it('names both clashing nodes and the channel, never an index or a bare MHz', () => {
    const message = draftBlocker(
      draft('Bracket A', [
        [0, 5658],
        [1, 5695],
        [3, 5658]
      ]),
      [0, 1, 3],
      holedView(),
      CATALOG
    );
    expect(message).toContain('Node 1');
    expect(message).toContain('Node 4');
    expect(message).toContain('Raceband R1');
    expect(message).not.toContain('5658');
    expect(message).toContain('cannot share a frequency');
  });

  it('names the untuned nodes by their 1-based labels', () => {
    const message = draftBlocker(draft('Bracket A', [[0, 5658]]), [0, 1, 3], holedView(), CATALOG);
    expect(message).toBe('Set a channel for Node 2, Node 4 — a layout tunes every enabled node.');
  });

  it('is quiet for a complete, conflict-free tuning', () => {
    const complete = draft('Bracket A', [
      [0, 5658],
      [1, 5695],
      [3, 5769]
    ]);
    expect(draftBlocker(complete, [0, 1, 3], holedView(), CATALOG)).toBeUndefined();
  });
});

describe('what a layout looks like on screen', () => {
  it('renders a node and its channel together — the pair an RD needs', () => {
    expect(layerNodeLabel({ node: 2, channel: 5732 }, CATALOG)).toBe('Node 3 · Raceband R3');
  });

  it('summarises the whole tuning without a single raw value', () => {
    const summary = layerSummary(LAYERS[0], CATALOG);
    expect(summary).toBe('Node 1 · Raceband R1 · Node 2 · Raceband R2');
    expect(summary).not.toMatch(/\d{4}/);
  });

  it('resolves a layout id to its name, and a deleted one to a phrase', () => {
    expect(layoutName(LAYERS, 'pack-b-z1x8')).toBe('Pack B');
    expect(layoutName(LAYERS, 'gone')).toBe('a deleted layout');
  });
});

describe('cross-layout overlap', () => {
  it('warns by naming both layouts and the shared channel — and reads as ignorable', () => {
    const message = overlapMessage(
      { layout: 'bracket-a-k3f9', other: 'pack-b-z1x8', channels: [5658] },
      LAYERS,
      CATALOG
    );
    expect(message).toContain('Bracket A');
    expect(message).toContain('Pack B');
    expect(message).toContain('Raceband R1');
    // The RD's decision: fine for a bracket, only matters for the keep-pilots-on-one-channel
    // strategy. Nothing here should read as "this is broken".
    expect(message).toContain('That is fine for a bracket');
    // And no raw id or MHz leaks into it.
    expect(message).not.toContain('bracket-a-k3f9');
    expect(message).not.toContain('5658');
  });

  it('lists several shared channels readably', () => {
    const message = overlapMessage(
      { layout: 'bracket-a-k3f9', other: 'pack-b-z1x8', channels: [5658, 5695, 5732] },
      LAYERS,
      CATALOG
    );
    expect(message).toContain('Raceband R1, Raceband R2 and Raceband R3');
  });
});

describe('the IMD reading (#117 S4)', () => {
  /** The catalog the offender sentence resolves against — Raceband R1–R8 plus one Fatshark. */
  const BAND: ChannelCatalogEntry[] = [
    { band: 'Raceband', channel: 'R1', mhz: 5658 },
    { band: 'Raceband', channel: 'R2', mhz: 5695 },
    { band: 'Raceband', channel: 'R3', mhz: 5732 },
    { band: 'Raceband', channel: 'R8', mhz: 5917 },
    { band: 'Fatshark', channel: 'F4', mhz: 5800 }
  ];

  /** All of Raceband, as the Director reads it: −635, and R2 + R1 landing exactly on R3. */
  const RACEBAND8: ImdReading = {
    rating: -635,
    worst: { doubled: 5695, subtracted: 5658, product: 5732, lands_on: 5732, gap_mhz: 0 }
  };
  /** RotorHazard's IMD6C: 29, worst offender 12 MHz off R8. */
  const IMD6C: ImdReading = {
    rating: 29,
    worst: { doubled: 5800, subtracted: 5695, product: 5905, lands_on: 5917, gap_mhz: 12 }
  };
  /** Racebnd4 — nothing within 35 MHz, so there is no offender to name. */
  const CLEAN: ImdReading = { rating: 100 };

  /**
   * The whole reading as the screen renders it. The two halves are separate functions since #474
   * (the rating is coloured, the offender is not), and this is the sentence they compose into —
   * asserted here so the split cannot quietly change the text a person reads.
   */
  const imdLine = (reading: ImdReading, catalog: ChannelCatalogEntry[]) =>
    `${imdRatingLabel(reading)} · ${imdTail(reading, catalog)}`;

  it('shows the rating the way IMDTabler states it, minus sign and all', () => {
    expect(imdRatingLabel(CLEAN)).toBe('IMD 100');
    expect(imdRatingLabel(IMD6C)).toBe('IMD 29');
    // A real minus sign, not a hyphen — a bad set genuinely goes negative.
    expect(imdRatingLabel(RACEBAND8)).toBe('IMD −635');
  });

  it('names the worst offender by its arithmetic AND the channel it lands on', () => {
    expect(imdOffenderMessage(RACEBAND8, BAND)).toBe(
      '2 × Raceband R2 − Raceband R1 = 5732 MHz — lands on Raceband R3'
    );
    expect(imdOffenderMessage(IMD6C, BAND)).toBe(
      '2 × Fatshark F4 − Raceband R2 = 5905 MHz — 12 MHz off Raceband R8'
    );
  });

  it('names every channel and never leaves one as a bare frequency', () => {
    const message = imdLine(IMD6C, BAND);
    // The two that mix and the one they land near are all named.
    expect(message).toContain('Fatshark F4');
    expect(message).toContain('Raceband R2');
    expect(message).toContain('Raceband R8');
    // ...and none of their frequencies appears raw. The only number that does is the product,
    // which is arithmetic — nobody is flying 5905.
    expect(message).not.toContain('5800');
    expect(message).not.toContain('5695');
    expect(message).not.toContain('5917');
    expect(message).toContain('5905 MHz');
  });

  it('says a clean set is clean rather than inventing a nearest miss', () => {
    const message = imdLine(CLEAN, BAND);
    expect(message).toBe('IMD 100 · nothing mixes within 35 MHz of a channel this layout uses');
    expect(message).not.toContain('worst offender');
  });

  it('carries no verdict word and no clean/marginal/poor band', () => {
    // The RD rejected a verdict chip outright: the ceiling collapses with pilot count, so any flat
    // band would call every six-pilot layout dirty. The sentence must state, never judge.
    for (const reading of [CLEAN, IMD6C, RACEBAND8]) {
      const message = imdLine(reading, BAND).toLowerCase();
      for (const verdict of ['marginal', 'poor', 'bad', 'good', 'warning', 'dirty', 'unusable']) {
        expect(message).not.toContain(verdict);
      }
    }
  });

  // ── The green/amber/red scale (#474) ───────────────────────────────────────────────────────
  //
  // The colour is judged against what is ACHIEVABLE at the layout's own pilot count, which is the
  // whole reason a flat band was refused in the first place. These pin the calibration to the
  // hobby's own reference sets, so a well-meant tweak to `IMD_BANDS` that quietly demotes MultiGP's
  // official six-pilot layout to amber turns this file red.
  const at = (rating: number): ImdReading => ({ rating });

  it('greens the layout the hobby recommends, at every pilot count it names one for', () => {
    // Racebnd4 = 100, ET6minus1 = 98, ETBest6 (MultiGP's official six) = 67 — the engine's own
    // validation table (`crates/engine/src/imd.rs::canonical_sets_rate_as_imdtabler_does`).
    expect(imdTone(at(100), 4)).toBe('success');
    expect(imdTone(at(98), 5)).toBe('success');
    expect(imdTone(at(67), 6)).toBe('success');
    // And the best eight-pilot set there is (−203 from the full catalog) — at eight pilots a deeply
    // negative number IS as good as it gets, and calling it red would be calling 5.8 GHz red.
    expect(imdTone(at(-203), 8)).toBe('success');
  });

  it('ambers RotorHazard’s own IMD6C at six pilots — workable, and beatable by 38', () => {
    // The single most important row to get right: IMD6C is the default an RD lands on by doing
    // nothing, and MultiGP's set really is meaningfully cleaner. Red would be wrong (it flies), and
    // green would be wrong (there is something to win here).
    expect(imdTone(at(29), 6)).toBe('warn');
  });

  it('reds a set that is worse than one wrong channel could explain', () => {
    // All eight of Raceband — the worst set in common use.
    expect(imdTone(at(-635), 8)).toBe('danger');
    // Alternating Raceband at four pilots (−145): every other channel, which sounds tidy and is
    // the classic mistake — 2×R3 − R1 lands exactly on R5.
    expect(imdTone(at(-145), 4)).toBe('danger');
  });

  it('is scaled by pilot count — the SAME rating reads differently at four and at eight', () => {
    // The point of the table. −203 is the ceiling at eight pilots and a disaster at four.
    expect(imdTone(at(-203), 4)).toBe('danger');
    expect(imdTone(at(-203), 8)).toBe('success');
    // 29 is amber at six and red at four, where 100 is reachable.
    expect(imdTone(at(29), 6)).toBe('warn');
    expect(imdTone(at(29), 4)).toBe('danger');
  });

  it('clamps at both ends of the table rather than inventing a row', () => {
    // Under two channels nothing can interfere with anything and the engine returns the ceiling.
    expect(imdTone(at(100), 1)).toBe('success');
    expect(imdTone(at(100), 0)).toBe('success');
    // Beyond eight the last row holds: nine simultaneous pilots on 5.8 GHz is off the edge of the
    // map, and a fabricated ninth row would be a guess wearing a constant's clothes.
    expect(imdTone(at(-203), 9)).toBe(imdTone(at(-203), 8));
    expect(imdTone(at(-635), 12)).toBe('danger');
  });

  it('keeps every band ordered and monotonically easier as pilots are added', () => {
    // Two structural invariants a field-tune must not break: green is never below amber (which
    // would make amber unreachable), and neither floor ever RISES with pilot count — the achievable
    // ceiling only falls, so demanding more of a bigger heat is backwards.
    let previous: { green: number; amber: number } | undefined;
    for (const band of IMD_BANDS) {
      expect(band.green).toBeGreaterThan(band.amber);
      if (previous) {
        expect(band.green).toBeLessThanOrEqual(previous.green);
        expect(band.amber).toBeLessThanOrEqual(previous.amber);
      }
      previous = band;
    }
    // One row per pilot count, ascending and with no gaps — the lookup indexes by equality.
    expect(IMD_BANDS.map((b) => b.pilots)).toEqual([2, 3, 4, 5, 6, 7, 8]);
  });

  it('frames the colour as guidance, names the pilot count, and never claims a refusal', () => {
    for (const [reading, pilots] of [
      [at(67), 6],
      [at(29), 6],
      [at(-635), 8]
    ] as const) {
      const hint = imdToneHint(reading, pilots);
      // Guidance, and it says so — the reading has never blocked anything and must not start
      // reading as though it does.
      expect(hint).toMatch(/guidance only/i);
      expect(hint).toMatch(/blocks saving/i);
      // Judged against a pilot count, because "is 29 good?" has no answer without one.
      expect(hint).toContain(`${pilots} channels flying at once`);
      // Still no verdict word on the layout itself.
      for (const verdict of ['broken', 'unusable', 'invalid', 'illegal', 'refuse']) {
        expect(hint.toLowerCase()).not.toContain(verdict);
      }
    }
  });

  it('counts a layout’s pilots as its distinct CHANNELS', () => {
    const layout: ChannelLayout = {
      id: 'bracket-a-k3f9',
      name: 'Bracket A',
      nodes: [
        { node: 0, channel: 5658 },
        { node: 1, channel: 5695 },
        { node: 2, channel: 5732 },
        { node: 3, channel: 5917 }
      ]
    };
    expect(layoutPilotCount(layout)).toBe(4);
  });

  it('splits the rating from the offender without changing the sentence', () => {
    // #474 colours the rating and leaves the offender sentence alone. The two halves still compose
    // into exactly the line the screen has always shown, which is what `imdLine` above asserts
    // against — and the offender half is the part that must be identical.
    expect(imdLine(RACEBAND8, BAND)).toBe(
      'IMD \u2212635 · worst offender: 2 × Raceband R2 − Raceband R1 = 5732 MHz — lands on Raceband R3'
    );
    expect(imdTail(IMD6C, BAND)).toMatch(/^worst offender: /);
    expect(imdTail(CLEAN, BAND)).not.toContain('worst offender');
  });

  it('resolves a rating by layout id, not by position', () => {
    const ratings = [
      { layout: 'pack-b-z1x8', imd: IMD6C },
      { layout: 'bracket-a-k3f9', imd: CLEAN }
    ];
    expect(layoutRating(ratings, 'bracket-a-k3f9')).toBe(CLEAN);
    expect(layoutRating(ratings, 'pack-b-z1x8')).toBe(IMD6C);
    expect(layoutRating(ratings, 'gone')).toBeUndefined();
    expect(layoutRating(undefined, 'bracket-a-k3f9')).toBeUndefined();
  });

  it('asks about a channel SET — ascending, de-duplicated, so one choice is asked once', () => {
    const d: LayerDraft = {
      name: 'Bracket A',
      channels: new Map([
        [3, 5769],
        [0, 5658],
        [1, 5695]
      ])
    };
    expect(draftChannelSet(d)).toEqual([5658, 5695, 5769]);
    // Two nodes briefly on one channel collapse — the blocker speaks to that, not the rating.
    const clash: LayerDraft = {
      name: 'x',
      channels: new Map([
        [0, 5658],
        [1, 5658]
      ])
    };
    expect(draftChannelSet(clash)).toEqual([5658]);
    expect(draftChannelSet({ name: 'x', channels: new Map() })).toEqual([]);
  });
});
