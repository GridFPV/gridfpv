<script lang="ts">
  import type { RoundRobin } from './roundRobin.js';
  import { formatMicros, medalFor } from './format.js';

  /**
   * RoundRobinView — a round-robin tournament view: an aggregate standings table + a per-rotation
   * grid of heats.
   *
   * Source type: the local {@link RoundRobin} view-model (see `roundRobin.ts` for why this is a
   * component-local type and how a caller derives it from heats + `HeatResult`s + the canonical
   * `roundRanking`). The standings show Pos | Pilot | Points | Best lap (best first, by the ranking);
   * the grid groups the round's heats by rotation, each heat listing its lineup in finishing order
   * (when scored). Every name is the resolved friendly label — no raw refs reach the screen.
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

  {#if view.rotations.length > 0}
    <div class="rr-grid" role="group" aria-label="Rotations">
      {#each view.rotations as rotation (rotation.name)}
        <section class="rotation" role="group" aria-label={rotation.name}>
          <h4 class="rotation-name">{rotation.name}</h4>
          <div class="rotation-heats">
            {#each rotation.heats as heat, hi (heat.heat ?? rotation.name + '-' + hi)}
              <div class="rr-heat" role="group" aria-label={heat.name}>
                <h5 class="rr-heat-name">{heat.name}</h5>
                <ol class="rr-lineup">
                  {#each heat.entries as entry, ei (entry.competitor ?? ei)}
                    <li class="rr-seat">
                      <span class="rr-place" aria-hidden="true">{entry.position ?? ei + 1}</span>
                      <span class="rr-call">{entry.label}</span>
                    </li>
                  {/each}
                  {#if heat.entries.length === 0}
                    <li class="rr-seat empty">— no pilots —</li>
                  {/if}
                </ol>
              </div>
            {/each}
          </div>
        </section>
      {/each}
    </div>
  {/if}
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

  /* ── Per-rotation grid ── */
  .rr-grid {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .rotation {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .rotation-name {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-2xs);
    font-weight: var(--gf-font-weight-semibold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
  }
  .rotation-heats {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(12rem, 1fr));
    gap: var(--gf-space-3);
  }
  .rr-heat {
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    overflow: hidden;
    background: var(--gf-elevated);
    box-shadow: var(--gf-shadow-xs);
  }
  .rr-heat-name {
    margin: 0;
    padding: var(--gf-space-2) var(--gf-space-3);
    font-size: var(--gf-font-size-2xs);
    font-weight: var(--gf-font-weight-semibold);
    color: var(--gf-text-muted);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    border-bottom: 1px solid var(--gf-border-subtle);
    background: var(--gf-surface-sunken);
  }
  .rr-lineup {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .rr-seat {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    padding: var(--gf-space-2) var(--gf-space-3);
    font-size: var(--gf-font-size-sm);
  }
  .rr-seat + .rr-seat {
    border-top: 1px solid var(--gf-border-subtle);
  }
  .rr-seat.empty {
    color: var(--gf-text-muted);
  }
  .rr-place {
    display: inline-grid;
    place-items: center;
    width: 1.5rem;
    height: 1.5rem;
    flex-shrink: 0;
    border-radius: var(--gf-radius-xs);
    background: var(--gf-surface-alt, var(--gf-surface-sunken));
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-2xs);
    font-variant-numeric: tabular-nums;
  }
  .rr-call {
    font-weight: var(--gf-font-weight-medium);
    color: var(--gf-text);
  }
</style>
