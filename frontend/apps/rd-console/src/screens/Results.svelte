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
    ChannelCatalogEntry,
    Class,
    ClassId,
    ClassStanding,
    CompetitorRef,
    HeatResult,
    HeatSummary,
    Pilot,
    PilotId,
    RankEntry,
    RoundDef,
    RoundId,
    RoundStanding
  } from '@gridfpv/types';
  import { buildResultsExport, downloadJson, toExportJson } from '../lib/results.js';
  import { isTimedQualFormat } from '../lib/formats.js';
  import { createCompetitorNameResolver } from '../lib/competitorName.js';
  import { channelLabel, nodeIndexOf } from '../lib/channels.js';
  import type { AuditPrefilter } from '../lib/auditFilter.svelte.js';
  import type { Session } from '../lib/session.svelte.js';

  let {
    session,
    heatResult = undefined,
    standings = undefined,
    onviewaudit = undefined
  }: {
    session?: Session;
    heatResult?: HeatResult;
    standings?: RankEntry[];
    /**
     * Jump to the event-wide Audit page pre-filtered to a pilot (each ROUND-standings row's
     * "audit" affordance — the defensible-results answer to "why is this pilot placed here?").
     * The shell wires this to the auditFilter seam (`openAudit(setTab, prefilter)`).
     */
    onviewaudit?: (prefilter: AuditPrefilter) => void;
  } = $props();

  const hasEventProjection = $derived(!!heatResult || !!(standings && standings.length));

  // --- Directory (class names + callsigns) -------------------------------------------------------
  // The class directory (class id → name), the pilot directory (callsigns), and the channel catalog.
  // Every displayed competitor resolves through the SHARED resolver (`createCompetitorNameResolver`,
  // the same one Live control + Marshaling use) — never the raw ref / `node-0` (CLAUDE.md). Class
  // names resolve through `className`, which shows a neutral placeholder while the directory loads
  // rather than flashing the raw id.
  let classes = $state<Class[]>([]);
  let pilots = $state<Pilot[]>([]);
  let catalog = $state<ChannelCatalogEntry[]>([]);
  // Loaded-flags so a not-yet-resolved name renders a neutral placeholder ("—") instead of the raw
  // id while the directory read is still in flight (the flash-the-raw-id bug).
  let classesLoaded = $state(false);
  let pilotsLoaded = $state(false);

  const eventClassIds = $derived<ClassId[]>(session?.currentEvent?.classes ?? []);
  const eventClasses = $derived<Class[]>(
    eventClassIds.map((id) => classes.find((c) => c.id === id)).filter((c): c is Class => !!c)
  );
  // A class id → its friendly name; never the raw id. A neutral placeholder shows while the directory
  // loads or for an unknown id (CLAUDE.md: never print the raw id to the screen).
  const className = (id: ClassId): string =>
    classesLoaded ? (classes.find((c) => c.id === id)?.name ?? '—') : '—';

  $effect(() => {
    if (!session) return;
    session
      .listClasses()
      .then((list) => ((classes = list), (classesLoaded = true)))
      .catch((e) => toast.error(e instanceof Error ? e.message : String(e)));
    session
      .listPilots()
      .then((list) => ((pilots = list), (pilotsLoaded = true)))
      .catch((e) => toast.error(e instanceof Error ? e.message : String(e)));
    session
      .listChannels()
      .then((list) => (catalog = list))
      .catch(() => (catalog = []));
  });

  // ── Friendly competitor names (the shared resolver) ──
  // A ref resolves to: (1) an explicit Register binding's callsign, (2) the ref-as-pilot-id callsign
  // (the common roster-seeded heat), (3) an open-practice `node-{i}` seat's channel label, else (4)
  // the bare handle. Results aggregates across heats, so the channel map is the UNION of every heat's
  // frequency assignment, and the explicit bindings are the union of every heat's DURABLE
  // heat-window bindings (`session.heatBindings`, the Marshaling source) — the global live stream
  // only carries the CURRENT heat's progress, so a FINISHED node-seeded heat's seats rendered raw
  // `node-0` in the placements + JSON export. The live current heat's progress merges on top so a
  // just-made Register resolves before its heat-window snapshot lands.
  const pilotById = $derived(new Map<PilotId, Pilot>(pilots.map((p) => [p.id, p])));
  $effect(() => {
    if (!session) return;
    const ids = heats.map((h) => h.heat);
    if (ids.length > 0) void session.ensureHeatBindings(ids);
  });
  const explicitPilotByRef = $derived.by(() => {
    const map = new Map<CompetitorRef, PilotId>();
    if (!session) return map;
    for (const h of heats) {
      const bound = session.heatBindings.get(h.heat);
      if (bound) for (const [ref, pid] of bound) map.set(ref, pid);
    }
    for (const p of session.liveState?.progress ?? [])
      if (p.pilot != null) map.set(p.competitor, p.pilot);
    return map;
  });
  const channelByRef = $derived.by(() => {
    const map = new Map<CompetitorRef, string>();
    for (const h of heats)
      for (const [ref, mhz] of h.frequencies ?? []) map.set(ref, channelLabel(mhz, catalog));
    return map;
  });
  const competitorName = $derived.by<(ref: CompetitorRef) => string>(() =>
    createCompetitorNameResolver({ pilotById, explicitPilotByRef, channelByRef })
  );

  // The display name for a competitor ref. Shows "—" while the pilot directory loads (so a
  // roster-seeded ref never flashes as its raw id), and a neutral "Node N" for an unresolved
  // open-practice seat (never the raw `node-0`). A genuine free-text sim handle is its own label.
  function resolveName(ref: CompetitorRef): string {
    if (!pilotsLoaded) return '—';
    const name = competitorName(ref);
    if (name === ref) {
      const idx = nodeIndexOf(ref);
      if (idx !== undefined) return `Node ${idx + 1}`;
    }
    return name;
  }

  // --- Rounds + heats (the phase inputs) --------------------------------------------------------
  // The event's rounds (read off `currentEvent`, kept live by the session) and the scheduled-heats
  // list (re-fetched whenever the stream advances, so a freshly-scored heat moves the phase).
  const rounds = $derived<RoundDef[]>(session?.currentEvent?.rounds ?? []);
  let heats = $state<HeatSummary[]>([]);
  let heatsLoaded = $state(false);
  // Bumped by the per-view "Try again" button; the fetch effects read it so a retry re-runs them.
  let reloadNonce = $state(0);
  function retry() {
    reloadNonce += 1;
  }

  // A heats-load FAILURE must be visible (#340): the old swallow-into-`[]` silently blanked the
  // phase default + the channel-label map with no hint anything was wrong. Keep the last good
  // list, toast once on the transition into the error state, and offer the explicit retry.
  let heatsError = $state(false);
  async function refreshHeats() {
    if (!session) return;
    try {
      heats = await session.listHeats();
      heatsError = false;
    } catch {
      if (!heatsError) toast.error('Couldn’t load the heats list — showing the last good data.');
      heatsError = true;
    } finally {
      heatsLoaded = true;
    }
  }
  $effect(() => {
    if (!session) return;
    // Touch the protocol state so a freshly scheduled/scored heat re-reads the list, and the
    // retry nonce so "Try again" re-runs a failed read.
    void session.protocolState;
    void reloadNonce;
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
  // A load FAILURE is distinct from a genuinely-empty round: track it so the view can show a
  // "Couldn't load — retry" state instead of the misleading "nothing scored yet" empty state (P1-5).
  let rankingError = $state(false);
  // Latest-wins guard (non-reactive): flipping the view selector re-runs the effect, but a SLOWER
  // earlier response could land after the newer one and leave the wrong table rendered until the
  // next stream tick. Only the newest fetch (matching sequence stamp) may assign.
  let rankingSeq = 0;
  $effect(() => {
    if (!session) return;
    const rid = rankingRoundId;
    void session.liveState;
    void reloadNonce;
    const seq = ++rankingSeq;
    if (!rid) {
      rankingRows = [];
      return;
    }
    rankingLoading = true;
    rankingError = false;
    session
      .roundRanking(rid)
      .then((rows) => {
        if (seq !== rankingSeq) return;
        rankingRows = rows;
        rankingError = false;
      })
      .catch(() => {
        if (seq !== rankingSeq) return;
        rankingRows = [];
        rankingError = true;
      })
      .finally(() => {
        if (seq === rankingSeq) rankingLoading = false;
      });
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
  let roundStandingsError = $state(false);
  // Latest-wins guard — see `rankingSeq`.
  let roundStandingsSeq = 0;
  $effect(() => {
    if (!session) return;
    const rid = timedQualRoundId;
    void session.liveState;
    void reloadNonce;
    const seq = ++roundStandingsSeq;
    if (!rid) {
      roundStandingRows = [];
      return;
    }
    roundStandingsLoading = true;
    roundStandingsError = false;
    session
      .roundStandings(rid)
      .then((rows) => {
        if (seq !== roundStandingsSeq) return;
        roundStandingRows = rows;
        roundStandingsError = false;
      })
      .catch(() => {
        if (seq !== roundStandingsSeq) return;
        roundStandingRows = [];
        roundStandingsError = true;
      })
      .finally(() => {
        if (seq === roundStandingsSeq) roundStandingsLoading = false;
      });
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
    if (!m) return '';
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
  let classError = $state(false);
  // Latest-wins guard — see `rankingSeq`.
  let classSeq = 0;
  $effect(() => {
    if (!session) return;
    const cls = selectedClassId;
    void session.liveState;
    void reloadNonce;
    const seq = ++classSeq;
    if (cls === '') {
      classRows = [];
      return;
    }
    classLoading = true;
    classError = false;
    session
      .classStandings(cls)
      .then((s) => {
        if (seq !== classSeq) return;
        classRows = s.standings;
        classError = false;
      })
      .catch(() => {
        if (seq !== classSeq) return;
        classRows = [];
        classError = true;
      })
      .finally(() => {
        if (seq === classSeq) classLoading = false;
      });
  });

  const roundLabelFor = (id: RoundId): string => rounds.find((r) => r.id === id)?.label ?? '—';

  // Export the CURRENT views with FRIENDLY names baked in (P1-2): competitor refs → callsigns and
  // class/round ids → their labels, so the JSON is human-usable, not a raw-ref wire dump. The raw ref
  // is preserved alongside (`competitor_ref`) so the export stays traceable. The legacy event-level
  // projection (`heatResult` / `standings`) resolves the same way (#341).
  function exportAll() {
    const payload = buildResultsExport({
      resolveCompetitor: resolveName,
      className: selectedClassId !== '' ? className(selectedClassId) : undefined,
      classStandings: selectedClassId !== '' ? classRows : undefined,
      roundLabel: rankingRoundId ? roundLabelFor(rankingRoundId) : undefined,
      roundRanking: rankingRoundId ? rankingRows : undefined,
      standings,
      heatResult
    });
    downloadJson('gridfpv-results.json', toExportJson(payload));
  }
</script>

<section class="results" aria-label="Results">
  <header class="head">
    <h2>Results</h2>
    <Button variant="secondary" onclick={exportAll}>Export JSON</Button>
  </header>

  {#if session}
    {#if heatsError}
      <!-- A failed heats read (#340): the last good list is kept (the phase default + channel
           labels keep working off it) — but the failure must be visible, with an explicit retry. -->
      <div class="load-error" role="alert">
        <p>Couldn't load the heats list — showing the last good data.</p>
        <Button variant="secondary" size="sm" onclick={retry}>Try again</Button>
      </div>
    {/if}
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
          {:else if roundStandingsError}
            {@render loadError()}
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
        {:else if rankingError}
          {@render loadError()}
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
        {:else if classError}
          {@render loadError()}
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
                  <td class="pilot">{resolveName(row.competitor)}</td>
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

{#snippet loadError()}
  <div class="load-error" role="alert">
    <p>Couldn't load the standings.</p>
    <Button variant="secondary" size="sm" onclick={retry}>Try again</Button>
  </div>
{/snippet}

{#snippet auditLink(competitor: CompetitorRef)}
  <!-- The per-pilot jump to the Audit page (round views only): every placement is backed by the
       ruling history, and this is the one-click path to it, pre-filtered to the pilot. -->
  {#if onviewaudit}
    <button
      type="button"
      class="audit-link"
      onclick={() => onviewaudit({ pilot: competitor })}
      title="View this pilot’s rulings on the Audit page"
      aria-label={`View audit for ${resolveName(competitor)}`}
    >
      audit
    </button>
  {/if}
{/snippet}

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
          <td class="pilot">{resolveName(row.competitor)}{@render auditLink(row.competitor)}</td>
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
          <td class="pilot">{resolveName(row.competitor)}{@render auditLink(row.competitor)}</td>
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
  .load-error {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--gf-space-3);
    color: var(--gf-text-secondary);
    font-size: var(--gf-font-size-md);
  }
  .load-error p {
    margin: 0;
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
  /* The per-pilot Audit-page jump: a quiet pill after the callsign (round views). */
  .audit-link {
    margin-left: var(--gf-space-2);
    padding: 0.05em 0.55em;
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-pill);
    background: transparent;
    color: var(--gf-text-muted);
    font-family: inherit;
    font-size: var(--gf-font-size-2xs);
    font-weight: var(--gf-font-weight-semibold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    cursor: pointer;
    vertical-align: middle;
  }
  .audit-link:hover {
    border-color: var(--gf-accent);
    color: var(--gf-accent);
  }
  .audit-link:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
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
