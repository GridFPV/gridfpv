<script lang="ts">
  /**
   * The RD console shell (#51; two-level app IA per #118).
   *
   * The console is now a **home hub of pages** plus the in-event workspace (#118). With no event
   * selected the shell shows an **app-level route**: the {@link HomeHub} (landing), or one of its
   * three pages — {@link PilotsPage}, {@link EventPicker} (the Events page), {@link TimersPage}.
   * Selecting an event from the Events page opens the **event workspace** (the existing in-event
   * screens, scoped to that event); the workspace's sidebar + {@link ContextHeader} are unchanged.
   * The old picker-as-landing and the Timers **modal** are retired — Timers is now its own page.
   *
   * On load the shell resolves the Director's active event (server state, #90) so a workspace URL
   * can render, but the hash is authoritative for *which* view to show (#118): a bare/hub load
   * stays on the hub and never auto-enters an event — the user only enters by navigating in or
   * loading a workspace URL.
   *
   * The RD token is handled **lazily**: reads/browse need none, and a privileged action prompts for
   * it via {@link TokenDialog} (registered as the session's token provider). A settings/gear lets
   * the RD set or clear the token up front.
   *
   * The console is "just a co-located web client over the same protocol" (clients.html §1):
   * reads come through `@gridfpv/protocol-client`, writes through the control client — both held
   * by the shared `Session`. Screens compose the shared `@gridfpv/components`; this shell only
   * routes between them and owns the session.
   */
  import '@gridfpv/components/tokens.css';
  import { ToastHost, Dialog, Button, Field, Input, toast } from '@gridfpv/components';
  import { Session } from './lib/session.svelte.js';
  import ContextHeader from './ContextHeader.svelte';
  import Breadcrumbs from './Breadcrumbs.svelte';
  import Brand from './Brand.svelte';
  import HomeHub from './screens/HomeHub.svelte';
  import EventPicker from './screens/EventPicker.svelte';
  import TimersPage from './screens/TimersPage.svelte';
  import PilotsPage from './screens/PilotsPage.svelte';
  import ClassesPage from './screens/ClassesPage.svelte';
  import TokenDialog from './screens/TokenDialog.svelte';
  import EventSetupWizard from './screens/EventSetupWizard.svelte';
  import EventTimers from './screens/EventTimers.svelte';
  import EventClassesRoster from './screens/EventClassesRoster.svelte';
  import EventRounds from './screens/EventRounds.svelte';
  import LiveRaceControl from './screens/LiveRaceControl.svelte';
  import Marshaling from './screens/Marshaling.svelte';
  import Results from './screens/Results.svelte';
  import EventAudit from './screens/EventAudit.svelte';
  import { openAudit } from './lib/auditFilter.svelte.js';
  import {
    parseHash,
    formatHash,
    reconcileRoute,
    resolveInitialRoute,
    type Route,
    type AppPage,
    type WorkspaceTab
  } from './lib/route.js';

  const session = new Session();

  // Role seam (#55/#80): a `?role=readonly` query selects the read-only-pilot tier, which hides
  // every mutating control (the Marshaling actions, etc.). The Director is the enforced boundary;
  // this only reflects it client-side. The full read-only-pilot wiring (deriving the role from the
  // connection tier) is a later slice — this query param is the first-class hook the UI gates on.
  if (
    typeof location !== 'undefined' &&
    new URLSearchParams(location.search).get('role') === 'readonly'
  ) {
    session.setRole('readonly');
  }

  // ── Hash-based URL routing (#118) ──────────────────────────────────────────
  // The view is reflected in `location.hash` so a refresh stays on the same page/tab and
  // back/forward + bookmarks work. The hash is the source of truth: navigating sets it, and the
  // reactive `route` is parsed from it on load and on every `hashchange`/`popstate`. We use a hash
  // (not a path) because `/pilots`, `/events`, `/timers` are already Director API routes — the `#`
  // stays client-side and can't collide. See `lib/route.ts` for the scheme + reconciliation rules.
  //
  // `route` is the *intended* view. It's reconciled against the live active event (server state)
  // for rendering: a workspace route with no active event reconciles to the Events page. The hash
  // is authoritative — an empty/hub hash stays on the hub even when an event is active (it no
  // longer auto-resumes into the workspace), all in `lib/route.ts`.
  let route = $state<Route>(parseHash(location.hash));

  // Navigate to a route: this is the single mutation point. It updates `location.hash`, which (for
  // a real change) fires `hashchange` and reloads `route` from the hash — keeping hash and view in
  // lockstep. We also set `route` directly so a no-op hash write (same hash) still re-renders.
  function navigate(next: Route) {
    const hash = formatHash(next);
    route = next;
    if (location.hash !== hash) location.hash = hash;
  }

  // Page navigations (hub cards, breadcrumbs, page "Home" crumbs).
  const goPage = (page: AppPage) => navigate({ kind: 'page', page });
  const goHome = () => goPage('home');
  // The workspace's app-route concept is "Events" (where event entry/switch happens).
  const route$page = $derived(route.kind === 'page' ? route.page : 'home');

  // On `hashchange`/`popstate` (back/forward, manual edit, or our own `navigate`) re-derive the
  // route from the hash. Reconcile against the live active event so a workspace hash with no event
  // doesn't render a broken workspace.
  function onHashChange() {
    route = reconcileRoute(parseHash(location.hash), !!session.currentEvent);
  }

  // The lazy token prompt: register a provider that opens the TokenDialog and resolves
  // with the entered token (or undefined if cancelled). This is the only auth surface left.
  let tokenDialog = $state<TokenDialog>();
  session.setTokenProvider(() => tokenDialog?.request() ?? Promise.resolve(undefined));

  // Resolve the *initial* route on load (#118): the active event is server-side state, so we first
  // read `GET /active-event` (it determines whether a workspace hash can render), then resolve the
  // route from the hash + whether an event is active. The hash is authoritative — an `#/event/<tab>`
  // hash restores that tab (or reconciles to Events if no event is active), a top-level page hash
  // shows that page, and an empty/hub hash stays on the hub *even when an event is active* (we no
  // longer auto-resume into the workspace; this intentionally supersedes #90's reload-resume per
  // user feedback). While it resolves we show a brief loading state; after this the hash is the
  // source of truth.
  let resumed = false;
  $effect(() => {
    if (resumed) return;
    resumed = true;
    void (async () => {
      await session.resolveActiveEvent();
      navigate(resolveInitialRoute(location.hash, !!session.currentEvent));
    })();
  });

  // Entering an event happens *inside* the EventPicker via session state (`chooseEvent` /
  // `createEventAndEnter` set `session.currentEvent`), not through a `navigate` call here. So when
  // the active event appears while we're on the Events page, flip the route into the workspace so
  // the hash becomes `#/event/live`. This is the routing counterpart of the old `route = 'events'`
  // → workspace transition that used to be implicit in `{#if session.currentEvent}`.
  $effect(() => {
    if (session.currentEvent && route.kind === 'page' && route.page === 'events') {
      navigate({ kind: 'workspace', tab: 'live' });
    }
  });

  type ScreenId =
    | 'timers'
    | 'classes-roster'
    | 'rounds'
    | 'live'
    | 'marshaling'
    | 'results'
    | 'audit';
  const SCREENS: { id: ScreenId; label: string; key: string; icon: string }[] = [
    {
      id: 'classes-roster',
      label: 'Classes & Roster',
      key: '1',
      icon: 'M16 11V7a4 4 0 1 0-8 0v4M5 11h14v9H5z'
    },
    {
      id: 'rounds',
      label: 'Rounds & Heats',
      key: '2',
      icon: 'M4 5h16M4 12h16M4 19h16M9 5v14'
    },
    { id: 'live', label: 'Race control', key: '3', icon: 'M5 3l14 9-14 9V3z' },
    { id: 'marshaling', label: 'Marshaling', key: '4', icon: 'M9 11l3 3L22 4M21 12v7H3V5h12' },
    { id: 'results', label: 'Results', key: '5', icon: 'M4 19V10M10 19V4M16 19v-7M22 19H2' },
    // The event-wide audit review page (a magnifier: search the ruling history). Sits with the
    // review surfaces (after Results); key '7' — the unused digit — so Timers keeps its Alt+6.
    {
      id: 'audit',
      label: 'Audit',
      key: '7',
      icon: 'M10.5 4a6.5 6.5 0 1 0 0 13 6.5 6.5 0 0 0 0-13zM15.5 15.5L21 21'
    },
    {
      id: 'timers',
      label: 'Timers',
      key: '6',
      icon: 'M12 8v4l2.5 2.5M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z'
    }
  ];
  // The active workspace tab is derived from the route (the hash is the source of truth, #118):
  // a `workspace` route carries the tab; outside the workspace it's the default (`live`). Switching
  // a sidebar tab is `setTab`, which sets the `#/event/<tab>` hash so a refresh restores it.
  const active = $derived<ScreenId>(route.kind === 'workspace' ? route.tab : 'live');
  const activeScreen = $derived(SCREENS.find((s) => s.id === active));
  const setTab = (tab: WorkspaceTab) => navigate({ kind: 'workspace', tab });

  // The setup wizard overlay (race redesign Slice 7): a guided first-pass over the stage-pages,
  // reusing their components/commands (no separate config bag). Optional + re-runnable; launched on
  // creating a new event (deferred via `pendingWizard` until the workspace mounts) and from the
  // workspace header's "Setup wizard" button. Everything it sets stays editable on the pages.
  let wizardOpen = $state(false);
  // When a new event is created from the Events page, defer opening the wizard until the workspace
  // route is in (the embedded stages need the active event), then pop it open once.
  let pendingWizard = $state(false);
  $effect(() => {
    if (pendingWizard && session.currentEvent && route.kind === 'workspace') {
      pendingWizard = false;
      wizardOpen = true;
    }
  });

  function leaveToPicker() {
    session.leaveEvent();
    // Close the wizard if open, and land back on the Events page (the app route the "Switch event"
    // action conceptually returns to). Setting the hash to `#/events` keeps the URL honest.
    wizardOpen = false;
    goPage('events');
  }

  // Leave the workspace straight to the **home hub** (#118) — the brand/logo and the breadcrumb's
  // "Home" crumb both use this. Same teardown as a switch, but lands on the hub, not the picker.
  function goToHubFromWorkspace() {
    session.leaveEvent();
    wizardOpen = false;
    goHome();
  }

  // A keyboard shortcut per screen (Alt+digit), keeping the console keyboard-driven.
  function onKeydown(e: KeyboardEvent) {
    if (!session.currentEvent) return;
    if (e.altKey && !e.ctrlKey && !e.metaKey) {
      const match = SCREENS.find((s) => s.key === e.key);
      if (match) {
        setTab(match.id);
        e.preventDefault();
      }
    }
  }

  // ── Settings (the RD token, set/cleared up front) ──────────────────────────
  let settingsOpen = $state(false);
  let tokenInput = $state('');
  function openSettings() {
    tokenInput = '';
    settingsOpen = true;
  }
  function saveToken() {
    if (!tokenInput.trim()) return;
    session.setToken(tokenInput.trim());
    settingsOpen = false;
    toast.success('Control token saved for this session.');
  }
  function clearToken() {
    session.clearToken();
    tokenInput = '';
    toast.info('Control token cleared.');
  }
</script>

<svelte:window onkeydown={onKeydown} onhashchange={onHashChange} onpopstate={onHashChange} />

{#if session.resolvingActiveEvent && !session.currentEvent}
  <!-- Resolving the Director's active event (#90): a brief loading state before we know
       whether to resume into the workspace or show the picker. -->
  <div class="gridfpv-root gridfpv-dense">
    <div class="resume-loading" role="status">
      <span class="resume-spinner" aria-hidden="true"></span>
      <span>Resuming…</span>
    </div>
  </div>
{:else if route.kind === 'page'}
  <!-- App-level routes (#118): the home hub, or one of its three pages. The view is driven by the
       hash, not just `session.currentEvent` — an explicit page hash (e.g. `#/pilots`) shows that
       page even if an event is active on the Director, so a refresh on a top-level page stays put.
       The page owns its own breadcrumb back to Home. -->
  <div class="gridfpv-root gridfpv-dense">
    {#if route$page === 'home'}
      <HomeHub
        {session}
        onpilots={() => goPage('pilots')}
        onclasses={() => goPage('classes')}
        onevents={() => goPage('events')}
        ontimers={() => goPage('timers')}
      />
    {:else if route$page === 'events'}
      <EventPicker {session} onhome={goHome} onsetup={() => (pendingWizard = true)} />
    {:else if route$page === 'timers'}
      <TimersPage {session} onhome={goHome} />
    {:else if route$page === 'pilots'}
      <PilotsPage {session} onhome={goHome} />
    {:else if route$page === 'classes'}
      <ClassesPage {session} onhome={goHome} />
    {/if}
  </div>
{:else if session.currentEvent}
  <div class="gridfpv-root gridfpv-dense app">
    <aside class="sidebar">
      <!-- The brand is the home root from inside the workspace (#118): it leaves the event and
           returns to the hub, mirroring the breadcrumb's "Home" crumb. Shared with the
           directory pages via Brand.svelte so home looks identical everywhere. -->
      <Brand onclick={goToHubFromWorkspace} />

      <nav aria-label="Screens">
        {#each SCREENS as s (s.id)}
          <button
            type="button"
            class="nav-item"
            class:active={active === s.id}
            aria-current={active === s.id ? 'page' : undefined}
            onclick={() => setTab(s.id)}
          >
            <svg class="nav-icon" viewBox="0 0 24 24" aria-hidden="true">
              <path
                d={s.icon}
                fill="none"
                stroke="currentColor"
                stroke-width="1.8"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </svg>
            <span class="nav-label">{s.label}</span>
            <kbd>{s.key}</kbd>
          </button>
        {/each}
      </nav>
    </aside>

    <div class="main-col">
      <!-- The app-level trail above the in-event context bar (#118): Home › Events › <event>.
           "Home" leaves to the hub; "Events" leaves to the picker; the event name is the leaf. -->
      <div class="crumb-bar">
        <Breadcrumbs
          crumbs={[
            { label: 'Home', onclick: goToHubFromWorkspace },
            { label: 'Events', onclick: leaveToPicker },
            { label: session.currentEvent?.name ?? 'Event' }
          ]}
        />
      </div>

      <header class="topbar">
        <ContextHeader {session} ongolive={() => setTab('live')} onswitchevent={leaveToPicker} />
        <div class="topbar-actions">
          <h1 class="screen-title">{activeScreen?.label}</h1>
          <button
            type="button"
            class="wizard-launch"
            onclick={() => (wizardOpen = true)}
            title="Walk this event's setup — classes, roster, timer, first round"
          >
            <svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true">
              <path
                d="M5 3v4M3 5h4M6 17v4M4 19h4M13 3l2.5 6.5L22 12l-6.5 2.5L13 21l-2.5-6.5L4 12l6.5-2.5L13 3z"
                fill="none"
                stroke="currentColor"
                stroke-width="1.6"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </svg>
            <span>Setup wizard</span>
          </button>
          <button
            type="button"
            class="gear"
            onclick={openSettings}
            aria-label="Settings"
            title={session.hasToken ? 'Settings — token set' : 'Settings — no token'}
          >
            <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true">
              <path
                d="M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6z M19.4 13a7.8 7.8 0 0 0 0-2l2-1.6-2-3.4-2.4 1a7.8 7.8 0 0 0-1.7-1l-.4-2.6h-4l-.4 2.6a7.8 7.8 0 0 0-1.7 1l-2.4-1-2 3.4 2 1.6a7.8 7.8 0 0 0 0 2l-2 1.6 2 3.4 2.4-1a7.8 7.8 0 0 0 1.7 1l.4 2.6h4l.4-2.6a7.8 7.8 0 0 0 1.7-1l2.4 1 2-3.4-2-1.6z"
                fill="none"
                stroke="currentColor"
                stroke-width="1.4"
                stroke-linejoin="round"
              />
            </svg>
            {#if session.hasToken}<span class="gear-dot" aria-hidden="true"></span>{/if}
          </button>
        </div>
      </header>

      <main class="content">
        {#if active === 'timers'}
          <EventTimers {session} />
        {:else if active === 'classes-roster'}
          <EventClassesRoster {session} />
        {:else if active === 'rounds'}
          <EventRounds {session} />
        {:else if active === 'live'}
          <LiveRaceControl {session} />
        {:else if active === 'marshaling'}
          <!-- The audit jumps ride the auditFilter seam: deposit the prefilter, switch the tab;
               EventAudit consumes it on mount. -->
          <Marshaling {session} onviewaudit={(prefilter) => openAudit(setTab, prefilter)} />
        {:else if active === 'results'}
          <Results
            {session}
            onviewaudit={(prefilter) => openAudit(setTab, prefilter)}
            heatResult={session.heatResult ??
              (session.protocolState?.body && 'HeatResult' in session.protocolState.body
                ? session.protocolState.body.HeatResult
                : undefined)}
            standings={session.protocolState?.body && 'Ranking' in session.protocolState.body
              ? session.protocolState.body.Ranking
              : undefined}
          />
        {:else if active === 'audit'}
          <EventAudit {session} />
        {/if}
      </main>
    </div>

    <!-- The guided setup wizard overlay (race redesign Slice 7): a first-pass over the stage-pages,
         reusing their components/commands. Lives above the workspace so it can embed the stages. -->
    <EventSetupWizard {session} bind:open={wizardOpen} />
  </div>
{:else}
  <!-- Edge case: a workspace route with no active event (e.g. a hand-edited `#/event/*` hash with
       nothing active). `onHashChange`/`reconcileRoute` flips the route to the Events page on the
       same tick, so this is a momentary fallback — show the resume spinner, never a broken shell. -->
  <div class="gridfpv-root gridfpv-dense">
    <div class="resume-loading" role="status">
      <span class="resume-spinner" aria-hidden="true"></span>
      <span>Resuming…</span>
    </div>
  </div>
{/if}

<!-- The lazy token prompt + the up-front settings menu live above any screen. -->
<TokenDialog bind:this={tokenDialog} />

<Dialog bind:open={settingsOpen} title="Settings">
  <div class="settings">
    <Field label="Control token" hint="Held for this session only — never written to disk.">
      <Input
        type="password"
        bind:value={tokenInput}
        placeholder={session.hasToken ? '•••••••• (set)' : 'bearer token'}
        aria-label="Control token"
        autocomplete="off"
      />
    </Field>
    <p class="settings-note">
      A token is only needed for privileged actions (creating an event, running a heat, registering,
      marshaling). You'll be asked automatically when one is required.
    </p>
  </div>
  {#snippet footer()}
    {#if session.hasToken}
      <Button variant="ghost" onclick={clearToken}>Clear token</Button>
    {/if}
    <Button variant="primary" onclick={saveToken} disabled={!tokenInput.trim()}>Save token</Button>
  {/snippet}
</Dialog>

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

  .app {
    display: grid;
    grid-template-columns: 15rem 1fr;
    min-height: 100vh;
    color: var(--gf-text);
    font-family: var(--gf-font-family);
  }

  /* ── Sidebar ─────────────────────────────────────────────────────────────── */
  .sidebar {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-6);
    padding: var(--gf-space-5) var(--gf-space-4);
    background: var(--gf-surface);
    border-right: 1px solid var(--gf-border-subtle);
    position: sticky;
    top: 0;
    height: 100vh;
  }
  nav {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .nav-item {
    position: relative;
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    padding: var(--gf-space-2) var(--gf-space-3);
    border: none;
    border-radius: var(--gf-radius-sm);
    background: transparent;
    color: var(--gf-text-muted);
    font-family: inherit;
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-medium);
    cursor: pointer;
    text-align: left;
    transition:
      background var(--gf-motion-fast) var(--gf-ease-out),
      color var(--gf-motion-fast) var(--gf-ease-out);
  }
  .nav-icon {
    width: 1.1rem;
    height: 1.1rem;
    flex-shrink: 0;
    opacity: 0.85;
  }
  .nav-label {
    flex: 1;
  }
  .nav-item:hover {
    background: var(--gf-elevated);
    color: var(--gf-text);
  }
  .nav-item.active {
    background: var(--gf-accent-soft);
    color: var(--gf-accent);
    font-weight: var(--gf-font-weight-semibold);
  }
  .nav-item.active::before {
    content: '';
    position: absolute;
    left: -4px;
    top: 50%;
    transform: translateY(-50%);
    width: 3px;
    height: 1.1rem;
    border-radius: var(--gf-radius-pill);
    background: var(--gf-accent);
  }
  .nav-item.active .nav-icon {
    opacity: 1;
  }
  kbd {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 1.3rem;
    height: 1.3rem;
    padding: 0 0.3rem;
    border-radius: var(--gf-radius-xs);
    border: 1px solid var(--gf-border);
    background: var(--gf-surface-sunken);
    font-size: var(--gf-font-size-2xs);
    color: var(--gf-text-faint);
  }

  /* ── Main column (topbar + content) ──────────────────────────────────────── */
  .main-col {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 100vh;
  }
  .crumb-bar {
    padding: var(--gf-space-3) var(--gf-space-8) 0;
  }
  .topbar {
    position: sticky;
    top: 0;
    z-index: 10;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-4);
    padding: var(--gf-space-4) var(--gf-space-8);
    background: color-mix(in srgb, var(--gf-surface) 82%, transparent);
    backdrop-filter: blur(10px);
    border-bottom: 1px solid var(--gf-border-subtle);
  }
  /* The context bar fills the topbar; the screen title + gear sit to its right. */
  .topbar :global(.ctx-bar) {
    flex: 1;
    min-width: 0;
  }
  .screen-title {
    margin: 0;
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-caps);
    text-transform: uppercase;
    color: var(--gf-text-muted);
    white-space: nowrap;
  }
  .topbar-actions {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    flex-shrink: 0;
  }
  .wizard-launch {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    height: 2rem;
    padding: 0 var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: transparent;
    color: var(--gf-text-secondary);
    font-family: inherit;
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-medium);
    cursor: pointer;
    white-space: nowrap;
    transition:
      border-color var(--gf-motion-fast) var(--gf-ease-out),
      color var(--gf-motion-fast) var(--gf-ease-out);
  }
  .wizard-launch:hover {
    border-color: var(--gf-accent);
    color: var(--gf-accent);
  }
  .wizard-launch:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
  @media (max-width: 48rem) {
    .wizard-launch span {
      display: none;
    }
  }

  .gear {
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 2rem;
    height: 2rem;
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: transparent;
    color: var(--gf-text-muted);
    cursor: pointer;
    transition:
      border-color var(--gf-motion-fast) var(--gf-ease-out),
      color var(--gf-motion-fast) var(--gf-ease-out);
  }
  .gear:hover {
    border-color: var(--gf-border-strong);
    color: var(--gf-text);
  }
  .gear-dot {
    position: absolute;
    top: 3px;
    right: 3px;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--gf-accent);
  }

  .content {
    flex: 1;
    padding: var(--gf-space-8);
    overflow: auto;
  }

  .settings {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .settings-note {
    margin: 0;
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-muted);
  }

  @media (max-width: 60rem) {
    .app {
      grid-template-columns: 4rem 1fr;
    }
    .nav-label,
    kbd {
      display: none;
    }
    .nav-item {
      justify-content: center;
    }
  }
</style>
