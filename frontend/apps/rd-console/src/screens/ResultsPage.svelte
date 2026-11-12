<script lang="ts">
  /**
   * ResultsPage — what the event has decided so far.
   *
   * - **Class standings**: each class from 1 to n, and the phase that placed each pilot.
   * - **Phase results**: one phase's ranking at a time, with the numbers behind each place.
   *   It opens on the phase raced last. A heat also lists its races, each with a link to
   *   Marshal.
   *
   * Every order shown is the Director's. A result is provisional until its phase is locked
   * (on the Phases page), and the page says which it is.
   */
  import { Badge, Banner, Card, Select, formatMicros } from '@gridfpv/components';
  import type { PhaseId } from '@gridfpv/types';
  import Page from '../Page.svelte';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import { racesOfHeat } from '../lib/phases.js';
  import { defaultResult, resultChoices, resultTable } from '../lib/results.js';
  import { formatHash } from '../lib/route.js';
  import { standingRows } from '../lib/rulings.js';
  import { finishOrder, raceStateLabel } from '../lib/run.js';

  let { live }: { live: EventLive } = $props();

  const names = $derived(live.names);
  const view = $derived(live.view);
  const choices = $derived(view ? resultChoices(view) : []);
  /** The director's pick. Until they pick, the page shows the phase raced last. */
  let picked = $state<PhaseId | undefined>(undefined);
  const shownId = $derived(
    picked !== undefined && choices.some((choice) => choice.id === picked)
      ? picked
      : view
        ? defaultResult(view, choices)
        : undefined
  );
  const phase = $derived(view?.phases.find((p) => p.def.id === shownId));
  const table = $derived(view && phase ? resultTable(view, phase, names) : undefined);
  const heat = $derived(choices.find((choice) => choice.id === shownId)?.heat ?? false);
  const races = $derived(
    view && phase && heat
      ? racesOfHeat(view, phase.def.id).map((race, index) => ({ race, number: index + 1 }))
      : []
  );
</script>

<Page
  title="Results"
  help="results"
  lede="Class standings and each phase's ranking. A result is provisional until its phase is locked."
>
  {#if live.loadError}
    <Banner tone="danger" title="The event could not be read.">{live.loadError}</Banner>
  {/if}

  {#if !view}
    {#if !live.loadError}<p class="muted" role="status">Loading…</p>{/if}
  {:else}
    {#each view.rosters as roster (roster.class)}
      {@const rows = standingRows(roster, names)}
      {@const name = names.klass(roster.class)}
      <Card title="{name} standings" elevation="sm">
        <p class="muted">
          {#if roster.counted_phases.length === 0}
            No phase counts toward this class yet. Choose what counts on the
            <a href={formatHash({ kind: 'event', section: 'event' })}>Event page</a>.
          {:else}
            From {roster.counted_phases.map((id) => names.phase(id)).join(', then ')}.
          {/if}
        </p>
        {#if rows.length > 0}
          <table aria-label="Standings of {name}">
            <thead>
              <tr>
                <th scope="col" class="num">Place</th>
                <th scope="col">Pilot</th>
                <th scope="col">Placed by</th>
              </tr>
            </thead>
            <tbody>
              {#each rows as row (`${row.position}/${row.callsign}`)}
                <tr>
                  <td class="num">{row.position}</td>
                  <th scope="row">{row.callsign}</th>
                  <td class:faint={row.placedBy === undefined}>
                    {row.placedBy ?? 'Not placed yet'}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </Card>
    {/each}

    <Card title="Phase results" elevation="sm">
      {#if choices.length === 0}
        <p class="muted" data-testid="no-results">
          No ranked phase has a result yet. Results show up here as races are saved.
        </p>
      {:else if phase && table}
        <div class="pick">
          <label class="pick-box">
            <span class="eyebrow">Phase</span>
            <Select
              aria-label="Phase"
              value={shownId}
              onchange={(e: Event) => (picked = (e.currentTarget as HTMLSelectElement).value)}
            >
              {#each choices as choice (choice.id)}
                <option value={choice.id}>{choice.label}</option>
              {/each}
            </Select>
          </label>
          <span class="state" data-testid="result-state">
            {#if phase.locked}
              <Badge tone="info">Official</Badge> Locked.
            {:else}
              <Badge tone="neutral">Provisional</Badge>
              {phase.complete ? 'Complete, not locked yet.' : 'Still being raced.'}
            {/if}
            {#if phase.format}<span class="faint">{phase.format.name}</span>{/if}
          </span>
        </div>
        <div class="scroll">
          <table aria-label="Results of {phase.def.label}">
            <thead>
              <tr>
                <th scope="col" class="num">Place</th>
                <th scope="col">Pilot</th>
                {#if table.runLength !== undefined}
                  <th scope="col" class="num">Best {table.runLength} in a row</th>
                {/if}
                {#if table.showWins}<th scope="col" class="num">Wins</th>{/if}
                {#if table.showPoints}<th scope="col" class="num">Points</th>{/if}
                <th scope="col" class="num">Best lap</th>
                <th scope="col" class="num">Most laps</th>
                <th scope="col"><span class="sr-only">Notes</span></th>
              </tr>
            </thead>
            <tbody>
              {#each table.rows as row (row.placed.pilot)}
                <tr>
                  <td class="num place">{row.position}</td>
                  <th scope="row">{row.callsign}</th>
                  {#if table.runLength !== undefined}
                    <td class="num strong">{formatMicros(row.bestRunMicros)}</td>
                  {/if}
                  {#if table.showWins}<td class="num">{row.placed.wins}</td>{/if}
                  {#if table.showPoints}<td class="num">{row.placed.points}</td>{/if}
                  <td class="num">{formatMicros(row.placed.best_lap_micros)}</td>
                  <td class="num">{row.placed.laps}</td>
                  <td class="flags">
                    {#if row.placed.position_set}<Badge tone="violet">Set by hand</Badge>{/if}
                    {#if row.placed.disqualified}<Badge tone="danger">Disqualified</Badge>{/if}
                    {#if !row.placed.flew}<Badge tone="neutral">Did not fly</Badge>{/if}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>

        {#if races.length > 0}
          <h3 class="sub">Races of {phase.def.label}</h3>
          <ol class="races" aria-label="Races of {phase.def.label}">
            {#each [...races].reverse() as { race, number } (race.race)}
              <li class:discarded={race.state === 'Discarded'}>
                <span class="race-name">Race {number}</span>
                <Badge tone={race.state === 'Saved' ? 'success' : 'neutral'}>
                  {raceStateLabel(race)}
                </Badge>
                <span class="finish">
                  {#each finishOrder(race) as seat (`${seat.adapter}/${seat.seat}`)}
                    {@const laps = seat.laps.filter((lap) => !lap.restart).length}
                    <span>
                      {seat.finish}. {seat.pilot === undefined
                        ? names.who('Open')
                        : names.pilot(seat.pilot)}
                      <span class="faint">({laps} {laps === 1 ? 'lap' : 'laps'})</span>
                    </span>
                  {/each}
                </span>
                {#if race.note}<span class="faint">“{race.note}”</span>{/if}
                <a class="open" href={formatHash({ kind: 'marshal', race: race.race })}
                  >Open in Marshal</a
                >
              </li>
            {/each}
          </ol>
        {/if}
      {/if}
    </Card>
  {/if}
</Page>

<style>
  .muted {
    margin: 0 0 var(--gf-space-3);
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .muted a,
  .open {
    color: var(--gf-accent);
  }
  .faint {
    color: var(--gf-text-muted);
  }
  .pick {
    display: flex;
    align-items: flex-end;
    gap: var(--gf-space-4);
    flex-wrap: wrap;
    margin-bottom: var(--gf-space-4);
  }
  .pick-box {
    display: grid;
    gap: var(--gf-space-1);
    min-width: 16rem;
  }
  .eyebrow {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .state {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
    font-size: var(--gf-font-size-sm);
  }
  .scroll {
    overflow-x: auto;
  }
  table {
    width: 100%;
    border-collapse: collapse;
  }
  th,
  td {
    padding: var(--gf-space-2) var(--gf-space-3);
    text-align: left;
    border-bottom: 1px solid var(--gf-border-subtle);
  }
  thead th {
    font-size: var(--gf-font-size-xs);
    font-weight: var(--gf-font-weight-semibold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
    white-space: nowrap;
  }
  tbody tr:last-child th,
  tbody tr:last-child td {
    border-bottom: none;
  }
  tbody th {
    font-weight: var(--gf-font-weight-semibold);
  }
  .num {
    text-align: right;
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
  }
  thead .num {
    font-family: inherit;
  }
  .place {
    color: var(--gf-text-muted);
    font-weight: var(--gf-font-weight-bold);
  }
  .strong {
    font-weight: var(--gf-font-weight-bold);
  }
  .flags {
    white-space: nowrap;
  }
  .sub {
    margin: var(--gf-space-5) 0 var(--gf-space-2);
    font-size: var(--gf-font-size-md);
  }
  .races {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .races li {
    display: flex;
    align-items: baseline;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
    font-size: var(--gf-font-size-sm);
  }
  .races li.discarded {
    opacity: 0.6;
  }
  .race-name {
    font-weight: var(--gf-font-weight-semibold);
    font-size: var(--gf-font-size-md);
  }
  .finish {
    display: flex;
    gap: var(--gf-space-1) var(--gf-space-4);
    flex-wrap: wrap;
  }
  .open {
    margin-left: auto;
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }
</style>
