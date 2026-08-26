/**
 * Unit tests for the pure half of the Tune page (#355, slice 2b) — `src/lib/tuning.ts`.
 *
 * The component test (`TunePage.test.ts`) proves the three editors stay locked to one value through
 * the DOM; this pins the rules that guarantee they *can*: the single clamp, the write gate, the
 * poll-driven confirmation, and the label that keeps a raw seat/frequency off the screen.
 *
 * ## The fixtures are built from the GENERATED types, on purpose
 *
 * `TimerSignal` / `NodeSignal` are imported from `@gridfpv/types` and the builders below are typed
 * as them with no casts, so every field name here is checked against the ts-rs bindings generated
 * from `crates/server/src/timers.rs`. That is the whole point: this module previously tested
 * against a hand-declared guess at the wire shape in which *every* field name was wrong, and the
 * suite passed green on a page that would have rendered `undefined` at every readout. A fixture
 * that can drift from the wire is worse than no fixture, because it manufactures confidence.
 */
import { describe, expect, it } from 'vitest';
import type {
  ChannelCapability,
  ChannelCatalogEntry,
  NodeSignal,
  TimerNodes,
  TimerSignal
} from '@gridfpv/types';
import {
  CONFIRM_TIMEOUT_MS,
  RSSI_MAX,
  RSSI_MIN,
  SIGNAL_LEASE_MS,
  SIGNAL_POLL_MS,
  adoptReported,
  channelGate,
  channelOptions,
  clampLevel,
  duplicateChannelNodes,
  duplicateChannelNote,
  foldPolled,
  foldPolledChannel,
  holdsLease,
  isParsableLevel,
  markChannelSent,
  markSent,
  nodeCountOf,
  nodeTraceOf,
  nodeTuneLabel,
  offerableNodes,
  phaseLabel,
  phaseTone,
  plottable,
  readoutsOf,
  seedChannel,
  seedThreshold,
  staleThresholdNote,
  writeGate,
  type ThresholdState
} from '../src/lib/tuning.js';

const CATALOG: ChannelCatalogEntry[] = [
  { band: 'Raceband', channel: 'R1', mhz: 5658 },
  { band: 'Raceband', channel: 'R7', mhz: 5880 },
  { band: 'Fatshark', channel: 'F4', mhz: 5800 }
];

/**
 * One node, defaulting to a node the timer HAS reported (`seen`). The three non-optional fields —
 * `node`, `seat`, `seen`, `crossing`, `crossed_recently`, `samples` — are spelled out because the
 * wire always carries them; everything else is `Option` on the Rust side and absent here unless a
 * test is about it.
 */
function node(over: Partial<NodeSignal> = {}): NodeSignal {
  return {
    node: 0,
    seat: 'node-0',
    seen: true,
    crossing: false,
    crossed_recently: false,
    samples: [],
    ...over
  };
}

/** One poll of `GET /timers/{id}/signal`: a live lease, a shared time base, and some nodes. */
function snapshot(over: Partial<TimerSignal> = {}): TimerSignal {
  const nodes = over.nodes ?? [node()];
  return {
    timer: 'rh-1',
    streaming: true,
    lease_ms_remaining: SIGNAL_LEASE_MS,
    period_micros: 200_000,
    // The time base is SHARED across every node — one axis, not one per node.
    sample_micros: nodes[0]?.samples.map((_, i) => i * 200_000) ?? [],
    ...over,
    nodes
  };
}

describe('clampLevel — the ONE clamp, at the state', () => {
  it('rounds to an integer (RSSI is an ADC count, not a fraction)', () => {
    // The whole reason this lives in one place: if each control rounded for itself the box could
    // hold 90.4 while the slider sat at 90 and the graph drew a third position.
    expect(clampLevel(90.4)).toBe(90);
    expect(clampLevel(90.5)).toBe(91);
    expect(clampLevel('90.6')).toBe(91);
  });

  it('clamps the minimum to 1, never 0', () => {
    // RotorHazard's calibration.py tests `enter_at_level` for truthiness, so a 0 is read as "go
    // read the level off the node" — a typed 0 would look accepted and silently no-op.
    expect(clampLevel(0)).toBe(RSSI_MIN);
    expect(clampLevel('0')).toBe(1);
    expect(clampLevel(-40)).toBe(1);
  });

  it('clamps the maximum to 254, NOT the 8-bit 255', () => {
    // `Node.is_valid_rssi` is `value > 0 and value < 255` — a STRICT `<`, verified in RH v4.3.0 and
    // v4.4.0. A literal 255 writes the profile row, is silently dropped before the detector, and
    // then comes back CONFIRMED, because RH broadcasts the profile rather than the node. A
    // threshold reading "On timer" that is not on the timer is the worst thing this page can do.
    expect(RSSI_MAX).toBe(254);
    expect(clampLevel(999)).toBe(254);
    expect(clampLevel(255)).toBe(254);
    expect(clampLevel(254)).toBe(254);
  });

  it('clamps both ends away, because both look accepted and neither is', () => {
    // The two failures are the same failure: RH takes the value, does nothing with it, and reports
    // success. 0 is falsy so `calibration.py` re-reads off the node; 255 fails `is_valid_rssi`.
    expect(clampLevel(0)).toBe(RSSI_MIN);
    expect(clampLevel(255)).toBe(RSSI_MAX);
  });

  it('resolves un-parseable input to the fallback rather than NaN', () => {
    // A NaN in the state would poison all three views of it at once.
    expect(clampLevel('')).toBe(RSSI_MIN);
    expect(clampLevel('abc', 88)).toBe(88);
    expect(clampLevel(undefined, 42)).toBe(42);
    expect(clampLevel(NaN, 42)).toBe(42);
  });

  it('is idempotent — clamping a clamped value changes nothing', () => {
    for (const raw of [-5, 0, 0.4, 90.5, 254.6, 1e6]) {
      expect(clampLevel(clampLevel(raw))).toBe(clampLevel(raw));
    }
  });
});

describe('isParsableLevel', () => {
  it('rejects the half-typed / emptied box so the state is left alone', () => {
    expect(isParsableLevel('')).toBe(false);
    expect(isParsableLevel('   ')).toBe(false);
    expect(isParsableLevel('-')).toBe(false);
    expect(isParsableLevel('abc')).toBe(false);
  });

  it('accepts anything numeric', () => {
    expect(isParsableLevel('0')).toBe(true);
    expect(isParsableLevel('90')).toBe(true);
    expect(isParsableLevel(' 90.5 ')).toBe(true);
  });
});

describe('writeGate — practice only, checked per write', () => {
  it('allows a write with no heat on the timer (the ordinary case)', () => {
    expect(writeGate(undefined, undefined).allowed).toBe(true);
    expect(writeGate('Scheduled', undefined).allowed).toBe(true);
  });

  it('allows a write while an OPEN PRACTICE heat is running', () => {
    // Practice is excluded from scoring (#398) — there is no result to corrupt, and pilots in the
    // air is the natural moment to tune.
    expect(writeGate('Running', 'practice').allowed).toBe(true);
  });

  it('refuses a write while a COMPETITION heat is running, and says why', () => {
    const gate = writeGate('Running', 'competition');
    expect(gate.allowed).toBe(false);
    expect(gate.allowed === false && gate.reason).toMatch(/competition heat is running/i);
  });

  it('allows a write in every non-Running phase, even a competition heat', () => {
    // Staged/Armed are pre-race and Unofficial/Final are past it: in none of those is the detector
    // deciding laps that count right now.
    for (const phase of ['Scheduled', 'Staged', 'Armed', 'Unofficial', 'Final'] as const) {
      expect(writeGate(phase, 'competition').allowed).toBe(true);
    }
  });
});

describe('seedThreshold / adoptReported', () => {
  it('seeds at rest from the level the timer reports', () => {
    expect(seedThreshold(90)).toEqual({ value: 90, confirmed: 90, phase: 'confirmed' });
  });

  it('clamps a bad reported level like any other input', () => {
    expect(seedThreshold(0)).toEqual({ value: 1, confirmed: 1, phase: 'confirmed' });
  });

  it('follows the hardware when the threshold is at rest (the RD tuned in RH’s own UI)', () => {
    const atRest: ThresholdState = { value: 90, confirmed: 90, phase: 'confirmed' };
    expect(adoptReported(atRest, 104)).toEqual({ value: 104, confirmed: 104, phase: 'confirmed' });
  });

  it('leaves a threshold the RD is adjusting alone', () => {
    // The next poll must never yank a value out from under a drag, an in-flight write, or a
    // failure the RD has not read yet.
    for (const phase of ['pending', 'sent', 'mismatch', 'failed', 'refused'] as const) {
      const busy: ThresholdState = { value: 120, confirmed: 90, phase };
      expect(adoptReported(busy, 104)).toBe(busy);
    }
  });

  it('is a no-op when the hardware already agrees', () => {
    const state: ThresholdState = { value: 90, confirmed: 90, phase: 'confirmed' };
    expect(adoptReported(state, 90)).toBe(state);
  });

  it('is a no-op for a level that only differs as a FLOAT — the wire carries f32', () => {
    // Without the clamp-before-compare this churns the state on every single poll, four times a
    // second, replacing the object the UI is keyed on for no change at all.
    const state: ThresholdState = { value: 90, confirmed: 90, phase: 'confirmed' };
    expect(adoptReported(state, 90.0)).toBe(state);
    expect(adoptReported(state, 89.6)).toBe(state);
  });
});

describe('markSent / foldPolled — the confirmation is a POLL, not a response', () => {
  const T0 = 1_000_000;
  const sending = (over: Partial<ThresholdState> = {}): ThresholdState => ({
    value: 104,
    confirmed: 90,
    phase: 'sent',
    sent: 104,
    sentAt: T0,
    ...over
  });

  it('records what was asked for and when, which is all the confirmation needs', () => {
    const state = markSent({ value: 104, confirmed: 90, phase: 'pending' }, 104, T0);
    expect(state).toMatchObject({ value: 104, phase: 'sent', sent: 104, sentAt: T0 });
  });

  it('confirms once a POLL shows the timer holding the level', () => {
    // `POST /calibration` only says "accepted". RotorHazard broadcasts `enter_and_exit_at_levels`,
    // which arrives as `NodeSignal.enter_at` on a LATER `GET /signal` — so this, and nothing about
    // the response body, is the evidence the write landed.
    expect(foldPolled(sending(), 104, T0 + 500)).toEqual({
      value: 104,
      confirmed: 104,
      phase: 'confirmed'
    });
  });

  it('stays Sending… while the polls still disagree but the timeout has not run out', () => {
    // A mismatch declared one poll too early is a false alarm: the change may simply still be in
    // flight through the Director and RH's broadcast.
    const state = sending();
    expect(foldPolled(state, 90, T0 + CONFIRM_TIMEOUT_MS - 1)).toBe(state);
  });

  it('flags a MISMATCH once the polls have kept disagreeing, and names both levels', () => {
    const folded = foldPolled(sending(), 90, T0 + CONFIRM_TIMEOUT_MS);
    expect(folded.phase).toBe('mismatch');
    expect(folded.confirmed).toBe(90);
    expect(folded.detail).toContain('90');
    expect(folded.detail).toContain('104');
  });

  it('flags a mismatch when the timer reports no such threshold at all', () => {
    // `enter_at` is `Option`: a node that has dropped its thresholds reports nothing, which is a
    // failed write just as surely as a wrong number — and must not read as "still sending".
    const folded = foldPolled(sending(), undefined, T0 + CONFIRM_TIMEOUT_MS);
    expect(folded.phase).toBe('mismatch');
    expect(folded.confirmed).toBeUndefined();
  });

  it('matches a level the wire rounds — the thresholds cross as f32, the domain is integers', () => {
    expect(foldPolled(sending(), 104.0, T0 + 10).phase).toBe('confirmed');
    expect(foldPolled(sending(), 103.7, T0 + 10).phase).toBe('confirmed');
  });

  it('keeps no undo value — the number is on screen and re-draggable', () => {
    const folded = foldPolled(sending(), 104, T0 + 10);
    expect(Object.keys(folded).sort()).toEqual(['confirmed', 'phase', 'value']);
  });

  it('at rest, is just the hardware winning — the poll adopts what the timer reports', () => {
    const atRest: ThresholdState = { value: 90, confirmed: 90, phase: 'confirmed' };
    expect(foldPolled(atRest, 104, T0)).toEqual({ value: 104, confirmed: 104, phase: 'confirmed' });
  });

  it('leaves a threshold the RD is holding alone, whatever the poll says', () => {
    for (const phase of ['pending', 'mismatch', 'failed', 'refused'] as const) {
      const busy: ThresholdState = { value: 120, confirmed: 90, phase };
      expect(foldPolled(busy, 104, T0)).toBe(busy);
    }
  });
});

describe('phase presentation', () => {
  it('labels every phase in plain language, readable at arm’s length', () => {
    expect(phaseLabel('confirmed')).toBe('On timer');
    expect(phaseLabel('pending')).toBe('Adjusting');
    expect(phaseLabel('sent')).toBe('Sending…');
    expect(phaseLabel('mismatch')).toBe('Not taken');
    expect(phaseLabel('failed')).toBe('Failed');
    expect(phaseLabel('refused')).toBe('Not sent');
  });

  it('tones the settled state apart from the wrong ones', () => {
    expect(phaseTone('confirmed')).toBe('success');
    expect(phaseTone('mismatch')).toBe('danger');
    expect(phaseTone('failed')).toBe('danger');
    expect(phaseTone('refused')).toBe('warn');
  });
});

describe('nodeTuneLabel — CLAUDE.md: never a bare frequency, never a raw seat', () => {
  it('resolves the frequency to band + channel through channels.ts', () => {
    expect(nodeTuneLabel(0, 5880, CATALOG)).toBe('Node 1 · Raceband R7');
    expect(nodeTuneLabel(3, 5658, CATALOG)).toBe('Node 4 · Raceband R1');
  });

  it('is the 1-based seat alone when the node has no frequency yet', () => {
    expect(nodeTuneLabel(0, undefined, CATALOG)).toBe('Node 1');
  });

  it('never renders a bare raw seat ref', () => {
    expect(nodeTuneLabel(0, 5880, CATALOG)).not.toContain('node-0');
  });
});

describe('nodeCountOf', () => {
  it('prefers what the timer actually reports', () => {
    const snap = snapshot({ nodes: [node(), node({ node: 1, seat: 'node-1' })] });
    expect(nodeCountOf(snap, 8)).toBe(2);
  });

  it('lays out from the registry before the first snapshot lands', () => {
    expect(nodeCountOf(undefined, 4)).toBe(4);
    expect(nodeCountOf(snapshot({ nodes: [] }), 4)).toBe(4);
  });

  it('counts UNSEEN nodes too — "is this node even alive?" is half the diagnostic', () => {
    // The Director includes unseated/unreported nodes deliberately. Filtering them out here would
    // silently drop the columns an RD chasing a dead gate is most likely to be looking at.
    const snap = snapshot({
      nodes: [node(), node({ node: 1, seat: 'node-1', seen: false })]
    });
    expect(nodeCountOf(snap, 8)).toBe(2);
  });
});

describe('nodeTraceOf — the adapter onto RssiGraph’s live mode', () => {
  it('keys the trace on the node’s OWN seat, not a locally re-spelled one', () => {
    // The seat is what a heat's registration binds a pilot to. Re-deriving `node-{i}` here is
    // exactly the resolver drift CLAUDE.md exists to prevent — so the wire's handle is used as-is.
    const n = node({ node: 2, seat: 'node-2', samples: [10, 20] });
    const trace = nodeTraceOf(snapshot({ nodes: [n] }), n);
    expect(trace.competitor).toEqual({ adapter: 'rh-1', competitor: 'node-2' });
    expect(trace.samples).toEqual([10, 20]);
  });

  it('carries the levels the TIMER holds — the page overlays its pending value via `tuned`', () => {
    const n = node({ enter_at: 90, exit_at: 80 });
    const trace = nodeTraceOf(snapshot({ nodes: [n] }), n);
    expect(trace.enter).toBe(90);
    expect(trace.exit).toBe(80);
  });

  it('takes its time base from the SHARED axis, not from the node', () => {
    // `sample_micros` lives on the snapshot, once, because every node is sampled in the same pass.
    // A per-node `from` would be O(nodes) copies of identical numbers — and 2b invented one.
    const n = node({ samples: [10, 20, 30] });
    const snap = snapshot({ nodes: [n], sample_micros: [1_000, 1_200, 1_400] });
    const trace = nodeTraceOf(snap, n);
    expect(trace.from).toBe(1_000);
    expect(trace.period_micros).toBe(200_000);
    // Handed on verbatim, so the plot is drawn at the instants the Director stamped rather than at
    // a `from + i·period` grid reconstructed from them.
    expect(trace.times).toEqual([1_000, 1_200, 1_400]);
  });

  it('drops the explicit axis when it does not line up with the node’s samples', () => {
    // Belt and braces: a mis-lengthed axis would misplace every sample, and the uniform grid is
    // exact for this (steady-cadence) feed anyway.
    const n = node({ samples: [10, 20, 30] });
    const trace = nodeTraceOf(snapshot({ nodes: [n], sample_micros: [1_000] }), n);
    expect(trace.times).toBeUndefined();
  });

  it('never yields a zero period (a zero would divide the whole projection by nothing)', () => {
    const n = node();
    expect(nodeTraceOf(snapshot({ nodes: [n], period_micros: 0 }), n).period_micros).toBe(1);
  });
});

describe('plottable — an unseen node is DEAD, not quiet', () => {
  it('refuses to plot a node RotorHazard has never reported', () => {
    // The Director samples every node on the same pass and fills an unreported one's slot with 0.0,
    // so a dead node arrives with a full, perfectly plottable ring of zeroes. Drawn, that is a flat
    // trace along the floor — indistinguishable from a live node over a quiet gate, which is the
    // one confusion this page exists to remove.
    expect(plottable(node({ seen: false, samples: [0, 0, 0, 0] }))).toBe(false);
  });

  it('plots a node that HAS reported, even with nothing on the gate', () => {
    expect(plottable(node({ seen: true, samples: [12, 11, 12] }))).toBe(true);
  });

  it('has nothing to plot before the first snapshot', () => {
    expect(plottable(undefined)).toBe(false);
  });
});

describe('the signal feed is LEASED, and the poll is what holds it', () => {
  it('polls an order of magnitude inside the lease, not merely inside it', () => {
    // Every GET renews the lease; stop polling and the Director tears the stream down. A cadence
    // that only just fits would drop the feed the first time one answer is slow, and the RD would
    // see a plot stop moving for no reason they can see.
    expect(holdsLease(SIGNAL_POLL_MS)).toBe(true);
    expect(SIGNAL_POLL_MS * 10).toBeLessThanOrEqual(SIGNAL_LEASE_MS);
  });

  it('rejects a cadence with no margin, even one that technically renews in time', () => {
    expect(holdsLease(4_000)).toBe(false);
    expect(holdsLease(SIGNAL_LEASE_MS)).toBe(false);
    expect(holdsLease(0)).toBe(false);
  });
});

describe('readoutsOf — the six stats, all from node_data', () => {
  it('reads the peaks/nadirs/count off node_data, not the heartbeat', () => {
    // `get_heartbeat_json` carries only rssi / frequency_mhz / loop_time_micros / crossing; a page
    // that looked for the peaks there would render six permanent dashes.
    const out = readoutsOf(
      node({
        rssi: 48,
        node_peak_rssi: 132,
        node_nadir_rssi: 12,
        pass_peak_rssi: 118,
        pass_nadir_rssi: 41,
        pass_count: 7
      })
    );
    expect(out.map((r) => `${r.label} ${r.value}`)).toEqual([
      'RSSI 48',
      'Node peak 132',
      'Node nadir 12',
      'Pass peak 118',
      'Pass nadir 41',
      'Passes 7'
    ]);
  });

  it('dashes an unreported field rather than rendering a misleading zero', () => {
    // Every field is `Option` on the wire and that is load-bearing: a dash says "not reported",
    // which is information. A zero standing in for it is a lie the RD would tune against.
    expect(readoutsOf(undefined).every((r) => r.value === '—')).toBe(true);
    expect(readoutsOf(node({ pass_count: 0 })).at(-1)?.value).toBe('0');
  });

  it('renders the counts as integers — they cross the wire as f32 but they are ADC counts', () => {
    const out = readoutsOf(node({ rssi: 48.0, node_peak_rssi: 131.6 }));
    expect(out[0].value).toBe('48');
    expect(out[1].value).toBe('132');
  });

  it('dashes every stat of a node the timer has never reported', () => {
    // An unseen node carries no `node_data` at all — six dashes, and the column says why.
    expect(
      readoutsOf(node({ seen: false, samples: [0, 0, 0] })).every((r) => r.value === '—')
    ).toBe(true);
  });
});

// ── The channel half (#413) ─────────────────────────────────────────────────────────────────────

/** A fuller catalog: two bands, and a frequency that appears in both (the DJI/HDZero overlap). */
const FULL: ChannelCatalogEntry[] = [
  { band: 'Raceband', channel: 'R1', mhz: 5658 },
  { band: 'Raceband', channel: 'R7', mhz: 5880 },
  { band: 'Fatshark', channel: 'F4', mhz: 5800 },
  { band: 'HDZero', channel: 'R7', mhz: 5880 }
];

const FLEXIBLE: ChannelCapability = 'Flexible';
const FIXED: ChannelCapability = { Fixed: { channels: [5658, 5800] } };

describe('channelOptions — the dropdown source is the CAPABILITY, never `available_channels`', () => {
  it('offers the whole catalog on a Flexible timer whose channel pool is EMPTY', () => {
    // The trap this function exists for. Measured on the bench: Docker RH 4.3.0 and NuclearHazard
    // both report `Flexible` with `available_channels: []`. That empty list means "no restriction"
    // — it is the per-heat allocation POOL, which an RD tuning at the bench has usually never
    // configured — and NOT "no channels". A dropdown bound naively to it renders EMPTY on exactly
    // the timers this feature is for, which is the whole reason #413 was filed with a warning.
    const options = channelOptions(FLEXIBLE, FULL, []);
    expect(options.map((o) => o.mhz)).toEqual([5658, 5880, 5800, 5880]);
    expect(options.length).toBe(FULL.length);
  });

  it('limits a Fixed timer to its declared set', () => {
    // The other half of the capability: a limited module offers only what it physically supports,
    // so a channel it cannot tune to is never offered (and never refused after the fact).
    expect(channelOptions(FIXED, FULL, []).map((o) => o.mhz)).toEqual([5658, 5800]);
  });

  it("includes the RD's custom raw-MHz channels alongside the catalog, on a Flexible timer", () => {
    // The one thing `available_channels` legitimately contributes here: the custom entries the RD
    // typed into the timer's channel config. They come AFTER the catalog, ascending, and only the
    // ones the catalog does not already know (5800 is Fatshark F4 — it must not appear twice).
    const options = channelOptions(FLEXIBLE, FULL, [5891, 5800, 5645]);
    expect(options.slice(FULL.length).map((o) => o.mhz)).toEqual([5645, 5891]);
    expect(options.filter((o) => o.mhz === 5800)).toHaveLength(1);
    expect(options.find((o) => o.mhz === 5891)?.custom).toBe(true);
  });

  it('ignores custom channels on a Fixed timer — it supports what it supports', () => {
    expect(channelOptions(FIXED, FULL, [5891]).some((o) => o.mhz === 5891)).toBe(false);
  });

  it('always offers the channel the node is currently on, even off-catalog and off-pool', () => {
    // A dropdown that cannot show the node's actual value would silently render some OTHER option
    // as selected — the RD would read a channel the gate is not on.
    expect(channelOptions(FLEXIBLE, FULL, [], 5905).some((o) => o.mhz === 5905)).toBe(true);
    // …but not one a Fixed timer cannot tune to: that would offer a refusal.
    expect(channelOptions(FIXED, FULL, [], 5905).some((o) => o.mhz === 5905)).toBe(false);
  });

  it('labels every option by BAND AND CHANNEL, never as a bare frequency', () => {
    // CLAUDE.md: the option `value` may stay the raw MHz, the visible label may not.
    const options = channelOptions(FLEXIBLE, FULL, [5891]);
    expect(options[1].label).toBe('Raceband R7');
    // A coincident frequency keeps the band the RD picked, rather than collapsing to the first
    // catalog entry that happens to share the number.
    expect(options[3].label).toBe('HDZero R7');
    expect(options.every((o) => !/\d{4}/.test(o.label) || o.label.endsWith('MHz'))).toBe(true);
    // A custom channel has no catalog name, so it is spelled as a measurement rather than a bare
    // number standing in for a name.
    expect(options.at(-1)?.label).toBe('5891 MHz');
  });

  it('carries the band and channel of the picked entry, so the emit can label it on RotorHazard', () => {
    // RotorHazard stores band/channel on its profile, and the RD validates this work by refreshing
    // RotorHazard's own page — where a bare frequency reads as "it half worked".
    const hdzero = channelOptions(FLEXIBLE, FULL, []).at(-1);
    expect(hdzero).toMatchObject({ mhz: 5880, band: 'HDZero', channel: 'R7' });
  });
});

describe('offerableNodes — never offer a node the hardware does not have (#412)', () => {
  const view = (over: Partial<TimerNodes> = {}): TimerNodes => ({
    timer: 'rh-1',
    width: 4,
    nodes: [],
    enabled: [0, 1, 3],
    ...over
  });

  it('offers exactly the enabled set — a hole in the middle stays a hole', () => {
    expect([...offerableNodes(view())]).toEqual([0, 1, 3]);
  });

  it('FAILS CLOSED with no node view: nothing is offered', () => {
    // RotorHazard validates `0 <= node < num_nodes` and otherwise only writes a log line, so a
    // channel write to a node that does not exist looks accepted and lands nowhere. Better a
    // control that appears a beat late than one that offers a gate the hardware does not have.
    expect(offerableNodes(undefined).size).toBe(0);
  });
});

describe('the channel write lifecycle — the confirmation is a POLL, as it is for a threshold', () => {
  it('settles to confirmed when the timer reports the channel that was sent', () => {
    const sent = markChannelSent(seedChannel(5658), 5880, 1_000, false);
    expect(sent.phase).toBe('sent');
    expect(foldPolledChannel(sent, 5880, 1_100, FULL).phase).toBe('confirmed');
  });

  it('stays `sent` while the change may simply still be in flight', () => {
    const sent = markChannelSent(seedChannel(5658), 5880, 1_000, false);
    expect(foldPolledChannel(sent, 5658, 1_000 + CONFIRM_TIMEOUT_MS - 1, FULL).phase).toBe('sent');
  });

  it('says NOT TAKEN, loudly and by name, when the polls keep disagreeing', () => {
    // The #403 failure class: a write that reports dispatched and never lands. And the message
    // names both channels the way the RD reads them (CLAUDE.md), never as bare numbers.
    const sent = markChannelSent(seedChannel(5658), 5880, 1_000, false);
    const out = foldPolledChannel(sent, 5658, 1_000 + CONFIRM_TIMEOUT_MS + 1, FULL);
    expect(out.phase).toBe('mismatch');
    expect(out.detail).toContain('Raceband R1');
    expect(out.detail).toContain('Raceband R7');
    expect(out.detail).not.toMatch(/\b5880\b/);
  });

  it('FOLLOWS the hardware at rest — a heat legitimately retunes every node', () => {
    // Channel here is a bench setting; heat setup reassigns, and that is correct. The page shows
    // what the node is on rather than insisting on what was picked at the bench.
    const held = seedChannel(5880);
    expect(foldPolledChannel(held, 5658, 1_000, FULL)).toMatchObject({
      mhz: 5658,
      confirmed: 5658,
      phase: 'confirmed'
    });
  });

  it('never adopts "tuned to nothing" as a channel', () => {
    // RotorHazard reports `0` for a node tuned to nothing; the adapter turns that into an absence,
    // and an absence is not a value to display as the node's channel.
    const held = seedChannel(5880);
    expect(foldPolledChannel(held, undefined, 1_000, FULL)).toBe(held);
  });
});

describe('staleThresholdNote — the thing nothing else announces', () => {
  it('states plainly that the levels were tuned on the previous channel', () => {
    // `on_set_frequency` writes the frequency into the SAME profile row that holds the thresholds,
    // so a channel change leaves them untouched — tuned for the channel the node just left.
    const note = staleThresholdNote(5880, 5800, FULL);
    expect(note).toContain('Raceband R7');
    expect(note).toContain('Fatshark F4');
    expect(note).toContain('unchanged');
  });

  it('is FACTUAL, not alarming — the levels are unverified, not necessarily wrong', () => {
    const note = staleThresholdNote(5880, 5800, FULL);
    expect(note).not.toMatch(/wrong|broken|error|invalid|fail/i);
  });

  it('names both channels rather than printing a raw frequency', () => {
    expect(staleThresholdNote(5880, 5800, FULL)).not.toMatch(/\b5880\b/);
  });

  it('carries the ORIGINAL tuning channel through a second change', () => {
    // Two changes in a row: the thresholds are still the ones tuned on the FIRST channel, and
    // saying "tuned on the one you just left" would be a lie.
    const first = markChannelSent(seedChannel(5880), 5800, 1_000, true);
    const second = markChannelSent(first, 5658, 2_000, true);
    expect(second.tunedOn).toBe(5880);
  });

  it('records nothing to announce when the Director says the thresholds are not stale', () => {
    expect(markChannelSent(seedChannel(5880), 5800, 1_000, false).tunedOn).toBeUndefined();
  });
});

describe('duplicateChannelNodes — flagged, never blocked', () => {
  it('finds every node sharing a channel with another', () => {
    const clashing = duplicateChannelNodes(
      new Map([
        [0, 5880],
        [1, 5658],
        [2, 5880]
      ])
    );
    expect([...clashing].sort()).toEqual([0, 2]);
  });

  it('does not treat two nodes tuned to NOTHING as a clash', () => {
    expect(
      duplicateChannelNodes(
        new Map([
          [0, undefined],
          [1, undefined]
        ])
      ).size
    ).toBe(0);
  });

  it('names the other nodes the way the RD does — 1-based, never a raw index', () => {
    const note = duplicateChannelNote(0, [0, 2]);
    expect(note).toContain('Node 3');
    expect(note).not.toContain('Node 0');
    // And it says why it matters without forbidding it: a swap looks exactly like this.
    expect(note).toContain('swapping');
  });
});

describe('channelGate — ONE rule for a Tune-page write, two ways of saying it', () => {
  it('refuses under a competition heat and allows in open practice, exactly as writeGate does', () => {
    // Delegation, not a restated rule: the channel dropdown and the threshold sliders must never
    // disagree about whether a heat is protected.
    expect(channelGate('Running', 'competition').allowed).toBe(false);
    expect(channelGate('Running', 'practice').allowed).toBe(true);
    expect(channelGate('Running', undefined).allowed).toBe(true);
    expect(channelGate('Staged', 'competition').allowed).toBe(true);
    expect(channelGate(undefined, undefined).allowed).toBe(true);
  });

  it('gives the refusal a reason about the CHANNEL, not about thresholds', () => {
    // Retuning a receiver mid-race is a different kind of wrong from moving a threshold: it takes
    // the gate off the channel the pilot is flying, rather than changing what counts as a lap.
    const gate = channelGate('Running', 'competition');
    expect(gate.allowed).toBe(false);
    if (!gate.allowed) {
      expect(gate.reason).toContain('channel');
      expect(gate.reason).not.toContain('threshold');
    }
  });
});
