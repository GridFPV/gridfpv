<script lang="ts">
  /**
   * TimerManager — the shared timer **registry management** piece (issue #73, Slice 2b).
   *
   * The single home for timer CRUD: it lists every configured timer (name + a **kind badge**
   * Mock/RotorHazard + its `status` via `StatusPill`) and drives **add** / **edit** / **remove**
   * through one stacked Dialog. The built-in **Mock** is undeletable (its Remove is hidden) and a
   * stray 400 is surfaced gracefully. Ids are auto-generated server-side — never shown or asked for.
   *
   * It is used two ways, so the CRUD lives in one place:
   *  - the picker's {@link Timers} modal embeds it **CRUD-only** (no event context to select for);
   *  - the in-event {@link EventTimers} screen embeds it and layers **per-event selection** on top,
   *    via the optional `rowLead` snippet (a checkbox per row) and `listHeader`/`listFooter`
   *    snippets (the selection count + Save).
   *
   * The list (and so the available-to-select set) refreshes after every create/edit/delete; the
   * latest `timers` are exposed back to the parent through the bindable `timers` prop and an
   * `onchange` callback so a selection owner can reconcile.
   */
  import {
    Badge,
    Button,
    Card,
    Dialog,
    Field,
    Input,
    Select,
    StatusPill,
    toast
  } from '@gridfpv/components';
  import type { Snippet } from 'svelte';
  import type { CreateTimerRequest, Timer, TimerKind, UpdateTimerRequest } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import {
    DEFAULT_MOCK_LAPS,
    DEFAULT_MOCK_LAP_MS,
    isBuiltInMock,
    kindLabel,
    kindSummary,
    kindTag,
    kindTone,
    type TimerKindTag
  } from '../lib/timers.js';

  let {
    session,
    timers = $bindable([]),
    onchange,
    rowLead,
    listHeader,
    listFooter,
    rowChecked
  }: {
    session: Session;
    /** The latest loaded registry, exposed so a selection owner can reconcile its working set. */
    timers?: Timer[];
    /** Called after every create/edit/delete (and the reload it triggers) with the fresh list. */
    onchange?: (timers: Timer[]) => void;
    /** Rendered at the **start** of each row (e.g. the per-event selection checkbox). */
    rowLead?: Snippet<[Timer]>;
    /** Rendered **above** the list (e.g. nothing today; reserved for selection chrome). */
    listHeader?: Snippet;
    /** Rendered **below** the list (e.g. the selection count + Save). */
    listFooter?: Snippet;
    /** Whether a row is currently selected — drives the row's "checked" highlight in select mode. */
    rowChecked?: (timer: Timer) => boolean;
  } = $props();

  type LoadState =
    | { kind: 'loading' }
    | { kind: 'error'; message: string }
    | { kind: 'ready'; timers: Timer[] };

  let loadState = $state<LoadState>({ kind: 'loading' });

  export async function load() {
    loadState = { kind: 'loading' };
    try {
      const list = await session.listTimers();
      loadState = { kind: 'ready', timers: list };
      timers = list;
    } catch (e) {
      loadState = { kind: 'error', message: e instanceof Error ? e.message : String(e) };
    }
  }

  // (Re)load on mount, so the list always reflects the current registry.
  $effect(() => {
    void load();
  });

  /** Reload, then notify the parent so a selection owner can reconcile against the fresh set. */
  async function reload() {
    await load();
    if (loadState.kind === 'ready') onchange?.(loadState.timers);
  }

  // ── The add / edit dialog ──────────────────────────────────────────────────
  // One dialog drives both create and edit; `editing` (the timer, or undefined for "add") chooses
  // which protocol call submit makes. Kind-specific fields are kept as strings while the RD types
  // and coerced on submit; switching the kind `Select` keeps both kinds' fields around.
  let formOpen = $state(false);
  let editing = $state<Timer | undefined>(undefined);
  let formName = $state('');
  let formKind = $state<TimerKindTag>('Mock');
  let formLaps = $state(String(DEFAULT_MOCK_LAPS));
  let formLapMs = $state(String(DEFAULT_MOCK_LAP_MS));
  let formUrl = $state('');
  let saving = $state(false);
  let formError = $state<string | undefined>(undefined);

  export function openAdd() {
    editing = undefined;
    formName = '';
    formKind = 'Mock';
    formLaps = String(DEFAULT_MOCK_LAPS);
    formLapMs = String(DEFAULT_MOCK_LAP_MS);
    formUrl = '';
    formError = undefined;
    formOpen = true;
  }

  function openEdit(timer: Timer) {
    editing = timer;
    formName = timer.name;
    formKind = kindTag(timer.kind);
    if ('Mock' in timer.kind) {
      formLaps = String(timer.kind.Mock.laps);
      formLapMs = String(timer.kind.Mock.lap_ms);
      formUrl = '';
    } else {
      formLaps = String(DEFAULT_MOCK_LAPS);
      formLapMs = String(DEFAULT_MOCK_LAP_MS);
      formUrl = timer.kind.Rotorhazard.url;
    }
    formError = undefined;
    formOpen = true;
  }

  /** Build the typed `TimerKind` from the form, or a problem string if a field is invalid. */
  function buildKind(): { kind: TimerKind } | { problem: string } {
    if (formKind === 'Mock') {
      const laps = Number(formLaps);
      const lapMs = Number(formLapMs);
      if (!Number.isFinite(laps) || laps < 1) return { problem: 'Laps must be at least 1.' };
      if (!Number.isFinite(lapMs) || lapMs < 1)
        return { problem: 'Lap pace must be at least 1ms.' };
      return { kind: { Mock: { laps: Math.round(laps), lap_ms: Math.round(lapMs) } } };
    }
    const url = formUrl.trim();
    if (!url) return { problem: 'A RotorHazard URL is required.' };
    return { kind: { Rotorhazard: { url } } };
  }

  async function submitForm(e?: Event) {
    e?.preventDefault();
    if (saving) return;
    const name = formName.trim();
    if (!name) {
      formError = 'A name is required.';
      return;
    }
    const built = buildKind();
    if ('problem' in built) {
      formError = built.problem;
      return;
    }
    saving = true;
    formError = undefined;
    try {
      if (editing) {
        const req: UpdateTimerRequest = { name, kind: built.kind };
        const updated = await session.updateTimer(editing.id, req);
        if (!updated) {
          formError = 'A control token is required to edit a timer.';
          return;
        }
        toast.success(`Updated “${updated.name}”.`);
      } else {
        const req: CreateTimerRequest = { name, kind: built.kind };
        const created = await session.createTimer(req);
        if (!created) {
          formError = 'A control token is required to add a timer.';
          return;
        }
        toast.success(`Added “${created.name}”.`);
      }
      formOpen = false;
      await reload();
    } catch (err) {
      formError = err instanceof Error ? err.message : String(err);
    } finally {
      saving = false;
    }
  }

  // ── Remove ──────────────────────────────────────────────────────────────────
  let removing = $state<string | undefined>(undefined);
  async function remove(timer: Timer) {
    if (removing) return;
    removing = timer.id;
    try {
      const done = await session.deleteTimer(timer.id);
      if (done === undefined) {
        // The RD cancelled the token prompt on a gated Director — nothing removed.
        toast.info('A control token is required to remove a timer.');
        return;
      }
      toast.success(`Removed “${timer.name}”.`);
      await reload();
    } catch (err) {
      // The built-in Mock answers 400; any other id, a transport/404 — surface it gracefully.
      const msg = err instanceof Error ? err.message : String(err);
      if (/\b400\b/.test(msg)) toast.error('The built-in Mock timer can’t be removed.');
      else toast.error(msg);
    } finally {
      removing = undefined;
    }
  }
</script>

<div class="timer-manager">
  {#if loadState.kind === 'loading'}
    <div class="state-msg" role="status">
      <span class="spinner" aria-hidden="true"></span>
      Loading timers…
    </div>
  {:else if loadState.kind === 'error'}
    <Card elevation="sm">
      <div class="state-error">
        <p>Couldn't load the timers.</p>
        <code>{loadState.message}</code>
        <Button variant="secondary" onclick={load}>Try again</Button>
      </div>
    </Card>
  {:else if loadState.timers.length === 0}
    <div class="empty">
      <p>No timers configured.</p>
      <p class="empty-sub">Add one to give your events a lap source.</p>
    </div>
  {:else}
    {@render listHeader?.()}
    <ul class="timer-list" aria-label="Configured timers">
      {#each loadState.timers as timer (timer.id)}
        <li class="timer-row" class:checked={rowChecked?.(timer)}>
          {@render rowLead?.(timer)}
          <div class="timer-main">
            <div class="timer-head">
              <span class="timer-name">{timer.name}</span>
              <Badge tone={kindTone(timer.kind)}>{kindLabel(timer.kind)}</Badge>
              {#if isBuiltInMock(timer)}<Badge tone="neutral" variant="outline">Built-in</Badge
                >{/if}
            </div>
            <span class="timer-sub">{kindSummary(timer.kind)}</span>
          </div>
          <StatusPill status={timer.status} label={timer.status} size="sm" />
          <div class="timer-actions">
            <Button variant="ghost" size="sm" onclick={() => openEdit(timer)}>Edit</Button>
            {#if !isBuiltInMock(timer)}
              <Button
                variant="danger"
                size="sm"
                loading={removing === timer.id}
                onclick={() => remove(timer)}
              >
                Remove
              </Button>
            {/if}
          </div>
        </li>
      {/each}
    </ul>
    {@render listFooter?.()}
  {/if}
</div>

<!-- The add / edit dialog stacks above whatever embeds the manager. -->
<Dialog bind:open={formOpen} title={editing ? 'Edit timer' : 'Add timer'}>
  <form class="timer-form" onsubmit={submitForm} aria-label={editing ? 'Edit timer' : 'Add timer'}>
    <Field label="Name" error={formError}>
      <Input
        bind:value={formName}
        placeholder="e.g. Mock — fast"
        aria-label="Timer name"
        autocomplete="off"
      />
    </Field>

    <Field label="Kind">
      <Select bind:value={formKind} aria-label="Timer kind">
        <option value="Mock">Mock (synthetic)</option>
        <option value="Rotorhazard">RotorHazard</option>
      </Select>
    </Field>

    {#if formKind === 'Mock'}
      <div class="kind-grid">
        <Field label="Laps" hint="Laps each sim pilot flies.">
          <Input type="number" min="1" step="1" bind:value={formLaps} aria-label="Mock laps" />
        </Field>
        <Field label="Lap pace (ms)" hint="Nominal time for one sim lap.">
          <Input
            type="number"
            min="1"
            step="100"
            bind:value={formLapMs}
            aria-label="Mock lap pace"
          />
        </Field>
      </div>
    {:else}
      <Field label="URL" hint="Stored now; the live connection lands in a later slice (2b).">
        <Input
          type="url"
          bind:value={formUrl}
          placeholder="http://rotorhazard.local:5000"
          aria-label="RotorHazard URL"
          autocomplete="off"
        />
      </Field>
    {/if}
  </form>
  {#snippet footer()}
    <Button variant="ghost" onclick={() => (formOpen = false)} disabled={saving}>Cancel</Button>
    <Button variant="primary" onclick={submitForm} loading={saving} disabled={!formName.trim()}>
      {editing ? 'Save changes' : 'Add timer'}
    </Button>
  {/snippet}
</Dialog>

<style>
  .timer-manager {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
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
    animation: tm-spin 0.7s linear infinite;
  }
  @keyframes tm-spin {
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
    padding: var(--gf-space-5);
    border: 1px dashed var(--gf-border);
    border-radius: var(--gf-radius-lg);
    background: var(--gf-surface-alt);
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

  .timer-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .timer-row {
    display: flex;
    align-items: center;
    gap: var(--gf-space-4);
    padding: var(--gf-space-3) var(--gf-space-4);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    background: var(--gf-surface);
    transition:
      border-color var(--gf-motion-fast) var(--gf-ease-out),
      background var(--gf-motion-fast) var(--gf-ease-out);
  }
  /* When the manager is embedded in select mode, the chosen rows read as "on". */
  .timer-row.checked {
    border-color: var(--gf-accent);
    background: var(--gf-accent-soft);
  }
  .timer-main {
    display: flex;
    flex-direction: column;
    gap: 3px;
    flex: 1;
    min-width: 0;
  }
  .timer-head {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
  }
  .timer-name {
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
  }
  .timer-sub {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-muted);
  }
  .timer-actions {
    display: flex;
    gap: var(--gf-space-2);
    flex-shrink: 0;
  }

  .timer-form {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .kind-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--gf-space-3);
  }
</style>
