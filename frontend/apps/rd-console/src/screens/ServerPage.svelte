<script lang="ts">
  /**
   * ServerPage — sending events to clubs on Grid servers (docs/primitives.md §13).
   *
   * The director keeps a list of clubs: each is a server's address and a link token for
   * one club there. An event goes to one club, or to none. Grid connects only to the club
   * of the open event, and says here how that is going.
   *
   * An event the server holds a different copy of is never overwritten on its own: the
   * director says so here. A link token is typed in and never shown back.
   */
  import { Badge, Banner, Button, Card, Dialog, Input, Select } from '@gridfpv/components';
  import type { CertificateLook, LinkClub, LinkEvent, LinkView } from '@gridfpv/types';
  import Page from '../Page.svelte';
  import {
    certificateVerdict,
    changed,
    clubName,
    eventLabel,
    eventNote,
    eventTone,
    eventsOf,
    fingerprintRows,
    formOf,
    formProblem,
    linkLabel,
    linkLine,
    linkProblem,
    linkTone,
    openEvent,
    refreshEvery,
    trustOf,
    settingsOf,
    type ClubForm
  } from '../lib/serverLink.js';
  import type { Session } from '../lib/session.svelte.js';

  let {
    session
  }: {
    session: Pick<
      Session,
      | 'getServerLink'
      | 'saveLinkClub'
      | 'removeLinkClub'
      | 'setEventLinkClub'
      | 'replaceOnServer'
      | 'lookAtServerCertificate'
    >;
  } = $props();

  let view = $state<LinkView | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);
  /** The event whose copy on the server the director is about to replace. */
  let replacing = $state<LinkEvent | undefined>(undefined);
  /** The saved club the director is about to remove. */
  let removing = $state<LinkClub | undefined>(undefined);

  /** The club form, when it is open: for the saved club with `id`, or a new one. */
  let editing = $state<{ id: string | undefined; form: ClubForm } | undefined>(undefined);
  /** The saved club the open form is for. Nothing for a new club. */
  const edited = $derived(
    editing && view ? view.clubs.find((club) => club.id === editing?.id) : undefined
  );

  const say = (err: unknown): string => (err instanceof Error ? err.message : String(err));

  async function load() {
    try {
      view = await session.getServerLink();
      loadError = undefined;
    } catch (err) {
      loadError = say(err);
    }
  }

  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  function schedule() {
    clearTimeout(timer);
    if (stopped) return;
    timer = setTimeout(async () => {
      await load();
      schedule();
    }, refreshEvery(view));
  }
  $effect(() => {
    stopped = false;
    void load().then(schedule);
    return () => {
      stopped = true;
      clearTimeout(timer);
    };
  });

  async function change(what: () => Promise<LinkView | undefined>): Promise<boolean> {
    if (busy) return false;
    busy = true;
    error = undefined;
    try {
      const next = await what();
      if (!next) return false;
      view = next;
      schedule();
      return true;
    } catch (err) {
      error = say(err);
      return false;
    } finally {
      busy = false;
    }
  }

  // ── The list of clubs ───────────────────────────────────────────────────────
  const problem = $derived(editing ? formProblem(editing.form, edited) : undefined);
  const dirty = $derived(editing !== undefined && changed(editing.form, edited));

  function edit(club: LinkClub | undefined) {
    error = undefined;
    looked = undefined;
    lookError = undefined;
    editing = { id: club?.id, form: formOf(club) };
  }

  async function save(e: Event) {
    e.preventDefault();
    if (!editing || problem) return;
    const { id, form } = editing;
    if (await change(() => session.saveLinkClub(id, settingsOf(form)))) editing = undefined;
  }

  async function remove() {
    const target = removing;
    if (!target) return;
    if (await change(() => session.removeLinkClub(target.id))) {
      removing = undefined;
      if (editing?.id === target.id) editing = undefined;
    }
  }

  // ── Which club an event goes to ─────────────────────────────────────────────
  async function send(event: LinkEvent, e: Event) {
    const select = e.currentTarget as HTMLSelectElement;
    const club = select.value === '' ? null : select.value;
    // A refusal leaves the event where it was, and the box says so again.
    if (!(await change(() => session.setEventLinkClub(event.id, club)))) {
      select.value = event.club ?? '';
    }
  }

  // ── The server's own certificate (§13.5) ────────────────────────────────────
  /** What Grid found when it looked at the server's certificate. */
  let looked = $state<CertificateLook | undefined>(undefined);
  let looking = $state(false);
  let lookError = $state<string | undefined>(undefined);
  const verdict = $derived(looked ? certificateVerdict(looked) : undefined);

  async function lookAt() {
    if (!editing || looking) return;
    looking = true;
    lookError = undefined;
    try {
      const found = await session.lookAtServerCertificate({
        url: settingsOf(editing.form).url,
        trust: trustOf(editing.form)
      });
      if (found) looked = found;
    } catch (err) {
      lookError = say(err);
    } finally {
      looking = false;
    }
  }

  /**
   * The director accepts the certificate they were shown. It goes in the form, to be kept
   * with the club when the form is saved.
   */
  function accept() {
    const found = looked;
    if (!editing || !found) return;
    editing.form.extra = true;
    editing.form.pem = found.pem.trim();
    looked = undefined;
  }

  async function replace() {
    const target = replacing;
    if (!target) return;
    if (await change(() => session.replaceOnServer(target.id))) replacing = undefined;
  }
</script>

<Page title="Server" help="server" lede="Send your events to your club on a Grid server.">
  {#if loadError}
    <Banner tone="danger" title="Grid could not read its server setting.">{loadError}</Banner>
  {/if}
  {#if view}
    {@const open = openEvent(view)}
    <Card elevation="sm">
      <section class="link" aria-label="The open event" data-testid="server-link">
        <header>
          <div class="title">
            <h2>{open ? open.name : 'No event is open'}</h2>
          </div>
          <Badge tone={linkTone(view.state)}>
            <span data-testid="link-state">{linkLabel(view)}</span>
          </Badge>
        </header>
        <p class="muted" data-testid="link-line">{linkLine(view)}</p>
        {#if linkProblem(view.state)}
          <Banner tone="warn"
            ><span data-testid="link-problem">{linkProblem(view.state)}</span></Banner
          >
        {/if}
      </section>
    </Card>

    <Card elevation="sm">
      <section class="clubs" aria-label="Clubs">
        <header>
          <div class="title">
            <h2>Clubs</h2>
          </div>
          {#if !editing}
            <Button size="sm" disabled={busy} onclick={() => edit(undefined)}>Add a club</Button>
          {/if}
        </header>
        <p class="muted">
          A club here is a server's address and a link token for your club on it. Save each club you
          run events for.
        </p>
        {#if error && !editing && !removing && !replacing}
          <Banner tone="danger">{error}</Banner>
        {/if}
        {#if view.clubs.length === 0}
          {#if !editing}<p class="muted" data-testid="no-clubs">No clubs saved yet.</p>{/if}
        {:else}
          <ul aria-label="Saved clubs">
            {#each view.clubs as club (club.id)}
              <li data-testid="link-club">
                <span class="name">{clubName(club)}</span>
                <span class="muted">{club.url}</span>
                {#if view.club === club.id && view.state === 'Connected'}
                  <Badge tone="success">Connected</Badge>
                {/if}
                <span class="grow"></span>
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  aria-label={`Change ${clubName(club)}`}
                  onclick={() => edit(club)}>Change…</Button
                >
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  aria-label={`Remove ${clubName(club)}`}
                  onclick={() => {
                    error = undefined;
                    removing = club;
                  }}>Remove…</Button
                >
              </li>
            {/each}
          </ul>
        {/if}

        {#if editing}
          <!-- A refusal is about what was in the form then. Typing again clears it. -->
          <form
            class="club-form"
            onsubmit={save}
            oninput={() => (error = undefined)}
            aria-label={edited ? `Change ${clubName(edited)}` : 'Add a club'}
            data-testid="club-form"
          >
            <h3>{edited ? `Change ${clubName(edited)}` : 'Add a club'}</h3>
            {#if error}<Banner tone="danger">{error}</Banner>{/if}
            <div class="fields">
              <label for="server-url">Address</label>
              <Input
                id="server-url"
                type="url"
                autocomplete="off"
                spellcheck="false"
                placeholder="https://grid.example.org"
                bind:value={editing.form.url}
                disabled={busy}
              />
              <label for="server-token">Link token</label>
              <Input
                id="server-token"
                type="password"
                autocomplete="off"
                spellcheck="false"
                placeholder={edited ? 'Set. Type a new one to change it' : 'gfs_…'}
                bind:value={editing.form.token}
                disabled={busy}
              />
              <label for="server-name">Name</label>
              <Input
                id="server-name"
                autocomplete="off"
                placeholder={edited?.club?.name ??
                  "Your own name for it. Leave empty to use the club's"}
                bind:value={editing.form.name}
                disabled={busy}
              />
            </div>

            <label class="extra">
              <input type="checkbox" bind:checked={editing.form.extra} disabled={busy} />
              The server uses its own certificate
            </label>
            {#if editing.form.extra}
              <p class="muted">
                Let Grid look at the certificate the server shows, and accept it. Or paste the
                certificate, or its certificate authority, in the box.
              </p>
              <div>
                <Button
                  size="sm"
                  disabled={busy || looking || editing.form.url.trim() === ''}
                  onclick={lookAt}
                  >{looking ? 'Looking…' : "Look at the server's certificate"}</Button
                >
              </div>
              {#if lookError}
                <p class="error" role="alert" data-testid="certificate-problem">{lookError}</p>
              {/if}
              <textarea
                aria-label="The server's certificate, or its certificate authority"
                rows="6"
                spellcheck="false"
                placeholder="-----BEGIN CERTIFICATE-----"
                bind:value={editing.form.pem}
                disabled={busy}
              ></textarea>
            {/if}

            {#if problem && dirty && (editing.form.url.trim() !== '' || editing.form.token.trim() !== '')}
              <p class="error" role="alert">{problem}</p>
            {/if}
            <div class="actions">
              <Button
                variant="primary"
                size="sm"
                type="submit"
                disabled={busy || !dirty || problem !== undefined}>Save</Button
              >
              <Button
                variant="ghost"
                size="sm"
                disabled={busy}
                onclick={() => (editing = undefined)}>Cancel</Button
              >
            </div>
          </form>
        {/if}
      </section>
    </Card>

    {#if view.events.length > 0}
      <Card elevation="sm">
        <div class="events">
          <h2>Events</h2>
          <p class="muted">
            Pick the club each event is sent to. A new event is not sent until you pick one.
          </p>
          <ul aria-label="Events, where each is sent, and how much of each is on the server">
            {#each view.events as event (event.id)}
              <li data-testid="link-event">
                <span class="name">{event.name}</span>
                {#if event.id === view.open}<Badge tone="info">Open</Badge>{/if}
                <span class="where">
                  <Select
                    size="sm"
                    value={event.club ?? ''}
                    aria-label={`Where ${event.name} is sent`}
                    disabled={busy || view.clubs.length === 0}
                    onchange={(e: Event) => send(event, e)}
                  >
                    <option value="">Not sent</option>
                    {#each view.clubs as club (club.id)}
                      <option value={club.id}>{clubName(club)}</option>
                    {/each}
                  </Select>
                </span>
                {#if event.state !== 'NoClub'}
                  <Badge tone={eventTone(event)}>{eventLabel(event)}</Badge>
                {/if}
                {#if eventNote(event, view)}
                  <span class="muted">{eventNote(event, view)}</span>
                {/if}
                {#if event.state === 'Different'}
                  <Button size="sm" disabled={busy} onclick={() => (replacing = event)}
                    >Replace the server's copy…</Button
                  >
                {:else if event.state === 'Removed'}
                  <Button size="sm" disabled={busy} onclick={() => (replacing = event)}
                    >Send it again…</Button
                  >
                {/if}
              </li>
            {/each}
          </ul>
        </div>
      </Card>
    {/if}
  {/if}
</Page>

{#if looked && verdict}
  <Dialog open={true} title="The server's certificate" onclose={() => (looked = undefined)}>
    {#if verdict === 'trusted'}
      <p data-testid="certificate-verdict">
        Grid already trusts this server's certificate. There is nothing to accept.
      </p>
    {:else}
      <p>This is the fingerprint of the certificate the server showed:</p>
      <p class="fingerprint" data-testid="certificate-fingerprint">
        {#each fingerprintRows(looked.fingerprint) as row (row)}<span>{row}</span>{/each}
      </p>
      {#if verdict === 'accept'}
        <p data-testid="certificate-verdict">
          The server made this certificate for itself. Ask whoever runs the server to read you the
          fingerprint of theirs. <strong
            >Accept it only if every letter and number is the same.</strong
          >
        </p>
        <p class="muted">
          Grid will then trust this one certificate, for this server, and no other. It is kept when
          you save the club.
        </p>
      {:else}
        <p data-testid="certificate-verdict">
          Accepting this certificate would not be enough. It was signed by a certificate authority
          this computer does not know, or it was made for another address.
        </p>
        <p class="muted">
          Ask whoever runs the server for their certificate authority, and paste it in the box on
          this page. Check the address too.
        </p>
      {/if}
    {/if}
    {#snippet footer()}
      {#if verdict === 'accept'}
        <Button variant="ghost" onclick={() => (looked = undefined)}>Cancel</Button>
        <Button variant="primary" onclick={accept}>Accept this certificate</Button>
      {:else}
        <Button variant="primary" onclick={() => (looked = undefined)}>Done</Button>
      {/if}
    {/snippet}
  </Dialog>
{/if}

{#if removing}
  {@const target = removing}
  {@const count = view ? eventsOf(view, target.id) : 0}
  <Dialog open={true} title={`Remove ${clubName(target)}?`} onclose={() => (removing = undefined)}>
    <p>Grid forgets the address and the link token.</p>
    {#if count > 0}
      <p data-testid="remove-club-events">
        {count === 1 ? '1 event goes' : `${count} events go`} to this club. {count === 1
          ? 'It'
          : 'They'} will not be sent any more.
      </p>
    {/if}
    <p class="muted">What is already on the server stays there.</p>
    {#if error}<Banner tone="danger">{error}</Banner>{/if}
    {#snippet footer()}
      <Button variant="ghost" onclick={() => (removing = undefined)}>Cancel</Button>
      <Button variant="danger" disabled={busy} onclick={remove}>Remove</Button>
    {/snippet}
  </Dialog>
{/if}

{#if replacing}
  {@const target = replacing}
  {@const again = target.state === 'Removed'}
  <Dialog
    open={true}
    title={again
      ? `Send ${target.name} to the server again?`
      : `Replace the server's copy of ${target.name}?`}
    onclose={() => (replacing = undefined)}
  >
    {#if again}
      <p>Someone who runs your club removed this event from the server.</p>
      <p>Sending it again puts all of it back, for anyone to see.</p>
    {:else}
      <p>
        The server holds a different event under the same name. It may be an older copy of this one,
        or one from another computer.
      </p>
      <p>Replacing removes what the server holds and sends this computer's copy.</p>
    {/if}
    {#if error}<Banner tone="danger">{error}</Banner>{/if}
    {#snippet footer()}
      <Button variant="ghost" onclick={() => (replacing = undefined)}>Cancel</Button>
      <Button variant={again ? 'primary' : 'danger'} disabled={busy} onclick={replace}
        >{again ? 'Send it again' : 'Replace'}</Button
      >
    {/snippet}
  </Dialog>
{/if}

<style>
  .fingerprint {
    display: grid;
    gap: var(--gf-space-1);
    font-family: var(--gf-font-mono);
    font-size: var(--gf-font-size-lg);
    letter-spacing: 0.04em;
    user-select: all;
  }
  p {
    margin: 0 0 var(--gf-space-3);
    max-width: 65ch;
  }
  .link,
  .clubs,
  .club-form,
  .events {
    display: grid;
    gap: var(--gf-space-3);
  }
  .club-form {
    padding-top: var(--gf-space-3);
    border-top: 1px solid var(--gf-border);
  }
  h3 {
    margin: 0;
    font-size: var(--gf-font-size-md);
  }
  .grow {
    flex: 1;
  }
  .where {
    width: 14rem;
    max-width: 100%;
  }
  header {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
  }
  .title {
    flex: 1;
    min-width: 0;
  }
  h2 {
    margin: 0;
    font-size: var(--gf-font-size-lg);
  }
  .fields {
    display: grid;
    grid-template-columns: max-content minmax(0, 32rem);
    align-items: center;
    gap: var(--gf-space-2) var(--gf-space-3);
    font-size: var(--gf-font-size-sm);
  }
  .fields label {
    color: var(--gf-text-muted);
  }
  .extra {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    font-size: var(--gf-font-size-sm);
  }
  textarea {
    width: 100%;
    max-width: 40rem;
    box-sizing: border-box;
    font-family: var(--gf-font-mono);
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text);
    background: var(--gf-surface-sunken);
    border: 1px solid var(--gf-border-strong);
    border-radius: var(--gf-radius-sm);
    padding: var(--gf-space-2);
  }
  .actions {
    display: flex;
    gap: var(--gf-space-2);
  }
  .error {
    margin: 0;
    color: var(--gf-danger);
    font-size: var(--gf-font-size-sm);
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
  .name {
    font-weight: var(--gf-font-weight-semibold);
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
</style>
