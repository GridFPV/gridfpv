<script lang="ts">
  /**
   * ServerReconnect — what a person sees when they open a re-connect link
   * (docs/primitives.md §13.8).
   *
   * An owner of the server made the link for someone who can no longer sign in. The
   * person signs in here with a provider account they do have, and that sign-in is put on
   * their old account. Nothing on the account changes.
   */
  import { Card } from '@gridfpv/components';
  import type { Me, MyAccount, ReconnectView, SignInProvider } from '@gridfpv/types';
  import type { PeopleApi } from '../lib/serverPeople.js';
  import ServerSignIn from './ServerSignIn.svelte';

  let {
    api,
    secret,
    account,
    providers,
    password = false,
    email = false,
    onsignedin = undefined
  }: {
    api: Pick<PeopleApi, 'reconnect' | 'signInUrl'> &
      Partial<
        Pick<PeopleApi, 'passwordSignIn' | 'passwordSignUp' | 'setPassword' | 'forgotPassword'>
      >;
    secret: string;
    /** The account this browser is signed in as now, if it is. */
    account: MyAccount | undefined;
    providers: readonly SignInProvider[] | undefined;
    /** Whether this server lets people sign in with a username and password. */
    password?: boolean;
    /** Whether this server sends email. */
    email?: boolean;
    /** Someone set a username and password with the link, and is signed in. */
    onsignedin?: ((me: Me) => void) | undefined;
  } = $props();

  let link = $state<ReconnectView | undefined>(undefined);
  /** The link was looked up and does not work. */
  let dead = $state(false);

  $effect(() => {
    const asked = secret;
    link = undefined;
    dead = false;
    let stopped = false;
    api
      .reconnect(asked)
      .then((found) => {
        if (!stopped) link = found;
      })
      .catch(() => {
        if (!stopped) dead = true;
      });
    return () => {
      stopped = true;
    };
  });
</script>

<Card elevation="sm">
  <div class="reconnect" data-testid="server-reconnect">
    {#if dead}
      <p class="lead">This link does not work</p>
      <p>
        It may have been used already, taken back, or have run out. Ask an owner of this server for
        a new one.
      </p>
    {:else if !link}
      <p class="muted" role="status">Loading…</p>
    {:else}
      <p class="lead" data-testid="reconnect-for">Get back in to {link.name}</p>
      <p class="muted">
        {#if password}
          <span data-testid="reconnect-password">Type your username and a new password.</span>
        {:else}
          Sign in with an account you can still use.
        {/if}
        This link works once.
      </p>
      {#if account}
        <p data-testid="reconnect-signed-in">
          This browser is signed in as <strong>{account.name}</strong> now. Going on signs it in as
          <strong>{link.name}</strong>.
        </p>
      {/if}
      <ServerSignIn {api} {providers} invite={secret} {password} {email} {onsignedin} />
    {/if}
  </div>
</Card>

<style>
  .reconnect {
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
