<script lang="ts">
  /**
   * ServerResults — the events the signed-in account's pilots flew, newest first, and
   * where they stood in each class (docs/primitives.md §13.7).
   */
  import { Banner, Card } from '@gridfpv/components';
  import type { MyResult } from '@gridfpv/types';
  import { eventDetail, formatServerHash } from '../lib/serverHome.js';
  import { placeWords, type PeopleApi } from '../lib/serverPeople.js';

  let { api }: { api: Pick<PeopleApi, 'results'> } = $props();

  let results = $state<MyResult[] | undefined>(undefined);
  let error = $state<string | undefined>(undefined);

  $effect(() => {
    let stopped = false;
    api
      .results()
      .then((found) => {
        if (!stopped) results = found;
      })
      .catch((err: unknown) => {
        if (!stopped) error = err instanceof Error ? err.message : String(err);
      });
    return () => {
      stopped = true;
    };
  });
</script>

<div class="results" data-testid="server-results">
  <p class="lead">Your results</p>
  {#if error}
    <Banner tone="danger" title="Your results could not be read.">{error}</Banner>
  {:else if results === undefined}
    <p class="muted" role="status">Loading…</p>
  {:else if results.length === 0}
    <Card elevation="sm">
      <p>No results yet.</p>
      <p class="muted">
        They show here once you hold a pilot and that pilot has flown in an event. Find yourself
        from your account page.
      </p>
    </Card>
  {:else}
    <ul aria-label="Your results">
      {#each results as result (result.club.id + '/' + result.event.id + '/' + result.callsign)}
        <li data-testid="result">
          <a
            class="name"
            href={formatServerHash({
              kind: 'event',
              club: result.club.id,
              event: result.event.id
            })}>{result.event.name}</a
          >
          <span class="muted"
            >{[eventDetail(result.event), result.club.name]
              .filter((part) => part !== '')
              .join(' · ')}</span
          >
          <span class="muted">Flew as {result.callsign}</span>
          {#each result.classes as place (place.class)}
            <span data-testid="result-place"
              >{place.class}: <strong>{placeWords(place)}</strong></span
            >
          {/each}
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .results {
    display: grid;
    gap: var(--gf-space-3);
  }
  p {
    margin: 0;
  }
  .lead {
    font-size: var(--gf-font-size-xl);
    font-weight: var(--gf-font-weight-bold);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--gf-space-2);
  }
  li {
    display: grid;
    gap: 2px;
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface);
    overflow-wrap: anywhere;
  }
  .name {
    font-weight: var(--gf-font-weight-semibold);
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
</style>
