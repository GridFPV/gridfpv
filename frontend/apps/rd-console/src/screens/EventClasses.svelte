<script lang="ts">
  /**
   * EventClasses — the classes at this event, each a block that is shut until opened.
   *
   * Shut, a class is one line: its name, how many pilots, and what its standings come from.
   * Opened, it is the class's **roster**, to change in place (pick pilots, then Save roster),
   * and its **standings** once a phase counts (position, callsign, and the phase that placed
   * each pilot, by name).
   *
   * A class is added from the class directory with a roster picked from the pilot directory.
   * The standings are the Director's; this only shows them.
   */
  import { Banner, Button, Collapsible } from '@gridfpv/components';
  import type { ClassId, PilotId, RosterView } from '@gridfpv/types';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import { standingRows } from '../lib/rulings.js';
  import ClassStandingsDialog from './ClassStandingsDialog.svelte';
  import PilotPicker from './PilotPicker.svelte';

  let { live }: { live: EventLive } = $props();

  const names = $derived(live.names);
  const rosters = $derived(live.view?.rosters ?? []);

  /** The class whose "what counts" dialog is open. It is mounted only while open. */
  let editingStandings = $state<ClassId | undefined>(undefined);
  const rosterOf = (id: ClassId | undefined) => rosters.find((r) => r.class === id);

  /** Which classes are open. Every class starts shut. */
  let opened = $state<Record<ClassId, boolean>>({});

  /**
   * The roster as it is being changed, by class. A class has an entry only while its roster
   * differs from the Director's, so a change made elsewhere shows at once in a class that
   * is not being edited.
   */
  let drafts = $state<Record<ClassId, PilotId[]>>({});
  let busy = $state<ClassId | undefined>(undefined);
  let errors = $state<Record<ClassId, string | undefined>>({});

  const same = (a: PilotId[], b: PilotId[]) =>
    a.length === b.length && a.every((p, i) => p === b[i]);
  const rosterNow = (roster: RosterView): PilotId[] => drafts[roster.class] ?? roster.pilots;

  function change(roster: RosterView, next: PilotId[]) {
    if (same(next, roster.pilots)) delete drafts[roster.class];
    else drafts[roster.class] = next;
  }

  async function save(roster: RosterView) {
    const pilots = drafts[roster.class];
    if (pilots === undefined || busy !== undefined) return;
    busy = roster.class;
    errors[roster.class] = undefined;
    try {
      if (await live.send({ Record: { RosterSet: { class: roster.class, pilots } } })) {
        delete drafts[roster.class];
      }
    } catch (err) {
      // The Director's own sentence, shown as it is.
      errors[roster.class] = err instanceof Error ? err.message : String(err);
    } finally {
      busy = undefined;
    }
  }

  // The pilot directory may have changed since the page opened: read it when a class is opened.
  function onOpened(id: ClassId, isOpen: boolean) {
    opened[id] = isOpen;
    if (isOpen) void live.reloadPilots();
  }
</script>

{#if rosters.length === 0}
  <p class="muted">No class is in this event yet.</p>
{:else}
  <ul class="classes" aria-label="Classes at this event">
    {#each rosters as roster (roster.class)}
      {@const name = names.klass(roster.class)}
      {@const rows = standingRows(roster, names)}
      {@const dirty = drafts[roster.class] !== undefined}
      <li aria-label={name}>
        <Collapsible
          title={name}
          bind:open={
            () => opened[roster.class] ?? false, (isOpen) => onOpened(roster.class, isOpen)
          }
        >
          {#snippet summary()}
            <span class="count">
              {roster.pilots.length}
              {roster.pilots.length === 1 ? 'pilot' : 'pilots'}
            </span>
            <span class="counted" data-testid="counted">
              {#if roster.counted_phases.length > 0}
                Standings from {roster.counted_phases.map((id) => names.phase(id)).join(', then ')}
              {/if}
            </span>
          {/snippet}
          {#snippet actions()}
            <Button
              size="sm"
              variant="ghost"
              aria-label="Choose what counts for {name}"
              onclick={() => (editingStandings = roster.class)}>What counts</Button
            >
          {/snippet}

          <form
            class="roster"
            aria-label="Roster of {name}"
            onsubmit={(e) => {
              e.preventDefault();
              void save(roster);
            }}
          >
            <PilotPicker
              pilots={live.pilots}
              bind:chosen={() => rosterNow(roster), (next) => change(roster, next)}
              slotLabel={(index) => `${index + 1}`}
              disabled={busy === roster.class}
            />
            {#if errors[roster.class]}
              <Banner tone="danger">{errors[roster.class]}</Banner>
            {/if}
            {#if dirty}
              <div class="save">
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={busy === roster.class}
                  onclick={() => delete drafts[roster.class]}>Discard</Button
                >
                <Button variant="primary" size="sm" type="submit" disabled={busy === roster.class}>
                  Save roster
                </Button>
              </div>
            {/if}
          </form>

          {#if roster.counted_phases.length > 0 && rows.length > 0}
            <table class="standings" aria-label="Standings of {name}">
              <thead>
                <tr>
                  <th scope="col">Position</th>
                  <th scope="col">Pilot</th>
                  <th scope="col">Placed by</th>
                </tr>
              </thead>
              <tbody>
                {#each rows as row (`${row.position}/${row.callsign}`)}
                  <tr>
                    <td class="num">{row.position}</td>
                    <th scope="row">{row.callsign}</th>
                    <td class:muted={row.placedBy === undefined}>
                      {row.placedBy ?? 'Not placed yet'}
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        </Collapsible>
      </li>
    {/each}
  </ul>
{/if}

{#if editingStandings !== undefined}
  {@const roster = rosterOf(editingStandings)}
  {#if roster}
    <ClassStandingsDialog {live} {roster} onclose={() => (editingStandings = undefined)} />
  {/if}
{/if}

<style>
  .muted {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .classes {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .count,
  .counted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-regular);
  }
  .counted:not(:empty)::before {
    content: '·';
    margin: 0 var(--gf-space-2);
  }
  .roster {
    display: grid;
    gap: var(--gf-space-3);
  }
  .save {
    display: flex;
    justify-content: flex-end;
    gap: var(--gf-space-2);
  }
  .standings {
    width: 100%;
    margin-top: var(--gf-space-4);
    border-collapse: collapse;
    font-size: var(--gf-font-size-sm);
  }
  .standings th,
  .standings td {
    text-align: left;
    padding: var(--gf-space-1) var(--gf-space-2);
    border-bottom: 1px solid var(--gf-border-subtle);
  }
  .standings thead th {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
    font-weight: var(--gf-font-weight-semibold);
  }
  .num {
    font-family: var(--gf-font-mono);
    width: 5rem;
  }
</style>
