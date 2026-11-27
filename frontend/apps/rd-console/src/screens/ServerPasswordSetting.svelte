<script lang="ts">
  /**
   * ServerPasswordSetting — whether people may sign in with a username and password kept
   * on this server, for an owner of it (docs/primitives.md §13.8).
   *
   * Off on a new server. Switching it off locks out anyone whose only way in is a
   * password, so the owner is asked first.
   */
  import { Banner, Button, Card, Dialog, InfoTip } from '@gridfpv/components';
  import Switch from '../lib/Switch.svelte';
  import type { PeopleApi } from '../lib/serverPeople.js';

  let {
    api,
    onchange
  }: {
    api: Pick<PeopleApi, 'serverInfo' | 'setPasswordSignIn'>;
    /** Told what the setting now is. */
    onchange?: (on: boolean) => void;
  } = $props();

  let on = $state<boolean | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);
  /** The owner is being asked whether to switch passwords off. */
  let asking = $state(false);

  const say = (err: unknown): string => (err instanceof Error ? err.message : String(err));

  $effect(() => {
    api
      .serverInfo()
      .then((info) => {
        on = info.password_sign_in ?? false;
        loadError = undefined;
      })
      .catch((err) => (loadError = say(err)));
  });

  async function set(next: boolean) {
    if (busy) return;
    busy = true;
    error = undefined;
    try {
      on = (await api.setPasswordSignIn(next)).password_sign_in ?? false;
      asking = false;
      onchange?.(on);
    } catch (err) {
      error = say(err);
    } finally {
      busy = false;
    }
  }
</script>

<Card elevation="sm">
  <div class="block" data-testid="password-setting">
    <div class="row">
      <h2>Passwords</h2>
      <InfoTip
        text="People sign in with a username and a password this server keeps. Only a scrambled form is kept. A forgotten one is set again with a re-connect link."
        label="About passwords"
      />
      <span class="grow"></span>
      {#if on !== undefined}
        <span class="state" data-testid="password-state">{on ? 'On' : 'Off'}</span>
        <Switch
          checked={on}
          label="Let people sign in with a password"
          disabled={busy}
          onchange={(next) => {
            error = undefined;
            if (next) void set(true);
            else asking = true;
          }}
        />
      {/if}
    </div>
    {#if loadError}<Banner tone="danger">{loadError}</Banner>{/if}
    {#if error && !asking}<Banner tone="danger">{error}</Banner>{/if}
  </div>
</Card>

{#if asking}
  <Dialog open={true} title="Switch passwords off?" onclose={() => (asking = false)}>
    <p>Anyone whose only way in is a password is locked out until you switch it on again.</p>
    {#if error}<Banner tone="danger">{error}</Banner>{/if}
    {#snippet footer()}
      <Button variant="ghost" onclick={() => (asking = false)}>Cancel</Button>
      <Button variant="danger" disabled={busy} onclick={() => set(false)}>Switch off</Button>
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
