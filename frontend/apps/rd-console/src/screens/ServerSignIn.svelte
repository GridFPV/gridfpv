<script lang="ts">
  /**
   * ServerSignIn — the buttons that send a person to a provider to sign in
   * (docs/primitives.md §13.8). Grid never sees a password: the provider says who they are.
   *
   * With `invite`, the invite goes along, so that someone new gets their account.
   */
  import { Banner } from '@gridfpv/components';
  import type { SignInProvider } from '@gridfpv/types';
  import type { PeopleApi } from '../lib/serverPeople.js';

  let {
    api,
    providers,
    invite = undefined,
    add = false,
    problem = undefined
  }: {
    api: Pick<PeopleApi, 'signInUrl'>;
    /** The providers the server offers. `undefined` while that is being read. */
    providers: readonly SignInProvider[] | undefined;
    /** The invite the person came with. */
    invite?: string | undefined;
    /** Add the provider to the account that is signed in. */
    add?: boolean;
    /** Why the last try did not work, in words. */
    problem?: string | undefined;
  } = $props();
</script>

<div class="sign-in">
  {#if problem}<Banner tone="warn"><span data-testid="sign-in-problem">{problem}</span></Banner
    >{/if}
  {#if providers === undefined}
    <p class="muted" role="status">Loading…</p>
  {:else if providers.length === 0}
    <p class="muted" data-testid="sign-in-off">Signing in is not set up on this server.</p>
  {:else}
    <ul>
      {#each providers as provider (provider.id)}
        <li>
          <a class="provider" href={api.signInUrl(provider.id, add ? { add: true } : { invite })}>
            {add ? 'Add' : 'Continue with'}
            {provider.name}
          </a>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .sign-in {
    display: grid;
    gap: var(--gf-space-3);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--gf-space-2);
  }
  .provider {
    display: block;
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-border-strong);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface);
    color: inherit;
    text-align: center;
    text-decoration: none;
    font-weight: var(--gf-font-weight-semibold);
  }
  .provider:hover,
  .provider:focus-visible {
    border-color: var(--gf-accent);
  }
  .muted {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
</style>
