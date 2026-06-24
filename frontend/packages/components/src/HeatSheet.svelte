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
   * `names` optionally maps a `CompetitorRef` to a display name; `heatName` optionally overrides the
   * heading (otherwise the raw `current_heat` id shows — callers with the round context pass the
   * friendly "<Round> Heat N" name).
   */
  let {
    state,
    /** Optional display names keyed by source-local competitor ref. */
    names = {},
    /** Optional friendly heading for the heat (e.g. "Qualifying Heat 1"); falls back to the id. */
    heatName
  }: { state: LiveRaceState; names?: Record<CompetitorRef, string>; heatName?: string } = $props();

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
    <h3>{heatName ?? state.current_heat ?? 'No heat'}</h3>
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
    background: var(--gf-elevated);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    overflow: hidden;
    box-shadow: var(--gf-shadow-xs);
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--gf-space-3) var(--gf-space-4);
    border-bottom: 1px solid var(--gf-border-subtle);
    background: var(--gf-surface-alt);
  }
  h3 {
    margin: 0;
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
  }
  .phase {
    --_c: var(--gf-phase-scheduled);
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    padding: 0.18em 0.65em;
    border-radius: var(--gf-radius-pill);
    background: color-mix(in srgb, var(--_c) 14%, transparent);
    color: color-mix(in srgb, var(--_c) 85%, var(--gf-text));
    border: 1px solid color-mix(in srgb, var(--_c) 35%, transparent);
    font-size: var(--gf-font-size-2xs);
    font-weight: var(--gf-font-weight-semibold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-wide);
  }
  .phase[data-phase='Scheduled'] {
    --_c: var(--gf-phase-scheduled);
  }
  .phase[data-phase='Staged'] {
    --_c: var(--gf-phase-staged);
  }
  .phase[data-phase='Armed'] {
    --_c: var(--gf-phase-armed);
  }
  .phase[data-phase='Running'] {
    --_c: var(--gf-phase-running);
  }
  .phase[data-phase='Unofficial'] {
    --_c: var(--gf-phase-finished);
  }
  .phase[data-phase='Final'] {
    --_c: var(--gf-phase-scored);
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
    padding: var(--gf-space-3) var(--gf-space-4);
    transition: background var(--gf-motion-fast) var(--gf-ease-out);
  }
  .lineup li:hover {
    background: var(--gf-accent-soft);
  }
  .lineup li + li {
    border-top: 1px solid var(--gf-border-subtle);
  }
  .place {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 1.6em;
    height: 1.6em;
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-alt);
    font-weight: var(--gf-font-weight-bold);
    font-variant-numeric: tabular-nums;
    color: var(--gf-text-secondary);
  }
  .pilot {
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
  }
  .laps,
  .last-lap {
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
    text-align: right;
  }
  .laps {
    color: var(--gf-text-secondary);
  }
  .last-lap {
    color: var(--gf-text-muted);
  }
</style>
