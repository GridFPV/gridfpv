<script lang="ts">
  /**
   * ResultsPage — what the event has decided so far.
   *
   * - **Class standings**, under the phase results: each class from 1 to n, and the phase
   *   that placed each pilot. A class with nothing counting yet shows one line, not a table.
   * - **Phase results**: one phase's ranking at a time, with the numbers behind each place.
   *   It opens on the phase raced last. A best time names the race it was flown in.
   * - **The phase's races**: every race under the phase, with who finished where.
   * - **One race**: opened from the list, or from a best time. Each pilot with every lap.
   *
   * The three are steps down: a phase, its races, one race. The trail above a race leads
   * back up.
   *
   * Every order shown is the Director's. A result is provisional until its phase is locked
   * (on the Phases page), and the page says which it is.
   */
  import { Badge, Banner, Card, Select, formatMicros } from '@gridfpv/components';
  import type { PhaseId, RaceId } from '@gridfpv/types';
  import Page from '../Page.svelte';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import { consecutiveLaps } from '../lib/overlay.js';
  import {
    defaultResult,
    phaseRaces,
    raceDetail,
    resultChoices,
    resultTable
  } from '../lib/results.js';
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
  /** Every race under the phase, oldest first. */
  const races = $derived(view && phase ? phaseRaces(view, phase.def.id, names) : []);
  /** What a race is called here: inside its own heat the number says it all. */
  const raceLabel = (entry: { name: string; number: number }): string =>
    heat ? `Race ${entry.number}` : entry.name;

  /** The race opened in full, if one is. */
  let openedId = $state<RaceId | undefined>(undefined);
  const opened = $derived(races.find((entry) => entry.race === openedId));
  /** The opened race is ranked the way its own heat is. */
  const openedRunLength = $derived(
    opened ? consecutiveLaps(view?.phases.find((p) => p.def.id === opened.heat)?.format) : undefined
  );
  const detail = $derived(opened ? raceDetail(opened.view, names, openedRunLength) : []);
  function pickPhase(id: PhaseId): void {
    picked = id;
    openedId = undefined;
  }
</script>

<Page
  title="Results"
  help="results"
  lede="Each phase's ranking, then the class standings. A result is provisional until its phase is locked."
>
  {#if live.loadError}
    <Banner tone="danger" title="The event could not be read.">{live.loadError}</Banner>
  {/if}

  {#if !view}
    {#if !live.loadError}<p class="muted" role="status">Loading…</p>{/if}
  {:else}
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
              onchange={(e: Event) => pickPhase((e.currentTarget as HTMLSelectElement).value)}
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
        {#if opened}
          <nav class="trail" aria-label="Where this race is">
            <button type="button" class="link" onclick={() => (openedId = undefined)}>
              {phase.def.label}
            </button>
            <span aria-hidden="true">›</span>
            <span aria-current="page">{raceLabel(opened)}</span>
          </nav>
          <section class="race" aria-label={opened.name}>
            <div class="race-head">
              <h3 class="race-title">{opened.name}</h3>
              <Badge tone={opened.view.state === 'Saved' ? 'success' : 'neutral'}>
                {raceStateLabel(opened.view)}
              </Badge>
              {#if opened.view.state !== 'Saved'}
                <span class="faint">It does not count toward the results.</span>
              {/if}
              <a class="open" href={formatHash({ kind: 'marshal', race: opened.race })}
                >Open in Marshal</a
              >
            </div>
            {#if opened.view.note}<p class="muted">“{opened.view.note}”</p>{/if}
            {#if opened.view.video}
              <p class="muted">
                Video: <code>{opened.view.video.overlaid_file ?? opened.view.video.file}</code>
              </p>
            {/if}
            <div class="scroll">
              <table aria-label="Pilots of {opened.name}">
                <thead>
                  <tr>
                    <th scope="col" class="num">Place</th>
                    <th scope="col">Pilot</th>
                    <th scope="col" class="num">Laps</th>
                    {#if openedRunLength !== undefined}
                      <th scope="col" class="num">Best {openedRunLength} in a row</th>
                    {/if}
                    <th scope="col" class="num">Best lap</th>
                    <th scope="col" class="num">Total</th>
                    <th scope="col">Every lap</th>
                  </tr>
                </thead>
                <tbody>
                  {#each detail as row (row.key)}
                    <tr>
                      <td class="num place">{row.place ?? '–'}</td>
                      <th scope="row">
                        {row.name}
                        {#if row.falseStart}<Badge tone="danger">False start</Badge>{/if}
                      </th>
                      <td class="num">{row.laps}</td>
                      {#if openedRunLength !== undefined}
                        <td class="num strong">{formatMicros(row.bestRun?.totalMicros)}</td>
                      {/if}
                      <td class="num">{formatMicros(row.bestLapMicros)}</td>
                      <td class="num">{formatMicros(row.totalMicros)}</td>
                      <td>
                        {#if row.lapList.length === 0}
                          <span class="faint">No laps</span>
                        {:else}
                          <ol class="laps" aria-label="Laps of {row.name}">
                            {#each row.lapList as lap (lap.label)}
                              <li
                                class:in-run={lap.inBestRun}
                                class:best={lap.best}
                                title={lap.inBestRun
                                  ? 'Part of the best run'
                                  : lap.best
                                    ? 'Best lap'
                                    : undefined}
                              >
                                <span class="lap-n">{lap.label}</span>
                                {formatMicros(lap.micros)}
                              </li>
                            {/each}
                          </ol>
                        {/if}
                      </td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
            {#if openedRunLength !== undefined}
              <p class="muted key">
                <span class="swatch in-run"></span> the best {openedRunLength} in a row
                <span class="swatch best"></span> the best lap
              </p>
            {/if}
          </section>
        {:else}
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
                      <td class="num strong">
                        {formatMicros(row.bestRunMicros)}
                        {#if row.bestRunFrom}
                          {@const from = row.bestRunFrom}
                          <button
                            type="button"
                            class="link from"
                            aria-label="{row.callsign}'s best run: {from.name}"
                            onclick={() => (openedId = from.race)}
                          >
                            {raceLabel(from)}
                          </button>
                        {/if}
                      </td>
                    {/if}
                    {#if table.showWins}<td class="num">{row.placed.wins}</td>{/if}
                    {#if table.showPoints}<td class="num">{row.placed.points}</td>{/if}
                    <td class="num">
                      {formatMicros(row.placed.best_lap_micros)}
                      {#if row.bestLapFrom}
                        {@const from = row.bestLapFrom}
                        <button
                          type="button"
                          class="link from"
                          aria-label="{row.callsign}'s best lap: {from.name}"
                          onclick={() => (openedId = from.race)}
                        >
                          {raceLabel(from)}
                        </button>
                      {/if}
                    </td>
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
              {#each [...races].reverse() as entry (entry.race)}
                {@const race = entry.view}
                <li class:discarded={race.state === 'Discarded'}>
                  <button
                    type="button"
                    class="link race-name"
                    aria-label="Open {entry.name}"
                    onclick={() => (openedId = entry.race)}
                  >
                    {raceLabel(entry)}
                  </button>
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
      {/if}
    </Card>

    {#each view.rosters as roster (roster.class)}
      {@const rows = standingRows(roster, names)}
      {@const name = names.klass(roster.class)}
      <Card title="{name} standings" elevation="sm">
        <p class="muted">
          {#if roster.counted_phases.length === 0}
            No phase counts toward this class yet. Choose what counts on the
            <a href={formatHash({ kind: 'event', section: 'event' })}>Details page</a>.
          {:else}
            From {roster.counted_phases.map((id) => names.phase(id)).join(', then ')}.
          {/if}
        </p>
        <!-- With nothing counting yet there is no order to show, only the roster again. -->
        {#if rows.length > 0 && roster.counted_phases.length > 0}
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
  /* A button that reads as a link: it moves around this page, it does not leave it. */
  .link {
    appearance: none;
    border: 0;
    padding: 0;
    background: none;
    font: inherit;
    color: var(--gf-accent);
    cursor: pointer;
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .link:focus-visible {
    outline: 2px solid var(--gf-accent);
    outline-offset: 2px;
  }
  /* Under a best time: the race it was flown in. */
  .from {
    display: block;
    margin-left: auto;
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-xs);
    font-weight: var(--gf-font-weight-regular, 400);
    white-space: nowrap;
  }
  .trail {
    display: flex;
    align-items: baseline;
    gap: var(--gf-space-2);
    margin-bottom: var(--gf-space-3);
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .race-head {
    display: flex;
    align-items: baseline;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
    margin-bottom: var(--gf-space-2);
  }
  .race-title {
    margin: 0;
    font-size: var(--gf-font-size-lg);
  }
  .laps {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-1);
  }
  .laps li {
    padding: 1px var(--gf-space-2);
    border-radius: var(--gf-radius-sm);
    border: 1px solid var(--gf-border-subtle);
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
    font-size: var(--gf-font-size-sm);
    white-space: nowrap;
  }
  .lap-n {
    color: var(--gf-text-muted);
    margin-right: var(--gf-space-1);
  }
  .laps li.in-run,
  .swatch.in-run {
    background: var(--gf-elevated);
    border-color: var(--gf-accent);
  }
  .laps li.best {
    font-weight: var(--gf-font-weight-bold);
  }
  .key {
    margin-top: var(--gf-space-3);
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
  }
  .swatch {
    display: inline-block;
    width: 0.9em;
    height: 0.9em;
    border-radius: 3px;
    border: 1px solid var(--gf-border-subtle);
  }
  .swatch.best {
    border-width: 2px;
    border-color: var(--gf-text);
    margin-left: var(--gf-space-3);
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }
</style>
