<script lang="ts">
  /**
   * PhaseBracketFields — the questions for an elimination bracket: the pilots in seed
   * order, and the shape of its heats (`PhaseShapeFields`).
   *
   * The Director builds every heat of the bracket from one request, all or none. If it
   * cannot, its sentence is thrown from `add` for the dialog to show.
   */
  import { Field, Select } from '@gridfpv/components';
  import type { BracketRequest, PhaseId, PilotId } from '@gridfpv/types';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import { newPhaseId } from '../lib/phases.js';
  import { rosterPrefill } from '../lib/rulings.js';
  import PhaseShapeFields from './PhaseShapeFields.svelte';
  import PilotPicker from './PilotPicker.svelte';

  let {
    live,
    parent,
    busy = false
  }: {
    live: EventLive;
    /** The phase the bracket goes under, or `''` for the top of the event. */
    parent: PhaseId | '';
    busy?: boolean;
  } = $props();

  let chosen = $state<PilotId[]>([]);
  let shapeFields = $state<PhaseShapeFields | undefined>(undefined);

  $effect(() => {
    void live.reloadPilots();
    void live.reloadTimers();
  });

  /** The classes at the event, as a shortcut for picking pilots. */
  const rosters = $derived(live.view?.rosters ?? []);
  /** Put a class's roster in the picker. The director can still change it. */
  function fillFrom(classId: string) {
    const roster = rosters.find((r) => r.class === classId);
    if (roster) chosen = rosterPrefill(roster);
  }

  /** Add the bracket. Throws the sentence to show when an answer is missing. */
  export function add(label: string): Promise<boolean> {
    if (chosen.length < 2) throw new Error('Choose at least two pilots.');
    if (!shapeFields) return Promise.resolve(false);
    const request: BracketRequest = {
      id: newPhaseId(label),
      label,
      ...(parent ? { parent } : {}),
      field: [{ Imported: chosen }],
      entrants: chosen.map((pilot) => ({ Pilot: pilot })),
      ...shapeFields.shape()
    };
    return live.createBracket(request);
  }
</script>

{#if rosters.length > 0}
  <Field label="Start from a class" hint="Fills in the class's roster. You can still change it.">
    <Select
      value=""
      aria-label="Start from a class"
      disabled={busy}
      onchange={(e: Event) => fillFrom((e.currentTarget as HTMLSelectElement).value)}
    >
      <option value="">Pick pilots one by one</option>
      {#each rosters as roster (roster.class)}
        <option value={roster.class}>{live.names.klass(roster.class)}</option>
      {/each}
    </Select>
  </Field>
{/if}
<Field label="Pilots, in seed order" hint="The first pilot is the top seed.">
  <PilotPicker
    pilots={live.pilots}
    bind:chosen
    slotLabel={(index) => `Seed ${index + 1}`}
    disabled={busy}
  />
</Field>
<PhaseShapeFields bind:this={shapeFields} {live} {busy} />
