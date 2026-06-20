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
  import { Button, Input } from '@gridfpv/components';
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
  <header class="head">
    <h3>New heat</h3>
    <p class="muted">
      Type the heat id and the pilots flying it. Each name is registered on
      <code>sim</code> and becomes the heat's lineup.
    </p>
  </header>

  <label class="heat-id field">
    <span class="field-label">Heat id</span>
    <Input bind:value={heatId} placeholder="q-1" aria-label="Heat id" />
  </label>

  <div class="pilots">
    <span class="pilots-label field-label">Pilots</span>
    {#each names as pilot, i (i)}
      <div class="pilot-row">
        <span class="pilot-num" aria-hidden="true">{i + 1}</span>
        <Input
          bind:value={pilot.name}
          placeholder={`Pilot ${i + 1}`}
          aria-label={`Pilot ${i + 1} name`}
        />
        <Button
          variant="ghost"
          size="sm"
          onclick={() => removePilot(i)}
          disabled={names.length <= 1}
          aria-label={`Remove pilot ${i + 1}`}>Remove</Button
        >
      </div>
    {/each}
    <div class="add-row">
      <Button variant="secondary" size="sm" onclick={addPilot}>+ Add pilot</Button>
    </div>
  </div>

  {#if problems.length > 0}
    <ul class="problems">
      {#each problems as p (p)}<li>{p}</li>{/each}
    </ul>
  {/if}

  <div class="actions">
    <Button variant="primary" onclick={schedule} loading={scheduling} disabled={!ready}>
      {scheduling ? 'Scheduling…' : 'Schedule heat'}
    </Button>
  </div>
</section>

<style>
  .new-heat {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
    padding: var(--gf-space-5);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    background: var(--gf-elevated);
    box-shadow: var(--gf-shadow-xs);
  }
  .head h3 {
    margin: 0;
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    margin: var(--gf-space-1) 0 0;
  }
  .muted code {
    color: var(--gf-text-secondary);
    background: var(--gf-surface-alt);
    padding: 0.05em 0.35em;
    border-radius: var(--gf-radius-xs);
    font-size: 0.92em;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .field-label {
    font-size: var(--gf-font-size-2xs);
    font-weight: var(--gf-font-weight-semibold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .heat-id {
    max-width: 14rem;
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
  .pilot-num {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 1.6rem;
    height: 1.6rem;
    flex-shrink: 0;
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-alt);
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-xs);
    font-weight: var(--gf-font-weight-bold);
    font-variant-numeric: tabular-nums;
  }
  .add-row {
    margin-top: var(--gf-space-1);
  }
  .actions {
    display: flex;
  }
  .problems {
    color: var(--gf-danger);
    font-size: var(--gf-font-size-sm);
    margin: 0;
    padding-left: var(--gf-space-4);
  }
</style>
