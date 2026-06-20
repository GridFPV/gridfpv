/**
 * Typed fixtures for the console tests — built straight from `@gridfpv/types`, so a
 * contract change surfaces as a compile error here too.
 */
import type {
  CommandAck,
  EventOutcome,
  HeatResult,
  LapList,
  LiveRaceState,
  RankEntry
} from '@gridfpv/types';

export const liveRunning: LiveRaceState = {
  current_heat: 'heat-1',
  phase: 'Running',
  active_pilots: ['ALICE', 'BOB', 'CARMEN'],
  progress: [
    { competitor: 'ALICE', laps_completed: 3, last_lap_micros: 41_000_000n },
    { competitor: 'BOB', laps_completed: 2, last_lap_micros: 43_000_000n },
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
      metric: { BestLapMicros: 41_250_000n }
    },
    {
      competitor: { adapter: 'rh-1', competitor: 'BOB' },
      position: 2,
      laps: 3,
      metric: { BestLapMicros: 42_100_000n }
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
      laps: [
        { number: 1, duration_micros: 41_000_000n },
        { number: 2, duration_micros: 40_500_000n }
      ]
    },
    {
      competitor: { adapter: 'rh-1', competitor: 'BOB' },
      laps: [{ number: 1, duration_micros: 43_000_000n }]
    }
  ]
};

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
            metric: { BestLapMicros: 41_000_000n }
          },
          {
            competitor: { adapter: 'rh-1', competitor: 'DANA' },
            position: 2,
            laps: 3,
            metric: { BestLapMicros: 45_000_000n }
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
            metric: { BestLapMicros: 42_000_000n }
          },
          {
            competitor: { adapter: 'rh-1', competitor: 'CARMEN' },
            position: 2,
            laps: 3,
            metric: { BestLapMicros: 46_000_000n }
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
            metric: { BestLapMicros: 40_000_000n }
          },
          {
            competitor: { adapter: 'rh-1', competitor: 'BOB' },
            position: 2,
            laps: 3,
            metric: { BestLapMicros: 41_500_000n }
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
