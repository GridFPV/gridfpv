<script lang="ts">
  /**
   * ServerNav — what every page of a remote server sits under (docs/primitives.md §13). It
   * is the Director's own nav, for a server: the brand, a switch between the parts of the
   * area, and the row of pages of the part that is showing.
   *
   * The page has two areas, kept apart. Someone who runs a club or the server has a
   * Pilot / Admin switch at the end of the bar to go between them. Nobody else sees it.
   *
   * The **pilot area** is what everyone sees:
   * - **Events**: the events, the clubs, and a signed-in pilot's results.
   * - **Account**: the signed-in person's own settings.
   *
   * The **admin area** looks different on purpose, so it is plain which one is open:
   * - **Club** is for those who run a club: its members, invites, pilots, events, link
   *   tokens and settings. With more than one club, the page itself has a box to pick which.
   * - **Server** is for an owner of the server: its clubs, how people sign in, and email.
   *
   * A part someone has no say in is not shown. Using a thing and setting it up are kept
   * apart: nothing in the pilot area runs a club or the server.
   *
   * Everything is a plain link (`#/…`), so middle-click, copy-link and the back button work.
   */
  import type { MyAccount } from '@gridfpv/types';
  import Brand from '../Brand.svelte';
  import {
    formatServerHash,
    modeOfPlace,
    sideOfPlace,
    type ServerPlace,
    type ServerSide
  } from '../lib/serverHome.js';

  /** A club the signed-in person looks after. */
  interface MyClub {
    club: { id: string; name: string; word?: string };
    /** Whether they run it. A claims approver does not: they see only its pilots. */
    runs: boolean;
  }

  let {
    place,
    account,
    canSignIn,
    myClubs
  }: {
    /** The page showing now. */
    place: ServerPlace;
    /** Who is signed in. `null` for nobody, `undefined` while that is not known. */
    account: MyAccount | null | undefined;
    /** Whether there is any way to sign in on this server. */
    canSignIn: boolean;
    /** The clubs the signed-in person looks after. */
    myClubs: readonly MyClub[];
  } = $props();

  interface Entry {
    key: string;
    label: string;
    to: ServerPlace;
  }

  /** What a club is called in an address: its friendly word, or else its id. */
  const wordOf = (club: MyClub['club']): string => club.word ?? club.id;

  /** The club the Club part is about: the one in the address, or else the first. */
  const managed = $derived(
    (place.kind === 'manage'
      ? myClubs.find((mine) => wordOf(mine.club) === place.club || mine.club.id === place.club)
      : undefined) ?? myClubs[0]
  );

  const side = $derived<ServerSide | undefined>(sideOfPlace(place));

  const pages = $derived.by((): Entry[] => {
    if (side === 'account') {
      return [
        { key: 'profile', label: 'Profile', to: { kind: 'account', page: 'profile' } },
        { key: 'clubs', label: 'Clubs', to: { kind: 'account', page: 'clubs' } },
        { key: 'sign-in', label: 'Sign-in', to: { kind: 'account', page: 'sign-in' } }
      ];
    }
    if (side === 'club' && managed) {
      const club = wordOf(managed.club);
      const all: Entry[] = [
        { key: 'members', label: 'Members', to: { kind: 'manage', club, page: 'members' } },
        { key: 'invites', label: 'Invites', to: { kind: 'manage', club, page: 'invites' } },
        { key: 'pilots', label: 'Pilots', to: { kind: 'manage', club, page: 'pilots' } },
        { key: 'events', label: 'Events', to: { kind: 'manage', club, page: 'events' } },
        { key: 'tokens', label: 'Link tokens', to: { kind: 'manage', club, page: 'tokens' } },
        { key: 'settings', label: 'Settings', to: { kind: 'manage', club, page: 'settings' } }
      ];
      // A claims approver answers who holds which pilot, and nothing else.
      return managed.runs ? all : all.filter((page) => page.key === 'pilots');
    }
    if (side === 'server') {
      return [
        { key: 'clubs', label: 'Clubs', to: { kind: 'admin', page: 'clubs' } },
        { key: 'sign-in', label: 'Sign-in', to: { kind: 'admin', page: 'sign-in' } },
        { key: 'email', label: 'Email', to: { kind: 'admin', page: 'email' } }
      ];
    }
    if (side === 'events') {
      return [
        { key: 'list', label: 'Events', to: { kind: 'list' } },
        { key: 'clubs', label: 'Clubs', to: { kind: 'clubs' } },
        ...(account
          ? [{ key: 'results', label: 'My results', to: { kind: 'results' } } as Entry]
          : [])
      ];
    }
    return [];
  });

  /** Which entry of the row the showing page is. */
  const current = $derived.by((): string | undefined => {
    switch (place.kind) {
      case 'account':
      case 'manage':
      case 'admin':
        return place.page;
      case 'list':
      case 'event':
      case 'here':
        return 'list';
      case 'clubs':
      case 'club':
        return 'clubs';
      case 'results':
        return 'results';
      default:
        return undefined;
    }
  });

  const mode = $derived(modeOfPlace(place));

  const clubHref = $derived(
    managed
      ? formatServerHash({
          kind: 'manage',
          club: wordOf(managed.club),
          page: managed.runs ? 'members' : 'pilots'
        })
      : '#/'
  );
  const serverHref = formatServerHash({ kind: 'admin', page: 'clubs' });
  /** Whether the signed-in person runs anything: a club, or the server. */
  const runsSomething = $derived(!!account && (!!managed || account.owner));
  /** Where the admin area opens: the club they run, or else the server. */
  const adminHref = $derived(managed ? clubHref : serverHref);
</script>

<div class="bar" data-mode={mode} data-testid="server-bar">
  <header class="nav">
    <Brand
      sub={mode === 'admin' ? 'Admin' : 'Server'}
      onclick={() => (location.hash = mode === 'admin' ? adminHref : '#/')}
    />
    <nav class="sides" aria-label="Parts of this server">
      {#if mode === 'admin'}
        {#if account && managed}
          <a
            href={clubHref}
            class:active={side === 'club'}
            aria-current={side === 'club' ? 'true' : undefined}
            title="Run {managed.club.name}">Club</a
          >
        {/if}
        {#if account?.owner}
          <a
            href={serverHref}
            class:active={side === 'server'}
            aria-current={side === 'server' ? 'true' : undefined}
            title="Run this server: clubs, sign-in and email">Server</a
          >
        {/if}
      {:else}
        <a
          href="#/"
          class:active={side === 'events'}
          aria-current={side === 'events' ? 'true' : undefined}
          title="Events, clubs and results">Events</a
        >
        {#if account}
          <a
            href={formatServerHash({ kind: 'account', page: 'profile' })}
            class:active={side === 'account'}
            aria-current={side === 'account' ? 'true' : undefined}
            title="Your own profile, clubs and sign-in">Account</a
          >
        {/if}
      {/if}
    </nav>
    <nav class="pages" aria-label="Pages">
      {#each pages as page (page.key)}
        <a
          href={formatServerHash(page.to)}
          class:active={current === page.key}
          aria-current={current === page.key ? 'page' : undefined}>{page.label}</a
        >
      {/each}
    </nav>
    <div class="end">
      {#if runsSomething}
        <nav class="mode" aria-label="Area" data-testid="server-mode">
          <a
            href="#/"
            class:active={mode === 'pilot'}
            aria-current={mode === 'pilot' ? 'true' : undefined}
            title="What everyone sees: events, clubs and your account">Pilot</a
          >
          <a
            href={adminHref}
            class:active={mode === 'admin'}
            aria-current={mode === 'admin' ? 'true' : undefined}
            title={account?.owner ? 'Run your clubs and this server' : 'Run your club'}>Admin</a
          >
        </nav>
      {/if}
      {#if account}
        <a
          class="who"
          href={formatServerHash({ kind: 'account', page: 'profile' })}
          data-testid="server-who"
          title="Your account">{account.name}</a
        >
      {:else if account === null && canSignIn}
        <a class="sign-in" href={formatServerHash({ kind: 'sign-in' })} data-testid="server-sign-in"
          >Sign in</a
        >
      {/if}
    </div>
  </header>
</div>

<style>
  /* The admin area has a bar of its own colour, so it is plain which area is open. */
  .bar[data-mode='admin'] {
    padding-bottom: var(--gf-space-3);
    background: var(--gf-surface-alt);
    border-bottom: 2px solid var(--gf-warn);
  }
  .bar[data-mode='admin'] .sides a.active,
  .bar[data-mode='admin'] .pages a.active {
    background: var(--gf-warn-soft);
  }
  .bar[data-mode='admin'] .sides a.active {
    box-shadow: inset 0 -2px 0 var(--gf-warn);
  }
  /* Pilot / Admin: the two halves of one small switch. */
  .mode {
    display: flex;
    padding: 2px;
    border: 1px solid var(--gf-border);
    border-radius: 999px;
  }
  .mode a {
    padding: var(--gf-space-1) var(--gf-space-3);
    border-radius: 999px;
    text-decoration: none;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
  }
  .mode a:hover {
    color: var(--gf-text);
  }
  .mode a.active {
    color: var(--gf-accent-contrast);
    background: var(--gf-accent);
  }
  .bar[data-mode='admin'] .mode a.active {
    color: var(--gf-neutral-0);
    background: var(--gf-warn);
  }
  .nav {
    display: flex;
    align-items: center;
    gap: var(--gf-space-5);
    flex-wrap: wrap;
    width: min(76rem, 100%);
    margin: 0 auto;
    padding: var(--gf-space-4) var(--gf-space-6) 0;
    font-family: var(--gf-font-family);
    box-sizing: border-box;
  }
  /* The switch: the parts of one control. */
  .sides {
    display: flex;
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    overflow: hidden;
  }
  .sides a {
    display: flex;
    align-items: center;
    padding: var(--gf-space-2) var(--gf-space-4);
    text-decoration: none;
    color: var(--gf-text-muted);
    line-height: 1.25;
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
  }
  .sides a + a {
    border-left: 1px solid var(--gf-border);
  }
  .sides a:hover {
    color: var(--gf-text);
  }
  .sides a.active {
    color: var(--gf-text);
    background: var(--gf-surface);
    box-shadow: inset 0 -2px 0 var(--gf-accent);
  }
  .pages {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-1);
  }
  .pages a {
    display: inline-flex;
    align-items: center;
    padding: var(--gf-space-1) var(--gf-space-3);
    border-radius: var(--gf-radius-md);
    text-decoration: none;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-md);
  }
  .pages a:hover {
    color: var(--gf-text);
  }
  .pages a.active {
    color: var(--gf-text);
    background: var(--gf-surface);
  }
  .end {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    min-width: 0;
  }
  .who,
  .sign-in {
    max-width: 14rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    padding: var(--gf-space-1) var(--gf-space-3);
    border-radius: var(--gf-radius-md);
    text-decoration: none;
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
    color: var(--gf-text);
  }
  .who:hover {
    background: var(--gf-surface);
  }
  .sign-in {
    color: var(--gf-accent-contrast);
    background: var(--gf-accent);
  }
  a:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
  /* A phone: the row wraps, and the pages can be swiped sideways. */
  @media (max-width: 40rem) {
    .nav {
      gap: var(--gf-space-3);
      padding: var(--gf-space-3) var(--gf-space-3) 0;
    }
    .pages {
      order: 5;
      width: 100%;
      flex-wrap: nowrap;
      overflow-x: auto;
    }
    .pages a {
      white-space: nowrap;
    }
    .sides a {
      padding: var(--gf-space-2) var(--gf-space-3);
    }
  }
</style>
