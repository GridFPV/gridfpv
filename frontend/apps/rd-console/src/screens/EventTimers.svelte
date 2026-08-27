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
   *
   * ## The GridFPV-plugin gate (#405)
   *
   * A RotorHazard timer without a loaded, compatible GridFPV plugin **cannot be selected**: its
   * checkbox is disabled *and* the row carries the reason plus the next action, because "greyed
   * out with no explanation" is the state that stranded the RD in #385. The Director enforces the
   * same rule on `PUT /events/{id}/timers` — this is the half that fails while the RD is choosing
   * equipment rather than while they are trying to start a race.
   *
   * ## Tuning from inside the event (#355/#411)
   *
   * The Tune action on a row navigates to the **event-scoped** tune route, so back returns here
   * rather than to the global Timers page. This is where an RD actually stands when a gate is missing
   * laps — mid-event, with a heat waiting — so tuning has to be reachable from here and not only from
   * the app-level Timers page.
   *
   * A timer the event **already** selects stays tickable (and untickable) even when its plugin has
   * since gone away: the Director grandfathers an existing selection so a pre-#405 event is still
   * editable, and the row carries a warning instead. What stops such an event from racing it is
   * the Director's arm-time backstop.
   *
   * ## Channel layouts (#117 S2)
   *
   * Per the RD, in **event** scope the timer page becomes per-node channel selection — the
   * {@link EventChannelLayouts} card at the bottom. Note carefully what the two halves of this page
   * edit: the checkbox picker inside {@link TimerManager} writes `Timer.available_channels`, the
   * **global** record of what a timer may *ever* use; a **layout** is this event's own `node →
   * channel` tuning, stored on the event's meta. Global is the seed, the event owns what it runs —
   * and editing a layout never touches a timer, which is the bug that slice exists to close.
   */
  import { Button, Card, toast } from '@gridfpv/components';
  import type { Timer, TimerId } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import { AutoSaver } from '../lib/autosave.js';
  import { selectionRefusal } from '../lib/pluginPresence.js';
  import EventChannelLayouts from './EventChannelLayouts.svelte';
  import TimerManager from './TimerManager.svelte';

  let {
    session,
    ontune,
    onselectionchange = undefined,
    showLayers = true
  }: {
    session: Session;
    /**
     * Open the per-timer **Tune** page for a timer, scoped to THIS event (#355/#411). The shell
     * owns the route (`#/events/<eventId>/timers/<timerId>/tune`); this screen is the entry point
     * the RD actually works from mid-event — "tuning from in the event would be ideal, as long as
     * when we click back we are back in the event". Optional, so an embedder with nowhere to
     * navigate to (the setup wizard's Timer step) simply doesn't offer the action.
     */
    ontune?: (timerId: TimerId) => void;
    /**
     * Fires the **live** working-selection count whenever it changes — including the local-only
     * empty state the setup wizard gates on (an empty selection is kept local, not persisted, so
     * `currentEvent.timers` alone can't see it). The wizard uses this to block advancing past the
     * Timer step until ≥1 timer is ticked. Inert for the standalone Timers page (no callback).
     */
    onselectionchange?: (count: number) => void;
    /**
     * Whether to show the per-node **channel layout** editor (#117 S2). On by default — this is the
     * event Timers page, and in event scope the timer page *is* per-node channel selection.
     *
     * The setup wizard passes `false`: its Timer step is where the RD is still choosing *which*
     * timer feeds the event, and a layout tunes a timer that has not been settled on yet. Layouts
     * stay fully editable on this page afterwards, which is the wizard's whole posture.
     */
    showLayers?: boolean;
  } = $props();

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

  // Surface the live selection size (including the local-only empty state) to any embedder — the
  // setup wizard reads this to gate advancing past the Timer step.
  $effect(() => {
    onselectionchange?.(selected.size);
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

  // ── The GridFPV-plugin gate (#405) ────────────────────────────────────────
  //
  // `blocked` is "the Director would refuse to *newly* select this". A timer the event already
  // selects is grandfathered by the Director (so a pre-#405 event stays editable), so its box
  // stays live — the RD must be able to untick it, which is the fix. Its row still carries the
  // warning, which is how a presence change on an already-selected timer surfaces here.
  function isBlocked(timer: Timer): boolean {
    return selectionRefusal(timer) !== null && !selected.has(timer.id);
  }
  /** The sentence the row shows: the refusal, or the already-selected warning. */
  function rowReason(timer: Timer): string | undefined {
    const refusal = selectionRefusal(timer);
    if (!refusal) return undefined;
    return selected.has(timer.id) ? refusal.alreadySelectedWarning : refusal.reason;
  }

  // ── Auto-save (debounced, optimistic, wholesale `setEventTimers`) ──────────
  const autosaver = new AutoSaver();

  function toggle(id: TimerId) {
    // The plugin gate (#405): never send a selection the Director will refuse. The box is already
    // disabled for a blocked timer; this is the belt to that braces, so a programmatic/keyboard
    // path can't slip a refused id into the wholesale save.
    const timer = timers.find((t) => t.id === id);
    if (timer && isBlocked(timer)) return;
    // Optimistic flip first so the checkbox reflects the click instantly.
    const next = new Set(selected);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    selected = next;
    // The RD may uncheck everything: we let the selection empty out (the row highlight clears since
    // it derives from `selected.has(id)` — no stale green) but **skip the auto-save** while empty.
    // We keep the empty state purely local rather than persisting/erroring; the setup-wizard's gate
    // guarantees an event still ends with ≥1 timer, and re-ticking one resumes the wholesale save.
    if (next.size === 0) return;
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

  // ── Channel layouts (#117 S2) ──────────────────────────────────────────────
  //
  // Per the RD, in **event** scope the timer page becomes per-node channel selection. That is the
  // layout editor below: a layout is one complete tuning of this event's timer (node → channel),
  // drawn from the channels ticked above.
  //
  // The distinction the two halves of this page draw is the whole slice: the checkbox picker inside
  // `TimerManager` edits `Timer.available_channels` — **the global timer record**, what a timer may
  // *ever* use — while a layout is **event** state. Editing a layout never touches a timer.
  //
  // A layout tunes the event's **effective primary** timer (#112's redundant timers are two boxes at
  // one gate, so an alternate must be listening on the same channels). Resolved from the registry
  // rows this screen already holds, so the editor and the roles picker cannot disagree about which
  // timer is primary.
  const layerTimer = $derived(timers.find((t) => t.id === effectivePrimary));

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
      ontune={ontune ? (timer) => ontune(timer.id) : undefined}
    >
      {#snippet rowLead(timer)}
        <input
          type="checkbox"
          class="select-box"
          checked={selected.has(timer.id)}
          disabled={isBlocked(timer)}
          onchange={() => toggle(timer.id)}
          aria-label={`Use ${timer.name}`}
        />
      {/snippet}

      <!-- #405: the reason lives ON the row, not in a tooltip — a disabled checkbox with no
           explanation is exactly the dead end this gate is supposed to prevent. -->
      {#snippet rowNote(timer)}
        {@const reason = rowReason(timer)}
        {#if reason}
          <span class="gate-reason" role="status">{reason}</span>
        {/if}
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

  {#if showLayers}
    <EventChannelLayouts {session} timer={layerTimer} />
  {/if}
</section>

<style>
  .event-timers {
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
  .select-box:disabled {
    cursor: not-allowed;
    opacity: 0.5;
  }
  /* The #405 refusal / warning sentence. Sized like real data, not chrome: at a venue this is the
     line that tells the RD why they cannot race this timer and what to do about it. */
  .gate-reason {
    margin-top: 2px;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-warn);
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
