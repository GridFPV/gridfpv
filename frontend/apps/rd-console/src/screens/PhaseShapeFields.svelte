<script lang="ts">
  /**
   * PhaseShapeFields — the shape of a bracket's heats: what times them, how many pilots are
   * in each, how many move on, single or double elimination, and the format.
   *
   * A bracket and a set of mains (each main is a bracket) ask the same things, so both use
   * this. `shape()` gives the answers, or throws the sentence to show when one is missing.
   * A round robin has no elimination, so it switches those two questions off.
   *
   * "Timed by" lists every seat source by name (`lib/seats.ts`). A heat cannot hold more
   * pilots than the chosen source has seats.
   */
  import { Field, Select } from '@gridfpv/components';
  import type { AdapterId, EliminationKind, Format, PresetSeat } from '@gridfpv/types';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import { isRanked } from '../lib/formats.js';
  import { canChoose, chosenSource, presetSeats, sourceOptionLabel } from '../lib/seats.js';
  import FormatPicker from './FormatPicker.svelte';

  let {
    live,
    busy = false,
    eliminates = true,
    heatSize = $bindable('4')
  }: {
    live: EventLive;
    busy?: boolean;
    /** Whether pilots move on and go out: ask how many move on, and single or double. */
    eliminates?: boolean;
    /** How many pilots fly in each heat, for a parent that shows what the answers make. */
    heatSize?: string;
  } = $props();

  /** The source the director picked. Until they pick, the default is used. */
  let pickedSource = $state<AdapterId | undefined>(undefined);
  const source = $derived(chosenSource(live.seatSources, pickedSource));
  /** A bracket heat needs two pilots at least; the source's seats set the most. */
  const heatSizes = $derived(
    Array.from({ length: Math.max(0, source.seats.length - 1) }, (_, index) => index + 2)
  );
  // The selects hold strings; they are turned into numbers where they are used.
  let advance = $state('2');
  let elimination = $state<EliminationKind>('Double');
  /**
   * The saved format picked. A bracket heat needs a ranking to know who moves on, so only
   * ranked formats are offered, and the first one that races to a lap count is the default.
   */
  let formatId = $state<string | undefined>(undefined);
  const ranked = $derived(live.formats.filter((saved) => isRanked(saved.format)));
  const formatValue = $derived(
    formatId ??
      (
        ranked.find((saved) => {
          const end = saved.format.race.end;
          return typeof end === 'object' && 'FirstToLaps' in end;
        }) ?? ranked[0]
      )?.id ??
      ''
  );

  /** At most half of a heat can move on. */
  const advanceChoices = $derived(
    Array.from({ length: Math.floor(Number(heatSize) / 2) }, (_, index) => index + 1)
  );

  // A source with fewer seats can make the heat size too big; pull it back into range.
  $effect(() => {
    const most = heatSizes[heatSizes.length - 1];
    if (most !== undefined && Number(heatSize) > most) heatSize = String(most);
  });

  // A smaller heat can make the chosen number too big; pull it back into range.
  $effect(() => {
    const most = advanceChoices[advanceChoices.length - 1] ?? 1;
    if (Number(advance) > most) advance = String(most);
  });

  /** The answers, ready to send. Throws the sentence to show when one is missing. */
  export function shape(): {
    seats: PresetSeat[];
    advance: number;
    elimination: EliminationKind;
    format: Format;
  } {
    if (heatSizes.length === 0) {
      throw new Error(
        `A heat here needs at least two seats. ${source.name} has ${source.seats.length}.`
      );
    }
    const format = ranked.find((saved) => saved.id === formatValue)?.format;
    if (!format) throw new Error('Pick a format that ranks the heat. It decides who moves on.');
    return {
      seats: presetSeats(source, Number(heatSize)),
      advance: Number(advance),
      elimination,
      format
    };
  }
</script>

<Field
  label="Timed by"
  hint={source.bench
    ? 'The Bench is not a timer. Crossings are logged by hand on the Run page.'
    : undefined}
>
  <Select
    value={source.id}
    aria-label="Timed by"
    disabled={busy}
    onchange={(e: Event) => (pickedSource = (e.currentTarget as HTMLSelectElement).value)}
  >
    {#each live.seatSources as choice (choice.id)}
      <option value={choice.id} disabled={!canChoose(choice)}>
        {sourceOptionLabel(choice)}
      </option>
    {/each}
  </Select>
</Field>
<div class="row">
  <Field label="Pilots in each heat">
    <Select bind:value={heatSize} aria-label="Pilots in each heat" disabled={busy}>
      {#each heatSizes as size (size)}
        <option value={String(size)}>{size}</option>
      {/each}
    </Select>
  </Field>
  {#if eliminates}
    <Field label="How many move on">
      <Select bind:value={advance} aria-label="How many move on" disabled={busy}>
        {#each advanceChoices as count (count)}
          <option value={String(count)}>{count}</option>
        {/each}
      </Select>
    </Field>
  {/if}
</div>
{#if eliminates}
  <Field label="Elimination">
    <Select bind:value={elimination} aria-label="Elimination" disabled={busy}>
      <option value="Single">Single: one loss and a pilot is out</option>
      <option value="Double">Double: a pilot is out after two losses</option>
    </Select>
  </Field>
{/if}
<FormatPicker
  formats={live.formats}
  bind:value={() => formatValue, (id) => (formatId = id)}
  rankedOnly
  disabled={busy}
/>

<style>
  .row {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--gf-space-4);
  }
</style>
