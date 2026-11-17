<script lang="ts">
  /**
   * PhasePage — one phase of the event: its heats, its ranking, and its sign-up sheet.
   *
   * The Phases page lists the event's phases, one line each. This is what a line opens.
   * Everything about the phase is here, so the list can stay short: the heats with their
   * lineups, the rulings (lock, set a position, disqualify), adding a heat, and, for a
   * phase pilots sign up for, the sheet.
   */
  import { Badge, Banner, Card } from '@gridfpv/components';
  import type { PhaseId } from '@gridfpv/types';
  import Page from '../Page.svelte';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import { isHeat, PHASE_STATUS_LABELS, phaseStatus } from '../lib/phases.js';
  import { formatHash } from '../lib/route.js';
  import { signupPhases } from '../lib/signup.js';
  import AddHeatDialog from './AddHeatDialog.svelte';
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

  /** The phase a heat is being added under. */
  let addingUnder = $state<PhaseId | undefined>(undefined);
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
      : 'This phase: its heats and lineups, its ranking, and the rulings on it.'
    : ''}
>
  {#snippet actions()}
    <a class="back" href={back} data-testid="back-to-phases">← All phases</a>
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
    <p class="where">
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
    <Card title={isHeat(phase.def) ? 'Heat' : 'Heats and ranking'} elevation="sm">
      <PhaseList
        {view}
        names={live.names}
        {live}
        {onrun}
        root={phaseId}
        onaddheat={(under) => (addingUnder = under)}
        onsignup={(target) => (rulesFor = target)}
      />
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

{#if addingUnder !== undefined}
  <AddHeatDialog {live} under={addingUnder} onclose={() => (addingUnder = undefined)} />
{/if}

<style>
  .back {
    font-size: var(--gf-font-size-sm);
    white-space: nowrap;
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
