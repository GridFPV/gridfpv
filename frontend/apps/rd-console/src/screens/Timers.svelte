<script lang="ts">
  /**
   * Timers — the **application-level** timer registry management modal (issue #73, Slice 2a/2b).
   *
   * Timers are app-wide configuration, not event state: the RD configures them **once** (a
   * persisted registry) and each event selects which to use ({@link EventTimers}). So this lives
   * at the picker/home level — opened from the picker header — for the **no-event** context, where
   * there is no event to select for.
   *
   * The actual list + add/edit/remove lives in the shared {@link TimerManager}; this is the thin
   * **modal** wrapper around it (CRUD-only — no per-event selection). The same {@link TimerManager}
   * is embedded inside an event by {@link EventTimers}, where selection is layered on top, so the
   * CRUD lives in exactly one place.
   */
  import { Button, Dialog } from '@gridfpv/components';
  import type { Session } from '../lib/session.svelte.js';
  import TimerManager from './TimerManager.svelte';

  let { session, open = $bindable(false) }: { session: Session; open?: boolean } = $props();

  let manager = $state<TimerManager | undefined>(undefined);

  // (Re)load whenever the modal opens, so it always reflects the current registry.
  $effect(() => {
    if (open) void manager?.load();
  });
</script>

<Dialog bind:open title="Timers">
  <div class="timers">
    <p class="lead">
      Timers are configured once and reused across events. Each event picks which timers it uses;
      the built-in <strong>Mock</strong> source flies a synthetic race with no hardware.
    </p>

    <TimerManager bind:this={manager} {session} />
  </div>

  {#snippet footer()}
    <Button variant="ghost" onclick={() => (open = false)}>Close</Button>
    <Button variant="primary" onclick={() => manager?.openAdd()}>+ Add timer</Button>
  {/snippet}
</Dialog>

<style>
  .timers {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .lead {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .lead strong {
    color: var(--gf-text-secondary);
    font-weight: var(--gf-font-weight-semibold);
  }
</style>
