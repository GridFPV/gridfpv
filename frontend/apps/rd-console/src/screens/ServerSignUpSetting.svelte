<script lang="ts">
  /**
   * ServerSignUpSetting — who may make an account on a server, for an owner of it
   * (docs/primitives.md §13.7).
   *
   * A server starts by asking for an invite. An owner may let anyone who can sign in make
   * an account. Invites work either way.
   */
  import { Banner, Button, Card, Dialog, InfoTip } from '@gridfpv/components';
  import Switch from '../lib/Switch.svelte';
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
      <h2>Open sign-up</h2>
      <InfoTip
        text="On: anyone who can sign in gets an account, in no club. Off: an account needs an invite. Invites work either way."
        label="About open sign-up"
      />
      <span class="grow"></span>
      {#if open !== undefined}
        <span class="state" data-testid="sign-up-state">{open ? 'Anyone' : 'Invite only'}</span>
        <Switch
          checked={open}
          label="Let anyone make an account"
          disabled={busy}
          onchange={(next) => {
            error = undefined;
            if (next) asking = true;
            else void set(false);
          }}
        />
      {/if}
    </div>
    {#if loadError}<Banner tone="danger">{loadError}</Banner>{/if}
    {#if error && !asking}<Banner tone="danger">{error}</Banner>{/if}
  </div>
</Card>

{#if asking}
  <Dialog open={true} title="Let anyone make an account?" onclose={() => (asking = false)}>
    <p>Anyone who can sign in gets an account. A new account is in no club.</p>
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
  .state {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
</style>
