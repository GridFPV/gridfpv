<script lang="ts">
  /**
   * Results (#56) — heat results, rankings/standings, bracket, and export.
   *
   * Renders typed projection data through the shared components: a `HeatResult` →
   * `Leaderboard`, a `RankEntry[]` → `StandingsTable`, and an `EventOutcome`'s bracket
   * heats → `BracketTree` (via the `bracketFromOutcome` view-model derivation). Export
   * is a lossless JSON download of whichever projection is shown.
   */
  import { Leaderboard, StandingsTable, BracketTree } from '@gridfpv/components';
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
    <button type="button" class="export" onclick={exportAll}>Export JSON</button>
  </header>

  {#if heatResult}
    <div class="block">
      <h3>Heat result</h3>
      <Leaderboard result={heatResult} {metricLabel} />
    </div>
  {/if}

  {#if standings && standings.length > 0}
    <div class="block">
      <h3>Standings</h3>
      <StandingsTable entries={standings} caption="Overall ranking" />
    </div>
  {/if}

  {#if bracket && bracket.rounds.length > 0}
    <div class="block">
      <h3>Bracket</h3>
      <BracketTree {bracket} />
    </div>
  {/if}

  {#if !heatResult && !(standings && standings.length) && !(bracket && bracket.rounds.length)}
    <p class="empty">No results yet. They appear here as heats are scored.</p>
  {/if}
</section>

<style>
  .results {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-6);
  }
  .head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
  }
  h2 {
    font-size: var(--gf-font-size-lg);
    margin: 0;
  }
  h3 {
    font-size: var(--gf-font-size-md);
    margin: 0 0 var(--gf-space-3);
  }
  .export {
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    padding: var(--gf-space-2) var(--gf-space-4);
    border-radius: var(--gf-radius-sm);
    border: 1px solid var(--gf-color-accent);
    background: var(--gf-color-surface);
    color: var(--gf-color-accent);
    cursor: pointer;
  }
  .empty {
    color: var(--gf-color-text-muted);
    font-size: var(--gf-font-size-sm);
  }
</style>
