<script lang="ts">
  /**
   * The RD console shell.
   *
   * One row of pages ({@link AppNav}) over the page that is showing. Some pages belong to
   * the open event (Event, Phases, Run, Marshal, Results, Overlays) and some to the whole
   * Director (Seats and Timers, Pilots and Classes, Formats, Help, and the Events page
   * where an event is opened). `lib/route.ts` has the URL of each.
   *
   * An **overlay** route (`#/overlay/‹name›`) is the exception to all of it: a page for an OBS
   * browser source, drawn bare on a see-through page. It gets no nav, no token dialog and no
   * toasts, it does not open the event in this session, and nothing that makes sound is ever
   * mounted for it (the start tone and the lap callouts live in {@link RunPage} alone).
   *
   * On load the shell resolves the Director's active event (server state) so an event URL
   * can render. The hash is authoritative for *which* page to show; an event page with no
   * event open lands on the Events page.
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
  import AppNav from './AppNav.svelte';
  import { Session } from './lib/session.svelte.js';
  import { EventLive } from './lib/eventLive.svelte.js';
  import { marshalCount } from './lib/marshal.js';
  import CapturePage from './screens/CapturePage.svelte';
  import ClassesPage from './screens/ClassesPage.svelte';
  import EventPage from './screens/EventPage.svelte';
  import EventPicker from './screens/EventPicker.svelte';
  import FormatsPage from './screens/FormatsPage.svelte';
  import HelpPage from './screens/HelpPage.svelte';
  import MarshalPage from './screens/MarshalPage.svelte';
  import OverlaysPage from './screens/OverlaysPage.svelte';
  import PhasesPage from './screens/PhasesPage.svelte';
  import PilotsPage from './screens/PilotsPage.svelte';
  import RaceOverlay from './screens/RaceOverlay.svelte';
  import ResultsPage from './screens/ResultsPage.svelte';
  import RunPage from './screens/RunPage.svelte';
  import SeatsPage from './screens/SeatsPage.svelte';
  import TimersPage from './screens/TimersPage.svelte';
  import TokenDialog from './screens/TokenDialog.svelte';
  import {
    parseHash,
    formatHash,
    reconcileRoute,
    resolveInitialRoute,
    inEvent,
    isOverlay,
    type Route
  } from './lib/route.js';

  const session = new Session();

  // ── Hash-based URL routing ─────────────────────────────────────────────────
  // The view is reflected in `location.hash` so a refresh stays on the same page and
  // back/forward + bookmarks work. The hash is the source of truth: navigating sets it, and the
  // reactive `route` is parsed from it on load and on every `hashchange`/`popstate`. We use a hash
  // (not a path) because `/pilots`, `/events`, `/classes` are Director API routes — the `#`
  // stays client-side and can't collide. See `lib/route.ts` for the scheme + reconciliation rules.
  //
  // `route` is the *intended* view. It's reconciled against the live active event (server state)
  // for rendering: an event route with no active event reconciles to the Events page.
  let route = $state<Route>(parseHash(location.hash));

  // Navigate to a route: this is the single mutation point. It updates `location.hash`, which (for
  // a real change) fires `hashchange` and reloads `route` from the hash — keeping hash and view in
  // lockstep. We also set `route` directly so a no-op hash write (same hash) still re-renders.
  function navigate(next: Route) {
    const hash = formatHash(next);
    route = next;
    if (location.hash !== hash) location.hash = hash;
  }

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
  // An overlay skips this: it follows the active event by itself and needs no route resolved.
  // (It runs on the first route that is not an overlay, should the hash ever be edited to one.)
  let resumed = false;
  const bare = $derived(isOverlay(route));
  $effect(() => {
    if (resumed || bare) return;
    resumed = true;
    void (async () => {
      await session.resolveActiveEvent();
      navigate(resolveInitialRoute(location.hash, !!session.currentEvent));
    })();
  });

  // The live competition of the open event: one store, shared by every page, so moving
  // between them does not read everything again. It lives for as long as an event is open,
  // on whatever page: the nav shows how many races need a marshal from anywhere.
  let live = $state.raw<EventLive | undefined>(undefined);
  const liveEventId = $derived(bare ? undefined : session.currentEvent?.id);
  const toMarshal = $derived(live?.view ? marshalCount(live.view) : 0);
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

  // An overlay is laid over video, so the page under it must be see-through. `app.css` lifts the
  // dark canvas while this flag is on <html>.
  $effect(() => {
    if (!bare) return;
    document.documentElement.dataset.overlay = '';
    return () => {
      delete document.documentElement.dataset.overlay;
    };
  });
</script>

<svelte:window onhashchange={onHashChange} onpopstate={onHashChange} />

{#if route.kind === 'overlay'}
  <!-- An OBS browser source: bare, on a see-through page. A name the console does not have
       draws nothing at all. Not dense: an overlay sets its own sizes for a 1920x1080 frame. -->
  <div class="gridfpv-root">
    {#if route.overlay === 'race'}
      <RaceOverlay {session} theme={route.theme ?? 'dark'} />
    {/if}
  </div>
{:else}
  <div class="gridfpv-root gridfpv-dense">
    {@render shell()}
  </div>

  <!-- The lazy token prompt lives above any screen (the only token surface). -->
  <TokenDialog bind:this={tokenDialog} />

  <ToastHost />
{/if}

{#snippet shell()}
  <AppNav {route} eventName={session.currentEvent?.name} {toMarshal} />
  {#if session.resolvingActiveEvent && !session.currentEvent}
    <!-- Resolving the Director's active event: a brief loading state before we know whether
         an event page can render. -->
    <div class="resume-loading" role="status">
      <span class="resume-spinner" aria-hidden="true"></span>
      <span>Resuming…</span>
    </div>
  {:else if route.kind === 'page'}
    {#if route.page === 'events'}
      <!-- Arriving here with an event already open stays here: this is where another one is
           opened. Opening or creating one goes to its page. -->
      <EventPicker {session} onopened={() => navigate({ kind: 'event', section: 'event' })} />
    {:else if route.page === 'pilots'}
      <PilotsPage {session} />
    {:else if route.page === 'classes'}
      <ClassesPage {session} />
    {:else if route.page === 'formats'}
      <FormatsPage {session} />
    {:else if route.page === 'timers'}
      <TimersPage {session} ontune={(timer) => navigate({ kind: 'seats', timer })} />
    {/if}
  {:else if route.kind === 'seats'}
    <SeatsPage
      {session}
      timer={route.timer}
      onpick={(timer) => navigate({ kind: 'seats', timer })}
    />
  {:else if route.kind === 'capture'}
    <!-- Keyed on the seat, so another seat starts with a fresh recording. -->
    {#key `${route.timer}\u0000${route.seat}`}
      <CapturePage {session} timer={route.timer} seat={route.seat} />
    {/key}
  {:else if route.kind === 'help'}
    <HelpPage slug={route.slug} />
  {:else if session.currentEvent && live && inEvent(route)}
    {#if route.kind === 'run'}
      <RunPage
        {live}
        heat={route.heat}
        onrun={(heat) => navigate({ kind: 'run', heat })}
        ontune={(timer, seat) => navigate({ kind: 'capture', timer, seat })}
      />
    {:else if route.kind === 'marshal'}
      <MarshalPage
        {live}
        race={route.race}
        onopen={(race) => navigate({ kind: 'marshal', race })}
        onrun={(heat) => navigate({ kind: 'run', heat })}
      />
    {:else if route.kind === 'event' && route.section === 'phases'}
      <PhasesPage {live} onrun={(heat) => navigate({ kind: 'run', heat })} />
    {:else if route.kind === 'event' && route.section === 'results'}
      <ResultsPage {live} />
    {:else if route.kind === 'event' && route.section === 'overlays'}
      <OverlaysPage />
    {:else}
      <EventPage {session} event={session.currentEvent} {live} />
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
{/snippet}

<style>
  /* The active-event resume loading state (#90). */
  .resume-loading {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: var(--gf-space-3);
    min-height: 60vh;
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
