<script lang="ts">
  /**
   * AddHeatDialog — add one heat to the event: a name, the phase it goes under, what times
   * it, the pilots in seat order, a format.
   *
   * The format is one of the saved formats, copied into the heat. Under a phase that has a
   * format the heat can simply use that one, and then carries none of its own.
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
  import { holderChoices, newPhaseId } from '../lib/phases.js';
  import { canChoose, chosenSource, sourceOptionLabel } from '../lib/seats.js';
  import { rosterPrefill } from '../lib/rulings.js';
  import FormatPicker from './FormatPicker.svelte';
  import PilotPicker from './PilotPicker.svelte';

  let {
    live,
    under,
    onclose
  }: {
    live: EventLive;
    /** The phase the heat starts under, when it was opened from one. */
    under?: PhaseDef['id'];
    onclose: () => void;
  } = $props();

  let name = $state('');
  let chosen = $state<PilotId[]>([]);
  /** The source the director picked. Until they pick, the default is used. */
  let pickedSource = $state<AdapterId | undefined>(undefined);
  const source = $derived(chosenSource(live.seatSources, pickedSource));
  const seats = $derived(source.seats);
  // svelte-ignore state_referenced_locally
  let parent = $state(under ?? '');
  const holders = $derived(holderChoices(live.view?.phases ?? []));
  /** The format a heat under the chosen phase gets without one of its own. */
  const inherited = $derived(holders.find((holder) => holder.id === parent)?.format);
  /** The saved format picked. `''` is "use the phase's" where there is one. */
  let formatId = $state<string | undefined>(undefined);
  const formatValue = $derived(formatId ?? (inherited ? '' : (live.formats[0]?.id ?? '')));
  let busy = $state(false);
  let nameError = $state<string | undefined>(undefined);
  let error = $state<string | undefined>(undefined);

  // The directory may have changed since the event page opened.
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
    if (roster) chosen = rosterPrefill(roster, seats.length);
  }

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
    const format = live.formats.find((saved) => saved.id === formatValue)?.format;
    if (!format && !inherited) {
      error = 'Pick a format. It says how the race ends and how the heat is ranked.';
      return;
    }
    const lineup: HeatSeat[] = chosen.map((pilot, index) => ({
      adapter: seats[index].adapter,
      seat: seats[index].seat,
      source: { Pilot: pilot }
    }));
    const def: PhaseDef = {
      id: newPhaseId(label),
      label,
      kind: { Heat: { lineup } },
      ...(parent ? { parent } : {}),
      ...(format ? { format } : {})
    };
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
    <FormatPicker
      formats={live.formats}
      bind:value={() => formatValue, (id) => (formatId = id)}
      none={inherited ? `The phase's: ${inherited.name}` : undefined}
      noneSummary={inherited ? "The heat follows its phase's format." : undefined}
      disabled={busy}
    />
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
