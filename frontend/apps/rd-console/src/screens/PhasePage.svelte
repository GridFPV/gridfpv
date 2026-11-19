<script lang="ts">
  /**
   * PhasePage — one phase of the event: its heats, its ranking, and its sign-up sheet.
   *
   * The Phases page lists the event's phases, one line each. This is what a line opens.
   * Everything about the phase is here, so the list can stay short: the heats with their
   * lineups, the rulings (lock, set a position, disqualify), adding a heat, and, for a
   * phase pilots sign up for, the sheet.
   */
  import { Badge, Banner, Button, Card } from '@gridfpv/components';
  import type { PhaseId } from '@gridfpv/types';
  import Page from '../Page.svelte';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import {
    isHeat,
    PHASE_STATUS_LABELS,
    phaseStatus,
    phaseSummaries,
    type PhaseStatus
  } from '../lib/phases.js';
  import { formatHash } from '../lib/route.js';
  import { signupPhases } from '../lib/signup.js';
  import AddHeatDialog from './AddHeatDialog.svelte';
  import AddPhaseDialog from './AddPhaseDialog.svelte';
  import EditPhaseDialog from './EditPhaseDialog.svelte';
  import PhaseList from './PhaseList.svelte';
  import SignupRulesDialog from './SignupRulesDialog.svelte';
  import SignupSheet from './SignupSheet.svelte';

  let {
    live,
    phase: phaseId,
    onrun
  }: {
    live: EventLive;
    /** The phase to show. A wire handle; its name is what is shown. */
    phase: PhaseId;
    /** Put a heat on the timer and open the Run page. */
    onrun: (heat: PhaseId) => void;
  } = $props();

  const view = $derived(live.view);
  const phase = $derived(view?.phases.find((entry) => entry.def.id === phaseId));
  const sheet = $derived(signupPhases(view).find((entry) => entry.id === phaseId));
  const back = formatHash({ kind: 'event', section: 'phases' });
  const hashOf = (id: PhaseId) => formatHash({ kind: 'event', section: 'phases', phase: id });
  /** The phase that holds this one, when one does. */
  const parent = $derived(
    view?.phases.find((entry) => phase !== undefined && entry.def.id === phase.def.parent)
  );
  /** The phases directly inside this one. Each has its own page. */
  const inside = $derived(
    view ? phaseSummaries(view).filter((entry) => entry.parent === phaseId) : []
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

  /** The phase a heat is being added under. */
  let addingUnder = $state<PhaseId | undefined>(undefined);
  /** Whether a phase is being added inside this one. */
  let addingPhase = $state(false);
  /** The phase or heat being edited. The dialog is mounted only while it is open. */
  let editing = $state<PhaseId | undefined>(undefined);
  const editingPhase = $derived(view?.phases.find((entry) => entry.def.id === editing));
  /** The phase whose sign-up rules are being set. */
  let rulesFor = $state<PhaseId | undefined>(undefined);
  const rulesPhase = $derived(view?.phases.find((entry) => entry.def.id === rulesFor));
</script>

<Page
  title={phase?.def.label ?? 'Phase'}
  help="phases"
  lede={phase
    ? isHeat(phase.def)
      ? 'One heat: its lineup, its ranking, and the rulings on it.'
      : inside.length > 0
        ? 'This phase: the phases inside it, its own heats, its ranking, and the rulings on it.'
        : 'This phase: its heats and lineups, its ranking, and the rulings on it.'
    : ''}
>
  {#snippet actions()}
    <nav class="crumbs" aria-label="Where this phase is">
      <a class="back" href={back} data-testid="back-to-phases">← All phases</a>
      {#if parent}
        <a class="back" href={hashOf(parent.def.id)} data-testid="back-to-parent">
          ← {parent.def.label}
        </a>
      {/if}
    </nav>
    {#if phase && !isHeat(phase.def) && !phase.locked}
      <Button
        variant="secondary"
        aria-label="Add a phase inside {phase.def.label}"
        onclick={() => (addingPhase = true)}>+ Add phase</Button
      >
    {/if}
  {/snippet}

  {#if live.loadError}
    <Banner tone="danger" title="The event could not be read.">{live.loadError}</Banner>
  {/if}

  {#if !view}
    {#if !live.loadError}<p class="muted" role="status">Loading…</p>{/if}
  {:else if !phase}
    <Card elevation="sm">
      <p class="muted" data-testid="no-such-phase">
        This phase is not in the event. It may have been removed.
      </p>
      <a class="back" href={back}>← All phases</a>
    </Card>
  {:else}
    <p class="where" data-testid="phase-where">
      <Badge tone="neutral">{PHASE_STATUS_LABELS[phaseStatus(phase, view)]}</Badge>
      {#if phase.format}<span class="muted">{phase.format.name}</span>{/if}
    </p>
    {#if sheet}
      <SignupSheet
        phase={sheet}
        names={live.names}
        onTimer={view.on_timer}
        send={(command) => live.send(command)}
        onrules={() => (rulesFor = sheet.id)}
      />
    {/if}
    {#if inside.length > 0}
      <Card title="Phases in {phase.def.label}" elevation="sm">
        <ul class="inside" aria-label="Phases in {phase.def.label}">
          {#each inside as entry (entry.id)}
            <li>
              <a class="phase" href={hashOf(entry.id)} aria-label="Open {entry.name}">
                <span class="name">{entry.name}</span>
                <Badge tone={TONES[entry.status]} dot={entry.status === 'racing'}>
                  {PHASE_STATUS_LABELS[entry.status]}
                </Badge>
                {#if entry.onTimer !== undefined}
                  <Badge tone="accent">On the timer: {live.names.phase(entry.onTimer)}</Badge>
                {/if}
                {#if entry.needsDecision}<Badge tone="warn">Needs a decision</Badge>{/if}
                <span class="facts">
                  {#if entry.inside > 0}
                    <span>{entry.inside} {entry.inside === 1 ? 'phase' : 'phases'} inside</span>
                  {/if}
                  <span>{heatsWords(entry.heats, entry.flown)}</span>
                </span>
                <span class="open" aria-hidden="true">→</span>
              </a>
            </li>
          {/each}
        </ul>
      </Card>
    {/if}
    <Card title={isHeat(phase.def) ? 'Heat' : 'Heats and ranking'} elevation="sm">
      {#if isHeat(phase.def)}
        <PhaseList
          {view}
          names={live.names}
          {live}
          {onrun}
          root={phaseId}
          onedit={(target) => (editing = target)}
        />
      {:else}
        <PhaseList
          {view}
          names={live.names}
          {live}
          {onrun}
          heatsOf={phaseId}
          onaddheat={(under) => (addingUnder = under)}
          onedit={(target) => (editing = target)}
          onsignup={(target) => (rulesFor = target)}
        />
      {/if}
    </Card>
  {/if}
</Page>

{#if rulesPhase}
  <SignupRulesDialog
    phase={rulesPhase.def.id}
    phaseName={rulesPhase.def.label}
    rules={rulesPhase.signup?.rules}
    sources={live.seatSources}
    send={(command) => live.send(command)}
    onclose={() => (rulesFor = undefined)}
  />
{/if}

{#if editingPhase}
  {#key editingPhase.def.id}
    <EditPhaseDialog {live} phase={editingPhase} onclose={() => (editing = undefined)} />
  {/key}
{/if}

{#if addingPhase}
  <AddPhaseDialog {live} under={phaseId} onclose={() => (addingPhase = false)} />
{/if}

{#if addingUnder !== undefined}
  <AddHeatDialog {live} under={addingUnder} onclose={() => (addingUnder = undefined)} />
{/if}

<style>
  .back {
    font-size: var(--gf-font-size-sm);
    white-space: nowrap;
  }
  .crumbs {
    display: flex;
    gap: var(--gf-space-4);
    flex-wrap: wrap;
  }
  .inside {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--gf-space-2);
  }
  /* The whole line opens the phase. */
  .phase {
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
  .where {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    margin: 0;
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    margin: 0;
  }
</style>
