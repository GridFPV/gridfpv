<script lang="ts">
  /**
   * PhaseEmptyFields — the questions for an empty phase: the class it is for and its format.
   * The director adds its heats afterwards.
   *
   * A format set here is used by every heat added under the phase, so it is picked once.
   * The phase's own ranking takes each pilot's best result across its heats, by the same
   * rule the format ranks one heat with. A phase with no format, or an unranked one, lists
   * its heats in order and ranks nobody.
   */
  import { Field, Select } from '@gridfpv/components';
  import type { PhaseDef, PhaseId } from '@gridfpv/types';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import { holderChoices, newPhaseId } from '../lib/phases.js';
  import FormatPicker from './FormatPicker.svelte';

  let {
    live,
    parent,
    busy = false
  }: {
    live: EventLive;
    /** The phase the new one goes under, or `''` for the top of the event. */
    parent: PhaseId | '';
    busy?: boolean;
  } = $props();

  let classId = $state('');
  let formatId = $state('');
  const rosters = $derived(live.view?.rosters ?? []);
  const inherited = $derived(
    holderChoices(live.view?.phases ?? []).find((holder) => holder.id === parent)?.format
  );

  /** Add the phase. Sends one `PhaseDefined` fact. */
  export function add(label: string): Promise<boolean> {
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
    return live.send({ Record: { PhaseDefined: { def } } });
  }
</script>

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
