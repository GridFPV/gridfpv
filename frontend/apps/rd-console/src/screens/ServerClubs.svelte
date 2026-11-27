<script lang="ts">
  /**
   * ServerClubs — the clubs on a server, for an owner of it (docs/primitives.md §13.2).
   *
   * Only an owner makes a club or archives one. Each club has a friendly address to hand
   * out, which follows its name. An archived club is read-only and off the list of clubs:
   * its events still open by their links, and an owner can bring it back.
   */
  import { Badge, Banner, Button, Card, Dialog, Input, toast } from '@gridfpv/components';
  import type { ServerClub } from '@gridfpv/types';
  import { formatServerHash } from '../lib/serverHome.js';
  import { clubLink, type PeopleApi } from '../lib/serverPeople.js';

  let {
    api,
    origin = location.origin,
    onchange
  }: {
    api: Pick<PeopleApi, 'serverClubs' | 'makeClub' | 'archiveClub'>;
    /** This server's address, for the addresses shown. A test passes its own. */
    origin?: string;
    /** Told when a club was made, archived or brought back. */
    onchange?: () => void;
  } = $props();

  let clubs = $state<ServerClub[] | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);
  let name = $state('');
  /** The club about to be archived. */
  let archiving = $state<ServerClub | undefined>(undefined);

  const say = (err: unknown): string => (err instanceof Error ? err.message : String(err));

  async function load() {
    try {
      clubs = await api.serverClubs();
      loadError = undefined;
    } catch (err) {
      loadError = say(err);
    }
  }
  $effect(() => {
    void load();
  });

  async function run(what: () => Promise<unknown>): Promise<boolean> {
    if (busy) return false;
    busy = true;
    error = undefined;
    try {
      await what();
      await load();
      onchange?.();
      return true;
    } catch (err) {
      error = say(err);
      await load();
      return false;
    } finally {
      busy = false;
    }
  }

  async function make(e: Event) {
    e.preventDefault();
    if (name.trim() === '') return;
    if (await run(() => api.makeClub(name.trim()))) name = '';
  }

  async function archive() {
    const target = archiving;
    if (!target) return;
    await run(() => api.archiveClub(target.id, true));
    archiving = undefined;
  }

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      toast.success('Copied.');
    } catch {
      toast.error('Could not copy. Select the address and copy it by hand.');
    }
  }
</script>

<Card elevation="sm">
  <div class="block" data-testid="server-clubs">
    <h2>Clubs on this server</h2>
    <p class="muted">
      As an owner you make clubs here, and you can see and change everything in each one.
    </p>
    {#if loadError}
      <Banner tone="danger" title="The clubs could not be read.">{loadError}</Banner>
    {/if}
    {#if error}<Banner tone="danger"><span data-testid="clubs-error">{error}</span></Banner>{/if}
    {#if clubs === undefined}
      {#if !loadError}<p class="muted" role="status">Loading…</p>{/if}
    {:else}
      <ul aria-label="Clubs on this server">
        {#each clubs as club (club.id)}
          {@const link = clubLink(origin, club.word)}
          <li data-testid="owned-club" class:archived={club.archived}>
            <div class="row">
              <a class="who" href={formatServerHash({ kind: 'club', club: club.id })}>{club.name}</a
              >
              {#if club.archived}<Badge tone="neutral">Archived</Badge>{/if}
              {#if club.archived}
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  aria-label="Bring back {club.name}"
                  onclick={() => run(() => api.archiveClub(club.id, false))}>Bring back</Button
                >
              {:else}
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  aria-label="Archive {club.name}"
                  onclick={() => (archiving = club)}>Archive…</Button
                >
              {/if}
            </div>
            <div class="row">
              <code data-testid="club-address">{link}</code>
              <Button
                size="sm"
                variant="ghost"
                aria-label="Copy the address of {club.name}"
                onclick={() => copy(link)}>Copy</Button
              >
            </div>
          </li>
        {/each}
      </ul>
    {/if}

    <form onsubmit={make} aria-label="Make a club">
      <label for="new-club-name">New club</label>
      <div class="row">
        <Input
          id="new-club-name"
          maxlength={80}
          placeholder="The club's name"
          bind:value={name}
          disabled={busy}
        />
        <Button variant="primary" size="sm" type="submit" disabled={busy || name.trim() === ''}
          >Make the club</Button
        >
      </div>
      <p class="muted">
        Then open the club and invite its director. A club's address follows its name, and an old
        address keeps working.
      </p>
    </form>
  </div>
</Card>

{#if archiving}
  {@const target = archiving}
  <Dialog open={true} title="Archive {target.name}?" onclose={() => (archiving = undefined)}>
    <p>
      {target.name} leaves the list of clubs, and nothing in it can be changed. Its director's Grid can
      send no more, and its invites stop working.
    </p>
    <p>Its events still open by their links. You can bring it back at any time.</p>
    {#snippet footer()}
      <Button variant="ghost" onclick={() => (archiving = undefined)}>Cancel</Button>
      <Button variant="danger" disabled={busy} onclick={archive}>Archive</Button>
    {/snippet}
  </Dialog>
{/if}

<style>
  .block,
  form {
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
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--gf-space-3);
  }
  li {
    display: grid;
    gap: var(--gf-space-1);
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--gf-space-2);
  }
  .who {
    font-weight: var(--gf-font-weight-bold);
    overflow-wrap: anywhere;
  }
  li.archived .who {
    color: var(--gf-text-muted);
  }
  code {
    overflow-wrap: anywhere;
    user-select: all;
    font-size: var(--gf-font-size-sm);
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  label {
    font-weight: var(--gf-font-weight-bold);
  }
</style>
