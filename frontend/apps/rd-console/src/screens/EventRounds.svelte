<script lang="ts">
  /**
   * EventRounds — the **Rounds** half of the in-event "Rounds & Heats" stage (race redesign Slice
   * 2b). The Heats half (filling a round's heats with pilots) lands in Slice 3 and shows here only
   * as a labelled placeholder.
   *
   * A **round** ({@link RoundDef}) is an event-level, class-tagged, *dynamic* format-instance: a
   * named, configured run of one `FormatRegistry` format, scoped to the eligible classes it runs
   * for, with a {@link SeedingRule} deciding how its field is drawn. One eligible class is a *class
   * round*; many/all classes is an *open / practice* round. Rounds are added **as-you-go** — this
   * screen authors them dynamically and the add reflects immediately (the session re-homes
   * `currentEvent` after each write).
   *
   * The RD authors: a `label`; the **eligible classes** (a multi-select of the event's selected
   * classes); the **format** (from `GET /formats`, the engine's single source of truth); the **win
   * condition** ({@link WinCondition}); the **seeding** (From roster, or From ranking — a prior
   * round's top-N, the bracket case the engine consumes in a later slice); and a minimal optional
   * **params** key→value editor. Rounds can be edited (PUT) and removed (DELETE).
   *
   * Field-readable: large text, dark surfaces, consistent with the other stage screens.
   */
  import {
    Button,
    Card,
    Collapsible,
    Dialog,
    Field,
    Input,
    Select,
    toast
  } from '@gridfpv/components';
  import type {
    ChannelCatalogEntry,
    ChannelMode,
    Class,
    ClassId,
    CompetitorRef,
    FormatParam,
    FormatSchema,
    GraceWindow,
    HeatPhase,
    HeatSummary,
    NewRoundReq,
    Pilot,
    PilotId,
    ProtestWindow,
    RankEntry,
    RoundDef,
    RoundId,
    RoundIssue,
    SeedingRule,
    StartProcedure,
    Timer,
    WinCondition
  } from '@gridfpv/types';
  import { buildCompetitorNames } from '../lib/competitorName.js';
  import { collapseStore } from '../lib/collapse.svelte.js';
  import {
    fieldsForFormat,
    formatLabel,
    isHeadToHeadFormat,
    isQualifyingFormat,
    isRoundTypeFormat,
    OPEN_PRACTICE,
    ROUND_TYPE_FORMATS
  } from '../lib/formats.js';
  import {
    heatDisplayName as sharedHeatDisplayName,
    isDeterministicRound,
    isOpenPracticeRound
  } from '../lib/heats.js';
  import type { Session } from '../lib/session.svelte.js';

  let { session }: { session: Session } = $props();

  // ── Collapse persistence ────────────────────────────────────────────────────
  // Each round's heats block can be collapsed so the RD can manage a multi-round / many-heat event.
  // The choice persists per stable round id, namespaced by the event (gf.collapse.<event>.<section>),
  // sticking across navigation/reload. A *finished* round (all its heats Final) defaults collapsed;
  // an active/incomplete round defaults open.
  const eventId = $derived(session.currentEvent?.id ?? 'event');
  const collapseStores = new Map<string, ReturnType<typeof collapseStore>>();
  function collapse(sectionId: string, defaultOpen: boolean): ReturnType<typeof collapseStore> {
    const key = `${eventId}:${sectionId}`;
    let s = collapseStores.get(key);
    if (!s) {
      s = collapseStore(eventId, sectionId, defaultOpen);
      collapseStores.set(key, s);
    }
    return s;
  }
  // A round reads "finished" when it has heats and every one is Final — a sensible default-collapsed.
  function roundFinished(roundId: RoundId): boolean {
    const hs = heatsByRound(roundId);
    return hs.length > 0 && hs.every((h) => h.phase === 'Final');
  }

  // The app-level class directory (to resolve class ids → names) and the valid format **schemas**
  // (the engine's `FormatRegistry::standard()` + each format's declared param schema, via
  // `GET /formats`). Both are open reads, loaded once. The schema backs both the format dropdown and
  // the guided params editor (which offers only the chosen format's params, each typed by its kind).
  let classes = $state<Class[]>([]);
  // Loaded-flag so a not-yet-resolved class name renders a neutral placeholder ("—") instead of
  // the raw id while the directory read is in flight or after it failed (the flash-the-raw-id
  // bug — the Results-screen pattern; CLAUDE.md: never print the raw id to the screen).
  let classesLoaded = $state(false);
  let formatSchemas = $state<FormatSchema[]>([]);
  const formats = $derived(formatSchemas.map((s) => s.name));
  // The standard channel catalog (race redesign Slice 4b): resolves a heat's assigned raw-MHz
  // frequency back to a band+channel label. An open read, loaded once; an empty catalog degrades
  // labels to raw "5800 MHz".
  let catalog = $state<ChannelCatalogEntry[]>([]);

  // ── Open-practice format (open-practice Slice 2) ─────────────────────────────────────────────
  // The casual **open-practice** format runs a single open heat over a set of active **channels**
  // (timer node seats) rather than pilots — its field is seeded `AllChannels { channels }` (node
  // indices), with no classes. So when this format is chosen the normal class/seeding inputs are
  // swapped for an active-channels picker driven by the event's **primary timer** (its `node_count`
  // seats, each labelled by its configured `available_channels[i]` channel). The picker reflects an
  // edited round's existing `AllChannels` selection.
  // The effective primary timer (its node_count + available_channels lay out the picker).
  const primaryTimer = $derived<Timer | undefined>(session.primaryTimer);
  // One pickable node seat: its index, the raw MHz it's configured to (if any), and its label.
  interface NodeSeat {
    node: number;
    mhz: number | undefined;
    label: string;
  }
  const timerNodes = $derived<NodeSeat[]>(buildTimerNodes(primaryTimer));
  function buildTimerNodes(timer: Timer | undefined): NodeSeat[] {
    if (!timer) return [];
    const count = Math.max(0, Math.round(timer.node_count ?? 0));
    const seats: NodeSeat[] = [];
    // Labelled through the SHARED builder (#416), never `available_channels[i]`: that pool is empty
    // on every Flexible timer, where empty means "no restriction" rather than "no channels", so
    // indexing it labelled every seat of every RotorHazard timer as channel-less.
    for (let i = 0; i < count; i++) {
      seats.push({ node: i, mhz: names.mhzFor(`node-${i}`), label: names.seatLabel(i) });
    }
    return seats;
  }
  // The chosen active node indices (the AllChannels payload), as a set for toggle ergonomics.
  let selectedNodes = $state<Set<number>>(new Set());
  function toggleNode(node: number) {
    const next = new Set(selectedNodes);
    if (next.has(node)) next.delete(node);
    else next.add(node);
    selectedNodes = next;
  }

  // The event's rounds, read straight off `currentEvent` (the session re-homes it after each write
  // so this stays live). Display order is definition order.
  const rounds = $derived<RoundDef[]>(session.currentEvent?.rounds ?? []);
  // The event's selected classes — the only classes a round may be eligible for.
  const eventClassIds = $derived<ClassId[]>(session.currentEvent?.classes ?? []);
  const eventClasses = $derived<Class[]>(
    eventClassIds.map((id) => classes.find((c) => c.id === id)).filter((c): c is Class => !!c)
  );

  // A class/round id → its friendly name; never the raw id. A neutral placeholder shows while the
  // class directory loads (or after a failed read), and for an unknown id (CLAUDE.md).
  const className = (id: ClassId): string =>
    classesLoaded ? (classes.find((c) => c.id === id)?.name ?? '—') : '—';
  const roundLabel = (id: RoundId): string => rounds.find((r) => r.id === id)?.label ?? '—';

  $effect(() => {
    session
      .listClasses()
      .then((list) => ((classes = list), (classesLoaded = true)))
      .catch((e) => toast.error(e instanceof Error ? e.message : String(e)));
    session
      .listFormatSchemas()
      .then((list) => (formatSchemas = list))
      .catch((e) => toast.error(e instanceof Error ? e.message : String(e)));
    session
      .listPilots()
      .then((list) => (pilots = list))
      .catch((e) => toast.error(e instanceof Error ? e.message : String(e)));
    session
      .listChannels()
      .then((list) => (catalog = list))
      .catch(() => (catalog = []));
  });

  // --- The Heats half of the stage (race redesign Slice 3b) --------------------------------------
  // The round-tagged heats list (one entry per scheduled heat) and the pilot directory used to
  // resolve a heat's `CompetitorRef` lineup to callsigns. The heats list is a read of
  // `GET /events/{id}/heats`; it is re-fetched on enter, after each Fill round / manual build, and
  // whenever the stream advances (so a heat's status follows it through Running → Final, and a heat
  // scheduled by another console — or one that doesn't move the live state — appears too).

  let pilots = $state<Pilot[]>([]);
  let heats = $state<HeatSummary[]>([]);
  let fillingRound = $state<RoundId | undefined>(undefined);

  // A heats-load FAILURE is tracked distinctly from a round that genuinely has no heats yet, so the
  // list can show a "Couldn't load — retry" state instead of the misleading "No heats yet" empty
  // state (P1-5). The toast still surfaces the message.
  let heatsError = $state(false);
  async function refreshHeats() {
    try {
      heats = await session.listHeats();
      heatsError = false;
    } catch (e) {
      heatsError = true;
      toast.error(e instanceof Error ? e.message : String(e));
    }
  }

  // Initial load + re-load on every stream update. Keying off `protocolState` (reassigned on every
  // stream envelope) rather than `liveState` (its content) is what makes a freshly *scheduled* heat
  // appear without a transition: filling a heat does not move `current_heat` (fill-no-steal, #191)
  // and often leaves the whole `LiveRaceState` body unchanged, so the backend force-emits a stream
  // envelope on a schedule and this re-reads the heats list when it lands.
  $effect(() => {
    // Touch the protocol state so this effect re-runs on every stream update (not only on a
    // live-state content change).
    void session.protocolState;
    void refreshHeats();
    void refreshRoundIssues();
  });

  // A pilot id maps straight to a `CompetitorRef` of the same string (round_engine.rs). Resolve
  // through the SHARED builder (friendly-names rule — never re-derive inline, and never re-derive
  // its *inputs* either): `buildCompetitorNames` is the one place that assembles the directory, the
  // channel sources and the seat labels, so this screen and Live control cannot answer differently
  // for the same seat (#416 — `node-6` here against `Node 7` there).
  //
  // `namesFor(h)` scopes it to one heat, so that heat's own frequency assignment wins; the
  // event-level `names` (no heat) is what the round card and the node picker read.
  function namesFor(h: HeatSummary | undefined) {
    return buildCompetitorNames({
      pilots,
      heat: h,
      catalog,
      timer: primaryTimer,
      membership: session.currentEvent?.classes_membership
    });
  }
  const names = $derived(namesFor(undefined));
  const callsign = $derived.by<(ref: CompetitorRef) => string>(() => names.name);

  const heatsByRound = (id: RoundId): HeatSummary[] => heats.filter((h) => h.round === id);

  // Whether a saved round is an **open-practice** round (open-practice refinement): its heat is
  // auto-created on round creation, so the Heats area drops the manual Fill / Standings / Advance
  // controls for it and shows the practice heat as ready to Start. Shared with the Live-control
  // heat picker via `../lib/heats.js`.
  // The heat-name rule (round + position → "Qualifying Heat 2" / "Open Practice Heat") is shared
  // with the Live-control heat picker so both render the same label — see `../lib/heats.js`.
  function heatDisplayName(round: RoundDef, h: HeatSummary): string {
    return sharedHeatDisplayName(round, h, heatsByRound(round.id));
  }

  // ── The stored rounds that cannot record a lap (#416) ────────────────────────────────────────
  // `GET /events/{id}/round-issues`: every stored round seating a `node-{i}` that does not exist on
  // the primary timer, is switched off, or is beyond what the timer reported. #412 refuses such a
  // seat when a round is *written*; this is the same rule applied to what is already stored, because
  // the round on the bench predates that fix — it seats onto node 6 of a four-node timer, so its
  // practice heat can never record a lap, and nothing said so.
  //
  // Re-read with the heats (the same stream tick), so disabling a node or changing the primary timer
  // surfaces here without a reload. A failed read is NOT swallowed: silently rendering a seat that
  // cannot record is exactly what this exists to stop, so the RD is told the check did not run.
  let roundIssues = $state<RoundIssue[]>([]);
  let roundIssuesError = $state(false);
  async function refreshRoundIssues() {
    try {
      roundIssues = await session.listRoundIssues();
      roundIssuesError = false;
    } catch {
      roundIssuesError = true;
    }
  }
  /** The impossible seats in one round, in seeding order. Empty means the round's seats are live. */
  const issuesFor = (id: RoundId): RoundIssue[] => roundIssues.filter((i) => i.round === id);

  function statusLabel(h: HeatSummary): string {
    if (h.phase === 'Final') return 'Final';
    if (h.phase === 'Scheduled') return 'Scheduled';
    // Staged / Armed / Running / Unofficial all read as the heat being live/in-progress.
    return h.phase === 'Unofficial' ? 'Unofficial' : 'Running';
  }
  function statusKind(phase: HeatPhase): 'scheduled' | 'running' | 'scored' {
    if (phase === 'Final') return 'scored';
    if (phase === 'Scheduled') return 'scheduled';
    return 'running';
  }

  // ── Editing a round is refused while one of its heats is in progress (#387) ──────────────────
  // Saving a round now **re-materializes** its still-`Scheduled` heats under the edited config, so
  // the Director refuses the edit outright when any of the round's heats is Staged/Armed/Running/
  // Unofficial, or is the heat loaded on the timer — re-tuning a heat out from under the timer is
  // worse than not editing. The console mirrors that rule so the control is dead **before** the RD
  // types: at a timing table, filling in a form and then being rejected is worse than not being
  // offered it.

  /** The heat phases the Director treats as in progress (`events.rs::round_heat_facts`). */
  const IN_PROGRESS_PHASES: HeatPhase[] = ['Staged', 'Armed', 'Running', 'Unofficial'];

  /**
   * Whether any heat in the **event** has ever left `Scheduled` — the guard that keeps the
   * current-heat half of the rule honest.
   *
   * `LiveRaceState.current_heat` (and `HeatSummary.is_current`, which is derived from it) falls
   * back to the **first scheduled heat** when nothing has ever been staged or explicitly selected,
   * so a fresh event always reports a "current" heat that is not actually loaded on any timer. The
   * server's refusal deliberately does NOT use that fallback (`round_engine::heat_on_timer`) —
   * because treating it as a real load would refuse every round edit in a fresh event, including
   * the open-practice channel edit #387 exists to make work. Requiring evidence that *something*
   * has run is the closest client-side stand-in, and it errs the safe way: at worst the RD is
   * offered an edit the server then refuses with a clear message (today's behaviour), never denied
   * one the server would have allowed.
   */
  const someHeatHasRun = $derived(heats.some((h) => h.phase !== 'Scheduled'));

  /**
   * The heat blocking this round's edit, by **friendly name** (never a raw id — repo display rule),
   * or `undefined` when the round is editable.
   */
  function editBlockedBy(round: RoundDef): string | undefined {
    const rHeats = heatsByRound(round.id);
    const live = session.liveState?.current_heat;
    const blocking = rHeats.find((h) => {
      if (IN_PROGRESS_PHASES.includes(h.phase)) return true;
      // A still-`Scheduled` heat the RD has loaded in Live control is off limits too: its channels
      // may already have been read off to the pilots on the line. `Final` is NOT — a raced round
      // stays editable in the fields the scoring freeze allows.
      if (h.phase !== 'Scheduled') return false;
      return someHeatHasRun && (h.is_current || (live !== undefined && h.heat === live));
    });
    return blocking ? heatDisplayName(round, blocking) : undefined;
  }

  // Fill a round's heats (#216). Deterministic formats (Time Trials, Round Robin, Multi-Main,
  // brackets) **generate all** their heats in one action (`mode: 'All'`); the dynamic Open Practice
  // single-steps (`'Next'`).
  //
  // What happened comes from the ack's `outcome` (#395), not from counting heats before and after.
  // The old count-diff could only ever say "nothing appeared" and had to guess at the cause — which
  // is how a Head-to-Head round refusing a single-pilot field (#394) got reported as "the round is
  // complete" on a round where nothing had raced. The server knows which of the three it is, so it
  // says so, and `detail` is the RD-facing sentence it wrote (already naming the round and heats by
  // their friendly names).
  // Open-ended round: "Heats per pilot" set to 0 (Time Trials / Round Robin). Instead of a fixed
  // set, the round generates the next heat on demand forever — so it single-steps ('Next') like
  // Open Practice rather than generating all at once (which would never terminate).
  function isOpenEndedRound(round: RoundDef): boolean {
    return (round.params?.rounds ?? '') === '0';
  }

  async function fillRound(round: RoundDef) {
    if (fillingRound) return;
    fillingRound = round.id;
    const generateAll = isDeterministicRound(round) && !isOpenEndedRound(round);
    try {
      const ack = await session.fillRound(round.id, generateAll ? 'All' : 'Next');
      if (!ack.ok) return; // The error banner / toast surfaces session.lastCommandError.
      await refreshHeats();
      const fill = ack.outcome && 'FillRound' in ack.outcome ? ack.outcome.FillRound : undefined;
      if (!fill) return; // A server too old to report the outcome: the refreshed list is the tell.
      if (fill.scheduled.length > 0) {
        toast.success(fill.detail);
      } else if (fill.stopped === 'Blocked') {
        // The round can never fill as configured — the RD has to change something, so this is not
        // a passing "nothing happened" note. `detail` says exactly what to change.
        toast.warn(fill.detail);
      } else {
        toast.info(fill.detail);
      }
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      fillingRound = undefined;
    }
  }

  // --- Per-round ranking ("Standings") (race redesign Slice 5/6b) -------------------------------
  // Each round can show a compact ordered ranking (`session.roundRanking`) — the cross-round seeding
  // source a later round draws `FromRanking` from.

  // The expanded-standings round, its loaded ranking, and the in-flight load. The ranking is
  // (re-)fetched by the effect below whenever the expanded round changes OR the stream advances —
  // the old fetch-once-on-expand went stale the moment another heat finalized (a correction, a
  // finalize from Marshaling) while the panel stayed open.
  let standingsRound = $state<RoundId | undefined>(undefined);
  let standingsRows = $state<RankEntry[]>([]);
  let standingsLoading = $state(false);
  let standingsError = $state<string | undefined>(undefined);
  // Latest-wins guard (non-reactive): a slower earlier response must not overwrite a newer one
  // (round flipped, or a fresher stream-tick re-fetch already landed).
  let standingsSeq = 0;

  function toggleStandings(round: RoundDef) {
    if (standingsRound === round.id) {
      standingsRound = undefined;
      return;
    }
    standingsRound = round.id;
    standingsRows = [];
    standingsError = undefined;
  }

  // Fetch while a round's standings are expanded, re-keyed off the stream cursor so a fresh
  // finalize/correction re-aggregates the ranking live. Keeps the last good rows on a re-fetch
  // (the loading state only shows for an EMPTY panel, so the open list never flashes away).
  $effect(() => {
    const rid = standingsRound;
    void session.protocolState;
    const seq = ++standingsSeq;
    if (!rid) return;
    standingsLoading = true;
    session
      .roundRanking(rid)
      .then((rows) => {
        if (seq !== standingsSeq) return;
        standingsRows = rows;
        standingsError = undefined;
      })
      .catch((e) => {
        // An unscored / unscorable round 400s — surface it inline rather than as a row list.
        if (seq !== standingsSeq) return;
        standingsError = e instanceof Error ? e.message : String(e);
      })
      .finally(() => {
        if (seq === standingsSeq) standingsLoading = false;
      });
  });

  // --- Manual heat build (replaces the retired NewHeat free-text form) ---------------------------
  // Pick a round, then select from that round's **eligible class members** (real roster pilots, no
  // typed names) → schedule a heat tagged with the round + its single class. The heat id is
  // **auto-generated** (round-scoped + collision-safe, in the readable `<round>-h-…` generator
  // style) so the RD never hand-types the internal handle; the lineup is the chosen pilots' refs.

  let buildOpen = $state(false);
  let buildRound = $state<RoundId | ''>('');
  // An optional human name for the heat. When set it becomes the heat's display name everywhere
  // (overriding the derived "‹Round› Heat N" / tier convention); empty = auto-name (label None).
  let buildHeatLabel = $state('');
  let buildSelected = $state<Set<PilotId>>(new Set());
  let building = $state(false);

  // The pilot ids eligible for the chosen round: the union of its eligible classes' membership.
  const eligibleMembers = $derived<PilotId[]>(buildEligibleMembers(buildRound));
  function buildEligibleMembers(roundId: RoundId | ''): PilotId[] {
    if (!roundId) return [];
    const round = rounds.find((r) => r.id === roundId);
    if (!round) return [];
    const membership = session.currentEvent?.classes_membership ?? [];
    const out: PilotId[] = [];
    for (const cls of round.classes) {
      const m = membership.find((mm) => mm.class === cls);
      // Membership entries are member slots (`{ pilot, channel? }`) — Slice 7a; field is the pilots.
      for (const s of m?.pilots ?? []) if (!out.includes(s.pilot)) out.push(s.pilot);
    }
    return out;
  }

  // The single class a round tags its heats with (one eligible class), else undefined (open round).
  function roundClass(roundId: RoundId | ''): ClassId | undefined {
    const round = rounds.find((r) => r.id === roundId);
    return round && round.classes.length === 1 ? round.classes[0] : undefined;
  }

  // A heat only needs a round + a non-empty lineup; the id is generated, the name is optional.
  const canBuild = $derived(buildRound !== '' && buildSelected.size > 0);
  // A hand-built heat can hold at most the primary timer's node count — the most pilots it can run at
  // once. No primary timer ⇒ no cap (the RD will set a timer before running it).
  const heatNodeCap = $derived(
    primaryTimer?.node_count && primaryTimer.node_count > 0 ? primaryTimer.node_count : Infinity
  );
  const buildAtNodeCap = $derived(buildSelected.size >= heatNodeCap);

  // Mint a unique, round-scoped heat id in the readable generator style (`<round>-h-<suffix>`). The
  // suffix is a short random base36 token, and we re-roll on the (vanishingly rare) chance it
  // collides with an already-scheduled heat for the round — `scheduleHeat`'s ack also dup-checks the
  // id, so this only avoids a needless round-trip rejection. The RD never types this handle.
  function nextHeatId(roundId: RoundId): string {
    const taken = new Set(heatsByRound(roundId).map((h) => h.heat));
    for (;;) {
      const suffix = Math.random().toString(36).slice(2, 8);
      const id = `${roundId}-h-${suffix}`;
      if (!taken.has(id)) return id;
    }
  }

  function openBuild() {
    buildOpen = true;
    buildRound = rounds[0]?.id ?? '';
    buildHeatLabel = '';
    buildSelected = new Set();
  }
  function cancelBuild() {
    buildOpen = false;
    buildSelected = new Set();
  }
  function toggleMember(pid: PilotId) {
    const next = new Set(buildSelected);
    if (next.has(pid)) next.delete(pid);
    // Don't let the lineup exceed the primary timer's node count — a heat can't run more pilots than
    // there are nodes.
    else if (!buildAtNodeCap) next.add(pid);
    buildSelected = next;
  }

  async function submitBuild() {
    if (building || !canBuild || buildRound === '') return;
    building = true;
    // Lineup in eligible-member order; a pilot id is its own CompetitorRef.
    const lineup: CompetitorRef[] = eligibleMembers.filter((pid) => buildSelected.has(pid));
    // A blank name = no custom label (the heat keeps its derived auto-name).
    const label = buildHeatLabel.trim() || undefined;
    // The internal handle is auto-generated (round-scoped + collision-safe), not RD-entered.
    const heatId = nextHeatId(buildRound);
    try {
      const ack = await session.scheduleHeat(heatId, lineup, {
        round: buildRound,
        class: roundClass(buildRound),
        label
      });
      if (!ack.ok) return; // The toast/banner surfaces session.lastCommandError (e.g. a dup id).
      await refreshHeats();
      toast.success('Heat scheduled.');
      buildOpen = false;
      buildSelected = new Set();
      buildHeatLabel = '';
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      building = false;
    }
  }

  // --- The add/edit form -------------------------------------------------------------------------
  // One form drives both add (no `editing`) and edit (an existing round id). Field order is **Label
  // first, then Format**, then the remaining fields shown **dynamically** per the chosen format
  // (Rounds form redesign item 2): see `lib/formats.ts` `fieldsForFormat`. The win condition and
  // seeding are kept as discriminator + a couple of numeric knobs, assembled into the wire shapes on
  // submit; each format's declared params are surfaced inline as proper labeled fields (item 4).

  // Win-condition kinds the form authors. `BestOfN` is the converged time-trial metric — best of N
  // laps, where N = 1 is just the best single lap (it serialises to BestLap on the wire) and N > 1 is
  // the best N consecutive laps (BestConsecutive). Head-to-Head offers only Timed / FirstToLaps;
  // qualifying offers only Timed / BestOfN; everything else offers all three.
  type WinKind = 'Timed' | 'FirstToLaps' | 'BestOfN';
  type SeedKind = 'FromRoster' | 'FromRanking';

  let editing = $state<RoundId | undefined>(undefined);
  let formOpen = $state(false);
  let saving = $state(false);

  // Bracket formats (single/double elim, Chase the Ace) are built via "Advance to bracket" (the
  // full-chain builder), never added as a standalone round — a bracket level added by hand would be
  // The Add-round picker offers only the three round TYPES (Practice / Time Trial / Head-to-Head,
  // D17). Tournament structures (round-robin, single/double elim, multi-main) are composed from
  // Head-to-Head via the tournament builder, not added directly — so they never appear here. The
  // format of an existing non-round-type round stays selectable ONLY while editing that round (to
  // adjust it), so the dropdown still shows its current format.
  const formatOptions = $derived.by(() => {
    const editingRound = editing !== undefined ? rounds.find((r) => r.id === editing) : undefined;
    const offered = formats.filter((f) => isRoundTypeFormat(f) || editingRound?.format === f);
    // Order the round types Open Practice → Time Trials → Head-to-Head (ROUND_TYPE_FORMATS order);
    // an editing-only structure format (not a round type) sorts after them.
    const rank = (f: string) => {
      const i = ROUND_TYPE_FORMATS.indexOf(f);
      return i < 0 ? ROUND_TYPE_FORMATS.length : i;
    };
    return offered.slice().sort((a, b) => rank(a) - rank(b));
  });

  let label = $state('');
  // The round's single eligible class (Rounds form redesign item 6): a round targets exactly one
  // class, so this is a single-select `<select>` value rather than a multi-select. Stored on the
  // wire as the existing one-element `classes` list. `''` = none chosen yet.
  let selectedClass = $state<ClassId | ''>('');
  let format = $state('');
  let winKind = $state<WinKind>('Timed');
  let winSeconds = $state(120); // Timed window, in seconds (converted to micros on submit).
  let winLaps = $state(3); // FirstToLaps target / BestConsecutive span.
  let seedKind = $state<SeedKind>('FromRoster');
  // The source rounds a `FromRanking` seed aggregates (issue #51): a **multi-select** of prior rounds
  // — the field is seeded from the best-per-pilot ranking across all the selected rounds. Held as a
  // Set of round ids for toggle ergonomics; serialized to the wire `source_rounds: RoundId[]`. A
  // single selection (the common bracket-from-one-qual case) is just a one-element set.
  let seedSources = $state<Set<RoundId>>(new Set());
  let seedTopN = $state(8);
  // A seeding this form can't model (`FromHeatWinners` bracket advancement, `FromRankingRange`,
  // `Combine`), captured verbatim when editing such a round. The server replaces the round
  // WHOLESALE on update, so sending the form's roster/ranking approximation silently rewrote a
  // bracket level's "winners of Semifinal" seeding to FromRoster — a grace-window tweak destroyed
  // the bracket chain. When set, the seeding controls lock and submit round-trips it unchanged.
  let editPreservedSeeding = $state<SeedingRule | undefined>(undefined);
  // The edited round's stored start procedure, verbatim — same wholesale-replace trap as the
  // seeding above: the form models only min/max delay, so rebuilding the procedure from the
  // two inputs silently ERASED a configured start `tone` (and would flatten any future
  // non-randomized mode back to randomized-delay). Submit spreads this under the form's
  // fields, so everything the form doesn't model survives the round trip.
  let editPreservedStart = $state<StartProcedure | undefined>(undefined);
  // The chosen format's params, as a `key → value` map (Rounds form redesign item 4): every param
  // the format declares is shown inline as a proper labeled field, seeded from its schema default
  // (or the edited round's stored value). On a format switch the map is re-seeded to the new
  // format's declared params. The wire shape is this same `key → value` map.
  let paramValues = $state<Record<string, string>>({});
  // The per-position points table for a Head-to-Head **Points** round (1st place first), authored by
  // the points editor and serialized to the `points` param on submit. A steep MultiGP-style default
  // the RD can edit/grow/shrink; positions beyond the list score 0.
  const DEFAULT_POINTS_TABLE = [10, 6, 4, 3, 2, 1];
  let pointsTable = $state<number[]>([...DEFAULT_POINTS_TABLE]);
  // The round's channel mode (Static = fixed channels / channel-balanced heats; Per-heat = assigned
  // per heat, for brackets). Defaulted by format on the backend; the toggle overrides it.
  let channelMode = $state<ChannelMode>('PerHeat');
  // ── Heat-lifecycle config (Slice 3) ─────────────────────────────────────────
  // The staging timer (entered as mm:ss, the field-friendly form), the randomized start-procedure
  // window (min/max as whole/decimal **seconds** — Rounds form redesign item 3), and the completion
  // grace (seconds). All map to the `RoundDef` fields the runtime + console read; sane defaults match
  // the engine (5:00 staging, 2.0–5.0s start, 30s grace — Rounds form redesign item 5).
  let stagingMinutes = $state(5); // staging timer minutes part
  let stagingSeconds = $state(0); // staging timer seconds part
  let startMinSeconds = $state(2); // randomized start hold: shortest, in seconds (→ min_delay_ms)
  let startMaxSeconds = $state(5); // randomized start hold: longest, in seconds (→ max_delay_ms)
  let graceSeconds = $state(30); // grace window after the win condition, in seconds
  // Min lap time (D26): raw crossings that would close a shorter lap are auto-removed (a gate
  // reflection / double-detection), marshal-restorable. 0 = off; NEW rounds seed the
  // field-standard 5s so a double-fire never fabricates a 0.004s best lap out of the box.
  let minLapSeconds = $state(5);
  // ── Protest window (marshaling Slice 5) ──────────────────────────────────────
  // The **auto-official timer**, in seconds. 0 (the default) = OFF: the result stays provisional
  // (Unofficial) until the RD finalizes manually — today's behaviour. A positive value arms the
  // auto-official timer: the runtime auto-finalizes the heat that long after it ends (the RD can
  // still finalize early). Maps to `RoundDef.protest_window` (`Off` / `After { micros }`).
  let protestSeconds = $state(0);
  // ── Open-practice duration (open-practice refinement) ────────────────────────
  // The **time limit** for an open-practice round — the practice duration, entered as a single
  // "Minutes" field. Blank = no limit (the RD ends the practice manually). When set, the runtime
  // auto-ends the practice once the elapsed running time reaches it. Only shown / sent for the
  // open-practice format (it has no win condition; the time limit is its only auto-end). Stored as
  // `time_limit_secs = minutes * 60`. Held as a **string** (the raw <input> value) so a blank field
  // is distinct from 0 — blank ⇒ no limit, where 0/blank both `buildTimeLimitSecs()` → undefined.
  let timeLimitMinutes = $state('');

  // The fields the chosen format shows (Rounds form redesign item 2) — drives which dynamic sections
  // render. Open practice swaps the class/win/seeding/channel-mode block for the active-channels
  // picker + time limit; every other format shows the full block plus its declared params (item 4).
  const fields = $derived(fieldsForFormat(format));
  // Open-practice format (open-practice Slice 2): submittable on a label + at least one active
  // channel (no classes).
  const isOpenPractice = $derived(format === OPEN_PRACTICE);
  // A **qualifying** format (timed_qual / round_robin): the cross-round ranking metric *is* the win
  // condition (the qualifying metric is derived from the win condition, not a separate field —
  // Rounds form redesign). So the win-condition dropdown offers only the qualifying-applicable
  // conditions (Best lap, Best N consecutive, Timed — Most Laps); First-to-N-laps is not a
  // qualifying metric and is hidden for these formats.
  const isQualifying = $derived(isQualifyingFormat(format));
  // A Head-to-Head round, and whether it ranks by a points table (vs placement) — the latter drives
  // the per-position points editor.
  const isHeadToHead = $derived(isHeadToHeadFormat(format));
  const h2hPoints = $derived(isHeadToHead && paramValues['scoring'] === 'points');
  // Group size (pilots per heat) is capped at the primary timer's node count — the most pilots a heat
  // can physically run; default 8 when no primary timer is set yet.
  const maxGroupSize = $derived(Math.max(2, primaryTimer?.node_count || 8));
  const groupSizeOptions = $derived(Array.from({ length: maxGroupSize - 1 }, (_, i) => i + 2));
  // Head-to-Head Points: the points table has exactly one row per finishing position — i.e. group_size
  // rows. Resize it as the group size changes, keeping entered values and padding new rows with 0.
  $effect(() => {
    if (!h2hPoints) return;
    const n = Math.max(
      2,
      Math.min(maxGroupSize, Math.round(Number(paramValues['group_size']) || 2))
    );
    if (pointsTable.length !== n) {
      pointsTable = Array.from({ length: n }, (_, i) => pointsTable[i] ?? 0);
    }
  });
  const canSubmitOpenPractice = $derived(
    isOpenPractice && label.trim().length > 0 && selectedNodes.size > 0
  );

  // The hint under the single-class dropdown (Rounds form redesign item 6): a round targets exactly
  // one class, so this just nudges to pick one.
  const classHint = $derived(
    selectedClass === '' ? 'Pick the class this round runs for.' : 'This round runs for one class.'
  );

  // The other rounds a FromRanking seed may draw from (every round but the one being edited).
  const sourceCandidates = $derived(rounds.filter((r) => r.id !== editing));

  // The most pilots a FromRanking cut can take: the distinct competitors racing in the selected
  // source rounds (their heats' lineups). A source round with no heats yet (build-ahead) falls back
  // to its eligible class members, and an empty union to the event's whole membership; undefined =
  // nothing to bound against (leave the input uncapped rather than guess).
  const seedTopNMax = $derived.by(() => {
    const field = new Set<string>();
    for (const id of seedSources) {
      const hs = heatsByRound(id);
      if (hs.length > 0) for (const h of hs) for (const ref of h.lineup) field.add(ref);
      else for (const p of buildEligibleMembers(id)) field.add(p);
    }
    if (field.size > 0) return field.size;
    const all = new Set<string>();
    for (const m of session.currentEvent?.classes_membership ?? [])
      for (const s of m.pilots) all.add(s.pilot);
    return all.size > 0 ? all.size : undefined;
  });

  // Keep the cut within the real field as sources are toggled (or an edited round's saved top_n
  // exceeds what its sources can rank today).
  $effect(() => {
    if (seedTopNMax !== undefined && seedTopN > seedTopNMax) seedTopN = seedTopNMax;
  });

  // The chosen format's declared params (its schema), in display order — each surfaced inline as a
  // proper labeled field (Rounds form redesign item 4). Only the chosen format's params are shown.
  const formatParams = $derived<FormatParam[]>(
    formatSchemas.find((s) => s.name === format)?.params ?? []
  );

  /** The seed value for a param: its schema default, else a kind-appropriate blank. */
  function defaultValueFor(p: FormatParam): string {
    if (p.default !== undefined && p.default !== null) return p.default;
    if (p.kind === 'bool') return 'false';
    if (p.kind === 'enum') return p.options?.[0] ?? '';
    return '';
  }

  // Re-seed the param values when the format changes: every param the new format declares gets a
  // shown field, keeping the edited round's stored value where the param carries over and seeding
  // from the schema default otherwise. Tracked so it fires on a real format switch, not every
  // keystroke; the initial open/edit seed (which loads the round's params) runs before this settles,
  // then this reconciles to the chosen format's declared set.
  let lastParamFormat = $state('');
  $effect(() => {
    // Only reconcile once the schemas have loaded and the chosen format is among them — otherwise an
    // edit whose params load before the schemas would seed against an empty schema.
    const known = formatSchemas.some((s) => s.name === format);
    if (known && format !== lastParamFormat) {
      lastParamFormat = format;
      const next: Record<string, string> = {};
      for (const p of formatParams) {
        next[p.key] = paramValues[p.key] ?? defaultValueFor(p);
      }
      paramValues = next;
    }
  });

  // Keep the win condition valid for the chosen format. A qualifying format offers only Timed /
  // Best-of-N, so snap off First-to-N to Best-of-N. Head-to-Head offers only Timed / First-to-N
  // (Best-of-N is a time-trial metric, not how you decide a race), so snap off Best-of-N to
  // First-to-N. The win condition then drives the round's ranking / advancement.
  $effect(() => {
    if (isQualifying && winKind === 'FirstToLaps') winKind = 'BestOfN';
  });
  $effect(() => {
    if (isHeadToHead && winKind === 'BestOfN') winKind = 'FirstToLaps';
  });

  function setParamValue(key: string, value: string) {
    paramValues = { ...paramValues, [key]: value };
  }

  function resetForm() {
    editing = undefined;
    label = '';
    selectedClass = '';
    // Default a new round to the first offered racing round type (Time Trials), not whatever the
    // schema list happens to lead with (which may be a filtered-out structure) — so the form always
    // opens on a selectable round type, and not the special open-practice form.
    format =
      ROUND_TYPE_FORMATS.find((f) => f !== OPEN_PRACTICE && formats.includes(f)) ??
      formats[0] ??
      '';
    winKind = 'Timed';
    winSeconds = 120;
    winLaps = 3;
    seedKind = 'FromRoster';
    seedSources = new Set();
    seedTopN = 8;
    editPreservedSeeding = undefined;
    editPreservedStart = undefined;
    selectedNodes = new Set();
    paramValues = {};
    pointsTable = [...DEFAULT_POINTS_TABLE];
    lastParamFormat = ''; // force the format effect to re-seed the new format's params
    channelMode = 'PerHeat';
    // Heat-lifecycle config defaults — match the engine (5:00 staging, 2.0–5.0s start, 30s grace).
    stagingMinutes = 5;
    stagingSeconds = 0;
    startMinSeconds = 2;
    startMaxSeconds = 5;
    graceSeconds = 30;
    minLapSeconds = 5;
    protestSeconds = 0; // off by default — manual finalize only
    timeLimitMinutes = ''; // blank = no limit
  }

  export function openAdd() {
    resetForm();
    formOpen = true;
  }

  function openEdit(round: RoundDef) {
    editing = round.id;
    label = round.label;
    // A round stores its eligible classes as a list; the UI targets one class, so seed the
    // single-select from the first (a legacy multi-class round shows its first class). Open practice
    // carries an empty list, leaving the dropdown unset (it has no class field).
    selectedClass = round.classes[0] ?? '';
    format = round.format;
    // Seed the param values from the round's stored params; the format effect then fills any of the
    // format's declared params this round didn't set from their defaults.
    paramValues = { ...(round.params ?? {}) };
    pointsTable = parsePointsTable(round.params?.points);
    lastParamFormat = ''; // force the format effect to re-seed against this round's format
    channelMode = round.channel_mode ?? 'PerHeat';

    const wc = round.win_condition;
    if (typeof wc === 'string') {
      // 'BestLap' on the wire is Best-of-N with N = 1.
      winKind = 'BestOfN';
      winLaps = 1;
    } else if ('Timed' in wc) {
      winKind = 'Timed';
      winSeconds = Math.round(wc.Timed.window_micros / 1_000_000);
    } else if ('FirstToLaps' in wc) {
      winKind = 'FirstToLaps';
      winLaps = wc.FirstToLaps.n;
    } else if ('BestConsecutive' in wc) {
      winKind = 'BestOfN';
      winLaps = wc.BestConsecutive.n;
    }
    // Best-of-N stores its race time as the round's time limit (see submit) — load it back into the
    // Race time field so editing shows it.
    if (winKind === 'BestOfN' && round.time_limit_secs != null) {
      winSeconds = Math.round(round.time_limit_secs);
    }

    const seed = round.seeding;
    if (typeof seed === 'string') {
      seedKind = 'FromRoster';
    } else if ('FromRanking' in seed) {
      seedKind = 'FromRanking';
      // `source_rounds` is the current multi-select shape; a round stored before issue #51 is read
      // back by the server as a one-element list, so the form always sees an array here.
      seedSources = new Set(seed.FromRanking.source_rounds);
      seedTopN = seed.FromRanking.top_n;
    } else if ('AllChannels' in seed) {
      // AllChannels (open-practice format): reflect the round's active node selection into the
      // channels picker (the format swaps the class/seeding inputs for it below).
      seedKind = 'FromRoster';
      selectedNodes = new Set(seed.AllChannels.channels);
    } else {
      // FromHeatWinners (bracket-level advancement, #217) / FromRankingRange / Combine — seedings
      // this form doesn't model. Preserve the ORIGINAL verbatim and lock the seeding controls:
      // `update_round` replaces the round wholesale, so sending the form's approximation would
      // silently rewrite a bracket level's advancement chain to FromRoster.
      seedKind = 'FromRoster';
      editPreservedSeeding = seed;
    }

    // Heat-lifecycle config (Slice 3): staging timer (split mm:ss), the randomized start window
    // (stored ms → shown as **seconds**, Rounds form redesign item 3), and the grace. Each falls back
    // to the engine default when the round predates these fields.
    const stagingTotal = round.staging_timer_secs ?? 300;
    stagingMinutes = Math.floor(stagingTotal / 60);
    stagingSeconds = stagingTotal % 60;
    editPreservedStart = round.start_procedure ?? undefined;
    startMinSeconds = msToSeconds(round.start_procedure?.min_delay_ms ?? 2000);
    startMaxSeconds = msToSeconds(round.start_procedure?.max_delay_ms ?? 5000);
    const grace = round.grace_window;
    graceSeconds =
      grace && typeof grace !== 'string' ? Math.round(grace.Duration.micros / 1_000_000) : 30;
    minLapSeconds = round.min_lap_secs ?? 0;

    // Protest window (marshaling Slice 5): reflect an `After { micros }` back as seconds; `Off` (or a
    // round that predates the field) reads back as 0 (the timer disabled — manual finalize only).
    const protest = round.protest_window;
    protestSeconds =
      protest && typeof protest !== 'string' ? Math.round(protest.After.micros / 1_000_000) : 0;

    // Open-practice duration (open-practice refinement): reflect an existing time limit into the
    // single Minutes input. Unset (no limit) reads back as blank (`''`).
    timeLimitMinutes =
      round.time_limit_secs != null ? String(Math.round(round.time_limit_secs / 60)) : '';

    formOpen = true;
  }

  function cancel() {
    formOpen = false;
    resetForm();
  }

  /**
   * Whole/decimal seconds from a stored millisecond delay (Rounds form redesign item 3): the
   * start-procedure inputs read/write seconds while the wire stays in ms. Rounded to one decimal so
   * a 2500ms hold reads back as `2.5`, not `2.5000001`.
   */
  function msToSeconds(ms: number): number {
    return Math.round((ms / 1000) * 10) / 10;
  }
  /** Whole milliseconds from a seconds input (the inverse of {@link msToSeconds}). */
  function secondsToMs(seconds: number): number {
    return Math.max(0, Math.round((Number(seconds) || 0) * 1000));
  }

  function buildWinCondition(): WinCondition {
    switch (winKind) {
      case 'Timed':
        // Clamp to a non-zero window: clearing the seconds input leaves `winSeconds`
        // undefined/NaN, which would otherwise serialize a NaN/0-µs window (an instantly-elapsed
        // heat). `(winSeconds || 0)` guards the NaN; `Math.max(1, …)` keeps the window positive.
        return {
          Timed: { window_micros: Math.max(1, Math.round((winSeconds || 0) * 1_000_000)) }
        };
      case 'FirstToLaps':
        return { FirstToLaps: { n: Math.max(1, Math.round(winLaps)) } };
      case 'BestOfN':
      default: {
        // Best of N laps: N = 1 is the best single lap (BestLap on the wire); N > 1 is the best N
        // consecutive laps (BestConsecutive). The engine is unchanged — only the UI converged.
        const n = Math.max(1, Math.round(winLaps));
        return n === 1 ? 'BestLap' : { BestConsecutive: { n } };
      }
    }
  }

  function buildSeeding(): SeedingRule {
    // Editing a round whose seeding this form can't model: round-trip the original verbatim
    // (the seeding controls are locked in that state) so the update never rewrites it.
    if (editPreservedSeeding !== undefined) return editPreservedSeeding;
    if (seedKind === 'FromRanking' && seedSources.size > 0) {
      // Serialize the multi-select in a stable order: the order the source rounds are defined on the
      // event (so the same selection always produces the same `source_rounds`, independent of click
      // order). The server aggregates best-per-pilot across them regardless of order.
      const ordered = rounds.filter((r) => seedSources.has(r.id)).map((r) => r.id);
      const top = Math.min(
        seedTopNMax ?? Number.POSITIVE_INFINITY,
        Math.max(1, Math.round(seedTopN))
      );
      return {
        FromRanking: { source_rounds: ordered, top_n: top }
      };
    }
    return 'FromRoster';
  }

  /** Toggle a source round in/out of the `FromRanking` multi-select (issue #51). */
  function toggleSeedSource(id: RoundId) {
    const next = new Set(seedSources);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    seedSources = next;
  }

  /**
   * The round's params (Rounds form redesign item 4): the chosen format's declared params, each
   * with its (default-or-edited) value. Open practice declares none, so its map is empty. Only keys
   * the current format declares are sent — a stale value left over from a previous format is dropped.
   */
  function buildParams(): { [key: string]: string } {
    const out: { [key: string]: string } = {};
    for (const p of formatParams) {
      const value = paramValues[p.key];
      if (value !== undefined) out[p.key] = value;
    }
    // Head-to-Head Points: serialize the authored points table (1st place first) into the `points`
    // param the engine scores with. Placement scoring ignores it, so only send it for Points.
    if (isHeadToHead && paramValues['scoring'] === 'points') {
      out.points = pointsTable.map((n) => Math.max(0, Math.round(n || 0))).join(', ');
    }
    return out;
  }

  // ── Head-to-Head points editor ───────────────────────────────────────────────
  /** Parse a stored `points` CSV (e.g. "10, 6, 4") into a table; falls back to the default. */
  function parsePointsTable(csv: string | undefined): number[] {
    const parsed = (csv ?? '')
      .split(/[,\s]+/)
      .filter((s) => s.length > 0)
      .map((s) => Math.max(0, Math.round(Number(s) || 0)));
    return parsed.length > 0 ? parsed : [...DEFAULT_POINTS_TABLE];
  }
  /** The ordinal label for a finishing position (1 → "1st", 2 → "2nd", …). */
  function ordinal(n: number): string {
    const mod100 = n % 100;
    if (mod100 >= 11 && mod100 <= 13) return `${n}th`;
    const suffix = ['th', 'st', 'nd', 'rd'][n % 10] ?? 'th';
    return `${n}${suffix}`;
  }
  function setPointsAt(index: number, value: string) {
    const next = [...pointsTable];
    next[index] = Math.max(0, Math.round(Number(value) || 0));
    pointsTable = next;
  }

  // ── Heat-lifecycle config builders (Slice 3) ─────────────────────────────────
  /** The staging timer in whole seconds, from the mm:ss inputs (≥ 0; minutes/seconds clamped). */
  function buildStagingSecs(): number {
    const mins = Math.max(0, Math.round(stagingMinutes || 0));
    const secs = Math.min(59, Math.max(0, Math.round(stagingSeconds || 0)));
    return mins * 60 + secs;
  }

  /**
   * The randomized-delay start procedure. The seconds inputs are converted to the stored `*_delay_ms`
   * (Rounds form redesign item 3 — the inputs are seconds, the wire stays ms). The min is clamped ≥ 0
   * and the max ≥ min (a mis-ordered pair becomes a point delay — the same defensive rule the runtime
   * applies).
   */
  function buildStartProcedure(): StartProcedure {
    // A mode this form can't model (a future fixed-countdown / external trigger): round-trip
    // it VERBATIM — the delay inputs simply don't apply to it.
    if (
      editPreservedStart &&
      (editPreservedStart as { mode: string }).mode !== 'randomized-delay'
    ) {
      return editPreservedStart;
    }
    const min = secondsToMs(startMinSeconds);
    const max = Math.max(min, secondsToMs(startMaxSeconds));
    // Spread the stored procedure UNDER the form's fields: the `tone` (and any additive future
    // field) survives; only what the form actually edits is rewritten.
    return {
      ...editPreservedStart,
      mode: 'randomized-delay',
      min_delay_ms: min,
      max_delay_ms: max
    };
  }

  /** The completion grace window as a bounded `Duration` (seconds → micros). */
  function buildGraceWindow(): GraceWindow {
    return { Duration: { micros: Math.max(0, Math.round(graceSeconds || 0)) * 1_000_000 } };
  }

  /**
   * The protest window (marshaling Slice 5): `Off` when the input is 0/blank (manual finalize only —
   * the default), else `After { micros }` to arm the auto-official timer (seconds → micros).
   */
  function buildProtestWindow(): ProtestWindow {
    const secs = Math.max(0, Math.round(Number(protestSeconds) || 0));
    return secs > 0 ? { After: { micros: secs * 1_000_000 } } : 'Off';
  }

  /**
   * The open-practice **time limit** in whole seconds from the single Minutes input, or `undefined`
   * when blank / zero (no limit — the RD ends the practice manually). Open-practice refinement.
   */
  function buildTimeLimitSecs(): number | undefined {
    const mins = Math.max(0, Math.round(Number(timeLimitMinutes) || 0));
    const total = mins * 60;
    return total > 0 ? total : undefined;
  }

  // Whether the win condition needs a "Race time (seconds)" value (Timed window, and Best-of-N which
  // is always timed): both read `winSeconds` and would build a NaN/0-µs window if it were left blank.
  const needsRaceTime = $derived(winKind === 'Timed' || winKind === 'BestOfN');
  const raceTimeValid = $derived(Number.isFinite(Number(winSeconds)) && Number(winSeconds) >= 1);
  // The "Laps" field backs First-to-N and Best-of-N: clearing it left `winLaps` blank and the
  // builder silently clamped it to 1 (#340) — block submit instead, the race-time pattern (#329).
  const needsLaps = $derived(winKind === 'FirstToLaps' || winKind === 'BestOfN');
  const lapsValid = $derived(Number.isFinite(Number(winLaps)) && Number(winLaps) >= 1);
  // Same for the FromRanking "Take top" cut: a cleared field silently saved `top_n: 1` (#340).
  const seedTopNValid = $derived(Number.isFinite(Number(seedTopN)) && Number(seedTopN) >= 1);

  // The form is submittable once it has a label, a single eligible class, a format, and — when
  // seeding from a ranking — at least one chosen source round (the multi-select, issue #51) plus a
  // valid "Take top" cut. When the win condition is timed (Timed / Best-of-N) a valid race time is
  // also required (else the heat would run forever / build a degenerate window), and a lap-target
  // condition (First-to-N / Best-of-N) requires a valid lap count (else 1 would silently save).
  const canSubmit = $derived(
    isOpenPractice
      ? canSubmitOpenPractice
      : label.trim().length > 0 &&
          selectedClass !== '' &&
          format.length > 0 &&
          (!needsRaceTime || raceTimeValid) &&
          (!needsLaps || lapsValid) &&
          (seedKind === 'FromRoster' ||
            (seedKind === 'FromRanking' && seedSources.size > 0 && seedTopNValid))
  );

  async function submit() {
    if (saving || !canSubmit) return;
    saving = true;
    // A round targets one class, stored on the wire as a one-element `classes` list. Open practice is
    // class-less and seeds from the active channels (node indices) instead.
    const req: NewRoundReq = {
      label: label.trim(),
      classes: isOpenPractice || selectedClass === '' ? [] : [selectedClass],
      format,
      params: buildParams(),
      // Open practice does no scoring (open-practice refinement): send NO win condition — the backend
      // stores its inert default — and the optional time limit instead. A normal round sends its
      // chosen win condition and no time limit.
      win_condition: isOpenPractice ? undefined : buildWinCondition(),
      // Best-lap / best-consecutive qualifying is **always timed** — the win condition only ranks, it
      // doesn't end the heat — so a race time is required or the heat would run forever. Send it as the
      // round's time limit (the engine auto-ends on it, independent of the win condition). Timed ends
      // via its own window; first-to-laps ends on the lap target; open practice uses its minutes field.
      time_limit_secs: isOpenPractice
        ? buildTimeLimitSecs()
        : winKind === 'BestOfN'
          ? Math.max(1, Math.round(winSeconds || 0))
          : undefined,
      seeding: isOpenPractice
        ? { AllChannels: { channels: [...selectedNodes].sort((a, b) => a - b) } }
        : buildSeeding(),
      channel_mode: channelMode,
      staging_timer_secs: buildStagingSecs(),
      start_procedure: buildStartProcedure(),
      min_lap_secs: Math.max(0, Math.round(Number(minLapSeconds) || 0)),
      grace_window: buildGraceWindow(),
      protest_window: buildProtestWindow()
    };
    try {
      const result = editing
        ? await session.updateRound(editing, req)
        : await session.createRound(req);
      if (!result) {
        toast.info('A control token is required to manage rounds.');
        return;
      }
      toast.success(editing ? 'Round updated.' : 'Round added.');
      formOpen = false;
      resetForm();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      saving = false;
    }
  }

  async function remove(round: RoundDef) {
    try {
      const updated = await session.deleteRound(round.id);
      if (!updated) {
        toast.info('A control token is required to manage rounds.');
        return;
      }
      if (editing === round.id) cancel();
      toast.success('Round removed.');
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    }
  }

  // --- Read-only summaries for the list ----------------------------------------------------------
  function winSummary(wc: WinCondition): string {
    // Best-of-N display (converged): 'BestLap' on the wire is best of 1 lap; BestConsecutive{n} is
    // best of N laps.
    if (typeof wc === 'string') return 'Best lap';
    if ('Timed' in wc)
      return `Timed — Most Laps · ${Math.round(wc.Timed.window_micros / 1_000_000)}s`;
    if ('FirstToLaps' in wc) return `First to ${wc.FirstToLaps.n} laps`;
    if ('BestConsecutive' in wc) return `Best of ${wc.BestConsecutive.n} laps`;
    return 'Best lap';
  }

  // The open-practice time-limit summary (open-practice refinement): "1h 30m" / "45m" / "No limit".
  function timeLimitSummary(secs: number | undefined | null): string {
    if (!secs || secs <= 0) return 'No limit';
    const hours = Math.floor(secs / 3600);
    const mins = Math.floor((secs % 3600) / 60);
    const parts: string[] = [];
    if (hours > 0) parts.push(`${hours}h`);
    if (mins > 0) parts.push(`${mins}m`);
    return parts.length > 0 ? parts.join(' ') : 'No limit';
  }

  function seedSummary(seed: SeedingRule): string {
    if (typeof seed === 'string') return 'From roster';
    if ('AllChannels' in seed) {
      // Open practice (open-practice format): seeded from the active channels (node indices).
      return `Open practice · ${seed.AllChannels.channels.length} channel(s)`;
    }
    if ('FromHeatWinners' in seed) {
      // Bracket-level advancement (#217): seeded from the prior level's heat winners.
      return `Winners of ${roundLabel(seed.FromHeatWinners.source_round)}`;
    }
    if ('FromRankingRange' in seed) {
      // Multi-main / consolation slice: seeds skip+1 … skip+take of the aggregated ranking.
      const { source_rounds, skip, take } = seed.FromRankingRange;
      const labels = source_rounds.map(roundLabel);
      const from = labels.length > 0 ? labels.join(', ') : '—';
      return `Seeds ${skip + 1}–${skip + take} from ${from}`;
    }
    if ('Combine' in seed) {
      // Union of sub-sources (the multi-main composition): summarize each sub-rule in order.
      return `Combine: ${seed.Combine.sources.map(seedSummary).join(' + ')}`;
    }
    if (seed && 'FromRanking' in seed) {
      const { source_rounds, top_n } = seed.FromRanking;
      // One source round reads "Top N from <round>"; several read "Top N from <a>, <b>" (issue #51
      // aggregated seeding). An empty list (shouldn't occur — the form requires one) degrades cleanly.
      const labels = source_rounds.map(roundLabel);
      const from = labels.length > 0 ? labels.join(', ') : '—';
      return `Top ${top_n} from ${from}`;
    }
    // Unknown / future seeding shape — never throw (seedSummary runs per round in the list render).
    return '—';
  }
</script>

<section class="event-rounds" aria-label="Rounds and heats">
  <Card
    title="Rounds"
    subtitle="Define this event's rounds — eligible classes, format, win condition, and seeding. Rounds are added as you go."
  >
    {#snippet actions()}
      <Button
        variant="secondary"
        size="sm"
        onclick={openAdd}
        disabled={eventClasses.length === 0 && !primaryTimer}
      >
        + Add round
      </Button>
    {/snippet}

    {#if eventClasses.length === 0 && !primaryTimer}
      <p class="empty" role="status">
        This event selects no classes yet. Pick classes in the <strong>Classes</strong> stage first —
        a round runs for one or more of them (or set a timer to run open practice).
      </p>
    {:else if rounds.length === 0}
      <p class="empty" role="status">No rounds yet. Add the first round to get going.</p>
    {/if}

    {#if roundIssuesError && rounds.length > 0}
      <p class="round-bad-seat" role="alert">
        Couldn’t check these rounds’ node seats against the timer. A round seating a node the timer
        does not have records nothing, and that check has not run — verify the active channels
        before racing.
      </p>
    {/if}

    {#if rounds.length > 0}
      <ol class="round-list">
        {#each rounds as round, i (round.id)}
          <!-- The heat (if any) whose progress makes this round un-editable (#387). -->
          {@const blockedBy = editBlockedBy(round)}
          <!-- The seats in this round that cannot record a lap (#416). -->
          {@const badSeats = issuesFor(round.id)}
          <li class="round-row">
            <span class="round-index" aria-hidden="true">{i + 1}</span>
            <div class="round-main">
              <div class="round-head">
                <span class="round-label">{round.label}</span>
                <span class="round-format">{formatLabel(round.format)}</span>
              </div>
              <div class="round-meta">
                <span class="meta-chip">
                  {round.classes.length === eventClasses.length && eventClasses.length > 1
                    ? 'All classes'
                    : round.classes.map(className).join(', ') || '—'}
                </span>
                <!-- Open practice does no scoring (open-practice refinement): show its time limit (or
                     "No limit") in place of the win condition, which it stores only as an inert default. -->
                {#if isOpenPracticeRound(round)}
                  <span class="meta-chip">{timeLimitSummary(round.time_limit_secs)}</span>
                {:else}
                  <span class="meta-chip">{winSummary(round.win_condition)}</span>
                {/if}
                <span class="meta-chip">{seedSummary(round.seeding)}</span>
              </div>
              <!-- A seat that can never record a lap, on the round that owns it and next to the
                   Edit control that repairs it (#416). The Director writes the sentence — round
                   label, timer name, 1-based node, and what to do — so the console cannot drift
                   from the rule that produced it. -->
              {#each badSeats as issue (issue.node)}
                <p class="round-bad-seat" role="alert">
                  <strong>{issue.node_label} records nothing.</strong>
                  {issue.detail}
                </p>
              {/each}
              <!-- Why Edit is dead, right under the round it belongs to (#387). -->
              {#if blockedBy}
                <p class="round-blocked" role="note">
                  Can’t edit while {blockedBy} is in progress — finalize or reset it first.
                </p>
              {/if}
            </div>
            <div class="round-actions">
              <!-- #387: dead while one of this round's heats is in progress, and it says which
                   heat and what to do about it — the Director refuses this edit anyway. -->
              <Button
                variant="ghost"
                size="sm"
                disabled={blockedBy !== undefined}
                title={blockedBy
                  ? `${blockedBy} is in progress — finalize or reset it before editing this round.`
                  : undefined}
                onclick={() => openEdit(round)}>Edit</Button
              >
              <Button variant="ghost" size="sm" onclick={() => remove(round)}>Remove</Button>
            </div>
          </li>
        {/each}
      </ol>
    {/if}
  </Card>

  <Card
    title="Heats"
    subtitle="Fill each round’s heats from its field, or build one by hand. Run them from Race control."
  >
    {#snippet actions()}
      <Button variant="secondary" size="sm" onclick={openBuild} disabled={rounds.length === 0}>
        + Build heat
      </Button>
    {/snippet}

    {#if rounds.length === 0}
      <p class="empty" role="status">
        Add a round above first — heats are drawn from a round’s field.
      </p>
    {:else}
      <div class="heat-rounds">
        {#snippet roundCard(round: RoundDef)}
          {@const heatCount = heatsByRound(round.id).length}
          {@const rc = collapse(`round:${round.id}`, !roundFinished(round.id))}
          <section class="heat-round" aria-label={`Heats for ${round.label}`}>
            <Collapsible title={round.label} id={`round-${round.id}`} bind:open={rc.open}>
              {#snippet summary()}
                <span class="meta-chip">{formatLabel(round.format)}</span>
                <span class="meta-chip">
                  {round.classes.length === eventClasses.length && eventClasses.length > 1
                    ? 'All classes'
                    : round.classes.map(className).join(', ') || '—'}
                </span>
                <span class="heat-count-chip">
                  {heatCount}
                  {heatCount === 1 ? 'heat' : 'heats'}
                </span>
              {/snippet}
              {#snippet actions()}
                <!-- Open practice (open-practice refinement): its single channel heat is auto-created
                     on round creation — there is nothing to Fill and no scoring to rank. So the Heats
                     controls collapse to just the ready-to-Start heat. -->
                {#if !isOpenPracticeRound(round)}
                  <Button
                    variant="ghost"
                    size="sm"
                    onclick={() => toggleStandings(round)}
                    aria-pressed={standingsRound === round.id}
                  >
                    {standingsRound === round.id ? 'Hide standings' : 'Standings'}
                  </Button>
                  <!-- Format-aware fill (#216): a deterministic round generates all its heats in one
                       action; a dynamic round (Open Practice) single-steps. -->
                  <Button
                    variant="primary"
                    size="sm"
                    onclick={() => fillRound(round)}
                    loading={fillingRound === round.id}
                    disabled={fillingRound !== undefined}
                  >
                    {isOpenEndedRound(round)
                      ? 'Generate next heat'
                      : isDeterministicRound(round)
                        ? 'Generate heats'
                        : 'Add next heat'}
                  </Button>
                {/if}
              {/snippet}

              <div class="heat-round-body">
                {#if standingsRound === round.id}
                  {@const rHeats = heatsByRound(round.id)}
                  {@const finalizedCount = rHeats.filter((h) => h.phase === 'Final').length}
                  {@const allTied =
                    standingsRows.length > 0 &&
                    standingsRows.every((r) => r.position === standingsRows[0].position)}
                  <div class="round-standings" aria-label={`Standings for ${round.label}`}>
                    <h4 class="standings-title">Standings</h4>
                    {#if standingsLoading && standingsRows.length === 0}
                      <p class="empty small" role="status">Loading standings…</p>
                    {:else if standingsError}
                      <p class="empty small" role="status">
                        No ranking yet — score this round's heats first.
                      </p>
                    {:else if standingsRows.length === 0}
                      <p class="empty small" role="status">No ranked competitors yet.</p>
                    {:else if allTied}
                      <!-- The round ranking only counts FINALIZED heats; with none finalized every
                           pilot is tied (position 1), which reads as a broken ranking. Show the
                           finalize-progress instead and let the order fill in as heats finalize. -->
                      <p class="empty small" role="status">
                        Ranking appears as you finalize heats — {finalizedCount} of {rHeats.length}
                        finalized.
                      </p>
                    {:else}
                      {#if finalizedCount > 0 && finalizedCount < rHeats.length}
                        <p class="standings-progress" role="status">
                          {finalizedCount} of {rHeats.length} heats finalized — updates as you finalize
                          more.
                        </p>
                      {/if}
                      <ol class="standings-list">
                        {#each standingsRows as entry (entry.competitor)}
                          <li class="standings-row">
                            <span class="standings-pos">{entry.position}</span>
                            <span class="standings-call">{callsign(entry.competitor)}</span>
                          </li>
                        {/each}
                      </ol>
                    {/if}
                  </div>
                {/if}

                {#if heatsByRound(round.id).length === 0}
                  {#if heatsError}
                    <p class="empty small" role="alert">
                      Couldn't load heats — <button
                        type="button"
                        class="link-btn"
                        onclick={refreshHeats}>try again</button
                      >.
                    </p>
                  {:else if isOpenPracticeRound(round)}
                    <p class="empty small" role="status">
                      The practice heat is being prepared — it is created automatically for an
                      open-practice round.
                    </p>
                  {:else}
                    <p class="empty small" role="status">
                      No heats yet — <strong
                        >{isOpenEndedRound(round)
                          ? 'Generate next heat'
                          : isDeterministicRound(round)
                            ? 'Generate heats'
                            : 'Add next heat'}</strong
                      > to draw from this round’s field.
                    </p>
                  {/if}
                {:else}
                  {#if isOpenPracticeRound(round)}
                    <p class="inline-note small" role="status">
                      This practice heat is ready — open <strong>Race control</strong> to
                      <strong>Stage</strong> then <strong>Start</strong> it.
                    </p>
                  {/if}
                  <ol class="heat-list">
                    {#each heatsByRound(round.id) as h (h.heat)}
                      {@const heatNames = namesFor(h)}
                      <li class="heat-row" class:current={h.is_current}>
                        <div class="heat-main">
                          <div class="heat-head">
                            <span class="heat-id">{heatDisplayName(round, h)}</span>
                            {#if h.is_current}<span class="current-pill">Current</span>{/if}
                            <span class={`status-pill ${statusKind(h.phase)}`}
                              >{statusLabel(h)}</span
                            >
                          </div>
                          <div class="lineup">
                            {#each h.lineup as ref, i (ref)}
                              <span class="lineup-pilot">
                                <span class="lineup-num" aria-hidden="true">{i + 1}</span>
                                <span class="lineup-call">{heatNames.name(ref)}</span>
                                <!-- Unknown is not "none" (#416): a Flexible timer with no channel
                                     pool configured has simply not told GridFPV what its nodes are
                                     on, which is a different statement from "no channel". -->
                                <span class="lineup-chan" class:none={!heatNames.channelFor(ref)}>
                                  {heatNames.channelFor(ref) ?? 'unknown'}
                                </span>
                              </span>
                            {/each}
                            {#if h.lineup.length === 0}<span class="lineup-empty"
                                >— no pilots —</span
                              >{/if}
                          </div>
                        </div>
                      </li>
                    {/each}
                  </ol>
                {/if}
              </div>
            </Collapsible>
          </section>
        {/snippet}

        {#each rounds as round (round.id)}
          {@render roundCard(round)}
        {/each}
      </div>
    {/if}
  </Card>

  <!-- The add / edit round form is a modal Dialog (backdrop, focus trap, Esc-to-close) — opened by
         the "+ Add round" / per-round "Edit" buttons, closed on submit/cancel. -->
  <Dialog bind:open={formOpen} title={editing ? 'Edit round' : 'New round'} onclose={cancel}>
    <form
      class="round-form"
      aria-label={editing ? 'Edit round' : 'Add round'}
      onsubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      <!-- Saving an EDIT now has a visible side effect (#387): the Director re-materializes this
             round's still-scheduled heats under the new config, so lineups and channel assignments
             are rebuilt. Say so up front — an RD who has read channels off to pilots needs to know
             before they save, not after. Raced heats are untouched, and the edit is refused
             outright while a heat is in progress (the Edit button is dead in that case). -->
      {#if editing}
        <p class="form-note" role="note">
          Saving rebuilds this round’s <strong>scheduled</strong> heats — their lineups and channel assignments
          are re-derived from the round’s new settings. Heats that have already raced are left alone.
        </p>
      {/if}

      <!-- Field order (Rounds form redesign item 2): Label first, then Format, then the remaining
             fields shown dynamically per the chosen format (`fields` ← `fieldsForFormat`). -->
      <Field label="Label" required>
        <Input bind:value={label} placeholder="e.g. Time Trials R1" aria-label="Label" />
      </Field>

      <Field label="Format" required>
        <Select bind:value={format} aria-label="Format">
          {#each formatOptions as f (f)}
            <!-- Friendly label shown (Rounds form redesign item 1); the value stays the key. -->
            <option value={f}>{formatLabel(f)}</option>
          {/each}
        </Select>
      </Field>

      <!-- Eligible class — a single-select dropdown (Rounds form redesign item 6): a round targets
             exactly one class. Stored on the wire as a one-element `classes` list. -->
      {#if fields.eligibleClass}
        <Field label="Eligible class" required hint={classHint}>
          <Select bind:value={selectedClass} aria-label="Eligible class">
            <option value="" disabled>Choose a class…</option>
            {#each eventClasses as cls (cls.id)}
              <option value={cls.id}>{cls.name}</option>
            {/each}
          </Select>
        </Field>
      {/if}

      <!-- Open practice does no scoring (open-practice refinement): hide the win-condition input and
             offer the practice **Time limit** instead. A normal round keeps its win condition.
             For a **qualifying** format the win condition IS the qualifying metric, so only the
             qualifying-applicable conditions are offered (First-to-N-laps is hidden) and there is no
             separate "qualifying metric" field — the win condition drives the ranking. -->
      {#if fields.winCondition}
        <div class="form-grid">
          <Field
            label="Win condition"
            hint={isQualifying
              ? 'The qualifying metric — the win condition is how this round’s ranking is decided.'
              : undefined}
          >
            <Select bind:value={winKind} aria-label="Win condition">
              <option value="Timed">Timed — Most Laps</option>
              {#if !isQualifying}
                <option value="FirstToLaps">First to N laps</option>
              {/if}
              {#if !isHeadToHead}
                <option value="BestOfN">Best of N laps</option>
              {/if}
            </Select>
          </Field>

          {#if winKind === 'Timed' || winKind === 'BestOfN'}
            <Field
              label="Race time (seconds)"
              hint={winKind === 'Timed'
                ? undefined
                : 'Always timed — the heat ends after this; your best result during the window counts.'}
            >
              <Input type="number" min="1" bind:value={winSeconds} aria-label="Race time seconds" />
            </Field>
          {/if}
          {#if winKind === 'FirstToLaps' || winKind === 'BestOfN'}
            <Field
              label="Laps"
              hint={winKind === 'BestOfN' ? 'N = 1 is the best single lap.' : undefined}
            >
              <Input type="number" min="1" bind:value={winLaps} aria-label="Laps" />
            </Field>
          {/if}
        </div>
      {/if}

      {#if fields.timeLimit}
        <Field
          label="Time limit (minutes)"
          hint="Optional — blank = no limit (end the practice manually). When set, the practice auto-ends after this many minutes."
        >
          <div class="mmss" role="group" aria-label="Time limit">
            <Input
              type="number"
              min="0"
              bind:value={timeLimitMinutes}
              aria-label="Time limit minutes"
            />
            <span class="mmss-sep" aria-hidden="true">min</span>
          </div>
        </Field>
      {/if}

      {#if fields.activeChannels}
        <!-- Open-practice active-channels picker (open-practice Slice 2): the round runs one open
               heat over the primary timer's active node seats; pick which channels are live. Saved as
               `seeding: AllChannels { channels: [<node indices>] }` with no classes. -->
        <Field
          label="Active channels"
          required
          hint={timerNodes.length === 0
            ? 'Set a primary timer with channels first — open practice runs over its node seats.'
            : `${selectedNodes.size} of ${timerNodes.length} node${
                timerNodes.length === 1 ? '' : 's'
              } active. Each active channel shows a live practice board.`}
        >
          {#if timerNodes.length === 0}
            <p class="inline-note" role="status">
              {#if !primaryTimer}
                No primary timer for this event. Set a timer in the <strong>Timers</strong> stage — open
                practice runs over its channels.
              {:else}
                <strong>{primaryTimer.name}</strong> has no node seats configured. Set its channels
                in the <strong>Timers</strong> stage first.
              {/if}
            </p>
          {:else}
            <div class="channel-picker" role="group" aria-label="Active channels">
              {#each timerNodes as seat (seat.node)}
                <label class="channel-chip" class:unset={seat.mhz === undefined}>
                  <input
                    type="checkbox"
                    checked={selectedNodes.has(seat.node)}
                    onchange={() => toggleNode(seat.node)}
                    aria-label={`Channel ${seat.label}`}
                  />
                  <span class="channel-seat">
                    <span class="channel-node" aria-hidden="true">{seat.node + 1}</span>
                    <span class="channel-name">{seat.label}</span>
                  </span>
                </label>
              {/each}
            </div>
          {/if}
        </Field>
      {/if}

      <!-- Seeding (Rounds form redesign item 2): roster-seeded, or seeded from a prior round's
             ranking. The FromRanking source-rounds multi-select + top-N reveals for the ranking case;
             several source rounds are aggregated best-per-pilot (issue #51). -->
      {#if fields.seeding}
        {#if editPreservedSeeding !== undefined}
          <!-- A seeding this form doesn't model (bracket advancement / ranking range / combine):
               locked — saving keeps it exactly as-is, so an unrelated edit (grace, staging, …)
               can never rewrite the bracket chain. -->
          <Field label="Seeding" hint="Kept as-is when you save.">
            <p class="inline-note">
              This round's seeding (bracket advancement) isn't editable here — it will be preserved
              unchanged.
            </p>
          </Field>
        {:else}
          <Field
            label="Seeding"
            hint={seedKind === 'FromRanking'
              ? 'Draw this round from one or more prior rounds’ rankings.'
              : 'Draw straight from the eligible class’ roster membership.'}
          >
            <Select bind:value={seedKind} aria-label="Seeding">
              <option value="FromRoster">From roster</option>
              <option value="FromRanking">From ranking</option>
            </Select>
          </Field>
        {/if}

        {#if editPreservedSeeding === undefined && seedKind === 'FromRanking'}
          <div class="form-grid">
            <Field
              label="Source rounds"
              required
              hint={sourceCandidates.length === 0
                ? undefined
                : 'Pick one or more rounds to seed from. Several are aggregated by each pilot’s best result.'}
            >
              {#if sourceCandidates.length === 0}
                <p class="inline-note">Add another round first to seed from its ranking.</p>
              {:else}
                <div class="source-picker" role="group" aria-label="Source rounds">
                  {#each sourceCandidates as r (r.id)}
                    <label class="source-chip">
                      <input
                        type="checkbox"
                        checked={seedSources.has(r.id)}
                        onchange={() => toggleSeedSource(r.id)}
                        aria-label={`Seed from ${r.label}`}
                      />
                      <span class="source-name">{r.label}</span>
                    </label>
                  {/each}
                </div>
              {/if}
            </Field>
            <Field
              label="Take top"
              hint={seedTopNMax !== undefined
                ? `How many race in this round — of the ${seedTopNMax} pilots in the source ${seedSources.size > 1 ? 'rounds' : 'round'}.`
                : 'How many pilots from the source ranking race in this round.'}
            >
              <Input
                type="number"
                min="1"
                max={seedTopNMax}
                bind:value={seedTopN}
                aria-label="Top N"
              />
            </Field>
          </div>
        {/if}
      {/if}

      {#if fields.channelMode}
        <Field
          label="Channel mode"
          hint={channelMode === 'Static'
            ? 'Static = each pilot’s fixed channel; heats are channel-balanced (time-trial / qualifying).'
            : 'Per-heat = channels assigned per heat from the timer’s pool.'}
        >
          <Select bind:value={channelMode} aria-label="Channel mode">
            <option value="Static">Static</option>
            <option value="PerHeat">Per-heat</option>
          </Select>
        </Field>
      {/if}

      <!-- Format params (Rounds form redesign item 4): the chosen format's declared params, each a
             proper labeled field seeded from its default. The generic "Format Params" add/remove
             editor is gone — these knobs (rounds, heat_size, metric, bracket_reset, main_size) are
             meaningful per format, so they show inline. Open practice declares none, so this is empty. -->
      {#if fields.params && formatParams.length > 0}
        <fieldset class="config-group">
          <legend class="config-legend">Format options</legend>
          <div class="params">
            {#each formatParams as schema (schema.key)}
              {@const value = paramValues[schema.key] ?? ''}
              <Field
                label={schema.label}
                hint={schema.key === 'rounds'
                  ? '0 = open-ended: generate the next heat on demand until you stop.'
                  : schema.key === 'group_size'
                    ? 'Pilots per heat — capped at the primary timer’s node count.'
                    : schema.key === 'rotations'
                      ? 'Each group races this many heats back to back — scoring accumulates. 1 = everyone races once.'
                      : undefined}
              >
                {#if schema.key === 'group_size'}
                  <Select
                    value={value || '2'}
                    aria-label={`${schema.label} value`}
                    onchange={(e: Event) =>
                      setParamValue(schema.key, (e.currentTarget as HTMLSelectElement).value)}
                  >
                    {#each groupSizeOptions as n (n)}
                      <option value={String(n)}>{n}</option>
                    {/each}
                  </Select>
                {:else if schema.kind === 'bool'}
                  <label class="param-toggle">
                    <input
                      type="checkbox"
                      checked={value === 'true' || value === '1'}
                      aria-label={`${schema.label} value`}
                      onchange={(e) =>
                        setParamValue(
                          schema.key,
                          (e.currentTarget as HTMLInputElement).checked ? 'true' : 'false'
                        )}
                    />
                    <span>{value === 'true' || value === '1' ? 'On' : 'Off'}</span>
                  </label>
                {:else if schema.kind === 'enum'}
                  <Select
                    {value}
                    aria-label={`${schema.label} value`}
                    onchange={(e: Event) =>
                      setParamValue(schema.key, (e.currentTarget as HTMLSelectElement).value)}
                  >
                    {#each schema.options ?? [] as opt (opt)}
                      <option value={opt}>{opt}</option>
                    {/each}
                  </Select>
                {:else}
                  <Input
                    type="number"
                    min={schema.key === 'rounds'
                      ? '0'
                      : schema.key === 'rotations'
                        ? '1'
                        : undefined}
                    {value}
                    aria-label={`${schema.label} value`}
                    oninput={(e: Event) =>
                      setParamValue(schema.key, (e.currentTarget as HTMLInputElement).value)}
                  />
                {/if}
              </Field>
            {/each}
          </div>
        </fieldset>
      {/if}

      <!-- Head-to-Head Points: the per-position points table (item 4 — the editor lives in the
           Head-to-Head inputs, shown only when Scoring is Points). One row per finishing position,
           following the group size; 1st place first. A steep MultiGP-style default the RD can edit
           (0 is fine for the tail). -->
      {#if h2hPoints}
        <fieldset class="config-group">
          <legend class="config-legend">Points per position</legend>
          <p class="inline-note">
            Points awarded by finishing position (one per pilot in the group), summed across the
            round. Edit freely — 0 is fine.
          </p>
          <div class="points-editor" role="group" aria-label="Points per position">
            {#each pointsTable as value, i (i)}
              <Field label={ordinal(i + 1)}>
                <Input
                  type="number"
                  min="0"
                  {value}
                  aria-label={`Points for ${ordinal(i + 1)} place`}
                  oninput={(e: Event) =>
                    setPointsAt(i, (e.currentTarget as HTMLInputElement).value)}
                />
              </Field>
            {/each}
          </div>
        </fieldset>
      {/if}

      <fieldset class="config-group">
        <legend class="config-legend">Start &amp; timing</legend>
        <div class="form-grid">
          <Field label="Staging timer" hint="Informational only — no auto-advance.">
            <div class="mmss" role="group" aria-label="Staging timer">
              <Input
                type="number"
                min="0"
                bind:value={stagingMinutes}
                aria-label="Staging minutes"
              />
              <span class="mmss-sep" aria-hidden="true">:</span>
              <Input
                type="number"
                min="0"
                max="59"
                bind:value={stagingSeconds}
                aria-label="Staging seconds"
              />
            </div>
          </Field>
          <Field label="Grace window (seconds)" hint="Late crossings count after the win.">
            <Input
              type="number"
              min="0"
              bind:value={graceSeconds}
              aria-label="Grace window seconds"
            />
          </Field>
          <Field
            label="Min lap time (seconds)"
            hint="Crossings closing a shorter lap are auto-removed (marshal-restorable). 0 = off."
          >
            <Input type="number" min="0" bind:value={minLapSeconds} aria-label="Min lap seconds" />
          </Field>
          <Field
            label="Protest window (seconds)"
            hint="0 = off (manual finalize). Otherwise the result auto-finalizes after this long."
          >
            <Input
              type="number"
              min="0"
              bind:value={protestSeconds}
              aria-label="Protest window seconds"
            />
          </Field>
        </div>
        <!-- Start procedure delays entered in **seconds** (Rounds form redesign item 3); converted
               to/from the stored `*_delay_ms` on save/load. -->
        <Field
          label="Start procedure"
          hint="Randomized hold before race-go (seconds) — the “arm… and… go”. Max is held ≥ min."
        >
          <div class="form-grid">
            <Field label="Min delay (seconds)">
              <Input
                type="number"
                min="0"
                step="0.1"
                bind:value={startMinSeconds}
                aria-label="Start min delay seconds"
              />
            </Field>
            <Field label="Max delay (seconds)">
              <Input
                type="number"
                min="0"
                step="0.1"
                bind:value={startMaxSeconds}
                aria-label="Start max delay seconds"
              />
            </Field>
          </div>
        </Field>
      </fieldset>
    </form>
    <!-- The actions live in the Dialog footer (outside the <form>), so submit is wired via onclick;
           the form's onsubmit still drives Enter-to-submit. -->
    {#snippet footer()}
      <Button variant="ghost" type="button" onclick={cancel} disabled={saving}>Cancel</Button>
      <Button variant="primary" onclick={submit} loading={saving} disabled={!canSubmit}>
        {editing ? 'Save round' : 'Add round'}
      </Button>
    {/snippet}
  </Dialog>

  <!-- The build-a-heat form is a modal Dialog — opened by the "+ Build heat" button, closed on
         submit/cancel. -->
  <Dialog bind:open={buildOpen} title="Build a heat by hand" onclose={cancelBuild}>
    <form
      class="build-form"
      aria-label="Build heat"
      onsubmit={(e) => {
        e.preventDefault();
        submitBuild();
      }}
    >
      <div class="form-grid">
        <Field label="Round" required>
          <Select bind:value={buildRound} aria-label="Build round">
            <option value="" disabled>Choose a round…</option>
            {#each rounds as r (r.id)}
              <option value={r.id}>{r.label}</option>
            {/each}
          </Select>
        </Field>
        <Field label="Heat name (optional)" hint="Overrides the auto-name. Leave blank to keep it.">
          <Input
            bind:value={buildHeatLabel}
            placeholder="e.g. Featured Heat"
            aria-label="Build heat name"
          />
        </Field>
      </div>

      <Field
        label="Pilots"
        required
        hint={buildRound === ''
          ? 'Pick a round to see its eligible members.'
          : eligibleMembers.length === 0
            ? 'This round’s classes have no members yet — set them in the Roster stage.'
            : 'Select the round’s eligible class members to fly this heat.'}
      >
        <div class="member-picker" role="group" aria-label="Eligible members">
          {#each eligibleMembers as pid (pid)}
            <label class="member-chip">
              <input
                type="checkbox"
                checked={buildSelected.has(pid)}
                disabled={!buildSelected.has(pid) && buildAtNodeCap}
                onchange={() => toggleMember(pid)}
                aria-label={`Select ${callsign(pid)}`}
              />
              <span>{callsign(pid)}</span>
            </label>
          {/each}
        </div>
      </Field>
      {#if buildAtNodeCap && Number.isFinite(heatNodeCap)}
        <p class="node-cap-note" role="status">
          All {heatNodeCap} nodes on the primary timer are taken — a heat can't run more pilots than the
          timer has nodes.
        </p>
      {/if}
    </form>
    {#snippet footer()}
      <Button variant="ghost" type="button" onclick={cancelBuild} disabled={building}>
        Cancel
      </Button>
      <Button variant="primary" onclick={submitBuild} loading={building} disabled={!canBuild}>
        Schedule heat
      </Button>
    {/snippet}
  </Dialog>
</section>

<style>
  .event-rounds {
    /* Fill the workspace width (like the standings screen) — the rounds/heats lists and especially
       the bracket tree want the room; the shell + cards are already responsive. */
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .empty {
    margin: 0;
    font-size: var(--gf-font-size-md);
    color: var(--gf-text-secondary);
    line-height: 1.5;
  }

  .round-list {
    list-style: none;
    margin: var(--gf-space-2) 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .round-row {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
  }
  .round-index {
    flex-shrink: 0;
    width: 1.9rem;
    height: 1.9rem;
    display: grid;
    place-items: center;
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface);
    color: var(--gf-text-muted);
    font-variant-numeric: tabular-nums;
    font-weight: var(--gf-font-weight-semibold);
  }
  .round-main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
  }
  .round-head {
    display: flex;
    align-items: baseline;
    gap: var(--gf-space-2);
  }
  .round-label {
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-semibold);
    color: var(--gf-text);
  }
  .round-format {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-muted);
    font-family: var(--gf-font-mono, monospace);
  }
  .round-meta {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-2);
  }
  .meta-chip {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-secondary);
    padding: 0.1rem var(--gf-space-2);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface);
  }
  .round-actions {
    display: flex;
    gap: var(--gf-space-1);
    flex-shrink: 0;
  }
  /* Why this round's Edit is dead (#387) — under the round's own meta, beside the dead button. */
  /* A seat that can never record a lap (#416) — a real warning, toned like one, on the round it
     belongs to and beside the Edit control that repairs it. */
  .round-bad-seat {
    margin: var(--gf-space-1) 0 0;
    padding: var(--gf-space-2) var(--gf-space-3);
    border-left: 3px solid var(--gf-danger);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-danger-soft);
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text);
  }

  .round-blocked {
    margin: var(--gf-space-1) 0 0;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-secondary);
  }
  /* The edit form's "this rebuilds your scheduled heats" heads-up (#387) — a real warning, sized
     and toned like one, at the top of the form rather than buried next to a field. */
  .form-note {
    margin: 0;
    padding: var(--gf-space-2) var(--gf-space-3);
    border-left: 3px solid var(--gf-warning, var(--gf-accent));
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface);
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-secondary);
  }

  /* The round + build forms now render inside a modal Dialog (the Dialog supplies the title,
     padding, and surface), so they are just a vertical stack of fields. The round form is long, so
     it scrolls within the dialog body on short (sunlit-laptop) screens rather than overflowing. */
  .round-form {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
    max-height: min(70vh, 40rem);
    overflow-y: auto;
  }
  .form-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(11rem, 1fr));
    gap: var(--gf-space-3);
  }
  .config-group {
    margin: 0;
    padding: var(--gf-space-3) var(--gf-space-4) var(--gf-space-4);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .config-legend {
    padding: 0 var(--gf-space-2);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
    color: var(--gf-text-muted);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
  }
  .mmss {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
  }
  .mmss-sep {
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-bold);
    color: var(--gf-text-muted);
  }
  /* Open-practice active-channels picker — a node-seat checkbox grid. */
  .channel-picker {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(11rem, 1fr));
    gap: var(--gf-space-2);
  }
  .channel-chip {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    cursor: pointer;
  }
  .channel-chip input {
    width: 1.05rem;
    height: 1.05rem;
    accent-color: var(--gf-accent);
    cursor: pointer;
  }
  .channel-chip.unset {
    opacity: 0.7;
  }
  .channel-seat {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    min-width: 0;
  }
  .channel-node {
    display: inline-grid;
    place-items: center;
    width: 1.6rem;
    height: 1.6rem;
    flex-shrink: 0;
    border-radius: var(--gf-radius-xs);
    background: var(--gf-surface);
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    font-variant-numeric: tabular-nums;
  }
  .channel-name {
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-medium);
    color: var(--gf-text);
  }
  /* Source-rounds multi-select (issue #51): a wrapped checkbox group, same chip styling as the
     open-practice channel picker so the two pickers read consistently. */
  .source-picker {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-2);
  }
  .source-chip {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    cursor: pointer;
  }
  .source-chip input {
    width: 1.05rem;
    height: 1.05rem;
    accent-color: var(--gf-accent);
    cursor: pointer;
  }
  .source-name {
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-medium);
    color: var(--gf-text);
  }
  .params {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  /* Head-to-Head points editor: a wrapping row of compact per-position number fields, plus the
     add/remove-position actions beneath. */
  .points-editor {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-3);
  }
  .points-editor :global(.gf-field) {
    width: 5.5rem;
  }
  .points-actions {
    display: flex;
    gap: var(--gf-space-2);
    margin-top: var(--gf-space-2);
  }
  .param-toggle {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    font-size: var(--gf-font-size-md);
    color: var(--gf-text);
    cursor: pointer;
  }
  .param-toggle input {
    width: 1.05rem;
    height: 1.05rem;
    accent-color: var(--gf-accent);
    cursor: pointer;
  }
  .inline-note {
    margin: 0;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  /* The hand-built-heat node-cap warning — larger + danger-red so it reads as a hard limit. */
  .node-cap-note {
    margin: var(--gf-space-2) 0 0;
    font-size: var(--gf-font-size-md);
    font-weight: 600;
    color: var(--gf-danger);
  }
  .form-actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--gf-space-2);
    padding-top: var(--gf-space-2);
  }

  /* ── Heats half of the stage ─────────────────────────────────────────────── */
  .heat-rounds {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .heat-round {
    display: flex;
    flex-direction: column;
  }
  .heat-count-chip {
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-medium);
    color: var(--gf-text-muted);
    font-variant-numeric: tabular-nums;
  }
  .heat-round-body {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .round-standings {
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface);
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .standings-title {
    margin: 0;
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
    color: var(--gf-text-muted);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
  }
  .standings-progress {
    margin: var(--gf-space-1) 0 var(--gf-space-2);
    font-size: var(--gf-font-size-2xs);
    color: var(--gf-text-faint);
  }
  .standings-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
  }
  .standings-row {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    font-size: var(--gf-font-size-md);
    color: var(--gf-text);
  }
  .standings-pos {
    display: inline-grid;
    place-items: center;
    min-width: 1.7rem;
    height: 1.7rem;
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    color: var(--gf-text-secondary);
    font-weight: var(--gf-font-weight-bold);
    font-variant-numeric: tabular-nums;
  }
  .standings-call {
    font-weight: var(--gf-font-weight-semibold);
  }
  .empty.small {
    font-size: var(--gf-font-size-sm);
  }
  .empty strong {
    color: var(--gf-text);
  }
  .link-btn {
    background: none;
    border: none;
    padding: 0;
    font: inherit;
    color: var(--gf-accent);
    text-decoration: underline;
    cursor: pointer;
  }
  .heat-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .heat-row {
    display: flex;
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface);
  }
  .heat-row.current {
    border-color: var(--gf-accent);
    box-shadow: 0 0 0 1px var(--gf-accent);
  }
  .heat-main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .heat-head {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
  }
  .heat-id {
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-semibold);
    color: var(--gf-text);
    font-family: var(--gf-font-mono, monospace);
  }
  .current-pill {
    font-size: var(--gf-font-size-xs);
    font-weight: var(--gf-font-weight-bold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    padding: 0.1rem var(--gf-space-2);
    border-radius: var(--gf-radius-pill);
    background: var(--gf-accent);
    color: var(--gf-on-accent, #000);
  }
  .status-pill {
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
    padding: 0.1rem var(--gf-space-2);
    border-radius: var(--gf-radius-pill);
    background: var(--gf-surface-sunken);
    color: var(--gf-text-secondary);
    border: 1px solid var(--gf-border-subtle);
  }
  .status-pill.running {
    color: var(--gf-phase-running, var(--gf-accent));
    border-color: var(--gf-phase-running, var(--gf-accent));
  }
  .status-pill.scored {
    color: var(--gf-phase-scored, var(--gf-text-secondary));
    border-color: var(--gf-phase-scored, var(--gf-border));
  }
  .lineup {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-2);
  }
  .lineup-pilot {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-1);
    font-size: var(--gf-font-size-md);
    color: var(--gf-text);
    padding: 0.1rem var(--gf-space-2);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
  }
  .lineup-num {
    display: inline-grid;
    place-items: center;
    width: 1.4rem;
    height: 1.4rem;
    border-radius: var(--gf-radius-xs);
    background: var(--gf-surface);
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-xs);
    font-variant-numeric: tabular-nums;
  }
  .lineup-call {
    font-weight: var(--gf-font-weight-medium);
  }
  .lineup-chan {
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
    color: var(--gf-accent);
    font-variant-numeric: tabular-nums;
    padding-left: var(--gf-space-1);
    border-left: 1px solid var(--gf-border-subtle);
  }
  .lineup-chan.none {
    color: var(--gf-text-faint);
    font-weight: var(--gf-font-weight-regular);
  }
  .lineup-empty {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .build-form {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .member-picker {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-2);
  }
  .member-chip {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    padding: 0.3rem var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    font-size: var(--gf-font-size-md);
    color: var(--gf-text);
    cursor: pointer;
  }
  .member-chip input {
    width: 1.05rem;
    height: 1.05rem;
    accent-color: var(--gf-accent);
    cursor: pointer;
  }
</style>
