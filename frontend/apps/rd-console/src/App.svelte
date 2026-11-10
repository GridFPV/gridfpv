<script lang="ts">
  /**
   * The RD console shell (#51; two-level app IA per #118).
   *
   * The console is a **home hub of pages** plus the open event's page. The shell shows an
   * **app-level route**: the {@link HomeHub} (landing), or one of its pages — {@link EventPicker}
   * (the Events page), {@link PilotsPage}, {@link ClassesPage}. Selecting an event on the Events
   * page opens {@link EventPage}, and a heat on it opens that heat's {@link RunPage}.
   *
   * On load the shell resolves the Director's active event (server state, #90) so an `#/event`
   * URL can render, but the hash is authoritative for *which* view to show (#118): a bare/hub
   * load stays on the hub and never opens an event — the user only gets there by navigating in or
   * loading the event URL.
   *
   * The RD token is handled **lazily**: reads need none, and a write the Director refuses prompts
   * for it via {@link TokenDialog} (registered as the session's token provider).
   *
   * The console is "just a co-located web client over the same protocol": every call goes through
   * `@gridfpv/protocol-client`, held by the shared `Session`. Screens compose the shared
   * `@gridfpv/components`; this shell only routes between them and owns the session.
   */
  import '@gridfpv/components/tokens.css';
  import { ToastHost } from '@gridfpv/components';
  import { Session } from './lib/session.svelte.js';
  import { EventLive } from './lib/eventLive.svelte.js';
  import HomeHub from './screens/HomeHub.svelte';
  import EventPicker from './screens/EventPicker.svelte';
  import EventPage from './screens/EventPage.svelte';
  import RunPage from './screens/RunPage.svelte';
  import PilotsPage from './screens/PilotsPage.svelte';
  import ClassesPage from './screens/ClassesPage.svelte';
  import TokenDialog from './screens/TokenDialog.svelte';
  import {
    parseHash,
    formatHash,
    reconcileRoute,
    resolveInitialRoute,
    type Route,
    type AppPage
  } from './lib/route.js';

  const session = new Session();

  // ── Hash-based URL routing (#118) ──────────────────────────────────────────
  // The view is reflected in `location.hash` so a refresh stays on the same page and
  // back/forward + bookmarks work. The hash is the source of truth: navigating sets it, and the
  // reactive `route` is parsed from it on load and on every `hashchange`/`popstate`. We use a hash
  // (not a path) because `/pilots`, `/events`, `/classes` are Director API routes — the `#`
  // stays client-side and can't collide. See `lib/route.ts` for the scheme + reconciliation rules.
  //
  // `route` is the *intended* view. It's reconciled against the live active event (server state)
  // for rendering: an event route with no active event reconciles to the Events page. The hash
  // is authoritative — an empty/hub hash stays on the hub even when an event is active.
  let route = $state<Route>(parseHash(location.hash));

  // Navigate to a route: this is the single mutation point. It updates `location.hash`, which (for
  // a real change) fires `hashchange` and reloads `route` from the hash — keeping hash and view in
  // lockstep. We also set `route` directly so a no-op hash write (same hash) still re-renders.
  function navigate(next: Route) {
    const hash = formatHash(next);
    route = next;
    if (location.hash !== hash) location.hash = hash;
  }

  // Page navigations (hub cards, breadcrumbs, the brand).
  const goPage = (page: AppPage) => navigate({ kind: 'page', page });
  const goHome = () => goPage('home');

  // On `hashchange`/`popstate` (back/forward, manual edit, or our own `navigate`) re-derive the
  // route from the hash. Reconcile against the live active event so an event hash with no event
  // doesn't render an empty page.
  function onHashChange() {
    route = reconcileRoute(parseHash(location.hash), !!session.currentEvent);
  }

  // The lazy token prompt: register a provider that opens the TokenDialog and resolves
  // with the entered token (or undefined if cancelled). This is the only auth surface.
  let tokenDialog = $state<TokenDialog>();
  session.setTokenProvider(() => tokenDialog?.request() ?? Promise.resolve(undefined));

  // Resolve the *initial* route on load (#118): the active event is server-side state, so we first
  // read `GET /active-event` (it determines whether an event hash can render), then resolve the
  // route from the hash + whether an event is active. While it resolves we show a brief loading
  // state; after this the hash is the source of truth.
  let resumed = false;
  $effect(() => {
    if (resumed) return;
    resumed = true;
    void (async () => {
      await session.resolveActiveEvent();
      navigate(resolveInitialRoute(location.hash, !!session.currentEvent));
    })();
  });

  // Opening an event happens *inside* the EventPicker via session state (`chooseEvent` /
  // `createEventAndEnter` set `session.currentEvent`), not through a `navigate` call here. So when
  // an event becomes open while we're on the Events page, flip the route to the event page so
  // the hash becomes `#/event`.
  $effect(() => {
    if (session.currentEvent && route.kind === 'page' && route.page === 'events') {
      navigate({ kind: 'event' });
    }
  });

  // The live competition of the open event: one store, shared by the event page and the Run
  // view, so moving between them does not read everything again. It lives only while an event
  // screen is showing; on the hub or a directory page there is nothing to keep fresh.
  let live = $state.raw<EventLive | undefined>(undefined);
  const liveEventId = $derived(route.kind === 'page' ? undefined : session.currentEvent?.id);
  $effect(() => {
    const id = liveEventId;
    if (!id) {
      live = undefined;
      return;
    }
    const store = new EventLive(session, id);
    live = store;
    store.start();
    return () => store.stop();
  });

  // Leaving the event page closes the event in this console (the Director's active event is left
  // alone) and lands on the page asked for.
  function leaveEventTo(page: AppPage) {
    session.leaveEvent();
    goPage(page);
  }
</script>

<svelte:window onhashchange={onHashChange} onpopstate={onHashChange} />

<div class="gridfpv-root gridfpv-dense">
  {#if session.resolvingActiveEvent && !session.currentEvent}
    <!-- Resolving the Director's active event (#90): a brief loading state before we know
         whether an event page can render. -->
    <div class="resume-loading" role="status">
      <span class="resume-spinner" aria-hidden="true"></span>
      <span>Resuming…</span>
    </div>
  {:else if route.kind === 'page'}
    <!-- App-level routes (#118): the home hub, or one of its pages. The view is driven by the
         hash, not just `session.currentEvent` — an explicit page hash (e.g. `#/pilots`) shows that
         page even if an event is active on the Director, so a refresh on a top-level page stays
         put. Each page owns its own breadcrumb back to Home. -->
    {#if route.page === 'home'}
      <HomeHub
        {session}
        onpilots={() => goPage('pilots')}
        onclasses={() => goPage('classes')}
        onevents={() => goPage('events')}
      />
    {:else if route.page === 'events'}
      <EventPicker {session} onhome={goHome} />
    {:else if route.page === 'pilots'}
      <PilotsPage {session} onhome={goHome} />
    {:else if route.page === 'classes'}
      <ClassesPage {session} onhome={goHome} />
    {/if}
  {:else if session.currentEvent && live}
    {#if route.kind === 'run'}
      <RunPage
        event={session.currentEvent}
        {live}
        heat={route.heat}
        onhome={() => leaveEventTo('home')}
        onevents={() => leaveEventTo('events')}
        onevent={() => navigate({ kind: 'event' })}
        onrun={(heat) => navigate({ kind: 'run', heat })}
      />
    {:else}
      <EventPage
        event={session.currentEvent}
        {live}
        onhome={() => leaveEventTo('home')}
        onevents={() => leaveEventTo('events')}
        onrun={(heat) => navigate({ kind: 'run', heat })}
      />
    {/if}
  {:else}
    <!-- Edge case: an event route with no open event (e.g. the event was just deleted).
         `onHashChange`/`reconcileRoute` flips the route to the Events page, so this is a momentary
         fallback — show the spinner, never an empty page. -->
    <div class="resume-loading" role="status">
      <span class="resume-spinner" aria-hidden="true"></span>
      <span>Resuming…</span>
    </div>
  {/if}
</div>

<!-- The lazy token prompt lives above any screen (the only token surface). -->
<TokenDialog bind:this={tokenDialog} />

<ToastHost />

<style>
  /* The active-event resume loading state (#90). */
  .resume-loading {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: var(--gf-space-3);
    min-height: 100vh;
    color: var(--gf-text-muted);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
  }
  .resume-spinner {
    width: 1.1rem;
    height: 1.1rem;
    border-radius: 50%;
    border: 2px solid var(--gf-border);
    border-top-color: var(--gf-accent);
    animation: resume-spin 0.7s linear infinite;
  }
  @keyframes resume-spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .resume-spinner {
      animation-duration: 2s;
    }
  }
</style>
