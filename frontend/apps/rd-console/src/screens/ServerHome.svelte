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
  import { Badge, Banner, Button, Card, Dialog, Input } from '@gridfpv/components';
  import {
    dropSeat,
    getServerEvent,
    listServerClubs,
    getServerClubByWord,
    listServerEvents,
    serverEventBase,
    takeSeatOnServer
  } from '@gridfpv/protocol-client';
  import type {
    AdapterId,
    MyAccount,
    PushClub,
    ServerClub as ClubOnServer,
    RaceId,
    SeatRef,
    ServerEvent,
    SignInProvider
  } from '@gridfpv/types';
  import { codeProblem } from '../lib/pilotAccess.js';
  import {
    dropRaceCode,
    keepRaceCode,
    keptRaceCode,
    needsRaceCode,
    type CodeStore
  } from '../lib/raceCode.js';
  import { EventLive, type LiveSession } from '../lib/eventLive.svelte.js';
  import {
    eventDetail,
    formatServerHash,
    parseServerHash,
    pilotHere,
    type ServerPlace
  } from '../lib/serverHome.js';
  import {
    peopleApi,
    sayIn,
    signInProblem,
    signUpLine,
    type PeopleApi
  } from '../lib/serverPeople.js';
  import { publicSource, type PublicSource } from '../lib/spectator.js';
  import ServerAccount from './ServerAccount.svelte';
  import ServerClub from './ServerClub.svelte';
  import ServerInvite from './ServerInvite.svelte';
  import ServerReconnect from './ServerReconnect.svelte';
  import ServerClubs from './ServerClubs.svelte';
  import ServerResults from './ServerResults.svelte';
  import ServerSignIn from './ServerSignIn.svelte';
  import ServerSignUpSetting from './ServerSignUpSetting.svelte';
  import SpectatorView from './SpectatorView.svelte';

  /** An event's reads, with the three things a pilot can ask for when they are there. */
  type EventSource = LiveSession &
    Partial<Pick<PublicSource, 'takeSpot' | 'takeNextSpot' | 'leaveSpot'>>;

  /** A club in the list: its friendly address and whether it is archived, when told. */
  type ListedClub = PushClub & Partial<Pick<ClubOnServer, 'word' | 'archived'>>;

  /** How a pilot asks the Director for an open seat from here, and gives one up. */
  interface SeatAsks {
    take(
      club: string,
      event: string,
      race: RaceId,
      seat: { adapter: AdapterId; seat: SeatRef },
      code: string | undefined
    ): Promise<unknown>;
    drop(club: string, event: string, race: RaceId): Promise<unknown>;
  }

  /** How long a pilot's change may take to come back from the Director, in milliseconds. */
  const BACK_WITHIN_MS = 4_000;

  /** How often the list of events is read again, in milliseconds. */
  const LIST_POLL_MS = 15_000;

  let {
    baseUrl,
    load = {
      clubs: () => listServerClubs(baseUrl),
      clubByWord: (word: string) => getServerClubByWord(baseUrl, word),
      events: (club: string) => listServerEvents(baseUrl, club),
      event: (club: string, event: string) => getServerEvent(baseUrl, club, event)
    },
    sourceFor = (club: string, event: string) =>
      publicSource(serverEventBase(baseUrl, club, event)),
    people = peopleApi(baseUrl),
    seats = {
      take: (club, event, race, seat, code) =>
        takeSeatOnServer(serverEventBase(baseUrl, club, event), race, seat, code),
      drop: (club, event, race) => dropSeat(serverEventBase(baseUrl, club, event), race)
    },
    codes = undefined
  }: {
    /** The server's address. */
    baseUrl: string;
    /** How the lists are read. A test passes its own. */
    load?: {
      clubs: () => Promise<ListedClub[]>;
      /** The club a friendly address leads to. `undefined` when there is none. */
      clubByWord?: (word: string) => Promise<ClubOnServer | undefined>;
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
    /**
     * How a pilot asks the Director for an open seat, and gives one up (§11.9, §13.13).
     * `take` carries the race code the pilot typed. A test passes its own.
     */
    seats?: SeatAsks;
    /** Where the race code is kept in this browser. A test passes its own. */
    codes?: CodeStore;
  } = $props();

  // ── Who is signed in ───────────────────────────────────────────────────────
  /** The account this browser is signed in as. `null` once it is known there is none. */
  let account = $state<MyAccount | null | undefined>(undefined);
  let providers = $state<SignInProvider[] | undefined>(undefined);
  /** Whether someone with no invite may make an account here. Unknown until read. */
  let openSignUp = $state<boolean | undefined>(undefined);

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
    people
      .serverInfo()
      .then((info) => (openSignUp = info.open_sign_up))
      // An older server cannot say. It asks for an invite.
      .catch(() => (openSignUp = false));
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
      place.kind === 'c' ||
      place.kind === 'invite' ||
      place.kind === 'reconnect' ||
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
    club: ListedClub;
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

  // ── One club, opened by its friendly address ───────────────────────────────
  /**
   * The club the address names, with its events. `club` is `null` once the server has
   * said no club has that word. An archived club is not in the list above and still opens
   * here.
   */
  let worded = $state<
    { word: string; club: ClubOnServer | null; events: ServerEvent[] } | undefined
  >(undefined);
  $effect(() => {
    if (place.kind !== 'c') return;
    const word = place.word;
    void account?.id;
    void listAgain;
    let stopped = false;
    const read = async () => {
      try {
        const club = (await load.clubByWord?.(word)) ?? null;
        const events = club ? await load.events(club.id) : [];
        if (!stopped) worded = { word, club, events };
      } catch {
        // Not reachable right now. What was shown stays.
      }
    };
    void read();
    const timer = setInterval(read, LIST_POLL_MS);
    return () => {
      stopped = true;
      clearInterval(timer);
    };
  });
  const wordedNow = $derived(
    place.kind === 'c' && worded?.word === place.word ? worded : undefined
  );

  /**
   * The signed-in person's primary club, when the front page is open and the club is on
   * the list (§13.2). The front page then shows that club alone.
   */
  const primary = $derived(
    place.kind === 'list' && account?.primary !== undefined
      ? clubs?.find((entry) => entry.club.id === account?.primary)
      : undefined
  );
  /** The clubs the page lists now: the primary one, or all of them. */
  const shown = $derived(primary ? [primary] : clubs);
  const total = $derived(shown?.reduce((sum, club) => sum + club.events.length, 0) ?? 0);

  // ── The event that is open on the page ─────────────────────────────────────
  // A link from the race code's QR: keep the code in this browser, and go on to the event.
  // The code is taken out of the address, so it is not left in the history or shared on.
  $effect(() => {
    if (place.kind !== 'here') return;
    keepRaceCode(place.club, place.event, place.code, codes);
    const to = formatServerHash({ kind: 'event', club: place.club, event: place.event });
    history.replaceState(null, '', to);
    place = parseServerHash(to);
  });

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

  // ── A pilot checking in to an open seat (§11.9, §13.13) ─────────────────────
  /**
   * The seat a pilot asked for and the race code is still needed for: none is kept in
   * this browser, or the Director said the one sent is not right.
   */
  let wanting = $state<
    | {
        club: string;
        event: string;
        race: RaceId;
        seat: { adapter: AdapterId; seat: SeatRef };
        /** What the Director said about the last code, if one was sent. */
        why?: string;
        done: () => void;
      }
    | undefined
  >(undefined);
  let typed = $state('');
  let codeBusy = $state(false);

  /** Ask for a seat with a code. `false` when the Director wants another code. */
  async function takeWith(
    at: { club: string; event: string },
    race: RaceId,
    seat: { adapter: AdapterId; seat: SeatRef },
    code: string
  ): Promise<string | true> {
    const before = live?.view?.cursor;
    try {
      await seats.take(at.club, at.event, race, seat, code);
    } catch (err) {
      if (!needsRaceCode(err)) throw err;
      dropRaceCode(at.club, at.event, codes);
      return err instanceof Error ? err.message : String(err);
    }
    keepRaceCode(at.club, at.event, code, codes);
    await comeBack(before);
    return true;
  }

  const checkin = $derived(
    pilot && picked
      ? {
          take: async (race: RaceId, seat: { adapter: AdapterId; seat: SeatRef }) => {
            const at = { club: picked.club, event: picked.event };
            const kept = keptRaceCode(at.club, at.event, codes);
            let why: string | undefined;
            if (kept) {
              const got = await takeWith(at, race, seat, kept);
              if (got === true) return;
              // The code kept here is an old one. Say so, and ask for the new one.
              why = got;
            }
            typed = '';
            await new Promise<void>((done) => {
              wanting = { ...at, race, seat, why, done };
            });
          },
          drop: sent((race: RaceId) => seats.drop(picked.club, picked.event, race))
        }
      : undefined
  );

  const typedProblem = $derived(typed.trim() === '' ? undefined : codeProblem(typed));

  async function sendCode(e: Event) {
    e.preventDefault();
    const want = wanting;
    if (!want || codeBusy || typed.trim() === '' || typedProblem) return;
    codeBusy = true;
    try {
      const got = await takeWith(want, want.race, want.seat, typed);
      if (got === true) {
        wanting = undefined;
        want.done();
      } else {
        wanting = { ...want, why: got };
      }
    } catch (err) {
      // Not about the code: the Director's own reason. It is shown, and the code that
      // was typed may well be right, so it is kept for the next try.
      wanting = { ...want, why: err instanceof Error ? err.message : String(err) };
    } finally {
      codeBusy = false;
    }
  }

  function stopWanting() {
    const want = wanting;
    wanting = undefined;
    want?.done();
  }
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
      <p class="muted" data-testid="sign-up-line">{signUpLine(openSignUp)}</p>
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
  {:else if place.kind === 'reconnect'}
    <p class="back"><a href={formatServerHash({ kind: 'list' })}>← All events</a></p>
    {#if account !== undefined}
      <ServerReconnect
        api={people}
        secret={place.secret}
        account={account ?? undefined}
        {providers}
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
      {#if account.owner}
        <ServerSignUpSetting api={people} onchange={(open) => (openSignUp = open)} />
        <ServerClubs
          api={people}
          onchange={() => {
            listAgain += 1;
          }}
        />
      {/if}
    {:else if account === null}
      <p class="event">Sign in</p>
      <ServerSignIn api={people} {providers} />
    {/if}
  {:else if place.kind === 'c'}
    <p class="back"><a href={formatServerHash({ kind: 'list' })}>← All events</a></p>
    {#if !wordedNow}
      <p class="muted" role="status">Loading…</p>
    {:else if !wordedNow.club}
      <Card elevation="sm">
        <p class="event">This club is not here</p>
        <p>The address may not be right.</p>
      </Card>
    {:else}
      {@const club = wordedNow.club}
      <section aria-label={club.name} data-testid="server-one-club">
        <p class="event">
          {club.name}
          {#if club.archived}<Badge tone="neutral">Archived</Badge>{/if}
        </p>
        {#if club.archived}
          <p class="muted">This club is archived. Its events can still be looked at.</p>
        {/if}
        {#if wordedNow.events.length === 0}
          <p class="muted">No events yet.</p>
        {:else}
          {@render eventRows(club.id, wordedNow.events)}
        {/if}
      </section>
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
        owner={account.owner}
        {providers}
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
        <SpectatorView {live} {pilot} {signup} {checkin} />
      {/if}
    {/if}
  {:else if clubs === undefined || shown === undefined}
    {#if !offline}<p class="muted" role="status">Loading…</p>{/if}
  {:else}
    {#if place.kind === 'clubs' && account?.primary !== undefined}
      <p class="back"><a href={formatServerHash({ kind: 'list' })}>← Your club</a></p>
    {/if}
    {#if total === 0}
      <Card elevation="sm">
        {#if primary}
          <p class="event">{primary.club.name}</p>
          <p data-testid="no-events">No events yet.</p>
        {:else}
          <p class="event">No events yet</p>
          <p>When a director sends one, it will show here.</p>
        {/if}
      </Card>
    {/if}
    {#each shown as { club, events } (club.id)}
      {#if events.length > 0}
        <section aria-label={club.name}>
          <h2>
            {club.name}
            {#if club.archived}<Badge tone="neutral">Archived</Badge>{/if}
          </h2>
          {@render eventRows(club.id, events)}
        </section>
      {/if}
    {/each}
    {#if primary && clubs.length > 1}
      <p class="muted" data-testid="all-clubs">
        This is your primary club.
        <a href={formatServerHash({ kind: 'clubs' })}>Every club on this server</a>
      </p>
    {/if}
  {/if}
</main>

{#snippet eventRows(club: string, events: readonly ServerEvent[])}
  <ul>
    {#each events as event (event.id)}
      <li>
        <a
          class="row"
          href={formatServerHash({ kind: 'event', club, event: event.id })}
          data-testid="server-event-link"
        >
          <span class="name">{event.name}</span>
          {#if event.hidden}<Badge tone="neutral">Hidden</Badge>{/if}
          {#if eventDetail(event)}<span class="muted">{eventDetail(event)}</span>{/if}
        </a>
      </li>
    {/each}
  </ul>
{/snippet}

{#if wanting}
  <Dialog open={true} title="Are you at the race?" onclose={stopWanting}>
    <form id="race-code-form" class="race-code" onsubmit={sendCode}>
      <p>
        Taking a seat is for pilots who are at the race. Type the <strong>race code</strong>
        the director shows there. You only need to do this once.
      </p>
      {#if wanting.why}
        <Banner tone="danger"><span data-testid="race-code-why">{wanting.why}</span></Banner>
      {/if}
      <label for="race-code">Race code</label>
      <Input
        id="race-code"
        autocomplete="off"
        autocapitalize="characters"
        spellcheck={false}
        maxlength={12}
        bind:value={typed}
        disabled={codeBusy}
      />
      {#if typedProblem}<p class="muted" role="status">{typedProblem}</p>{/if}
    </form>
    {#snippet footer()}
      <Button variant="ghost" onclick={stopWanting}>Cancel</Button>
      <Button
        variant="primary"
        type="submit"
        form="race-code-form"
        disabled={codeBusy || typed.trim() === '' || typedProblem !== undefined}
        >Take the seat</Button
      >
    {/snippet}
  </Dialog>
{/if}

<style>
  .race-code {
    display: grid;
    gap: var(--gf-space-3);
    max-width: 28rem;
  }
  .race-code p {
    margin: 0;
  }
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
