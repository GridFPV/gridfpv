<script lang="ts">
  /**
   * EventPage — the open event's page (Home › Events › ‹event name›).
   *
   * Shows the event's own details: name, date, location, organizer and description. That is all
   * an event carries right now — the competition content (classes at the event, phases, heats,
   * races) is being rebuilt from the primitives in `docs/primitives.md`, and this page says so
   * rather than showing empty sections for it.
   */
  import { Card } from '@gridfpv/components';
  import type { EventMeta } from '@gridfpv/types';
  import Brand from '../Brand.svelte';
  import Breadcrumbs from '../Breadcrumbs.svelte';

  let {
    event,
    onhome,
    onevents
  }: {
    event: EventMeta;
    /** Leave for the home hub. */
    onhome: () => void;
    /** Leave for the Events page. */
    onevents: () => void;
  } = $props();

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

    <p class="rebuild-note">
      This is everything an event holds for now. Running races in an event is being rebuilt.
    </p>
  </div>
</div>

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
  .rebuild-note {
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
