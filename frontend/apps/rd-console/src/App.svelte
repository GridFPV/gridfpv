<script lang="ts">
  /**
   * The RD console shell (#51, restyled by the #71 design foundation) — the app
   * frame: bearer-token login gate, a keyboard-friendly sidebar across the five
   * screens, a topbar carrying the event/heat context + the live connection pill,
   * and the global toast host. Everything wraps the `.gridfpv-root .gridfpv-dense`
   * token context (clients.html §1: the console is the dense, control-oriented
   * surface) so the whole frame themes off the shared design tokens.
   *
   * The console is "just a co-located web client over the same protocol" (clients.html
   * §1): reads come through `@gridfpv/protocol-client`, writes through the control
   * client — both held by the shared `Session`. Screens compose the shared
   * `@gridfpv/components`; this shell only routes between them and owns the session.
   */
  import '@gridfpv/components/tokens.css';
  import { StatusPill, ToastHost } from '@gridfpv/components';
  import { Session } from './lib/session.svelte.js';
  import { emptyConfig, type EventConfig } from './lib/setup.js';
  import Login from './screens/Login.svelte';
  import SetupWizard from './screens/SetupWizard.svelte';
  import Registration from './screens/Registration.svelte';
  import LiveRaceControl from './screens/LiveRaceControl.svelte';
  import Marshaling from './screens/Marshaling.svelte';
  import Results from './screens/Results.svelte';

  const session = new Session();

  type ScreenId = 'setup' | 'registration' | 'live' | 'marshaling' | 'results';
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
    { id: 'results', label: 'Results', key: '5', icon: 'M4 19V10M10 19V4M16 19v-7M22 19H2' }
  ];
  let active = $state<ScreenId>('live');
  const activeScreen = $derived(SCREENS.find((s) => s.id === active));

  // The setup wizard's config lives at the shell so it survives screen switches.
  let config = $state<EventConfig>(emptyConfig());

  function onSetupCommit(committed: EventConfig) {
    // Re-scope the live read client to the configured event once it's known.
    if (committed.eventId.trim()) {
      session.resubscribe({ Event: { event: committed.eventId.trim() } });
    }
    active = 'registration';
  }

  // A keyboard shortcut per screen (Alt+digit), keeping the console keyboard-driven.
  function onKeydown(e: KeyboardEvent) {
    if (!session.authenticated) return;
    if (e.altKey && !e.ctrlKey && !e.metaKey) {
      const match = SCREENS.find((s) => s.key === e.key);
      if (match) {
        active = match.id;
        e.preventDefault();
      }
    }
  }

  // Note: the wizard's config is held locally (no event/class/track setup command on
  // the contract yet — see lib/setup.ts); the one wire-driving setup action available
  // today is `scheduleHeatCommand` (ScheduleHeat), emitted once a lineup is known.

  const eventLabel = $derived(config.eventName?.trim() || 'No event configured');
  const liveHeat = $derived(session.liveState?.current_heat);
</script>

<svelte:window onkeydown={onKeydown} />

{#if !session.authenticated}
  <div class="gridfpv-root gridfpv-dense">
    <Login {session} />
  </div>
{:else}
  <div class="gridfpv-root gridfpv-dense app">
    <aside class="sidebar">
      <div class="brand">
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
      </div>

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
        <div class="conn" title={`Read stream: ${session.connectionStatus}`}>
          <StatusPill status={session.connectionStatus} size="sm" />
          <span class="conn-label">{session.connectionStatus}</span>
        </div>
        <code class="base">{session.baseUrl}</code>
        <button type="button" class="signout" onclick={() => session.logout()}>Sign out</button>
      </div>
    </aside>

    <div class="main-col">
      <header class="topbar">
        <div class="crumbs">
          <span class="event" title={eventLabel}>{eventLabel}</span>
          <span class="sep" aria-hidden="true">/</span>
          <h1 class="screen-title">{activeScreen?.label}</h1>
          {#if liveHeat}
            <span class="heat-chip">Heat {liveHeat}</span>
          {/if}
        </div>
        <div class="topbar-status">
          <StatusPill status={session.connectionStatus} />
        </div>
      </header>

      <main class="content">
        {#if active === 'setup'}
          <SetupWizard bind:config oncommit={onSetupCommit} />
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
  <ToastHost />
{/if}

<style>
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
    padding: 0 var(--gf-space-2);
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
  .conn {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
  }
  /* Kept as an explicit text hook for the e2e (`.conn-label` === status text),
   * but visually folded into the StatusPill above; hidden off-screen. */
  .conn-label {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }
  .base {
    font-size: var(--gf-font-size-2xs);
    color: var(--gf-text-faint);
    word-break: break-all;
  }
  .signout {
    align-self: flex-start;
    background: transparent;
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    padding: var(--gf-space-1) var(--gf-space-3);
    color: var(--gf-text-secondary);
    font-family: inherit;
    font-size: var(--gf-font-size-xs);
    cursor: pointer;
    transition:
      border-color var(--gf-motion-fast) var(--gf-ease-out),
      color var(--gf-motion-fast) var(--gf-ease-out);
  }
  .signout:hover {
    border-color: var(--gf-border-strong);
    color: var(--gf-text);
  }

  /* ── Main column (topbar + content) ──────────────────────────────────────── */
  .main-col {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 100vh;
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
  .crumbs {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    min-width: 0;
  }
  .event {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
    max-width: 16rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sep {
    color: var(--gf-text-faint);
  }
  .screen-title {
    margin: 0;
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
  }
  .heat-chip {
    padding: 0.15em 0.6em;
    border-radius: var(--gf-radius-pill);
    background: var(--gf-accent-soft);
    color: var(--gf-accent);
    font-size: var(--gf-font-size-xs);
    font-weight: var(--gf-font-weight-semibold);
    font-variant-numeric: tabular-nums;
  }
  .topbar-status {
    flex-shrink: 0;
  }

  .content {
    flex: 1;
    padding: var(--gf-space-8);
    overflow: auto;
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
    .event {
      display: none;
    }
  }
</style>
