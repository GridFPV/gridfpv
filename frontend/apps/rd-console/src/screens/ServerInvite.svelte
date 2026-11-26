<script lang="ts">
  /**
   * ServerInvite — what a person sees when they open an invite link
   * (docs/primitives.md §13.8).
   *
   * It says what the invite is for. Someone not signed in signs in with a provider, and
   * the invite goes along: that is how an account is made. Someone already signed in joins
   * with the account they have.
   */
  import { Banner, Button, Card } from '@gridfpv/components';
  import type { InviteView, MyAccount, SignInProvider } from '@gridfpv/types';
  import { inviteWords, type PeopleApi } from '../lib/serverPeople.js';
  import ServerSignIn from './ServerSignIn.svelte';

  let {
    api,
    secret,
    account,
    providers,
    onjoined
  }: {
    api: Pick<PeopleApi, 'invite' | 'acceptInvite' | 'signInUrl'>;
    secret: string;
    /** The account that is signed in, if one is. */
    account: MyAccount | undefined;
    providers: readonly SignInProvider[] | undefined;
    /** The signed-in account has used the invite. */
    onjoined: () => void;
  } = $props();

  let invite = $state<InviteView | undefined>(undefined);
  /** The link was looked up and does not work. */
  let dead = $state(false);
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);

  $effect(() => {
    const asked = secret;
    invite = undefined;
    dead = false;
    error = undefined;
    let stopped = false;
    api
      .invite(asked)
      .then((found) => {
        if (!stopped) invite = found;
      })
      .catch(() => {
        if (!stopped) dead = true;
      });
    return () => {
      stopped = true;
    };
  });

  async function join() {
    if (busy) return;
    busy = true;
    error = undefined;
    try {
      await api.acceptInvite(secret);
      onjoined();
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }
</script>

<Card elevation="sm">
  <div class="invite" data-testid="server-invite">
    {#if dead}
      <p class="lead">This invite does not work</p>
      <p>It may have been used already, taken back, or have run out. Ask for a new one.</p>
    {:else if !invite}
      <p class="muted" role="status">Loading…</p>
    {:else}
      <p class="lead" data-testid="invite-for">You are invited to {inviteWords(invite)}</p>
      {#if error}<Banner tone="danger">{error}</Banner>{/if}
      {#if account}
        <p>You are signed in as <strong>{account.name}</strong>.</p>
        <div>
          <Button variant="primary" disabled={busy} onclick={join}>Accept as {account.name}</Button>
        </div>
      {:else}
        <p>Sign in to accept. If you have no account here yet, this makes one.</p>
        <ServerSignIn {api} {providers} invite={secret} />
      {/if}
    {/if}
  </div>
</Card>

<style>
  .invite {
    display: grid;
    gap: var(--gf-space-3);
  }
  p {
    margin: 0;
  }
  .lead {
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-bold);
    overflow-wrap: anywhere;
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
</style>
