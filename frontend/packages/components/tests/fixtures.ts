/**
 * Typed test fixtures — representative projection data for the component tests.
 *
 * Every fixture is typed against `@gridfpv/types` (the generated-bindings seam),
 * so a contract change surfaces as a type error here rather than silently passing
 * a wrong shape into a component.
 */
import type {
  HeatResult,
  RankEntry,
  LiveRaceState,
  PilotProgress,
  CompetitorRef
} from '@gridfpv/types';

export const heatResult: HeatResult = {
  places: [
    {
      competitor: { adapter: 'rh-1', competitor: 'ALICE' },
      position: 1,
      laps: 3,
      metric: { BestLapMicros: 41_250_000 },
      best_lap_micros: 41_250_000
    },
    {
      competitor: { adapter: 'rh-1', competitor: 'BOB' },
      position: 2,
      laps: 3,
      metric: { BestLapMicros: 42_100_000 },
      best_lap_micros: 42_100_000
    },
    {
      competitor: { adapter: 'rh-1', competitor: 'CARMEN' },
      position: 3,
      laps: 2,
      metric: { BestLapMicros: null },
      best_lap_micros: null
    }
  ]
};

export const standings: RankEntry[] = [
  { competitor: 'ALICE', position: 1 },
  { competitor: 'BOB', position: 2 },
  { competitor: 'BOB2', position: 2 },
  { competitor: 'CARMEN', position: 4 }
];

export const pilotProgress: PilotProgress = {
  competitor: 'ALICE',
  laps_completed: 2,
  last_lap_micros: 41_250_000
};

export const liveState: LiveRaceState = {
  current_heat: 'heat-1',
  phase: 'Running',
  active_pilots: ['ALICE', 'BOB', 'CARMEN'] as CompetitorRef[],
  progress: [
    { competitor: 'ALICE', laps_completed: 3, last_lap_micros: 41_000_000 },
    { competitor: 'BOB', laps_completed: 2, last_lap_micros: 43_000_000 },
    { competitor: 'CARMEN', laps_completed: 2, last_lap_micros: undefined }
  ],
  running_order: ['ALICE', 'BOB', 'CARMEN'] as CompetitorRef[]
};
