<script lang="ts">
  /**
   * PilotsPage — the app-level **Pilots** page (#118, placeholder for #74).
   *
   * A real route in the two-level IA's home hub (Home › Pilots), but **deliberately minimal** for
   * this slice: a heading, a read-only list of the directory's callsigns (`GET /pilots`, open, no
   * token), and a "registration UI coming" note. The full pilot-directory management — the add /
   * edit / remove `PilotManager` form — is the very next slice (#74).
   *
   * SEAM for #74: the `PilotManager` component plugs in **right here**, replacing the read-only
   * `<ul class="pilot-list">` + the "coming" note below with the management UI (mirroring how
   * {@link TimersPage} hosts {@link TimerManager}). The session already exposes `listPilots()`;
   * the next slice adds `createPilot` / `updatePilot` / `deletePilot` seams alongside it.
   */
  import { Badge, Button, Card } from '@gridfpv/components';
  import type { Pilot } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import Breadcrumbs from '../Breadcrumbs.svelte';

  let { session, onhome }: { session: Session; onhome: () => void } = $props();

  type LoadState =
    | { kind: 'loading' }
    | { kind: 'error'; message: string }
    | { kind: 'ready'; pilots: Pilot[] };

  let loadState = $state<LoadState>({ kind: 'loading' });

  async function load() {
    loadState = { kind: 'loading' };
    try {
      const pilots = await session.listPilots();
      loadState = { kind: 'ready', pilots };
    } catch (e) {
      loadState = { kind: 'error', message: e instanceof Error ? e.message : String(e) };
    }
  }

  $effect(() => {
    void load();
  });
</script>

<div class="page">
  <div class="page-inner">
    <Breadcrumbs crumbs={[{ label: 'Home', onclick: onhome }, { label: 'Pilots' }]} />

    <header class="page-head">
      <h1 class="page-title">Pilots</h1>
      <p class="lead">
        The application-level pilot directory — maintained once, rostered per event.
      </p>
    </header>

    <Card elevation="sm">
      <div class="body" aria-label="Pilot directory">
        {#if loadState.kind === 'loading'}
          <div class="state-msg" role="status">
            <span class="spinner" aria-hidden="true"></span>
            Loading pilots…
          </div>
        {:else if loadState.kind === 'error'}
          <div class="state-error">
            <p>Couldn't reach the Director.</p>
            <code>{loadState.message}</code>
            <Button variant="secondary" onclick={load}>Try again</Button>
          </div>
        {:else if loadState.pilots.length === 0}
          <div class="empty">
            <p>No pilots in the directory yet.</p>
          </div>
        {:else}
          <ul class="pilot-list">
            {#each loadState.pilots as pilot (pilot.id)}
              <li class="pilot-row">
                <span class="callsign">{pilot.callsign}</span>
                {#if pilot.name}<span class="real-name">{pilot.name}</span>{/if}
                {#if pilot.team}<Badge tone="neutral">{pilot.team}</Badge>{/if}
              </li>
            {/each}
          </ul>
        {/if}

        <!-- TODO(#74): PilotManager — the add/edit/remove directory form plugs in here,
             replacing this read-only list + the note below (see TimersPage/TimerManager). -->
        <p class="coming">
          <span class="coming-tag">Coming soon</span>
          Pilot registration UI (add, edit, remove, cloud-pull import) lands in the next slice.
        </p>
      </div>
    </Card>
  </div>
</div>

<style>
  .page {
    min-height: 100vh;
    padding: var(--gf-space-6) var(--gf-space-8) var(--gf-space-8);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    overflow: auto;
  }
  .page-inner {
    width: min(52rem, 100%);
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
  }
  .page-head {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .page-title {
    margin: 0;
    font-size: var(--gf-font-size-2xl);
    letter-spacing: var(--gf-tracking-tight);
  }
  .lead {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .body {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
    padding: var(--gf-space-2);
  }
  .state-msg {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    padding: var(--gf-space-4) 0;
  }
  .spinner {
    width: 1.1rem;
    height: 1.1rem;
    border-radius: 50%;
    border: 2px solid var(--gf-border);
    border-top-color: var(--gf-accent);
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .spinner {
      animation-duration: 2s;
    }
  }
  .state-error {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
    align-items: flex-start;
  }
  .state-error p {
    margin: 0;
    font-weight: var(--gf-font-weight-semibold);
  }
  .state-error code {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-danger);
    word-break: break-all;
  }
  .empty {
    padding: var(--gf-space-4) 0;
    color: var(--gf-text-muted);
  }
  .empty p {
    margin: 0;
  }
  .pilot-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .pilot-row {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    padding: var(--gf-space-3) var(--gf-space-4);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-md);
    background: var(--gf-surface);
  }
  .callsign {
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
  }
  .real-name {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .coming {
    margin: 0;
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
    padding: var(--gf-space-3) var(--gf-space-4);
    border: 1px dashed var(--gf-border);
    border-radius: var(--gf-radius-md);
    background: var(--gf-surface-alt);
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .coming-tag {
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    font-size: var(--gf-font-size-2xs);
    font-weight: var(--gf-font-weight-semibold);
    color: var(--gf-accent);
  }
</style>
