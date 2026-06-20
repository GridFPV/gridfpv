<script lang="ts">
  import type { HeatResult } from '@gridfpv/types';
  import { formatMetric, medalFor } from './format.js';

  /**
   * Leaderboard — the scored standing of a single heat.
   *
   * Source type: `HeatResult` (`places: Placement[]`), best position first with
   * ties sharing a position. Each row shows finishing position, competitor, the
   * laps that counted, and the condition-specific deciding metric.
   */
  let {
    result,
    /** Optional column heading for the deciding metric (e.g. "Best lap"). */
    metricLabel = 'Metric'
  }: { result: HeatResult; metricLabel?: string } = $props();
</script>

<table class="gridfpv-leaderboard" aria-label="Heat leaderboard">
  <thead>
    <tr>
      <th scope="col" class="pos">Pos</th>
      <th scope="col" class="pilot">Pilot</th>
      <th scope="col" class="laps">Laps</th>
      <th scope="col" class="metric">{metricLabel}</th>
    </tr>
  </thead>
  <tbody>
    {#each result.places as place (place.competitor.adapter + '/' + place.competitor.competitor)}
      {@const medal = medalFor(place.position)}
      <tr class:medal={medal !== null} data-medal={medal}>
        <td class="pos"><span class="badge">{place.position}</span></td>
        <td class="pilot">{place.competitor.competitor}</td>
        <td class="laps">{place.laps}</td>
        <td class="metric">{formatMetric(place.metric)}</td>
      </tr>
    {/each}
  </tbody>
</table>

<style>
  .gridfpv-leaderboard {
    border-collapse: collapse;
    width: 100%;
    color: var(--gf-text);
    background: var(--gf-elevated);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
  }
  th,
  td {
    padding: var(--gf-space-3) var(--gf-space-3);
    text-align: left;
  }
  thead th {
    color: var(--gf-text-muted);
    font-weight: var(--gf-font-weight-semibold);
    font-size: var(--gf-font-size-2xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    border-bottom: 1px solid var(--gf-border);
  }
  tbody tr {
    transition: background var(--gf-motion-fast) var(--gf-ease-out);
  }
  tbody tr:hover {
    background: var(--gf-accent-soft);
  }
  tbody tr + tr td {
    border-top: 1px solid var(--gf-border-subtle);
  }
  .pos {
    width: 2.75em;
  }
  .laps,
  .metric {
    text-align: right;
    font-variant-numeric: tabular-nums;
    font-family: var(--gf-font-mono);
    color: var(--gf-text-secondary);
  }
  .badge {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 1.7em;
    height: 1.7em;
    padding: 0 0.35em;
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-alt);
    color: var(--gf-text-secondary);
    font-weight: var(--gf-font-weight-bold);
    font-variant-numeric: tabular-nums;
  }
  tr.medal[data-medal='gold'] .badge {
    background: var(--gf-color-gold);
    color: var(--gf-medal-gold-contrast);
    box-shadow: 0 0 12px -2px color-mix(in srgb, var(--gf-color-gold) 70%, transparent);
  }
  tr.medal[data-medal='silver'] .badge {
    background: var(--gf-color-silver);
    color: var(--gf-medal-silver-contrast);
  }
  tr.medal[data-medal='bronze'] .badge {
    background: var(--gf-color-bronze);
    color: var(--gf-medal-bronze-contrast);
  }
  .pilot {
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
  }
</style>
