<script lang="ts">
  /**
   * PilotManager — the shared pilot **directory management** piece (issue #74).
   *
   * The single home for pilot CRUD: it lists every directory pilot (callsign + an optional flag,
   * team, color swatch, and VTX badge) and drives **add** / **edit** / **remove** (with confirm)
   * through one stacked {@link Dialog} — mirroring {@link TimerManager} exactly, including the
   * full-trust-first → lazy-token write flow on the {@link Session}. Ids are auto-generated
   * server-side, never shown or asked for.
   *
   * It is built **reusable** so the same CRUD lives in one place:
   *  - the {@link PilotsPage} embeds it for directory CRUD now;
   *  - the next slice's **in-event Roster** embeds it and layers **per-event selection** on top, via
   *    the optional `rowLead` snippet (a checkbox per row), the `listHeader`/`listFooter` snippets
   *    (the selection count + Save), and `rowChecked` (the selected-row highlight) — the very seam
   *    {@link TimerManager} exposes for {@link EventTimers}. **Selection/roster concerns live in that
   *    host, not here** — this component is purely the directory + form.
   *
   * The list refreshes after every create/edit/delete; the latest `pilots` are exposed back through
   * the bindable `pilots` prop and an `onchange` callback so a selection owner can reconcile.
   */
  import { Badge, Button, Dialog, Field, Input, Select, toast } from '@gridfpv/components';
  import type { Snippet } from 'svelte';
  import type { CreatePilotRequest, Pilot, UpdatePilotRequest } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import {
    VTX_TYPES,
    buildCreateRequest,
    buildUpdateRequest,
    emptyForm,
    formFromPilot,
    vtxTone,
    type PilotFormValues
  } from '../lib/pilots.js';
  import { COUNTRY_NAME } from '../lib/countries.js';
  import { flagSvg } from '../lib/flags.js';
  import CountrySelect from './CountrySelect.svelte';

  let {
    session,
    pilots = $bindable([]),
    onchange,
    rowLead,
    listHeader,
    listFooter,
    rowChecked
  }: {
    session: Session;
    /** The latest loaded directory, exposed so a selection owner can reconcile its working set. */
    pilots?: Pilot[];
    /** Called after every create/edit/delete (and the reload it triggers) with the fresh list. */
    onchange?: (pilots: Pilot[]) => void;
    /** Rendered at the **start** of each row (e.g. the in-event roster checkbox — the next slice). */
    rowLead?: Snippet<[Pilot]>;
    /** Rendered **above** the list (reserved for selection chrome). */
    listHeader?: Snippet;
    /** Rendered **below** the list (e.g. the selection count + Save). */
    listFooter?: Snippet;
    /** Whether a row is currently selected — drives the row's "checked" highlight in select mode. */
    rowChecked?: (pilot: Pilot) => boolean;
  } = $props();

  type LoadState =
    | { kind: 'loading' }
    | { kind: 'error'; message: string }
    | { kind: 'ready'; pilots: Pilot[] };

  let loadState = $state<LoadState>({ kind: 'loading' });

  export async function load() {
    loadState = { kind: 'loading' };
    try {
      const list = await session.listPilots();
      loadState = { kind: 'ready', pilots: list };
      pilots = list;
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
    if (loadState.kind === 'ready') onchange?.(loadState.pilots);
  }

  // ── The add / edit dialog ──────────────────────────────────────────────────
  // One dialog drives both create and edit; `editing` (the pilot, or undefined for "add") chooses
  // which protocol call submit makes. The form holds plain strings + the attributes bag; on submit
  // it maps to a create request or a clear-via-null update diff (see ../lib/pilots.ts).
  let formOpen = $state(false);
  let editing = $state<Pilot | undefined>(undefined);
  let form = $state<PilotFormValues>(emptyForm());
  let saving = $state(false);
  let formError = $state<string | undefined>(undefined);
  // The custom-attributes editor works on an ordered array of [key, value] rows so blank/duplicate
  // keys are editable; it is folded back into `form.attributes` on every change.
  let attrRows = $state<Array<{ key: string; value: string }>>([]);

  function syncAttributes() {
    const bag: Record<string, string> = {};
    for (const r of attrRows) {
      const k = r.key.trim();
      if (k) bag[k] = r.value;
    }
    form.attributes = bag;
  }

  function rowsFromBag(bag: Record<string, string>): Array<{ key: string; value: string }> {
    return Object.entries(bag).map(([key, value]) => ({ key, value }));
  }

  export function openAdd() {
    editing = undefined;
    form = emptyForm();
    attrRows = [];
    formError = undefined;
    formOpen = true;
  }

  function openEdit(pilot: Pilot) {
    editing = pilot;
    form = formFromPilot(pilot);
    attrRows = rowsFromBag(form.attributes);
    formError = undefined;
    formOpen = true;
  }

  function addAttrRow() {
    attrRows = [...attrRows, { key: '', value: '' }];
  }
  function removeAttrRow(i: number) {
    attrRows = attrRows.filter((_, idx) => idx !== i);
    syncAttributes();
  }

  async function submitForm(e?: Event) {
    e?.preventDefault();
    if (saving) return;
    if (!form.callsign.trim()) {
      formError = 'A callsign is required.';
      return;
    }
    syncAttributes();
    saving = true;
    formError = undefined;
    try {
      if (editing) {
        const req: UpdatePilotRequest = buildUpdateRequest(editing, form);
        const updated = await session.updatePilot(editing.id, req);
        if (!updated) {
          formError = 'A control token is required to edit a pilot.';
          return;
        }
        toast.success(`Updated “${updated.callsign}”.`);
      } else {
        const req: CreatePilotRequest = buildCreateRequest(form);
        const created = await session.createPilot(req);
        if (!created) {
          formError = 'A control token is required to add a pilot.';
          return;
        }
        toast.success(`Added “${created.callsign}”.`);
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
  let confirming = $state<Pilot | undefined>(undefined);
  let removing = $state<string | undefined>(undefined);

  function askRemove(pilot: Pilot) {
    confirming = pilot;
  }

  async function confirmRemove() {
    const pilot = confirming;
    if (!pilot || removing) return;
    removing = pilot.id;
    try {
      const done = await session.deletePilot(pilot.id);
      if (done === undefined) {
        toast.info('A control token is required to remove a pilot.');
        return;
      }
      toast.success(`Removed “${pilot.callsign}”.`);
      confirming = undefined;
      await reload();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : String(err));
    } finally {
      removing = undefined;
    }
  }

  function countryName(code: string): string {
    return COUNTRY_NAME[code.toUpperCase()] ?? code;
  }
</script>

<div class="pilot-manager">
  {#if loadState.kind === 'loading'}
    <div class="state-msg" role="status">
      <span class="spinner" aria-hidden="true"></span>
      Loading pilots…
    </div>
  {:else if loadState.kind === 'error'}
    <div class="state-error">
      <p>Couldn't load the pilots.</p>
      <code>{loadState.message}</code>
      <Button variant="secondary" onclick={load}>Try again</Button>
    </div>
  {:else if loadState.pilots.length === 0}
    <div class="empty">
      <p>No pilots in the directory yet.</p>
      <p class="empty-sub">Add one to start building your roster.</p>
    </div>
  {:else}
    {@render listHeader?.()}
    <ul class="pilot-list" aria-label="Pilot directory">
      {#each loadState.pilots as pilot (pilot.id)}
        <li class="pilot-row" class:checked={rowChecked?.(pilot)}>
          {@render rowLead?.(pilot)}
          {#if pilot.color}
            <span class="color-swatch" style:background={pilot.color} aria-hidden="true"></span>
          {/if}
          <div class="pilot-main">
            <div class="pilot-head">
              {#if pilot.country && flagSvg(pilot.country)}
                <!-- Static SVG flag string from `country-flag-icons`, keyed by the stored alpha-2
                     code — no user input, no scripting. -->
                <span
                  class="flag"
                  title={countryName(pilot.country)}
                  aria-label={countryName(pilot.country)}
                >
                  <!-- eslint-disable-next-line svelte/no-at-html-tags -->
                  {@html flagSvg(pilot.country)}
                </span>
              {/if}
              <span class="callsign">{pilot.callsign}</span>
              {#if pilot.team}<Badge tone="neutral">{pilot.team}</Badge>{/if}
              {#if pilot.vtx_type}<Badge tone={vtxTone(pilot.vtx_type)}>{pilot.vtx_type}</Badge
                >{/if}
            </div>
            {#if pilot.name}<span class="real-name">{pilot.name}</span>{/if}
          </div>
          <div class="pilot-actions">
            <Button variant="ghost" size="sm" onclick={() => openEdit(pilot)}>Edit</Button>
            <Button
              variant="danger"
              size="sm"
              loading={removing === pilot.id}
              onclick={() => askRemove(pilot)}
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
<Dialog bind:open={formOpen} title={editing ? 'Edit pilot' : 'Add pilot'}>
  <form class="pilot-form" onsubmit={submitForm} aria-label={editing ? 'Edit pilot' : 'Add pilot'}>
    <Field label="Callsign" required error={formError}>
      <Input
        bind:value={form.callsign}
        placeholder="e.g. Acro"
        aria-label="Callsign"
        autocomplete="off"
      />
    </Field>

    <div class="form-grid">
      <Field label="Real name">
        <Input bind:value={form.name} aria-label="Real name" autocomplete="off" />
      </Field>
      <Field label="Phonetic" hint="Pronunciation hint for callouts.">
        <Input bind:value={form.phonetic} aria-label="Phonetic" autocomplete="off" />
      </Field>
    </div>

    <div class="form-grid">
      <Field label="Team">
        <Input bind:value={form.team} aria-label="Team" autocomplete="off" />
      </Field>
      <Field label="VTX type">
        <Select bind:value={form.vtx} aria-label="VTX type">
          <option value="">None</option>
          {#each VTX_TYPES as vtx (vtx)}
            <option value={vtx}>{vtx}</option>
          {/each}
        </Select>
      </Field>
    </div>

    <div class="form-grid">
      <Field label="Color" hint="For overlays & leaderboards.">
        <div class="color-control">
          <input
            type="color"
            class="color-input"
            aria-label="Color"
            value={form.color || '#888888'}
            oninput={(e) => (form.color = (e.currentTarget as HTMLInputElement).value)}
          />
          <span class="color-hex">{form.color || '—'}</span>
          {#if form.color}
            <Button variant="ghost" size="sm" onclick={() => (form.color = '')}>Clear</Button>
          {/if}
        </div>
      </Field>
      <Field label="Country">
        <CountrySelect bind:value={form.country} />
      </Field>
    </div>

    <div class="form-grid">
      <Field label="MultiGP ID" hint="For a future cloud import.">
        <Input bind:value={form.multigp_id} aria-label="MultiGP ID" autocomplete="off" />
      </Field>
      <Field label="Velocidrone ID" hint="For a future cloud import.">
        <Input bind:value={form.velocidrone_id} aria-label="Velocidrone ID" autocomplete="off" />
      </Field>
    </div>

    <fieldset class="attrs">
      <legend>Custom attributes</legend>
      <p class="attrs-hint">Anything else — insurance #, FAA/FCC license, bib, sponsor…</p>
      {#if attrRows.length > 0}
        <ul class="attr-list" aria-label="Custom attributes">
          {#each attrRows as row, i (i)}
            <li class="attr-row">
              <Input
                bind:value={row.key}
                placeholder="Key"
                aria-label={`Attribute ${i + 1} key`}
                size="sm"
                autocomplete="off"
                oninput={syncAttributes}
              />
              <Input
                bind:value={row.value}
                placeholder="Value"
                aria-label={`Attribute ${i + 1} value`}
                size="sm"
                autocomplete="off"
                oninput={syncAttributes}
              />
              <Button
                variant="ghost"
                size="sm"
                onclick={() => removeAttrRow(i)}
                aria-label={`Remove attribute ${i + 1}`}>Remove</Button
              >
            </li>
          {/each}
        </ul>
      {/if}
      <Button variant="secondary" size="sm" onclick={addAttrRow}>+ Add attribute</Button>
    </fieldset>
  </form>
  {#snippet footer()}
    <Button variant="ghost" onclick={() => (formOpen = false)} disabled={saving}>Cancel</Button>
    <Button
      variant="primary"
      onclick={submitForm}
      loading={saving}
      disabled={!form.callsign.trim()}
    >
      {editing ? 'Save changes' : 'Add pilot'}
    </Button>
  {/snippet}
</Dialog>

<!-- Remove confirmation. -->
<Dialog
  open={confirming !== undefined}
  onclose={() => (confirming = undefined)}
  title="Remove pilot"
>
  <p class="confirm-text">
    Remove <strong>{confirming?.callsign}</strong> from the directory? Any event that rostered them keeps
    running; this only removes the directory entry.
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
  .pilot-manager {
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
    animation: pm-spin 0.7s linear infinite;
  }
  @keyframes pm-spin {
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

  .pilot-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .pilot-row {
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
  .pilot-row.checked {
    border-color: var(--gf-accent);
    background: var(--gf-accent-soft);
  }
  .color-swatch {
    width: 0.9rem;
    height: 0.9rem;
    border-radius: var(--gf-radius-pill);
    flex-shrink: 0;
    box-shadow: 0 0 0 1px var(--gf-border-strong);
  }
  .pilot-main {
    display: flex;
    flex-direction: column;
    gap: 3px;
    flex: 1;
    min-width: 0;
  }
  .pilot-head {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
  }
  .flag {
    display: inline-flex;
    width: 1.4rem;
    border-radius: 2px;
    overflow: hidden;
    box-shadow: 0 0 0 1px var(--gf-border-subtle);
  }
  .flag :global(svg) {
    display: block;
    width: 100%;
    height: auto;
  }
  .callsign {
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
  }
  .real-name {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-muted);
  }
  .pilot-actions {
    display: flex;
    gap: var(--gf-space-2);
    flex-shrink: 0;
  }

  .pilot-form {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .form-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--gf-space-3);
  }

  .color-control {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
  }
  .color-input {
    width: 2.5rem;
    height: 2.25rem;
    padding: 2px;
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    cursor: pointer;
  }
  .color-hex {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
    font-variant-numeric: tabular-nums;
    min-width: 4.5rem;
  }

  .attrs {
    margin: 0;
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-md);
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .attrs legend {
    font-size: var(--gf-font-size-xs);
    font-weight: var(--gf-font-weight-semibold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
    padding: 0 var(--gf-space-1);
  }
  .attrs-hint {
    margin: 0;
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-faint);
  }
  .attr-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .attr-row {
    display: grid;
    grid-template-columns: 1fr 1fr auto;
    gap: var(--gf-space-2);
    align-items: center;
  }

  .confirm-text {
    margin: 0;
    color: var(--gf-text-secondary);
    font-size: var(--gf-font-size-sm);
    line-height: 1.5;
  }
</style>
