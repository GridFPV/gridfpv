<script lang="ts">
  /**
   * ServerHome — what a remote server shows (docs/primitives.md §13).
   *
   * A Director shows a visitor the one event that is open. A server holds many, so it
   * shows a list, newest first, and one of them when it is picked. An event here is drawn
   * by the same `SpectatorView` a Director's visitors get, from the same reads; only the
   * address in front of them differs (§11.7).
   *
   * Nothing can be changed from this page.
   */
  import { Card } from '@gridfpv/components';
  import { listServerClubs, listServerEvents, serverEventBase } from '@gridfpv/protocol-client';
  import type { PushClub, ServerEvent } from '@gridfpv/types';
  import { EventLive, type LiveSession } from '../lib/eventLive.svelte.js';
  import {
    eventDetail,
    formatServerHash,
    parseServerHash,
    type ServerPlace
  } from '../lib/serverHome.js';
  import { publicSource } from '../lib/spectator.js';
  import SpectatorView from './SpectatorView.svelte';

  /** How often the list of events is read again, in milliseconds. */
  const LIST_POLL_MS = 15_000;

  let {
    baseUrl,
    load = {
      clubs: () => listServerClubs(baseUrl),
      events: (club: string) => listServerEvents(baseUrl, club)
    },
    sourceFor = (club: string, event: string) => publicSource(serverEventBase(baseUrl, club, event))
  }: {
    /** The server's address. */
    baseUrl: string;
    /** How the lists are read. A test passes its own. */
    load?: {
      clubs: () => Promise<PushClub[]>;
      events: (club: string) => Promise<ServerEvent[]>;
    };
    /** Where one event is read from. A test passes its own. */
    sourceFor?: (club: string, event: string) => LiveSession;
  } = $props();

  // ── Where the page is ──────────────────────────────────────────────────────
  let place = $state<ServerPlace>(parseServerHash(location.hash));
  $effect(() => {
    const follow = () => (place = parseServerHash(location.hash));
    window.addEventListener('hashchange', follow);
    return () => window.removeEventListener('hashchange', follow);
  });

  // ── The clubs and their events ─────────────────────────────────────────────
  interface ClubEvents {
    club: PushClub;
    events: ServerEvent[];
  }
  let clubs = $state<ClubEvents[] | undefined>(undefined);
  /** Whether the last try to reach the server failed. What is shown may be old. */
  let offline = $state(false);

  $effect(() => {
    let stopped = false;
    const read = async () => {
      try {
        const found = await load.clubs();
        const lists = await Promise.all(
          found.map(async (club) => ({ club, events: await load.events(club.id) }))
        );
        if (stopped) return;
        clubs = lists;
        offline = false;
      } catch {
        if (!stopped) offline = true;
      }
    };
    void read();
    const timer = setInterval(read, LIST_POLL_MS);
    return () => {
      stopped = true;
      clearInterval(timer);
    };
  });

  const total = $derived(clubs?.reduce((sum, club) => sum + club.events.length, 0) ?? 0);

  // ── The event that is open on the page ─────────────────────────────────────
  const picked = $derived(place.kind === 'event' ? place : undefined);
  /** The picked event as the list knows it, for its name. */
  const pickedEvent = $derived(
    picked &&
      clubs
        ?.find((entry) => entry.club.id === picked.club)
        ?.events.find((event) => event.id === picked.event)
  );
  /** The list has been read and the picked event is not in it. */
  const gone = $derived(picked !== undefined && clubs !== undefined && pickedEvent === undefined);

  let live = $state.raw<EventLive | undefined>(undefined);
  const pickedKey = $derived(picked ? `${picked.club}/${picked.event}` : undefined);
  $effect(() => {
    const at = picked;
    // One live store per event. `pickedKey` is what says the event changed.
    if (pickedKey === undefined || at === undefined) {
      live = undefined;
      return;
    }
    const store = new EventLive(sourceFor(at.club, at.event), at.event, { pollTimers: false });
    live = store;
    store.start();
    return () => store.stop();
  });
</script>

<main class="server">
  <header class="top">
    <h1><a href="#/">Grid<span class="accent">FPV</span></a></h1>
  </header>

  {#if offline}
    <p class="offline" role="status" data-testid="server-offline">
      Not connected right now. Trying again.
    </p>
  {/if}

  {#if picked}
    <p class="back"><a href={formatServerHash({ kind: 'list' })}>← All events</a></p>
    {#if gone}
      <Card elevation="sm">
        <p class="event">This event is not here</p>
        <p>It may have been removed, or the address is not right.</p>
      </Card>
    {:else}
      {#if pickedEvent}
        <p class="event" data-testid="server-event">{pickedEvent.name}</p>
        {#if eventDetail(pickedEvent)}<p class="muted">{eventDetail(pickedEvent)}</p>{/if}
      {/if}
      {#if live}
        <SpectatorView {live} />
      {/if}
    {/if}
  {:else if clubs === undefined}
    {#if !offline}<p class="muted" role="status">Loading…</p>{/if}
  {:else if total === 0}
    <Card elevation="sm">
      <p class="event">No events yet</p>
      <p>When a director sends one, it will show here.</p>
    </Card>
  {:else}
    {#each clubs as { club, events } (club.id)}
      {#if events.length > 0}
        <section aria-label={club.name}>
          <h2>{club.name}</h2>
          <ul>
            {#each events as event (event.id)}
              <li>
                <a
                  class="row"
                  href={formatServerHash({ kind: 'event', club: club.id, event: event.id })}
                  data-testid="server-event-link"
                >
                  <span class="name">{event.name}</span>
                  {#if eventDetail(event)}<span class="muted">{eventDetail(event)}</span>{/if}
                </a>
              </li>
            {/each}
          </ul>
        </section>
      {/if}
    {/each}
  {/if}
</main>

<style>
  .server {
    max-width: 40rem;
    margin: 0 auto;
    padding: var(--gf-space-5) var(--gf-space-4);
    display: grid;
    gap: var(--gf-space-4);
    font-family: var(--gf-font-family);
  }
  .top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-3);
  }
  h1 {
    margin: 0;
    font-size: var(--gf-font-size-xl);
  }
  h1 a {
    color: inherit;
    text-decoration: none;
  }
  .accent {
    color: var(--gf-accent);
  }
  h2 {
    margin: 0 0 var(--gf-space-2);
    font-size: var(--gf-font-size-lg);
  }
  p {
    margin: 0 0 var(--gf-space-2);
  }
  .event {
    margin: 0;
    font-size: var(--gf-font-size-xl);
    font-weight: var(--gf-font-weight-bold);
    overflow-wrap: anywhere;
  }
  .back {
    margin: 0;
    font-size: var(--gf-font-size-sm);
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .offline {
    margin: 0;
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-warn);
    border-radius: var(--gf-radius-sm);
    color: var(--gf-warn);
    font-size: var(--gf-font-size-sm);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--gf-space-2);
  }
  .row {
    display: grid;
    gap: 2px;
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface);
    color: inherit;
    text-decoration: none;
  }
  .row:hover,
  .row:focus-visible {
    border-color: var(--gf-accent);
  }
  .name {
    font-weight: var(--gf-font-weight-semibold);
    overflow-wrap: anywhere;
  }
</style>
