<script lang="ts">
  /**
   * EventClasses — the classes at this event: each with its roster, the phases that count
   * toward its standings, and the standings table (position, callsign, and the phase that
   * placed each pilot, by name).
   *
   * A class is added from the class directory with a roster picked from the pilot directory.
   * The standings are the Director's; this only shows them.
   */
  import { Button } from '@gridfpv/components';
  import type { RosterView } from '@gridfpv/types';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import { standingRows } from '../lib/rulings.js';
  import ClassRosterDialog from './ClassRosterDialog.svelte';
  import ClassStandingsDialog from './ClassStandingsDialog.svelte';

  let { live }: { live: EventLive } = $props();

  const names = $derived(live.names);
  const rosters = $derived(live.view?.rosters ?? []);

  /** The roster whose dialog is open, by class. Each dialog is mounted only while open. */
  let editingRoster = $state<RosterView['class'] | undefined>(undefined);
  let editingStandings = $state<RosterView['class'] | undefined>(undefined);
  const rosterOf = (id: RosterView['class'] | undefined) => rosters.find((r) => r.class === id);
</script>

{#if rosters.length === 0}
  <p class="muted">
    No class is in this event yet. Add one to keep a roster and get standings from 1 to n.
  </p>
{:else}
  <ul class="classes" aria-label="Classes at this event">
    {#each rosters as roster (roster.class)}
      {@const name = names.klass(roster.class)}
      {@const rows = standingRows(roster, names)}
      <li aria-label={name}>
        <div class="head">
          <span class="name">{name}</span>
          <span class="muted">
            {roster.pilots.length}
            {roster.pilots.length === 1 ? 'pilot' : 'pilots'}
          </span>
          <span class="actions">
            <Button
              size="sm"
              variant="ghost"
              aria-label="Edit the roster of {name}"
              onclick={() => (editingRoster = roster.class)}>Edit roster</Button
            >
            <Button
              size="sm"
              variant="ghost"
              aria-label="Choose what counts for {name}"
              onclick={() => (editingStandings = roster.class)}>What counts</Button
            >
          </span>
        </div>
        <p class="muted" data-testid="counted">
          {#if roster.counted_phases.length === 0}
            No phase counts toward the standings yet.
          {:else}
            Standings come from {roster.counted_phases
              .map((id) => names.phase(id))
              .join(', then ')}.
          {/if}
        </p>
        {#if rows.length > 0}
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
      </li>
    {/each}
  </ul>
{/if}

{#if editingRoster !== undefined}
  {@const roster = rosterOf(editingRoster)}
  {#if roster}
    <ClassRosterDialog {live} {roster} onclose={() => (editingRoster = undefined)} />
  {/if}
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
  .classes > li {
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface);
    display: grid;
    gap: var(--gf-space-2);
  }
  .head {
    display: flex;
    align-items: baseline;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
  }
  .name {
    font-weight: var(--gf-font-weight-bold);
    font-size: var(--gf-font-size-lg);
  }
  .actions {
    margin-left: auto;
    display: flex;
    gap: var(--gf-space-1);
  }
  .standings {
    width: 100%;
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
