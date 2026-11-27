<script lang="ts">
  /**
   * ServerHome — what a remote server shows (docs/primitives.md §13).
   *
   * A Director shows a visitor the one event that is open. A server holds many, so it
   * shows a list, newest first, and one of them when it is picked. An event here is drawn
   * by the same `SpectatorView` a Director's visitors get, from the same reads; only the
   * address in front of them differs (§11.7).
   *
   * Anyone may look. A signed-in pilot who is in the event also gets **Sign up**, where the
   * event has it (§13.4). What they ask for goes to the Director, which decides it as it
   * would at the race. Nothing else can be changed from this page.
   */
  import { Badge, Card } from '@gridfpv/components';
  import {
    getServerEvent,
    listServerClubs,
    listServerEvents,
    serverEventBase
  } from '@gridfpv/protocol-client';
  import type { MyAccount, PushClub, ServerEvent, SignInProvider } from '@gridfpv/types';
  import { EventLive, type LiveSession } from '../lib/eventLive.svelte.js';
  import {
    eventDetail,
    formatServerHash,
    parseServerHash,
    pilotHere,
    type ServerPlace
  } from '../lib/serverHome.js';
  import { peopleApi, sayIn, signInProblem, type PeopleApi } from '../lib/serverPeople.js';
  import { publicSource, type PublicSource } from '../lib/spectator.js';
  import ServerAccount from './ServerAccount.svelte';
  import ServerClub from './ServerClub.svelte';
  import ServerInvite from './ServerInvite.svelte';
  import ServerResults from './ServerResults.svelte';
  import ServerSignIn from './ServerSignIn.svelte';
  import SpectatorView from './SpectatorView.svelte';

  /** An event's reads, with the three things a pilot can ask for when they are there. */
  type EventSource = LiveSession &
    Partial<Pick<PublicSource, 'takeSpot' | 'takeNextSpot' | 'leaveSpot'>>;

  /** How long a pilot's change may take to come back from the Director, in milliseconds. */
  const BACK_WITHIN_MS = 4_000;

  /** How often the list of events is read again, in milliseconds. */
  const LIST_POLL_MS = 15_000;

  let {
    baseUrl,
    load = {
      clubs: () => listServerClubs(baseUrl),
      events: (club: string) => listServerEvents(baseUrl, club),
      event: (club: string, event: string) => getServerEvent(baseUrl, club, event)
    },
    sourceFor = (club: string, event: string) =>
      publicSource(serverEventBase(baseUrl, club, event)),
    people = peopleApi(baseUrl)
  }: {
    /** The server's address. */
    baseUrl: string;
    /** How the lists are read. A test passes its own. */
    load?: {
      clubs: () => Promise<PushClub[]>;
      events: (club: string) => Promise<ServerEvent[]>;
      /** One event by its address. `undefined` when the server holds none. */
      event?: (club: string, event: string) => Promise<ServerEvent | undefined>;
    };
    /**
     * Where one event is read from, and where a pilot's sign-up is sent. A test passes its
     * own; one with no sign-up calls only shows the event.
     */
    sourceFor?: (club: string, event: string) => EventSource;
    /** Accounts: who is signed in, invites, members. A test passes its own. */
    people?: PeopleApi;
  } = $props();

  // ── Who is signed in ───────────────────────────────────────────────────────
  /** The account this browser is signed in as. `null` once it is known there is none. */
  let account = $state<MyAccount | null | undefined>(undefined);
  let providers = $state<SignInProvider[] | undefined>(undefined);

  async function readMe() {
    try {
      account = (await people.me()).account ?? null;
    } catch {
      // Not reachable right now. The page still shows events; nobody is shown signed in.
      if (account === undefined) account = null;
    }
  }
  $effect(() => {
    void readMe();
    people
      .providers()
      .then((found) => (providers = found))
      .catch(() => (providers = []));
  });

  const go = (to: ServerPlace) => {
    location.hash = formatServerHash(to);
  };

  // A page about the account reads it again on the way in: a role may have been given or
  // taken since the page was opened, and what it shows must be what is so. An event does
  // too: whether the person may sign up there depends on the pilots they hold now.
  $effect(() => {
    if (
      place.kind === 'event' ||
      place.kind === 'account' ||
      place.kind === 'club' ||
      place.kind === 'invite' ||
      place.kind === 'results'
    ) {
      void readMe();
    }
  });

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

  /** Counted up to read the lists again now: a club was renamed, an event hidden or removed. */
  let listAgain = $state(0);

  $effect(() => {
    // Who is signed in changes what they may see: a hidden event is listed for those who
    // run its club.
    void account?.id;
    void listAgain;
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
  const pickedKey = $derived(picked ? `${picked.club}/${picked.event}` : undefined);
  /** The picked event as the list knows it. */
  const pickedListed = $derived(
    picked &&
      clubs
        ?.find((entry) => entry.club.id === picked.club)
        ?.events.find((event) => event.id === picked.event)
  );
  /**
   * The picked event asked for by its own address, when the list does not have it: a
   * hidden event is left out of the list and still opens for anyone who has its link.
   * `null` once the server has said it holds no such event.
   */
  let asked = $state<{ key: string; event: ServerEvent | null } | undefined>(undefined);
  $effect(() => {
    const at = picked;
    const key = pickedKey;
    // Asked again each time the list is read: an event can be removed while it is open.
    if (!at || key === undefined || clubs === undefined || pickedListed) return;
    if (!load.event) {
      asked = { key, event: null };
      return;
    }
    let stopped = false;
    load
      .event(at.club, at.event)
      .then((event) => {
        if (!stopped) asked = { key, event: event ?? null };
      })
      // The server did not answer. That is not the same as the event being gone.
      .catch(() => {});
    return () => {
      stopped = true;
    };
  });
  /** The picked event, for its name. */
  const pickedEvent = $derived(
    pickedListed ?? (asked?.key === pickedKey ? (asked?.event ?? undefined) : undefined)
  );
  /** The server has said it holds no such event. */
  const gone = $derived(
    picked !== undefined &&
      clubs !== undefined &&
      pickedListed === undefined &&
      asked?.key === pickedKey &&
      asked?.event === null
  );

  let live = $state.raw<EventLive | undefined>(undefined);
  let source = $state.raw<EventSource | undefined>(undefined);
  $effect(() => {
    const at = picked;
    // One live store per event. `pickedKey` is what says the event changed.
    if (pickedKey === undefined || at === undefined) {
      live = undefined;
      source = undefined;
      return;
    }
    const from = sourceFor(at.club, at.event);
    const store = new EventLive(from, at.event, { pollTimers: false });
    source = from;
    live = store;
    store.start();
    return () => store.stop();
  });

  // ── A pilot signing up ─────────────────────────────────────────────────────
  /** The pilot the signed-in account is in the event that is open on the page. */
  const pilot = $derived(picked && live ? pilotHere(account, picked.club, live.pilots) : undefined);

  /**
   * Wait for what the Director decided to come back. The Director said yes, and the
   * change reaches this server with its next push. Until the page has it, the sheet would
   * still show the old spots, and a pilot would tap again.
   */
  async function comeBack(before: number | undefined) {
    const end = Date.now() + BACK_WITHIN_MS;
    await live?.reload();
    while (live?.view?.cursor === before && Date.now() < end) {
      await new Promise((done) => setTimeout(done, 100));
      await live?.reload();
    }
  }

  const sent =
    <A extends unknown[]>(what: ((...args: A) => Promise<unknown>) | undefined) =>
    async (...args: A) => {
      const before = live?.view?.cursor;
      await what?.(...args);
      await comeBack(before);
    };
  const signup = $derived(
    pilot && source?.takeSpot && source.takeNextSpot && source.leaveSpot
      ? {
          take: sent(source.takeSpot),
          next: sent(source.takeNextSpot),
          leave: sent(source.leaveSpot)
        }
      : undefined
  );
</script>

<main class="server">
  <header class="top">
    <h1><a href="#/">Grid<span class="accent">FPV</span></a></h1>
    {#if account}
      <a class="who" href={formatServerHash({ kind: 'account' })} data-testid="server-who"
        >{account.name}</a
      >
    {:else if account === null && providers && providers.length > 0}
      <a class="who" href={formatServerHash({ kind: 'sign-in' })} data-testid="server-sign-in"
        >Sign in</a
      >
    {/if}
  </header>

  {#if offline}
    <p class="offline" role="status" data-testid="server-offline">
      Not connected right now. Trying again.
    </p>
  {/if}

  {#if place.kind === 'sign-in'}
    <p class="back"><a href={formatServerHash({ kind: 'list' })}>← All events</a></p>
    {#if account}
      <Card elevation="sm">
        <p>You are signed in as <strong>{account.name}</strong>.</p>
        {#if signInProblem(place.problem)}<p class="muted">{signInProblem(place.problem)}</p>{/if}
      </Card>
    {:else}
      <p class="event">Sign in</p>
      <ServerSignIn api={people} {providers} problem={signInProblem(place.problem)} />
      <p class="muted">You need an invite from your club to make an account.</p>
    {/if}
  {:else if place.kind === 'invite'}
    <p class="back"><a href={formatServerHash({ kind: 'list' })}>← All events</a></p>
    {#if account !== undefined}
      <ServerInvite
        api={people}
        secret={place.secret}
        account={account ?? undefined}
        {providers}
        onjoined={async () => {
          await readMe();
          go({ kind: 'account' });
        }}
      />
    {/if}
  {:else if place.kind === 'account'}
    <p class="back"><a href={formatServerHash({ kind: 'list' })}>← All events</a></p>
    {#if account}
      <ServerAccount
        api={people}
        {account}
        {providers}
        clubs={clubs?.map((entry) => entry.club) ?? []}
        onchange={(me) => (account = me.account ?? null)}
        onreread={readMe}
        onsignedout={() => {
          account = null;
          go({ kind: 'list' });
        }}
      />
    {:else if account === null}
      <p class="event">Sign in</p>
      <ServerSignIn api={people} {providers} />
    {/if}
  {:else if place.kind === 'club'}
    {@const wanted = place.club}
    {@const found = clubs?.find((entry) => entry.club.id === wanted)?.club}
    <p class="back"><a href={formatServerHash({ kind: 'account' })}>← Your account</a></p>
    {@const say = sayIn(account ?? undefined, wanted)}
    {#if account && found && say}
      <ServerClub
        api={people}
        club={found}
        me={account.id}
        runs={say === 'runs'}
        onchange={() => {
          listAgain += 1;
          void readMe();
        }}
      />
    {:else if account && found}
      <Card elevation="sm"><p>Only those who look after this club can open this page.</p></Card>
    {:else if account === null}
      <p class="event">Sign in</p>
      <ServerSignIn api={people} {providers} />
    {:else if account && clubs !== undefined}
      <Card elevation="sm"><p class="event">This club is not here</p></Card>
    {/if}
  {:else if place.kind === 'results'}
    <p class="back"><a href={formatServerHash({ kind: 'account' })}>← Your account</a></p>
    {#if account}
      <ServerResults api={people} />
    {:else if account === null}
      <p class="event">Sign in</p>
      <ServerSignIn api={people} {providers} />
    {/if}
  {:else if picked}
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
        {#if pickedEvent.organizer?.trim()}
          <p class="muted" data-testid="server-event-organizer">
            Run by {pickedEvent.organizer.trim()}
          </p>
        {/if}
        {#if pickedEvent.description?.trim()}
          <p class="about" data-testid="server-event-description">
            {pickedEvent.description.trim()}
          </p>
        {/if}
      {/if}
      {#if live}
        <SpectatorView {live} {pilot} {signup} />
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
                  {#if event.hidden}<Badge tone="neutral">Hidden</Badge>{/if}
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
  .who {
    font-size: var(--gf-font-size-sm);
    overflow-wrap: anywhere;
    min-width: 0;
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
  .about {
    margin: 0;
    white-space: pre-line;
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
