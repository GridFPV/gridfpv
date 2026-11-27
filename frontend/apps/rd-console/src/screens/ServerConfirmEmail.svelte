<script lang="ts">
  /**
   * ServerConfirmEmail — the page a link sent to an email address opens
   * (docs/primitives.md §13.8). Opening it confirms the address is its account's.
   *
   * The link works once. Whoever opens it need not be signed in.
   */
  import { Card } from '@gridfpv/components';
  import { formatServerHash } from '../lib/serverHome.js';
  import type { PeopleApi } from '../lib/serverPeople.js';

  let {
    api,
    secret,
    onconfirmed = undefined
  }: {
    api: Pick<PeopleApi, 'confirmEmail'>;
    secret: string;
    /** Told once the address is confirmed, so the account can be read again. */
    onconfirmed?: (() => void) | undefined;
  } = $props();

  /** The address that was confirmed. */
  let address = $state<string | undefined>(undefined);
  /** The link does not work. */
  let dead = $state(false);

  $effect(() => {
    const asked = secret;
    address = undefined;
    dead = false;
    let stopped = false;
    api
      .confirmEmail(asked)
      .then((done) => {
        if (stopped) return;
        address = done.address;
        onconfirmed?.();
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
  <div class="block" data-testid="confirm-email">
    {#if address}
      <p class="lead" data-testid="email-confirmed">{address} is confirmed.</p>
      <p>If you forget your password, you can now have a link sent to this address.</p>
      <p><a href={formatServerHash({ kind: 'account' })}>Your account</a></p>
    {:else if dead}
      <p class="lead" data-testid="email-link-dead">This link does not work</p>
      <p>
        It may have been used already, or have run out. On your account page, press
        <strong>Send the link again</strong>.
      </p>
      <p><a href={formatServerHash({ kind: 'account' })}>Your account</a></p>
    {:else}
      <p class="muted" role="status">Confirming…</p>
    {/if}
  </div>
</Card>

<style>
  .block {
    display: grid;
    gap: var(--gf-space-3);
  }
  p {
    margin: 0;
    max-width: 65ch;
  }
  .lead {
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-semibold);
  }
  .muted {
    color: var(--gf-text-muted);
  }
</style>
