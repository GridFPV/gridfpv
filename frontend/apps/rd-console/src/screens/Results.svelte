<script lang="ts">
  /**
   * Results (#56, race redesign Slice 5/6b) — a **phase-aware** results screen.
   *
   * Rather than only the per-class season standings, this screen shows the standings relevant to
   * where the event currently is and lets the RD switch between views with a single selector:
   *
   *  - **Tournament** (a bracket root) — the champion (when decided), the bracket **tree** stitched
   *    from the chain's level rounds + heats (chase-final aware, collapsing the N races into one
   *    node), and the **final standings** (the final level's ranking).
   *  - **Round** (a non-bracket round) — that round's `roundRanking`, rendered as a standings table.
   *  - **Per-class standings** — the season-join `classStandings` per event class, as before.
   *
   * The selector lists, in order: each tournament (by bracket name), then each non-bracket round (by
   * label), then one entry per event class. It **defaults to the current phase**: the most-recently-
   * created tournament that has run, else the latest scored round, else per-class.
   *
   * Friendly names everywhere — competitor refs resolve to callsigns (never the raw ref), bracket
   * levels render their level name, and a tournament reads by its bracket name. The legacy event-level
   * projections (a scored `HeatResult` → `Leaderboard`, a `RankEntry[]` → `StandingsTable`, an
   * `EventOutcome` bracket) are kept below as a blended "Event projection" section; export is a
   * lossless JSON download of whichever projections are present.
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
  import type { Bracket } from '@gridfpv/components';
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
    EventOutcome
  } from '@gridfpv/types';
  import { bracketFromOutcome, downloadJson, toExportJson } from '../lib/results.js';
  import {
    bracketChainRounds,
    buildBracketView,
    chaseWinTally,
    isBracketLevel,
    isBracketRoot,
    isLevelComplete,
    splitBracketLabel
  } from '../lib/brackets.js';
  import type { ChaseFinalTally } from '../lib/brackets.js';
  import { isChaseTheAceFormat } from '../lib/formats.js';
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
  // The pickable views, in order: each tournament (bracket root, by bracket name), then each
  // non-bracket round (by label), then one per-class entry per event class.
  type ViewOption =
    | { value: string; label: string; kind: 'tournament'; root: RoundDef }
    | { value: string; label: string; kind: 'round'; round: RoundDef }
    | { value: string; label: string; kind: 'class'; classId: ClassId };

  const tournamentRoots = $derived<RoundDef[]>(rounds.filter(isBracketRoot));
  const plainRounds = $derived<RoundDef[]>(rounds.filter((r) => !isBracketLevel(r)));

  const viewOptions = $derived.by<ViewOption[]>(() => {
    const opts: ViewOption[] = [];
    for (const root of tournamentRoots) {
      const name = splitBracketLabel(root.label).name || root.label;
      opts.push({ value: `tournament:${root.id}`, label: name, kind: 'tournament', root });
    }
    for (const round of plainRounds) {
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
    // The most-recently-created tournament whose chain has any heat (scheduled / running / final).
    const runRoots = tournamentRoots.filter((root) => {
      const ids = new Set(bracketChainRounds(root, rounds).map((r) => r.id));
      return heats.some((h) => h.round !== undefined && ids.has(h.round));
    });
    if (runRoots.length > 0) return `tournament:${runRoots[runRoots.length - 1].id}`;

    // Else the latest non-bracket round with a finalized heat.
    const scoredRounds = plainRounds.filter((r) =>
      heatsByRound(r.id).some((h) => h.phase === 'Final')
    );
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

  // --- Tournament champion + chase tally (mirrors EventRounds) ------------------------------------
  // A bracket root's champion + (for a chase final) its series tally. Mirrors EventRounds so the
  // header chip + the bracket tree agree, loaded lazily per root and refreshed as levels complete.
  let championByRoot = $state<Record<RoundId, CompetitorRef | undefined>>({});
  $effect(() => {
    if (!session) return;
    void heats;
    for (const root of tournamentRoots) {
      const chain = bracketChainRounds(root, rounds);
      const final = chain[chain.length - 1];
      // A chase final's champion comes from its race-win tally (below), not a single-heat ranking.
      if (!final || isChaseTheAceFormat(final.format)) continue;
      const finalHeats = heatsByRound(final.id);
      if (finalHeats.length === 1 && isLevelComplete(final.id, heats)) {
        if (championByRoot[root.id] === undefined) {
          session
            .roundRanking(final.id)
            .then((rows) => {
              const top = rows.find((r) => r.position === 1)?.competitor;
              if (top) championByRoot = { ...championByRoot, [root.id]: top };
            })
            .catch(() => {});
        }
      }
    }
  });

  // The chase-final series tally: the engine ranking exposes only an overall placement, so the
  // frontend counts the race winners. Fetch each completed race result once (guarded), cache it, and
  // replay the cache into a per-root tally (wins per finalist + the champion).
  let chaseResultByHeat = $state<Record<string, HeatResult>>({});
  const chaseFetchedHeats = new Set<string>();
  let chaseTallyByRoot = $state<Record<RoundId, ChaseFinalTally>>({});
  function chaseRaceIndex(heatId: string): number {
    const m = /^cta-r(\d+)$/.exec(heatId);
    return m ? Number(m[1]) : Number.MAX_SAFE_INTEGER;
  }
  $effect(() => {
    if (!session) return;
    void heats;
    const cache = chaseResultByHeat;
    const next: Record<RoundId, ChaseFinalTally> = {};
    for (const root of tournamentRoots) {
      const chain = bracketChainRounds(root, rounds);
      const final = chain[chain.length - 1];
      if (!final || !isChaseTheAceFormat(final.format)) continue;
      const completed = heatsByRound(final.id)
        .filter((h) => h.phase === 'Final')
        .sort((a, b) => chaseRaceIndex(a.heat) - chaseRaceIndex(b.heat));
      for (const h of completed) {
        if (!chaseFetchedHeats.has(h.heat)) {
          chaseFetchedHeats.add(h.heat);
          session
            .fetchHeatResult(h.heat)
            .then((res) => {
              if (res) chaseResultByHeat = { ...chaseResultByHeat, [h.heat]: res };
            })
            .catch(() => {});
        }
      }
      const target = Math.max(1, Math.round(Number(final.params?.wins_to_win ?? 2)));
      const results = completed
        .map((h) => cache[h.heat])
        .filter((r): r is HeatResult => r !== undefined);
      next[root.id] = chaseWinTally(results, target);
    }
    chaseTallyByRoot = next;
  });

  function championOf(root: RoundDef): CompetitorRef | undefined {
    return chaseTallyByRoot[root.id]?.champion ?? championByRoot[root.id];
  }

  // The roster size of a class — its membership count off the event (the level-1 field for a
  // roster-seeded bracket). Mirrors EventRounds' classRosterSize.
  function classRosterSize(classId: ClassId): number {
    const membership = session?.currentEvent?.classes_membership ?? [];
    return membership.find((m) => m.class === classId)?.pilots.length ?? 0;
  }

  // The BracketTree view-model for a tournament root — level columns stitched from the chain rounds
  // + their heats, winners inferred from the next level's lineups (the final's from the champion),
  // the chase final collapsed via its tally. Mirrors EventRounds' bracketViewFor.
  function bracketViewFor(root: RoundDef): Bracket {
    const heatSize = Math.max(2, Math.round(Number(root.params?.heat_size ?? 2)));
    const advance = Math.max(
      1,
      Math.round(Number(root.params?.advance ?? Math.floor(heatSize / 2)))
    );
    const seed = root.seeding;
    let levelOneField: number;
    if (typeof seed === 'object' && 'FromRanking' in seed) {
      levelOneField = seed.FromRanking.top_n;
    } else if (seed === 'FromRoster') {
      levelOneField = classRosterSize(root.classes[0] ?? '');
    } else {
      levelOneField = heatsByRound(root.id).reduce((sum, h) => sum + h.lineup.length, 0);
    }
    return buildBracketView(
      root,
      rounds,
      heats,
      callsign,
      championOf(root),
      levelOneField,
      heatSize,
      advance,
      chaseTallyByRoot[root.id]
    );
  }

  // The bracket container's sub-line: where it was seeded from + the cut + how many levels. Resolves
  // the source round to its friendly label (never the raw id).
  function bracketSubtitle(root: RoundDef): string {
    const seed = root.seeding;
    const parts: string[] = [];
    if (typeof seed === 'object' && 'FromRanking' in seed) {
      const srcId = seed.FromRanking.source_rounds[0];
      const src = rounds.find((r) => r.id === srcId);
      parts.push(src ? `from ${src.label}` : 'seeded from a ranking');
      parts.push(`top ${seed.FromRanking.top_n}`);
    }
    const levels = bracketChainRounds(root, rounds).length;
    parts.push(`${levels} ${levels === 1 ? 'level' : 'levels'}`);
    return parts.join(' · ');
  }

  // --- Round / final-standings ranking ----------------------------------------------------------
  // The ranking shown for a round view (the round's own ranking) or a tournament view (its final
  // level's ranking → "Final standings"). Re-fetched when the displayed round or the live state
  // changes, so a freshly-scored heat re-aggregates.
  const rankingRoundId = $derived.by<RoundId | undefined>(() => {
    const v = currentView;
    if (!v) return undefined;
    if (v.kind === 'round') return v.round.id;
    if (v.kind === 'tournament') {
      const chain = bracketChainRounds(v.root, rounds);
      return chain[chain.length - 1]?.id;
    }
    return undefined;
  });
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
      title={currentView?.kind === 'tournament'
        ? currentView.label
        : currentView?.kind === 'round'
          ? `${currentView.label} standings`
          : 'Standings'}
      subtitle={currentView?.kind === 'tournament'
        ? bracketSubtitle(currentView.root)
        : currentView?.kind === 'class'
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
        <p class="empty" role="status">
          No results yet. They appear here as rounds are scored and tournaments run.
        </p>
      {:else if currentView?.kind === 'tournament'}
        {@const champ = championOf(currentView.root)}
        {#if champ !== undefined}
          <p class="champion" role="status">
            <span class="champion-label">Champion</span>
            <span class="champion-sep" aria-hidden="true">·</span>
            <span class="champion-name">{callsign(champ)}</span>
          </p>
        {/if}
        <div class="tree-scroll">
          <BracketTree bracket={bracketViewFor(currentView.root)} />
        </div>
        <div class="final-standings">
          <h3 class="sub-head">Final standings</h3>
          {#if rankingLoading && rankingRows.length === 0}
            <p class="empty" role="status">Loading standings…</p>
          {:else if rankingRows.length === 0}
            <p class="empty" role="status">
              The final standings appear once the bracket's final is scored.
            </p>
          {:else}
            {@render rankTable(`${currentView.label} final standings`)}
          {/if}
        </div>
      {:else if currentView?.kind === 'round'}
        {#if rankingLoading && rankingRows.length === 0}
          <p class="empty" role="status">Loading standings…</p>
        {:else if rankingRows.length === 0}
          <p class="empty" role="status">
            Nothing scored for <strong>{currentView.label}</strong> yet — standings populate as the round
            is scored.
          </p>
        {:else}
          {@render rankTable(`${currentView.label} standings`)}
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

  {#if heatResult}
    <Card title="Heat result" pad={false}>
      <Leaderboard result={heatResult} {metricLabel} nameFor={callsign} />
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

{#snippet rankTable(caption: string)}
  <table class="standings" aria-label={caption}>
    <thead>
      <tr>
        <th scope="col" class="pos">Pos</th>
        <th scope="col" class="pilot">Pilot</th>
      </tr>
    </thead>
    <tbody>
      {#each rankingRows as row (row.competitor)}
        <tr>
          <td class="pos"><span class="badge">{row.position}</span></td>
          <td class="pilot">{callsign(row.competitor)}</td>
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

  .champion {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    margin: 0 0 var(--gf-space-4);
    padding: var(--gf-space-2) var(--gf-space-3);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-accent-soft, var(--gf-surface-sunken));
    font-size: var(--gf-font-size-md);
  }
  .champion-label {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    font-weight: var(--gf-font-weight-semibold);
  }
  .champion-name {
    color: var(--gf-text);
    font-weight: var(--gf-font-weight-bold);
    letter-spacing: var(--gf-tracking-tight);
  }
  .champion-sep {
    color: var(--gf-text-muted);
  }

  .tree-scroll {
    overflow-x: auto;
  }
  .final-standings {
    margin-top: var(--gf-space-5);
  }
  .sub-head {
    margin: 0 0 var(--gf-space-3);
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    font-weight: var(--gf-font-weight-semibold);
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
