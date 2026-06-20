<script lang="ts">
  import type { LiveRaceState, PilotProgress, CompetitorRef } from '@gridfpv/types';
  import { formatMicros } from './format.js';

  /**
   * HeatSheet — the current heat's lineup with each pilot's live lap progress.
   *
   * Source type: `LiveRaceState`. It reads `active_pilots` (the lineup, in
   * seeding order), `progress` (per-pilot `PilotProgress`, ordered like the
   * lineup), `running_order` (live ranking), and `phase`. Rows render in running
   * order when available, otherwise lineup order; a leading place number shows
   * the live standing.
   *
   * `names` optionally maps a `CompetitorRef` to a display name.
   */
  let {
    state,
    /** Optional display names keyed by source-local competitor ref. */
    names = {}
  }: { state: LiveRaceState; names?: Record<CompetitorRef, string> } = $props();

  const lineup = $derived(state.active_pilots ?? []);
  const progressByRef = $derived(
    new Map<CompetitorRef, PilotProgress>((state.progress ?? []).map((p) => [p.competitor, p]))
  );

  // Render in live running order when the projection provides it, else lineup.
  const order = $derived(
    state.running_order && state.running_order.length > 0 ? state.running_order : lineup
  );
</script>

<section class="gridfpv-heat-sheet" aria-label="Heat sheet">
  <header>
    <h3>{state.current_heat ?? 'No heat'}</h3>
    <span class="phase" data-phase={state.phase}>{state.phase}</span>
  </header>
  <ol class="lineup">
    {#each order as ref, i (ref)}
      {@const p = progressByRef.get(ref)}
      <li>
        <span class="place">{i + 1}</span>
        <span class="pilot">{names[ref] ?? ref}</span>
        <span class="laps">{p ? p.laps_completed : 0} laps</span>
        <span class="last-lap">{formatMicros(p?.last_lap_micros)}</span>
      </li>
    {/each}
  </ol>
</section>

<style>
  .gridfpv-heat-sheet {
    background: var(--gf-color-surface);
    border: 1px solid var(--gf-color-border);
    border-radius: var(--gf-radius-md);
    color: var(--gf-color-text);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    overflow: hidden;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--gf-space-3) var(--gf-space-4);
    border-bottom: 1px solid var(--gf-color-border);
    background: var(--gf-color-surface-alt);
  }
  h3 {
    margin: 0;
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-bold);
  }
  .phase {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--gf-color-text-muted);
  }
  .phase[data-phase='Running'] {
    color: var(--gf-color-live);
    font-weight: var(--gf-font-weight-bold);
  }
  .lineup {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .lineup li {
    display: grid;
    grid-template-columns: 2em 1fr auto auto;
    gap: var(--gf-space-3);
    align-items: center;
    padding: var(--gf-space-2) var(--gf-space-4);
  }
  .lineup li + li {
    border-top: 1px solid var(--gf-color-border);
  }
  .place {
    font-weight: var(--gf-font-weight-bold);
    color: var(--gf-color-text-muted);
  }
  .pilot {
    font-weight: var(--gf-font-weight-medium);
  }
  .laps,
  .last-lap {
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
    text-align: right;
  }
  .last-lap {
    color: var(--gf-color-text-muted);
  }
</style>
