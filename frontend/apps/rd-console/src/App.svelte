<script lang="ts">
  // RD console shell — doubles as a components playground for the v1 library.
  // Composes every @gridfpv/components widget from typed @gridfpv/types data, so
  // #51+ can lift these into the real live-projection-driven console. The real
  // console (setup wizard, registration, live control, marshaling, results) is
  // built out in #51+; the protocol client stays stubbed here.
  import {
    Leaderboard,
    StandingsTable,
    BracketTree,
    HeatSheet,
    RaceClock,
    PilotCard,
    type Bracket
  } from '@gridfpv/components';
  import { connect } from '@gridfpv/protocol-client';
  import type { HeatResult, RankEntry, LiveRaceState, PilotProgress, Scope } from '@gridfpv/types';

  // ── Demo data (typed against @gridfpv/types) ────────────────────────────────
  const heatResult: HeatResult = {
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
      },
      {
        competitor: { adapter: 'rh-1', competitor: 'CARMEN' },
        position: 3,
        laps: 2,
        metric: { BestLapMicros: null }
      }
    ]
  };

  const standings: RankEntry[] = [
    { competitor: 'ALICE', position: 1 },
    { competitor: 'BOB', position: 2 },
    { competitor: 'CARMEN', position: 3 },
    { competitor: 'DANA', position: 4 }
  ];

  const liveState: LiveRaceState = {
    current_heat: 'heat-1',
    phase: 'Running',
    active_pilots: ['ALICE', 'BOB', 'CARMEN'],
    progress: [
      { competitor: 'ALICE', laps_completed: 3, last_lap_micros: 41_000_000n },
      { competitor: 'BOB', laps_completed: 2, last_lap_micros: 43_000_000n },
      { competitor: 'CARMEN', laps_completed: 2, last_lap_micros: undefined }
    ],
    running_order: ['ALICE', 'BOB', 'CARMEN']
  };

  const leader: PilotProgress = liveState.progress![0];

  const bracket: Bracket = {
    rounds: [
      {
        name: 'Semifinals',
        matches: [
          { heat: 'sf-1', slots: [{ competitor: 'ALICE', winner: true }, { competitor: 'DANA' }] },
          { heat: 'sf-2', slots: [{ competitor: 'BOB', winner: true }, { competitor: 'CARMEN' }] }
        ]
      },
      {
        name: 'Final',
        matches: [
          { heat: 'final', slots: [{ competitor: 'ALICE', winner: true }, { competitor: 'BOB' }] }
        ]
      }
    ]
  };

  // The real protocol client (#49): snapshot + WS subscribe, scoped to this heat.
  const scope: Scope = { Heat: { heat: 'heat-1' } };
  const client = connect({ baseUrl: 'http://localhost:8080', scope });
</script>

<main class="gridfpv-root">
  <h1>GridFPV — RD console</h1>
  <p class="muted">
    Component library playground. Connected to <code>{client.baseUrl}</code> (stub).
  </p>

  <section>
    <h2>Race clock</h2>
    <div class="clocks">
      <RaceClock elapsedMs={83_456} />
      <RaceClock remainingMs={36_544} />
    </div>
  </section>

  <section>
    <h2>Pilot card</h2>
    <PilotCard progress={leader} name="Alice A." position={1} />
  </section>

  <section>
    <h2>Heat sheet (live)</h2>
    <HeatSheet
      state={liveState}
      names={{ ALICE: 'Alice A.', BOB: 'Bob B.', CARMEN: 'Carmen C.' }}
    />
  </section>

  <section>
    <h2>Leaderboard</h2>
    <Leaderboard result={heatResult} metricLabel="Best lap" />
  </section>

  <section>
    <h2>Standings</h2>
    <StandingsTable entries={standings} caption="Open class — overall" />
  </section>

  <section>
    <h2>Bracket</h2>
    <BracketTree {bracket} />
  </section>
</main>

<style>
  main {
    max-width: 48rem;
    margin: 2rem auto;
    padding: 0 1rem;
    font-family: var(--gf-font-family);
  }
  h1 {
    font-size: var(--gf-font-size-xl);
  }
  h2 {
    font-size: var(--gf-font-size-lg);
    margin-top: var(--gf-space-8);
  }
  .muted {
    color: var(--gf-color-text-muted);
  }
  .clocks {
    display: flex;
    gap: var(--gf-space-8);
    align-items: baseline;
  }
</style>
