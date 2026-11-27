<script lang="ts">
  /**
   * ServerJoin — joining one club (docs/primitives.md §13.7).
   *
   * A public club is joined with one press. A private club is asked, and a director of it
   * says yes or no. Someone already in the club, and an archived club, are shown nothing
   * to press.
   */
  import { Button } from '@gridfpv/components';
  import type { Me, MyAccount } from '@gridfpv/types';
  import { formatServerHash } from '../lib/serverHome.js';
  import { joinStanding, type PeopleApi } from '../lib/serverPeople.js';

  let {
    api,
    club,
    account,
    onchange
  }: {
    api: Pick<PeopleApi, 'joinClub' | 'leaveClub'>;
    club: { id: string; name: string; open?: boolean; archived?: boolean };
    /** Who is signed in. `null` for nobody, `undefined` while that is not known. */
    account: MyAccount | null | undefined;
    /** The account changed: here it is as it now stands. */
    onchange: (me: Me) => void;
  } = $props();

  let busy = $state(false);
  let error = $state<string | undefined>(undefined);
  const standing = $derived(account === undefined ? undefined : joinStanding(account, club));

  async function run(what: () => Promise<Me>) {
    if (busy) return;
    busy = true;
    error = undefined;
    try {
      onchange(await what());
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }
</script>

{#if standing && standing !== 'closed'}
  <div class="join" data-testid="club-join">
    {#if standing === 'signed-out'}
      <a href={formatServerHash({ kind: 'sign-in' })}>Sign in to join {club.name}</a>
    {:else if standing === 'member'}
      <span class="muted">You are in this club.</span>
    {:else if standing === 'asked'}
      <span class="muted">You asked to join. A director of the club will answer.</span>
      <Button
        size="sm"
        variant="ghost"
        disabled={busy}
        aria-label="Take back your ask to join {club.name}"
        onclick={() => run(() => api.leaveClub(club.id))}>Take it back</Button
      >
    {:else if standing === 'can-join'}
      <Button
        size="sm"
        variant="primary"
        disabled={busy}
        aria-label="Join {club.name}"
        onclick={() => run(() => api.joinClub(club.id))}>Join</Button
      >
    {:else}
      <Button
        size="sm"
        disabled={busy}
        aria-label="Ask to join {club.name}"
        onclick={() => run(() => api.joinClub(club.id))}>Ask to join</Button
      >
      <span class="muted">A director of the club says yes.</span>
    {/if}
    {#if error}<span class="error" role="alert">{error}</span>{/if}
  </div>
{/if}

<style>
  .join {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--gf-space-2) var(--gf-space-3);
    font-size: var(--gf-font-size-sm);
  }
  .muted {
    color: var(--gf-text-muted);
  }
  .error {
    color: var(--gf-danger);
  }
</style>
