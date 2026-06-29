<script lang="ts">
  import type { MultiMain } from './multiMain.js';
  import { medalFor } from './format.js';

  /**
   * MultiMainView — a multi-main (tiered finals) view: the aggregate standings table.
   *
   * Source type: the local {@link MultiMain} view-model (see `multiMain.ts` for why this is a
   * component-local type and how a caller derives it from heats + `HeatResult`s + the canonical
   * `roundRanking`). The standings show Pos | Pilot | Tier (best first, by the ranking) — the **Tier**
   * column surfaces which main (A-Main, B-Main, …) each pilot raced in, so the structure is visible.
   * Every name is the resolved friendly label — no raw refs reach the screen. (The round's individual
   * mains are listed in the normal Heats area, so no per-main heat grid is shown here.)
   */
  let { view, caption }: { view: MultiMain; caption?: string } = $props();
</script>

<div class="gridfpv-multi-main">
  <table class="mm-standings" aria-label={caption ?? 'Multi-main standings'}>
    {#if caption}
      <caption>{caption}</caption>
    {/if}
    <thead>
      <tr>
        <th scope="col" class="pos">Pos</th>
        <th scope="col" class="pilot">Pilot</th>
        <th scope="col" class="tier">Tier</th>
      </tr>
    </thead>
    <tbody>
      {#each view.standings as row (row.competitor ?? row.label)}
        {@const medal = medalFor(row.position)}
        <tr class:medal={medal !== null} data-medal={medal}>
          <td class="pos"><span class="badge">{row.position}</span></td>
          <td class="pilot">{row.label}</td>
          <td class="tier">{row.tierName ?? '—'}</td>
        </tr>
      {/each}
      {#if view.standings.length === 0}
        <tr>
          <td class="empty" colspan="3">Standings appear as the mains are scored.</td>
        </tr>
      {/if}
    </tbody>
  </table>
</div>

<style>
  .gridfpv-multi-main {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
  }

  /* ── Standings table ── */
  .mm-standings {
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
  .mm-standings th,
  .mm-standings td {
    padding: var(--gf-space-3);
    text-align: left;
  }
  .mm-standings thead th {
    color: var(--gf-text-muted);
    font-weight: var(--gf-font-weight-semibold);
    font-size: var(--gf-font-size-2xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    border-bottom: 1px solid var(--gf-border);
  }
  .mm-standings tbody tr + tr td {
    border-top: 1px solid var(--gf-border-subtle);
  }
  .mm-standings .tier {
    color: var(--gf-text-secondary);
    font-variant-numeric: tabular-nums;
  }
  .mm-standings .pilot {
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
  }
  .mm-standings .pos {
    width: 2.75em;
  }
  .mm-standings .empty {
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
