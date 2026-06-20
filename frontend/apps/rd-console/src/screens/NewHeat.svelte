<script lang="ts">
  /**
   * New-heat form (#13, v0.4 Director wiring) — define a heat's field directly.
   *
   * The built-in sim source reports **no** seen competitors, so a basic race can't lean on
   * a "seen competitors" handshake (see `lib/heat.ts`). Instead the RD types a heat id and
   * a few pilot names here; on "Schedule heat" the form emits one `Register` per pilot
   * (binding the source-local ref to the typed pilot name) then a single `ScheduleHeat`
   * with that lineup. The heat then shows up on the live screen, ready to Stage → Start.
   *
   * A failed `CommandAck` (e.g. a duplicate heat id) surfaces via the shared `ErrorBanner`
   * and aborts the rest of the batch so the RD can correct and retry.
   */
  import type { HeatId } from '@gridfpv/types';
  import {
    defineHeatCommands,
    isHeatValid,
    validateHeat,
    type HeatDefinition
  } from '../lib/heat.js';
  import type { Session } from '../lib/session.svelte.js';

  let {
    session,
    onscheduled = undefined
  }: { session: Session; onscheduled?: (heat: HeatId) => void } = $props();

  let heatId = $state('q-1');
  // Pilot rows as objects (not bare strings) so each row is a stable, reactive `$state`
  // entry `bind:value` can write through without re-creating the input on every keystroke.
  let names = $state<{ name: string }[]>([{ name: '' }, { name: '' }]);
  let scheduling = $state(false);

  const def = $derived<HeatDefinition>({ heat: heatId, pilots: names });
  const problems = $derived(validateHeat(def));
  const ready = $derived(isHeatValid(def));

  function addPilot() {
    names = [...names, { name: '' }];
  }
  function removePilot(i: number) {
    names = names.filter((_, j) => j !== i);
  }

  async function schedule() {
    if (!ready || scheduling) return;
    scheduling = true;
    try {
      for (const command of defineHeatCommands(def)) {
        const ack = await session.send(command);
        if (!ack.ok) return; // ErrorBanner shows session.lastCommandError; stop the batch.
      }
      onscheduled?.(heatId.trim());
    } finally {
      scheduling = false;
    }
  }
</script>

<section class="new-heat" aria-label="New heat">
  <h3>New heat</h3>
  <p class="muted">
    Type the heat id and the pilots flying it. Each name is registered on
    <code>sim</code> and becomes the heat's lineup.
  </p>

  <label class="heat-id">
    Heat id
    <input type="text" bind:value={heatId} placeholder="q-1" aria-label="Heat id" />
  </label>

  <div class="pilots">
    <span class="pilots-label">Pilots</span>
    {#each names as pilot, i (i)}
      <div class="pilot-row">
        <input
          type="text"
          bind:value={pilot.name}
          placeholder={`Pilot ${i + 1}`}
          aria-label={`Pilot ${i + 1} name`}
        />
        <button
          type="button"
          class="remove"
          onclick={() => removePilot(i)}
          disabled={names.length <= 1}
          aria-label={`Remove pilot ${i + 1}`}>Remove</button
        >
      </div>
    {/each}
    <button type="button" class="add" onclick={addPilot}>Add pilot</button>
  </div>

  {#if problems.length > 0}
    <ul class="problems">
      {#each problems as p (p)}<li>{p}</li>{/each}
    </ul>
  {/if}

  <button type="button" class="schedule" onclick={schedule} disabled={!ready || scheduling}>
    {scheduling ? 'Scheduling…' : 'Schedule heat'}
  </button>
</section>

<style>
  .new-heat {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
    padding: var(--gf-space-4);
    border: 1px solid var(--gf-color-border);
    border-radius: var(--gf-radius-md);
    background: var(--gf-color-surface);
  }
  h3 {
    margin: 0;
    font-size: var(--gf-font-size-md);
  }
  .muted {
    color: var(--gf-color-text-muted);
    font-size: var(--gf-font-size-sm);
    margin: 0;
  }
  label,
  .pilots-label {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
    font-size: var(--gf-font-size-sm);
  }
  .pilots {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .pilot-row {
    display: flex;
    gap: var(--gf-space-2);
    align-items: center;
  }
  input {
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    padding: var(--gf-space-1) var(--gf-space-2);
    border: 1px solid var(--gf-color-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-color-surface);
    color: var(--gf-color-text);
  }
  .heat-id input {
    max-width: 12rem;
  }
  button {
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    padding: var(--gf-space-2) var(--gf-space-4);
    border-radius: var(--gf-radius-sm);
    border: 1px solid var(--gf-color-border);
    background: var(--gf-color-surface);
    color: var(--gf-color-text);
    cursor: pointer;
  }
  button:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  .schedule {
    align-self: flex-start;
    border-color: var(--gf-color-accent);
    background: var(--gf-color-accent);
    color: var(--gf-color-accent-contrast);
  }
  .problems {
    color: var(--gf-color-danger);
    font-size: var(--gf-font-size-sm);
    margin: 0;
    padding-left: var(--gf-space-4);
  }
</style>
