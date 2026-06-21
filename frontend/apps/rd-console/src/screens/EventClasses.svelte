<script lang="ts">
  /**
   * EventClasses — the in-event **Classes** screen (issue #84).
   *
   * A single unified screen that lets the RD both **manage the directory** (add / edit / remove
   * classes) and **pick which classes this event runs** — all without leaving the event. The CRUD
   * comes from the shared {@link ClassManager}; this screen layers per-event **class selection** on
   * top: a checkbox per row bound to a local working set, saved to `EventMeta.classes` via
   * `setEventClasses`. This mirrors {@link EventRoster} (which wraps {@link PilotManager}) exactly —
   * same `rowLead` / `listHeader` / `listFooter` / `rowChecked` seams.
   *
   * Because the embedded manager's add/edit/remove operate on the **app-level directory**, the RD
   * can register a brand-new class and immediately tick it into the event. A freshly-created class
   * appears in the list and is selectable.
   *
   * Selection edits a local working set as the RD ticks boxes; "Save" pushes it (the session then
   * re-homes `currentEvent` with the server's response). After any create/edit/delete the manager
   * reloads and hands back the fresh list, so the working set is reconciled (a removed class drops
   * out of the selection; a freshly added one is simply available to tick).
   */
  import { Button, Card, toast } from '@gridfpv/components';
  import type { Class, ClassId } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import ClassManager from './ClassManager.svelte';

  let { session }: { session: Session } = $props();

  let manager = $state<ClassManager | undefined>(undefined);

  // The directory list (kept in sync by the manager via `bind:classes`) and the working selection
  // (a set of class ids), seeded from the event and edited locally until the RD saves. We snapshot
  // the event's saved selection so "Save"/"changed" can compare.
  let classes = $state<Class[]>([]);
  let selected = $state<Set<ClassId>>(new Set());
  let savedClasses = $state<ClassId[]>([]);
  let saving = $state(false);

  function syncFromEvent() {
    const ids = session.currentEvent?.classes ?? [];
    savedClasses = [...ids];
    selected = new Set(ids);
  }

  // Seed the selection from the event on mount; the manager loads the directory itself.
  $effect(() => {
    syncFromEvent();
  });

  /**
   * Reconcile the working set against the fresh directory after a create/edit/delete: drop any
   * selected ids that no longer exist (e.g. a removed class). Newly added classes just become
   * available to tick — we don't auto-select them.
   */
  function onDirectoryChange(list: Class[]) {
    const present = new Set(list.map((c) => c.id));
    const next = new Set<ClassId>();
    for (const id of selected) if (present.has(id)) next.add(id);
    selected = next;
  }

  function toggle(id: ClassId) {
    const next = new Set(selected);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    selected = next;
  }

  // The ids to save, in the directory's listed order (a stable, sensible order).
  const orderedSelection = $derived(classes.filter((c) => selected.has(c.id)).map((c) => c.id));

  const changed = $derived(
    orderedSelection.length !== savedClasses.length ||
      orderedSelection.some((id, i) => id !== savedClasses[i])
  );

  async function save() {
    if (saving || !changed) return;
    saving = true;
    try {
      const updated = await session.setEventClasses(orderedSelection);
      if (!updated) {
        toast.info('A control token is required to set the event’s classes.');
        return;
      }
      syncFromEvent();
      toast.success('Event classes saved.');
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

<section class="event-classes" aria-label="Event classes">
  <Card
    title="Classes for this event"
    subtitle="Add, edit, or remove classes in the directory, and tick which ones this event runs."
  >
    {#snippet actions()}
      <Button variant="secondary" size="sm" onclick={() => manager?.openAdd()}>+ Add class</Button>
    {/snippet}

    <!-- The selection count always shows (even on an empty directory, where it reads "0 of 0" and
         the manager nudges to add classes) — so it lives here, above the manager, not in
         `listHeader` (which only renders when the directory has classes). -->
    <p class="class-count" aria-live="polite">
      <strong>{orderedSelection.length}</strong> of {classes.length}
      {classes.length === 1 ? 'class' : 'classes'} selected for this event
    </p>

    <ClassManager
      bind:this={manager}
      {session}
      bind:classes
      onchange={onDirectoryChange}
      rowChecked={(c) => selected.has(c.id)}
    >
      {#snippet rowLead(cls)}
        <input
          type="checkbox"
          class="select-box"
          checked={selected.has(cls.id)}
          onchange={() => toggle(cls.id)}
          aria-label={`Select ${cls.name}`}
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
              Save classes
            </Button>
          </div>
        </div>
      {/snippet}
    </ClassManager>
  </Card>
</section>

<style>
  .event-classes {
    max-width: 44rem;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .class-count {
    margin: 0 0 var(--gf-space-1);
    font-size: var(--gf-font-size-md);
    color: var(--gf-text-secondary);
  }
  .class-count strong {
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
