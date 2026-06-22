<script lang="ts">
  /**
   * ClassManager — the shared class **directory management** piece (issue #84).
   *
   * The single home for class CRUD: it lists every directory class (name + a source badge +
   * reference link + description) and drives **add** / **edit** / **remove** (with confirm) through
   * one stacked {@link Dialog} — mirroring {@link PilotManager} exactly, including the
   * full-trust-first → lazy-token write flow on the {@link Session}. Ids are auto-generated
   * server-side, never shown or asked for.
   *
   * It is built **reusable** so the same CRUD lives in one place:
   *  - the {@link ClassesPage} embeds it for directory CRUD;
   *  - the in-event {@link EventClasses} embeds it and layers **per-event selection** on top, via the
   *    optional `rowLead` snippet (a checkbox per row), the `listHeader`/`listFooter` snippets (the
   *    selection count + Save), and `rowChecked` (the selected-row highlight) — the very seam
   *    {@link PilotManager} exposes for {@link EventRoster}. **Selection concerns live in that host,
   *    not here** — this component is purely the directory + form.
   *
   * New classes the RD creates are always **`Custom`** (the create form has no org source picker).
   * The canonical org classes ship as locked, fixed-id **built-ins** the Director seeds: each is
   * shown with its org badge + a small lock indicator and has **no Edit/Remove** — only Custom rows
   * are editable/removable.
   *
   * The list refreshes after every create/edit/delete; the latest `classes` are exposed back through
   * the bindable `classes` prop and an `onchange` callback so a selection owner can reconcile.
   */
  import { Badge, Button, Dialog, Field, Input, toast } from '@gridfpv/components';
  import type { Snippet } from 'svelte';
  import type { Class, CreateClassRequest, UpdateClassRequest } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import {
    buildCreateRequest,
    buildUpdateRequest,
    emptyForm,
    formFromClass,
    isBuiltin,
    isHidden,
    sourceTone,
    type ClassFormValues
  } from '../lib/classes.js';

  let {
    session,
    classes = $bindable([]),
    onchange,
    rowLead,
    listHeader,
    listFooter,
    rowChecked,
    manageHidden = false,
    filterHidden = false,
    keepRow
  }: {
    session: Session;
    /** The latest loaded directory, exposed so a selection owner can reconcile its working set. */
    classes?: Class[];
    /** Called after every create/edit/delete (and the reload it triggers) with the fresh list. */
    onchange?: (classes: Class[]) => void;
    /** Rendered at the **start** of each row (e.g. the in-event selection checkbox). */
    rowLead?: Snippet<[Class]>;
    /** Rendered **above** the list (reserved for selection chrome). */
    listHeader?: Snippet;
    /** Rendered **below** the list (e.g. the selection count + Save). */
    listFooter?: Snippet;
    /** Whether a row is currently selected — drives the row's "checked" highlight in select mode. */
    rowChecked?: (cls: Class) => boolean;
    /**
     * Whether to offer the **hide / unhide** affordance per row and surface hidden classes
     * (hide/archive classes). The main Classes view sets this `true` (manageable, marked); the
     * in-event picker leaves it `false`.
     */
    manageHidden?: boolean;
    /**
     * Whether to **filter out hidden classes** from the rendered list (hide/archive classes) — the
     * per-event picker sets this so the offered list excludes archived classes. A row the
     * {@link keepRow} predicate keeps (e.g. one already selected on the event) is shown even when
     * hidden, so a previously-selected class never silently vanishes.
     */
    filterHidden?: boolean;
    /** When {@link filterHidden} is set, keep a row that would otherwise be filtered (e.g. selected). */
    keepRow?: (cls: Class) => boolean;
  } = $props();

  type LoadState =
    | { kind: 'loading' }
    | { kind: 'error'; message: string }
    | { kind: 'ready'; classes: Class[] };

  let loadState = $state<LoadState>({ kind: 'loading' });

  export async function load() {
    loadState = { kind: 'loading' };
    try {
      const list = await session.listClasses();
      loadState = { kind: 'ready', classes: list };
      classes = list;
    } catch (e) {
      loadState = { kind: 'error', message: e instanceof Error ? e.message : String(e) };
    }
  }

  // (Re)load on mount, so the list always reflects the current directory.
  $effect(() => {
    void load();
  });

  /** Reload, then notify the parent so a selection owner can reconcile against the fresh set. */
  async function reload() {
    await load();
    if (loadState.kind === 'ready') onchange?.(loadState.classes);
  }

  // ── The add / edit dialog ──────────────────────────────────────────────────
  // One dialog drives both create and edit; `editing` (the class, or undefined for "add") chooses
  // which protocol call submit makes. The form holds plain strings; on submit it maps to a create
  // request (always source=Custom) or a clear-via-null update diff (see ../lib/classes.ts).
  let formOpen = $state(false);
  let editing = $state<Class | undefined>(undefined);
  let form = $state<ClassFormValues>(emptyForm());
  let saving = $state(false);
  let formError = $state<string | undefined>(undefined);

  export function openAdd() {
    editing = undefined;
    form = emptyForm();
    formError = undefined;
    formOpen = true;
  }

  function openEdit(cls: Class) {
    // Built-ins are read-only — never open the edit form for one (the UI offers no Edit on them).
    if (isBuiltin(cls)) return;
    editing = cls;
    form = formFromClass(cls);
    formError = undefined;
    formOpen = true;
  }

  async function submitForm(e?: Event) {
    e?.preventDefault();
    if (saving) return;
    if (!form.name.trim()) {
      formError = 'A name is required.';
      return;
    }
    saving = true;
    formError = undefined;
    try {
      if (editing) {
        const req: UpdateClassRequest = buildUpdateRequest(editing, form);
        const updated = await session.updateClass(editing.id, req);
        if (!updated) {
          formError = 'A control token is required to edit a class.';
          return;
        }
        toast.success(`Updated “${updated.name}”.`);
      } else {
        const req: CreateClassRequest = buildCreateRequest(form);
        const created = await session.createClass(req);
        if (!created) {
          formError = 'A control token is required to add a class.';
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

  // ── Remove (with a confirm step) ─────────────────────────────────────────────
  let confirming = $state<Class | undefined>(undefined);
  let removing = $state<string | undefined>(undefined);

  function askRemove(cls: Class) {
    // Built-ins are read-only — they can't be removed (the UI offers no Remove on them).
    if (isBuiltin(cls)) return;
    confirming = cls;
  }

  async function confirmRemove() {
    const cls = confirming;
    if (!cls || removing) return;
    removing = cls.id;
    try {
      const done = await session.deleteClass(cls.id);
      if (done === undefined) {
        toast.info('A control token is required to remove a class.');
        return;
      }
      toast.success(`Removed “${cls.name}”.`);
      confirming = undefined;
      await reload();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : String(err));
    } finally {
      removing = undefined;
    }
  }

  // ── Hide / un-hide (hide/archive classes) ────────────────────────────────────
  // A visibility toggle, valid for built-ins too (hiding is a preference, not an edit). Hidden
  // classes stay listed here (marked) so the RD can un-hide them; the per-event picker filters them.
  let togglingHidden = $state<string | undefined>(undefined);
  async function setHidden(cls: Class, hidden: boolean) {
    if (togglingHidden) return;
    togglingHidden = cls.id;
    try {
      const updated = await session.setClassHidden(cls.id, hidden);
      if (updated === undefined) {
        toast.info('A control token is required to change class visibility.');
        return;
      }
      toast.success(hidden ? `Hid “${cls.name}”.` : `Unhid “${cls.name}”.`);
      await reload();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : String(err));
    } finally {
      togglingHidden = undefined;
    }
  }

  // The rows actually rendered. In the per-event picker (`filterHidden`) hidden classes drop out,
  // except a row `keepRow` keeps (e.g. one already selected on the event), so a selected class never
  // silently disappears. Everywhere else (the main view) all classes show; hidden ones are marked.
  const renderedClasses = $derived(
    loadState.kind === 'ready'
      ? filterHidden
        ? loadState.classes.filter((c) => !isHidden(c) || (keepRow?.(c) ?? false))
        : loadState.classes
      : []
  );
</script>

<div class="class-manager">
  {#if loadState.kind === 'loading'}
    <div class="state-msg" role="status">
      <span class="spinner" aria-hidden="true"></span>
      Loading classes…
    </div>
  {:else if loadState.kind === 'error'}
    <div class="state-error">
      <p>Couldn't load the classes.</p>
      <code>{loadState.message}</code>
      <Button variant="secondary" onclick={load}>Try again</Button>
    </div>
  {:else if renderedClasses.length === 0}
    <div class="empty">
      {#if loadState.classes.length > 0 && filterHidden}
        <p>No classes available to select.</p>
        <p class="empty-sub">Every class is hidden — unhide one in the Classes page to offer it.</p>
      {:else}
        <p>No classes in the directory yet.</p>
        <p class="empty-sub">Add a custom class to get started.</p>
      {/if}
    </div>
  {:else}
    {@render listHeader?.()}
    <ul class="class-list" aria-label="Class directory">
      {#each renderedClasses as cls (cls.id)}
        <li class="class-row" class:checked={rowChecked?.(cls)} class:hidden-row={isHidden(cls)}>
          {@render rowLead?.(cls)}
          <div class="class-main">
            <div class="class-head">
              <span class="class-name">{cls.name}</span>
              <Badge tone={sourceTone(cls.source)}>{cls.source}</Badge>
              {#if isBuiltin(cls)}
                <span class="builtin-lock" title="Built-in class — read-only" aria-label="Built-in">
                  🔒 Built-in
                </span>
              {/if}
              {#if isHidden(cls)}
                <Badge tone="neutral">Hidden</Badge>
              {/if}
              {#if cls.reference}
                <a
                  class="reference"
                  href={cls.reference}
                  target="_blank"
                  rel="noreferrer noopener"
                  aria-label={`Reference for ${cls.name}`}
                >
                  Reference ↗
                </a>
              {/if}
            </div>
            {#if cls.description}<span class="description">{cls.description}</span>{/if}
          </div>
          <div class="class-actions">
            {#if manageHidden}
              <Button
                variant="ghost"
                size="sm"
                loading={togglingHidden === cls.id}
                onclick={() => setHidden(cls, !isHidden(cls))}
              >
                {isHidden(cls) ? 'Unhide' : 'Hide'}
              </Button>
            {/if}
            {#if !isBuiltin(cls)}
              <Button variant="ghost" size="sm" onclick={() => openEdit(cls)}>Edit</Button>
              <Button
                variant="danger"
                size="sm"
                loading={removing === cls.id}
                onclick={() => askRemove(cls)}
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
<Dialog bind:open={formOpen} title={editing ? 'Edit class' : 'Add class'}>
  <form class="class-form" onsubmit={submitForm} aria-label={editing ? 'Edit class' : 'Add class'}>
    {#if !editing}
      <!-- New classes are always Custom; the canonical org classes ship as locked built-ins. -->
      <p class="custom-note">
        New classes are <strong>Custom</strong>. The standard org classes (MultiGP, Five33, …) are
        built in.
      </p>
    {/if}

    <Field label="Name" required error={formError}>
      <Input
        bind:value={form.name}
        placeholder="e.g. House Spec"
        aria-label="Name"
        autocomplete="off"
      />
    </Field>

    <Field label="Reference" hint="An optional source id/handle or URL.">
      <Input bind:value={form.reference} aria-label="Reference" autocomplete="off" />
    </Field>

    <Field label="Description" hint="Optional notes for this class.">
      <textarea
        class="description-input"
        bind:value={form.description}
        aria-label="Description"
        rows="3"
      ></textarea>
    </Field>
  </form>
  {#snippet footer()}
    <Button variant="ghost" onclick={() => (formOpen = false)} disabled={saving}>Cancel</Button>
    <Button variant="primary" onclick={submitForm} loading={saving} disabled={!form.name.trim()}>
      {editing ? 'Save changes' : 'Add class'}
    </Button>
  {/snippet}
</Dialog>

<!-- Remove confirmation. -->
<Dialog
  open={confirming !== undefined}
  onclose={() => (confirming = undefined)}
  title="Remove class"
>
  <p class="confirm-text">
    Remove <strong>{confirming?.name}</strong> from the directory? Any event that selected it keeps running;
    this only removes the directory entry.
  </p>
  {#snippet footer()}
    <Button
      variant="ghost"
      onclick={() => (confirming = undefined)}
      disabled={removing !== undefined}
    >
      Cancel
    </Button>
    <Button variant="danger" onclick={confirmRemove} loading={removing !== undefined}>Remove</Button
    >
  {/snippet}
</Dialog>

<style>
  .class-manager {
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
    animation: cm-spin 0.7s linear infinite;
  }
  @keyframes cm-spin {
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

  .class-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .class-row {
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
  .class-row.checked {
    border-color: var(--gf-accent);
    background: var(--gf-accent-soft);
  }
  /* A hidden/archived class reads as muted (greyed) while staying manageable here. */
  .class-row.hidden-row {
    opacity: 0.6;
    background: var(--gf-surface-alt);
  }
  .class-row.hidden-row .class-name {
    color: var(--gf-text-muted);
  }
  .class-main {
    display: flex;
    flex-direction: column;
    gap: 3px;
    flex: 1;
    min-width: 0;
  }
  .class-head {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
  }
  .class-name {
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
  }
  .builtin-lock {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-muted);
    font-weight: var(--gf-font-weight-medium);
    letter-spacing: var(--gf-tracking-tight);
  }
  .custom-note {
    margin: 0;
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-muted);
    line-height: 1.5;
  }
  .custom-note strong {
    color: var(--gf-text-secondary);
  }
  .reference {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-accent);
    text-decoration: none;
  }
  .reference:hover {
    text-decoration: underline;
  }
  .description {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-muted);
  }
  .class-actions {
    display: flex;
    gap: var(--gf-space-2);
    flex-shrink: 0;
  }

  .class-form {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .description-input {
    width: 100%;
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    resize: vertical;
  }
  .description-input:focus {
    outline: none;
    border-color: var(--gf-accent);
    box-shadow: var(--gf-focus-ring);
  }

  .confirm-text {
    margin: 0;
    color: var(--gf-text-secondary);
    font-size: var(--gf-font-size-sm);
    line-height: 1.5;
  }
</style>
