<script lang="ts">
  /**
   * Results (#56, race redesign Slice 5/6b) — **per-class standings** plus the event-level heat
   * result / ranking / bracket views.
   *
   * The headline is now the season-join **standings per class**: for each class the event selects,
   * `session.classStandings(eventId, classId)` is read and rendered as a standings table —
   * position, pilot callsign (resolved from the directory), points, best lap (µs → `s.mmm`), total
   * laps, and rounds entered, best first. A class selector switches between classes; an empty class
   * (nothing scored yet) shows an inline empty state.
   *
   * The prior event-level projections — a scored `HeatResult` → `Leaderboard`, a `RankEntry[]` →
   * `StandingsTable`, and an `EventOutcome`'s bracket → `BracketTree` — are kept below as a blended
   * "Event projection" section, and export is a lossless JSON download of whichever projection is
   * shown.
   */
  import {
    Leaderboard,
    StandingsTable,
    BracketTree,
    Button,
    Card,
    Select,
    formatMicros,
    toast
  } from '@gridfpv/components';
  import type {
    Class,
    ClassId,
    ClassStanding,
    CompetitorRef,
    HeatResult,
    Pilot,
    RankEntry,
    EventOutcome
  } from '@gridfpv/types';
  import { bracketFromOutcome, downloadJson, toExportJson } from '../lib/results.js';
  import type { Session } from '../lib/session.svelte.js';

  let {
    session,
    heatResult = undefined,
    standings = undefined,
    outcome = undefined,
    metricLabel = 'Best lap'
  }: {
    session?: Session;
    heatResult?: HeatResult;
    standings?: RankEntry[];
    outcome?: EventOutcome;
    metricLabel?: string;
  } = $props();

  const bracket = $derived(outcome ? bracketFromOutcome(outcome) : undefined);
  const hasEventProjection = $derived(
    !!heatResult || !!(standings && standings.length) || !!(bracket && bracket.rounds.length)
  );

  // --- Per-class standings (the season-join read) ----------------------------------------------
  // The directory (to resolve a class id → name and a competitor ref → callsign), the event's
  // selected classes, and the standings rows for the currently-shown class. Loaded once; the rows
  // re-fetch when the chosen class (or the live state) changes, so a freshly-scored heat shows up.

  let classes = $state<Class[]>([]);
  let pilots = $state<Pilot[]>([]);
  let selectedClass = $state<ClassId | ''>('');
  let rows = $state<ClassStanding[]>([]);
  let loading = $state(false);

  const eventClassIds = $derived<ClassId[]>(session?.currentEvent?.classes ?? []);
  const eventClasses = $derived<Class[]>(
    eventClassIds.map((id) => classes.find((c) => c.id === id)).filter((c): c is Class => !!c)
  );
  const className = (id: ClassId): string => classes.find((c) => c.id === id)?.name ?? id;
  const pilotByRef = $derived(new Map(pilots.map((p) => [p.id, p] as const)));
  const callsign = (ref: CompetitorRef): string => pilotByRef.get(ref)?.callsign ?? ref;

  $effect(() => {
    if (!session) return;
    session
      .listClasses()
      .then((list) => (classes = list))
      .catch((e) => toast.error(e instanceof Error ? e.message : String(e)));
    session
      .listPilots()
      .then((list) => (pilots = list))
      .catch((e) => toast.error(e instanceof Error ? e.message : String(e)));
  });

  // Default the selector to the first selected class once classes resolve.
  $effect(() => {
    if (selectedClass === '' && eventClassIds.length > 0) selectedClass = eventClassIds[0];
  });

  // (Re)load the chosen class's standings whenever it changes or the live state advances (a newly
  // scored heat re-aggregates the season-join). A failed read leaves the rows empty.
  $effect(() => {
    const cls = selectedClass;
    // Touch the live state so a just-scored heat re-fetches the standings.
    void session?.liveState;
    if (!session || cls === '') {
      rows = [];
      return;
    }
    loading = true;
    session
      .classStandings(cls)
      .then((s) => (rows = s.standings))
      .catch(() => (rows = []))
      .finally(() => (loading = false));
  });

  function exportAll() {
    const payload = {
      class_standings: selectedClass ? { class: selectedClass, standings: rows } : undefined,
      heatResult,
      standings,
      outcome
    };
    downloadJson('gridfpv-results.json', toExportJson(payload));
  }
</script>

<section class="results" aria-label="Results">
  <header class="head">
    <h2>Results</h2>
    <Button variant="secondary" onclick={exportAll}>Export JSON</Button>
  </header>

  {#if session}
    <Card
      title="Standings"
      subtitle="Per-class season standings, aggregated across the class's rounds."
    >
      {#snippet actions()}
        {#if eventClasses.length > 1}
          <Select bind:value={selectedClass} aria-label="Class">
            {#each eventClasses as cls (cls.id)}
              <option value={cls.id}>{cls.name}</option>
            {/each}
          </Select>
        {/if}
      {/snippet}

      {#if eventClasses.length === 0}
        <p class="empty" role="status">
          This event selects no classes yet. Standings appear here once a class runs a round.
        </p>
      {:else}
        {#if eventClasses.length === 1}
          <p class="class-name">{className(eventClasses[0].id)}</p>
        {/if}
        {#if loading && rows.length === 0}
          <p class="empty" role="status">Loading standings…</p>
        {:else if rows.length === 0}
          <p class="empty" role="status">
            Nothing scored for <strong>{className(selectedClass || eventClasses[0].id)}</strong> yet —
            standings populate as this class's rounds are scored.
          </p>
        {:else}
          <table class="standings" aria-label={`${className(selectedClass)} standings`}>
            <thead>
              <tr>
                <th scope="col" class="pos">Pos</th>
                <th scope="col" class="pilot">Pilot</th>
                <th scope="col" class="num">Points</th>
                <th scope="col" class="num">Best lap</th>
                <th scope="col" class="num">Laps</th>
                <th scope="col" class="num">Rounds</th>
              </tr>
            </thead>
            <tbody>
              {#each rows as row (row.competitor)}
                <tr>
                  <td class="pos"><span class="badge">{row.position}</span></td>
                  <td class="pilot">{callsign(row.competitor)}</td>
                  <td class="num points">{row.points}</td>
                  <td class="num">{formatMicros(row.best_lap_micros)}</td>
                  <td class="num">{row.total_laps}</td>
                  <td class="num">{row.rounds_entered}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      {/if}
    </Card>
  {/if}

  {#if heatResult}
    <Card title="Heat result" pad={false}>
      <Leaderboard result={heatResult} {metricLabel} />
    </Card>
  {/if}

  {#if standings && standings.length > 0}
    <Card title="Ranking" pad={false}>
      <StandingsTable entries={standings} caption="Overall ranking" />
    </Card>
  {/if}

  {#if bracket && bracket.rounds.length > 0}
    <Card title="Bracket">
      <BracketTree {bracket} />
    </Card>
  {/if}

  {#if !session && !hasEventProjection}
    <Card elevation="flat">
      <p class="empty">No results yet. They appear here as heats are scored.</p>
    </Card>
  {/if}
</section>

<style>
  .results {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  h2 {
    font-size: var(--gf-font-size-xl);
    margin: 0;
    letter-spacing: var(--gf-tracking-tight);
  }
  .empty {
    margin: 0;
    color: var(--gf-text-secondary);
    font-size: var(--gf-font-size-md);
    line-height: 1.5;
  }
  .empty strong {
    color: var(--gf-text);
  }
  .class-name {
    margin: 0 0 var(--gf-space-2);
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-semibold);
    color: var(--gf-text);
  }

  .standings {
    border-collapse: collapse;
    width: 100%;
    color: var(--gf-text);
    font-size: var(--gf-font-size-md);
  }
  .standings th,
  .standings td {
    padding: var(--gf-space-3);
    text-align: left;
  }
  .standings thead th {
    color: var(--gf-text-muted);
    font-weight: var(--gf-font-weight-semibold);
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    border-bottom: 1px solid var(--gf-border);
  }
  .standings tbody tr + tr td {
    border-top: 1px solid var(--gf-border-subtle);
  }
  .standings .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .standings .points {
    font-weight: var(--gf-font-weight-bold);
    color: var(--gf-text);
  }
  .standings .pilot {
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
  }
  .standings .pos {
    width: 2.75em;
  }
  .badge {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 1.7em;
    height: 1.7em;
    padding: 0 0.35em;
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-alt, var(--gf-surface-sunken));
    color: var(--gf-text-secondary);
    font-weight: var(--gf-font-weight-bold);
    font-variant-numeric: tabular-nums;
  }
</style>
