<script lang="ts">
  /**
   * ServerPasswordSetting — whether people may sign in with a username and password kept
   * on this server, for an owner of it (docs/primitives.md §13.8).
   *
   * Off on a new server. Switching it off locks out anyone whose only way in is a
   * password, so the owner is asked first.
   */
  import { Badge, Banner, Button, Card, Dialog } from '@gridfpv/components';
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
      <h2>Signing in with a password</h2>
      {#if on !== undefined}
        <Badge tone={on ? 'info' : 'neutral'}>
          <span data-testid="password-state">{on ? 'On' : 'Off'}</span>
        </Badge>
      {/if}
    </div>
    {#if loadError}<Banner tone="danger">{loadError}</Banner>{/if}
    {#if error && !asking}<Banner tone="danger">{error}</Banner>{/if}
    {#if on === true}
      <p>People may sign in with a username and a password that this server keeps.</p>
      <div>
        <Button size="sm" disabled={busy} onclick={() => ((error = undefined), (asking = true))}
          >Switch passwords off…</Button
        >
      </div>
    {:else if on === false}
      <p>
        People sign in only with the sign-in services set up for this server, such as Discord or
        Google.
      </p>
      <div>
        <Button size="sm" disabled={busy} onclick={() => set(true)}
          >Let people use a password</Button
        >
      </div>
    {/if}
    <p class="muted">
      The server keeps a scrambled form of each password, never the password itself. If someone
      forgets theirs, send them a re-connect link from the club's Members list.
    </p>
  </div>
</Card>

{#if asking}
  <Dialog open={true} title="Switch passwords off?" onclose={() => (asking = false)}>
    <p>Nobody can sign in with a username and password while this is off.</p>
    <p>Anyone whose only way in is a password is locked out until you switch it on again.</p>
    <p class="muted">Their passwords are kept, and work again when it is back on.</p>
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
