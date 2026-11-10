<script lang="ts">
  /**
   * EventPage — the open event's page (Home › Events › ‹event name›).
   *
   * Two parts. The details card shows what the event is: date, location, organizer and
   * description. The Phases card shows the competition: every phase as a tree, each heat with
   * its lineup and a button that opens its Run view. Heats and brackets are added from here.
   *
   * The competition comes from the live event store, so the list follows the Director as
   * races are run, from this console or another one.
   */
  import { Banner, Button, Card } from '@gridfpv/components';
  import type { EventMeta, PhaseId } from '@gridfpv/types';
  import Brand from '../Brand.svelte';
  import Breadcrumbs from '../Breadcrumbs.svelte';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import AddBracketDialog from './AddBracketDialog.svelte';
  import AddHeatDialog from './AddHeatDialog.svelte';
  import PhaseList from './PhaseList.svelte';

  let {
    event,
    live,
    onhome,
    onevents,
    onrun
  }: {
    event: EventMeta;
    /** The live competition of this event. */
    live: EventLive;
    /** Leave for the home hub. */
    onhome: () => void;
    /** Leave for the Events page. */
    onevents: () => void;
    /** Open the Run view for a heat. */
    onrun: (heat: PhaseId) => void;
  } = $props();

  // Each dialog is mounted only while open, so a closed one never sits in the page.
  let addHeatOpen = $state(false);
  let addBracketOpen = $state(false);

  /** The detail rows that have a value — an unset detail is left out rather than shown blank. */
  const details = $derived(
    [
      { label: 'Date', value: event.date },
      { label: 'Location', value: event.location },
      { label: 'Organizer', value: event.organizer },
      { label: 'Description', value: event.description }
    ].filter((row): row is { label: string; value: string } => !!row.value?.trim())
  );
</script>

<div class="page">
  <div class="page-inner">
    <!-- The brand is the home root (#118): every page carries the same top-left GridFPV mark,
         leaving for the hub. -->
    <div class="brand-row"><Brand onclick={onhome} /></div>
    <Breadcrumbs
      crumbs={[
        { label: 'Home', onclick: onhome },
        { label: 'Events', onclick: onevents },
        { label: event.name }
      ]}
    />

    <header class="page-head">
      <h1 class="page-title">{event.name}</h1>
    </header>

    <Card elevation="sm">
      <div class="details-wrap">
        {#if details.length > 0}
          <dl class="details" aria-label="Event details">
            {#each details as row (row.label)}
              <div class="detail">
                <dt>{row.label}</dt>
                <dd>{row.value}</dd>
              </div>
            {/each}
          </dl>
        {:else}
          <p class="no-details">No date, location, organizer or description was added.</p>
        {/if}
      </div>
    </Card>

    <Card title="Phases" elevation="sm">
      {#snippet actions()}
        <Button size="sm" variant="secondary" onclick={() => (addBracketOpen = true)}>
          + Add bracket
        </Button>
        <Button size="sm" variant="primary" onclick={() => (addHeatOpen = true)}>+ Add heat</Button>
      {/snippet}
      {#if live.loadError}
        <Banner tone="danger" title="The event could not be read.">{live.loadError}</Banner>
      {/if}
      {#if !live.view}
        {#if !live.loadError}<p class="muted" role="status">Loading…</p>{/if}
      {:else if live.view.phases.length === 0}
        <p class="muted">Nothing to race yet. Add a heat or a bracket to start.</p>
      {:else}
        <PhaseList view={live.view} names={live.names} {onrun} />
      {/if}
    </Card>
  </div>
</div>

{#if addHeatOpen}
  <AddHeatDialog {live} onclose={() => (addHeatOpen = false)} />
{/if}
{#if addBracketOpen}
  <AddBracketDialog {live} onclose={() => (addBracketOpen = false)} />
{/if}

<style>
  .brand-row {
    margin-bottom: var(--gf-space-4);
  }

  .page {
    min-height: 100vh;
    padding: var(--gf-space-6) var(--gf-space-8) var(--gf-space-8);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    overflow: auto;
  }
  .page-inner {
    width: min(52rem, 100%);
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
  }
  .page-title {
    margin: 0;
    font-size: var(--gf-font-size-2xl);
    letter-spacing: var(--gf-tracking-tight);
  }
  .details-wrap {
    padding: var(--gf-space-2);
  }
  .details {
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .detail {
    display: grid;
    grid-template-columns: 8rem 1fr;
    gap: var(--gf-space-4);
    align-items: baseline;
  }
  .detail dt {
    font-size: var(--gf-font-size-xs);
    font-weight: var(--gf-font-weight-semibold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .detail dd {
    margin: 0;
    font-size: var(--gf-font-size-md);
    overflow-wrap: anywhere;
  }
  .no-details,
  .muted {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  @media (max-width: 36rem) {
    .detail {
      grid-template-columns: 1fr;
      gap: var(--gf-space-1);
    }
  }
</style>
