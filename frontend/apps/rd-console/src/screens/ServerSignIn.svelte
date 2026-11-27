<script lang="ts">
  /**
   * ServerSignIn — the buttons that send a person to a provider to sign in
   * (docs/primitives.md §13.8). Grid never sees a password: the provider says who they are.
   *
   * With `invite`, the invite goes along, so that someone new gets their account.
   */
  import { Banner, InfoTip } from '@gridfpv/components';
  import type { Me, SignInProvider } from '@gridfpv/types';
  import { OWNERS_SEE_ALL, type PeopleApi } from '../lib/serverPeople.js';
  import ServerPassword from './ServerPassword.svelte';

  let {
    api,
    providers,
    invite = undefined,
    add = false,
    problem = undefined,
    password = false,
    canMake = false,
    email = false,
    onsignedin = undefined
  }: {
    api: Pick<PeopleApi, 'signInUrl'> &
      Partial<
        Pick<PeopleApi, 'passwordSignIn' | 'passwordSignUp' | 'setPassword' | 'forgotPassword'>
      >;
    /** The providers the server offers. `undefined` while that is being read. */
    providers: readonly SignInProvider[] | undefined;
    /** The invite the person came with. */
    invite?: string | undefined;
    /** Add the provider to the account that is signed in. */
    add?: boolean;
    /** Why the last try did not work, in words. */
    problem?: string | undefined;
    /** Whether this server lets people sign in with a username and password. */
    password?: boolean;
    /** Whether someone with no invite may make an account here. */
    canMake?: boolean;
    /** Whether this server sends email, so a forgotten password can be set by a link. */
    email?: boolean;
    /** Someone signed in with a password: here is their account. */
    onsignedin?: ((me: Me) => void) | undefined;
  } = $props();

  /** A re-connect link sets a way back in. It is not signed in with. */
  const reconnecting = $derived(invite?.startsWith('gfr_') ?? false);
  /** The password form is offered where the server allows it, and not when adding. */
  const withPassword = $derived(
    password &&
      !add &&
      onsignedin !== undefined &&
      api.passwordSignIn !== undefined &&
      api.passwordSignUp !== undefined &&
      api.setPassword !== undefined
  );
</script>

<div class="sign-in">
  {#if problem}<Banner tone="warn"><span data-testid="sign-in-problem">{problem}</span></Banner
    >{/if}
  {#if providers === undefined}
    <p class="muted" role="status">Loading…</p>
  {:else if providers.length === 0 && !withPassword}
    <p class="muted" data-testid="sign-in-off">Signing in is not set up on this server.</p>
  {:else}
    {#if withPassword && onsignedin}
      <ServerPassword
        api={api as Pick<PeopleApi, 'passwordSignIn' | 'passwordSignUp' | 'setPassword'> &
          Partial<Pick<PeopleApi, 'forgotPassword'>>}
        forgot={reconnecting ? undefined : email ? 'email' : 'owner'}
        mode={reconnecting ? 'make' : 'sign-in'}
        {invite}
        canMake={!reconnecting && (invite !== undefined || canMake)}
        makeWords={reconnecting ? 'Set the username and password' : 'Create account'}
        ondone={onsignedin}
      />
      {#if providers.length > 0}<p class="or"><span>or</span></p>{/if}
    {/if}
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
    {#if !add}
      <p class="muted fine" data-testid="owners-see-all">
        Who can see my account?
        <InfoTip text={OWNERS_SEE_ALL} label="Who can see my account" />
      </p>
    {/if}
  {/if}
</div>

<style>
  .or {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    margin: 0;
    color: var(--gf-text-faint, var(--gf-text-muted));
    font-size: var(--gf-font-size-sm);
    max-width: 22rem;
  }
  .or::before,
  .or::after {
    content: '';
    flex: 1;
    border-top: 1px solid var(--gf-border);
  }
  .fine {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    font-size: var(--gf-font-size-xs, var(--gf-font-size-sm));
  }
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
