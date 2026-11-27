<script lang="ts">
  /**
   * ServerJoinClubs — the clubs on a server that the signed-in account is not in, each
   * with a way in (docs/primitives.md §13.7).
   */
  import { Badge, Card } from '@gridfpv/components';
  import type { Me, MyAccount, ServerClub } from '@gridfpv/types';
  import { type PeopleApi } from '../lib/serverPeople.js';
  import ServerJoin from './ServerJoin.svelte';

  let {
    api,
    account,
    onchange
  }: {
    api: Pick<PeopleApi, 'serverClubs' | 'joinClub' | 'leaveClub'>;
    account: MyAccount;
    /** The account changed: here it is as it now stands. */
    onchange: (me: Me) => void;
  } = $props();

  let clubs = $state<ServerClub[] | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);

  $effect(() => {
    api
      .serverClubs()
      .then((found) => {
        clubs = found;
        loadError = undefined;
      })
      .catch((err) => (loadError = err instanceof Error ? err.message : String(err)));
  });

  /** The clubs to offer: not archived, and not ones the account is already in. */
  const others = $derived(
    clubs?.filter(
      (club) => !club.archived && !account.clubs.some((entry) => entry.club.id === club.id)
    )
  );
</script>

{#if loadError || (others && (others.length > 0 || account.clubs.length === 0))}
  <Card elevation="sm">
    <div class="block" data-testid="join-clubs">
      <h2>Join a club</h2>
      {#if loadError}
        <p class="muted">{loadError}</p>
      {:else if others && others.length === 0}
        <p class="muted">There is no club to join on this server yet.</p>
      {:else if others}
        <ul aria-label="Clubs you can join">
          {#each others as club (club.id)}
            <li data-testid="join-club">
              <span class="name">{club.name}</span>
              <Badge tone="neutral">{club.open ? 'Public' : 'Private'}</Badge>
              <ServerJoin {api} {club} {account} {onchange} />
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  </Card>
{/if}

<style>
  .block {
    display: grid;
    gap: var(--gf-space-3);
  }
  h2 {
    margin: 0;
    font-size: var(--gf-font-size-lg);
  }
  p {
    margin: 0;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--gf-space-2);
  }
  li {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--gf-space-2) var(--gf-space-3);
  }
  .name {
    font-weight: var(--gf-font-weight-semibold);
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
</style>
