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
  import { Button, Badge, Card } from '@gridfpv/components';
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
    <Card elevation="flat">
      <p class="empty">No competitors seen yet. They appear here as the timer reports them.</p>
    </Card>
  {:else}
    <Card pad={false}>
      <table>
        <thead>
          <tr>
            <th>Competitor (source ref)</th>
            <th>Pilot</th>
            <th class="action-col">Action</th>
          </tr>
        </thead>
        <tbody>
          {#each seen as ref (ref)}
            <tr>
              <td class="ref">{ref}</td>
              <td>
                <input
                  class="gf-reg-input"
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
                <div class="action-cell">
                  <Button
                    variant={bound[ref] ? 'secondary' : 'primary'}
                    size="sm"
                    onclick={() => register(ref)}
                    disabled={!(pilotInputs[ref] ?? '').trim() && !bound[ref]}
                  >
                    {bound[ref] ? 'Re-bind' : 'Register'}
                  </Button>
                  {#if bound[ref]}
                    <span aria-label="Bound"><Badge tone="success" dot>{bound[ref]}</Badge></span>
                  {/if}
                </div>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </Card>
  {/if}
</section>

<style>
  .registration {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  h2 {
    font-size: var(--gf-font-size-xl);
    margin: 0;
    letter-spacing: var(--gf-tracking-tight);
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    margin: var(--gf-space-1) 0 0;
  }
  .muted code {
    color: var(--gf-text-secondary);
    background: var(--gf-surface-alt);
    padding: 0.05em 0.35em;
    border-radius: var(--gf-radius-xs);
  }
  table {
    border-collapse: collapse;
    width: 100%;
    font-size: var(--gf-font-size-sm);
  }
  thead th {
    text-align: left;
    padding: var(--gf-space-3) var(--gf-space-4);
    border-bottom: 1px solid var(--gf-border);
    background: var(--gf-surface-alt);
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-2xs);
    font-weight: var(--gf-font-weight-semibold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
  }
  tbody td {
    text-align: left;
    padding: var(--gf-space-3) var(--gf-space-4);
  }
  tbody tr + tr td {
    border-top: 1px solid var(--gf-border-subtle);
  }
  .action-col {
    width: 1%;
  }
  .ref {
    font-family: var(--gf-font-mono);
    color: var(--gf-text-secondary);
  }
  .action-cell {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
  }
  .gf-reg-input {
    width: 100%;
    max-width: 16rem;
    box-sizing: border-box;
    height: 2.1rem;
    padding: 0 var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    transition:
      border-color var(--gf-motion-fast) var(--gf-ease-out),
      box-shadow var(--gf-motion-fast) var(--gf-ease-out);
  }
  .gf-reg-input::placeholder {
    color: var(--gf-text-faint);
  }
  .gf-reg-input:focus {
    outline: none;
    border-color: var(--gf-accent);
    box-shadow: 0 0 0 3px var(--gf-accent-soft);
  }
  .empty {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
</style>
