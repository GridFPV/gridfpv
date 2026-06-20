<script lang="ts">
  /**
   * EventTimers — the per-event **timer selector** (issue #73, Slice 2a).
   *
   * Inside an event, the RD chooses which of the app-level registry timers this event uses: a
   * lightweight checklist of every configured timer bound to `EventMeta.timers`, saved via
   * `setEventTimers`. The registry itself is managed app-wide ({@link Timers}, from the picker);
   * here we only **select**. New events and Practice default to the built-in Mock.
   *
   * The selection edits a local working set as the RD ticks boxes; "Save" pushes it (the session
   * then re-homes `currentEvent` with the server's response). A link points back to the picker's
   * Timers screen for adding/editing the timers themselves.
   */
  import { Badge, Button, Card, StatusPill, toast } from '@gridfpv/components';
  import type { Timer, TimerId } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import { kindLabel, kindSummary, kindTone } from '../lib/timers.js';

  let { session }: { session: Session } = $props();

  type LoadState =
    | { kind: 'loading' }
    | { kind: 'error'; message: string }
    | { kind: 'ready'; timers: Timer[] };

  let loadState = $state<LoadState>({ kind: 'loading' });

  // The working selection (a set of timer ids), seeded from the event and edited locally until
  // the RD saves. We snapshot the event's current selection so "Save"/"changed" can compare.
  let selected = $state<Set<TimerId>>(new Set());
  let savedSelection = $state<TimerId[]>([]);
  let saving = $state(false);

  function syncFromEvent() {
    const ids = session.currentEvent?.timers ?? [];
    savedSelection = [...ids];
    selected = new Set(ids);
  }

  async function load() {
    loadState = { kind: 'loading' };
    syncFromEvent();
    try {
      const timers = await session.listTimers();
      loadState = { kind: 'ready', timers };
    } catch (e) {
      loadState = { kind: 'error', message: e instanceof Error ? e.message : String(e) };
    }
  }

  $effect(() => {
    void load();
  });

  function toggle(id: TimerId) {
    const next = new Set(selected);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    selected = next;
  }

  // The ids to save, in the registry's listed order (a stable, sensible order).
  const orderedSelection = $derived(
    loadState.kind === 'ready'
      ? loadState.timers.filter((t) => selected.has(t.id)).map((t) => t.id)
      : []
  );

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
    subtitle="Choose which configured timers feed this event's races. Manage the timers themselves from the home screen."
  >
    {#if loadState.kind === 'loading'}
      <div class="state-msg" role="status">
        <span class="spinner" aria-hidden="true"></span>
        Loading timers…
      </div>
    {:else if loadState.kind === 'error'}
      <div class="state-error">
        <p>Couldn't load the timers.</p>
        <code>{loadState.message}</code>
        <Button variant="secondary" onclick={load}>Try again</Button>
      </div>
    {:else if loadState.timers.length === 0}
      <div class="empty">
        <p>No timers configured.</p>
        <p class="empty-sub">Add a timer from the home screen first, then select it here.</p>
      </div>
    {:else}
      <ul class="select-list">
        {#each loadState.timers as timer (timer.id)}
          <li>
            <label class="select-row" class:checked={selected.has(timer.id)}>
              <input
                type="checkbox"
                checked={selected.has(timer.id)}
                onchange={() => toggle(timer.id)}
                aria-label={`Use ${timer.name}`}
              />
              <span class="select-main">
                <span class="select-head">
                  <span class="select-name">{timer.name}</span>
                  <Badge tone={kindTone(timer.kind)}>{kindLabel(timer.kind)}</Badge>
                </span>
                <span class="select-sub">{kindSummary(timer.kind)}</span>
              </span>
              <StatusPill status={timer.status} label={timer.status} size="sm" />
            </label>
          </li>
        {/each}
      </ul>

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
    {/if}
  </Card>
</section>

<style>
  .event-timers {
    max-width: 42rem;
  }
  .state-msg {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    padding: var(--gf-space-2) 0;
  }
  .spinner {
    width: 1.1rem;
    height: 1.1rem;
    border-radius: 50%;
    border: 2px solid var(--gf-border);
    border-top-color: var(--gf-accent);
    animation: et-spin 0.7s linear infinite;
  }
  @keyframes et-spin {
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
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
  }
  .empty p {
    margin: 0;
    font-weight: var(--gf-font-weight-medium);
  }
  .empty-sub {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-regular) !important;
  }

  .select-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .select-row {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    padding: var(--gf-space-3) var(--gf-space-4);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    background: var(--gf-surface);
    cursor: pointer;
    transition:
      border-color var(--gf-motion-fast) var(--gf-ease-out),
      background var(--gf-motion-fast) var(--gf-ease-out);
  }
  .select-row:hover {
    border-color: var(--gf-border-strong);
  }
  .select-row.checked {
    border-color: var(--gf-accent);
    background: var(--gf-accent-soft);
  }
  .select-row input[type='checkbox'] {
    width: 1.05rem;
    height: 1.05rem;
    accent-color: var(--gf-accent);
    flex-shrink: 0;
    cursor: pointer;
  }
  .select-main {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1;
    min-width: 0;
  }
  .select-head {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
  }
  .select-name {
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
  }
  .select-sub {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-muted);
  }

  .foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-3);
    margin-top: var(--gf-space-4);
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
