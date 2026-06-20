<script lang="ts">
  // Minimal RD console shell. Proves the monorepo wiring end to end:
  //   - imports shared widgets from @gridfpv/components
  //   - imports protocol types from @gridfpv/types (the generated-bindings seam)
  //   - imports the (stub) protocol client from @gridfpv/protocol-client
  // The real console (setup wizard, registration, live race control, marshaling,
  // results) is built out in #51+.
  import { Leaderboard, RaceClock } from '@gridfpv/components';
  import { connect } from '@gridfpv/protocol-client';
  import type { RaceSnapshot } from '@gridfpv/types';

  // Placeholder data until the protocol client (#49) streams real snapshots.
  const demo: RaceSnapshot = {
    raceId: 'demo-heat-1',
    pilots: ['ALICE', 'BOB', 'CARMEN']
  };

  // The client is a stub today (#49 implements it); this just shows the wiring.
  const client = connect({ baseUrl: 'http://localhost:8080' });
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
    <Leaderboard snapshot={demo} />
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
