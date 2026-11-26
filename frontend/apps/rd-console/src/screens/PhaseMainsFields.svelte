<script lang="ts">
  /**
   * PhaseMainsFields — the questions for a multi-main: stacked mains, each one a bracket.
   *
   * The pilots come from the ranking of another phase (qualifying). The best go straight to
   * the A-Main, the next to the B-Main, and so on. The top pilots of each main bump up into
   * the one above. The Director works out how many mains the field takes and builds every
   * heat from one request, all or none; this says the count before it is sent.
   */
  import { Field, Input, Select } from '@gridfpv/components';
  import type { MainsRequest, PhaseId } from '@gridfpv/types';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import { isRanked } from '../lib/formats.js';
  import { holderChoices, mainName, mainsNeeded, newPhaseId } from '../lib/phases.js';
  import PhaseShapeFields from './PhaseShapeFields.svelte';

  let {
    live,
    parent,
    busy = false
  }: {
    live: EventLive;
    /** The phase the mains go under, or `''` for the top of the event. */
    parent: PhaseId | '';
    busy?: boolean;
  } = $props();

  let shapeFields = $state<PhaseShapeFields | undefined>(undefined);

  $effect(() => {
    void live.reloadTimers();
  });

  const phases = $derived(live.view?.phases ?? []);
  /** The phases that can seed the mains: any phase that holds heats. */
  const seeders = $derived(holderChoices(phases));
  const ranksOf = (id: PhaseId): number => {
    const found = phases.find((phase) => phase.def.id === id);
    return found ? found.ranking.length || found.field.length : 0;
  };
  /**
   * The phase picked. Until one is: the first finished phase that ranks anyone (qualifying
   * that is over), or failing that the first phase that ranks anyone.
   */
  let pickedSeeder = $state<PhaseId | undefined>(undefined);
  const seeder = $derived.by(() => {
    if (pickedSeeder !== undefined) return pickedSeeder;
    const ranking = seeders.filter((choice) => ranksOf(choice.id) > 0);
    const done = ranking.find(
      (choice) => phases.find((phase) => phase.def.id === choice.id)?.complete
    );
    return (done ?? ranking[0])?.id ?? '';
  });
  /** How many pilots. Until it is typed, as many as the seeding phase ranks. */
  let typedPilots = $state<string | undefined>(undefined);
  const pilots = $derived(
    typedPilots ?? (seeder && ranksOf(seeder) > 0 ? String(ranksOf(seeder)) : '')
  );
  let perMain = $state('16');
  let bump = $state('2');
  /** The format for the A-Main's final only, or `''` for the same as every other heat. */
  let topFinalId = $state('');
  const ranked = $derived(live.formats.filter((saved) => isRanked(saved.format)));

  /** A whole number from a box, or `undefined` when it holds anything else. */
  const count = (text: string): number | undefined => {
    const value = Number(text);
    return text.trim() !== '' && Number.isInteger(value) && value >= 0 ? value : undefined;
  };
  /** Why the numbers cannot work, or `undefined` when they can. */
  const problem = $derived.by(() => {
    const n = count(pilots);
    const each = count(perMain);
    const up = count(bump);
    if (n === undefined || n < 2) return 'Say how many pilots race the mains, two at least.';
    if (each === undefined || each < 2) return 'A main needs two pilots at least.';
    if (up === undefined) return 'Say how many pilots bump up. It can be 0.';
    if (up >= each) return 'Fewer pilots must bump up than start in a main.';
    return undefined;
  });
  const mains = $derived(
    problem === undefined ? mainsNeeded(Number(pilots), Number(perMain), Number(bump)) : undefined
  );

  /** Add the mains. Throws the sentence to show when an answer is missing. */
  export function add(label: string): Promise<boolean> {
    if (!seeder) throw new Error('Pick the phase the mains are seeded from.');
    if (problem !== undefined) throw new Error(problem);
    if (!shapeFields) return Promise.resolve(false);
    const topFinal = ranked.find((saved) => saved.id === topFinalId)?.format;
    const request: MainsRequest = {
      id: newPhaseId(label),
      label,
      ...(parent ? { parent } : {}),
      seeded_from: seeder,
      pilots: Number(pilots),
      per_main: Number(perMain),
      bump: Number(bump),
      ...shapeFields.shape(),
      ...(topFinal ? { top_final_format: topFinal } : {})
    };
    return live.createMains(request);
  }
</script>

<Field
  label="Seeded from"
  hint="The phase whose ranking fills the mains, such as Qualifying. Its best pilots go to the A-Main."
>
  <Select
    value={seeder}
    aria-label="Seeded from"
    disabled={busy}
    onchange={(e: Event) => (pickedSeeder = (e.currentTarget as HTMLSelectElement).value)}
  >
    <option value="" disabled>Pick a phase</option>
    {#each seeders as choice (choice.id)}
      <option value={choice.id}>{choice.label}</option>
    {/each}
  </Select>
</Field>
<div class="row three">
  <Field label="Pilots">
    <Input
      type="number"
      inputmode="numeric"
      min="2"
      step="1"
      aria-label="Pilots"
      value={pilots}
      oninput={(e: Event) => (typedPilots = (e.currentTarget as HTMLInputElement).value)}
      disabled={busy}
    />
  </Field>
  <Field label="Pilots in each main">
    <Input
      type="number"
      inputmode="numeric"
      min="2"
      step="1"
      aria-label="Pilots in each main"
      value={perMain}
      oninput={(e: Event) => (perMain = (e.currentTarget as HTMLInputElement).value)}
      disabled={busy}
    />
  </Field>
  <Field label="How many bump up">
    <Input
      type="number"
      inputmode="numeric"
      min="0"
      step="1"
      aria-label="How many bump up"
      value={bump}
      oninput={(e: Event) => (bump = (e.currentTarget as HTMLInputElement).value)}
      disabled={busy}
    />
  </Field>
</div>
<p class="makes" data-testid="mains-count" role="status">
  {#if mains === undefined}
    {problem}
  {:else if mains === 1}
    This makes 1 main, the {mainName(0)}.
  {:else}
    This makes {mains} mains, {mainName(0)} to {mainName(mains - 1)}. The {mainName(mains - 1)} is flown
    first.
  {/if}
</p>
<PhaseShapeFields bind:this={shapeFields} {live} {busy} />
<Field
  label="{mainName(0)} final"
  hint="The last heat of the top main can have its own format, such as Chase the Ace."
>
  <Select bind:value={topFinalId} aria-label="{mainName(0)} final" disabled={busy}>
    <option value="">The same format as the other heats</option>
    {#each ranked as saved (saved.id)}
      <option value={saved.id}>{saved.format.name}</option>
    {/each}
  </Select>
</Field>

<style>
  .row {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: var(--gf-space-4);
  }
  .makes {
    margin: 0;
    color: var(--gf-text-secondary);
    font-size: var(--gf-font-size-sm);
  }
</style>
