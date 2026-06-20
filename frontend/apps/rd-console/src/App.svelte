<script lang="ts">
  /**
   * The RD console shell (#51) — the app frame: bearer-token login gate, a
   * keyboard-friendly sidebar across the five screens, and a live connection-status
   * readout. Everything below wraps the `.gridfpv-dense` token context (clients.html
   * §1: the console is the dense, control-oriented surface).
   *
   * The console is "just a co-located web client over the same protocol" (clients.html
   * §1): reads come through `@gridfpv/protocol-client`, writes through the control
   * client — both held by the shared `Session`. Screens compose the shared
   * `@gridfpv/components`; this shell only routes between them and owns the session.
   */
  import '@gridfpv/components/tokens.css';
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
  const SCREENS: { id: ScreenId; label: string; key: string }[] = [
    { id: 'setup', label: 'Setup', key: '1' },
    { id: 'registration', label: 'Registration', key: '2' },
    { id: 'live', label: 'Live control', key: '3' },
    { id: 'marshaling', label: 'Marshaling', key: '4' },
    { id: 'results', label: 'Results', key: '5' }
  ];
  let active = $state<ScreenId>('live');

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

  function statusTone(status: string): string {
    if (status === 'live') return 'ok';
    if (status === 'reconnecting' || status === 'closed') return 'warn';
    if (status === 'idle') return 'idle';
    return 'pending';
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if !session.authenticated}
  <div class="gridfpv-root gridfpv-dense">
    <Login {session} />
  </div>
{:else}
  <div class="gridfpv-root gridfpv-dense app">
    <aside class="sidebar">
      <div class="brand">GridFPV<span class="sub">RD console</span></div>
      <nav aria-label="Screens">
        {#each SCREENS as s (s.id)}
          <button
            type="button"
            class="nav-item"
            class:active={active === s.id}
            aria-current={active === s.id ? 'page' : undefined}
            onclick={() => (active = s.id)}
          >
            <span>{s.label}</span>
            <kbd>Alt+{s.key}</kbd>
          </button>
        {/each}
      </nav>
      <div class="conn">
        <span class="dot" data-tone={statusTone(session.connectionStatus)}></span>
        <span class="conn-label">{session.connectionStatus}</span>
      </div>
      <div class="account">
        <code class="base">{session.baseUrl}</code>
        <button type="button" class="signout" onclick={() => session.logout()}>Sign out</button>
      </div>
    </aside>

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
{/if}

<style>
  .app {
    display: grid;
    grid-template-columns: 14rem 1fr;
    min-height: 100vh;
    background: var(--gf-color-surface-alt);
    color: var(--gf-color-text);
    font-family: var(--gf-font-family);
  }
  .sidebar {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
    padding: var(--gf-space-4);
    background: var(--gf-color-surface);
    border-right: 1px solid var(--gf-color-border);
  }
  .brand {
    font-weight: var(--gf-font-weight-bold);
    font-size: var(--gf-font-size-md);
    display: flex;
    flex-direction: column;
  }
  .brand .sub {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-color-text-muted);
    font-weight: var(--gf-font-weight-normal);
  }
  nav {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
  }
  .nav-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid transparent;
    border-radius: var(--gf-radius-sm);
    background: transparent;
    color: var(--gf-color-text);
    font-size: var(--gf-font-size-sm);
    cursor: pointer;
    text-align: left;
  }
  .nav-item:hover {
    background: var(--gf-color-surface-alt);
  }
  .nav-item.active {
    border-color: var(--gf-color-accent);
    color: var(--gf-color-accent);
    font-weight: var(--gf-font-weight-bold);
  }
  kbd {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-color-text-muted);
    font-family: monospace;
  }
  .conn {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    margin-top: auto;
    font-size: var(--gf-font-size-sm);
  }
  .dot {
    width: 0.6rem;
    height: 0.6rem;
    border-radius: 50%;
    background: var(--gf-color-text-muted);
  }
  .dot[data-tone='ok'] {
    background: var(--gf-color-live);
  }
  .dot[data-tone='warn'] {
    background: var(--gf-color-warn);
  }
  .dot[data-tone='pending'] {
    background: var(--gf-color-accent);
  }
  .conn-label {
    text-transform: capitalize;
    color: var(--gf-color-text-muted);
  }
  .account {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
    font-size: var(--gf-font-size-xs);
  }
  .base {
    color: var(--gf-color-text-muted);
    word-break: break-all;
  }
  .signout {
    align-self: flex-start;
    background: transparent;
    border: 1px solid var(--gf-color-border);
    border-radius: var(--gf-radius-sm);
    padding: var(--gf-space-1) var(--gf-space-3);
    color: var(--gf-color-text);
    font-size: var(--gf-font-size-xs);
    cursor: pointer;
  }
  .content {
    padding: var(--gf-space-8);
    overflow: auto;
  }
</style>
