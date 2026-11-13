<script lang="ts">
  /**
   * ClassStandingsDialog — choose which phases count toward a class's standings, and in what
   * order. The first one decides. A later one is only used for pilots the first leaves
   * unplaced or level (for example, pilots who never made a main are ordered by qualifying).
   * It sends one `StandingsSet` fact. The parent mounts this only while it is open.
   */
  import { Banner, Button, Dialog } from '@gridfpv/components';
  import type { PhaseId, RosterView } from '@gridfpv/types';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import { ordinal } from '../lib/names.js';
  import { countablePhases } from '../lib/rulings.js';

  let {
    live,
    roster,
    onclose
  }: {
    live: EventLive;
    roster: RosterView;
    onclose: () => void;
  } = $props();

  const names = $derived(live.names);
  // The list starts from the phases counted now.
  // svelte-ignore state_referenced_locally
  let chosen = $state<PhaseId[]>([...roster.counted_phases]);
  const available = $derived(
    countablePhases(live.view?.phases ?? []).filter((phase) => !chosen.includes(phase.def.id))
  );
  let busy = $state(false);
  let error = $state<string | undefined>(undefined);

  function moveUp(index: number) {
    if (index === 0) return;
    const next = [...chosen];
    [next[index - 1], next[index]] = [next[index], next[index - 1]];
    chosen = next;
  }

  async function submit() {
    if (busy) return;
    busy = true;
    error = undefined;
    try {
      if (await live.send({ Record: { StandingsSet: { class: roster.class, phases: chosen } } })) {
        onclose();
      }
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  const title = $derived(`What counts for ${names.klass(roster.class)}`);
</script>

<Dialog open={true} {title} {onclose}>
  <div class="body" aria-label={title}>
    <p>
      Choose the phases that count, the deciding one first. A later phase only places pilots the
      ones before it left unplaced or level.
    </p>
    {#if chosen.length > 0}
      <ol class="chosen" aria-label="Phases that count">
        {#each chosen as id, index (id)}
          {@const label = names.phase(id)}
          <li>
            <span class="slot">{ordinal(index + 1)}</span>
            <span class="label">{label}</span>
            <button
              type="button"
              class="mini"
              disabled={busy || index === 0}
              aria-label="Move {label} up"
              onclick={() => moveUp(index)}>↑</button
            >
            <button
              type="button"
              class="mini"
              disabled={busy}
              aria-label="Remove {label}"
              onclick={() => (chosen = chosen.filter((other) => other !== id))}>×</button
            >
          </li>
        {/each}
      </ol>
    {:else}
      <p class="empty">No phase counts yet, so nobody is placed.</p>
    {/if}
    {#if available.length > 0}
      <div class="available" role="group" aria-label="Phases to choose from">
        {#each available as phase (phase.def.id)}
          <button
            type="button"
            class="chip"
            disabled={busy}
            onclick={() => (chosen = [...chosen, phase.def.id])}>{phase.def.label}</button
          >
        {/each}
      </div>
    {:else if chosen.length === 0}
      <p class="empty">The event has no ranked phase yet. Add a heat or a bracket first.</p>
    {/if}
    {#if error}
      <Banner tone="danger">{error}</Banner>
    {/if}
  </div>
  {#snippet footer()}
    <Button variant="ghost" onclick={onclose} disabled={busy}>Cancel</Button>
    <Button variant="primary" onclick={submit} disabled={busy}>Save</Button>
  {/snippet}
</Dialog>

<style>
  .body {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .body p {
    margin: 0;
    color: var(--gf-text-secondary);
  }
  .body p.empty {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .chosen {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
  }
  .chosen li {
    display: grid;
    grid-template-columns: 3rem 1fr auto auto;
    gap: var(--gf-space-2);
    align-items: center;
    padding: var(--gf-space-1) var(--gf-space-2);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
  }
  .slot {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .label {
    font-weight: var(--gf-font-weight-semibold);
    overflow-wrap: anywhere;
  }
  .mini {
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-xs);
    background: transparent;
    color: var(--gf-text-muted);
    cursor: pointer;
    font: inherit;
    line-height: 1;
    padding: var(--gf-space-1) var(--gf-space-2);
  }
  .mini:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .available {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-2);
  }
  .chip {
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-pill);
    background: var(--gf-elevated);
    color: var(--gf-text);
    cursor: pointer;
    font: inherit;
    font-size: var(--gf-font-size-sm);
    padding: var(--gf-space-1) var(--gf-space-3);
  }
  .chip:hover:not(:disabled) {
    border-color: var(--gf-accent);
  }
  .mini:focus-visible,
  .chip:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
</style>
