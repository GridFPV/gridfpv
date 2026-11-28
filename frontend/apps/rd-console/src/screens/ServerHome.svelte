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
  import { Badge, Banner, Button, Card, Dialog, Input, Select } from '@gridfpv/components';
  import Page from '../Page.svelte';
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
    Me,
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
    type AccountPage,
    type AdminPage,
    type ManagePage,
    type ServerPlace
  } from '../lib/serverHome.js';
  import {
    lookedAfterClubs,
    peopleApi,
    signInProblem,
    signUpLine,
    type PeopleApi
  } from '../lib/serverPeople.js';
  import { publicSource, type PublicSource } from '../lib/spectator.js';
  import ServerAccount from './ServerAccount.svelte';
  import ServerClub from './ServerClub.svelte';
  import ServerInvite from './ServerInvite.svelte';
  import ServerNav from './ServerNav.svelte';
  import ServerSignInServices from './ServerSignInServices.svelte';
  import ServerJoin from './ServerJoin.svelte';
  import ServerReconnect from './ServerReconnect.svelte';
  import ServerClubs from './ServerClubs.svelte';
  import ServerConfirmEmail from './ServerConfirmEmail.svelte';
  import ServerResults from './ServerResults.svelte';
  import ServerSignIn from './ServerSignIn.svelte';
  import ServerPasswordSetting from './ServerPasswordSetting.svelte';
  import ServerEmailSetting from './ServerEmailSetting.svelte';
  import ServerSignUpSetting from './ServerSignUpSetting.svelte';
  import SpectatorView from './SpectatorView.svelte';

  /** An event's reads, with the three things a pilot can ask for when they are there. */
  type EventSource = LiveSession &
    Partial<Pick<PublicSource, 'takeSpot' | 'takeNextSpot' | 'leaveSpot'>>;

  /** A club in the list: its friendly address and whether it is archived, when told. */
  type ListedClub = PushClub & Partial<Pick<ClubOnServer, 'word' | 'archived' | 'open'>>;

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
  /** Whether people may sign in here with a username and password. */
  let passwordOn = $state(false);
  /** Whether this server sends email. */
  let emailOn = $state(false);
  /** Whether there is any way to sign in here. */
  const canSignIn = $derived((providers !== undefined && providers.length > 0) || passwordOn);
  /** Someone signed in with a password. `to` is where they land. */
  function signedIn(me: Me, to?: ServerPlace) {
    account = me.account ?? null;
    listAgain += 1;
    if (to) go(to);
  }

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
      .then((info) => {
        openSignUp = info.open_sign_up;
        passwordOn = info.password_sign_in ?? false;
        emailOn = info.email ?? false;
      })
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
      place.kind === 'manage' ||
      place.kind === 'admin' ||
      place.kind === 'admin-home' ||
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
    if (place.kind !== 'club') return;
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
    place.kind === 'club' && worded?.word === place.word ? worded : undefined
  );

  // ── The clubs the signed-in person looks after ─────────────────────────────
  const lookedAfter = $derived(
    lookedAfterClubs(account ?? undefined, clubs?.map((entry) => entry.club) ?? [])
  );
  /** The club an address under `#/admin/club/` names: by its friendly word, or else its id. */
  const managedWord = $derived(place.kind === 'manage' ? place.club : undefined);
  const managedClub = $derived(
    managedWord === undefined
      ? undefined
      : clubs?.find((entry) => entry.club.word === managedWord || entry.club.id === managedWord)
          ?.club
  );
  /**
   * A club's address follows its name. When the club on the page is renamed, the address
   * in the bar is the old one: go on to the new one, on the same page of the club.
   */
  let lastManaged: { word: string; id: string } | undefined;
  $effect(() => {
    if (place.kind !== 'manage' || clubs === undefined) return;
    if (managedClub) {
      lastManaged = { word: place.club, id: managedClub.id };
      return;
    }
    const before = lastManaged;
    if (!before || before.word !== place.club) return;
    const now = clubs.find((entry) => entry.club.id === before.id)?.club;
    if (now?.word && now.word !== place.club) {
      location.replace(formatServerHash({ kind: 'manage', club: now.word, page: place.page }));
    }
  });

  // `#/admin` is the way in to the admin area. It goes on to the first thing the person
  // runs: a club they look after, or else the server itself.
  $effect(() => {
    if (place.kind !== 'admin-home' || !account || clubs === undefined) return;
    const first = lookedAfter[0];
    if (first) {
      location.replace(
        formatServerHash({
          kind: 'manage',
          club: first.club.word ?? first.club.id,
          page: first.runs ? 'members' : 'pilots'
        })
      );
    } else if (account.owner) {
      location.replace(formatServerHash({ kind: 'admin', page: 'clubs' }));
    }
  });

  /** That club, when the signed-in person looks after it. */
  const managed = $derived(lookedAfter.find((mine) => mine.club.id === managedClub?.id));

  const ACCOUNT_TITLES: Record<AccountPage, string> = {
    profile: 'Profile',
    clubs: 'Clubs',
    'sign-in': 'Sign-in'
  };
  const MANAGE_TITLES: Record<ManagePage, string> = {
    members: 'Members',
    invites: 'Invites',
    pilots: 'Pilots',
    events: 'Events',
    tokens: 'Link tokens',
    settings: 'Settings'
  };
  const ADMIN_TITLES: Record<AdminPage, string> = {
    clubs: 'Clubs',
    'sign-in': 'Sign-in',
    email: 'Email'
  };

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

<ServerNav {place} {account} {canSignIn} myClubs={lookedAfter} />

{#if offline}
  <p class="offline" role="status" data-testid="server-offline">
    Not connected right now. Trying again.
  </p>
{/if}

{#if place.kind === 'sign-in'}
  <Page title="Sign in">
    {#if account}
      <Card elevation="sm">
        <p>You are signed in as <strong>{account.name}</strong>.</p>
        {#if signInProblem(place.problem)}<p class="muted">{signInProblem(place.problem)}</p>{/if}
      </Card>
    {:else}
      <div class="narrow">
        <Card elevation="md">
          <div class="block">
            <ServerSignIn
              api={people}
              {providers}
              problem={signInProblem(place.problem)}
              password={passwordOn}
              email={emailOn}
              canMake={openSignUp === true}
              onsignedin={(me) =>
                signedIn(
                  me,
                  me.account?.clubs.length ? { kind: 'list' } : { kind: 'account', page: 'profile' }
                )}
            />
            {#if signUpLine(openSignUp) !== ''}
              <p class="muted" data-testid="sign-up-line">{signUpLine(openSignUp)}</p>
            {/if}
          </div>
        </Card>
      </div>
    {/if}
  </Page>
{:else if place.kind === 'invite'}
  <Page title="Invite">
    {#if account !== undefined}
      <div class="narrow">
        <ServerInvite
          api={people}
          secret={place.secret}
          account={account ?? undefined}
          {providers}
          password={passwordOn}
          email={emailOn}
          onsignedin={(me) => signedIn(me, { kind: 'account', page: 'clubs' })}
          onjoined={async () => {
            await readMe();
            go({ kind: 'account', page: 'clubs' });
          }}
        />
      </div>
    {/if}
  </Page>
{:else if place.kind === 'confirm-email'}
  <Page title="Email">
    <div class="narrow">
      <ServerConfirmEmail api={people} secret={place.secret} onconfirmed={readMe} />
    </div>
  </Page>
{:else if place.kind === 'reconnect'}
  <Page title="Get back in">
    {#if account !== undefined}
      <div class="narrow">
        <ServerReconnect
          api={people}
          secret={place.secret}
          account={account ?? undefined}
          {providers}
          password={passwordOn}
          email={emailOn}
          onsignedin={(me) => signedIn(me, { kind: 'account', page: 'sign-in' })}
        />
      </div>
    {/if}
  </Page>
{:else if place.kind === 'account'}
  <Page title={ACCOUNT_TITLES[place.page]}>
    {#if account}
      <ServerAccount
        api={people}
        {account}
        section={place.page}
        {providers}
        password={passwordOn}
        email={emailOn}
        onchange={(me) => {
          account = me.account ?? null;
          listAgain += 1;
        }}
        onreread={readMe}
        onsignedout={() => {
          account = null;
          go({ kind: 'list' });
        }}
      />
    {:else if account === null}
      {@render signInFirst()}
    {/if}
  </Page>
{:else if place.kind === 'admin'}
  <Page title={ADMIN_TITLES[place.page]}>
    {#if account?.owner}
      {#if place.page === 'clubs'}
        <ServerClubs
          api={people}
          onchange={() => {
            listAgain += 1;
          }}
        />
      {:else if place.page === 'sign-in'}
        <div class="cards">
          <ServerSignUpSetting api={people} onchange={(open) => (openSignUp = open)} />
          <ServerPasswordSetting api={people} onchange={(on) => (passwordOn = on)} />
          <ServerSignInServices
            api={people}
            onchange={() => {
              people
                .providers()
                .then((found) => (providers = found))
                .catch(() => {});
            }}
          />
        </div>
      {:else}
        <ServerEmailSetting api={people} onchange={(on) => (emailOn = on)} />
      {/if}
    {:else if account}
      <Card elevation="sm"><p>Only an owner of this server can open this page.</p></Card>
    {:else if account === null}
      {@render signInFirst()}
    {/if}
  </Page>
{:else if place.kind === 'club'}
  {#if !wordedNow}
    <Page title="Club"><p class="muted" role="status">Loading…</p></Page>
  {:else if !wordedNow.club}
    <Page title="Club">
      <Card elevation="sm">
        <p class="event">This club is not here</p>
        <p>The address may not be right.</p>
      </Card>
    </Page>
  {:else}
    {@const club = wordedNow.club}
    <Page title={club.name}>
      {#snippet actions()}
        {#if club.archived}<Badge tone="neutral">Archived</Badge>{/if}
        {#if lookedAfter.some((mine) => mine.club.id === club.id)}
          <a
            class="manage"
            href={formatServerHash({
              kind: 'manage',
              club: club.word,
              page: lookedAfter.find((mine) => mine.club.id === club.id)?.runs
                ? 'members'
                : 'pilots'
            })}>Manage</a
          >
        {/if}
      {/snippet}
      <section aria-label={club.name} data-testid="server-one-club" class="stack">
        <ServerJoin
          api={people}
          {club}
          {account}
          onchange={(me) => (account = me.account ?? null)}
        />
        <Card elevation="sm">
          <div class="block">
            <h2>Events</h2>
            {#if wordedNow.events.length === 0}
              <p class="muted">No events yet.</p>
            {:else}
              {@render eventRows(club.id, wordedNow.events)}
            {/if}
          </div>
        </Card>
      </section>
    </Page>
  {/if}
{:else if place.kind === 'manage'}
  {@const mine = managed}
  {@const page = place.page}
  <Page title={MANAGE_TITLES[page]} lede={lookedAfter.length > 1 ? undefined : mine?.club.name}>
    {#snippet actions()}
      {#if mine && lookedAfter.length > 1}
        <span class="club-pick">
          <Select
            size="sm"
            value={mine.club.word ?? mine.club.id}
            aria-label="Club"
            data-testid="club-pick"
            onchange={(e: Event) => {
              const word = (e.currentTarget as HTMLSelectElement).value;
              const next = lookedAfter.find((other) => (other.club.word ?? other.club.id) === word);
              if (next) go({ kind: 'manage', club: word, page: next.runs ? page : 'pilots' });
            }}
          >
            {#each lookedAfter as other (other.club.id)}
              <option value={other.club.word ?? other.club.id}>{other.club.name}</option>
            {/each}
          </Select>
        </span>
      {/if}
    {/snippet}
    {#if account && mine}
      <ServerClub
        api={people}
        club={mine.club}
        section={mine.runs ? place.page : 'pilots'}
        me={account.id}
        runs={mine.runs}
        owner={account.owner}
        {providers}
        onchange={() => {
          listAgain += 1;
          void readMe();
        }}
      />
    {:else if account && managedClub}
      <Card elevation="sm"><p>Only those who look after this club can open this page.</p></Card>
    {:else if account === null}
      {@render signInFirst()}
    {:else if account && clubs !== undefined}
      <Card elevation="sm"><p class="event">This club is not here</p></Card>
    {/if}
  </Page>
{:else if place.kind === 'admin-home'}
  <Page title="Admin">
    {#if account === null}
      {@render signInFirst()}
    {:else if account && clubs !== undefined && lookedAfter.length === 0 && !account.owner}
      <Card elevation="sm">
        <p data-testid="admin-nothing">You do not run a club or this server.</p>
      </Card>
    {:else}
      <p class="muted" role="status">Loading…</p>
    {/if}
  </Page>
{:else if place.kind === 'results'}
  <Page title="My results">
    {#if account}
      <ServerResults api={people} />
    {:else if account === null}
      {@render signInFirst()}
    {/if}
  </Page>
{:else if picked}
  <Page title={pickedEvent?.name ?? 'Event'}>
    {#if gone}
      <Card elevation="sm">
        <p class="event">This event is not here</p>
        <p>It may have been removed, or the address is not right.</p>
      </Card>
    {:else}
      {#if pickedEvent}
        <p class="visually-hidden" data-testid="server-event">{pickedEvent.name}</p>
        {#if eventDetail(pickedEvent) || pickedEvent.organizer?.trim()}
          <p class="muted">
            {eventDetail(pickedEvent)}
            {#if pickedEvent.organizer?.trim()}
              <span data-testid="server-event-organizer">Run by {pickedEvent.organizer.trim()}</span
              >
            {/if}
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
  </Page>
{:else if clubs === undefined || shown === undefined}
  <Page title={place.kind === 'clubs' ? 'Clubs' : 'Events'}>
    {#if !offline}<p class="muted" role="status">Loading…</p>{/if}
  </Page>
{:else if place.kind === 'clubs'}
  <Page title="Clubs">
    {#if clubs.length === 0}
      <Card elevation="sm"><p class="muted">No clubs yet.</p></Card>
    {/if}
    <div class="cards">
      {#each clubs as { club, events } (club.id)}
        <Card elevation="sm">
          <section class="block" aria-label={club.name} data-testid="club-card">
            <div class="head">
              <h2>
                {#if club.word}
                  <a href={formatServerHash({ kind: 'club', word: club.word })}>{club.name}</a>
                {:else}{club.name}{/if}
              </h2>
              {#if club.archived}<Badge tone="neutral">Archived</Badge>{/if}
              {#if club.open !== undefined}
                <Badge tone="neutral">{club.open ? 'Public' : 'Private'}</Badge>
              {/if}
            </div>
            {#if events.length === 0}
              <p class="muted">No events yet.</p>
            {:else}
              {@render eventRows(club.id, events)}
            {/if}
            <ServerJoin
              api={people}
              {club}
              {account}
              quiet
              onchange={(me) => (account = me.account ?? null)}
            />
          </section>
        </Card>
      {/each}
    </div>
  </Page>
{:else}
  <Page title={primary ? primary.club.name : 'Events'}>
    {#if total === 0}
      <Card elevation="sm">
        <p class="muted" data-testid="no-events">No events yet.</p>
      </Card>
    {/if}
    <div class="cards">
      {#each shown as { club, events } (club.id)}
        {#if events.length > 0}
          <Card elevation="sm">
            <section class="block" aria-label={club.name}>
              {#if !primary}
                <div class="head">
                  <h2>{club.name}</h2>
                  {#if club.archived}<Badge tone="neutral">Archived</Badge>{/if}
                </div>
              {/if}
              {@render eventRows(club.id, events)}
            </section>
          </Card>
        {/if}
      {/each}
    </div>
  </Page>
{/if}

{#snippet signInFirst()}
  <div class="narrow">
    <ServerSignIn
      api={people}
      {providers}
      password={passwordOn}
      email={emailOn}
      canMake={openSignUp === true}
      onsignedin={(me) => signedIn(me)}
    />
  </div>
{/snippet}

{#snippet eventRows(club: string, events: readonly ServerEvent[])}
  <ul class="tiles">
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
  /* Cards side by side on a wide screen, one under the other on a narrow one. */
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(24rem, 100%), 1fr));
    gap: var(--gf-space-4);
    align-items: start;
  }
  .stack,
  .block {
    display: grid;
    gap: var(--gf-space-3);
  }
  /* A form someone fills in is not stretched across a wide screen. */
  .narrow {
    width: min(28rem, 100%);
    display: grid;
    gap: var(--gf-space-3);
  }
  .head {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
  }
  .head h2 {
    flex: 1;
    margin: 0;
  }
  .head h2 a {
    color: inherit;
  }
  .offline {
    width: min(76rem, 100%);
    margin: var(--gf-space-3) auto 0;
    padding: 0 var(--gf-space-6);
    box-sizing: border-box;
    color: var(--gf-warn, var(--gf-text-muted));
    font-size: var(--gf-font-size-sm);
  }
  .club-pick {
    width: 14rem;
  }
  .manage {
    font-size: var(--gf-font-size-md);
  }
  .visually-hidden {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
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
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--gf-space-2);
  }
  /* Events side by side on a wide screen. */
  .tiles {
    grid-template-columns: repeat(auto-fill, minmax(min(18rem, 100%), 1fr));
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
