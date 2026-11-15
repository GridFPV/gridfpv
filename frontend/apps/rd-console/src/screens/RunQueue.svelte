<script lang="ts">
  /**
   * RunQueue — "Up next": the heats that can be raced now, in the suggested order, each with
   * its lineup by callsign and a Run button. The arrows move a heat up or down; each move
   * sends the whole new order to the Director.
   *
   * The order is only a suggestion. Any heat here can be run at any time.
   */
  import { Button } from '@gridfpv/components';
  import type { EventView, PhaseId, ProtocolError } from '@gridfpv/types';
  import ErrorBanner from '../lib/ErrorBanner.svelte';
  import { toProtocolError, type EventLive } from '../lib/eventLive.svelte.js';
  import type { Names } from '../lib/names.js';
  import { moveInQueue } from '../lib/rulings.js';

  let {
    view,
    names,
    live,
    onrun
  }: {
    view: EventView;
    names: Names;
    live: Pick<EventLive, 'send'>;
    /** Open the Run view for a heat. */
    onrun: (heat: PhaseId) => void;
  } = $props();

  const heats = $derived(
    view.queue.flatMap((id) => {
      const phase = view.phases.find((p) => p.def.id === id);
      return phase ? [phase] : [];
    })
  );

  let busy = $state(false);
  let error = $state<ProtocolError | undefined>(undefined);

  async function move(index: number, step: -1 | 1) {
    if (busy) return;
    busy = true;
    error = undefined;
    try {
      await live.send({ Record: { QueueSet: { heats: moveInQueue(view.queue, index, step) } } });
    } catch (e) {
      error = toProtocolError(e);
    } finally {
      busy = false;
    }
  }
</script>

<ErrorBanner {error} ondismiss={() => (error = undefined)} />
{#if heats.length === 0}
  <p class="muted">No heat is ready to race.</p>
{:else}
  <p class="muted">The order is a suggestion. You can run any of these heats at any time.</p>
  <ol class="queue" aria-label="Up next">
    {#each heats as phase, index (phase.def.id)}
      <li aria-label={phase.def.label}>
        <span class="n">{index + 1}</span>
        <span class="what">
          <span class="label">{phase.def.label}</span>
          <span class="lineup">
            {phase.lineup
              .filter((seat) => seat.who !== 'Nobody')
              .map((seat) => names.who(seat.who))
              .join(', ')}
          </span>
        </span>
        <span class="moves">
          <button
            type="button"
            class="mini"
            disabled={busy || index === 0}
            aria-label="Move {phase.def.label} up"
            onclick={() => move(index, -1)}>↑</button
          >
          <button
            type="button"
            class="mini"
            disabled={busy || index === heats.length - 1}
            aria-label="Move {phase.def.label} down"
            onclick={() => move(index, 1)}>↓</button
          >
          <Button
            size="sm"
            variant={index === 0 ? 'primary' : 'secondary'}
            aria-label="Run {phase.def.label} next"
            onclick={() => onrun(phase.def.id)}>Run</Button
          >
        </span>
      </li>
    {/each}
  </ol>
{/if}

<style>
  .muted {
    margin: 0 0 var(--gf-space-3);
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .queue {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .queue li {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface);
  }
  .n {
    min-width: 1.5rem;
    font-family: var(--gf-font-mono);
    font-weight: var(--gf-font-weight-bold);
    color: var(--gf-text-muted);
  }
  .what {
    display: grid;
    gap: 2px;
    min-width: 0;
    flex: 1;
  }
  .label {
    font-weight: var(--gf-font-weight-semibold);
    overflow-wrap: anywhere;
  }
  .lineup {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-secondary);
    overflow-wrap: anywhere;
  }
  .moves {
    display: flex;
    align-items: center;
    gap: var(--gf-space-1);
  }
  .mini {
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-xs);
    background: transparent;
    color: var(--gf-text-muted);
    cursor: pointer;
    font: inherit;
    line-height: 1;
    padding: var(--gf-space-2);
  }
  .mini:hover:not(:disabled) {
    color: var(--gf-text);
    border-color: var(--gf-border-strong);
  }
  .mini:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .mini:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
</style>
