<script lang="ts">
  /**
   * PilotPicker — choose pilots from the directory, in order.
   *
   * The order matters: it is the seat order for a heat and the seed order for a bracket.
   * Click a pilot to add them to the end of the list. The chosen list shows each pilot
   * beside the place they hold (`slotLabel`), with buttons to move them up or take them out.
   */
  import type { Pilot, PilotId } from '@gridfpv/types';

  let {
    pilots,
    chosen = $bindable([]),
    max = undefined,
    slotLabel,
    disabled = false
  }: {
    /** The pilot directory. */
    pilots: readonly Pilot[];
    /** The chosen pilots, in order. */
    chosen?: PilotId[];
    /** The most pilots that can be chosen, when there is a limit. */
    max?: number;
    /** The name of the place at `index` (from 0): "Seat 1", "Seed 1". */
    slotLabel: (index: number) => string;
    disabled?: boolean;
  } = $props();

  const byId = $derived(new Map(pilots.map((pilot) => [pilot.id, pilot])));
  const available = $derived(
    pilots
      .filter((pilot) => !chosen.includes(pilot.id))
      .sort((a, b) => a.callsign.localeCompare(b.callsign))
  );
  const full = $derived(max !== undefined && chosen.length >= max);

  function add(id: PilotId) {
    if (full || chosen.includes(id)) return;
    chosen = [...chosen, id];
  }

  function remove(id: PilotId) {
    chosen = chosen.filter((other) => other !== id);
  }

  function moveUp(index: number) {
    if (index === 0) return;
    const next = [...chosen];
    [next[index - 1], next[index]] = [next[index], next[index - 1]];
    chosen = next;
  }
</script>

<div class="picker">
  {#if pilots.length === 0}
    <p class="empty">There are no pilots yet. Add pilots on the Pilots page first.</p>
  {:else}
    {#if chosen.length > 0}
      <ol class="chosen" aria-label="Chosen pilots">
        {#each chosen as id, index (id)}
          {@const callsign = byId.get(id)?.callsign ?? 'Unknown pilot'}
          <li>
            <span class="slot">{slotLabel(index)}</span>
            <span class="callsign">{callsign}</span>
            <button
              type="button"
              class="mini"
              disabled={disabled || index === 0}
              aria-label="Move {callsign} up"
              onclick={() => moveUp(index)}>↑</button
            >
            <button
              type="button"
              class="mini"
              {disabled}
              aria-label="Remove {callsign}"
              onclick={() => remove(id)}>×</button
            >
          </li>
        {/each}
      </ol>
    {:else}
      <p class="empty">Nobody chosen yet. Click pilots below in the order you want them.</p>
    {/if}

    {#if available.length > 0}
      <div class="available" role="group" aria-label="Pilots to choose from">
        {#each available as pilot (pilot.id)}
          <button
            type="button"
            class="chip"
            disabled={disabled || full}
            onclick={() => add(pilot.id)}>{pilot.callsign}</button
          >
        {/each}
      </div>
    {/if}
    {#if full && available.length > 0}
      <p class="empty">That is the most this can hold.</p>
    {/if}
  {/if}
</div>

<style>
  .picker {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .empty {
    margin: 0;
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
    grid-template-columns: 5rem 1fr auto auto;
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
  .callsign {
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
  .mini:hover:not(:disabled) {
    color: var(--gf-text);
    border-color: var(--gf-border-strong);
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
  .chip:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .mini:focus-visible,
  .chip:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
</style>
