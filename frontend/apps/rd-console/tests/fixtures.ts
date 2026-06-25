/**
 * Typed fixtures for the console tests — built straight from `@gridfpv/types`, so a
 * contract change surfaces as a compile error here too.
 */
import type {
  AuditEntry,
  CommandAck,
  EventOutcome,
  HeatResult,
  LapList,
  LiveRaceState,
  RankEntry,
  SignalTraceView
} from '@gridfpv/types';

export const liveRunning: LiveRaceState = {
  current_heat: 'heat-1',
  phase: 'Running',
  active_pilots: ['ALICE', 'BOB', 'CARMEN'],
  progress: [
    { competitor: 'ALICE', laps_completed: 3, last_lap_micros: 41_000_000 },
    { competitor: 'BOB', laps_completed: 2, last_lap_micros: 43_000_000 },
    { competitor: 'CARMEN', laps_completed: 2, last_lap_micros: undefined }
  ],
  running_order: ['ALICE', 'BOB', 'CARMEN'],
  on_deck: 'heat-2'
};

export const heatResult: HeatResult = {
  places: [
    {
      competitor: { adapter: 'rh-1', competitor: 'ALICE' },
      position: 1,
      laps: 3,
      metric: { BestLapMicros: 41_250_000 }
    },
    {
      competitor: { adapter: 'rh-1', competitor: 'BOB' },
      position: 2,
      laps: 3,
      metric: { BestLapMicros: 42_100_000 }
    }
  ]
};

export const standings: RankEntry[] = [
  { competitor: 'ALICE', position: 1 },
  { competitor: 'BOB', position: 2 },
  { competitor: 'CARMEN', position: 3 }
];

export const lapList: LapList = {
  competitors: [
    {
      competitor: { adapter: 'rh-1', competitor: 'ALICE' },
      // Global pass offsets: ALICE's three passes at 10, 12, 14. `at` is the closing pass's
      // source-clock time (µs) — the gate-pass instant the RSSI graph marks. Lap 1's opening
      // pass is at 0s, so lap 1 closes at 41.0s and lap 2 at 81.5s.
      laps: [
        { number: 1, duration_micros: 41_000_000, at: 41_000_000, start_ref: 10, end_ref: 12 },
        { number: 2, duration_micros: 40_500_000, at: 81_500_000, start_ref: 12, end_ref: 14 }
      ]
    },
    {
      competitor: { adapter: 'rh-1', competitor: 'BOB' },
      // BOB's two passes at 11, 13; its single lap closes at 43.0s.
      laps: [{ number: 1, duration_micros: 43_000_000, at: 43_000_000, start_ref: 11, end_ref: 13 }]
    }
  ]
};

/**
 * A captured RSSI signal trace for the heat (marshaling Slice 1 shape) — one competitor (ALICE)
 * with a streaming-cadence sample run, enter/exit thresholds, and a time base (`from` /
 * `period_micros`) that places sample `i` at `from + i·period_micros` on the source clock. The
 * span covers ALICE's lap-gate passes (41.0s, 81.5s) so the Slice-4 graph can mark them. BOB has
 * no trace entry — a heat where only some nodes captured signal. A heat with **no** trace at all
 * (a sim heat) is `{ competitors: [] }`; see {@link emptySignalTrace}.
 */
export const signalTrace: SignalTraceView = {
  competitors: [
    {
      competitor: { adapter: 'rh-1', competitor: 'ALICE' },
      from: 0,
      // 1s cadence; 91 samples span 0..90s, covering both lap-gate passes.
      period_micros: 1_000_000,
      samples: Array.from({ length: 91 }, (_, i) => {
        // Two peaks near the gate passes (41s, 81.5s) crossing the enter threshold, noisy floor.
        const near = (c: number) => Math.max(0, 60 - Math.abs(i - c) * 12);
        return 70 + near(41) + near(81) + (i % 3);
      }),
      enter: 110,
      exit: 95
    }
  ]
};

/** A heat with no captured trace (a sim heat) — the graph is skipped, lap-only layout shown. */
export const emptySignalTrace: SignalTraceView = { competitors: [] };

export const marshalingAudit: AuditEntry[] = [
  {
    kind: 'PenaltyApplied',
    at: 1_700_000_000_000_000,
    at_ref: 20,
    summary: 'DQ applied for CARMEN'
  },
  { kind: 'Voided', at: 1_700_000_000_000_000, at_ref: 18, summary: 'Detection voided (ref 12)' }
];

export const eventOutcome: EventOutcome = {
  qualifying: standings,
  qualifying_heats: [],
  bracket_seeds: ['ALICE', 'BOB', 'CARMEN', 'DANA'],
  bracket: [
    { competitor: 'ALICE', position: 1 },
    { competitor: 'BOB', position: 2 }
  ],
  bracket_heats: [
    {
      heat: 'sf-1',
      result: {
        places: [
          {
            competitor: { adapter: 'rh-1', competitor: 'ALICE' },
            position: 1,
            laps: 3,
            metric: { BestLapMicros: 41_000_000 }
          },
          {
            competitor: { adapter: 'rh-1', competitor: 'DANA' },
            position: 2,
            laps: 3,
            metric: { BestLapMicros: 45_000_000 }
          }
        ]
      }
    },
    {
      heat: 'sf-2',
      result: {
        places: [
          {
            competitor: { adapter: 'rh-1', competitor: 'BOB' },
            position: 1,
            laps: 3,
            metric: { BestLapMicros: 42_000_000 }
          },
          {
            competitor: { adapter: 'rh-1', competitor: 'CARMEN' },
            position: 2,
            laps: 3,
            metric: { BestLapMicros: 46_000_000 }
          }
        ]
      }
    },
    {
      heat: 'final',
      result: {
        places: [
          {
            competitor: { adapter: 'rh-1', competitor: 'ALICE' },
            position: 1,
            laps: 3,
            metric: { BestLapMicros: 40_000_000 }
          },
          {
            competitor: { adapter: 'rh-1', competitor: 'BOB' },
            position: 2,
            laps: 3,
            metric: { BestLapMicros: 41_500_000 }
          }
        ]
      }
    }
  ]
};

export const okAck: CommandAck = { ok: true };
export const failAck: CommandAck = {
  ok: false,
  error: { code: 'BadRequest', message: 'illegal transition' }
};
