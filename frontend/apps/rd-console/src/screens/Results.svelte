<script lang="ts">
  /**
   * Results (#56) — a **phase-aware** results screen for the round / per-class primitives.
   *
   * The screen shows the standings relevant to where the event currently is and lets the RD switch
   * between views with a single selector:
   *
   *  - **Round** — that round's `roundRanking`, rendered as a standings table. A time-trial
   *    (`timed_qual`) round shows the richer per-pilot standings (Best lap + the win-condition metric).
   *  - **Per-class standings** — the season-join `classStandings` per event class.
   *
   * The selector lists each round (by label), then one entry per event class. It **defaults to the
   * current phase**: the latest scored round, else per-class.
   *
   * Friendly names everywhere — competitor refs resolve to callsigns (never the raw ref). The legacy
   * event-level projections (a `RankEntry[]` → `StandingsTable`) are kept below as an "Event
   * projection" section; export is a lossless JSON download of whichever projections are present.
   */
  import { StandingsTable, Button, Card, Select, formatMicros, toast } from '@gridfpv/components';
  import type {
    Class,
    ClassId,
    ClassStanding,
    CompetitorRef,
    HeatResult,
    HeatSummary,
    Pilot,
    RankEntry,
    RoundDef,
    RoundId,
    RoundStanding
  } from '@gridfpv/types';
  import { downloadJson, toExportJson } from '../lib/results.js';
  import { isTimedQualFormat } from '../lib/formats.js';
  import type { Session } from '../lib/session.svelte.js';

  let {
    session,
    heatResult = undefined,
    standings = undefined
  }: {
    session?: Session;
    heatResult?: HeatResult;
    standings?: RankEntry[];
  } = $props();

  const hasEventProjection = $derived(!!heatResult || !!(standings && standings.length));

  // --- Directory (class names + callsigns) -------------------------------------------------------
  // Resolved once: the class directory (class id → name) and the pilot directory (competitor ref →
  // callsign). Every displayed competitor resolves through `callsign` — never the raw ref (CLAUDE.md).
  let classes = $state<Class[]>([]);
  let pilots = $state<Pilot[]>([]);

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

  // --- Rounds + heats (the phase inputs) --------------------------------------------------------
  // The event's rounds (read off `currentEvent`, kept live by the session) and the scheduled-heats
  // list (re-fetched whenever the stream advances, so a freshly-scored heat moves the phase).
  const rounds = $derived<RoundDef[]>(session?.currentEvent?.rounds ?? []);
  let heats = $state<HeatSummary[]>([]);
  let heatsLoaded = $state(false);

  async function refreshHeats() {
    if (!session) return;
    try {
      heats = await session.listHeats();
    } catch {
      heats = [];
    } finally {
      heatsLoaded = true;
    }
  }
  $effect(() => {
    if (!session) return;
    // Touch the protocol state so a freshly scheduled/scored heat re-reads the list.
    void session.protocolState;
    void refreshHeats();
  });

  const heatsByRound = (id: RoundId): HeatSummary[] => heats.filter((h) => h.round === id);

  // --- The view selector --------------------------------------------------------------------------
  // The pickable views, in order: each round (by label), then one per-class entry per event class.
  type ViewOption =
    | { value: string; label: string; kind: 'round'; round: RoundDef }
    | { value: string; label: string; kind: 'class'; classId: ClassId };

  const viewOptions = $derived.by<ViewOption[]>(() => {
    const opts: ViewOption[] = [];
    for (const round of rounds) {
      opts.push({ value: `round:${round.id}`, label: round.label, kind: 'round', round });
    }
    for (const cls of eventClasses) {
      opts.push({ value: `class:${cls.id}`, label: cls.name, kind: 'class', classId: cls.id });
    }
    return opts;
  });

  let selectedView = $state('');
  const currentView = $derived<ViewOption | undefined>(
    viewOptions.find((o) => o.value === selectedView)
  );

  // --- Phase default ------------------------------------------------------------------------------
  // Infer the current phase from rounds + heats (no explicit phase field). Computed once the heats
  // have loaded so a not-yet-scored event correctly falls through to per-class.
  function defaultViewValue(): string | undefined {
    // The latest round with a finalized heat.
    const scoredRounds = rounds.filter((r) => heatsByRound(r.id).some((h) => h.phase === 'Final'));
    if (scoredRounds.length > 0) return `round:${scoredRounds[scoredRounds.length - 1].id}`;

    // Else per-class (the first event class).
    if (eventClassIds.length > 0) return `class:${eventClassIds[0]}`;
    return viewOptions[0]?.value;
  }

  let viewInitialized = $state(false);
  $effect(() => {
    if (viewInitialized || !heatsLoaded) return;
    if (viewOptions.length === 0) return;
    const def = defaultViewValue();
    if (def) {
      selectedView = def;
      viewInitialized = true;
    }
  });

  // --- Round ranking ----------------------------------------------------------------------------
  // The ranking shown for a round view (the round's own ranking). Re-fetched when the displayed round
  // or the live state changes, so a freshly-scored heat re-aggregates.
  const rankingRoundId = $derived.by<RoundId | undefined>(() =>
    currentView?.kind === 'round' ? currentView.round.id : undefined
  );
  let rankingRows = $state<RankEntry[]>([]);
  let rankingLoading = $state(false);
  $effect(() => {
    if (!session) return;
    const rid = rankingRoundId;
    void session.liveState;
    if (!rid) {
      rankingRows = [];
      return;
    }
    rankingLoading = true;
    session
      .roundRanking(rid)
      .then((rows) => (rankingRows = rows))
      .catch(() => (rankingRows = []))
      .finally(() => (rankingLoading = false));
  });

  // --- Time-trial round standings (Best lap + the win-condition metric) -------------------------
  // A TIME-TRIAL (timed_qual) round view shows the richer per-pilot standings — each pilot's best
  // single lap plus the metric the round is won by (best lap / best-N-consecutive / most laps) —
  // instead of the bare ranking. Fetched only for a timed_qual round view, refreshed on a live advance.
  const timedQualRoundId = $derived.by<RoundId | undefined>(() => {
    const v = currentView;
    return v?.kind === 'round' && isTimedQualFormat(v.round.format) ? v.round.id : undefined;
  });
  let roundStandingRows = $state<RoundStanding[]>([]);
  let roundStandingsLoading = $state(false);
  $effect(() => {
    if (!session) return;
    const rid = timedQualRoundId;
    void session.liveState;
    if (!rid) {
      roundStandingRows = [];
      return;
    }
    roundStandingsLoading = true;
    session
      .roundStandings(rid)
      .then((rows) => (roundStandingRows = rows))
      .catch(() => (roundStandingRows = []))
      .finally(() => (roundStandingsLoading = false));
  });

  // The win-condition metric column for the time-trial table: its header (or `undefined` when the
  // metric is Best lap, which the dedicated Best-lap column already covers), and each row's value. The
  // metric is round-wide (the win condition), so the header reads off the first standing.
  const ttMetricHeader = $derived.by<string | undefined>(() => {
    const m = roundStandingRows[0]?.metric;
    if (!m) return undefined;
    if ('BestConsecutive' in m) return `Best ${m.BestConsecutive.n} consec`;
    if ('MostLaps' in m) return 'Laps';
    return undefined;
  });
  function ttMetricValue(s: RoundStanding): string {
    const m = s.metric;
    if ('BestConsecutive' in m) return formatMicros(m.BestConsecutive.micros);
    if ('MostLaps' in m) return String(m.MostLaps.laps);
    return '';
  }

  // --- Per-class standings ----------------------------------------------------------------------
  // The season-join read for the selected class, re-fetched on class change or a live advance.
  const selectedClassId = $derived<ClassId | ''>(
    currentView?.kind === 'class' ? currentView.classId : ''
  );
  let classRows = $state<ClassStanding[]>([]);
  let classLoading = $state(false);
  $effect(() => {
    if (!session) return;
    const cls = selectedClassId;
    void session.liveState;
    if (cls === '') {
      classRows = [];
      return;
    }
    classLoading = true;
    session
      .classStandings(cls)
      .then((s) => (classRows = s.standings))
      .catch(() => (classRows = []))
      .finally(() => (classLoading = false));
  });

  function exportAll() {
    const payload = {
      class_standings:
        selectedClassId !== '' ? { class: selectedClassId, standings: classRows } : undefined,
      round_ranking: rankingRoundId ? { round: rankingRoundId, ranking: rankingRows } : undefined,
      heatResult,
      standings
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
      title={currentView?.kind === 'round' ? `${currentView.label} standings` : 'Standings'}
      subtitle={currentView?.kind === 'class'
        ? "Per-class season standings, aggregated across the class's rounds."
        : undefined}
    >
      {#snippet actions()}
        {#if viewOptions.length > 1}
          <Select bind:value={selectedView} aria-label="Results view">
            {#each viewOptions as opt (opt.value)}
              <option value={opt.value}>{opt.label}</option>
            {/each}
          </Select>
        {/if}
      {/snippet}

      {#if viewOptions.length === 0}
        <p class="empty" role="status">No results yet. They appear here as rounds are scored.</p>
      {:else if currentView?.kind === 'round'}
        {#if isTimedQualFormat(currentView.round.format)}
          {#if roundStandingsLoading && roundStandingRows.length === 0}
            <p class="empty" role="status">Loading standings…</p>
          {:else if roundStandingRows.length === 0}
            <p class="empty" role="status">
              Nothing scored for <strong>{currentView.label}</strong> yet — standings populate as the
              round is scored.
            </p>
          {:else}
            {@render ttTable(currentView.label, roundStandingRows)}
          {/if}
        {:else if rankingLoading && rankingRows.length === 0}
          <p class="empty" role="status">Loading standings…</p>
        {:else if rankingRows.length === 0}
          <p class="empty" role="status">
            Nothing scored for <strong>{currentView.label}</strong> yet — standings populate as the round
            is scored.
          </p>
        {:else}
          {@render rankTable(`${currentView.label} standings`, rankingRows)}
        {/if}
      {:else if currentView?.kind === 'class'}
        {#if classLoading && classRows.length === 0}
          <p class="empty" role="status">Loading standings…</p>
        {:else if classRows.length === 0}
          <p class="empty" role="status">
            Nothing scored for <strong>{className(currentView.classId)}</strong> yet — standings populate
            as this class's rounds are scored.
          </p>
        {:else}
          <table class="standings" aria-label={`${className(currentView.classId)} standings`}>
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
              {#each classRows as row (row.competitor)}
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

  {#if standings && standings.length > 0}
    <Card title="Ranking" pad={false}>
      <StandingsTable entries={standings} caption="Overall ranking" />
    </Card>
  {/if}

  {#if !session && !hasEventProjection}
    <Card elevation="flat">
      <p class="empty">No results yet. They appear here as heats are scored.</p>
    </Card>
  {/if}
</section>

{#snippet rankTable(caption: string, rows: RankEntry[])}
  <table class="standings" aria-label={caption}>
    <thead>
      <tr>
        <th scope="col" class="pos">Pos</th>
        <th scope="col" class="pilot">Pilot</th>
      </tr>
    </thead>
    <tbody>
      {#each rows as row (row.competitor)}
        <tr>
          <td class="pos"><span class="badge">{row.position}</span></td>
          <td class="pilot">{callsign(row.competitor)}</td>
        </tr>
      {/each}
    </tbody>
  </table>
{/snippet}

{#snippet ttTable(label: string, rows: RoundStanding[])}
  <table class="standings" aria-label={`${label} standings`}>
    <thead>
      <tr>
        <th scope="col" class="pos">Pos</th>
        <th scope="col" class="pilot">Pilot</th>
        <th scope="col" class="num">Best lap</th>
        {#if ttMetricHeader}
          <th scope="col" class="num">{ttMetricHeader}</th>
        {/if}
      </tr>
    </thead>
    <tbody>
      {#each rows as row (row.competitor)}
        <tr>
          <td class="pos"><span class="badge">{row.position}</span></td>
          <td class="pilot">{callsign(row.competitor)}</td>
          <td class="num">{formatMicros(row.best_lap_micros)}</td>
          {#if ttMetricHeader}
            <td class="num">{ttMetricValue(row)}</td>
          {/if}
        </tr>
      {/each}
    </tbody>
  </table>
{/snippet}

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
