<script lang="ts">
  /**
   * PhaseList — every phase of the event as a tree: a phase that holds phases, with its
   * heats indented beneath it. Each row shows the phase's name and where it stands. A heat
   * also shows its lineup, and a button that opens its Run view.
   */
  import { Badge, Button } from '@gridfpv/components';
  import type { EventView, PhaseView } from '@gridfpv/types';
  import type { Names } from '../lib/names.js';
  import {
    isHeat,
    PHASE_STATUS_LABELS,
    phaseStatus,
    phaseTree,
    racesOfHeat,
    type PhaseStatus
  } from '../lib/phases.js';

  let {
    view,
    names,
    onrun
  }: {
    view: EventView;
    names: Names;
    /** Open the Run view for a heat. */
    onrun: (heat: PhaseView['def']['id']) => void;
  } = $props();

  const rows = $derived(phaseTree(view.phases));

  const TONES: Record<PhaseStatus, 'neutral' | 'accent' | 'success' | 'warn' | 'info'> = {
    waiting: 'neutral',
    ready: 'accent',
    racing: 'warn',
    complete: 'success',
    locked: 'info'
  };
</script>

<ul class="phases" aria-label="Phases">
  {#each rows as { phase, depth } (phase.def.id)}
    {@const status = phaseStatus(phase, view)}
    {@const heat = isHeat(phase.def)}
    <li class="phase" class:heat style="--depth: {depth}">
      <div class="head">
        <span class="label">{phase.def.label}</span>
        <Badge tone={TONES[status]} dot={status === 'racing'}>{PHASE_STATUS_LABELS[status]}</Badge>
        {#if heat}
          <span class="action">
            {#if status === 'ready' || status === 'racing'}
              <Button
                size="sm"
                variant="primary"
                aria-label="Run {phase.def.label}"
                onclick={() => onrun(phase.def.id)}>Run</Button
              >
            {:else if racesOfHeat(view, phase.def.id).length > 0 || status === 'complete'}
              <Button
                size="sm"
                variant="ghost"
                aria-label="View {phase.def.label}"
                onclick={() => onrun(phase.def.id)}>View</Button
              >
            {/if}
          </span>
        {/if}
      </div>
      {#if heat && phase.lineup.length > 0}
        <ol class="lineup" aria-label="Lineup of {phase.def.label}">
          {#each phase.lineup as seat, index (index)}
            <li class:pending={typeof seat.who === 'string' || 'Waiting' in seat.who}>
              <span class="seat">{names.seat(seat.adapter, seat.seat, index)}</span>
              <span class="who">{names.who(seat.who)}</span>
            </li>
          {/each}
        </ol>
      {/if}
    </li>
  {/each}
</ul>

<style>
  .phases {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .phase {
    margin-left: calc(var(--depth) * var(--gf-space-6));
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-border-subtle);
    border-left: 3px solid var(--gf-border-strong);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .phase.heat {
    border-left-color: var(--gf-accent);
    background: var(--gf-surface);
  }
  .head {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
  }
  .label {
    font-weight: var(--gf-font-weight-semibold);
    overflow-wrap: anywhere;
  }
  .action {
    margin-left: auto;
  }
  .lineup {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-1) var(--gf-space-4);
    font-size: var(--gf-font-size-sm);
  }
  .lineup li {
    display: flex;
    gap: var(--gf-space-2);
    align-items: baseline;
  }
  .seat {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .pending .who {
    color: var(--gf-text-muted);
    font-style: italic;
  }
  @media (max-width: 36rem) {
    .phase {
      margin-left: calc(var(--depth) * var(--gf-space-3));
    }
  }
</style>
