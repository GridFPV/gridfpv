<script lang="ts">
  /**
   * EventRoster — the in-event **Registration** screen (issue #74).
   *
   * A single unified screen that lets the RD both **manage the directory** (add / edit / remove
   * pilots) and **pick which pilots race this event** — all without leaving the event to register
   * someone. The CRUD comes from the shared {@link PilotManager}; this screen layers per-event
   * **roster selection** on top: a checkbox per row bound to a local working set, saved to
   * `EventMeta.roster` via `setEventRoster`. This mirrors {@link EventTimers} (which wraps
   * {@link TimerManager}) exactly — same `rowLead` / `listHeader` / `listFooter` / `rowChecked`
   * seams.
   *
   * Because the embedded manager's add/edit/remove operate on the **app-level directory**, the RD
   * can register a brand-new pilot and immediately tick them into the event — the whole point. A
   * freshly-created pilot appears in the list and is selectable.
   *
   * Selection edits a local working set as the RD ticks boxes; "Save" pushes it (the session then
   * re-homes `currentEvent` with the server's response). After any create/edit/delete the manager
   * reloads and hands back the fresh list, so the working set is reconciled (a removed pilot drops
   * out of the selection; a freshly added one is simply available to tick).
   *
   * Heat building (which draws from this roster) is a later slice — out of scope here.
   */
  import { Button, Card, toast } from '@gridfpv/components';
  import type { Pilot, PilotId } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import PilotManager from './PilotManager.svelte';

  let { session }: { session: Session } = $props();

  let manager = $state<PilotManager | undefined>(undefined);

  // The directory list (kept in sync by the manager via `bind:pilots`) and the working selection
  // (a set of pilot ids), seeded from the event and edited locally until the RD saves. We snapshot
  // the event's saved roster so "Save"/"changed" can compare.
  let pilots = $state<Pilot[]>([]);
  let selected = $state<Set<PilotId>>(new Set());
  let savedRoster = $state<PilotId[]>([]);
  let saving = $state(false);

  function syncFromEvent() {
    const ids = session.currentEvent?.roster ?? [];
    savedRoster = [...ids];
    selected = new Set(ids);
  }

  // Seed the selection from the event on mount; the manager loads the directory itself.
  $effect(() => {
    syncFromEvent();
  });

  /**
   * Reconcile the working set against the fresh directory after a create/edit/delete: drop any
   * selected ids that no longer exist (e.g. a removed pilot). Newly added pilots just become
   * available to tick — we don't auto-select them.
   */
  function onDirectoryChange(list: Pilot[]) {
    const present = new Set(list.map((p) => p.id));
    const next = new Set<PilotId>();
    for (const id of selected) if (present.has(id)) next.add(id);
    selected = next;
  }

  function toggle(id: PilotId) {
    const next = new Set(selected);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    selected = next;
  }

  // The ids to save, in the directory's listed order (a stable, sensible order).
  const orderedSelection = $derived(pilots.filter((p) => selected.has(p.id)).map((p) => p.id));

  const changed = $derived(
    orderedSelection.length !== savedRoster.length ||
      orderedSelection.some((id, i) => id !== savedRoster[i])
  );

  async function save() {
    if (saving || !changed) return;
    saving = true;
    try {
      const updated = await session.setEventRoster(orderedSelection);
      if (!updated) {
        toast.info('A control token is required to set the event’s roster.');
        return;
      }
      syncFromEvent();
      toast.success('Event roster saved.');
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

<section class="event-roster" aria-label="Event roster">
  <Card
    title="Roster for this event"
    subtitle="Add, edit, or remove pilots in the directory, and tick which ones race this event."
  >
    {#snippet actions()}
      <Button variant="secondary" size="sm" onclick={() => manager?.openAdd()}>+ Add pilot</Button>
    {/snippet}

    <!-- The roster count always shows (even on an empty directory, where it reads "0 of 0" and the
         manager nudges to add pilots) — so it lives here, above the manager, not in `listHeader`
         (which only renders when the directory has pilots). -->
    <p class="roster-count" aria-live="polite">
      <strong>{orderedSelection.length}</strong> of {pilots.length}
      {pilots.length === 1 ? 'pilot' : 'pilots'} rostered for this event
    </p>

    <PilotManager
      bind:this={manager}
      {session}
      bind:pilots
      onchange={onDirectoryChange}
      rowChecked={(p) => selected.has(p.id)}
    >
      {#snippet rowLead(pilot)}
        <input
          type="checkbox"
          class="select-box"
          checked={selected.has(pilot.id)}
          onchange={() => toggle(pilot.id)}
          aria-label={`Roster ${pilot.callsign}`}
        />
      {/snippet}

      {#snippet listFooter()}
        <div class="foot">
          <span class="count" aria-live="polite">
            {orderedSelection.length} rostered
          </span>
          <div class="foot-actions">
            {#if changed}
              <Button variant="ghost" onclick={reset} disabled={saving}>Reset</Button>
            {/if}
            <Button variant="primary" onclick={save} loading={saving} disabled={!changed}>
              Save roster
            </Button>
          </div>
        </div>
      {/snippet}
    </PilotManager>
  </Card>
</section>

<style>
  .event-roster {
    max-width: 44rem;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .roster-count {
    margin: 0 0 var(--gf-space-1);
    font-size: var(--gf-font-size-md);
    color: var(--gf-text-secondary);
  }
  .roster-count strong {
    color: var(--gf-text);
    font-variant-numeric: tabular-nums;
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
