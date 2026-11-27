<script lang="ts">
  /**
   * ServerPage — sending events to a remote server (docs/primitives.md §13).
   *
   * One card sets the link: the server's address, the link token and a switch. Under it is
   * every event and how much of it the server holds. An event the server holds a
   * different copy of is never overwritten on its own: the director says so here.
   *
   * The link token is typed in and never shown back. The Director only says one is set.
   */
  import { Badge, Banner, Button, Card, Dialog, Input } from '@gridfpv/components';
  import type { LinkEvent, LinkView } from '@gridfpv/types';
  import Page from '../Page.svelte';
  import {
    changed,
    eventLabel,
    eventProblem,
    eventTone,
    formOf,
    formProblem,
    linkLabel,
    linkProblem,
    linkTone,
    refreshEvery,
    settingsOf,
    type LinkForm
  } from '../lib/serverLink.js';
  import type { Session } from '../lib/session.svelte.js';

  let {
    session
  }: {
    session: Pick<Session, 'getServerLink' | 'putServerLink' | 'replaceOnServer'>;
  } = $props();

  let view = $state<LinkView | undefined>(undefined);
  let form = $state<LinkForm | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);
  /** The event whose copy on the server the director is about to replace. */
  let replacing = $state<LinkEvent | undefined>(undefined);

  const say = (err: unknown): string => (err instanceof Error ? err.message : String(err));

  /** Take what the Director says. The form is left alone while the director is in it. */
  function take(next: LinkView, reset: boolean) {
    const untouched = form === undefined || view === undefined || !changed(form, view);
    view = next;
    if (reset || untouched) form = { ...formOf(next), on: next.on };
  }

  async function load() {
    try {
      take(await session.getServerLink(), false);
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
      take(next, true);
      schedule();
      return true;
    } catch (err) {
      error = say(err);
      return false;
    } finally {
      busy = false;
    }
  }

  const problem = $derived(form && view ? formProblem(form, view) : undefined);
  const dirty = $derived(form !== undefined && view !== undefined && changed(form, view));

  async function save(e: Event) {
    e.preventDefault();
    if (!form || problem) return;
    const sending = form;
    await change(() => session.putServerLink(settingsOf(sending)));
  }

  /** The switch saves at once, with whatever else is in the form. */
  async function flip(e: Event) {
    if (!form || !view) return;
    const box = e.currentTarget as HTMLInputElement;
    const next = { ...form, on: box.checked };
    const trouble = formProblem(next, view);
    if (trouble) {
      error = trouble;
      box.checked = form.on;
      return;
    }
    if (!(await change(() => session.putServerLink(settingsOf(next))))) {
      box.checked = view.on;
    }
  }

  async function replace() {
    const target = replacing;
    if (!target) return;
    if (await change(() => session.replaceOnServer(target.id))) replacing = undefined;
  }
</script>

<Page title="Server" help="server" lede="Send your events to a Grid server.">
  {#if loadError}
    <Banner tone="danger" title="Grid could not read its server setting.">{loadError}</Banner>
  {/if}
  {#if view && form}
    <Card elevation="sm">
      <!-- A refusal is about what was in the form then. Typing again clears it. -->
      <form
        class="link"
        onsubmit={save}
        oninput={() => (error = undefined)}
        aria-label="Server"
        data-testid="server-link"
      >
        <header>
          <div class="title">
            <h2>Remote server</h2>
          </div>
          <Badge tone={linkTone(view.state)}>
            <span data-testid="link-state">{linkLabel(view)}</span>
          </Badge>
          <label class="switch">
            <input
              type="checkbox"
              role="switch"
              aria-label="Send events to the server"
              checked={view.on}
              disabled={busy}
              onchange={flip}
            />
            <span class="track" aria-hidden="true"><span class="thumb"></span></span>
          </label>
        </header>

        {#if error}<Banner tone="danger">{error}</Banner>{/if}
        {#if linkProblem(view.state)}
          <Banner tone="warn"
            ><span data-testid="link-problem">{linkProblem(view.state)}</span></Banner
          >
        {/if}

        <div class="fields">
          <label for="server-url">Address</label>
          <Input
            id="server-url"
            type="url"
            autocomplete="off"
            spellcheck="false"
            placeholder="https://grid.example.org"
            bind:value={form.url}
            disabled={busy}
          />
          <label for="server-token">Link token</label>
          <Input
            id="server-token"
            type="password"
            autocomplete="off"
            spellcheck="false"
            placeholder={view.has_token ? 'Set. Type a new one to change it' : 'gfs_…'}
            bind:value={form.token}
            disabled={busy}
          />
        </div>

        <label class="extra">
          <input type="checkbox" bind:checked={form.extra} disabled={busy} />
          The server uses its own certificate
        </label>
        {#if form.extra}
          <textarea
            aria-label="The server's certificate, or its certificate authority"
            rows="6"
            spellcheck="false"
            placeholder="-----BEGIN CERTIFICATE-----"
            bind:value={form.pem}
            disabled={busy}
          ></textarea>
        {/if}

        {#if problem && dirty}<p class="error" role="alert">{problem}</p>{/if}
        {#if dirty}
          <div class="actions">
            <Button
              variant="primary"
              size="sm"
              type="submit"
              disabled={busy || problem !== undefined}>Save</Button
            >
            <Button
              variant="ghost"
              size="sm"
              disabled={busy}
              onclick={() => view && (form = formOf(view))}>Cancel</Button
            >
          </div>
        {/if}
      </form>
    </Card>

    {#if view.events.length > 0}
      <Card elevation="sm">
        <div class="events">
          <h2>Events</h2>
          <ul aria-label="Events and how much of each is on the server">
            {#each view.events as event (event.id)}
              <li data-testid="link-event">
                <span class="name">{event.name}</span>
                <Badge tone={eventTone(event)}>{eventLabel(event)}</Badge>
                {#if eventProblem(event)}
                  <span class="muted">{eventProblem(event)}</span>
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
  p {
    margin: 0 0 var(--gf-space-3);
    max-width: 65ch;
  }
  .link,
  .events {
    display: grid;
    gap: var(--gf-space-3);
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

  .switch {
    position: relative;
    display: inline-flex;
    cursor: pointer;
  }
  .switch input {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    margin: 0;
    opacity: 0;
    cursor: pointer;
    /* Above the drawn track and its thumb, so a click always lands on the box itself. */
    z-index: 1;
  }
  .switch input:disabled {
    cursor: default;
  }
  .track {
    width: 2.75rem;
    height: 1.5rem;
    border-radius: var(--gf-radius-pill);
    background: var(--gf-surface-sunken);
    border: 1px solid var(--gf-border-strong);
    transition: background var(--gf-motion-fast) var(--gf-ease-out);
    display: flex;
    align-items: center;
    padding: 0 2px;
    pointer-events: none;
  }
  .thumb {
    width: 1.125rem;
    height: 1.125rem;
    border-radius: 50%;
    background: var(--gf-text-muted);
    transition:
      transform var(--gf-motion-fast) var(--gf-ease-out),
      background var(--gf-motion-fast) var(--gf-ease-out);
  }
  .switch input:checked + .track {
    background: var(--gf-accent);
    border-color: var(--gf-accent);
  }
  .switch input:checked + .track .thumb {
    transform: translateX(1.2rem);
    background: var(--gf-accent-contrast);
  }
  .switch input:focus-visible + .track {
    outline: var(--gf-ring-width) solid var(--gf-ring-color);
    outline-offset: var(--gf-ring-offset);
  }
  .switch input:disabled + .track {
    opacity: 0.6;
  }
</style>
