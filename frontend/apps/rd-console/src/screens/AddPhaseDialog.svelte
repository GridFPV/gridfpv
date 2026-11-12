<script lang="ts">
  /**
   * AddPhaseDialog — add a phase that holds heats: Practice, Qualifying, a track of a home
   * series. A name, the phase it goes under, the class it is for, and a format.
   *
   * A format set here is used by every heat added under the phase, so it is picked once.
   * The phase's own ranking takes each pilot's best result across its heats, by the same
   * rule the format ranks one heat with. A phase with no format, or an unranked one, lists
   * its heats in order and ranks nobody.
   *
   * The parent mounts this only while it is open. It sends one `PhaseDefined` fact; a
   * refusal is shown here in the Director's words.
   */
  import { Banner, Button, Dialog, Field, Input, Select } from '@gridfpv/components';
  import type { PhaseDef } from '@gridfpv/types';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import { holderChoices, newPhaseId } from '../lib/phases.js';
  import FormatPicker from './FormatPicker.svelte';

  let {
    live,
    under,
    onclose
  }: {
    live: EventLive;
    /** The phase the new one starts under, when it was opened from one. */
    under?: PhaseDef['id'];
    onclose: () => void;
  } = $props();

  let name = $state('');
  // svelte-ignore state_referenced_locally
  let parent = $state(under ?? '');
  let classId = $state('');
  let formatId = $state('');
  let busy = $state(false);
  let nameError = $state<string | undefined>(undefined);
  let error = $state<string | undefined>(undefined);

  const holders = $derived(holderChoices(live.view?.phases ?? []));
  const rosters = $derived(live.view?.rosters ?? []);
  const inherited = $derived(holders.find((holder) => holder.id === parent)?.format);

  $effect(() => {
    void live.reloadFormats();
  });

  async function submit(e?: Event) {
    e?.preventDefault();
    if (busy) return;
    nameError = undefined;
    error = undefined;
    const label = name.trim();
    if (!label) {
      nameError = 'Give the phase a name.';
      return;
    }
    const format = live.formats.find((saved) => saved.id === formatId)?.format;
    // The rule the phase ranks by: its format's, or the one it inherits.
    const ranking = (format ?? inherited)?.ranking;
    const def: PhaseDef = {
      id: newPhaseId(label),
      label,
      kind: {
        Group: {
          ...(classId ? { field: [{ Class: classId }] } : {}),
          combine: ranking ? { Measures: ranking } : 'Stack'
        }
      },
      ...(parent ? { parent } : {}),
      ...(format ? { format } : {})
    };
    busy = true;
    try {
      if (await live.send({ Record: { PhaseDefined: { def } } })) onclose();
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog open={true} title="Add a phase" {onclose}>
  <form class="form" aria-label="Add a phase" onsubmit={submit}>
    <Field label="Phase name" required error={nameError}>
      <Input
        bind:value={name}
        placeholder="e.g. Qualifying"
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
    {#if rosters.length > 0}
      <Field
        label="For the class"
        hint="A ranked phase is for one class. Its roster is then the phase's field, so every pilot in it gets a place."
      >
        <Select bind:value={classId} aria-label="For the class" disabled={busy}>
          <option value="">Whoever flies in its heats</option>
          {#each rosters as roster (roster.class)}
            <option value={roster.class}>{live.names.klass(roster.class)}</option>
          {/each}
        </Select>
      </Field>
    {/if}
    <FormatPicker
      formats={live.formats}
      bind:value={formatId}
      none={inherited ? `The phase above's: ${inherited.name}` : 'None of its own'}
      noneSummary={inherited
        ? 'Its heats follow the format of the phase above.'
        : 'Each heat under it is then given its own format.'}
      disabled={busy}
    />
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
