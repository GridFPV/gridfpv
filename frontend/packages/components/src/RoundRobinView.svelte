<script lang="ts">
  import type { RoundRobin } from './roundRobin.js';
  import { formatMicros, medalFor } from './format.js';

  /**
   * RoundRobinView — a round-robin tournament view: the aggregate standings table.
   *
   * Source type: the local {@link RoundRobin} view-model (see `roundRobin.ts` for why this is a
   * component-local type and how a caller derives it from heats + `HeatResult`s + the canonical
   * `roundRanking`). The standings show Pos | Pilot | Points | Best lap (best first, by the ranking).
   * Every name is the resolved friendly label — no raw refs reach the screen. (The round's individual
   * heats are listed in the normal Heats area, so no per-rotation grid is shown here.)
   */
  let { view, caption }: { view: RoundRobin; caption?: string } = $props();
</script>

<div class="gridfpv-round-robin">
  <table class="rr-standings" aria-label={caption ?? 'Round-robin standings'}>
    {#if caption}
      <caption>{caption}</caption>
    {/if}
    <thead>
      <tr>
        <th scope="col" class="pos">Pos</th>
        <th scope="col" class="pilot">Pilot</th>
        <th scope="col" class="num">Points</th>
        <th scope="col" class="num">Best lap</th>
      </tr>
    </thead>
    <tbody>
      {#each view.standings as row (row.competitor ?? row.label)}
        {@const medal = medalFor(row.position)}
        <tr class:medal={medal !== null} data-medal={medal}>
          <td class="pos"><span class="badge">{row.position}</span></td>
          <td class="pilot">{row.label}</td>
          <td class="num points">{row.points}</td>
          <td class="num">{formatMicros(row.bestLapMicros)}</td>
        </tr>
      {/each}
      {#if view.standings.length === 0}
        <tr>
          <td class="empty" colspan="4">Standings appear as the round's heats are scored.</td>
        </tr>
      {/if}
    </tbody>
  </table>
</div>

<style>
  .gridfpv-round-robin {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
  }

  /* ── Standings table ── */
  .rr-standings {
    border-collapse: collapse;
    width: 100%;
    font-size: var(--gf-font-size-sm);
  }
  caption {
    text-align: left;
    padding: var(--gf-space-3);
    color: var(--gf-text-muted);
    font-weight: var(--gf-font-weight-semibold);
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
  }
  .rr-standings th,
  .rr-standings td {
    padding: var(--gf-space-3);
    text-align: left;
  }
  .rr-standings thead th {
    color: var(--gf-text-muted);
    font-weight: var(--gf-font-weight-semibold);
    font-size: var(--gf-font-size-2xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    border-bottom: 1px solid var(--gf-border);
  }
  .rr-standings tbody tr + tr td {
    border-top: 1px solid var(--gf-border-subtle);
  }
  .rr-standings .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .rr-standings .points {
    font-weight: var(--gf-font-weight-bold);
    color: var(--gf-text);
  }
  .rr-standings .pilot {
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
  }
  .rr-standings .pos {
    width: 2.75em;
  }
  .rr-standings .empty {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-xs);
  }
  .badge {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 1.7em;
    height: 1.7em;
    padding: 0 0.35em;
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-alt, var(--gf-surface-sunken));
    color: var(--gf-text-secondary);
    font-weight: var(--gf-font-weight-bold);
    font-variant-numeric: tabular-nums;
  }
  tr.medal[data-medal='gold'] .badge {
    background: var(--gf-color-gold);
    color: var(--gf-medal-gold-contrast);
  }
  tr.medal[data-medal='silver'] .badge {
    background: var(--gf-color-silver);
    color: var(--gf-medal-silver-contrast);
  }
  tr.medal[data-medal='bronze'] .badge {
    background: var(--gf-color-bronze);
    color: var(--gf-medal-bronze-contrast);
  }
</style>
