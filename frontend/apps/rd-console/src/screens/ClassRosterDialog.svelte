<script lang="ts">
  /**
   * ClassRosterDialog — add a class to the event with its roster, or change the roster of a
   * class that is already in it. The class comes from the class directory and the pilots from
   * the pilot directory. It sends one `RosterSet` fact.
   *
   * The parent mounts this only while it is open.
   */
  import { Banner, Button, Dialog, Field, Select } from '@gridfpv/components';
  import type { ClassId, PilotId, RosterView } from '@gridfpv/types';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import { classesToAdd } from '../lib/rulings.js';
  import PilotPicker from './PilotPicker.svelte';

  let {
    live,
    roster = undefined,
    onclose
  }: {
    live: EventLive;
    /** The roster to change. Left out to add a class. */
    roster?: RosterView | undefined;
    onclose: () => void;
  } = $props();

  const choices = $derived(classesToAdd(live.classes, live.view?.rosters ?? []));
  /** The class picked when adding. Until one is picked, the first on offer. */
  let pickedClass = $state<ClassId | undefined>(undefined);
  const classId = $derived(roster?.class ?? pickedClass ?? choices[0]?.id);
  // The picker starts from the roster as it is now.
  // svelte-ignore state_referenced_locally
  let chosen = $state<PilotId[]>([...(roster?.pilots ?? [])]);
  let busy = $state(false);
  let error = $state<string | undefined>(undefined);

  // The directories may have changed since the event page opened.
  $effect(() => {
    void live.reloadPilots();
    void live.reloadClasses();
  });

  async function submit(e?: Event) {
    e?.preventDefault();
    if (busy) return;
    error = undefined;
    if (classId === undefined) {
      error = 'Choose a class.';
      return;
    }
    busy = true;
    try {
      if (await live.send({ Record: { RosterSet: { class: classId, pilots: chosen } } })) onclose();
    } catch (err) {
      // The Director's own sentence, shown as it is.
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  const title = $derived(roster ? `Roster of ${live.names.klass(roster.class)}` : 'Add a class');
</script>

<Dialog open={true} {title} {onclose}>
  <form class="form" aria-label={title} onsubmit={submit}>
    {#if !roster}
      {#if choices.length === 0}
        <Banner tone="info">
          Every class in the class directory is already in this event. Add more on the Classes page.
        </Banner>
      {:else}
        <Field label="Class">
          <Select
            value={classId ?? ''}
            aria-label="Class"
            disabled={busy}
            onchange={(e: Event) => (pickedClass = (e.currentTarget as HTMLSelectElement).value)}
          >
            {#each choices as choice (choice.id)}
              <option value={choice.id}>{choice.name}</option>
            {/each}
          </Select>
        </Field>
      {/if}
    {/if}
    <Field label="Roster" hint="The pilots who race in this class at this event.">
      <PilotPicker
        pilots={live.pilots}
        bind:chosen
        slotLabel={(index) => `${index + 1}`}
        disabled={busy}
      />
    </Field>
    {#if error}
      <Banner tone="danger">{error}</Banner>
    {/if}
  </form>
  {#snippet footer()}
    <Button variant="ghost" onclick={onclose} disabled={busy}>Cancel</Button>
    <Button variant="primary" onclick={() => submit()} disabled={busy || classId === undefined}>
      {roster ? 'Save roster' : 'Add class'}
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
