<script lang="ts">
  /**
   * SpectatorView — the open event for someone who is watching (docs/primitives.md §11.2).
   *
   * Three things, one at a time on a phone: the race that is **on now** (or how the last
   * one ended), the heats **coming up** with who is in them, and the **results**.
   *
   * It reads the same live store the console's pages do, fed from the Director's public
   * routes, and draws with the console's own helpers. It has no button that changes
   * anything: a spectator can only look.
   */
  import { Badge, Tabs, formatMicros } from '@gridfpv/components';
  import type { PhaseId } from '@gridfpv/types';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import { ordinal } from '../lib/names.js';
  import { consecutiveLaps, overlayClock, overlayRows } from '../lib/overlay.js';
  import { defaultResult, resultChoices, resultTable } from '../lib/results.js';
  import { missingNames, nowShowing, raceWord, upcomingHeats } from '../lib/spectator.js';

  let {
    live
  }: {
    live: Pick<
      EventLive,
      | 'view'
      | 'names'
      | 'pilots'
      | 'classes'
      | 'timers'
      | 'loadError'
      | 'nowMicros'
      | 'reloadPilots'
      | 'reloadClasses'
      | 'reloadTimers'
    >;
  } = $props();

  /** A lap time or a run of laps, or a dash when there is none yet. */
  const time = (micros: number | null | undefined): string =>
    micros === undefined || micros === null ? '—' : formatMicros(micros);

  type Tab = 'now' | 'next' | 'results';
  let tab = $state<Tab>('now');

  const view = $derived(live.view);
  const names = $derived(live.names);
  const showing = $derived(nowShowing(view));
  const upcoming = $derived(upcomingHeats(view, names));

  // The names a visitor is given are only the ones the event uses. When the snapshot
  // mentions someone new, ask for the names again.
  $effect(() => {
    const missing = missingNames(view, live.pilots, live.classes, live.timers);
    if (missing.pilots) void live.reloadPilots();
    if (missing.classes) void live.reloadClasses();
    if (missing.timers) void live.reloadTimers();
  });

  // ── The race on top ────────────────────────────────────────────────────────
  const heat = $derived(
    showing ? view?.phases.find((phase) => phase.def.id === showing.race.heat) : undefined
  );
  const runLength = $derived(consecutiveLaps(heat?.format));
  const rows = $derived(showing ? overlayRows(showing.race, names, runLength) : []);
  /** The Director's clock, read again a few times a second while a race is on. */
  let now = $state(0);
  $effect(() => {
    now = live.nowMicros();
    if (!showing?.live) return;
    const timer = setInterval(() => (now = live.nowMicros()), 200);
    return () => clearInterval(timer);
  });
  const clock = $derived(showing?.live ? overlayClock(showing.race, now) : undefined);

  // ── Results ────────────────────────────────────────────────────────────────
  const choices = $derived(view ? resultChoices(view) : []);
  /** The phase picked by hand. Until one is, the page follows the phase raced last. */
  let picked = $state<PhaseId | undefined>(undefined);
  const resultId = $derived(
    picked !== undefined && choices.some((choice) => choice.id === picked)
      ? picked
      : view
        ? defaultResult(view, choices)
        : undefined
  );
  const resultPhase = $derived(view?.phases.find((phase) => phase.def.id === resultId));
  const table = $derived(view && resultPhase ? resultTable(view, resultPhase, names) : undefined);

  const tabs = $derived([
    { id: 'now', label: showing?.live ? 'On now' : 'Last race' },
    { id: 'next', label: 'Up next', badge: upcoming.length > 0 ? upcoming.length : undefined },
    { id: 'results', label: 'Results' }
  ]);
</script>

<div class="spectator">
  {#if live.loadError && !view}
    <p class="notice" role="status" data-testid="spectator-offline">
      Could not reach Grid. Trying again.
    </p>
  {:else if !view}
    <p class="notice" role="status">Loading…</p>
  {:else}
    {#if live.loadError}
      <p class="notice" role="status" data-testid="spectator-stale">
        Not live right now. Trying again.
      </p>
    {/if}

    <Tabs items={tabs} bind:value={tab} ariaLabel="What to show" />

    {#if tab === 'now'}
      <section aria-label={showing?.live ? 'On now' : 'Last race'}>
        {#if !showing}
          <p class="empty" data-testid="no-race">No race yet.</p>
        {:else}
          <header class="race">
            <h2 data-testid="race-heat">{names.phase(showing.race.heat)}</h2>
            <span class="word">
              <Badge tone={showing.live ? 'success' : 'neutral'}>
                <span data-testid="race-word">{raceWord(showing.race)}</span>
              </Badge>
            </span>
            {#if clock}
              <span class="clock" role="timer" data-testid="race-clock">{clock.text}</span>
            {/if}
          </header>
          <table aria-label="Pilots in this race">
            <thead>
              <tr>
                <th scope="col"><span class="visually-hidden">Place</span></th>
                <th scope="col">Pilot</th>
                <th scope="col" class="num">Laps</th>
                <th scope="col" class="num">Last</th>
                <th scope="col" class="num">{runLength ? `Best ${runLength}` : 'Best'}</th>
              </tr>
            </thead>
            <tbody>
              {#each rows as row (row.key)}
                <tr>
                  <td class="place">{row.place === undefined ? '' : ordinal(row.place)}</td>
                  <th scope="row">
                    {#if row.color}
                      <span class="dot" style:background={row.color} aria-hidden="true"></span>
                    {/if}
                    {row.name}
                  </th>
                  <td class="num">{row.laps}</td>
                  <td class="num">{time(row.lastLapMicros)}</td>
                  <td class="num">
                    {time(runLength ? row.bestRun?.totalMicros : row.bestLapMicros)}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </section>
    {:else if tab === 'next'}
      <section aria-label="Up next">
        {#if upcoming.length === 0}
          <p class="empty" data-testid="no-upcoming">Nothing is lined up yet.</p>
        {:else}
          <ol class="heats">
            {#each upcoming as entry (entry.id)}
              <li aria-label={entry.name}>
                <h3>{entry.name}</h3>
                <ul>
                  {#each entry.seats as seat (`${seat.adapter}\u0000${seat.seat}`)}
                    <li>
                      <span class="seat">{seat.seatName}</span>
                      <span class="who">{seat.who}</span>
                    </li>
                  {/each}
                </ul>
              </li>
            {/each}
          </ol>
        {/if}
      </section>
    {:else}
      <section aria-label="Results">
        {#if choices.length === 0 || !table}
          <p class="empty" data-testid="no-results">No results yet.</p>
        {:else}
          <label class="pick">
            <span class="visually-hidden">Results for</span>
            <select
              aria-label="Results for"
              value={resultId}
              onchange={(e) => (picked = (e.currentTarget as HTMLSelectElement).value)}
            >
              {#each choices as choice (choice.id)}
                <option value={choice.id}>{choice.label}</option>
              {/each}
            </select>
          </label>
          <table aria-label="Results">
            <thead>
              <tr>
                <th scope="col"><span class="visually-hidden">Place</span></th>
                <th scope="col">Pilot</th>
                {#if table.showWins}<th scope="col" class="num">Wins</th>{/if}
                {#if table.showPoints}<th scope="col" class="num">Points</th>{/if}
                <th scope="col" class="num">Laps</th>
                {#if table.runLength}<th scope="col" class="num">Best {table.runLength}</th>{/if}
                <th scope="col" class="num">Best lap</th>
              </tr>
            </thead>
            <tbody>
              {#each table.rows as row (row.placed.pilot)}
                <tr class:out={row.placed.disqualified}>
                  <td class="place">{ordinal(row.position)}</td>
                  <th scope="row">
                    {row.callsign}
                    {#if row.placed.disqualified}<Badge tone="danger">Disqualified</Badge>{/if}
                    {#if !row.placed.flew}<span class="muted">did not fly</span>{/if}
                  </th>
                  {#if table.showWins}<td class="num">{row.placed.wins}</td>{/if}
                  {#if table.showPoints}<td class="num">{row.placed.points}</td>{/if}
                  <td class="num">{row.placed.laps}</td>
                  {#if table.runLength}
                    <td class="num">
                      {time(row.bestRunMicros)}
                    </td>
                  {/if}
                  <td class="num">
                    {time(row.placed.best_lap_micros)}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </section>
    {/if}
  {/if}
</div>

<style>
  .spectator {
    display: grid;
    gap: var(--gf-space-3);
    min-width: 0;
  }
  .notice,
  .empty {
    margin: 0;
    color: var(--gf-text-muted);
  }
  .notice {
    font-size: var(--gf-font-size-sm);
  }
  .race {
    display: flex;
    align-items: baseline;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
    margin-bottom: var(--gf-space-2);
  }
  h2 {
    margin: 0;
    font-size: var(--gf-font-size-lg);
    overflow-wrap: anywhere;
  }
  .clock {
    margin-left: auto;
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
    font-size: var(--gf-font-size-xl);
    font-weight: var(--gf-font-weight-bold);
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--gf-font-size-sm);
  }
  th,
  td {
    text-align: left;
    padding: var(--gf-space-2) var(--gf-space-1);
    border-bottom: 1px solid var(--gf-border-subtle);
    vertical-align: baseline;
  }
  thead th {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
    font-weight: var(--gf-font-weight-semibold);
  }
  tbody th {
    font-weight: var(--gf-font-weight-semibold);
    overflow-wrap: anywhere;
  }
  .num {
    text-align: right;
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .place {
    width: 2.5rem;
    color: var(--gf-text-muted);
    white-space: nowrap;
  }
  .dot {
    display: inline-block;
    width: 0.6rem;
    height: 0.6rem;
    border-radius: 50%;
    margin-right: var(--gf-space-1);
  }
  tr.out th {
    color: var(--gf-text-muted);
  }
  .muted {
    color: var(--gf-text-muted);
    font-weight: var(--gf-font-weight-regular, 400);
    font-size: var(--gf-font-size-xs);
  }
  .heats {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--gf-space-3);
  }
  .heats h3 {
    margin: 0 0 var(--gf-space-1);
    font-size: var(--gf-font-size-md, 1rem);
  }
  .heats ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--gf-space-1);
    font-size: var(--gf-font-size-sm);
  }
  .heats ul li {
    display: flex;
    gap: var(--gf-space-3);
  }
  .seat {
    color: var(--gf-text-muted);
    min-width: 4rem;
  }
  .who {
    overflow-wrap: anywhere;
  }
  .pick select {
    width: 100%;
    padding: var(--gf-space-2);
    background: var(--gf-surface);
    color: var(--gf-text);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    font: inherit;
    margin-bottom: var(--gf-space-2);
  }
  .visually-hidden {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
</style>
