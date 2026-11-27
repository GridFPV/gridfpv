<script lang="ts">
  /**
   * ServerClubManage — a club's own things on a remote server, for those who run it
   * (docs/primitives.md §13.2, §13.6, §13.9): its name, the link tokens its directors'
   * Grid sends with, and which of its events are listed or removed.
   *
   * A link token is shown once, when it is made. After that the list has only what it is
   * for and whether it still works.
   *
   * A hidden event is left out of the club's list for everyone else, and still opens for
   * anyone who has its address. A removed event is gone from the server.
   */
  import { Badge, Banner, Button, Card, Dialog, InfoTip, Input, toast } from '@gridfpv/components';
  import type { LinkTokenMade, LinkTokenView, ServerEvent } from '@gridfpv/types';
  import { formatServerHash } from '../lib/serverHome.js';
  import { tokenStanding, type PeopleApi } from '../lib/serverPeople.js';

  let {
    api,
    club,
    section,
    onchange
  }: {
    api: Pick<
      PeopleApi,
      | 'renameClub'
      | 'linkTokens'
      | 'makeLinkToken'
      | 'revokeLinkToken'
      | 'clubEvents'
      | 'hideEvent'
      | 'removeEvent'
    >;
    club: { id: string; name: string };
    /** Which card to show: the club's events, its link tokens, or its name. */
    section: 'events' | 'tokens' | 'settings';
    /** Told when the club's name or its list of events changed. */
    onchange?: () => void;
  } = $props();

  let tokens = $state<LinkTokenView[] | undefined>(undefined);
  let events = $state<ServerEvent[] | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);
  /** The token just made. It is not shown again. */
  let made = $state<LinkTokenMade | undefined>(undefined);
  /** The token about to be taken back. */
  let revoking = $state<LinkTokenView | undefined>(undefined);
  /** The event about to be removed. */
  let removing = $state<ServerEvent | undefined>(undefined);

  const say = (err: unknown): string => (err instanceof Error ? err.message : String(err));

  async function load() {
    try {
      [tokens, events] = await Promise.all([api.linkTokens(clubId), api.clubEvents(clubId)]);
      loadError = undefined;
    } catch (err) {
      loadError = say(err);
    }
  }
  /**
   * The club by its id alone. The page hands over the club afresh each time it reads its
   * lists, and that is not another club: nothing here is thrown away for it.
   */
  const clubId = $derived(club.id);
  $effect(() => {
    void clubId;
    tokens = undefined;
    events = undefined;
    made = undefined;
    void load();
  });

  async function run(what: () => Promise<unknown>): Promise<boolean> {
    if (busy) return false;
    busy = true;
    error = undefined;
    try {
      await what();
      await load();
      return true;
    } catch (err) {
      error = say(err);
      await load();
      return false;
    } finally {
      busy = false;
    }
  }

  // ── The name ───────────────────────────────────────────────────────────────
  let name = $state('');
  // Only a new name puts the box back. The page hands the club over afresh each time it
  // reads its lists, and that must not wipe out what is being typed.
  const clubName = $derived(club.name);
  $effect(() => {
    name = clubName;
  });
  const canRename = $derived(name.trim() !== '' && name.trim() !== club.name);

  async function rename(e: Event) {
    e.preventDefault();
    if (!canRename) return;
    if (await run(() => api.renameClub(club.id, name.trim()))) {
      toast.success('Saved.');
      onchange?.();
    }
  }

  // ── Link tokens ────────────────────────────────────────────────────────────
  let label = $state('');

  async function make(e: Event) {
    e.preventDefault();
    if (label.trim() === '') return;
    let result: LinkTokenMade | undefined;
    if (
      await run(async () => {
        result = await api.makeLinkToken(club.id, label.trim());
      })
    ) {
      made = result;
      label = '';
    }
  }

  async function revoke() {
    const target = revoking;
    if (!target) return;
    await run(() => api.revokeLinkToken(club.id, target.id));
    if (made?.token.id === target.id) made = undefined;
    revoking = undefined;
  }

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      toast.success('Copied.');
    } catch {
      toast.error('Could not copy. Select the token and copy it by hand.');
    }
  }

  const day = (ms: number): string =>
    new Date(ms).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });

  // ── Events ─────────────────────────────────────────────────────────────────
  async function hide(event: ServerEvent, hidden: boolean) {
    if (await run(() => api.hideEvent(club.id, event.id, hidden))) onchange?.();
  }

  async function remove() {
    const target = removing;
    if (!target) return;
    if (await run(() => api.removeEvent(club.id, target.id))) onchange?.();
    removing = undefined;
  }
</script>

{#if loadError}
  <Banner tone="danger" title="This club's tokens and events could not be read.">{loadError}</Banner
  >
{/if}
{#if error}<Banner tone="danger"><span data-testid="manage-error">{error}</span></Banner>{/if}

{#if section === 'events'}
  <Card elevation="sm">
    <div class="block">
      <h2>Events</h2>
      {#if events === undefined}
        {#if !loadError}<p class="muted" role="status">Loading…</p>{/if}
      {:else if events.length === 0}
        <p class="muted">No events yet.</p>
      {:else}
        <ul aria-label="Events of {club.name}">
          {#each events as event (event.id)}
            <li data-testid="manage-event">
              <a
                class="who"
                href={formatServerHash({ kind: 'event', club: club.id, event: event.id })}
                >{event.name}</a
              >
              {#if event.hidden}<Badge tone="neutral">Hidden</Badge>{/if}
              <Button
                size="sm"
                variant="ghost"
                disabled={busy}
                aria-label="{event.hidden ? 'Show' : 'Hide'} {event.name}"
                onclick={() => hide(event, !event.hidden)}>{event.hidden ? 'Show' : 'Hide'}</Button
              >
              <Button
                size="sm"
                variant="ghost"
                disabled={busy}
                aria-label="Remove {event.name}"
                onclick={() => (removing = event)}>Remove…</Button
              >
            </li>
          {/each}
        </ul>
        <p class="muted">
          A hidden event is left out of the list. Anyone who has its link can still open it.
        </p>
      {/if}
    </div>
  </Card>
{:else if section === 'tokens'}
  <Card elevation="sm">
    <div class="block">
      <div class="row">
        <h2>Link tokens</h2>
        <InfoTip
          text="A director pastes a link token into Grid, on Setup → Server, to send events here."
          label="About link tokens"
        />
      </div>
      <form onsubmit={make} aria-label="New link token">
        <div class="fields">
          <label for="token-label">What it is for</label>
          <Input
            id="token-label"
            maxlength={80}
            placeholder="Ryan's laptop"
            bind:value={label}
            disabled={busy}
          />
        </div>
        <div>
          <Button variant="primary" size="sm" type="submit" disabled={busy || label.trim() === ''}
            >Make a link token</Button
          >
        </div>
      </form>

      {#if made}
        <div class="made" data-testid="token-made">
          <p>
            This is the token for <strong>{made.token.label}</strong>. It is shown this once.
          </p>
          <code data-testid="token-secret">{made.secret}</code>
          <div class="row">
            <Button size="sm" onclick={() => copy(made?.secret ?? '')}>Copy</Button>
            <Button size="sm" variant="ghost" onclick={() => (made = undefined)}>Done</Button>
          </div>
        </div>
      {/if}

      {#if tokens && tokens.length > 0}
        <ul aria-label="Link tokens">
          {#each tokens as token (token.id)}
            {@const works = token.revoked_at === undefined}
            <li data-testid="link-token" class:spent={!works}>
              <span class="who">{token.label}</span>
              <span class="muted">Made {day(token.created_at)}</span>
              <span class="muted">{tokenStanding(token)}</span>
              {#if works}
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  aria-label="Take back the token for {token.label}"
                  onclick={() => (revoking = token)}>Take back…</Button
                >
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  </Card>
{:else}
  <Card elevation="sm">
    <div class="block">
      <h2>Club name</h2>
      <form onsubmit={rename} aria-label="Club name">
        <div class="fields">
          <label for="club-name">Name</label>
          <Input id="club-name" maxlength={80} bind:value={name} disabled={busy} />
        </div>
        <div>
          <Button size="sm" type="submit" disabled={busy || !canRename}>Save</Button>
        </div>
      </form>
    </div>
  </Card>
{/if}

{#if revoking}
  {@const target = revoking}
  <Dialog
    open={true}
    title="Take back the token for {target.label}?"
    onclose={() => (revoking = undefined)}
  >
    <p>It stops working straight away. A Grid that uses it can no longer send events here.</p>
    <p>You can make a new one at any time.</p>
    {#snippet footer()}
      <Button variant="ghost" onclick={() => (revoking = undefined)}>Cancel</Button>
      <Button variant="danger" disabled={busy} onclick={revoke}>Take back</Button>
    {/snippet}
  </Dialog>
{/if}

{#if removing}
  {@const target = removing}
  <Dialog
    open={true}
    title="Remove {target.name} from this server?"
    onclose={() => (removing = undefined)}
  >
    <p>Its results and laps are taken off this server. Nobody can open it here any more.</p>
    <p>
      The director's own copy in Grid is not touched. They can send it again from Setup → Server.
    </p>
    {#snippet footer()}
      <Button variant="ghost" onclick={() => (removing = undefined)}>Cancel</Button>
      <Button variant="danger" disabled={busy} onclick={remove}>Remove</Button>
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
    gap: var(--gf-space-2);
  }
  li {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
  }
  .who {
    flex: 1;
    min-width: 8rem;
    overflow-wrap: anywhere;
    font-weight: var(--gf-font-weight-semibold);
  }
  a.who {
    color: inherit;
  }
  .spent .who {
    font-weight: normal;
    color: var(--gf-text-muted);
  }
  .row {
    display: flex;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
  }
  .fields {
    display: grid;
    grid-template-columns: max-content minmax(0, 20rem);
    align-items: center;
    gap: var(--gf-space-2) var(--gf-space-3);
    font-size: var(--gf-font-size-sm);
  }
  .made {
    display: grid;
    gap: var(--gf-space-2);
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-accent);
    border-radius: var(--gf-radius-sm);
  }
  code {
    font-family: var(--gf-font-mono);
    font-size: var(--gf-font-size-sm);
    padding: var(--gf-space-2);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    overflow-wrap: anywhere;
    user-select: all;
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    font-weight: normal;
  }
</style>
