<script lang="ts">
  /**
   * EditPhaseDialog — change a phase or a heat that is already in the event
   * (docs/primitives.md §12.4): its name, its format, and the phase it sits under.
   *
   * The name can always be changed. The format and the place can be changed until a
   * ranked race beneath it has been saved; after that the dialog says why not and offers
   * the name alone. A phase Grid made (a bracket, mains, a round robin) keeps its shape.
   *
   * It sends one `PhaseDefined` fact for the same phase. If the Director refuses, its
   * sentence is shown here and nothing is changed.
   *
   * The parent mounts this only while it is open.
   */
  import { Banner, Button, Dialog, Field, Input, Select } from '@gridfpv/components';
  import type { PhaseDef, PhaseView } from '@gridfpv/types';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import { holderChoices, isHeat, moveChoices, phaseEditable } from '../lib/phases.js';
  import FormatPicker from './FormatPicker.svelte';

  let {
    live,
    phase,
    onclose
  }: {
    live: EventLive;
    phase: PhaseView;
    onclose: () => void;
  } = $props();

  const def = $derived(phase.def);
  const phases = $derived(live.view?.phases ?? []);
  const editable = $derived(phaseEditable(phase, phases, live.view?.races ?? []));

  // The dialog starts from the phase as it is.
  // svelte-ignore state_referenced_locally
  let label = $state(def.label);
  // svelte-ignore state_referenced_locally
  let parent = $state(def.parent ?? '');
  /**
   * The saved format the phase's own format is, when it is one of them. A format that was
   * changed on the Formats page since, or came from a generator, is none of them: it is
   * then offered as "as it is now", so opening the dialog never changes it by itself.
   */
  const savedNow = $derived(
    def.format === undefined
      ? undefined
      : live.formats.find((saved) => JSON.stringify(saved.format) === JSON.stringify(def.format))
  );
  const KEEP = '';
  // svelte-ignore state_referenced_locally
  let formatId = $state(savedNow?.id ?? KEEP);
  /** Whether "the first entry" of the picker means "keep the format it has". */
  const keeps = $derived(def.format !== undefined && savedNow === undefined);

  const moves = $derived(moveChoices(phase, phases));
  const inherited = $derived(holderChoices(phases).find((holder) => holder.id === parent)?.format);
  let busy = $state(false);
  let error = $state<string | undefined>(undefined);

  /** The phase as it would be after the edit. */
  function edited(): PhaseDef {
    const name = label.trim();
    if (!editable.shape) return { ...def, label: name };
    const format =
      formatId === KEEP
        ? keeps
          ? def.format
          : undefined
        : live.formats.find((saved) => saved.id === formatId)?.format;
    const { parent: _was, format: _had, ...rest } = def;
    let kind = def.kind;
    // A phase that ranks by its format's rule follows the format it is given. One that
    // combines its heats another way (a generator's) is never reached here.
    const follows = (combine: unknown): boolean =>
      combine === 'Stack' ||
      (typeof combine === 'object' && combine !== null && 'Measures' in combine);
    if ('Group' in kind && follows(kind.Group.combine)) {
      const ranking = (format ?? inherited)?.ranking;
      kind = { Group: { ...kind.Group, combine: ranking ? { Measures: ranking } : 'Stack' } };
    }
    return {
      ...rest,
      kind,
      label: name,
      ...(parent ? { parent } : {}),
      ...(format ? { format } : {})
    };
  }

  const changed = $derived(JSON.stringify(edited()) !== JSON.stringify(def));

  async function submit(e?: Event) {
    e?.preventDefault();
    if (busy) return;
    error = undefined;
    if (label.trim() === '') {
      error = 'Give it a name.';
      return;
    }
    if (!changed) {
      onclose();
      return;
    }
    busy = true;
    try {
      if (await live.send({ Record: { PhaseDefined: { def: edited() } } })) onclose();
    } catch (err) {
      // The Director's own sentence, shown as it is.
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  const title = $derived(`Edit ${def.label}`);
</script>

<Dialog open={true} {title} {onclose}>
  <form class="form" aria-label={title} onsubmit={submit}>
    <Field label="Name" required>
      <Input bind:value={label} aria-label="Name" autocomplete="off" disabled={busy} />
    </Field>
    {#if editable.shape}
      <FormatPicker
        formats={live.formats}
        bind:value={formatId}
        none={keeps
          ? `As it is now: ${def.format?.name}`
          : inherited
            ? `The phase above's: ${inherited.name}`
            : 'None of its own'}
        noneSummary={keeps
          ? 'The format it has is kept as it is.'
          : inherited
            ? isHeat(def)
              ? 'It follows the format of the phase above.'
              : 'Its heats follow the format of the phase above.'
            : isHeat(def)
              ? 'A heat needs a format before it can be raced.'
              : 'Each heat under it is then given its own format.'}
        disabled={busy}
      />
      <Field label="Under">
        <Select bind:value={parent} aria-label="Under" disabled={busy}>
          <!-- A heat is always in a phase. One that stands alone from before can stay there
               until it is moved, so opening this never moves it by itself. -->
          {#if !isHeat(def) || def.parent === undefined}
            <option value="">The top of the event</option>
          {/if}
          {#each moves as holder (holder.id)}
            <option value={holder.id}>{holder.label}</option>
          {/each}
        </Select>
      </Field>
    {:else}
      <p class="why" data-testid="edit-why">{editable.why}</p>
    {/if}
    {#if error}
      <Banner tone="danger">{error}</Banner>
    {/if}
  </form>
  {#snippet footer()}
    <Button variant="ghost" onclick={onclose} disabled={busy}>Cancel</Button>
    <Button variant="primary" onclick={() => submit()} disabled={busy || label.trim() === ''}>
      Save
    </Button>
  {/snippet}
</Dialog>

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .why {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
</style>
