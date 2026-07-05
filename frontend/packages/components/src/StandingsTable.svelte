<script lang="ts">
  import type { RankEntry } from '@gridfpv/types';
  import { medalFor } from './format.js';

  /**
   * StandingsTable — a generator's overall ranking across an event/class.
   *
   * Source type: `RankEntry[]` (1-based, tie-aware positions, best first). Unlike
   * a single-heat `Leaderboard`, this is the rolled-up standing, so it shows just
   * position + competitor; surfaces add their own context columns later.
   */
  let {
    entries,
    /** Optional table caption (e.g. "Open class — overall"). */
    caption
  }: { entries: RankEntry[]; caption?: string } = $props();
</script>

<table class="gridfpv-standings" aria-label={caption ?? 'Overall standings'}>
  {#if caption}
    <caption>{caption}</caption>
  {/if}
  <thead>
    <tr>
      <th scope="col" class="pos">Pos</th>
      <th scope="col" class="pilot">Pilot</th>
    </tr>
  </thead>
  <tbody>
    {#each entries as entry (entry.competitor)}
      {@const medal = medalFor(entry.position)}
      <tr class:medal={medal !== null} data-medal={medal}>
        <td class="pos"><span class="badge">{entry.position}</span></td>
        <td class="pilot">{entry.competitor}</td>
      </tr>
    {/each}
  </tbody>
</table>

<style>
  .gridfpv-standings {
    border-collapse: collapse;
    width: 100%;
    color: var(--gf-text);
    background: var(--gf-elevated);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
  }
  caption {
    text-align: left;
    padding: var(--gf-space-3) var(--gf-space-3);
    color: var(--gf-text-muted);
    font-weight: var(--gf-font-weight-semibold);
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
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
