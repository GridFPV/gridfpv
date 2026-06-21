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
   * The add form offers an **"Add from MultiGP"** quick-pick that pre-fills `source = MultiGP` + a
   * reference URL for one of the seven standard MultiGP classes.
   *
   * The list refreshes after every create/edit/delete; the latest `classes` are exposed back through
   * the bindable `classes` prop and an `onchange` callback so a selection owner can reconcile.
   */
  import { Badge, Button, Dialog, Field, Input, Select, toast } from '@gridfpv/components';
  import type { Snippet } from 'svelte';
  import type { Class, CreateClassRequest, UpdateClassRequest } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import {
    CLASS_SOURCES,
    MULTIGP_CLASSES,
    buildCreateRequest,
    buildUpdateRequest,
    emptyForm,
    formFromClass,
    formFromMultiGp,
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
    rowChecked
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
  // which protocol call submit makes. The form holds plain strings + the source; on submit it maps
  // to a create request or a clear-via-null update diff (see ../lib/classes.ts).
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
    editing = cls;
    form = formFromClass(cls);
    formError = undefined;
    formOpen = true;
  }

  /** Quick-pick: pre-fill the (add) form from a MultiGP preset — `source = MultiGP` + reference. */
  function pickMultiGp(name: string) {
    const preset = MULTIGP_CLASSES.find((p) => p.name === name);
    if (preset) form = formFromMultiGp(preset);
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
  {:else if loadState.classes.length === 0}
    <div class="empty">
      <p>No classes in the directory yet.</p>
      <p class="empty-sub">Add one — or pick a standard MultiGP class — to get started.</p>
    </div>
  {:else}
    {@render listHeader?.()}
    <ul class="class-list" aria-label="Class directory">
      {#each loadState.classes as cls (cls.id)}
        <li class="class-row" class:checked={rowChecked?.(cls)}>
          {@render rowLead?.(cls)}
          <div class="class-main">
            <div class="class-head">
              <span class="class-name">{cls.name}</span>
              <Badge tone={sourceTone(cls.source)}>{cls.source}</Badge>
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
            <Button variant="ghost" size="sm" onclick={() => openEdit(cls)}>Edit</Button>
            <Button
              variant="danger"
              size="sm"
              loading={removing === cls.id}
              onclick={() => askRemove(cls)}
            >
              Remove
            </Button>
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
      <!-- The MultiGP quick-pick only makes sense on a fresh add; it pre-fills the form below. -->
      <Field label="Add from MultiGP" hint="Pre-fills a standard MultiGP class + its reference.">
        <Select
          value=""
          aria-label="Add from MultiGP"
          onchange={(e: Event) => pickMultiGp((e.currentTarget as HTMLSelectElement).value)}
        >
          <option value="" disabled selected>Choose a MultiGP class…</option>
          {#each MULTIGP_CLASSES as preset (preset.name)}
            <option value={preset.name}>{preset.name}</option>
          {/each}
        </Select>
      </Field>
    {/if}

    <Field label="Name" required error={formError}>
      <Input bind:value={form.name} placeholder="e.g. Open" aria-label="Name" autocomplete="off" />
    </Field>

    <Field label="Source" hint="Where this class came from.">
      <Select bind:value={form.source} aria-label="Source">
        {#each CLASS_SOURCES as src (src)}
          <option value={src}>{src}</option>
        {/each}
      </Select>
    </Field>

    <Field label="Reference" hint="A source id/handle or URL (e.g. a MultiGP class link).">
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
