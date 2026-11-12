<script lang="ts">
  /**
   * FormatDialog — make or change one saved format.
   *
   * The form is a draft (`lib/formats.ts`): nothing is saved until Save, and a field that
   * does not apply to the kind of race chosen is not shown. The parent mounts this only
   * while it is open. A refusal from the Director (a name another format has) is shown here
   * and the dialog stays open with everything typed.
   */
  import { Banner, Button, Dialog, Field, Input, Select } from '@gridfpv/components';
  import type { Format } from '@gridfpv/types';
  import {
    RANK_CHOICES,
    formatFromDraft,
    formatSummary,
    type FormatDraft
  } from '../lib/formats.js';

  let {
    title,
    start,
    onsave,
    onclose
  }: {
    /** "New format", or "Edit ‹name›". */
    title: string;
    /** The draft the form opens with. */
    start: FormatDraft;
    /** Save the format. Throws with the Director's sentence when it is refused. */
    onsave: (format: Format) => Promise<void>;
    onclose: () => void;
  } = $props();

  // The form edits its own copy, so Cancel leaves the caller's draft alone.
  // svelte-ignore state_referenced_locally
  let draft = $state<FormatDraft>({ ...start });
  let busy = $state(false);
  let error = $state<string | undefined>(undefined);
  const result = $derived(formatFromDraft(draft));

  async function submit(e?: Event) {
    e?.preventDefault();
    if (busy) return;
    if (!result.ok) {
      error = result.problem;
      return;
    }
    busy = true;
    error = undefined;
    try {
      await onsave(result.format);
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog open={true} {title} {onclose}>
  <form class="form" aria-label={title} onsubmit={submit}>
    <Field label="Name" required>
      <Input
        bind:value={draft.name}
        placeholder="e.g. 2 minute qualifying"
        aria-label="Format name"
        autocomplete="off"
        disabled={busy}
      />
    </Field>

    <fieldset>
      <legend>One race</legend>
      <Field label="The race ends">
        <Select bind:value={draft.end} aria-label="The race ends" disabled={busy}>
          <option value="Open">When you stop it</option>
          <option value="Laps">When a pilot finishes a set number of laps</option>
          <option value="Timed">After a set time</option>
        </Select>
      </Field>
      {#if draft.end === 'Laps'}
        <Field label="Laps to win">
          <Input
            type="number"
            inputmode="numeric"
            min="1"
            step="1"
            aria-label="Laps to win"
            bind:value={draft.laps}
            disabled={busy}
          />
        </Field>
        <Field
          label="Stop by itself after everyone finishes (seconds)"
          hint="Leave empty and you stop the race yourself. A wait gives time to sort out laps on a hard track."
        >
          <Input
            type="number"
            inputmode="decimal"
            min="0"
            step="1"
            aria-label="Seconds to wait after everyone finishes"
            bind:value={draft.autoStopSeconds}
            disabled={busy}
          />
        </Field>
      {:else if draft.end === 'Timed'}
        <div class="pair">
          <Field label="Race time (seconds)">
            <Input
              type="number"
              inputmode="numeric"
              min="1"
              step="1"
              aria-label="Race time, in seconds"
              bind:value={draft.timedSeconds}
              disabled={busy}
            />
          </Field>
          <Field label="Then to finish the lap (seconds)">
            <Input
              type="number"
              inputmode="numeric"
              min="0"
              step="1"
              aria-label="Seconds to finish the lap"
              bind:value={draft.graceSeconds}
              disabled={busy}
            />
          </Field>
        </div>
      {/if}
      {#if draft.end !== 'Timed'}
        <Field
          label="Time limit (seconds)"
          hint="The race stops by itself here, whoever has finished. Leave empty for no limit."
        >
          <Input
            type="number"
            inputmode="numeric"
            min="1"
            step="1"
            aria-label="Time limit, in seconds"
            bind:value={draft.limitSeconds}
            disabled={busy}
          />
        </Field>
      {/if}
      <Field label="Countdown before the start tone (seconds)" hint="0 starts the race at once.">
        <Input
          type="number"
          inputmode="decimal"
          min="0"
          step="1"
          aria-label="Countdown, in seconds"
          bind:value={draft.stagingSeconds}
          disabled={busy}
        />
      </Field>
      <label class="check">
        <input type="checkbox" bind:checked={draft.fromTone} disabled={busy} />
        <span>
          Time lap 1 from the start tone
          <small>Left off, a pilot's first crossing starts lap 1 (the hole shot).</small>
        </span>
      </label>
    </fieldset>

    <fieldset>
      <legend>The heat</legend>
      <Field label="The heat is done">
        <Select bind:value={draft.done} aria-label="The heat is done" disabled={busy}>
          <option value="Director">When you call it done</option>
          <option value="Races">After a set number of races</option>
          <option value="Wins">When a pilot has a set number of wins</option>
        </Select>
      </Field>
      {#if draft.done !== 'Director'}
        <Field label={draft.done === 'Races' ? 'Races' : 'Wins'}>
          <Input
            type="number"
            inputmode="numeric"
            min="1"
            step="1"
            aria-label={draft.done === 'Races' ? 'Number of races' : 'Number of wins'}
            bind:value={draft.doneCount}
            disabled={busy}
          />
        </Field>
      {/if}
      <Field label="Ranked by">
        <Select bind:value={draft.rank} aria-label="Ranked by" disabled={busy}>
          {#each RANK_CHOICES as choice (choice.key)}
            <option value={choice.key}>{choice.label}</option>
          {/each}
          {#if draft.customSort}
            <option value="custom">Its own rule (kept as it is)</option>
          {/if}
        </Select>
      </Field>
      {#if draft.rank === 'consecutive'}
        <Field label="Laps in a row">
          <Input
            type="number"
            inputmode="numeric"
            min="1"
            step="1"
            aria-label="Laps in a row"
            bind:value={draft.consecutive}
            disabled={busy}
          />
        </Field>
      {:else if draft.rank === 'points'}
        <Field
          label="Points by place"
          hint="First place first, like 10, 8, 6, 4. A place past the end scores 0."
        >
          <Input
            bind:value={draft.points}
            placeholder="10, 8, 6, 4"
            aria-label="Points by place"
            autocomplete="off"
            disabled={busy}
          />
        </Field>
      {/if}
    </fieldset>

    <details class="more">
      <summary>More</summary>
      <Field
        label="Minimum lap time for this format (seconds)"
        hint="Leave empty to use the event's, set on the Event page. That is right for almost every format."
      >
        <Input
          type="number"
          inputmode="decimal"
          min="0"
          step="0.1"
          aria-label="Minimum lap time for this format, in seconds"
          bind:value={draft.minLapSeconds}
          disabled={busy}
        />
      </Field>
    </details>

    <p class="reads" data-testid="format-reads">
      {#if result.ok}{formatSummary(result.format)}{/if}
    </p>
    {#if error}
      <Banner tone="danger">{error}</Banner>
    {/if}
  </form>
  {#snippet footer()}
    <Button variant="ghost" onclick={onclose} disabled={busy}>Cancel</Button>
    <Button variant="primary" onclick={() => submit()} disabled={busy}>
      {busy ? 'Saving…' : 'Save format'}
    </Button>
  {/snippet}
</Dialog>

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  fieldset {
    margin: 0;
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  legend {
    padding: 0 var(--gf-space-2);
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .pair {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--gf-space-3);
  }
  .check {
    display: flex;
    align-items: flex-start;
    gap: var(--gf-space-2);
    font-size: var(--gf-font-size-sm);
  }
  .check small {
    display: block;
    color: var(--gf-text-muted);
  }
  .more summary {
    cursor: pointer;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-secondary);
    margin-bottom: var(--gf-space-3);
  }
  .reads {
    margin: 0;
    min-height: 1.2em;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
</style>
