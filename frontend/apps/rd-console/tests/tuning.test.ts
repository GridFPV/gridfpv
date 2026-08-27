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
  captureLabel,
  captureSecondsLeft,
  channelGate,
  channelOptions,
  clampLevel,
  duplicateChannelNodes,
  duplicateChannelNote,
  foldCapture,
  foldPolled,
  foldPolledChannel,
  holdsLease,
  isParsableLevel,
  markChannelSent,
  markSent,
  nodeCountOf,
  nodeTraceOf,
  offerableNodes,
  phaseLabel,
  phaseTone,
  plottable,
  readoutsOf,
  seedChannel,
  seedThreshold,
  staleThresholdNote,
  writeGate,
  type CaptureState,
  type ThresholdState
} from '../src/lib/tuning.js';

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

describe('markSent — a LATE write resolve must not clobber a newer value (#442, defect 1)', () => {
  const T0 = 1_000_000;

  // The scenario, as the RD lives it: they commit `enter = 100` (the write leaves), and inside the
  // round trip they type `120`. `adjust()` puts the state on 120/'pending' but does NOT bump
  // `writeSeq` (only `commit()` does), so when the 100-write resolves, TunePage's seq guard passes
  // and it calls `markSent(state, 100)` on a state that has moved on. `markSent` overwrites
  // unconditionally — the value goes back to 100 and the phase to 'sent' — and the 300 ms idle
  // commit for the 120 then dies on `if (state.phase === 'sent') return`. The 120 is gone, and the
  // badge reads "On timer" over a number the RD did not choose: the #403 sent-vs-landed lie, in the
  // UI layer.
  //
  // Fixed by making the write mark only the state it actually wrote: a resolve that finds the value
  // already moved on returns the state untouched.
  it('leaves the value the RD moved on to standing, and leaves it pending', () => {
    // The state at the instant `commit()` issued the 100-write, then the RD's 120 typed over it.
    const rdTyped120: ThresholdState = { value: 120, confirmed: 90, phase: 'pending' };

    const afterLateResolve = markSent(rdTyped120, 100, T0);

    // The 120 is the newest thing the RD asked for. Nothing the older write does may replace it.
    expect(afterLateResolve.value).toBe(120);
    // And it has to still read as 'pending', because that is the only phase the idle commit will
    // write from — a state parked on 'sent' silently swallows the follow-up write.
    expect(afterLateResolve.phase).toBe('pending');
  });

  // The companion, and the reason the fix cannot just be "never overwrite": the ordinary write —
  // where nothing moved between issuing and resolving — must still go to 'sent' with its receipt,
  // or `foldPolled` has nothing to confirm against.
  it('still marks the ordinary, unraced write as sent', () => {
    const unraced: ThresholdState = { value: 100, confirmed: 90, phase: 'pending' };
    expect(markSent(unraced, 100, T0)).toMatchObject({
      value: 100,
      phase: 'sent',
      sent: 100,
      sentAt: T0
    });
  });

  it('returns the moved-on state IDENTICALLY, so nothing downstream reads as a change', () => {
    // Not just equal — the same object. `ingest` and the confirm backstop both compare states by
    // identity to decide whether anything happened; a fresh clone here would churn the reactive
    // record on every late resolve and re-render a column the RD is mid-drag on.
    const movedOn: ThresholdState = { value: 120, confirmed: 90, phase: 'pending' };
    expect(markSent(movedOn, 100, T0)).toBe(movedOn);
  });

  it('leaves no receipt behind for a write the state moved on from', () => {
    // `sent`/`sentAt` are what `foldPolled` confirms against. Recording 100 on a state holding 120
    // would arm the poll to "confirm" a value the RD had already abandoned, and settle it green.
    const movedOn: ThresholdState = { value: 120, confirmed: 90, phase: 'pending' };
    const after = markSent(movedOn, 100, T0);
    expect(after.sent).toBeUndefined();
    expect(after.sentAt).toBeUndefined();
  });

  it('a re-typed value that lands back on what was written still marks sent', () => {
    // The RD went 100 → 120 → 100 inside the round trip. The write and the state agree again, so
    // there is nothing newer to protect and the receipt is the honest one.
    const backTo100: ThresholdState = { value: 100, confirmed: 90, phase: 'pending' };
    expect(markSent(backTo100, 100, T0).phase).toBe('sent');
  });

  it('clears a stale failure detail on the write it does mark', () => {
    const retry: ThresholdState = { value: 100, confirmed: 90, phase: 'failed', detail: 'boom' };
    expect(markSent(retry, 100, T0).detail).toBeUndefined();
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
  // A second name for a frequency the catalog already carries: 5880 is Raceband R7 AND
  // Fatshark F8. The picker must show it ONCE, led by the first-listed band.
  { band: 'Fatshark', channel: 'F8', mhz: 5880 }
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
    // One row per FREQUENCY: 5880 appears once even though the catalog names it twice.
    expect(options.map((o) => o.mhz)).toEqual([5658, 5880, 5800]);
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
    expect(options.filter((o) => o.custom).map((o) => o.mhz)).toEqual([5645, 5891]);
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
  });

  // #449. This assertion used to read the other way — "…but not one a Fixed timer cannot tune to:
  // that would offer a refusal" — which asserted the bug as the behaviour. It is wrong on its own
  // terms: nothing is being offered *as a write* here, the node is ALREADY on that channel, and
  // `<select value={chan.mhz}>` matching no option does not hide the situation, it makes the
  // browser show the first option instead — a channel the node is not on, presented as the RD's
  // own selection.
  it('shows a Fixed timer the channel its node is on, even outside the declared set', () => {
    // How a real timer gets here: a heat retuned the node before the RD narrowed the declared set,
    // or RotorHazard came back holding a stale profile.
    const options = channelOptions(FIXED, FULL, [], 5905);
    expect(options.some((o) => o.mhz === 5905)).toBe(true);
    // Named through the shared helper, so the off-catalog frequency reads as the RD's own rather
    // than as a bare number standing in for a name it does not have.
    expect(options.find((o) => o.mhz === 5905)?.label).toBe('Custom — 5905');
    // And the declared set is still all it OFFERS beyond that — the escape hatch is one channel
    // wide, not a hole in the capability.
    expect(options.map((o) => o.mhz)).toEqual([5658, 5800, 5905]);
  });

  it('shows a Fixed timer a CATALOG channel its node is on but that it does not declare', () => {
    // The same situation with a channel the catalog can name: it keeps its band and channel, so
    // the emit that moves the node off it can still label itself on RotorHazard.
    const options = channelOptions(FIXED, FULL, [], 5880);
    expect(options.find((o) => o.mhz === 5880)).toMatchObject({
      mhz: 5880,
      label: 'Raceband R7 (F8) — 5880',
      custom: false
    });
  });

  it('does not duplicate the current channel when the capability already offers it', () => {
    expect(channelOptions(FIXED, FULL, [], 5800).filter((o) => o.mhz === 5800)).toHaveLength(1);
    expect(channelOptions(FLEXIBLE, FULL, [5891], 5891).filter((o) => o.mhz === 5891)).toHaveLength(
      1
    );
  });

  // #449, the second half: `offeredCatalog` filtered the catalog, so a Fixed timer's declared
  // frequency that the catalog had no entry for was dropped before it ever reached an option.
  it('offers a Fixed timer a declared channel the catalog does not know', () => {
    const oddball: ChannelCapability = { Fixed: { channels: [5658, 5891] } };
    const options = channelOptions(oddball, FULL, []);
    expect(options.map((o) => o.mhz)).toEqual([5658, 5891]);
    // Labelled from the raw MHz through `channels.ts`, and marked as the non-catalog entry it is.
    expect(options[1]).toMatchObject({ label: 'Custom — 5891', custom: true });
    // No band/channel invented for it — the emit omits them rather than handing RotorHazard a
    // made-up label (see `commitChannel`).
    expect(options[1].band).toBeUndefined();
    expect(options[1].channel).toBeUndefined();
  });

  it('offers a Fixed timer whose declared set is entirely off-catalog, rather than nothing', () => {
    const oddball: ChannelCapability = { Fixed: { channels: [5921, 5891] } };
    expect(channelOptions(oddball, FULL, []).map((o) => o.mhz)).toEqual([5891, 5921]);
  });

  it('labels every option by band, channel AND frequency, and marks a custom one', () => {
    // The option `value` stays the raw MHz (a wire handle). The visible label LEADS with the
    // friendly name and carries the frequency after it: an RD choosing a channel is matching it
    // against a VTX, a printed sheet, or RotorHazard's own screen, and those speak in MHz. The
    // number is extra information beside the name, never a substitute for it — which is the line
    // CLAUDE.md's display rule actually draws.
    const options = channelOptions(FLEXIBLE, FULL, [5891]);
    // A frequency with two names is ONE row, led by the first-listed band, with the other name in
    // parentheses — so a pilot who knows their VTX as F8 still finds it, and the RD is not shown
    // the same frequency twice with nothing to choose between the rows.
    expect(options[1].label).toBe('Raceband R7 (F8) — 5880');
    expect(options.filter((o) => o.mhz === 5880)).toHaveLength(1);
    // Every catalog option still leads with its name, never a bare number.
    expect(options.every((o) => /^[A-Za-z]/.test(o.label))).toBe(true);
    // A frequency the catalog does not know is marked as the RD's own.
    expect(options.at(-1)?.label).toBe('Custom — 5891');
  });

  it('carries the band and channel of the picked entry, so the emit can label it on RotorHazard', () => {
    // RotorHazard stores band/channel on its profile, and the RD validates this work by refreshing
    // RotorHazard's own page — where a bare frequency reads as "it half worked".
    // The row that survives the de-duplication carries ITS OWN band and channel — the ones shown
    // to the RD — so what RotorHazard is told matches what the console said.
    const shared = channelOptions(FLEXIBLE, FULL, []).find((o) => o.mhz === 5880);
    expect(shared).toMatchObject({ mhz: 5880, band: 'Raceband', channel: 'R7' });
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

describe('Capture — the timer measures the level (#355)', () => {
  /** A capture that started at t=0 with the timer holding 90, sampling for 3 s then 4 s of grace. */
  const started = (): CaptureState => ({
    phase: 'sampling',
    startedAt: 0,
    windowMs: 3_000,
    settleMs: 4_000,
    previous: 90
  });

  it('credits NOTHING to the capture while RotorHazard is still sampling', () => {
    // The window is three seconds long and RotorHazard has not computed a level until it closes —
    // it accumulates `current_rssi` and only divides at the deadline. A threshold that moved during
    // those seconds moved for some other reason, and crediting it would report a number that was
    // never measured.
    expect(foldCapture(started(), 118, 1_500).phase).toBe('sampling');
    expect(foldCapture(started(), 118, 2_999).phase).toBe('sampling');
  });

  it('takes a level that CHANGED after the window as the captured one', () => {
    const next = foldCapture(started(), 118, 3_100);
    expect(next.phase).toBe('captured');
    expect(next.level).toBe(118);
  });

  it('keeps waiting through the grace before giving a verdict', () => {
    // The level has to survive RotorHazard's own write, the Director's decimation and a poll. A
    // verdict declared one poll too early is a false alarm on a capture that did land.
    expect(foldCapture(started(), 90, 3_100).phase).toBe('waiting');
    expect(foldCapture(started(), 90, 6_900).phase).toBe('waiting');
  });

  it('reports a capture that produced NO new level, rather than showing a success', () => {
    // This is the #423 failure class in its capture costume. RotorHazard refuses a capture — a node
    // whose `api_valid_flag` is clear, or one already capturing — by returning False and emitting
    // absolutely nothing, so an unchanged level is the only evidence of that refusal there is.
    const next = foldCapture(started(), 90, 7_100);
    expect(next.phase).toBe('unchanged');
    expect(next.level).toBeUndefined();
    expect(next.detail).toContain('still reporting 90');
    expect(next.detail).toContain('nothing was recorded');
  });

  it('says so plainly when the timer never reported the threshold at all', () => {
    const next = foldCapture(started(), undefined, 7_100);
    expect(next.phase).toBe('unchanged');
    expect(next.detail).toContain('never reported a level');
  });

  it('claims no diagnosis it cannot support', () => {
    // A capture CAN legitimately measure the same level it started from. "Nothing changed" is the
    // whole of what is known, and the copy has to stop there rather than assert a cause.
    const next = foldCapture(started(), 90, 7_100);
    expect(next.detail).toContain('either the pass fell outside the window');
    expect(next.detail).not.toMatch(/RotorHazard is not responding|the node is dead/i);
  });

  it('counts the window down in whole seconds, and stops at zero', () => {
    // The countdown IS the instruction: RotorHazard's window opens at the press, so an RD who does
    // not know how long they have is an RD whose pass lands outside it.
    expect(captureSecondsLeft(started(), 0)).toBe(3);
    expect(captureSecondsLeft(started(), 1_200)).toBe(2);
    expect(captureSecondsLeft(started(), 2_900)).toBe(1);
    expect(captureSecondsLeft(started(), 3_500)).toBe(0);
  });

  it('labels the sampling state as an instruction, not a status', () => {
    expect(captureLabel(started(), 500)).toMatch(/Fly the pass now/);
    expect(captureLabel({ ...started(), phase: 'captured', level: 118 }, 0)).toBe('Captured 118');
    expect(captureLabel({ ...started(), phase: 'unchanged' }, 0)).toBe('Nothing captured');
  });

  it('does NOT clamp the reported level when deciding whether it changed', () => {
    // The comparison is against `previous`, which the Director rounded off the same feed without
    // clamping. Clamping only this side would read a timer sitting on 255 as 254 and call an
    // unchanged level a capture — a fabricated success, which is the one outcome this must never
    // produce. (`clampLevel` is the rule for a value an EDITOR produces; this is the timer's own.)
    const at255: CaptureState = { ...started(), previous: 255 };
    expect(foldCapture(at255, 255, 7_100).phase).toBe('unchanged');
    // …and a genuine change is still a capture, reported at the level the timer actually holds.
    expect(foldCapture(at255, 255.4, 7_100).phase).toBe('unchanged');
    expect(foldCapture(at255, 200, 3_100).level).toBe(200);
  });

  it('leaves a settled capture alone on every later poll', () => {
    const done = foldCapture(started(), 118, 3_100);
    expect(foldCapture(done, 118, 9_000)).toBe(done);
    expect(foldCapture(done, 90, 9_000)).toBe(done);
  });
});
