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
   * Active-event resume (#90) is preserved: on load the session resolves the Director's active
   * event and, if one is set, enters its workspace directly; otherwise the shell lands on the hub.
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
  import { emptyConfig, type EventConfig } from './lib/setup.js';
  import HomeHub from './screens/HomeHub.svelte';
  import EventPicker from './screens/EventPicker.svelte';
  import TimersPage from './screens/TimersPage.svelte';
  import PilotsPage from './screens/PilotsPage.svelte';
  import TokenDialog from './screens/TokenDialog.svelte';
  import SetupWizard from './screens/SetupWizard.svelte';
  import EventTimers from './screens/EventTimers.svelte';
  import Registration from './screens/Registration.svelte';
  import LiveRaceControl from './screens/LiveRaceControl.svelte';
  import Marshaling from './screens/Marshaling.svelte';
  import Results from './screens/Results.svelte';

  const session = new Session();

  // ── App-level routing (#118) ───────────────────────────────────────────────
  // The two-level IA: with no event selected the shell is on one of the app-level routes — the hub
  // or one of its three pages. Entering an event switches to the workspace; leaving the workspace
  // returns to the Events page (where the switch happened). `goHome` is the brand/breadcrumb root.
  type AppRoute = 'home' | 'events' | 'timers' | 'pilots';
  let route = $state<AppRoute>('home');
  const goHome = () => (route = 'home');

  // The lazy token prompt: register a provider that opens the TokenDialog and resolves
  // with the entered token (or undefined if cancelled). This is the only auth surface left.
  let tokenDialog = $state<TokenDialog>();
  session.setTokenProvider(() => tokenDialog?.request() ?? Promise.resolve(undefined));

  // Resume into the Director's active event on load (#90): the active event is server-side
  // state, so a reload/reconnect/app-restart reads `GET /active-event` and re-enters the same
  // event instead of dropping to the picker. While this resolves we show a brief loading state;
  // it then settles into the workspace (active set) or the picker (none / unreachable).
  $effect(() => {
    void session.resolveActiveEvent();
  });

  type ScreenId = 'setup' | 'timers' | 'registration' | 'live' | 'marshaling' | 'results';
  const SCREENS: { id: ScreenId; label: string; key: string; icon: string }[] = [
    { id: 'setup', label: 'Setup', key: '1', icon: 'M4 6h16M4 12h16M4 18h10' },
    {
      id: 'registration',
      label: 'Registration',
      key: '2',
      icon: 'M16 11V7a4 4 0 1 0-8 0v4M5 11h14v9H5z'
    },
    { id: 'live', label: 'Live control', key: '3', icon: 'M5 3l14 9-14 9V3z' },
    { id: 'marshaling', label: 'Marshaling', key: '4', icon: 'M9 11l3 3L22 4M21 12v7H3V5h12' },
    { id: 'results', label: 'Results', key: '5', icon: 'M4 19V10M10 19V4M16 19v-7M22 19H2' },
    {
      id: 'timers',
      label: 'Timers',
      key: '6',
      icon: 'M12 8v4l2.5 2.5M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z'
    }
  ];
  let active = $state<ScreenId>('live');
  const activeScreen = $derived(SCREENS.find((s) => s.id === active));

  // The setup wizard's config lives at the shell so it survives screen switches.
  let config = $state<EventConfig>(emptyConfig());

  function onSetupCommit() {
    // The console is already inside an event (#72) — the live read client was scoped to it on
    // entry — so committing the wizard just advances to registration; there is no separate
    // event to re-scope to (the redundant event field was removed, #72 Slice 1b A1).
    active = 'registration';
  }

  function leaveToPicker() {
    session.leaveEvent();
    // Reset the workspace's local view so re-entering starts clean, and land back on the Events
    // page (the app route the "Switch event" action conceptually returns to).
    active = 'live';
    config = emptyConfig();
    route = 'events';
  }

  // Leave the workspace straight to the **home hub** (#118) — the brand/logo and the breadcrumb's
  // "Home" crumb both use this. Same teardown as a switch, but lands on the hub, not the picker.
  function goToHubFromWorkspace() {
    session.leaveEvent();
    active = 'live';
    config = emptyConfig();
    route = 'home';
  }

  // A keyboard shortcut per screen (Alt+digit), keeping the console keyboard-driven.
  function onKeydown(e: KeyboardEvent) {
    if (!session.currentEvent) return;
    if (e.altKey && !e.ctrlKey && !e.metaKey) {
      const match = SCREENS.find((s) => s.key === e.key);
      if (match) {
        active = match.id;
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

<svelte:window onkeydown={onKeydown} />

{#if session.resolvingActiveEvent && !session.currentEvent}
  <!-- Resolving the Director's active event (#90): a brief loading state before we know
       whether to resume into the workspace or show the picker. -->
  <div class="gridfpv-root gridfpv-dense">
    <div class="resume-loading" role="status">
      <span class="resume-spinner" aria-hidden="true"></span>
      <span>Resuming…</span>
    </div>
  </div>
{:else if !session.currentEvent}
  <!-- App-level routes (#118): the home hub, or one of its three pages. No event is selected,
       so the in-event workspace is not shown; the page owns its own breadcrumb back to Home. -->
  <div class="gridfpv-root gridfpv-dense">
    {#if route === 'home'}
      <HomeHub
        {session}
        onpilots={() => (route = 'pilots')}
        onevents={() => (route = 'events')}
        ontimers={() => (route = 'timers')}
      />
    {:else if route === 'events'}
      <EventPicker {session} onhome={goHome} />
    {:else if route === 'timers'}
      <TimersPage {session} onhome={goHome} />
    {:else if route === 'pilots'}
      <PilotsPage {session} onhome={goHome} />
    {/if}
  </div>
{:else}
  <div class="gridfpv-root gridfpv-dense app">
    <aside class="sidebar">
      <!-- The brand is the home root from inside the workspace (#118): it leaves the event and
           returns to the hub, mirroring the breadcrumb's "Home" crumb. -->
      <button type="button" class="brand" onclick={goToHubFromWorkspace} title="Home — GridFPV hub">
        <span class="logo" aria-hidden="true">
          <svg viewBox="0 0 32 32" width="28" height="28">
            <rect x="2" y="2" width="28" height="28" rx="8" fill="var(--gf-accent-soft)" />
            <path
              d="M16 6 L25 11 L25 21 L16 26 L7 21 L7 11 Z"
              fill="none"
              stroke="var(--gf-accent)"
              stroke-width="2"
              stroke-linejoin="round"
            />
            <circle cx="16" cy="16" r="3" fill="var(--gf-accent)" />
          </svg>
        </span>
        <span class="wordmark">
          GridFPV
          <span class="sub">RD Console</span>
        </span>
      </button>

      <nav aria-label="Screens">
        {#each SCREENS as s (s.id)}
          <button
            type="button"
            class="nav-item"
            class:active={active === s.id}
            aria-current={active === s.id ? 'page' : undefined}
            onclick={() => (active = s.id)}
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

      <div class="sidebar-foot">
        <code class="base">{session.baseUrl}</code>
      </div>
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
        <ContextHeader {session} ongolive={() => (active = 'live')} onswitchevent={leaveToPicker} />
        <div class="topbar-actions">
          <h1 class="screen-title">{activeScreen?.label}</h1>
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
        {#if active === 'setup'}
          <SetupWizard
            bind:config
            eventName={session.currentEvent?.name ?? ''}
            oncommit={onSetupCommit}
          />
        {:else if active === 'timers'}
          <EventTimers {session} />
        {:else if active === 'registration'}
          <Registration {session} />
        {:else if active === 'live'}
          <LiveRaceControl {session} />
        {:else if active === 'marshaling'}
          <Marshaling {session} />
        {:else if active === 'results'}
          <Results
            heatResult={session.heatResult ??
              (session.protocolState?.body && 'HeatResult' in session.protocolState.body
                ? session.protocolState.body.HeatResult
                : undefined)}
            standings={session.protocolState?.body && 'Ranking' in session.protocolState.body
              ? session.protocolState.body.Ranking
              : undefined}
            outcome={session.protocolState?.body && 'EventOutcome' in session.protocolState.body
              ? session.protocolState.body.EventOutcome
              : undefined}
          />
        {/if}
      </main>
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
  .brand {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    padding: var(--gf-space-2);
    margin: calc(-1 * var(--gf-space-2));
    border: none;
    background: transparent;
    color: inherit;
    font-family: inherit;
    text-align: left;
    cursor: pointer;
    border-radius: var(--gf-radius-sm);
    transition: background var(--gf-motion-fast) var(--gf-ease-out);
  }
  .brand:hover {
    background: var(--gf-elevated);
  }
  .brand:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
  .logo {
    display: inline-flex;
    flex-shrink: 0;
  }
  .wordmark {
    display: flex;
    flex-direction: column;
    line-height: 1.1;
    font-weight: var(--gf-font-weight-bold);
    font-size: var(--gf-font-size-md);
    letter-spacing: var(--gf-tracking-tight);
  }
  .wordmark .sub {
    font-size: var(--gf-font-size-2xs);
    font-weight: var(--gf-font-weight-medium);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
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

  .sidebar-foot {
    margin-top: auto;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
    padding: var(--gf-space-3) var(--gf-space-2) 0;
    border-top: 1px solid var(--gf-border-subtle);
  }
  .base {
    font-size: var(--gf-font-size-2xs);
    color: var(--gf-text-faint);
    word-break: break-all;
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
    .wordmark,
    .nav-label,
    kbd,
    .base {
      display: none;
    }
    .brand {
      justify-content: center;
    }
    .nav-item {
      justify-content: center;
    }
  }
</style>
