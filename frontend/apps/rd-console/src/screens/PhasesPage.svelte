<script lang="ts">
  /**
   * PhasesPage — the competition of the open event, as a tree.
   *
   * A **phase** takes pilots in and gives a ranking out: Practice, Qualifying, a bracket, a
   * main. A **heat** is the smallest phase, a lineup that is raced one or more times.
   * Everything raced at the event is added here:
   *
   * - **Add phase** makes a phase that holds heats, with one format for all of them.
   * - **Add heat** makes one heat, at the top of the event or under a phase.
   * - **Add bracket** makes every heat of an elimination bracket at once.
   *
   * "Up next" is the run queue: the heats that can be raced now, in an order the director
   * can change. The tree under it shows each phase's state, lineup and ranking, and is where
   * a phase is locked, unlocked or removed.
   */
  import { Banner, Button, Card } from '@gridfpv/components';
  import type { PhaseId } from '@gridfpv/types';
  import Page from '../Page.svelte';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import AddBracketDialog from './AddBracketDialog.svelte';
  import AddHeatDialog from './AddHeatDialog.svelte';
  import AddPhaseDialog from './AddPhaseDialog.svelte';
  import PhaseList from './PhaseList.svelte';
  import RunQueue from './RunQueue.svelte';

  let {
    live,
    onrun
  }: {
    live: EventLive;
    /** Open the Run page on a heat. */
    onrun: (heat: PhaseId) => void;
  } = $props();

  // Each dialog is mounted only while open, so a closed one never sits in the page.
  let adding = $state<{ what: 'phase' | 'heat' | 'bracket'; under?: PhaseId } | undefined>(
    undefined
  );
  const close = () => (adding = undefined);
</script>

<Page
  title="Phases"
  help="phases"
  lede="What is raced at this event, in order. A phase holds heats and ranks the pilots in them. A heat is one lineup, raced one or more times."
>
  {#snippet actions()}
    <Button variant="secondary" onclick={() => (adding = { what: 'bracket' })}>
      + Add bracket
    </Button>
    <Button variant="secondary" onclick={() => (adding = { what: 'phase' })}>+ Add phase</Button>
    <Button variant="primary" onclick={() => (adding = { what: 'heat' })}>+ Add heat</Button>
  {/snippet}

  {#if live.loadError}
    <Banner tone="danger" title="The event could not be read.">{live.loadError}</Banner>
  {/if}

  {#if !live.view}
    {#if !live.loadError}<p class="muted" role="status">Loading…</p>{/if}
  {:else if live.view.phases.length === 0}
    <Card elevation="sm">
      <p class="muted" data-testid="no-phases">
        Nothing to race yet. For one pilot flying laps, add a heat. For a race day, add a phase for
        each part of it (Practice, Qualifying) and put heats under it.
      </p>
    </Card>
  {:else}
    {#if live.view.queue.length > 0}
      <Card title="Up next" elevation="sm">
        <RunQueue view={live.view} names={live.names} {live} {onrun} />
      </Card>
    {/if}
    <Card title="All phases" elevation="sm">
      <PhaseList
        view={live.view}
        names={live.names}
        {live}
        {onrun}
        onaddheat={(under) => (adding = { what: 'heat', under })}
      />
    </Card>
  {/if}
</Page>

{#if adding?.what === 'phase'}
  <AddPhaseDialog {live} under={adding.under} onclose={close} />
{:else if adding?.what === 'heat'}
  <AddHeatDialog {live} under={adding.under} onclose={close} />
{:else if adding?.what === 'bracket'}
  <AddBracketDialog {live} under={adding.under} onclose={close} />
{/if}

<style>
  .muted {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
</style>
