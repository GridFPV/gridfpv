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
    color: var(--gf-color-text);
    background: var(--gf-color-surface);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
  }
  caption {
    text-align: left;
    padding: var(--gf-space-2) var(--gf-space-3);
    color: var(--gf-color-text-muted);
    font-weight: var(--gf-font-weight-medium);
  }
  th,
  td {
    padding: var(--gf-space-2) var(--gf-space-3);
    text-align: left;
  }
  thead th {
    color: var(--gf-color-text-muted);
    font-weight: var(--gf-font-weight-medium);
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    border-bottom: 1px solid var(--gf-color-border);
  }
  tbody tr + tr td {
    border-top: 1px solid var(--gf-color-border);
  }
  .pos {
    width: 2.5em;
  }
  .badge {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 1.6em;
    padding: 0 0.3em;
    border-radius: var(--gf-radius-sm);
    background: var(--gf-color-surface-alt);
    font-weight: var(--gf-font-weight-bold);
  }
  tr.medal[data-medal='gold'] .badge {
    background: var(--gf-color-gold);
    color: #000;
  }
  tr.medal[data-medal='silver'] .badge {
    background: var(--gf-color-silver);
    color: #000;
  }
  tr.medal[data-medal='bronze'] .badge {
    background: var(--gf-color-bronze);
    color: #fff;
  }
  .pilot {
    font-weight: var(--gf-font-weight-medium);
  }
</style>
