<script lang="ts">
  /**
   * AddHeatDialog — add one heat to the event: a name, what times it, the pilots in seat
   * order, a format.
   *
   * "Timed by" lists every seat source by name (`lib/seats.ts`): each timer, then the Bench.
   * The seats on offer, and so the most pilots the heat can hold, follow the one chosen.
   *
   * The parent mounts this only while it is open, so a closed copy never sits in the page.
   * It sends one `PhaseDefined` fact. If the Director refuses, its sentence is shown here
   * and the dialog stays open with everything the director typed.
   */
  import { Banner, Button, Dialog, Field, Input, Select } from '@gridfpv/components';
  import type { AdapterId, HeatSeat, PhaseDef, PilotId } from '@gridfpv/types';
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
  import { canChoose, chosenSource, sourceOptionLabel } from '../lib/seats.js';
  import PilotPicker from './PilotPicker.svelte';

  let { live, onclose }: { live: EventLive; onclose: () => void } = $props();

  let name = $state('');
  let chosen = $state<PilotId[]>([]);
  /** The source the director picked. Until they pick, the default is used. */
  let pickedSource = $state<AdapterId | undefined>(undefined);
  const source = $derived(chosenSource(live.seatSources, pickedSource));
  const seats = $derived(source.seats);
  let formatKey = $state(BUILT_IN_FORMATS[0].key);
  let stopKey = $state(STOP_CHOICES[0].key);
  let limitKey = $state(LIMIT_CHOICES[0].key);
  let busy = $state(false);
  let nameError = $state<string | undefined>(undefined);
  let error = $state<string | undefined>(undefined);

  // The directory may have changed since the event page opened.
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
      nameError = 'Give the heat a name.';
      return;
    }
    if (chosen.length === 0) {
      error = 'Choose at least one pilot.';
      return;
    }
    if (chosen.length > seats.length) {
      error = `${source.name} has ${seats.length} ${seats.length === 1 ? 'seat' : 'seats'}. Take out ${chosen.length - seats.length} of the pilots.`;
      return;
    }
    const picked = formatByKey(formatKey);
    if (!picked) return;
    const format = withLimitChoice(withStopChoice(picked, stopKey), limitKey);
    const lineup: HeatSeat[] = chosen.map((pilot, index) => ({
      adapter: seats[index].adapter,
      seat: seats[index].seat,
      source: { Pilot: pilot }
    }));
    const def: PhaseDef = { id: newPhaseId(label), label, kind: { Heat: { lineup } }, format };
    busy = true;
    try {
      if (await live.send({ Record: { PhaseDefined: { def } } })) onclose();
    } catch (err) {
      // The Director's own sentence, shown as it is.
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog open={true} title="Add a heat" {onclose}>
  <form class="form" aria-label="Add a heat" onsubmit={submit}>
    <Field label="Heat name" required error={nameError}>
      <Input
        bind:value={name}
        placeholder="e.g. Qualifying Heat 1"
        aria-label="Heat name"
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
    <Field
      label="Pilots, in seat order"
      hint="Up to {seats.length}. The first pilot gets {seats[0]?.label ?? 'Seat 1'}."
    >
      <PilotPicker
        pilots={live.pilots}
        bind:chosen
        max={seats.length}
        slotLabel={(index) => seats[index]?.label ?? 'No seat'}
        disabled={busy}
      />
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
      {busy ? 'Adding…' : 'Add heat'}
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
