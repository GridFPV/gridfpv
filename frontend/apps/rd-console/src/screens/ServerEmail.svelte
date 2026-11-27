<script lang="ts">
  /**
   * ServerEmail — the signed-in account's email address (docs/primitives.md §13.8).
   *
   * It is optional. A link is sent to the address, and it is used for nothing until the
   * link is opened. After that the person can set a forgotten password by themselves.
   * Nobody else is shown the address.
   */
  import { Badge, Button, Card, InfoTip, Input } from '@gridfpv/components';
  import type { Me, MyAccount } from '@gridfpv/types';
  import { emailProblem, type PeopleApi } from '../lib/serverPeople.js';

  let {
    api,
    account,
    onchange
  }: {
    api: Pick<PeopleApi, 'setEmail' | 'removeEmail'>;
    account: MyAccount;
    /** The account changed: here it is as it now stands. */
    onchange: (me: Me) => void;
  } = $props();

  /** The address being typed, when the form is open. */
  let typing = $state<string | undefined>(undefined);
  let error = $state<string | undefined>(undefined);
  /** A link was sent to this address a moment ago. */
  let sentTo = $state<string | undefined>(undefined);
  let busy = $state(false);

  const email = $derived(account.email);
  const problem = $derived(typing === undefined ? undefined : emailProblem(typing));

  async function run(what: () => Promise<Me>, after?: (me: Me) => void) {
    if (busy) return;
    busy = true;
    error = undefined;
    try {
      const me = await what();
      onchange(me);
      after?.(me);
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  async function save(e: Event) {
    e.preventDefault();
    const address = typing?.trim() ?? '';
    if (address === '' || problem) return;
    await run(
      () => api.setEmail(address),
      (me) => {
        // The address as the server keeps it, which may be tidier than what was typed.
        sentTo = me.account?.email?.address;
        typing = undefined;
      }
    );
  }
</script>

<Card elevation="sm">
  <div class="block" data-testid="account-email">
    <div class="row">
      <h2>Email</h2>
      <InfoTip
        text="Optional. With a confirmed address you can set a forgotten password by yourself. Nobody else is shown it."
        label="About email"
      />
      <span class="grow"></span>
      {#if email}
        <Badge tone={email.confirmed ? 'success' : 'warn'}>
          <span data-testid="email-state">{email.confirmed ? 'Confirmed' : 'Not confirmed'}</span>
        </Badge>
      {/if}
    </div>
    {#if error}<p class="error" role="alert" data-testid="email-error">{error}</p>{/if}

    {#if typing !== undefined}
      <!-- A refusal is about what was in the form then. Typing again clears it. -->
      <form class="row" onsubmit={save} oninput={() => (error = undefined)} aria-label="Email">
        <label class="visually-hidden" for="account-email-box">Email address</label>
        <Input
          id="account-email-box"
          type="email"
          maxlength={254}
          autocomplete="email"
          placeholder="you@example.org"
          bind:value={typing}
          disabled={busy}
        />
        <Button
          variant="primary"
          size="sm"
          type="submit"
          disabled={busy || typing.trim() === '' || problem !== undefined}>Save</Button
        >
        <Button variant="ghost" size="sm" disabled={busy} onclick={() => (typing = undefined)}
          >Cancel</Button
        >
      </form>
      {#if problem}<p class="error" role="alert">{problem}</p>{/if}
    {:else if email}
      <p class="address" data-testid="email-address">{email.address}</p>
      {#if !email.confirmed}
        <p data-testid="email-waiting">
          {sentTo === email.address ? 'A link is on its way.' : 'A link was sent to this address.'}
          Open it to confirm the address is yours. It works for one day.
        </p>
      {/if}
      <div class="row">
        {#if !email.confirmed}
          <Button
            size="sm"
            disabled={busy}
            onclick={() =>
              run(
                () => api.setEmail(email.address),
                () => (sentTo = email.address)
              )}>Send the link again</Button
          >
        {/if}
        <Button
          size="sm"
          variant="ghost"
          disabled={busy}
          onclick={() => ((error = undefined), (typing = email.address))}>Change</Button
        >
        <Button
          size="sm"
          variant="ghost"
          disabled={busy}
          onclick={() =>
            run(
              () => api.removeEmail(),
              () => (sentTo = undefined)
            )}>Remove</Button
        >
      </div>
    {:else}
      <div>
        <Button size="sm" disabled={busy} onclick={() => ((error = undefined), (typing = ''))}
          >Add an email address</Button
        >
      </div>
    {/if}
  </div>
</Card>

<style>
  .block {
    display: grid;
    gap: var(--gf-space-3);
  }
  .row {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--gf-space-2) var(--gf-space-3);
  }
  .grow {
    flex: 1;
  }
  h2 {
    margin: 0;
    font-size: var(--gf-font-size-lg);
  }
  p {
    margin: 0;
    max-width: 65ch;
  }
  .address {
    font-weight: var(--gf-font-weight-semibold);
  }
  .error {
    color: var(--gf-danger);
    font-size: var(--gf-font-size-sm);
  }
  .visually-hidden {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
</style>
