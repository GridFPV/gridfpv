<script lang="ts">
  /**
   * RunAllLaps — every lap of the race on the Run page, in one table under the seat cards
   * (docs/primitives.md §12.3).
   *
   * A seat card lists only its latest laps, so it stays one size in a long practice. This
   * is where the rest are: every seat's laps, shut until opened so it never clutters the
   * page. It can be narrowed to one seat or one pilot, and put fastest first.
   */
  import { Collapsible, Select, formatMicros } from '@gridfpv/components';
  import type { PilotId, RaceView } from '@gridfpv/types';
  import type { Names } from '../lib/names.js';
  import { allLapRows, filterLapRows, type LapOrder } from '../lib/run.js';

  let { race, names }: { race: RaceView; names: Names } = $props();

  let open = $state(false);
  let seat = $state('');
  let pilot = $state('');
  let order = $state<LapOrder>('flown');

  const rows = $derived(allLapRows(race, names));
  /** The seats and pilots that have a lap, for the two pickers. */
  const seats = $derived([...new Map(rows.map((row) => [row.seatKey, row.seatName]))]);
  const pilots = $derived(
    [
      ...new Map(
        rows.flatMap((row) => (row.pilot === undefined ? [] : [[row.pilot, row.who] as const]))
      )
    ].sort((a, b) => a[1].localeCompare(b[1]))
  );
  // A pick that no longer has a lap (a new race, a lap removed) is no pick.
  const seatPick = $derived(seats.some(([key]) => key === seat) ? seat : '');
  const pilotPick = $derived(pilots.some(([id]) => id === pilot) ? pilot : '');
  const shown = $derived(
    filterLapRows(rows, {
      seat: seatPick || undefined,
      pilot: (pilotPick || undefined) as PilotId | undefined,
      order
    })
  );
  const value = (e: Event) => (e.currentTarget as HTMLSelectElement).value;
</script>

{#if rows.length > 0}
  <Collapsible title="All laps" bind:open>
    {#snippet summary()}
      <span class="count" data-testid="all-laps-count">{rows.length}</span>
    {/snippet}
    <div class="filters">
      <Select
        size="sm"
        aria-label="Seat"
        value={seatPick}
        onchange={(e: Event) => (seat = value(e))}
      >
        <option value="">Every seat</option>
        {#each seats as [key, name] (key)}
          <option value={key}>{name}</option>
        {/each}
      </Select>
      <Select
        size="sm"
        aria-label="Pilot"
        value={pilotPick}
        onchange={(e: Event) => (pilot = value(e))}
      >
        <option value="">Every pilot</option>
        {#each pilots as [id, callsign] (id)}
          <option value={id}>{callsign}</option>
        {/each}
      </Select>
      <Select
        size="sm"
        aria-label="Order"
        value={order}
        onchange={(e: Event) => (order = value(e) as LapOrder)}
      >
        <option value="flown">As flown</option>
        <option value="fastest">Fastest first</option>
      </Select>
    </div>
    {#if shown.length === 0}
      <p class="muted">No lap matches.</p>
    {:else}
      <table aria-label="All laps">
        <thead>
          <tr>
            <th scope="col">Seat</th>
            <th scope="col">Pilot</th>
            <th scope="col" class="num">Lap</th>
            <th scope="col" class="num">Time</th>
          </tr>
        </thead>
        <tbody>
          {#each shown as row (row.key)}
            <tr class:best={row.best}>
              <td>{row.seatName}</td>
              <td class:muted={row.pilot === undefined}>{row.who}</td>
              <td class="num">{row.number}</td>
              <td class="num time">{formatMicros(row.micros)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </Collapsible>
{/if}

<style>
  .count {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-regular);
  }
  .filters {
    display: flex;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
    margin-bottom: var(--gf-space-3);
  }
  .muted {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--gf-font-size-sm);
    font-variant-numeric: tabular-nums;
  }
  th,
  td {
    text-align: left;
    padding: var(--gf-space-1) var(--gf-space-2);
    border-bottom: 1px solid var(--gf-border-subtle);
  }
  thead th {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
    font-weight: var(--gf-font-weight-semibold);
  }
  .num {
    text-align: right;
  }
  .time {
    font-family: var(--gf-font-mono);
  }
  /* A pilot's fastest lap on a seat. */
  tr.best .time {
    color: var(--gf-accent);
    font-weight: var(--gf-font-weight-semibold);
  }
</style>
