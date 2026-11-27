<script lang="ts">
  /**
   * ServerPassword — a username and a password kept on the server (docs/primitives.md
   * §13.8). One form, four jobs:
   *
   * - `sign-in`: sign in. With `canMake`, a line under it switches to making an account.
   * - `make`: make an account. With `invite`, the invite or re-connect link goes along.
   * - `add`: give the signed-in account a username and password.
   * - `change`: change the signed-in account's password. It asks for the one there is.
   *
   * A password is typed, sent once, and never shown again.
   */
  import { Button, Input } from '@gridfpv/components';
  import type { Me } from '@gridfpv/types';
  import {
    PASSWORD_MIN,
    passwordProblem,
    usernameProblem,
    type PeopleApi
  } from '../lib/serverPeople.js';

  type Mode = 'sign-in' | 'make' | 'add' | 'change';

  let {
    api,
    mode: start,
    invite = undefined,
    canMake = false,
    makeWords = 'Make the account',
    oncancel = undefined,
    ondone
  }: {
    api: Pick<PeopleApi, 'passwordSignIn' | 'passwordSignUp' | 'setPassword'>;
    mode: Mode;
    /** The invite, or the re-connect link, the person came with. */
    invite?: string | undefined;
    /** Whether someone signing in may make an account here instead. */
    canMake?: boolean;
    /** What the button says when an account is being made. */
    makeWords?: string;
    /** Shown as a Cancel button when given. */
    oncancel?: (() => void) | undefined;
    /** It worked: here is the account as it now stands. */
    ondone: (me: Me) => void;
  } = $props();

  // The form can go from signing in to making an account and back.
  let mode = $state<Mode>('sign-in');
  $effect(() => {
    mode = start;
  });

  let username = $state('');
  let password = $state('');
  let again = $state('');
  let current = $state('');
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);

  const setting = $derived(mode !== 'sign-in');
  const problem = $derived(
    setting
      ? ((mode === 'change' ? undefined : usernameProblem(username)) ??
          passwordProblem(password, again))
      : undefined
  );
  const ready = $derived.by(() => {
    if (mode === 'sign-in') return username.trim() !== '' && password !== '';
    if (problem) return false;
    const typed = password !== '' && again === password;
    if (mode === 'change') return typed && current !== '';
    return typed && username.trim() !== '';
  });
  const words = $derived(
    mode === 'sign-in'
      ? 'Sign in'
      : mode === 'make'
        ? makeWords
        : mode === 'add'
          ? 'Add the username and password'
          : 'Change the password'
  );

  async function submit(e: Event) {
    e.preventDefault();
    if (!ready || busy) return;
    busy = true;
    error = undefined;
    try {
      const name = username.trim();
      const me =
        mode === 'sign-in'
          ? await api.passwordSignIn(name, password)
          : mode === 'make'
            ? await api.passwordSignUp({
                username: name,
                password,
                ...(invite ? { invite } : {})
              })
            : mode === 'add'
              ? await api.setPassword({ username: name, password })
              : await api.setPassword({ current, password });
      // A password does not stay in the page once it has been used.
      password = again = current = '';
      ondone(me);
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
      // A password that did not work is typed again, not sent again by mistake.
      if (mode === 'sign-in') password = '';
    } finally {
      busy = false;
    }
  }

  function switchTo(next: Mode) {
    mode = next;
    error = undefined;
    password = again = current = '';
  }
</script>

<!-- A refusal is about what was in the form then. Typing again clears it. -->
<form
  class="password"
  onsubmit={submit}
  oninput={() => (error = undefined)}
  aria-label={words}
  data-testid="password-form"
>
  {#if mode !== 'change'}
    <label for="pw-username">Username</label>
    <Input
      id="pw-username"
      maxlength={32}
      autocomplete="username"
      autocapitalize="none"
      spellcheck="false"
      bind:value={username}
      disabled={busy}
    />
  {:else}
    <label for="pw-current">The password you have now</label>
    <Input
      id="pw-current"
      type="password"
      autocomplete="current-password"
      bind:value={current}
      disabled={busy}
    />
  {/if}
  <label for="pw-password">{mode === 'change' ? 'New password' : 'Password'}</label>
  <Input
    id="pw-password"
    type="password"
    maxlength={200}
    autocomplete={setting ? 'new-password' : 'current-password'}
    bind:value={password}
    disabled={busy}
  />
  {#if setting}
    <label for="pw-again">The same password again</label>
    <Input
      id="pw-again"
      type="password"
      maxlength={200}
      autocomplete="new-password"
      bind:value={again}
      disabled={busy}
    />
    <p class="muted">At least {PASSWORD_MIN} characters. A few words in a row is a good one.</p>
  {/if}
  {#if problem}<p class="error" role="alert">{problem}</p>{/if}
  {#if error}<p class="error" role="alert" data-testid="password-error">{error}</p>{/if}
  <div class="actions">
    <Button variant="primary" size="sm" type="submit" disabled={busy || !ready}>{words}</Button>
    {#if oncancel}
      <Button variant="ghost" size="sm" disabled={busy} onclick={oncancel}>Cancel</Button>
    {/if}
  </div>
  {#if canMake && start === 'sign-in'}
    <p class="muted">
      {#if mode === 'sign-in'}
        No account yet?
        <button type="button" class="link" onclick={() => switchTo('make')}
          >Make one with a username and password</button
        >
      {:else}
        Have an account already?
        <button type="button" class="link" onclick={() => switchTo('sign-in')}>Sign in</button>
      {/if}
    </p>
  {/if}
</form>

<style>
  .password {
    display: grid;
    gap: var(--gf-space-2);
    max-width: 22rem;
  }
  label {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  p {
    margin: 0;
  }
  .actions {
    display: flex;
    gap: var(--gf-space-2);
    margin-top: var(--gf-space-1);
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .error {
    color: var(--gf-danger);
    font-size: var(--gf-font-size-sm);
  }
  .link {
    padding: 0;
    border: 0;
    background: none;
    color: var(--gf-accent);
    font: inherit;
    text-decoration: underline;
    cursor: pointer;
  }
</style>
