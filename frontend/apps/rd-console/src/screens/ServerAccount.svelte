<script lang="ts">
  /**
   * ServerAccount — the signed-in account on a remote server (docs/primitives.md §13.8):
   * its name, the ways it signs in, the clubs it is in, and the pilots it holds.
   *
   * An account can hold several sign-ins and keeps at least one. Another is added here,
   * signed in: that is the only way two sign-ins come to belong to one account.
   */
  import { Badge, Banner, Button, Card, Dialog, Input } from '@gridfpv/components';
  import type { Me, MyAccount, MyClub, SignInProvider } from '@gridfpv/types';
  import { formatServerHash } from '../lib/serverHome.js';
  import { lookedAfterClubs, providerName, roleWord, type PeopleApi } from '../lib/serverPeople.js';
  import ServerJoinClubs from './ServerJoinClubs.svelte';
  import ServerMyPilots from './ServerMyPilots.svelte';
  import ServerProfile from './ServerProfile.svelte';
  import ServerSignIn from './ServerSignIn.svelte';

  let {
    api,
    account,
    providers,
    clubs,
    onchange,
    onreread,
    onsignedout
  }: {
    api: Pick<
      PeopleApi,
      | 'rename'
      | 'removeSignIn'
      | 'signOut'
      | 'signInUrl'
      | 'clubPilots'
      | 'askFor'
      | 'dropClaim'
      | 'setPrimary'
      | 'setProfile'
      | 'serverClubs'
      | 'joinClub'
      | 'leaveClub'
    >;
    account: MyAccount;
    providers: readonly SignInProvider[] | undefined;
    /** Every club on the server, for an owner's links. */
    clubs: readonly { id: string; name: string }[];
    /** The account changed: here it is as it now stands. */
    onchange: (me: Me) => void;
    /** Something about the account changed on the server: read it again. */
    onreread: () => void | Promise<void>;
    onsignedout: () => void;
  } = $props();

  let name = $state('');
  let editing = $state(false);
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);
  /** The club the person is about to leave. */
  let leaving = $state<MyClub | undefined>(undefined);

  const say = (err: unknown): string => (err instanceof Error ? err.message : String(err));
  const lookedAfter = $derived(lookedAfterClubs(account, clubs));
  /** The providers the account does not sign in with yet. */
  const toAdd = $derived(
    providers?.filter((p) => !account.sign_ins.some((held) => held.provider === p.id))
  );

  async function run(what: () => Promise<Me | void>): Promise<boolean> {
    if (busy) return false;
    busy = true;
    error = undefined;
    try {
      const me = await what();
      if (me) onchange(me);
      return true;
    } catch (err) {
      error = say(err);
      return false;
    } finally {
      busy = false;
    }
  }

  async function saveName(e: Event) {
    e.preventDefault();
    if (await run(() => api.rename(name))) editing = false;
  }

  async function leave() {
    const target = leaving;
    if (!target) return;
    if (await run(() => api.leaveClub(target.club.id))) leaving = undefined;
  }

  async function signOut() {
    if (await run(() => api.signOut())) onsignedout();
  }
</script>

<div class="account" data-testid="server-account">
  {#if error}<Banner tone="danger">{error}</Banner>{/if}

  <Card elevation="sm">
    <div class="block">
      {#if editing}
        <form class="row" onsubmit={saveName} aria-label="Change your name">
          <label class="visually-hidden" for="account-name">Your name</label>
          <Input id="account-name" maxlength={80} bind:value={name} disabled={busy} />
          <Button variant="primary" size="sm" type="submit" disabled={busy || name.trim() === ''}
            >Save</Button
          >
          <Button variant="ghost" size="sm" disabled={busy} onclick={() => (editing = false)}
            >Cancel</Button
          >
        </form>
      {:else}
        <div class="row">
          <p class="name" data-testid="account-name">{account.name}</p>
          <Button
            size="sm"
            variant="ghost"
            onclick={() => ((name = account.name), (editing = true))}>Change name</Button
          >
        </div>
      {/if}
      {#if account.owner}<p class="muted">You are an owner of this server.</p>{/if}
    </div>
  </Card>

  <Card elevation="sm">
    <div class="block">
      <h2>Clubs</h2>
      {#if account.clubs.length === 0}
        <p class="muted">
          You are not in a club yet. Join one below, or use an invite from a club's director.
        </p>
      {:else}
        <ul aria-label="Your clubs">
          {#each account.clubs as entry (entry.club.id)}
            <li data-testid="my-club">
              <span>{entry.club.name}</span>
              <span class="muted">{roleWord(entry.role)}</span>
              {#if account.clubs.length > 1}
                {#if account.primary === entry.club.id}
                  <Badge tone="neutral">Primary</Badge>
                {:else}
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={busy}
                    aria-label="Make {entry.club.name} your primary club"
                    onclick={() => run(() => api.setPrimary(entry.club.id))}>Make primary</Button
                  >
                {/if}
              {/if}
              <Button
                size="sm"
                variant="ghost"
                disabled={busy}
                aria-label="Leave {entry.club.name}"
                onclick={() => ((error = undefined), (leaving = entry))}>Leave…</Button
              >
            </li>
          {/each}
        </ul>
        {#if account.clubs.length > 1}
          <p class="muted">The front page opens on your primary club.</p>
        {/if}
      {/if}
      {#if lookedAfter.length > 0}
        <ul aria-label="Clubs you look after">
          {#each lookedAfter as { club, runs } (club.id)}
            <li>
              <a href={formatServerHash({ kind: 'club', club: club.id })}
                >{runs ? 'Members, invites and pilots' : 'Pilots'} of {club.name}</a
              >
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  </Card>

  <ServerJoinClubs {api} {account} {onchange} />

  <ServerProfile {api} {account} {onchange} />

  <ServerMyPilots {api} {account} onchange={onreread} />

  <Card elevation="sm">
    <div class="block">
      <h2>How you sign in</h2>
      <ul aria-label="How you sign in">
        {#each account.sign_ins as held (held.provider + ':' + held.subject)}
          <li>
            <span>{providerName(held.provider, providers ?? [])}</span>
            <span class="muted">{held.label}</span>
            {#if account.sign_ins.length > 1}
              <Button
                size="sm"
                variant="ghost"
                disabled={busy}
                aria-label="Remove {providerName(held.provider, providers ?? [])} {held.label}"
                onclick={() => run(() => api.removeSignIn(held.provider, held.subject))}
                >Remove</Button
              >
            {/if}
          </li>
        {/each}
      </ul>
      {#if toAdd && toAdd.length > 0}
        <ServerSignIn {api} providers={toAdd} add />
      {/if}
    </div>
  </Card>

  <div>
    <Button variant="ghost" disabled={busy} onclick={signOut}>Sign out</Button>
  </div>
</div>

{#if leaving}
  {@const target = leaving}
  <Dialog open={true} title="Leave {target.club.name}?" onclose={() => (leaving = undefined)}>
    <p>You will not be in {target.club.name} any more.</p>
    <p>
      Pilots you hold in this club are let go, so its results no longer show under your account. The
      results themselves stay with the club.
    </p>
    <p class="muted">Your account and your other clubs stay as they are.</p>
    {#if error}<Banner tone="danger">{error}</Banner>{/if}
    {#snippet footer()}
      <Button variant="ghost" onclick={() => (leaving = undefined)}>Cancel</Button>
      <Button variant="danger" disabled={busy} onclick={leave}>Leave</Button>
    {/snippet}
  </Dialog>
{/if}

<style>
  .account,
  .block {
    display: grid;
    gap: var(--gf-space-3);
  }
  h2 {
    margin: 0;
    font-size: var(--gf-font-size-lg);
  }
  p {
    margin: 0;
  }
  .name {
    font-size: var(--gf-font-size-xl);
    font-weight: var(--gf-font-weight-bold);
    overflow-wrap: anywhere;
    flex: 1;
    min-width: 0;
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--gf-space-2);
  }
  li {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
    overflow-wrap: anywhere;
  }
  .muted {
    color: var(--gf-text-muted);
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
