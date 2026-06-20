<script lang="ts">
  import type { PilotProgress } from '@gridfpv/types';
  import { formatMicros } from './format.js';

  /**
   * PilotCard — one pilot's identity + live progress in the current heat.
   *
   * Source type: `PilotProgress` (`competitor`, `laps_completed`,
   * `last_lap_micros`). An optional `name` overrides the source-local
   * `competitor` ref for display, and `position` (the live running-order place,
   * if known) renders as a leading badge.
   */
  let {
    progress,
    /** Display name; falls back to the source-local competitor ref. */
    name,
    /** Live running-order position, if known. */
    position
  }: { progress: PilotProgress; name?: string; position?: number } = $props();

  let label = $derived(name ?? progress.competitor);
</script>

<article class="gridfpv-pilot-card" aria-label={`Pilot ${label}`}>
  {#if position !== undefined}
    <span class="position" aria-label={`Position ${position}`}>{position}</span>
  {/if}
  <div class="body">
    <span class="name">{label}</span>
    <dl class="stats">
      <div class="stat">
        <dt>Laps</dt>
        <dd class="laps">{progress.laps_completed}</dd>
      </div>
      <div class="stat">
        <dt>Last lap</dt>
        <dd class="last-lap">{formatMicros(progress.last_lap_micros)}</dd>
      </div>
    </dl>
  </div>
</article>

<style>
  .gridfpv-pilot-card {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    padding: var(--gf-space-3) var(--gf-space-4);
    background: var(--gf-color-surface);
    border: 1px solid var(--gf-color-border);
    border-radius: var(--gf-radius-md);
    color: var(--gf-color-text);
    font-family: var(--gf-font-family);
  }
  .position {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 1.8em;
    min-height: 1.8em;
    border-radius: var(--gf-radius-sm);
    background: var(--gf-color-leader);
    color: var(--gf-color-accent-contrast);
    font-weight: var(--gf-font-weight-bold);
    font-size: var(--gf-font-size-lg);
  }
  .body {
    flex: 1;
  }
  .name {
    display: block;
    font-weight: var(--gf-font-weight-medium);
    font-size: var(--gf-font-size-md);
  }
  .stats {
    display: flex;
    gap: var(--gf-space-6);
    margin: var(--gf-space-1) 0 0;
  }
  .stat {
    display: flex;
    flex-direction: column;
  }
  dt {
    color: var(--gf-color-text-muted);
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  dd {
    margin: 0;
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
    font-weight: var(--gf-font-weight-medium);
  }
</style>
