<script lang="ts">
  /**
   * ServerSignUpSetting — who may make an account on a server, for an owner of it
   * (docs/primitives.md §13.7).
   *
   * A server starts by asking for an invite. An owner may let anyone who can sign in make
   * an account. Invites work either way.
   */
  import { Badge, Banner, Button, Card, Dialog } from '@gridfpv/components';
  import type { PeopleApi } from '../lib/serverPeople.js';

  let {
    api,
    onchange
  }: {
    api: Pick<PeopleApi, 'serverInfo' | 'setOpenSignUp'>;
    /** Told what the setting now is. */
    onchange?: (open: boolean) => void;
  } = $props();

  let open = $state<boolean | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);
  /** The owner is being asked whether to let anyone in. */
  let asking = $state(false);

  const say = (err: unknown): string => (err instanceof Error ? err.message : String(err));

  $effect(() => {
    api
      .serverInfo()
      .then((info) => {
        open = info.open_sign_up;
        loadError = undefined;
      })
      .catch((err) => (loadError = say(err)));
  });

  async function set(next: boolean) {
    if (busy) return;
    busy = true;
    error = undefined;
    try {
      open = (await api.setOpenSignUp(next)).open_sign_up;
      asking = false;
      onchange?.(open);
    } catch (err) {
      error = say(err);
    } finally {
      busy = false;
    }
  }
</script>

<Card elevation="sm">
  <div class="block" data-testid="sign-up-setting">
    <div class="row">
      <h2>Who may make an account</h2>
      {#if open !== undefined}
        <Badge tone={open ? 'info' : 'neutral'}>
          <span data-testid="sign-up-state">{open ? 'Anyone' : 'Invite only'}</span>
        </Badge>
      {/if}
    </div>
    {#if loadError}<Banner tone="danger">{loadError}</Banner>{/if}
    {#if error && !asking}<Banner tone="danger">{error}</Banner>{/if}
    {#if open === true}
      <p>
        Anyone who can sign in may make an account. A new account is in no club until it joins one.
      </p>
      <div>
        <Button size="sm" disabled={busy} onclick={() => set(false)}>Ask for an invite again</Button
        >
      </div>
    {:else if open === false}
      <p>An account needs an invite. You, and each club's director, hand them out.</p>
      <div>
        <Button size="sm" disabled={busy} onclick={() => ((error = undefined), (asking = true))}
          >Let anyone make an account…</Button
        >
      </div>
    {/if}
    <p class="muted">Invites work either way.</p>
  </div>
</Card>

{#if asking}
  <Dialog open={true} title="Let anyone make an account?" onclose={() => (asking = false)}>
    <p>Anyone who can sign in with one of this server's sign-in services gets an account.</p>
    <p>A new account is in no club. It sees what any visitor sees until it joins a club.</p>
    <p class="muted">You can go back to invites at any time. Accounts already made stay.</p>
    {#if error}<Banner tone="danger">{error}</Banner>{/if}
    {#snippet footer()}
      <Button variant="ghost" onclick={() => (asking = false)}>Cancel</Button>
      <Button variant="primary" disabled={busy} onclick={() => set(true)}>Let anyone in</Button>
    {/snippet}
  </Dialog>
{/if}

<style>
  .block {
    display: grid;
    gap: var(--gf-space-3);
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
  }
  h2 {
    flex: 1;
    margin: 0;
    font-size: var(--gf-font-size-lg);
  }
  p {
    margin: 0;
    max-width: 65ch;
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
</style>
