<script lang="ts">
  // Minimal RD console shell. Proves the monorepo wiring end to end:
  //   - imports shared widgets from @gridfpv/components
  //   - imports protocol types from @gridfpv/types (the generated-bindings seam)
  //   - imports the (stub) protocol client from @gridfpv/protocol-client
  // The real console (setup wizard, registration, live race control, marshaling,
  // results) is built out in #51+.
  import { Leaderboard, RaceClock } from '@gridfpv/components';
  import { connect } from '@gridfpv/protocol-client';
  import type { HeatResult, Scope } from '@gridfpv/types';

  // Placeholder data until the live projection (#51+) streams real results.
  const demo: HeatResult = {
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

  // The real protocol client (#49): snapshot + WS subscribe, scoped to this heat.
  // The console reads `client.getState()` / `client.onState(...)` in #51+.
  const scope: Scope = { Heat: { heat: 'demo-heat-1' } };
  const client = connect({ baseUrl: 'http://localhost:8080', scope });
</script>

<main>
  <h1>GridFPV — RD console</h1>
  <p class="muted">Scaffold shell. Connected to <code>{client.baseUrl}</code> (stub).</p>

  <section>
    <h2>Race clock</h2>
    <RaceClock elapsedMs={83456} />
  </section>

  <section>
    <h2>Leaderboard</h2>
    <Leaderboard result={demo} />
  </section>
</main>

<style>
  main {
    max-width: 40rem;
    margin: 2rem auto;
    padding: 0 1rem;
    font:
      16px/1.5 system-ui,
      sans-serif;
  }
  .muted {
    color: #666;
  }
</style>
