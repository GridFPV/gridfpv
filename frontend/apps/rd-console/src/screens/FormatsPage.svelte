<script lang="ts">
  /**
   * FormatsPage — the saved formats: how one race is run and how a heat is ranked.
   *
   * A table, one row per format, saying what it does in words. A format is set on a phase or
   * a heat on the Phases page, which copies it into the event. So changing or deleting a
   * saved format here never changes a phase that already uses it.
   *
   * New and Duplicate open a draft; nothing is saved until Save. Formats outlive events.
   */
  import { Banner, Button, Card, toast } from '@gridfpv/components';
  import type { Format, SavedFormat } from '@gridfpv/types';
  import Page from '../Page.svelte';
  import ConfirmButton from '../lib/ConfirmButton.svelte';
  import {
    blankDraft,
    completionWords,
    copyName,
    draftFromFormat,
    raceWords,
    rankingWords,
    startWords,
    type FormatDraft
  } from '../lib/formats.js';
  import type { Session } from '../lib/session.svelte.js';
  import FormatDialog from './FormatDialog.svelte';

  let { session }: { session: Session } = $props();

  const say = (e: unknown): string => (e instanceof Error ? e.message : String(e));

  let formats = $state.raw<SavedFormat[] | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);
  let removing = $state<string | undefined>(undefined);

  async function load() {
    try {
      formats = await session.listFormats();
      loadError = undefined;
    } catch (e) {
      loadError = say(e);
    }
  }
  $effect(() => {
    void load();
  });

  /** The dialog, when open: the draft it starts from, and the saved format it changes. */
  let editing = $state<{ title: string; start: FormatDraft; id?: string } | undefined>(undefined);

  function openNew() {
    editing = { title: 'New format', start: blankDraft() };
  }
  function openEdit(saved: SavedFormat) {
    editing = {
      title: `Edit ${saved.format.name}`,
      start: draftFromFormat(saved.format),
      id: saved.id
    };
  }
  function openDuplicate(saved: SavedFormat) {
    const start = draftFromFormat(saved.format);
    start.name = copyName(
      saved.format.name,
      (formats ?? []).map((other) => other.format.name)
    );
    editing = { title: 'New format', start };
  }

  /** Save the dialog's format. A refusal is thrown back to the dialog, which shows it. */
  async function save(format: Format) {
    const target = editing;
    if (!target) return;
    const saved =
      target.id === undefined
        ? await session.createFormat(format)
        : await session.updateFormat(target.id, format);
    if (!saved) return; // the token prompt was cancelled; the dialog stays open
    editing = undefined;
    toast.success(`Saved “${saved.format.name}”.`);
    await load();
  }

  async function remove(saved: SavedFormat) {
    if (removing) return;
    removing = saved.id;
    try {
      if (await session.deleteFormat(saved.id)) {
        toast.success(`Deleted “${saved.format.name}”.`);
        await load();
      }
    } catch (e) {
      toast.error(say(e));
    } finally {
      removing = undefined;
    }
  }
</script>

<Page
  title="Formats"
  help="formats"
  lede="How one race is run and how a heat is ranked. A phase or a heat is given a format on the Phases page. Formats carry over between events."
>
  {#snippet actions()}
    <Button variant="primary" onclick={openNew}>+ New format</Button>
  {/snippet}

  {#if loadError}
    <Banner tone="danger" title="The formats could not be read.">{loadError}</Banner>
  {/if}

  <Card elevation="sm">
    {#if formats === undefined}
      {#if !loadError}<p class="muted" role="status">Loading…</p>{/if}
    {:else if formats.length === 0}
      <p class="muted" data-testid="no-formats">
        No format is saved. Add one to give a heat its rules.
      </p>
    {:else}
      <div class="scroll">
        <table aria-label="Formats">
          <thead>
            <tr>
              <th scope="col">Format</th>
              <th scope="col">One race</th>
              <th scope="col">Start</th>
              <th scope="col">Ranking</th>
              <th scope="col">The heat</th>
              <th scope="col"><span class="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            {#each formats as saved (saved.id)}
              {@const format = saved.format}
              <tr>
                <th scope="row">{format.name}</th>
                <td>{raceWords(format)}</td>
                <td>{startWords(format)}</td>
                <td>{rankingWords(format)}</td>
                <td>{completionWords(format)}</td>
                <td class="row-actions">
                  <Button
                    size="sm"
                    variant="secondary"
                    aria-label="Edit {format.name}"
                    onclick={() => openEdit(saved)}>Edit</Button
                  >
                  <Button
                    size="sm"
                    variant="ghost"
                    aria-label="Duplicate {format.name}"
                    onclick={() => openDuplicate(saved)}>Duplicate</Button
                  >
                  <ConfirmButton
                    size="sm"
                    variant="danger"
                    disabled={removing === saved.id}
                    title="Delete {format.name}"
                    onconfirm={() => remove(saved)}>Delete</ConfirmButton
                  >
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </Card>
</Page>

{#if editing}
  {#key editing}
    <FormatDialog
      title={editing.title}
      start={editing.start}
      onsave={save}
      onclose={() => (editing = undefined)}
    />
  {/key}
{/if}

<style>
  .muted {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .scroll {
    overflow-x: auto;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--gf-font-size-sm);
  }
  th,
  td {
    padding: var(--gf-space-2) var(--gf-space-3);
    text-align: left;
    vertical-align: top;
    border-bottom: 1px solid var(--gf-border-subtle);
  }
  thead th {
    font-size: var(--gf-font-size-xs);
    font-weight: var(--gf-font-weight-semibold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
    white-space: nowrap;
  }
  tbody tr:last-child th,
  tbody tr:last-child td {
    border-bottom: none;
  }
  tbody th {
    font-weight: var(--gf-font-weight-semibold);
    font-size: var(--gf-font-size-md);
    white-space: nowrap;
  }
  td {
    color: var(--gf-text-secondary);
  }
  .row-actions {
    white-space: nowrap;
    text-align: right;
  }
  .row-actions :global(.gf-btn) {
    margin-left: var(--gf-space-1);
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }
</style>
