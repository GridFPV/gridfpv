/**
 * The shared competitor → display-name resolution (`competitorName.ts`).
 *
 * Every screen resolves a competitor ref through this one module. The cases it must cover (and
 * which the Marshaling raw-id bug, #214, exercised):
 *   1. an explicit `Register` binding → the bound pilot's callsign;
 *   2. the roster-seeded binding (ref IS the pilot id) → the directory callsign, no progress needed;
 *   3. an unbound open-practice `node-{i}` seat → its seat label (never "node-0");
 *   4. a bare human handle (a sim heat) → as-is.
 *
 * Plus the #416 half: **one place builds the inputs**, the seat label is node AND channel, and a
 * `node-{i}` ref can never reach the screen raw — not even when a caller supplies no channel data.
 */
import { describe, expect, it } from 'vitest';
import type {
  ChannelCatalogEntry,
  ChannelLayout,
  ClassMembership,
  HeatSummary,
  Pilot,
  PilotProgress,
  Timer,
  TimerSignal
} from '@gridfpv/types';
import { buildCompetitorNames, createCompetitorNameResolver } from '../src/lib/competitorName.js';

const pilot = (id: string, callsign: string): Pilot =>
  ({ id, callsign, vtx_types: [] }) as unknown as Pilot;

const CATALOG: ChannelCatalogEntry[] = [
  { band: 'Raceband', channel: 'R1', mhz: 5658 },
  { band: 'Raceband', channel: 'R7', mhz: 5880 },
  { band: 'Fatshark', channel: 'F4', mhz: 5800 }
];

/** A Flexible RotorHazard timer as the bench actually reports one: four nodes, NO channel pool. */
const rhTimer = (over: Partial<Timer> = {}): Timer =>
  ({
    id: 'rh',
    name: 'Docker RH',
    node_count: 4,
    disabled_nodes: [],
    available_channels: [],
    ...over
  }) as unknown as Timer;

const heat = (over: Partial<HeatSummary> = {}): HeatSummary =>
  ({
    heat: 'h-1',
    lineup: [],
    frequencies: [],
    phase: 'Scheduled',
    is_current: false,
    ...over
  }) as unknown as HeatSummary;

const progressRow = (competitor: string, pilotId: string | null): PilotProgress =>
  ({ competitor, pilot: pilotId, laps_completed: 0 }) as unknown as PilotProgress;

describe('createCompetitorNameResolver', () => {
  it('resolves an EXPLICIT registration binding to the bound pilot callsign', () => {
    const resolve = createCompetitorNameResolver({
      pilotById: new Map([['pilot-1', pilot('pilot-1', 'Maverick')]]),
      explicitPilotByRef: new Map([['node-0', 'pilot-1']])
    });
    expect(resolve('node-0')).toBe('Maverick');
  });

  it('resolves the ROSTER-SEEDED case where the ref IS the pilot id (no progress binding)', () => {
    const resolve = createCompetitorNameResolver({
      pilotById: new Map([['goose-yla6dp', pilot('goose-yla6dp', 'Goose')]]),
      explicitPilotByRef: new Map()
    });
    // The ref equals the pilot id (the FromRoster seeding) — resolves to the callsign directly.
    expect(resolve('goose-yla6dp')).toBe('Goose');
  });

  it('falls back to the SEAT LABEL for an unbound node-{i} seat', () => {
    const resolve = createCompetitorNameResolver({
      pilotById: new Map(),
      explicitPilotByRef: new Map(),
      seatLabelByRef: new Map([['node-0', 'Node 1 · Raceband R1']])
    });
    expect(resolve('node-0')).toBe('Node 1 · Raceband R1');
  });

  it('returns the bare ref for a human handle with no binding (a sim heat)', () => {
    const resolve = createCompetitorNameResolver({
      pilotById: new Map(),
      explicitPilotByRef: new Map()
    });
    expect(resolve('ALICE')).toBe('ALICE');
  });

  it('#416: a node seat with NO seat-label map still resolves to the node, never the raw ref', () => {
    // The guarantee, not belt-and-braces: a caller that assembled no channel data at all — which is
    // exactly what `EventRounds.svelte` used to do — must still not print `node-2`.
    const resolve = createCompetitorNameResolver({
      pilotById: new Map(),
      explicitPilotByRef: new Map()
    });
    expect(resolve('node-2')).toBe('Node 3');
    expect(resolve('node-2')).not.toContain('node-2');
  });

  it('the explicit binding wins over a same-named directory id', () => {
    const resolve = createCompetitorNameResolver({
      pilotById: new Map([
        ['pilot-1', pilot('pilot-1', 'Maverick')],
        ['node-0', pilot('node-0', 'WRONG')]
      ]),
      explicitPilotByRef: new Map([['node-0', 'pilot-1']])
    });
    expect(resolve('node-0')).toBe('Maverick');
  });
});

describe('buildCompetitorNames — the ONE input assembly (#416)', () => {
  it('renders an open-practice seat as node + channel', () => {
    // The RD's ask: "node count plus the channel should go there for practice."
    const names = buildCompetitorNames({
      catalog: CATALOG,
      signal: {
        nodes: [{ node: 6, seat: 'node-6', frequency_mhz: 5880 }]
      } as unknown as TimerSignal
    });
    expect(names.name('node-6')).toBe('Node 7 · Raceband R7');
    expect(names.channelFor('node-6')).toBe('Raceband R7');
    expect(names.seatLabel(6)).toBe('Node 7 · Raceband R7');
  });

  it('resolves a channel WITHOUT available_channels, from what the node reports it is tuned to', () => {
    // Every Flexible RotorHazard timer reports `available_channels: []`. The seat's channel comes
    // from `NodeSignal.frequency_mhz` instead — the hardware's own answer.
    const names = buildCompetitorNames({
      catalog: CATALOG,
      timer: rhTimer(),
      signal: {
        nodes: [{ node: 0, seat: 'node-0', frequency_mhz: 5658 }]
      } as unknown as TimerSignal
    });
    expect(names.name('node-0')).toBe('Node 1 · Raceband R1');
  });

  it('never reads an EMPTY channel pool as "this node has no channel"', () => {
    const names = buildCompetitorNames({ catalog: CATALOG, timer: rhTimer() });
    // Unknown, so the seat is the node alone — and emphatically not a claim about the channel.
    expect(names.name('node-2')).toBe('Node 3');
    expect(names.channelFor('node-2')).toBeUndefined();
  });

  it('prefers the HEAT’s own assignment over every other source', () => {
    // The heat's `frequencies` are what the round actually allocated for THIS heat — the only
    // per-heat source there is, and the one a raced heat keeps forever.
    const names = buildCompetitorNames({
      catalog: CATALOG,
      timer: rhTimer({ available_channels: [5658, 5800, 5880, 5658] }),
      layouts: [
        {
          id: 'bracket-a',
          name: 'Bracket A',
          nodes: [{ node: 0, channel: 5658 }]
        } as unknown as ChannelLayout
      ],
      heat: heat({ lineup: ['node-0'], frequencies: [['node-0', 5880]], layout: 'bracket-a' })
    });
    expect(names.name('node-0')).toBe('Node 1 · Raceband R7');
  });

  // ── Source (3): the heat's channel layout (#117 S3) ──────────────────────────────────────
  //
  // Source (3) used to be `Timer.available_channels[node]` — indexing the timer's ALLOWED SET by
  // node index. An allowed set says which channels the timer may ever use and carries no per-node
  // mapping at all, so the answer was invented. A `ChannelLayout` is the mapping it never was.

  /** A layout putting node 0 on R1 and node 2 on F4, and saying nothing about node 1. */
  const LAYOUT: ChannelLayout = {
    id: 'bracket-a',
    name: 'Bracket A',
    nodes: [
      { node: 0, channel: 5658 },
      { node: 2, channel: 5800 }
    ]
  } as unknown as ChannelLayout;

  it('resolves a seat through the layout the HEAT flies', () => {
    const names = buildCompetitorNames({
      catalog: CATALOG,
      timer: rhTimer(),
      layouts: [LAYOUT],
      heat: heat({ lineup: ['node-0', 'node-2'], layout: 'bracket-a' })
    });
    expect(names.name('node-0')).toBe('Node 1 · Raceband R1');
    expect(names.name('node-2')).toBe('Node 3 · Fatshark F4');
    // A node the layout says nothing about is UNKNOWN, not "no channel" — and never a guess.
    expect(names.name('node-1')).toBe('Node 2');
    expect(names.channelFor('node-1')).toBeUndefined();
  });

  it('will not resolve a layout the heat does not fly', () => {
    // The event has the layout; this heat is not bound to it. Reaching for it anyway would be the
    // same class of invention the allowed-set index was.
    const names = buildCompetitorNames({
      catalog: CATALOG,
      timer: rhTimer(),
      layouts: [LAYOUT],
      heat: heat({ lineup: ['node-0'] })
    });
    expect(names.name('node-0')).toBe('Node 1');
  });

  it('takes an explicit layout when there is no heat to ask (#402)', () => {
    // The open-practice round form: the RD is choosing which nodes practice runs on, before any
    // heat exists. #402's sharpest complaint was that this picker is channel-blind at exactly that
    // moment, so the round's own layout answers it.
    const names = buildCompetitorNames({
      catalog: CATALOG,
      timer: rhTimer(),
      layouts: [LAYOUT],
      layout: 'bracket-a'
    });
    expect(names.seatLabel(0)).toBe('Node 1 · Raceband R1');
    expect(names.seatLabel(2)).toBe('Node 3 · Fatshark F4');
  });

  it('lets what a node REPORTS win over the layout', () => {
    // The layout is Grid's intent; the heartbeat is the hardware's own answer about right now. On a
    // page watching a live gate, the observation is what the RD needs to see (D27: read a timer as
    // evidence, never adopt it as config — and this is a display, not a decision).
    const names = buildCompetitorNames({
      catalog: CATALOG,
      timer: rhTimer(),
      layouts: [LAYOUT],
      heat: heat({ lineup: ['node-0'], layout: 'bracket-a' }),
      signal: {
        nodes: [{ node: 0, seat: 'node-0', frequency_mhz: 5880 }]
      } as unknown as TimerSignal
    });
    expect(names.name('node-0')).toBe('Node 1 · Raceband R7');
  });

  it('never resolves a channel from available_channels, however it is shaped', () => {
    // The regression guard for the fabrication itself: a fully-populated allowed set, no layout and
    // no signal, resolves NOTHING. Positional agreement with the node index is a coincidence, and
    // the resolver no longer trades on it.
    const names = buildCompetitorNames({
      catalog: CATALOG,
      timer: rhTimer({ available_channels: [5658, 5880, 5800, 5658] }),
      heat: heat({ lineup: ['node-0', 'node-1', 'node-2'] })
    });
    expect(names.channelFor('node-0')).toBeUndefined();
    expect(names.channelFor('node-1')).toBeUndefined();
    expect(names.name('node-2')).toBe('Node 3');
  });

  it('a bound pilot still wins over the seat label', () => {
    const names = buildCompetitorNames({
      pilots: [pilot('mav', 'Maverick')],
      progress: [progressRow('node-0', 'mav')],
      catalog: CATALOG,
      timer: rhTimer({ available_channels: [5658] })
    });
    expect(names.name('node-0')).toBe('Maverick');
  });

  it('falls back to a pilot’s class-membership channel', () => {
    const membership: ClassMembership[] = [
      { class: 'open', pilots: [{ pilot: 'reconfpv', channel: 5880 }] }
    ] as unknown as ClassMembership[];
    const names = buildCompetitorNames({ catalog: CATALOG, membership });
    expect(names.channelFor('reconfpv')).toBe('Raceband R7');
  });

  it('#416: the two screens cannot disagree — same sources, same answer', () => {
    // Live control's sources and the Rounds & Heats stage's sources, for the SAME seat. The bug was
    // that the two assembled different inputs and answered `node-6` against `Node 7`.
    const shared = {
      catalog: CATALOG,
      timer: rhTimer(),
      signal: {
        nodes: [{ node: 6, seat: 'node-6', frequency_mhz: 5880 }]
      } as unknown as TimerSignal
    };
    const liveControl = buildCompetitorNames({
      ...shared,
      progress: [progressRow('node-6', null)]
    });
    const roundsStage = buildCompetitorNames({ ...shared, heat: heat({ lineup: ['node-6'] }) });
    expect(liveControl.name('node-6')).toBe(roundsStage.name('node-6'));
    expect(liveControl.name('node-6')).toBe('Node 7 · Raceband R7');
  });

  it('never lets a raw node-{i} ref reach the screen, from any source combination', () => {
    const cases = [
      buildCompetitorNames({}),
      buildCompetitorNames({ catalog: CATALOG }),
      buildCompetitorNames({ timer: rhTimer() }),
      buildCompetitorNames({ heat: heat({ lineup: ['node-6'] }), catalog: CATALOG }),
      buildCompetitorNames({ pilots: [pilot('x', 'X')], catalog: CATALOG, timer: rhTimer() })
    ];
    for (const names of cases) {
      expect(names.name('node-6')).not.toMatch(/^node-\d+$/);
      expect(names.name('node-6')).toContain('Node 7');
    }
  });
});
