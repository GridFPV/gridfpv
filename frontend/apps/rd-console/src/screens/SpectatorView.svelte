<script lang="ts">
  /**
   * SpectatorView — the open event for someone who is watching (docs/primitives.md §11.2).
   *
   * Three things, one at a time on a phone: the race that is **on now** (or how the last
   * one ended), the heats **coming up** with who is in them, and the **results**: pick a
   * phase, then its overall table or one of its races. A race opens to its lap times in a
   * large format.
   *
   * A pilot who is signed in also gets **Sign up**, when the event has sign-up, and **Me**: their own heats with the seat and
   * channel for each, and their races lap by lap. Above everything, an alert tells them
   * when they are a couple of heats away, next, or flying now. It can only reach a phone
   * that has this page open.
   *
   * It reads the same live store the console's pages do, fed from the Director's public
   * routes, and draws with the console's own helpers. It has no button that changes
   * anything: a spectator can only look.
   */
  import { Badge, Button, Tabs, formatMicros } from '@gridfpv/components';
  import type { PhaseId, PublicPilot, RaceId, RaceView } from '@gridfpv/types';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import { ordinal } from '../lib/names.js';
  import { consecutiveLaps, overlayClock, overlayRows } from '../lib/overlay.js';
  import {
    defaultResult,
    findRace,
    heatChoices,
    raceDetail,
    resultPhases,
    resultTable
  } from '../lib/results.js';
  import { myHeats, myRaces, nearnessWords, pilotAlert } from '../lib/pilotView.js';
  import { currentRace } from '../lib/phases.js';
  import { signupPhases } from '../lib/signup.js';
  import { currentRaceView, missingNames, raceWord, upcomingHeats } from '../lib/spectator.js';
  import PilotSignup, { type SignupActions } from './PilotSignup.svelte';
  import RaceLaps from './RaceLaps.svelte';

  let {
    live,
    pilot = undefined,
    buzz = (pattern) => navigator.vibrate?.(pattern),
    signup = undefined
  }: {
    /** What a signed-in pilot can ask for on the sign-up sheet. Left out: looking only. */
    signup?: SignupActions | undefined;
    /** The pilot this phone is signed in as. Left out for a spectator. */
    pilot?: PublicPilot | undefined;
    /** Make the phone buzz. A test passes its own. */
    buzz?: (pattern: number[]) => void;
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

  type Tab = 'me' | 'now' | 'next' | 'signup' | 'results';
  let tab = $state<Tab>('now');

  const view = $derived(live.view);
  const names = $derived(live.names);
  const coming = $derived(upcomingHeats(view, names));

  // The names a visitor is given are only the ones the event uses. When the snapshot
  // mentions someone new, ask for the names again.
  $effect(() => {
    const missing = missingNames(view, live.pilots, live.classes, live.timers);
    if (missing.pilots) void live.reloadPilots();
    if (missing.classes) void live.reloadClasses();
    if (missing.timers) void live.reloadTimers();
  });

  // ── The current race ───────────────────────────────────────────────────────
  /** The Director's clock. Read often while a race is on, and once a second otherwise. */
  let now = $state(0);
  const onNow = $derived(view ? currentRace(view)?.race : undefined);
  $effect(() => {
    now = live.nowMicros();
    const timer = setInterval(() => (now = live.nowMicros()), onNow === undefined ? 1000 : 200);
    return () => clearInterval(timer);
  });
  const current = $derived(currentRaceView(view, names));
  /**
   * The heats after the current one. The heat on the timer is on the Current race tab
   * while it waits to fly, so it is not listed again here.
   */
  const upcoming = $derived(
    current?.kind === 'next' ? coming.filter((heat) => heat.id !== current.heat.id) : coming
  );
  /** The race on the tab, when it is showing one and not the heat that is up next. */
  const shown = $derived(current && current.kind !== 'next' ? current.race : undefined);
  const heat = $derived(
    shown ? view?.phases.find((phase) => phase.def.id === shown.heat) : undefined
  );
  const runLength = $derived(consecutiveLaps(heat?.format));
  const rows = $derived(shown ? overlayRows(shown, names, runLength) : []);
  /** The clock is always there: counting for a race, its last time for a result, zero before. */
  const clock = $derived(overlayClock(shown, now));

  // ── Results ────────────────────────────────────────────────────────────────
  const choices = $derived(view ? resultPhases(view) : []);
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
  /** The heats of the phase picked that have a result, to look at one by one. */
  const heats = $derived(view && resultId !== undefined ? heatChoices(view, resultId, names) : []);
  /** The heat picked by hand. With none, the phase's overall table shows. */
  let pickedHeat = $state<PhaseId | undefined>(undefined);
  const resultHeat = $derived(heats.find((entry) => entry.id === pickedHeat));
  const resultHeatPhase = $derived(
    resultHeat ? view?.phases.find((phase) => phase.def.id === resultHeat.id) : undefined
  );
  /** A heat raced more than once has a result of its own, over all its races. */
  const heatTable = $derived(
    view &&
      resultHeat &&
      resultHeat.races.length > 1 &&
      resultHeatPhase &&
      resultHeatPhase.ranking.length > 0
      ? resultTable(view, resultHeatPhase, names)
      : undefined
  );
  const runLengthOf = (heat: PhaseId) =>
    consecutiveLaps(view?.phases.find((phase) => phase.def.id === heat)?.format);
  /** Who is disqualified from the heat picked: it is said beside their name in its races. */
  const outOfHeat = $derived(
    new Set(
      (resultHeatPhase?.ranking ?? [])
        .filter((placed) => placed.disqualified)
        .map((placed) => placed.pilot)
    )
  );
  const isOut = (race: RaceView, key: string) => {
    const seat = race.seats.find((entry) => `${entry.adapter}\u0000${entry.seat}` === key);
    return seat?.pilot !== undefined && outOfHeat.has(seat.pilot);
  };

  // ── One race's lap times, opened large ─────────────────────────────────────
  let lapsOf = $state<RaceId | undefined>(undefined);
  const lapsRace = $derived(
    view && lapsOf !== undefined ? findRace(view, lapsOf, names) : undefined
  );

  // ── The pilot's own ────────────────────────────────────────────────────────
  const mine = $derived(pilot ? myHeats(view, names, pilot.id) : []);
  const flown = $derived(pilot ? myRaces(view, names, pilot.id) : []);
  const alert = $derived(pilotAlert(mine));
  // Buzz once each time the alert is about something new: closer, or another heat.
  let buzzed = $state<string | undefined>(undefined);
  $effect(() => {
    const key = alert?.key;
    if (key === buzzed) return;
    buzzed = key;
    if (!alert) return;
    try {
      buzz(alert.nearness === 'soon' ? [200] : [300, 150, 300]);
    } catch {
      // A phone that cannot buzz, or will not until it is touched, still shows the alert.
    }
  });
  const sheets = $derived(signupPhases(view));

  // A pilot who signs in lands on their own tab. One who signs out leaves it.
  let wasPilot = $state(false);
  $effect(() => {
    const isPilot = pilot !== undefined;
    if (isPilot && !wasPilot) tab = 'me';
    if (!isPilot && tab === 'me') tab = 'now';
    wasPilot = isPilot;
  });

  const tabs = $derived([
    ...(pilot ? [{ id: 'me', label: 'Me', badge: mine.length > 0 ? mine.length : undefined }] : []),
    { id: 'now', label: 'Current race' },
    { id: 'next', label: 'Up next', badge: upcoming.length > 0 ? upcoming.length : undefined },
    // Only for a pilot, and only when there is something to sign up for.
    ...(pilot && sheets.length > 0 ? [{ id: 'signup', label: 'Sign up' }] : []),
    { id: 'results', label: 'Results' }
  ]);
  // A tab that has gone (sign-up was turned off) must not leave the page empty.
  $effect(() => {
    if (tab === 'signup' && (sheets.length === 0 || pilot === undefined)) tab = 'now';
  });
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

    {#if alert}
      <div class="alert" data-nearness={alert.nearness} role="status" data-testid="pilot-alert">
        <strong>{alert.title}</strong>
        <span>{alert.detail}</span>
      </div>
    {/if}

    <Tabs items={tabs} bind:value={tab} ariaLabel="What to show" />

    {#if tab === 'me' && pilot}
      <section aria-label="Me">
        <h2 class="part">My heats</h2>
        {#if mine.length === 0}
          <p class="empty" data-testid="no-heats">You are not in a heat that is coming up.</p>
        {:else}
          <ol class="mine" aria-label="My heats">
            {#each mine as entry (entry.id)}
              <li data-nearness={entry.nearness}>
                <span class="when">{nearnessWords(entry)}</span>
                <span class="what">
                  <strong>{entry.name}</strong>
                  <span class="where">
                    {entry.seatName}{#if entry.channel}{' · '}<span data-testid="my-channel"
                        >{entry.channel}</span
                      >{/if}
                  </span>
                </span>
              </li>
            {/each}
          </ol>
        {/if}

        <h2 class="part">My races</h2>
        {#if flown.length === 0}
          <p class="empty" data-testid="no-races">You have not flown a race that counts yet.</p>
        {:else}
          <ul class="flown" aria-label="My races">
            {#each flown as entry (entry.key)}
              <li>
                <div class="flown-head">
                  <strong>{entry.name}</strong>
                  {#if entry.place !== undefined}<Badge tone="accent">{ordinal(entry.place)}</Badge
                    >{/if}
                  {#if entry.falseStart}<Badge tone="danger">False start</Badge>{/if}
                  {#if entry.out}<Badge tone="warn">Out</Badge>{/if}
                  <span class="open">
                    <Button
                      size="sm"
                      variant="secondary"
                      aria-label="Lap times of {entry.name}"
                      onclick={() => (lapsOf = entry.key)}>Lap times</Button
                    >
                  </span>
                </div>
                {#if entry.laps.length === 0}
                  <p class="empty">No laps.</p>
                {:else}
                  <ol class="laps" aria-label="Laps of {entry.name}">
                    {#each entry.laps as micros, index (index)}
                      <li class:best={index === entry.bestLapIndex}>
                        <span class="lap-no">{index + 1}</span>
                        <span class="num">{time(micros)}</span>
                      </li>
                    {/each}
                  </ol>
                  <p class="totals">
                    Best <span class="num">{time(entry.bestLapMicros)}</span> · Total
                    <span class="num">{time(entry.totalMicros)}</span>
                  </p>
                {/if}
              </li>
            {/each}
          </ul>
        {/if}
      </section>
    {/if}

    {#if tab === 'now'}
      <section aria-label="Current race">
        {#if !current}
          <p class="empty" data-testid="no-race">No race yet.</p>
        {:else}
          <header class="race">
            <h2 data-testid="race-heat">
              {current.kind === 'next' ? current.heat.name : names.phase(current.race.heat)}
            </h2>
            <span class="word">
              <Badge tone={current.kind === 'race' ? 'success' : 'neutral'}>
                <span data-testid="race-word">
                  {current.kind === 'next' ? 'Up next' : raceWord(current.race)}
                </span>
              </Badge>
            </span>
            <span class="clock" role="timer" data-testid="race-clock">{clock.text}</span>
          </header>
          {#if current.kind === 'next'}
            <table aria-label="Pilots in this race">
              <thead>
                <tr>
                  <th scope="col">Seat</th>
                  <th scope="col">Pilot</th>
                </tr>
              </thead>
              <tbody>
                {#each current.heat.seats as seat (`${seat.adapter}\u0000${seat.seat}`)}
                  <tr class:me={pilot !== undefined && seat.pilot === pilot.id}>
                    <td class="seat-cell">{seat.seatName}</td>
                    <th scope="row">{seat.who}</th>
                  </tr>
                {/each}
              </tbody>
            </table>
          {:else}
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
                  <tr class:me={pilot !== undefined && row.pilot === pilot.id}>
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
        {/if}
      </section>
    {:else if tab === 'next'}
      <section aria-label="Up next">
        {#if upcoming.length === 0}
          <p class="empty" data-testid="no-upcoming">Nothing is lined up yet.</p>
        {:else}
          <ol class="heats">
            {#each upcoming as entry (entry.id)}
              <li class="card" aria-label={entry.name}>
                <h3>{entry.name}</h3>
                {#if entry.within}<p class="within">{entry.within}</p>{/if}
                <ul>
                  {#each entry.seats as seat (`${seat.adapter}\u0000${seat.seat}`)}
                    <li class:me={pilot !== undefined && seat.pilot === pilot.id}>
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
    {:else if tab === 'signup' && pilot}
      <section aria-label="Sign up">
        <PilotSignup phases={sheets} {names} {pilot} actions={signup} />
      </section>
    {:else if tab === 'results'}
      <section aria-label="Results">
        {#if choices.length === 0 || !table}
          <p class="empty" data-testid="no-results">No results yet.</p>
        {:else}
          <div class="picks">
            <label class="pick">
              <span class="pick-name">Phase</span>
              <select
                aria-label="Phase"
                value={resultId}
                onchange={(e) => {
                  picked = (e.currentTarget as HTMLSelectElement).value;
                  pickedHeat = undefined;
                }}
              >
                {#each choices as choice (choice.id)}
                  <option value={choice.id}>{choice.label}</option>
                {/each}
              </select>
            </label>
            <label class="pick">
              <span class="pick-name">Show</span>
              <select
                aria-label="Show"
                value={resultHeat?.id ?? ''}
                onchange={(e) =>
                  (pickedHeat = (e.currentTarget as HTMLSelectElement).value || undefined)}
              >
                <option value="">Overall</option>
                {#each heats as entry (entry.id)}
                  <option value={entry.id}>{entry.label}</option>
                {/each}
              </select>
            </label>
          </div>
          {#snippet standings(shown: NonNullable<typeof table>, label: string)}
            <table aria-label={label}>
              <thead>
                <tr>
                  <th scope="col"><span class="visually-hidden">Place</span></th>
                  <th scope="col">Pilot</th>
                  {#if shown.showWins}<th scope="col" class="num">Wins</th>{/if}
                  {#if shown.showPoints}<th scope="col" class="num">Points</th>{/if}
                  <th scope="col" class="num">Laps</th>
                  {#if shown.runLength}<th scope="col" class="num">Best {shown.runLength}</th>{/if}
                  <th scope="col" class="num">Best lap</th>
                </tr>
              </thead>
              <tbody>
                {#each shown.rows as row (row.placed.pilot)}
                  <tr
                    class:out={row.placed.disqualified}
                    class:me={pilot !== undefined && row.placed.pilot === pilot.id}
                  >
                    <td class="place">{ordinal(row.position)}</td>
                    <th scope="row">
                      {row.callsign}
                      {#if row.placed.disqualified}<Badge tone="danger">Disqualified</Badge>{/if}
                      {#if !row.placed.flew}<span class="muted">did not fly</span>{/if}
                    </th>
                    {#if shown.showWins}<td class="num">{row.placed.wins}</td>{/if}
                    {#if shown.showPoints}<td class="num">{row.placed.points}</td>{/if}
                    <td class="num">{row.placed.laps}</td>
                    {#if shown.runLength}
                      <td class="num">{time(row.bestRunMicros)}</td>
                    {/if}
                    <td class="num">{time(row.placed.best_lap_micros)}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/snippet}
          {#if resultHeat}
            {@const runOf = runLengthOf(resultHeat.id)}
            {#if heatTable}
              <h3 class="part">All {resultHeat.races.length} races</h3>
              {@render standings(heatTable, `Result of ${resultHeat.label}`)}
            {/if}
            {#each resultHeat.races as race (race.race)}
              <div class="race-head">
                <h3 class="part" data-testid="result-race">
                  {resultHeat.races.length > 1 ? `Race ${race.number}` : resultHeat.label}
                </h3>
                <Button
                  size="sm"
                  variant="secondary"
                  aria-label="Lap times of {race.name}"
                  onclick={() => (lapsOf = race.race)}>Lap times</Button
                >
              </div>
              <table aria-label={race.name}>
                <thead>
                  <tr>
                    <th scope="col"><span class="visually-hidden">Place</span></th>
                    <th scope="col">Pilot</th>
                    <th scope="col" class="num">Laps</th>
                    {#if runOf}<th scope="col" class="num">Best {runOf}</th>{/if}
                    <th scope="col" class="num">Best lap</th>
                    <th scope="col" class="num">Total</th>
                  </tr>
                </thead>
                <tbody>
                  {#each raceDetail(race.view, names, runOf) as row (row.key)}
                    <tr class:out={isOut(race.view, row.key)}>
                      <td class="place">{row.place === undefined ? '' : ordinal(row.place)}</td>
                      <th scope="row">
                        {row.name}
                        {#if isOut(race.view, row.key)}<Badge tone="danger">Disqualified</Badge
                          >{/if}
                        {#if row.falseStart}<Badge tone="danger">False start</Badge>{/if}
                        {#if row.out}<Badge tone="warn">Out</Badge>{/if}
                      </th>
                      <td class="num">{row.laps}</td>
                      {#if runOf}<td class="num">{time(row.bestRun?.totalMicros)}</td>{/if}
                      <td class="num">{time(row.bestLapMicros)}</td>
                      <td class="num">{time(row.totalMicros)}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            {/each}
          {:else}
            {@render standings(table, 'Results')}
          {/if}
        {/if}
      </section>
    {/if}
  {/if}
</div>

{#if lapsRace}
  <RaceLaps
    name={lapsRace.name}
    race={lapsRace.view}
    {names}
    runLength={runLengthOf(lapsRace.heat)}
    pilot={pilot?.id}
    onclose={() => (lapsOf = undefined)}
  />
{/if}

<style>
  .picks {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--gf-space-2);
    margin-bottom: var(--gf-space-2);
  }
  .pick-name {
    display: block;
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .race-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-2);
    margin-bottom: var(--gf-space-1);
  }
  .open {
    margin-left: auto;
  }
  .spectator {
    display: grid;
    gap: var(--gf-space-3);
    min-width: 0;
  }
  /* Four tabs have to fit across a phone: keep each on one line and give up some padding. */
  .spectator :global(.gf-tab) {
    white-space: nowrap;
    padding-inline: var(--gf-space-2);
  }
  .spectator :global(.gf-tabs) {
    overflow-x: auto;
  }
  /* A pilot at an event with sign-up has five tabs. On a phone they only fit pulled in. */
  @media (max-width: 30rem) {
    .spectator :global(.gf-tab) {
      padding-inline: var(--gf-space-1);
      font-size: var(--gf-font-size-xs);
    }
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
  .seat-cell {
    color: var(--gf-text-muted);
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
  /* One heat to a card, one card to a row, on a phone and on a wide screen alike. */
  .heats {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: var(--gf-space-3);
  }
  .heats > .card {
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-md);
    padding: var(--gf-space-3);
    background: var(--gf-surface);
  }
  .heats h3 {
    margin: 0 0 var(--gf-space-1);
    font-size: var(--gf-font-size-md, 1rem);
  }
  .heats .within {
    margin: 0 0 var(--gf-space-2);
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
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
  /* The pilot's own row, wherever it is. */
  tr.me th,
  li.me .who {
    color: var(--gf-accent);
  }
  .alert {
    display: grid;
    gap: 2px;
    padding: var(--gf-space-3);
    border-radius: var(--gf-radius-md);
    border: 2px solid var(--gf-accent);
    background: var(--gf-accent-soft, var(--gf-surface));
  }
  .alert strong {
    font-size: var(--gf-font-size-lg);
  }
  .alert span {
    font-size: var(--gf-font-size-sm);
    overflow-wrap: anywhere;
  }
  .alert[data-nearness='soon'] {
    border-color: var(--gf-warn);
  }
  .alert[data-nearness='now'],
  .alert[data-nearness='next'] {
    border-color: var(--gf-success);
  }
  .part {
    margin: var(--gf-space-3) 0 var(--gf-space-2);
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .part:first-child {
    margin-top: 0;
  }
  .mine,
  .flown {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--gf-space-2);
  }
  .mine li {
    display: flex;
    gap: var(--gf-space-3);
    align-items: baseline;
    padding: var(--gf-space-2);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-sm);
  }
  .mine li[data-nearness='now'],
  .mine li[data-nearness='next'] {
    border-color: var(--gf-success);
  }
  .when {
    min-width: 6.5rem;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .what {
    display: grid;
    gap: 2px;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .where {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .flown > li {
    padding: var(--gf-space-2);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-sm);
  }
  .flown-head {
    display: flex;
    gap: var(--gf-space-2);
    align-items: center;
    flex-wrap: wrap;
    margin-bottom: var(--gf-space-1);
    overflow-wrap: anywhere;
  }
  .laps {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-1) var(--gf-space-3);
    font-size: var(--gf-font-size-sm);
  }
  .laps li {
    display: flex;
    gap: var(--gf-space-1);
  }
  .laps li.best .num {
    color: var(--gf-success);
    font-weight: var(--gf-font-weight-bold);
  }
  .lap-no {
    color: var(--gf-text-muted);
  }
  .totals {
    margin: var(--gf-space-1) 0 0;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
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
