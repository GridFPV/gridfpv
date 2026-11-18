<script lang="ts">
  /**
   * AddPhaseDialog — add a phase. One dialog for every kind, so the page needs one button:
   *
   * - **Empty**: a phase with no heats yet (Practice, Qualifying). The director adds them.
   * - **Bracket**: every heat of a single or double elimination bracket.
   * - **Multi-Main**: stacked mains with bump-ups, each main a bracket.
   *
   * A bracket and a multi-main are generators: the Director builds their phases and heats
   * from the answers. What they make are ordinary phases, so they can be changed afterwards.
   * A new generator is one more choice in "Kind", never one more button on the page.
   *
   * The name and the phase it goes under are asked here. The rest of the questions belong
   * to the kind and live in its own fields component, which also sends the request. A
   * refusal, or a missing answer, is shown here in a sentence and the dialog stays open.
   *
   * The parent mounts this only while it is open.
   */
  import { Banner, Button, Dialog, Field, Input, Select } from '@gridfpv/components';
  import type { PhaseId } from '@gridfpv/types';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import { holderChoices } from '../lib/phases.js';
  import PhaseBracketFields from './PhaseBracketFields.svelte';
  import PhaseEmptyFields from './PhaseEmptyFields.svelte';
  import PhaseMainsFields from './PhaseMainsFields.svelte';

  let {
    live,
    under,
    onclose
  }: {
    live: EventLive;
    /** The phase the new one starts under, when it was opened from one. */
    under?: PhaseId;
    onclose: () => void;
  } = $props();

  type Kind = 'empty' | 'bracket' | 'mains';
  const KINDS: Record<Kind, { label: string; says: string; example: string }> = {
    empty: {
      label: 'Empty',
      says: 'A phase with no heats yet. You add the heats yourself.',
      example: 'e.g. Qualifying'
    },
    bracket: {
      label: 'Bracket',
      says: 'Every heat of a single or double elimination bracket, made for you.',
      example: 'e.g. Finals'
    },
    mains: {
      label: 'Multi-Main',
      says: 'Stacked mains, each one a bracket. The top pilots of a main bump up into the next.',
      example: 'e.g. Mains'
    }
  };

  let kind = $state<Kind>('empty');
  let name = $state('');
  // svelte-ignore state_referenced_locally
  let parent = $state<PhaseId | ''>(under ?? '');
  let busy = $state(false);
  let nameError = $state<string | undefined>(undefined);
  let error = $state<string | undefined>(undefined);
  /** The fields of the kind picked. It sends the request. */
  let fields = $state<{ add: (label: string) => Promise<boolean> } | undefined>(undefined);

  const holders = $derived(holderChoices(live.view?.phases ?? []));

  $effect(() => {
    void live.reloadFormats();
  });

  async function submit(e?: Event) {
    e?.preventDefault();
    if (busy || !fields) return;
    nameError = undefined;
    error = undefined;
    const label = name.trim();
    if (!label) {
      nameError = 'Give the phase a name.';
      return;
    }
    busy = true;
    try {
      if (await fields.add(label)) onclose();
    } catch (err) {
      // A missing answer, or the Director's own sentence, shown as it is.
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog open={true} title="Add a phase" {onclose}>
  <form class="form" aria-label="Add a phase" onsubmit={submit}>
    <Field label="Kind" hint={KINDS[kind].says}>
      <Select
        bind:value={kind}
        aria-label="Kind"
        disabled={busy}
        onchange={() => (error = undefined)}
      >
        {#each Object.entries(KINDS) as [value, choice] (value)}
          <option {value}>{choice.label}</option>
        {/each}
      </Select>
    </Field>
    <Field label="Phase name" required error={nameError}>
      <Input
        bind:value={name}
        placeholder={KINDS[kind].example}
        aria-label="Phase name"
        autocomplete="off"
        disabled={busy}
      />
    </Field>
    {#if holders.length > 0}
      <Field label="Under" hint="Leave it at the top unless it is part of a bigger phase.">
        <Select bind:value={parent} aria-label="Under" disabled={busy}>
          <option value="">The top of the event</option>
          {#each holders as holder (holder.id)}
            <option value={holder.id} disabled={holder.locked}>
              {holder.label}{holder.locked ? ' (locked)' : ''}
            </option>
          {/each}
        </Select>
      </Field>
    {/if}
    {#if kind === 'empty'}
      <PhaseEmptyFields bind:this={fields} {live} {parent} {busy} />
    {:else if kind === 'bracket'}
      <PhaseBracketFields bind:this={fields} {live} {parent} {busy} />
    {:else}
      <PhaseMainsFields bind:this={fields} {live} {parent} {busy} />
    {/if}
    {#if error}
      <Banner tone="danger">{error}</Banner>
    {/if}
  </form>
  {#snippet footer()}
    <Button variant="ghost" onclick={onclose} disabled={busy}>Cancel</Button>
    <Button variant="primary" onclick={() => submit()} disabled={busy}>
      {busy ? 'Adding…' : 'Add phase'}
    </Button>
  {/snippet}
</Dialog>

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
</style>
