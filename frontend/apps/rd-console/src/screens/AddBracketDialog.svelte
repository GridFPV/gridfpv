<script lang="ts">
  /**
   * AddBracketDialog — generate an elimination bracket: a name, what times it, the pilots in
   * seed order, the heat size, how many move on from each heat, single or double elimination, a format.
   *
   * The Director builds every heat of the bracket from this one request, all or none. If it
   * cannot (a shape that does not work, or something the event does not allow), its sentence
   * is shown here and the dialog stays open.
   *
   * "Timed by" lists every seat source by name (`lib/seats.ts`). A heat cannot hold more
   * pilots than the chosen source has seats.
   */
  import { Banner, Button, Dialog, Field, Input, Select } from '@gridfpv/components';
  import type { AdapterId, BracketRequest, EliminationKind, PilotId } from '@gridfpv/types';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import {
    BUILT_IN_FORMATS,
    LIMIT_CHOICES,
    STOP_CHOICES,
    canHaveLimit,
    canStopItself,
    formatByKey,
    withLimitChoice,
    withStopChoice
  } from '../lib/formats.js';
  import { newPhaseId } from '../lib/phases.js';
  import { canChoose, chosenSource, presetSeats, sourceOptionLabel } from '../lib/seats.js';
  import PilotPicker from './PilotPicker.svelte';

  let { live, onclose }: { live: EventLive; onclose: () => void } = $props();

  /** "First to 3 laps": a bracket heat needs a ranking to know who moves on. */
  const DEFAULT_FORMAT = BUILT_IN_FORMATS.find((choice) => choice.format.ranking)?.key ?? '';

  let name = $state('');
  let chosen = $state<PilotId[]>([]);
  /** The source the director picked. Until they pick, the default is used. */
  let pickedSource = $state<AdapterId | undefined>(undefined);
  const source = $derived(chosenSource(live.seatSources, pickedSource));
  /** A bracket heat needs two pilots at least; the source's seats set the most. */
  const heatSizes = $derived(
    Array.from({ length: Math.max(0, source.seats.length - 1) }, (_, index) => index + 2)
  );
  // The selects hold strings; they are turned into numbers where they are used.
  let heatSize = $state('4');
  let advance = $state('2');
  let elimination = $state<EliminationKind>('Double');
  let formatKey = $state(
    BUILT_IN_FORMATS.find((choice) => choice.key === 'first-to-3')?.key ?? DEFAULT_FORMAT
  );
  let stopKey = $state(STOP_CHOICES[0].key);
  let limitKey = $state(LIMIT_CHOICES[0].key);
  let busy = $state(false);
  let nameError = $state<string | undefined>(undefined);
  let error = $state<string | undefined>(undefined);

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

  $effect(() => {
    void live.reloadPilots();
    void live.reloadTimers();
  });

  async function submit(e?: Event) {
    e?.preventDefault();
    if (busy) return;
    nameError = undefined;
    error = undefined;
    const label = name.trim();
    if (!label) {
      nameError = 'Give the bracket a name.';
      return;
    }
    if (chosen.length < 2) {
      error = 'Choose at least two pilots.';
      return;
    }
    if (heatSizes.length === 0) {
      error = `A bracket heat needs at least two seats. ${source.name} has ${source.seats.length}.`;
      return;
    }
    const picked = formatByKey(formatKey);
    if (!picked) return;
    const format = withLimitChoice(withStopChoice(picked, stopKey), limitKey);
    const request: BracketRequest = {
      id: newPhaseId(label),
      label,
      field: [{ Imported: chosen }],
      entrants: chosen.map((pilot) => ({ Pilot: pilot })),
      seats: presetSeats(source, Number(heatSize)),
      advance: Number(advance),
      elimination,
      format
    };
    busy = true;
    try {
      if (await live.createBracket(request)) onclose();
    } catch (err) {
      // The Director's own sentence, shown as it is.
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog open={true} title="Add a bracket" {onclose}>
  <form class="form" aria-label="Add a bracket" onsubmit={submit}>
    <Field label="Bracket name" required error={nameError}>
      <Input
        bind:value={name}
        placeholder="e.g. Finals"
        aria-label="Bracket name"
        autocomplete="off"
      />
    </Field>
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
    <Field label="Pilots, in seed order" hint="The first pilot is the top seed.">
      <PilotPicker
        pilots={live.pilots}
        bind:chosen
        slotLabel={(index) => `Seed ${index + 1}`}
        disabled={busy}
      />
    </Field>
    <div class="row">
      <Field label="Pilots in each heat">
        <Select bind:value={heatSize} aria-label="Pilots in each heat" disabled={busy}>
          {#each heatSizes as size (size)}
            <option value={String(size)}>{size}</option>
          {/each}
        </Select>
      </Field>
      <Field label="How many move on">
        <Select bind:value={advance} aria-label="How many move on" disabled={busy}>
          {#each advanceChoices as count (count)}
            <option value={String(count)}>{count}</option>
          {/each}
        </Select>
      </Field>
    </div>
    <Field label="Elimination">
      <Select bind:value={elimination} aria-label="Elimination" disabled={busy}>
        <option value="Single">Single: one loss and a pilot is out</option>
        <option value="Double">Double: a pilot is out after two losses</option>
      </Select>
    </Field>
    <Field label="Format">
      <Select bind:value={formatKey} aria-label="Format" disabled={busy}>
        {#each BUILT_IN_FORMATS as choice (choice.key)}
          <option value={choice.key}>{choice.label}</option>
        {/each}
      </Select>
    </Field>
    {#if canStopItself(formatByKey(formatKey))}
      <Field label="The race stops">
        <Select bind:value={stopKey} aria-label="The race stops" disabled={busy}>
          {#each STOP_CHOICES as choice (choice.key)}
            <option value={choice.key}>{choice.label}</option>
          {/each}
        </Select>
      </Field>
    {/if}
    {#if canHaveLimit(formatByKey(formatKey))}
      <Field label="Time limit">
        <Select bind:value={limitKey} aria-label="Time limit" disabled={busy}>
          {#each LIMIT_CHOICES as choice (choice.key)}
            <option value={choice.key}>{choice.label}</option>
          {/each}
        </Select>
      </Field>
    {/if}
    {#if error}
      <Banner tone="danger">{error}</Banner>
    {/if}
  </form>
  {#snippet footer()}
    <Button variant="ghost" onclick={onclose} disabled={busy}>Cancel</Button>
    <Button variant="primary" onclick={() => submit()} disabled={busy}>
      {busy ? 'Adding…' : 'Add bracket'}
    </Button>
  {/snippet}
</Dialog>

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .row {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--gf-space-4);
  }
</style>
