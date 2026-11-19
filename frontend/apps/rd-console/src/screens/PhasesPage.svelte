<script lang="ts">
  /**
   * PhasesPage — what is raced at the open event, at a glance.
   *
   * A **phase** takes pilots in and gives a ranking out: Practice, Qualifying, a bracket, a
   * main. A **heat** is the smallest phase, a lineup that is raced one or more times.
   *
   * The page is short on purpose:
   *
   * 1. **Phases**: one line for each phase that holds heats, with where it stands. A line
   *    opens that phase's own page (`PhasePage`), which has its heats, lineups, ranking and
   *    sign-up sheet.
   * 2. **Up next**: the run order, the heats that can be raced now.
   * 3. **Heats on their own**: heats under no phase, shown in full, since there is nothing
   *    to open.
   *
   * Everything raced at the event is added here:
   *
   * - **Add phase** makes a phase: an empty one that heats are added to, or one a generator
   *   fills (a bracket, a multi-main).
   * - **Add heat** makes one heat, at the top of the event or under a phase.
   */
  import { Badge, Banner, Button, Card } from '@gridfpv/components';
  import type { PhaseId } from '@gridfpv/types';
  import Page from '../Page.svelte';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import { toggleFold, unfolded } from '../lib/phaseFold.svelte.js';
  import {
    isHeat,
    PHASE_STATUS_LABELS,
    phaseSummaries,
    shownSummaries,
    type PhaseStatus,
    newPhaseId
  } from '../lib/phases.js';
  import { formatHash } from '../lib/route.js';
  import AddHeatDialog from './AddHeatDialog.svelte';
  import AddPhaseDialog from './AddPhaseDialog.svelte';
  import EditPhaseDialog from './EditPhaseDialog.svelte';
  import PhaseList from './PhaseList.svelte';
  import RunQueue from './RunQueue.svelte';
  import { isRanked } from '../lib/formats.js';

  let {
    live,
    onrun
  }: {
    live: EventLive;
    /** Put a heat on the timer and open the Run page. */
    onrun: (heat: PhaseId) => void;
  } = $props();

  // Each dialog is mounted only while open, so a closed one never sits in the page.
  let adding = $state<{ what: 'phase' | 'heat'; under?: PhaseId } | undefined>(undefined);
  const close = () => (adding = undefined);
  /** The heat being edited. The dialog is mounted only while it is open. */
  let editing = $state<PhaseId | undefined>(undefined);
  const editingPhase = $derived(live.view?.phases.find((entry) => entry.def.id === editing));

  // ── The quick start on an empty event ──────────────────────────────────────
  let startBusy = $state(false);
  let startError = $state<string | undefined>(undefined);
  /**
   * Make a phase called Practice and go straight on to its first heat. It takes the first
   * saved format that ranks nobody, when there is one; with none, the heat picks its own.
   */
  async function startPractice() {
    if (startBusy) return;
    startBusy = true;
    startError = undefined;
    const format = live.formats.find((saved) => !isRanked(saved.format))?.format;
    const id = newPhaseId('Practice');
    try {
      const made = await live.send({
        Record: {
          PhaseDefined: {
            def: {
              id,
              label: 'Practice',
              kind: { Group: { combine: 'Stack' } },
              ...(format ? { format } : {})
            }
          }
        }
      });
      if (made) adding = { what: 'heat', under: id };
    } catch (err) {
      startError = err instanceof Error ? err.message : String(err);
    } finally {
      startBusy = false;
    }
  }

  const every = $derived(live.view ? phaseSummaries(live.view) : []);
  /** The lines shown: a phase inside another only while the one above it is unfolded. */
  const phases = $derived(shownSummaries(every, unfolded));
  /** Whether any phase holds phases, so every line leaves room for the fold button. */
  const anyInside = $derived(every.some((phase) => phase.inside > 0));
  /** Whether any heat stands alone at the top of the event. */
  const loneHeats = $derived(
    live.view?.phases.some((phase) => phase.def.parent === undefined && isHeat(phase.def)) ?? false
  );

  const TONES: Record<PhaseStatus, 'neutral' | 'accent' | 'success' | 'warn' | 'info'> = {
    waiting: 'neutral',
    ready: 'accent',
    racing: 'warn',
    complete: 'success',
    locked: 'info'
  };
  const heatsWords = (heats: number, flown: number): string =>
    heats === 0 ? 'No heats yet' : `${heats} ${heats === 1 ? 'heat' : 'heats'} · ${flown} flown`;
</script>

<Page
  title="Phases"
  help="phases"
  lede="What is raced at this event, in order. A phase holds heats and ranks the pilots in them. Open a phase for its heats."
>
  {#snippet actions()}
    <!-- A heat always goes in a phase: it is added with "+ Heat" on the phase's line. -->
    <Button variant="primary" onclick={() => (adding = { what: 'phase' })}>+ Add phase</Button>
  {/snippet}

  {#if live.loadError}
    <Banner tone="danger" title="The event could not be read.">{live.loadError}</Banner>
  {/if}

  {#if !live.view}
    {#if !live.loadError}<p class="muted" role="status">Loading…</p>{/if}
  {:else if live.view.phases.length === 0}
    <Card elevation="sm">
      <p class="muted" data-testid="no-phases">
        Nothing to race yet. Add a phase for each part of the day (Practice, Qualifying), then put
        heats in it.
      </p>
      <div class="start">
        <Button variant="secondary" disabled={startBusy} onclick={startPractice}>
          Add a practice phase
        </Button>
      </div>
      {#if startError}<Banner tone="danger">{startError}</Banner>{/if}
    </Card>
  {:else}
    {#if phases.length > 0}
      <Card title="Phases" elevation="sm">
        <ul class="phases" aria-label="Phases of the event">
          {#each phases as phase (phase.id)}
            <li style="--depth: {phase.depth}" data-depth={phase.depth}>
              {#if phase.inside > 0}
                <button
                  type="button"
                  class="fold"
                  aria-expanded={unfolded.has(phase.id)}
                  aria-label="{unfolded.has(phase.id) ? 'Hide' : 'Show'} the phases in {phase.name}"
                  onclick={() => toggleFold(phase.id)}
                >
                  <span aria-hidden="true">{unfolded.has(phase.id) ? '▾' : '▸'}</span>
                </button>
              {:else if anyInside}
                <span class="fold-gap" aria-hidden="true"></span>
              {/if}
              <a
                class="phase"
                href={formatHash({ kind: 'event', section: 'phases', phase: phase.id })}
                aria-label="Open {phase.name}"
              >
                <span class="name" class:nested={phase.depth > 0}>{phase.name}</span>
                <Badge tone={TONES[phase.status]} dot={phase.status === 'racing'}>
                  {PHASE_STATUS_LABELS[phase.status]}
                </Badge>
                {#if phase.onTimer !== undefined}
                  <Badge tone="accent">On the timer: {live.names.phase(phase.onTimer)}</Badge>
                {/if}
                {#if phase.signup}
                  <Badge tone={phase.signup === 'open' ? 'success' : 'neutral'}>
                    Sign-up {phase.signup}
                  </Badge>
                {/if}
                {#if phase.needsDecision}<Badge tone="warn">Needs a decision</Badge>{/if}
                <span class="facts">
                  {#if phase.format && phase.depth === 0}<span>{phase.format}</span>{/if}
                  {#if phase.inside > 0}
                    <span data-testid="phase-inside">
                      {phase.inside}
                      {phase.inside === 1 ? 'phase' : 'phases'} inside
                    </span>
                  {/if}
                  <span data-testid="phase-heats">{heatsWords(phase.heats, phase.flown)}</span>
                </span>
                <span class="open" aria-hidden="true">→</span>
              </a>
              <Button
                size="sm"
                variant="secondary"
                aria-label="Add a heat under {phase.name}"
                onclick={() => (adding = { what: 'heat', under: phase.id })}>+ Heat</Button
              >
            </li>
          {/each}
        </ul>
      </Card>
    {/if}
    {#if live.view.queue.length > 0}
      <Card title="Up next" elevation="sm">
        <RunQueue view={live.view} names={live.names} {live} {onrun} />
      </Card>
    {/if}
    {#if loneHeats}
      <Card title={phases.length > 0 ? 'Heats on their own' : 'Heats'} elevation="sm">
        <PhaseList
          view={live.view}
          names={live.names}
          {live}
          {onrun}
          topHeats
          onedit={(target) => (editing = target)}
        />
      </Card>
    {/if}
  {/if}
</Page>

{#if editingPhase}
  {#key editingPhase.def.id}
    <EditPhaseDialog {live} phase={editingPhase} onclose={() => (editing = undefined)} />
  {/key}
{/if}

{#if adding?.what === 'phase'}
  <AddPhaseDialog {live} under={adding.under} onclose={close} />
{:else if adding?.what === 'heat'}
  <AddHeatDialog {live} under={adding.under} onclose={close} />
{/if}

<style>
  .start {
    margin-top: var(--gf-space-3);
  }
  .muted {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .phases {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--gf-space-2);
  }
  .phases li {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    margin-left: calc(var(--depth) * var(--gf-space-6));
  }
  .fold,
  .fold-gap {
    flex: none;
    width: 2rem;
    height: 2rem;
  }
  .fold {
    display: grid;
    place-items: center;
    padding: 0;
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-sm);
    background: none;
    color: var(--gf-text-secondary);
    font-size: var(--gf-font-size-lg);
    cursor: pointer;
  }
  .fold:hover,
  .fold:focus-visible {
    border-color: var(--gf-accent);
    color: var(--gf-text);
  }
  .name.nested {
    font-size: var(--gf-font-size-md);
  }
  /* The whole line opens the phase. */
  .phase {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--gf-space-2) var(--gf-space-3);
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-md);
    color: inherit;
    text-decoration: none;
  }
  .phase:hover,
  .phase:focus-visible {
    border-color: var(--gf-accent);
  }
  .name {
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-semibold);
    overflow-wrap: anywhere;
  }
  .facts {
    display: flex;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
    margin-left: auto;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .open {
    color: var(--gf-text-muted);
  }
</style>
