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
    background: var(--gf-elevated);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    box-shadow: var(--gf-shadow-xs);
    transition:
      border-color var(--gf-motion-fast) var(--gf-ease-out),
      transform var(--gf-motion-fast) var(--gf-ease-out);
  }
  .gridfpv-pilot-card:hover {
    border-color: var(--gf-border-strong);
  }
  .position {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 2em;
    min-height: 2em;
    border-radius: var(--gf-radius-md);
    background: var(--gf-accent);
    color: var(--gf-accent-contrast);
    font-weight: var(--gf-font-weight-bold);
    font-size: var(--gf-font-size-lg);
    font-variant-numeric: tabular-nums;
    box-shadow: 0 0 16px -4px var(--gf-accent-ring);
  }
  .body {
    flex: 1;
  }
  .name {
    display: block;
    font-weight: var(--gf-font-weight-semibold);
    font-size: var(--gf-font-size-md);
    letter-spacing: var(--gf-tracking-tight);
  }
  .stats {
    display: flex;
    gap: var(--gf-space-6);
    margin: var(--gf-space-1) 0 0;
  }
  .stat {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  dt {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-2xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    font-weight: var(--gf-font-weight-semibold);
  }
  dd {
    margin: 0;
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
    font-weight: var(--gf-font-weight-medium);
    color: var(--gf-text-secondary);
  }
</style>
