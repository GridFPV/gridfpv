<script lang="ts">
  /**
   * PhaseRoundRobinFields — the questions for a round robin: the pilots in seed order, how
   * many fly in each heat, how many rounds, the format, and the points for each place.
   *
   * Every pilot flies one heat in each round. The Director mixes the pilots between rounds
   * so they meet different pilots, and moves a pilot to another seat only when it has to,
   * since a change of seat is a change of video channel. The round robin ranks by points added up over every heat.
   *
   * The points table starts at one point for last place in a full heat and one more for each
   * place above it (4, 3, 2, 1 for heats of four). The director can type another. It is put
   * on the round robin's copy of the format, so the saved format is not changed. The
   * Director builds every heat from one request, all or none.
   */
  import { Field, Input, Select } from '@gridfpv/components';
  import type { PhaseId, PilotId, RoundRobinRequest } from '@gridfpv/types';
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
    /** The phase the round robin goes under, or `''` for the top of the event. */
    parent: PhaseId | '';
    busy?: boolean;
  } = $props();

  let chosen = $state<PilotId[]>([]);
  let shapeFields = $state<PhaseShapeFields | undefined>(undefined);
  let heatSize = $state('4');
  let rounds = $state('3');
  /** The points typed. Until they are, the table offered for this heat size. */
  let typedPoints = $state<string | undefined>(undefined);
  const offered = $derived(
    Array.from({ length: Number(heatSize) }, (_, place) => Number(heatSize) - place).join(', ')
  );
  const points = $derived(typedPoints ?? offered);

  $effect(() => {
    void live.reloadPilots();
    void live.reloadTimers();
  });

  /** The classes at the event: the phase can be for one of them. */
  const rosters = $derived(live.view?.rosters ?? []);
  /**
   * The class the phase is for, or `''` for none. It stays the phase's class whatever
   * pilots are picked in the end, so its results count toward that class's standings.
   */
  let classId = $state('');
  /** Pick the class, and put its roster in the picker. The director can still change it. */
  function fillFrom(picked: string) {
    classId = picked;
    const roster = rosters.find((r) => r.class === picked);
    if (roster) chosen = rosterPrefill(roster);
  }

  const roundCount = $derived.by(() => {
    const value = Number(rounds);
    return rounds.trim() !== '' && Number.isInteger(value) && value >= 1 ? value : undefined;
  });
  /** How many heats one round takes: as few as hold the pilots. */
  const heatsEach = $derived(Math.max(1, Math.ceil(chosen.length / Number(heatSize))));

  /** The points for each place, first place first, or `undefined` when they do not read. */
  function parsePoints(text: string): number[] | undefined {
    const parts = text
      .split(',')
      .map((part) => part.trim())
      .filter((part) => part !== '');
    const values = parts.map(Number);
    return values.length > 0 && values.every((value) => Number.isInteger(value) && value >= 0)
      ? values
      : undefined;
  }

  /** Add the round robin. Throws the sentence to show when an answer is missing. */
  export function add(label: string): Promise<boolean> {
    if (chosen.length < 2) throw new Error('Choose at least two pilots.');
    if (roundCount === undefined) throw new Error('Say how many rounds, one at least.');
    const table = parsePoints(points);
    if (table === undefined) {
      throw new Error('List the points for each place, first place first, like 4, 3, 2, 1.');
    }
    if (!shapeFields) return Promise.resolve(false);
    const { seats, format } = shapeFields.shape();
    const request: RoundRobinRequest = {
      id: newPhaseId(label),
      label,
      ...(parent ? { parent } : {}),
      ...(classId ? { class: classId } : {}),
      field: [{ Imported: chosen }],
      entrants: chosen.map((pilot) => ({ Pilot: pilot })),
      seats,
      rounds: roundCount,
      format: { ...format, points: table }
    };
    return live.createRoundRobin(request);
  }
</script>

{#if rosters.length > 0}
  <Field
    label="Class"
    hint="Fills in the class's roster, which you can still change. Its results then count toward that class's standings."
  >
    <Select
      value={classId}
      aria-label="Class"
      disabled={busy}
      onchange={(e: Event) => fillFrom((e.currentTarget as HTMLSelectElement).value)}
    >
      <option value="">No class: pick pilots one by one</option>
      {#each rosters as roster (roster.class)}
        <option value={roster.class}>{live.names.klass(roster.class)}</option>
      {/each}
    </Select>
  </Field>
{/if}
<Field
  label="Pilots, in seed order"
  hint="The first pilot is the top seed. Round 1 spreads the top seeds across the heats."
>
  <PilotPicker
    pilots={live.pilots}
    bind:chosen
    slotLabel={(index) => `Seed ${index + 1}`}
    disabled={busy}
  />
</Field>
<PhaseShapeFields bind:this={shapeFields} bind:heatSize {live} {busy} eliminates={false} />
<Field label="Rounds" hint="Every pilot flies one heat in each round.">
  <Input
    type="number"
    inputmode="numeric"
    min="1"
    step="1"
    aria-label="Rounds"
    value={rounds}
    oninput={(e: Event) => (rounds = (e.currentTarget as HTMLInputElement).value)}
    disabled={busy}
  />
</Field>
<Field
  label="Points by place"
  hint="First place first. A pilot's points from every heat are added up, and the most points wins."
>
  <Input
    aria-label="Points by place"
    value={points}
    oninput={(e: Event) => (typedPoints = (e.currentTarget as HTMLInputElement).value)}
    autocomplete="off"
    disabled={busy}
  />
</Field>
{#if chosen.length >= 2 && roundCount !== undefined}
  <p class="makes" data-testid="round-robin-count" role="status">
    This makes {roundCount}
    {roundCount === 1 ? 'round' : 'rounds'} of {heatsEach}
    {heatsEach === 1 ? 'heat' : 'heats'}, {roundCount * heatsEach} heats in all.
    {heatsEach === 1
      ? 'Each pilot keeps the same seat in every round.'
      : 'Pilots are mixed each round, and change seat as little as that allows.'}
  </p>
{/if}

<style>
  .makes {
    margin: 0;
    color: var(--gf-text-secondary);
    font-size: var(--gf-font-size-sm);
  }
</style>
