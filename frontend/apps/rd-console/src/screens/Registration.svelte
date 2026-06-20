<script lang="ts">
  /**
   * Registration (#53) — bind seen competitor refs to pilots.
   *
   * The timing source reports source-local `CompetitorRef`s; the live projection
   * surfaces them (here, `LiveRaceState.active_pilots`). The RD types a `PilotId` for
   * each and binds it, emitting one `Register` command per binding (which the adapter
   * never does itself — architecture.html §9). A failed ack surfaces via `ErrorBanner`.
   */
  import type { AdapterId, CompetitorRef, LiveRaceState } from '@gridfpv/types';
  import { registerCommand } from '../lib/registration.js';
  import type { Session } from '../lib/session.svelte.js';
  import ErrorBanner from '../lib/ErrorBanner.svelte';

  let { session, adapter = 'rh-1' }: { session: Session; adapter?: AdapterId } = $props();

  const live = $derived<LiveRaceState | undefined>(session.liveState);
  // The competitor refs the source has seen (lineup of the current heat). A real
  // build also merges a dedicated "seen competitors" projection when one exists; the
  // live lineup is the v0.4 source.
  const seen = $derived<CompetitorRef[]>(live?.active_pilots ?? []);

  // RD-entered pilot id per competitor ref, and which are already bound this session.
  let pilotInputs = $state<Record<CompetitorRef, string>>({});
  let bound = $state<Record<CompetitorRef, string>>({});

  async function register(ref: CompetitorRef) {
    const pilot = (pilotInputs[ref] ?? '').trim();
    if (!pilot) return;
    const ack = await session.send(registerCommand(adapter, ref, pilot));
    if (ack.ok) {
      bound = { ...bound, [ref]: pilot };
    }
  }
</script>

<section class="registration" aria-label="Registration">
  <header>
    <h2>Registration</h2>
    <p class="muted">
      Bind each competitor the timer has seen on <code>{adapter}</code> to a pilot.
    </p>
  </header>

  {#if session.lastCommandError}
    <ErrorBanner error={session.lastCommandError} ondismiss={() => session.clearCommandError()} />
  {/if}

  {#if seen.length === 0}
    <p class="empty">No competitors seen yet. They appear here as the timer reports them.</p>
  {:else}
    <table>
      <thead>
        <tr>
          <th>Competitor (source ref)</th>
          <th>Pilot</th>
          <th>Action</th>
        </tr>
      </thead>
      <tbody>
        {#each seen as ref (ref)}
          <tr>
            <td class="ref">{ref}</td>
            <td>
              <input
                type="text"
                placeholder="pilot id"
                aria-label={`Pilot for ${ref}`}
                bind:value={
                  () => pilotInputs[ref] ?? bound[ref] ?? '',
                  (v) => (pilotInputs = { ...pilotInputs, [ref]: v })
                }
              />
            </td>
            <td>
              <button
                type="button"
                onclick={() => register(ref)}
                disabled={!(pilotInputs[ref] ?? '').trim() && !bound[ref]}
              >
                {bound[ref] ? 'Re-bind' : 'Register'}
              </button>
              {#if bound[ref]}
                <span class="bound" aria-label="Bound">✓ {bound[ref]}</span>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</section>

<style>
  .registration {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  h2 {
    font-size: var(--gf-font-size-lg);
    margin: 0;
  }
  .muted {
    color: var(--gf-color-text-muted);
    font-size: var(--gf-font-size-sm);
    margin: var(--gf-space-1) 0 0;
  }
  table {
    border-collapse: collapse;
    width: 100%;
  }
  th,
  td {
    text-align: left;
    padding: var(--gf-space-2) var(--gf-space-3);
    border-bottom: 1px solid var(--gf-color-border);
    font-size: var(--gf-font-size-sm);
  }
  .ref {
    font-family: monospace;
  }
  input {
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    padding: var(--gf-space-1) var(--gf-space-2);
    border: 1px solid var(--gf-color-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-color-surface);
    color: var(--gf-color-text);
  }
  button {
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    padding: var(--gf-space-2) var(--gf-space-4);
    border-radius: var(--gf-radius-sm);
    border: 1px solid var(--gf-color-accent);
    background: var(--gf-color-accent);
    color: var(--gf-color-accent-contrast);
    cursor: pointer;
  }
  button:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  .bound {
    margin-left: var(--gf-space-2);
    color: var(--gf-color-live);
    font-size: var(--gf-font-size-xs);
  }
  .empty {
    color: var(--gf-color-text-muted);
    font-size: var(--gf-font-size-sm);
  }
</style>
