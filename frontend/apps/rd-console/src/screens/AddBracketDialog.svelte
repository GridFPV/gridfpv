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
  import { isRanked } from '../lib/formats.js';
  import { holderChoices, newPhaseId } from '../lib/phases.js';
  import { canChoose, chosenSource, presetSeats, sourceOptionLabel } from '../lib/seats.js';
  import { rosterPrefill } from '../lib/rulings.js';
  import FormatPicker from './FormatPicker.svelte';
  import PilotPicker from './PilotPicker.svelte';

  let {
    live,
    under,
    onclose
  }: {
    live: EventLive;
    /** The phase the bracket starts under, when it was opened from one. */
    under?: string;
    onclose: () => void;
  } = $props();

  // svelte-ignore state_referenced_locally
  let parent = $state(under ?? '');
  const holders = $derived(holderChoices(live.view?.phases ?? []));

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
    void live.reloadFormats();
  });

  /** The classes at the event, as a shortcut for picking pilots. */
  const rosters = $derived(live.view?.rosters ?? []);
  /** Put a class's roster in the picker. The director can still change it. */
  function fillFrom(classId: string) {
    const roster = rosters.find((r) => r.class === classId);
    if (roster) chosen = rosterPrefill(roster);
  }

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
    const format = ranked.find((saved) => saved.id === formatValue)?.format;
    if (!format) {
      error = 'Pick a format that ranks the heat. It decides who moves on.';
      return;
    }
    const request: BracketRequest = {
      id: newPhaseId(label),
      label,
      ...(parent ? { parent } : {}),
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
    {#if holders.length > 0}
      <Field label="Under" hint="The phase it belongs to. Leave it at the top if it stands alone.">
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
    {#if rosters.length > 0}
      <Field
        label="Start from a class"
        hint="Fills in the class's roster. You can still change it."
      >
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
    <FormatPicker
      formats={live.formats}
      bind:value={() => formatValue, (id) => (formatId = id)}
      rankedOnly
      disabled={busy}
    />
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
