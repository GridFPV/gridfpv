/**
 * Node-configuration readings (#412) — the pure half.
 *
 * These are the sentences an RD reads while deciding whether a heat is about to lose four pilots,
 * so they are asserted as text rather than as shapes. The 0-based/1-based boundary is asserted in
 * both directions: a wire index of `2` must never reach the screen, and the label the RD sees for
 * it must be "Node 3".
 */
import { describe, expect, it } from 'vitest';
import type { Timer, TimerNode, TimerNodes } from '@gridfpv/types';
import {
  DEFAULT_NODE_COUNT,
  driftReading,
  followTimerRequest,
  heatOverflow,
  heatOverflowMessage,
  enabledNodes,
  joinLabels,
  nodeLabel,
  nodeLabels,
  seatNodes,
  seatSummary,
  timerDrifts,
  timerNodeSummary,
  timerSeats,
  timerWidth
} from '../src/lib/timerNodes.js';

/** One node, wired the way the Director sends it: 0-based index, 1-based label. */
function node(index: number, opts: { enabled?: boolean; reported?: boolean } = {}): TimerNode {
  return {
    node: index,
    label: `Node ${index + 1}`,
    seat: `node-${index}`,
    enabled: opts.enabled ?? true,
    reported: opts.reported ?? true
  };
}

/** The bench bug: a real 4-node timer configured as 8, all eight seats enabled. */
function benchBug(): TimerNodes {
  const nodes = [0, 1, 2, 3, 4, 5, 6, 7].map((i) => node(i, { reported: i < 4 }));
  return {
    timer: 'rh-1',
    reported: 4,
    configured: 8,
    width: 8,
    nodes,
    enabled: [0, 1, 2, 3, 4, 5, 6, 7],
    drift: { reported: 4, configured: 8, enabled_beyond_reported: [4, 5, 6, 7] }
  };
}

describe('node labels (1-based on screen, 0-based on the wire)', () => {
  it('resolves a wire index to the Director’s 1-based label', () => {
    const view = benchBug();
    expect(nodeLabel(view, 0)).toBe('Node 1');
    expect(nodeLabel(view, 2)).toBe('Node 3');
    expect(nodeLabels(view, [0, 3])).toEqual(['Node 1', 'Node 4']);
  });

  it('falls back to the 1-based name for an index the view does not carry', () => {
    const view = { ...benchBug(), nodes: [] };
    expect(nodeLabel(view, 4)).toBe('Node 5');
  });

  it('joins labels readably', () => {
    expect(joinLabels([])).toBe('');
    expect(joinLabels(['Node 5'])).toBe('Node 5');
    expect(joinLabels(['Node 5', 'Node 6'])).toBe('Node 5 and Node 6');
    expect(joinLabels(['Node 5', 'Node 6', 'Node 7'])).toBe('Node 5, Node 6 and Node 7');
  });
});

describe('driftReading', () => {
  it('is quiet when reported and configured agree', () => {
    const view: TimerNodes = {
      timer: 'rh-1',
      reported: 4,
      configured: undefined,
      width: 4,
      nodes: [0, 1, 2, 3].map((i) => node(i)),
      enabled: [0, 1, 2, 3],
      drift: undefined
    };
    expect(driftReading(view)).toBeUndefined();
  });

  it('names the phantom nodes by their 1-based labels, and says what they cost', () => {
    const reading = driftReading(benchBug());
    expect(reading?.tone).toBe('danger');
    expect(reading?.headline).toBe('This timer reports 4 nodes; GridFPV is configured for 8.');
    expect(reading?.phantomLabels).toEqual(['Node 5', 'Node 6', 'Node 7', 'Node 8']);
    expect(reading?.detail).toContain('Node 5, Node 6, Node 7 and Node 8');
    expect(reading?.detail).toContain('record nothing');
    // Never the raw index or the seat ref (the repo display rule).
    expect(reading?.detail).not.toMatch(/node-\d/);
    expect(reading?.detail).not.toMatch(/Node 0\b/);
  });

  it('reads a single phantom node in the singular', () => {
    const view = benchBug();
    view.drift = { reported: 4, configured: 5, enabled_beyond_reported: [4] };
    const reading = driftReading(view);
    expect(reading?.detail).toContain('Node 5 is enabled but does not exist');
    expect(reading?.detail).toContain('A pilot seated there');
  });

  it('is informational when every extra node is already disabled', () => {
    const view = benchBug();
    view.drift = { reported: 4, configured: 8, enabled_beyond_reported: [] };
    const reading = driftReading(view);
    expect(reading?.tone).toBe('info');
    expect(reading?.detail).toContain('already disabled');
  });

  it('is informational when the timer has MORE nodes than GridFPV uses', () => {
    const view = benchBug();
    view.drift = { reported: 8, configured: 4, enabled_beyond_reported: [] };
    const reading = driftReading(view);
    expect(reading?.tone).toBe('info');
    expect(reading?.headline).toBe('This timer reports 8 nodes; GridFPV is configured for 4.');
    expect(reading?.detail).toContain('spare capacity');
  });
});

describe('seat readings', () => {
  it('counts seats from the ENABLED set, not the width', () => {
    const view = benchBug();
    // "reported is 4 but node 3 is busted" — nodes 1, 2 and 4 (wire 0, 1, 3).
    const fixed: TimerNodes = { ...view, width: 4, enabled: [0, 1, 3] };
    expect(seatSummary(fixed)).toBe('3 pilots per heat (1 of 4 nodes disabled)');
  });

  it('drops the disabled clause when every node is enabled', () => {
    const view: TimerNodes = { ...benchBug(), width: 4, enabled: [0, 1, 2, 3] };
    expect(seatSummary(view)).toBe('4 pilots per heat');
  });
});

describe('followTimerRequest', () => {
  it('clears the override with an explicit null (not an omitted field)', () => {
    const req = followTimerRequest();
    expect(req).toEqual({ node_count: null });
    // The three-valued field must serialise as a real null: absent would leave the override alone.
    expect(JSON.stringify(req)).toBe('{"node_count":null}');
  });
});

describe('heatOverflow', () => {
  const view: TimerNodes = { ...benchBug(), width: 4, enabled: [0, 1, 3] };

  it('is quiet when every heat fits the enabled set', () => {
    expect(
      heatOverflow(view, [{ lineup: ['a', 'b'] }, { lineup: ['a', 'b', 'c'] }])
    ).toBeUndefined();
  });

  it('flags heats that seat more pilots than there are enabled nodes', () => {
    const over = heatOverflow(view, [
      { lineup: ['a', 'b', 'c'] },
      { lineup: ['a', 'b', 'c', 'd'] },
      { lineup: ['a', 'b', 'c', 'd', 'e'] }
    ]);
    expect(over).toEqual({ seats: 3, largest: 5, heats: 2 });
    expect(heatOverflowMessage(over!)).toBe(
      '2 scheduled heats are built for more pilots than this timer can time: the largest seats 5, ' +
        'but only 3 nodes are enabled. 2 pilots in that heat would record nothing.'
    );
  });
});

describe('the timer-row reading', () => {
  const base: Timer = {
    id: 'rh-1',
    name: 'Track RH',
    kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
    status: 'Connected',
    channel_capability: 'Flexible',
    available_channels: [],
    manual_connect: false,
    calibration: [],
    disabled_nodes: []
  };

  it('resolves the width override → reported → fallback, exactly as the Director does', () => {
    expect(timerWidth({ ...base, node_count: 6, reported_nodes: 4 })).toBe(6);
    expect(timerWidth({ ...base, reported_nodes: 4 })).toBe(4);
    expect(timerWidth(base)).toBe(DEFAULT_NODE_COUNT);
  });

  it('counts seats as the width minus the disabled nodes below it', () => {
    expect(timerSeats({ ...base, reported_nodes: 4, disabled_nodes: [2] })).toBe(3);
    // A disabled index at or beyond the width is inert — it is kept, but it is not a lost seat.
    expect(timerSeats({ ...base, reported_nodes: 4, disabled_nodes: [7] })).toBe(4);
  });

  it('summarises seats vs width without printing any node index', () => {
    expect(timerNodeSummary({ ...base, reported_nodes: 4 })).toBe('4 nodes');
    expect(timerNodeSummary({ ...base, reported_nodes: 4, disabled_nodes: [2] })).toBe(
      '3 of 4 nodes'
    );
  });

  it('flags a row whose reported width disagrees with the width GridFPV uses', () => {
    expect(timerDrifts({ ...base, node_count: 8, reported_nodes: 4 })).toBe(true);
    expect(timerDrifts({ ...base, node_count: 4, reported_nodes: 4 })).toBe(false);
    // Never asked: nothing to disagree with.
    expect(timerDrifts({ ...base, node_count: 8 })).toBe(false);
  });

  // #445 — the same "never asked" case, spelled the way the WIRE actually spells it.
  //
  // The assertion above builds it by OMITTING the key, which is the one shape the Director never
  // sends: `reported_nodes` is `Option<u32>` with `#[serde(default)]` and **no**
  // `skip_serializing_if` (`crates/server/src/timers.rs:469-471`), so a Mock — or any timer the
  // Director has never dialed — serialises `"reported_nodes": null`. `timerDrifts` tests
  // `!== undefined`, and `null !== undefined`, so every one of those rows flags drift and
  // TimerManager renders a danger badge reading "Timer reports " with nothing after it (`null`
  // renders empty). `hasWidthOverride()` immediately above already guards both `undefined` and
  // `null`; this is that same guard, missing.
  //
  // `as unknown as Timer` for the same reason the #412 fixtures below use it: `#[ts(optional)]`
  // types the field as `reported_nodes?: number`, which cannot express the `null` the wire sends —
  // and a fixture that can only express shapes the wire does not send is how this shipped.
  //
  // `it.fails` rather than a skip: it runs in CI, passes while the bug stands, and goes red the
  // moment the guard becomes `!= null`, which forces the marker off with the fix.
  it.fails('does not flag drift on a timer the Director has never asked (wire: null)', () => {
    // A Mock: width pinned by the RD, nothing to ask.
    const mock = { ...base, node_count: 8, reported_nodes: null } as unknown as Timer;
    expect(timerDrifts(mock)).toBe(false);
    // A never-connected RotorHazard: nothing pinned and nothing observed either.
    const neverDialed = { ...base, node_count: null, reported_nodes: null } as unknown as Timer;
    expect(timerDrifts(neverDialed)).toBe(false);
  });
});

describe('the effective width is what consumers must read (#412 regression)', () => {
  // `node_count` changed from "the width" to "the RD's OVERRIDE" and is normally null. Every
  // consumer that read `timer.node_count ?? <fallback>` kept type-checking and silently changed
  // meaning: `?? 0` became "this timer has no nodes", which emptied the Tune page, the practice
  // channel picker and the seat-label seed on every timer that had never been pinned.
  const unpinned = (over: Partial<Timer> = {}): Timer =>
    ({
      id: 't',
      name: 'Field RH',
      kind: { Rotorhazard: { url: 'http://x' } },
      status: 'Connected',
      channel_capability: 'Flexible',
      node_count: null,
      reported_nodes: 4,
      disabled_nodes: [],
      available_channels: [],
      manual_connect: false,
      calibration: [],
      ...over
    }) as unknown as Timer;

  it('falls back to what the timer reported when the RD has pinned nothing', () => {
    expect(timerWidth(unpinned())).toBe(4);
    expect(timerSeats(unpinned())).toBe(4);
  });

  it('never reports zero for a connected timer that reported nodes', () => {
    // The exact shape of the bug: `node_count ?? 0` === 0 here, and 0 reads as "nothing to tune".
    expect(unpinned().node_count ?? 0).toBe(0);
    expect(timerWidth(unpinned())).toBeGreaterThan(0);
  });

  it('still honours an explicit override, and disabled nodes still cut seats', () => {
    expect(timerWidth(unpinned({ node_count: 8 }))).toBe(8);
    expect(timerSeats(unpinned({ disabled_nodes: [2] }))).toBe(3);
  });
});

/**
 * Which gate each lineup entry flies — the console-side mirror of the Director's `Timer::seat_nodes`.
 *
 * The seating editor shows the RD a gate per seat, so this and the Director must not be able to
 * disagree: a mismatch here puts a pilot's name against a gate they are not on.
 */
describe('seatNodes / enabledNodes — laying a lineup onto real gates', () => {
  const timer = (over: Partial<Timer>): Timer =>
    ({
      id: 'mock',
      name: 'Mock',
      kind: { Mock: { laps: 3, lap_ms: 1000 } },
      status: 'Ready',
      channel_capability: 'Flexible',
      available_channels: [],
      manual_connect: false,
      calibration: [],
      disabled_nodes: [],
      ...over
    }) as unknown as Timer;

  it('never renumbers around a disabled gate', () => {
    // The whole point of #412: with node 2 off, a 3-pilot heat is on 0, 1 and 3 — not 0, 1, 2.
    expect(enabledNodes(timer({ node_count: 4, disabled_nodes: [2] }))).toEqual([0, 1, 3]);
    expect(seatNodes([0, 1, 3], ['a', 'b', 'c']).map((s) => s.node)).toEqual([0, 1, 3]);
  });

  it('lets a node-{i} seat keep the gate it names, and routes pilots around it', () => {
    // The explicit handle names its own gate; the pilots take what is left, in order.
    expect(seatNodes([0, 1, 2, 3], ['a', 'node-0', 'b'])).toEqual([
      { node: 1, ref: 'a' },
      { node: 0, ref: 'node-0' },
      { node: 2, ref: 'b' }
    ]);
  });

  it('drops what it cannot place rather than squeezing it onto the wrong gate', () => {
    // A `node-{i}` naming a gate that is off or gone is not flown — seating nobody records nothing,
    // whereas squeezing would record the WRONG pilot.
    expect(seatNodes([0, 1, 3], ['node-2'])).toEqual([]);
    // And a pilot beyond the enabled set is dropped, not wrapped around.
    expect(seatNodes([0, 1], ['a', 'b', 'c']).map((s) => s.ref)).toEqual(['a', 'b']);
  });

  it('seats a lineup made only of gates, which is the practice case', () => {
    expect(seatNodes([0, 1, 3], ['node-0', 'node-3'])).toEqual([
      { node: 0, ref: 'node-0' },
      { node: 3, ref: 'node-3' }
    ]);
  });
});
