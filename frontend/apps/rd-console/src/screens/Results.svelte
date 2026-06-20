<script lang="ts">
  /**
   * Results (#56) — heat results, rankings/standings, bracket, and export.
   *
   * Renders typed projection data through the shared components: a `HeatResult` →
   * `Leaderboard`, a `RankEntry[]` → `StandingsTable`, and an `EventOutcome`'s bracket
   * heats → `BracketTree` (via the `bracketFromOutcome` view-model derivation). Export
   * is a lossless JSON download of whichever projection is shown.
   */
  import { Leaderboard, StandingsTable, BracketTree, Button, Card } from '@gridfpv/components';
  import type { HeatResult, RankEntry, EventOutcome } from '@gridfpv/types';
  import { bracketFromOutcome, downloadJson, toExportJson } from '../lib/results.js';

  let {
    heatResult = undefined,
    standings = undefined,
    outcome = undefined,
    metricLabel = 'Best lap'
  }: {
    heatResult?: HeatResult;
    standings?: RankEntry[];
    outcome?: EventOutcome;
    metricLabel?: string;
  } = $props();

  const bracket = $derived(outcome ? bracketFromOutcome(outcome) : undefined);

  function exportAll() {
    const payload = { heatResult, standings, outcome };
    downloadJson('gridfpv-results.json', toExportJson(payload));
  }
</script>

<section class="results" aria-label="Results">
  <header class="head">
    <h2>Results</h2>
    <Button variant="secondary" onclick={exportAll}>Export JSON</Button>
  </header>

  {#if heatResult}
    <Card title="Heat result" pad={false}>
      <Leaderboard result={heatResult} {metricLabel} />
    </Card>
  {/if}

  {#if standings && standings.length > 0}
    <Card title="Standings" pad={false}>
      <StandingsTable entries={standings} caption="Overall ranking" />
    </Card>
  {/if}

  {#if bracket && bracket.rounds.length > 0}
    <Card title="Bracket">
      <BracketTree {bracket} />
    </Card>
  {/if}

  {#if !heatResult && !(standings && standings.length) && !(bracket && bracket.rounds.length)}
    <Card elevation="flat">
      <p class="empty">No results yet. They appear here as heats are scored.</p>
    </Card>
  {/if}
</section>

<style>
  .results {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  h2 {
    font-size: var(--gf-font-size-xl);
    margin: 0;
    letter-spacing: var(--gf-tracking-tight);
  }
  .empty {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
</style>
