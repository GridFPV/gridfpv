<script lang="ts">
  /**
   * EventTimers — the in-event **Timers** screen (issue #73, Slice 2a/2b).
   *
   * A single unified screen that lets the RD both **manage the registry** (add / edit / remove
   * timers) and **pick which this event uses** — all without going back to the picker. The CRUD
   * comes from the shared {@link TimerManager}; this screen layers per-event **selection** on top:
   * a checkbox per row bound to a local working set, saved to `EventMeta.timers` via
   * `setEventTimers`. New events and Practice default to the built-in Mock.
   *
   * Selection edits a local working set as the RD ticks boxes; "Save" pushes it (the session then
   * re-homes `currentEvent` with the server's response). After any create/edit/delete the manager
   * reloads and hands back the fresh list, so the working set is reconciled (a removed timer drops
   * out of the selection; a freshly added one is simply available to tick).
   */
  import { Button, Card, toast } from '@gridfpv/components';
  import type { Timer, TimerId } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import TimerManager from './TimerManager.svelte';

  let { session }: { session: Session } = $props();

  let manager = $state<TimerManager | undefined>(undefined);

  // The registry list (kept in sync by the manager via `bind:timers`) and the working selection
  // (a set of timer ids), seeded from the event and edited locally until the RD saves. We snapshot
  // the event's saved selection so "Save"/"changed" can compare.
  let timers = $state<Timer[]>([]);
  let selected = $state<Set<TimerId>>(new Set());
  let savedSelection = $state<TimerId[]>([]);
  let saving = $state(false);

  function syncFromEvent() {
    const ids = session.currentEvent?.timers ?? [];
    savedSelection = [...ids];
    selected = new Set(ids);
  }

  // Seed the selection from the event on mount; the manager loads the registry itself.
  $effect(() => {
    syncFromEvent();
  });

  /**
   * Reconcile the working set against the fresh registry after a create/edit/delete: drop any
   * selected ids that no longer exist (e.g. a removed timer). Newly added timers just become
   * available to tick — we don't auto-select them.
   */
  function onRegistryChange(list: Timer[]) {
    const present = new Set(list.map((t) => t.id));
    const next = new Set<TimerId>();
    for (const id of selected) if (present.has(id)) next.add(id);
    selected = next;
  }

  function toggle(id: TimerId) {
    const next = new Set(selected);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    selected = next;
  }

  // The ids to save, in the registry's listed order (a stable, sensible order).
  const orderedSelection = $derived(timers.filter((t) => selected.has(t.id)).map((t) => t.id));

  const changed = $derived(
    orderedSelection.length !== savedSelection.length ||
      orderedSelection.some((id, i) => id !== savedSelection[i])
  );

  async function save() {
    if (saving || !changed) return;
    if (orderedSelection.length === 0) {
      toast.error('Select at least one timer for the event.');
      return;
    }
    saving = true;
    try {
      const updated = await session.setEventTimers(orderedSelection);
      if (!updated) {
        toast.info('A control token is required to set the event’s timers.');
        return;
      }
      syncFromEvent();
      toast.success('Event timers saved.');
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      saving = false;
    }
  }

  function reset() {
    syncFromEvent();
  }
</script>

<section class="event-timers" aria-label="Event timers">
  <Card
    title="Timers for this event"
    subtitle="Add, edit, or remove timers, and tick which ones feed this event's races."
  >
    {#snippet actions()}
      <Button variant="secondary" size="sm" onclick={() => manager?.openAdd()}>+ Add timer</Button>
    {/snippet}

    <TimerManager
      bind:this={manager}
      {session}
      bind:timers
      onchange={onRegistryChange}
      rowChecked={(t) => selected.has(t.id)}
    >
      {#snippet rowLead(timer)}
        <input
          type="checkbox"
          class="select-box"
          checked={selected.has(timer.id)}
          onchange={() => toggle(timer.id)}
          aria-label={`Use ${timer.name}`}
        />
      {/snippet}

      {#snippet listFooter()}
        <div class="foot">
          <span class="count" aria-live="polite">
            {orderedSelection.length} selected
          </span>
          <div class="foot-actions">
            {#if changed}
              <Button variant="ghost" onclick={reset} disabled={saving}>Reset</Button>
            {/if}
            <Button variant="primary" onclick={save} loading={saving} disabled={!changed}>
              Save selection
            </Button>
          </div>
        </div>
      {/snippet}
    </TimerManager>
  </Card>
</section>

<style>
  .event-timers {
    max-width: 44rem;
  }
  .select-box {
    width: 1.05rem;
    height: 1.05rem;
    accent-color: var(--gf-accent);
    flex-shrink: 0;
    cursor: pointer;
  }
  .foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-3);
    margin-top: var(--gf-space-1);
    padding-top: var(--gf-space-4);
    border-top: 1px solid var(--gf-border-subtle);
  }
  .count {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-muted);
  }
  .foot-actions {
    display: flex;
    gap: var(--gf-space-2);
  }
</style>
