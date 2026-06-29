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
    StandingsTable,
    BracketTree,
    Button,
    Card,
    MultiMainView,
    RoundRobinView,
    Select,
    formatMicros,
    toast
  } from '@gridfpv/components';
  import type { Bracket, MultiMain, RoundRobin } from '@gridfpv/components';
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
    RoundStanding,
    EventOutcome
  } from '@gridfpv/types';
  import { bracketFromOutcome, downloadJson, toExportJson } from '../lib/results.js';
  import {
    bracketChainRounds,
    buildBracketView,
    chaseWinTally,
    DEFAULT_BRACKET_POINTS,
    isBracketLevel,
    isBracketRoot,
    isLevelComplete,
    splitBracketLabel,
    tournamentPlacement
  } from '../lib/brackets.js';
  import type { ChaseFinalTally } from '../lib/brackets.js';
  import { isChaseTheAceFormat, isTimedQualFormat } from '../lib/formats.js';
  import {
    buildRoundRobinView,
    isRoundRobinRound,
    parseRoundRobinPoints
  } from '../lib/roundRobin.js';
  import { buildMultiMainView, isMultiMainRound } from '../lib/multiMain.js';
  import { heatNameById, mainTierName } from '../lib/heats.js';
  import type { Session } from '../lib/session.svelte.js';

  let {
    session,
    heatResult = undefined,
    standings = undefined,
    outcome = undefined
  }: {
    session?: Session;
    heatResult?: HeatResult;
    standings?: RankEntry[];
    outcome?: EventOutcome;
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
    | { value: string; label: string; kind: 'roundrobin'; round: RoundDef }
    | { value: string; label: string; kind: 'multimain'; round: RoundDef }
    | { value: string; label: string; kind: 'round'; round: RoundDef }
    | { value: string; label: string; kind: 'class'; classId: ClassId };

  const tournamentRoots = $derived<RoundDef[]>(rounds.filter(isBracketRoot));
  // Round-robin + multi-main rounds group with the tournaments (their own standings views), so they
  // are excluded from the plain-round ranking views below.
  const roundRobinRounds = $derived<RoundDef[]>(rounds.filter(isRoundRobinRound));
  const multiMainRounds = $derived<RoundDef[]>(rounds.filter(isMultiMainRound));
  const plainRounds = $derived<RoundDef[]>(
    rounds.filter((r) => !isBracketLevel(r) && !isRoundRobinRound(r) && !isMultiMainRound(r))
  );

  const viewOptions = $derived.by<ViewOption[]>(() => {
    const opts: ViewOption[] = [];
    for (const root of tournamentRoots) {
      const name = splitBracketLabel(root.label).name || root.label;
      opts.push({ value: `tournament:${root.id}`, label: name, kind: 'tournament', root });
    }
    for (const round of roundRobinRounds) {
      const name = splitBracketLabel(round.label).name || round.label;
      opts.push({ value: `roundrobin:${round.id}`, label: name, kind: 'roundrobin', round });
    }
    for (const round of multiMainRounds) {
      const name = splitBracketLabel(round.label).name || round.label;
      opts.push({ value: `multimain:${round.id}`, label: name, kind: 'multimain', round });
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

    // Else the latest round-robin round that has run (grouped with the tournaments).
    const runRobins = roundRobinRounds.filter((r) =>
      heatsByRound(r.id).some((h) => h.phase === 'Final')
    );
    if (runRobins.length > 0) return `roundrobin:${runRobins[runRobins.length - 1].id}`;

    // Else the latest multi-main round that has run (grouped with the tournaments).
    const runMains = multiMainRounds.filter((r) =>
      heatsByRound(r.id).some((h) => h.phase === 'Final')
    );
    if (runMains.length > 0) return `multimain:${runMains[runMains.length - 1].id}`;

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

  // Per-heat scored results across EVERY bracket-chain level, fetched once each (guarded) and cached.
  // The read stream only carries `LiveRaceState`; a scored `HeatResult` is a separate heat-scope read.
  // Two readers replay this cache: the chase-final series tally (below) and the full-field points
  // tiebreak (`pointsOfFor`). Refreshed on a live advance / heats change so a freshly-scored heat lands.
  let resultByHeat = $state<Record<string, HeatResult>>({});
  const fetchedResultHeats = new Set<string>();
  $effect(() => {
    if (!session) return;
    void session.liveState;
    void heats;
    const chainRounds = [
      ...tournamentRoots.flatMap((root) => bracketChainRounds(root, rounds)),
      ...roundRobinRounds,
      ...multiMainRounds
    ];
    for (const lr of chainRounds) {
      for (const h of heatsByRound(lr.id)) {
        if (h.phase !== 'Final' || fetchedResultHeats.has(h.heat)) continue;
        fetchedResultHeats.add(h.heat);
        session
          .fetchHeatResult(h.heat)
          .then((res) => {
            if (res) resultByHeat = { ...resultByHeat, [h.heat]: res };
          })
          .catch(() => {});
      }
    }
  });

  // The chase-final series tally: the engine ranking exposes only an overall placement, so the
  // frontend counts the race winners. Replays the shared per-heat result cache into a per-root tally
  // (wins per finalist + the champion).
  let chaseTallyByRoot = $state<Record<RoundId, ChaseFinalTally>>({});
  function chaseRaceIndex(heatId: string): number {
    const m = /^cta-r(\d+)$/.exec(heatId);
    return m ? Number(m[1]) : Number.MAX_SAFE_INTEGER;
  }
  $effect(() => {
    void heats;
    const cache = resultByHeat;
    const next: Record<RoundId, ChaseFinalTally> = {};
    for (const root of tournamentRoots) {
      const chain = bracketChainRounds(root, rounds);
      const final = chain[chain.length - 1];
      if (!final || !isChaseTheAceFormat(final.format)) continue;
      const completed = heatsByRound(final.id)
        .filter((h) => h.phase === 'Final')
        .sort((a, b) => chaseRaceIndex(a.heat) - chaseRaceIndex(b.heat));
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

  // The RoundRobinView view-model for a round-robin round: standings ordered by its ranking
  // (`rankingRows`, fetched for the current view) + the per-rotation grid, both off the round's heats
  // and the shared per-heat result cache. Friendly callsigns + heat names (never raw refs/ids).
  function roundRobinViewFor(round: RoundDef): RoundRobin {
    return buildRoundRobinView(rankingRows, heatsByRound(round.id), {
      pointsTable: parseRoundRobinPoints(round.params?.points),
      label: callsign,
      heatName: (h) => heatNameById(h.heat, heats, rounds),
      resultByHeat: (id) => resultByHeat[id],
      bestLapOf: (ref) => rrBestLap.get(ref)
    });
  }

  // A round-robin view's sub-line: where it was seeded from + the rotations.
  function roundRobinSubtitle(round: RoundDef): string {
    const seed = round.seeding;
    const parts: string[] = [];
    if (typeof seed === 'object' && 'FromRanking' in seed) {
      const srcId = seed.FromRanking.source_rounds[0];
      const src = rounds.find((r) => r.id === srcId);
      parts.push(src ? `from ${src.label}` : 'seeded from a ranking');
      parts.push(`top ${seed.FromRanking.top_n}`);
    } else if (seed === 'FromRoster' && round.classes[0]) {
      parts.push(`from ${className(round.classes[0])}`);
    }
    const rotations = Math.max(0, Math.round(Number(round.params?.rounds ?? 0)));
    if (rotations > 0) parts.push(`${rotations} ${rotations === 1 ? 'rotation' : 'rotations'}`);
    return parts.join(' · ');
  }

  // The MultiMainView view-model for a multi-main round: standings ordered by its ranking
  // (`rankingRows`, fetched for the current view), each tagged with the tier the pilot raced in (from
  // the main-X heat, via mainTierName + the shared per-heat result cache). Friendly callsigns.
  function multiMainViewFor(round: RoundDef): MultiMain {
    return buildMultiMainView(rankingRows, heatsByRound(round.id), {
      label: callsign,
      tierNameOf: mainTierName,
      resultByHeat: (id) => resultByHeat[id]
    });
  }

  // A multi-main view's sub-line: where it was seeded from + the main size + bump-up count.
  function multiMainSubtitle(round: RoundDef): string {
    const seed = round.seeding;
    const parts: string[] = [];
    if (typeof seed === 'object' && 'FromRanking' in seed) {
      const srcId = seed.FromRanking.source_rounds[0];
      const src = rounds.find((r) => r.id === srcId);
      parts.push(src ? `from ${src.label}` : 'seeded from a ranking');
      parts.push(`top ${seed.FromRanking.top_n}`);
    } else if (seed === 'FromRoster' && round.classes[0]) {
      parts.push(`from ${className(round.classes[0])}`);
    }
    const mainSize = Math.max(2, Math.round(Number(round.params?.main_size ?? 4)));
    parts.push(`mains of ${mainSize}`);
    const bumpN = Math.max(0, Math.round(Number(round.params?.bump_n ?? 0)));
    if (bumpN > 0) parts.push(`bump ${bumpN}`);
    return parts.join(' · ');
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

  // --- Tournament full-field seeds + placement --------------------------------------------------
  // The tournament view's "Final standings" rank the WHOLE field by bracket placement (how deep each
  // pilot got), breaking ties within a depth tier by SEED (lower seed = better). The seed source is
  // the bracket's `seeding`: a qualifier-seeded (`FromRanking`) bracket seeds off the source round's
  // ranking position; a roster-seeded (`FromRoster`) one off the pilot's order in the class roster.

  // The source round rankings backing a `FromRanking` bracket's seeds (`source round id → ref →
  // position`). Fetched per qualifier-seeded root and refreshed on a live advance (mirrors the
  // round-ranking effect), so the seed = that ranking's 1-based position.
  let seedRankingByRound = $state<Record<RoundId, Map<CompetitorRef, number>>>({});
  $effect(() => {
    if (!session) return;
    void session.liveState;
    for (const root of tournamentRoots) {
      const seed = root.seeding;
      if (typeof seed !== 'object' || !('FromRanking' in seed)) continue;
      const srcId = seed.FromRanking.source_rounds[0];
      if (!srcId) continue;
      session
        .roundRanking(srcId)
        .then((rows) => {
          seedRankingByRound = {
            ...seedRankingByRound,
            [srcId]: new Map(rows.map((r) => [r.competitor, r.position] as const))
          };
        })
        .catch(() => {});
    }
  });

  // A `seedOf(ref) => number` for a bracket root (lower = better; unknown → +∞, sorts last):
  //  - `FromRanking`: the source round's ranking position (the qualifier seed).
  //  - `FromRoster`: the pilot's 1-based index in the class's roster membership (else the event roster).
  //  - fallback: the final-round ranking order, then chain lineup order.
  function seedOfFor(root: RoundDef): (ref: CompetitorRef) => number {
    const seed = root.seeding;
    if (typeof seed === 'object' && 'FromRanking' in seed) {
      const srcId = seed.FromRanking.source_rounds[0];
      const m = srcId ? seedRankingByRound[srcId] : undefined;
      if (m && m.size > 0) return (ref) => m.get(ref) ?? Number.POSITIVE_INFINITY;
    }
    if (seed === 'FromRoster') {
      const classId = root.classes[0] ?? '';
      const membership = session?.currentEvent?.classes_membership ?? [];
      const pilots = membership.find((mm) => mm.class === classId)?.pilots ?? [];
      if (pilots.length > 0) {
        const order = new Map(pilots.map((p, i) => [p.pilot, i + 1] as const));
        return (ref) => order.get(ref) ?? Number.POSITIVE_INFINITY;
      }
      const roster = session?.currentEvent?.roster ?? [];
      if (roster.length > 0) {
        const order = new Map(roster.map((p, i) => [p, i + 1] as const));
        return (ref) => order.get(ref) ?? Number.POSITIVE_INFINITY;
      }
    }
    // Fallback: the final-round ranking order, then the chain's lineup order.
    const fallback = new Map<CompetitorRef, number>();
    let n = 1;
    for (const r of rankingRows) if (!fallback.has(r.competitor)) fallback.set(r.competitor, n++);
    for (const lr of bracketChainRounds(root, rounds))
      for (const h of heatsByRound(lr.id))
        for (const ref of h.lineup) if (!fallback.has(ref)) fallback.set(ref, n++);
    return (ref) => fallback.get(ref) ?? Number.POSITIVE_INFINITY;
  }

  // A `pointsOf(ref) => number` for a bracket root: sum the default per-position points table over the
  // pilot's finishes across every scored heat in the chain (from the shared per-heat result cache).
  // Seed alone can't order pilots eliminated from the SAME multi-pilot heat, so points are the primary
  // within-tier tiebreak; a pilot with no scored finish (in-progress / partial bracket) totals 0, so
  // seed still orders. Reactive: reads `resultByHeat`, so a freshly-fetched result re-ranks the field.
  function pointsOfFor(root: RoundDef): (ref: CompetitorRef) => number {
    const cache = resultByHeat;
    const totals = new Map<CompetitorRef, number>();
    for (const lr of bracketChainRounds(root, rounds)) {
      for (const h of heatsByRound(lr.id)) {
        const res = cache[h.heat];
        if (!res) continue;
        for (const place of res.places) {
          const ref = place.competitor.competitor;
          const pts = DEFAULT_BRACKET_POINTS[place.position - 1] ?? 0;
          totals.set(ref, (totals.get(ref) ?? 0) + pts);
        }
      }
    }
    return (ref) => totals.get(ref) ?? 0;
  }

  // The selected tournament's FULL-FIELD final standing: every pilot in the bracket, by placement
  // tier then points then seed (champion first). The tier + seed come fetch-free from the chain's
  // heat lineups + the champion; the points tiebreak replays the per-heat result cache.
  const tournamentRows = $derived.by<RankEntry[]>(() => {
    const v = currentView;
    if (v?.kind !== 'tournament') return [];
    const chain = bracketChainRounds(v.root, rounds);
    return tournamentPlacement(
      chain,
      heatsByRound,
      championOf(v.root),
      seedOfFor(v.root),
      pointsOfFor(v.root)
    );
  });

  // --- Round / final-standings ranking ----------------------------------------------------------
  // The ranking shown for a round view (the round's own ranking). Re-fetched when the displayed round
  // or the live state changes, so a freshly-scored heat re-aggregates. (The tournament view's "Final
  // standings" use the full-field `tournamentRows` above, not this round ranking.)
  const rankingRoundId = $derived.by<RoundId | undefined>(() => {
    const v = currentView;
    if (!v) return undefined;
    if (v.kind === 'round') return v.round.id;
    if (v.kind === 'roundrobin') return v.round.id;
    if (v.kind === 'multimain') return v.round.id;
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

  // --- Round-robin best lap ----------------------------------------------------------------------
  // The round-robin standings' Best-lap column sources from the dedicated standings endpoint, which
  // ALWAYS computes best lap regardless of the round's win condition (a race-scored heat's result
  // metric is the win-condition metric, e.g. ReachedAt, not a best lap, so best lap can't be read off
  // the heat-result metric). Fetched for a round-robin view, refreshed on a live advance.
  const roundRobinRoundId = $derived.by<RoundId | undefined>(() =>
    currentView?.kind === 'roundrobin' ? currentView.round.id : undefined
  );
  let rrBestLap = $state<Map<CompetitorRef, number | null>>(new Map());
  $effect(() => {
    if (!session) return;
    const rid = roundRobinRoundId;
    void session.liveState;
    if (!rid) {
      rrBestLap = new Map();
      return;
    }
    session
      .roundStandings(rid)
      .then((rows) => {
        const m = new Map<CompetitorRef, number | null>();
        for (const s of rows) m.set(s.competitor, s.best_lap_micros);
        rrBestLap = m;
      })
      // An unscored round 400s — leave it empty (every pilot shows "—").
      .catch(() => (rrBestLap = new Map()));
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
      title={currentView?.kind === 'tournament' ||
      currentView?.kind === 'roundrobin' ||
      currentView?.kind === 'multimain'
        ? currentView.label
        : currentView?.kind === 'round'
          ? `${currentView.label} standings`
          : 'Standings'}
      subtitle={currentView?.kind === 'tournament'
        ? bracketSubtitle(currentView.root)
        : currentView?.kind === 'roundrobin'
          ? roundRobinSubtitle(currentView.round)
          : currentView?.kind === 'multimain'
            ? multiMainSubtitle(currentView.round)
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
          {#if tournamentRows.length === 0}
            <p class="empty" role="status">
              The final standings appear as the bracket's heats are run.
            </p>
          {:else}
            {@render rankTable(`${currentView.label} final standings`, tournamentRows)}
          {/if}
        </div>
      {:else if currentView?.kind === 'roundrobin'}
        <RoundRobinView view={roundRobinViewFor(currentView.round)} />
      {:else if currentView?.kind === 'multimain'}
        <MultiMainView view={multiMainViewFor(currentView.round)} />
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
