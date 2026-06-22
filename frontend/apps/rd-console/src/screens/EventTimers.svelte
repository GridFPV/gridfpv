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
   * Selection edits a local working set as the RD ticks boxes; each tick **auto-saves** the full
   * selection (debounced, wholesale `setEventTimers`) — there is no explicit Save button. The
   * session then re-homes `currentEvent` with the server's response. After any create/edit/delete
   * the manager reloads and hands back the fresh list, so the working set is reconciled (a removed
   * timer drops out of the selection; a freshly added one is simply available to tick).
   *
   * Auto-save is **optimistic**: the checkbox flips instantly and the wholesale-set lands in the
   * background; on a save error the change is reverted (re-seeded from the event) and surfaced.
   * Because every save sends the *entire* current selection (not a delta), coalescing rapid clicks
   * into one trailing save is safe last-write-wins.
   */
  import { Button, Card, toast } from '@gridfpv/components';
  import type { Timer, TimerId } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import { AutoSaver } from '../lib/autosave.js';
  import TimerManager from './TimerManager.svelte';

  let { session }: { session: Session } = $props();

  let manager = $state<TimerManager | undefined>(undefined);

  // The registry list (kept in sync by the manager via `bind:timers`) and the working selection
  // (a set of timer ids), seeded from the event. Toggling a box edits the set optimistically and
  // schedules a debounced save; we snapshot the event's saved selection so a failed save can revert.
  let timers = $state<Timer[]>([]);
  let selected = $state<Set<TimerId>>(new Set());
  let savedSelection = $state<TimerId[]>([]);

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

  // The ids to save, in the registry's listed order (a stable, sensible order).
  const orderedSelection = $derived(timers.filter((t) => selected.has(t.id)).map((t) => t.id));

  // ── Auto-save (debounced, optimistic, wholesale `setEventTimers`) ──────────
  const autosaver = new AutoSaver();

  function toggle(id: TimerId) {
    // Optimistic flip first so the checkbox reflects the click instantly.
    const next = new Set(selected);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    // An event must keep at least one timer — refuse to auto-clear the last one (revert + nudge).
    if (next.size === 0) {
      toast.error('An event needs at least one timer.');
      // Re-seed from the event so the bound checkbox reliably snaps back to checked (the native
      // click already flipped the DOM; re-seeding restores the saved selection).
      syncFromEvent();
      return;
    }
    selected = next;
    scheduleSave();
  }

  // Schedule a single trailing save of the full selection. The payload is computed at flush time so
  // it reflects the latest selection even if more boxes were ticked during the debounce window.
  function scheduleSave() {
    autosaver.schedule('timers', {
      compute: () => orderedSelection,
      save: (ids) => session.setEventTimers(ids),
      onUnsaved: () => {
        toast.info('A control token is required to set the event’s timers.');
        syncFromEvent();
      },
      onError: (e) => {
        // Revert the optimistic change to the last-saved selection and surface the failure.
        syncFromEvent();
        toast.error(e instanceof Error ? e.message : String(e));
      }
    });
  }

  // ── Primary / alternate roles (issue #112) ────────────────────────────────
  //
  // Roles are a property of the event's **saved** selection (the timers actually feeding the
  // race), not the unsaved working set — `EventMeta.primary_timer` is server state keyed to the
  // saved timers. So the role picker reflects `savedSelection`, in its saved order.
  //
  // Effective primary: the event's `primary_timer` when it's set and still in the selection;
  // otherwise the **first** selected timer (the "first selected = primary when null" rule). Only
  // surfaced when 2+ timers are selected — a lone timer is trivially the primary, no UI noise.

  // The saved selection as registry rows (so we can show names), in saved order.
  const roleTimers = $derived(
    savedSelection
      .map((id) => timers.find((t) => t.id === id))
      .filter((t): t is Timer => t !== undefined)
  );

  const showRoles = $derived(roleTimers.length >= 2);

  // The currently-effective primary id, applying the "first selected = primary when null" rule
  // (shared with the context header via the session getter).
  const effectivePrimary = $derived(session.primaryTimerId);

  // Guards a primary change in flight so the radios don't double-fire mid-request.
  let settingPrimary = $state(false);

  async function choosePrimary(id: TimerId) {
    if (settingPrimary || id === effectivePrimary) return;
    settingPrimary = true;
    try {
      const updated = await session.setPrimaryTimer(id);
      if (!updated) {
        toast.info('A control token is required to set the primary timer.');
        return;
      }
      toast.success('Primary timer updated.');
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      settingPrimary = false;
    }
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
            {orderedSelection.length} selected · saved automatically
          </span>
        </div>
      {/snippet}
    </TimerManager>
  </Card>

  {#if showRoles}
    <Card
      title="Timer roles"
      subtitle="Primary feeds the race; alternates are hot standby and take over if the primary drops."
    >
      <ul class="roles" aria-label="Timer roles">
        {#each roleTimers as timer (timer.id)}
          {@const isPrimary = timer.id === effectivePrimary}
          <li class="role-row" class:primary={isPrimary}>
            <label class="role-label">
              <input
                type="radio"
                name="primary-timer"
                class="role-radio"
                checked={isPrimary}
                disabled={settingPrimary}
                onchange={() => choosePrimary(timer.id)}
                aria-label={`Make ${timer.name} the primary timer`}
              />
              <span class="role-name">{timer.name}</span>
            </label>
            <span
              class="role-badge"
              class:badge-primary={isPrimary}
              class:badge-alternate={!isPrimary}
            >
              {isPrimary ? 'Primary' : 'Alternate'}
            </span>
          </li>
        {/each}
      </ul>
    </Card>
  {/if}
</section>

<style>
  .event-timers {
    max-width: 44rem;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .roles {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .role-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-3);
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-md);
    background: var(--gf-surface-alt);
  }
  .role-row.primary {
    border-color: var(--gf-success);
  }
  .role-label {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    cursor: pointer;
    min-width: 0;
  }
  .role-radio {
    width: 1.05rem;
    height: 1.05rem;
    accent-color: var(--gf-success);
    flex-shrink: 0;
    cursor: pointer;
  }
  .role-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .role-badge {
    flex-shrink: 0;
    font-size: var(--gf-font-size-2xs);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding: 0.1rem var(--gf-space-2);
    border-radius: var(--gf-radius-pill);
  }
  .badge-primary {
    color: var(--gf-success);
    background: var(--gf-success-soft);
  }
  .badge-alternate {
    color: var(--gf-text-muted);
    background: var(--gf-surface-sunken);
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
</style>
