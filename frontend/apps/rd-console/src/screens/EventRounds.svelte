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
  import { Button, Card, Field, Input, Select, toast } from '@gridfpv/components';
  import type {
    ChannelCatalogEntry,
    ChannelMode,
    Class,
    ClassId,
    CompetitorRef,
    FormatParam,
    FormatSchema,
    HeatPhase,
    HeatSummary,
    NewRoundReq,
    Pilot,
    PilotId,
    RankEntry,
    RoundDef,
    RoundId,
    SeedingRule,
    WinCondition
  } from '@gridfpv/types';
  import { channelLabel } from '../lib/channels.js';
  import { advanceRoundLabel, advanceRoundReq, bracketTopNDefault } from '../lib/standings.js';
  import type { Session } from '../lib/session.svelte.js';

  let { session }: { session: Session } = $props();

  // The app-level class directory (to resolve class ids → names) and the valid format **schemas**
  // (the engine's `FormatRegistry::standard()` + each format's declared param schema, via
  // `GET /formats`). Both are open reads, loaded once. The schema backs both the format dropdown and
  // the guided params editor (which offers only the chosen format's params, each typed by its kind).
  let classes = $state<Class[]>([]);
  let formatSchemas = $state<FormatSchema[]>([]);
  const formats = $derived(formatSchemas.map((s) => s.name));
  // The standard channel catalog (race redesign Slice 4b): resolves a heat's assigned raw-MHz
  // frequency back to a band+channel label. An open read, loaded once; an empty catalog degrades
  // labels to raw "5800 MHz".
  let catalog = $state<ChannelCatalogEntry[]>([]);

  // The event's rounds, read straight off `currentEvent` (the session re-homes it after each write
  // so this stays live). Display order is definition order.
  const rounds = $derived<RoundDef[]>(session.currentEvent?.rounds ?? []);
  // The event's selected classes — the only classes a round may be eligible for.
  const eventClassIds = $derived<ClassId[]>(session.currentEvent?.classes ?? []);
  const eventClasses = $derived<Class[]>(
    eventClassIds.map((id) => classes.find((c) => c.id === id)).filter((c): c is Class => !!c)
  );

  const className = (id: ClassId): string => classes.find((c) => c.id === id)?.name ?? id;
  const roundLabel = (id: RoundId): string => rounds.find((r) => r.id === id)?.label ?? id;

  $effect(() => {
    session
      .listClasses()
      .then((list) => (classes = list))
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
  // whenever the live state advances (so a heat's status follows it through Running → Scored).

  let pilots = $state<Pilot[]>([]);
  let heats = $state<HeatSummary[]>([]);
  let fillingRound = $state<RoundId | undefined>(undefined);

  async function refreshHeats() {
    try {
      heats = await session.listHeats();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    }
  }

  // Initial load + re-load whenever the live state changes (a transition advances a heat's phase).
  $effect(() => {
    // Touch the live state so this effect re-runs on every stream update.
    void session.liveState;
    void refreshHeats();
  });

  // A pilot id maps straight to a `CompetitorRef` of the same string (round_engine.rs); resolve a
  // ref to its directory callsign, falling back to the bare ref for an unregistered/free-text one.
  const pilotByRef = $derived(new Map(pilots.map((p) => [p.id, p] as const)));
  const callsign = (ref: CompetitorRef): string => pilotByRef.get(ref)?.callsign ?? ref;

  const heatsByRound = (id: RoundId): HeatSummary[] => heats.filter((h) => h.round === id);

  // A heat's per-pilot channel assignment, resolved to a band+channel label (race redesign Slice
  // 4b). `HeatScheduled.frequencies` pairs each ref with a raw MHz; map ref → label so the lineup
  // can show it. A sim/free-text heat carries no frequencies, so a ref resolves to `undefined` ("—").
  function channelByRef(h: HeatSummary): Map<CompetitorRef, string> {
    const map = new Map<CompetitorRef, string>();
    for (const [ref, mhz] of h.frequencies ?? []) map.set(ref, channelLabel(mhz, catalog));
    return map;
  }

  function statusLabel(h: HeatSummary): string {
    if (h.phase === 'Scored') return 'Scored';
    if (h.phase === 'Scheduled') return 'Scheduled';
    // Staged / Armed / Running / Finished all read as the heat being live/in-progress.
    return h.phase === 'Finished' ? 'Finished' : 'Running';
  }
  function statusKind(phase: HeatPhase): 'scheduled' | 'running' | 'scored' {
    if (phase === 'Scored') return 'scored';
    if (phase === 'Scheduled') return 'scheduled';
    return 'running';
  }

  // Fill a round's next heat. The engine acks ok whether it appended a heat OR reported the round
  // complete / its outstanding heat unscored, so compare the round's heat count before and after to
  // tell the RD which happened.
  async function fillRound(round: RoundDef) {
    if (fillingRound) return;
    fillingRound = round.id;
    const before = heatsByRound(round.id).length;
    try {
      const ack = await session.fillRound(round.id);
      if (!ack.ok) return; // The error banner / toast surfaces session.lastCommandError.
      await refreshHeats();
      const after = heatsByRound(round.id).length;
      if (after > before) toast.success(`Heat added to ${round.label}.`);
      else toast.info(`${round.label}: no new heat — the round is complete or awaiting a score.`);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      fillingRound = undefined;
    }
  }

  // --- Per-round ranking ("Standings") + advance-to-bracket (race redesign Slice 5/6b) ----------
  // Each round can show a compact ordered ranking (`session.roundRanking`) — the seeding source a
  // bracket draws `FromRanking` from — and offers "Advance to bracket": create a new single_elim
  // round seeded from this round's ranking, top-N defaulting to the largest power-of-two ≤ the
  // round's field, then Fill it to generate the seeded bracket heats (which the heats list shows).

  // The expanded-standings round, its loaded ranking, and the in-flight load. Toggling reloads, so a
  // freshly-scored heat re-aggregates the ranking.
  let standingsRound = $state<RoundId | undefined>(undefined);
  let standingsRows = $state<RankEntry[]>([]);
  let standingsLoading = $state(false);
  let standingsError = $state<string | undefined>(undefined);

  async function toggleStandings(round: RoundDef) {
    if (standingsRound === round.id) {
      standingsRound = undefined;
      return;
    }
    standingsRound = round.id;
    standingsRows = [];
    standingsError = undefined;
    standingsLoading = true;
    try {
      standingsRows = await session.roundRanking(round.id);
    } catch (e) {
      // An unscored / unscorable round 400s — surface it inline rather than as a row list.
      standingsError = e instanceof Error ? e.message : String(e);
    } finally {
      standingsLoading = false;
    }
  }

  // The round's field size — the union of its eligible classes' membership (what the bracket cut is
  // taken from). Drives the top_n default when advancing.
  function roundFieldSize(round: RoundDef): number {
    return buildEligibleMembers(round.id).length;
  }

  // The advance-to-bracket confirm: which round, its proposed label + top_n (editable), in-flight.
  let advanceRoundId = $state<RoundId | undefined>(undefined);
  let advanceLabel = $state('');
  let advanceTopN = $state(8);
  let advancing = $state(false);

  function openAdvance(round: RoundDef) {
    advanceRoundId = round.id;
    advanceLabel = advanceRoundLabel(round);
    advanceTopN = bracketTopNDefault(roundFieldSize(round));
  }
  function cancelAdvance() {
    advanceRoundId = undefined;
    advancing = false;
  }

  // Create the seeded single_elim round, then immediately Fill its first bracket heat so the heats
  // list shows the ranking-seeded matchups. The bracket is editable thereafter (manual build).
  async function submitAdvance(source: RoundDef) {
    if (advancing) return;
    advancing = true;
    try {
      const req: NewRoundReq = advanceRoundReq(
        source,
        advanceTopN,
        advanceLabel.trim() || advanceRoundLabel(source)
      );
      const created = await session.createRound(req);
      if (!created) {
        toast.info('A control token is required to manage rounds.');
        return;
      }
      // Generate the seeded bracket heats from the ranking.
      const ack = await session.fillRound(created.id);
      if (ack.ok) await refreshHeats();
      toast.success(`Bracket “${created.label}” created, seeded from ${source.label}.`);
      advanceRoundId = undefined;
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      advancing = false;
    }
  }

  // --- Manual heat build (replaces the retired NewHeat free-text form) ---------------------------
  // Pick a round, then select from that round's **eligible class members** (real roster pilots, no
  // typed names) → schedule a heat tagged with the round + its single class. The heat id is
  // RD-entered (so a duplicate is caught by the ack); the lineup is the chosen pilots' refs.

  let buildOpen = $state(false);
  let buildRound = $state<RoundId | ''>('');
  let buildHeatId = $state('');
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

  const canBuild = $derived(
    buildRound !== '' && buildHeatId.trim().length > 0 && buildSelected.size > 0
  );

  function openBuild() {
    buildOpen = true;
    buildRound = rounds[0]?.id ?? '';
    buildHeatId = '';
    buildSelected = new Set();
  }
  function cancelBuild() {
    buildOpen = false;
    buildSelected = new Set();
  }
  function toggleMember(pid: PilotId) {
    const next = new Set(buildSelected);
    if (next.has(pid)) next.delete(pid);
    else next.add(pid);
    buildSelected = next;
  }

  async function submitBuild() {
    if (building || !canBuild || buildRound === '') return;
    building = true;
    // Lineup in eligible-member order; a pilot id is its own CompetitorRef.
    const lineup: CompetitorRef[] = eligibleMembers.filter((pid) => buildSelected.has(pid));
    try {
      const ack = await session.scheduleHeat(buildHeatId.trim(), lineup, {
        round: buildRound,
        class: roundClass(buildRound)
      });
      if (!ack.ok) return; // The toast/banner surfaces session.lastCommandError (e.g. a dup id).
      await refreshHeats();
      toast.success('Heat scheduled.');
      buildOpen = false;
      buildSelected = new Set();
      buildHeatId = '';
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      building = false;
    }
  }

  // --- The add/edit form -------------------------------------------------------------------------
  // One form drives both add (no `editing`) and edit (an existing round id). The win condition and
  // seeding are kept as discriminator + a couple of numeric knobs, assembled into the wire shapes
  // on submit; params is a free key→value list.

  type WinKind = 'Timed' | 'FirstToLaps' | 'BestLap' | 'BestConsecutive';
  type SeedKind = 'FromRoster' | 'FromRanking';
  // A guided param the RD has added — the schema `key` it targets + the chosen raw `value` (the
  // wire shape). The matching `FormatParam` schema (label/kind/options) is looked up by `key`.
  interface ParamRow {
    key: string;
    value: string;
  }

  let editing = $state<RoundId | undefined>(undefined);
  let formOpen = $state(false);
  let saving = $state(false);

  let label = $state('');
  let selectedClasses = $state<Set<ClassId>>(new Set());
  let format = $state('');
  let winKind = $state<WinKind>('Timed');
  let winSeconds = $state(120); // Timed window, in seconds (converted to micros on submit).
  let winLaps = $state(3); // FirstToLaps target / BestConsecutive span.
  let seedKind = $state<SeedKind>('FromRoster');
  let seedSource = $state<RoundId | ''>('');
  let seedTopN = $state(8);
  // The guided params the RD has added (a subset of the chosen format's schema), each with its
  // typed value. `addParam` (a dropdown of the format's not-yet-added params) appends one seeded
  // from its default; `removeParam` unsets it. Stored as `key → value` strings, the wire shape.
  let params = $state<ParamRow[]>([]);
  // The round's channel mode (Static = fixed channels / channel-balanced heats; Per-heat = assigned
  // per heat, for brackets). Defaulted by format on the backend; the toggle overrides it.
  let channelMode = $state<ChannelMode>('PerHeat');
  // Which param to add next (the `<select>`'s bound value), reset after each add.
  let addParamKey = $state('');

  // Whether the eligible-classes pick reads as open/practice (all selected) or a class round (one).
  const classHint = $derived(
    selectedClasses.size === 0
      ? 'Pick at least one eligible class.'
      : selectedClasses.size === eventClasses.length && eventClasses.length > 1
        ? 'All classes — an open / practice round.'
        : selectedClasses.size === 1
          ? 'One class — a class round.'
          : `${selectedClasses.size} classes eligible.`
  );

  // The other rounds a FromRanking seed may draw from (every round but the one being edited).
  const sourceCandidates = $derived(rounds.filter((r) => r.id !== editing));

  // The chosen format's declared params (its schema), keyed for lookup, and the ones not yet added
  // (what the "+ Add param" dropdown offers). Only the chosen format's params are ever offered.
  const formatParams = $derived<FormatParam[]>(
    formatSchemas.find((s) => s.name === format)?.params ?? []
  );
  const paramByKey = $derived(new Map(formatParams.map((p) => [p.key, p] as const)));
  const addableParams = $derived(formatParams.filter((p) => !params.some((r) => r.key === p.key)));

  /** The seed value for a freshly-added param: its schema default, else a kind-appropriate blank. */
  function defaultValueFor(p: FormatParam): string {
    if (p.default !== undefined && p.default !== null) return p.default;
    if (p.kind === 'bool') return 'false';
    if (p.kind === 'enum') return p.options?.[0] ?? '';
    return '';
  }

  // When the format changes, drop any added param that the new format doesn't declare (only the
  // chosen format's params are offered). Tracked so it fires on a real format switch, not on every
  // keystroke; the initial open/edit seed runs before this settles, then this reconciles to it.
  let lastParamFormat = $state('');
  $effect(() => {
    // Only reconcile once the schemas have loaded and the chosen format is among them — otherwise an
    // edit whose params load before the schemas would prune valid params against an empty schema.
    const known = formatSchemas.some((s) => s.name === format);
    if (known && format !== lastParamFormat) {
      lastParamFormat = format;
      const valid = new Set(formatParams.map((p) => p.key));
      const pruned = params.filter((r) => valid.has(r.key));
      if (pruned.length !== params.length) params = pruned;
      addParamKey = '';
    }
  });

  function addParam() {
    const key = addParamKey;
    if (!key) return;
    const schema = paramByKey.get(key);
    if (!schema || params.some((r) => r.key === key)) return;
    params = [...params, { key, value: defaultValueFor(schema) }];
    addParamKey = '';
  }
  function removeParam(key: string) {
    params = params.filter((r) => r.key !== key);
  }
  function setParamValue(key: string, value: string) {
    params = params.map((r) => (r.key === key ? { ...r, value } : r));
  }

  function resetForm() {
    editing = undefined;
    label = '';
    selectedClasses = new Set();
    format = formats[0] ?? '';
    winKind = 'Timed';
    winSeconds = 120;
    winLaps = 3;
    seedKind = 'FromRoster';
    seedSource = '';
    seedTopN = 8;
    params = [];
    channelMode = 'PerHeat';
    addParamKey = '';
  }

  export function openAdd() {
    resetForm();
    formOpen = true;
  }

  function openEdit(round: RoundDef) {
    editing = round.id;
    label = round.label;
    selectedClasses = new Set(round.classes);
    format = round.format;
    params = Object.entries(round.params ?? {}).map(([key, value]) => ({ key, value }));
    channelMode = round.channel_mode ?? 'PerHeat';
    addParamKey = '';

    const wc = round.win_condition;
    if (typeof wc === 'string') {
      winKind = 'BestLap';
    } else if ('Timed' in wc) {
      winKind = 'Timed';
      winSeconds = Math.round(wc.Timed.window_micros / 1_000_000);
    } else if ('FirstToLaps' in wc) {
      winKind = 'FirstToLaps';
      winLaps = wc.FirstToLaps.n;
    } else if ('BestConsecutive' in wc) {
      winKind = 'BestConsecutive';
      winLaps = wc.BestConsecutive.n;
    }

    const seed = round.seeding;
    if (typeof seed === 'string') {
      seedKind = 'FromRoster';
    } else {
      seedKind = 'FromRanking';
      seedSource = seed.FromRanking.source_round;
      seedTopN = seed.FromRanking.top_n;
    }
    formOpen = true;
  }

  function cancel() {
    formOpen = false;
    resetForm();
  }

  function toggleClass(id: ClassId) {
    const next = new Set(selectedClasses);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    selectedClasses = next;
  }

  function buildWinCondition(): WinCondition {
    switch (winKind) {
      case 'Timed':
        return { Timed: { window_micros: Math.max(0, Math.round(winSeconds * 1_000_000)) } };
      case 'FirstToLaps':
        return { FirstToLaps: { n: Math.max(1, Math.round(winLaps)) } };
      case 'BestConsecutive':
        return { BestConsecutive: { n: Math.max(1, Math.round(winLaps)) } };
      case 'BestLap':
      default:
        return 'BestLap';
    }
  }

  function buildSeeding(): SeedingRule {
    if (seedKind === 'FromRanking' && seedSource) {
      return {
        FromRanking: { source_round: seedSource, top_n: Math.max(1, Math.round(seedTopN)) }
      };
    }
    return 'FromRoster';
  }

  function buildParams(): { [key: string]: string } {
    const out: { [key: string]: string } = {};
    for (const { key, value } of params) {
      const k = key.trim();
      if (k) out[k] = value;
    }
    return out;
  }

  // The form is submittable once it has a label, at least one eligible class, a format, and — when
  // seeding from a ranking — a chosen source round.
  const canSubmit = $derived(
    label.trim().length > 0 &&
      selectedClasses.size > 0 &&
      format.length > 0 &&
      (seedKind === 'FromRoster' || (seedKind === 'FromRanking' && !!seedSource))
  );

  async function submit() {
    if (saving || !canSubmit) return;
    saving = true;
    // Eligible classes in the event's selection order (a stable, sensible order).
    const req: NewRoundReq = {
      label: label.trim(),
      classes: eventClassIds.filter((id) => selectedClasses.has(id)),
      format,
      params: buildParams(),
      win_condition: buildWinCondition(),
      seeding: buildSeeding(),
      channel_mode: channelMode
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
    if (typeof wc === 'string') return 'Best lap';
    if ('Timed' in wc) return `Timed · ${Math.round(wc.Timed.window_micros / 1_000_000)}s`;
    if ('FirstToLaps' in wc) return `First to ${wc.FirstToLaps.n} laps`;
    if ('BestConsecutive' in wc) return `Best ${wc.BestConsecutive.n} consecutive`;
    return 'Best lap';
  }

  function seedSummary(seed: SeedingRule): string {
    if (typeof seed === 'string') return 'From roster';
    const { source_round, top_n } = seed.FromRanking;
    return `Top ${top_n} from ${roundLabel(source_round)}`;
  }
</script>

<section class="event-rounds" aria-label="Rounds and heats">
  <Card
    title="Rounds"
    subtitle="Define this event's rounds — eligible classes, format, win condition, and seeding. Rounds are added as you go."
  >
    {#snippet actions()}
      <Button variant="secondary" size="sm" onclick={openAdd} disabled={eventClasses.length === 0}>
        + Add round
      </Button>
    {/snippet}

    {#if eventClasses.length === 0}
      <p class="empty" role="status">
        This event selects no classes yet. Pick classes in the <strong>Classes</strong> stage first —
        a round runs for one or more of them.
      </p>
    {:else if rounds.length === 0}
      <p class="empty" role="status">No rounds yet. Add the first round to get going.</p>
    {/if}

    {#if rounds.length > 0}
      <ol class="round-list">
        {#each rounds as round, i (round.id)}
          <li class="round-row">
            <span class="round-index" aria-hidden="true">{i + 1}</span>
            <div class="round-main">
              <div class="round-head">
                <span class="round-label">{round.label}</span>
                <span class="round-format">{round.format}</span>
              </div>
              <div class="round-meta">
                <span class="meta-chip">
                  {round.classes.length === eventClasses.length && eventClasses.length > 1
                    ? 'All classes'
                    : round.classes.map(className).join(', ') || '—'}
                </span>
                <span class="meta-chip">{winSummary(round.win_condition)}</span>
                <span class="meta-chip">{seedSummary(round.seeding)}</span>
              </div>
            </div>
            <div class="round-actions">
              <Button variant="ghost" size="sm" onclick={() => openEdit(round)}>Edit</Button>
              <Button variant="ghost" size="sm" onclick={() => remove(round)}>Remove</Button>
            </div>
          </li>
        {/each}
      </ol>
    {/if}

    {#if formOpen}
      <form
        class="round-form"
        aria-label={editing ? 'Edit round' : 'Add round'}
        onsubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <h3 class="form-title">{editing ? 'Edit round' : 'New round'}</h3>

        <Field label="Label" required>
          <Input bind:value={label} placeholder="e.g. Qualifying R1" aria-label="Label" />
        </Field>

        <Field label="Eligible classes" required hint={classHint}>
          <div class="class-picker" role="group" aria-label="Eligible classes">
            {#each eventClasses as cls (cls.id)}
              <label class="class-chip">
                <input
                  type="checkbox"
                  checked={selectedClasses.has(cls.id)}
                  onchange={() => toggleClass(cls.id)}
                  aria-label={`Eligible ${cls.name}`}
                />
                <span>{cls.name}</span>
              </label>
            {/each}
          </div>
        </Field>

        <div class="form-grid">
          <Field label="Format" required>
            <Select bind:value={format} aria-label="Format">
              {#each formats as f (f)}
                <option value={f}>{f}</option>
              {/each}
            </Select>
          </Field>

          <Field label="Win condition">
            <Select bind:value={winKind} aria-label="Win condition">
              <option value="Timed">Timed window</option>
              <option value="FirstToLaps">First to N laps</option>
              <option value="BestLap">Best lap</option>
              <option value="BestConsecutive">Best N consecutive</option>
            </Select>
          </Field>

          {#if winKind === 'Timed'}
            <Field label="Window (seconds)">
              <Input type="number" min="1" bind:value={winSeconds} aria-label="Window seconds" />
            </Field>
          {:else if winKind === 'FirstToLaps' || winKind === 'BestConsecutive'}
            <Field label="Laps">
              <Input type="number" min="1" bind:value={winLaps} aria-label="Laps" />
            </Field>
          {/if}
        </div>

        <Field
          label="Channel mode"
          hint={channelMode === 'Static'
            ? 'Static = each pilot’s fixed channel; heats are channel-balanced (time-trial / qualifying).'
            : 'Per-heat = channels assigned per heat from the timer’s pool (for brackets).'}
        >
          <Select bind:value={channelMode} aria-label="Channel mode">
            <option value="Static">Static</option>
            <option value="PerHeat">Per-heat</option>
          </Select>
        </Field>

        <Field
          label="Seeding"
          hint={seedKind === 'FromRanking'
            ? 'Draw this round from a prior round’s ranking (the bracket / cut case).'
            : 'Draw straight from the eligible classes’ roster membership.'}
        >
          <Select bind:value={seedKind} aria-label="Seeding">
            <option value="FromRoster">From roster</option>
            <option value="FromRanking">From ranking</option>
          </Select>
        </Field>

        {#if seedKind === 'FromRanking'}
          <div class="form-grid">
            <Field label="Source round" required>
              {#if sourceCandidates.length === 0}
                <p class="inline-note">Add another round first to seed from its ranking.</p>
              {:else}
                <Select bind:value={seedSource} aria-label="Source round">
                  <option value="" disabled>Choose a round…</option>
                  {#each sourceCandidates as r (r.id)}
                    <option value={r.id}>{r.label}</option>
                  {/each}
                </Select>
              {/if}
            </Field>
            <Field label="Top N advance">
              <Input type="number" min="1" bind:value={seedTopN} aria-label="Top N" />
            </Field>
          </div>
        {/if}

        <Field
          label="Format params"
          hint={formatParams.length === 0
            ? 'This format has no configurable params.'
            : 'Add only the knobs this format declares; each value is typed by the param. Remove one to unset it.'}
        >
          <div class="params">
            {#each params as row (row.key)}
              {@const schema = paramByKey.get(row.key)}
              <div class="param-row">
                <span class="param-name">{schema?.label ?? row.key}</span>
                <div class="param-input">
                  {#if schema?.kind === 'bool'}
                    <label class="param-toggle">
                      <input
                        type="checkbox"
                        checked={row.value === 'true'}
                        aria-label={`${schema?.label ?? row.key} value`}
                        onchange={(e) =>
                          setParamValue(
                            row.key,
                            (e.currentTarget as HTMLInputElement).checked ? 'true' : 'false'
                          )}
                      />
                      <span>{row.value === 'true' ? 'On' : 'Off'}</span>
                    </label>
                  {:else if schema?.kind === 'enum'}
                    <Select
                      value={row.value}
                      aria-label={`${schema?.label ?? row.key} value`}
                      onchange={(e: Event) =>
                        setParamValue(row.key, (e.currentTarget as HTMLSelectElement).value)}
                    >
                      {#each schema.options ?? [] as opt (opt)}
                        <option value={opt}>{opt}</option>
                      {/each}
                    </Select>
                  {:else}
                    <Input
                      type="number"
                      value={row.value}
                      aria-label={`${schema?.label ?? row.key} value`}
                      oninput={(e: Event) =>
                        setParamValue(row.key, (e.currentTarget as HTMLInputElement).value)}
                    />
                  {/if}
                </div>
                <Button
                  variant="ghost"
                  size="sm"
                  type="button"
                  onclick={() => removeParam(row.key)}
                  aria-label={`Remove ${schema?.label ?? row.key}`}
                >
                  ✕
                </Button>
              </div>
            {/each}

            {#if addableParams.length > 0}
              <div class="param-add">
                <Select bind:value={addParamKey} aria-label="Add param">
                  <option value="">+ Add param…</option>
                  {#each addableParams as p (p.key)}
                    <option value={p.key}>{p.label}</option>
                  {/each}
                </Select>
                <Button
                  variant="ghost"
                  size="sm"
                  type="button"
                  onclick={addParam}
                  disabled={addParamKey === ''}
                >
                  Add
                </Button>
              </div>
            {:else if formatParams.length > 0}
              <p class="inline-note">All of this format’s params are added.</p>
            {/if}
          </div>
        </Field>

        <div class="form-actions">
          <Button variant="ghost" type="button" onclick={cancel} disabled={saving}>Cancel</Button>
          <Button variant="primary" type="submit" loading={saving} disabled={!canSubmit}>
            {editing ? 'Save round' : 'Add round'}
          </Button>
        </div>
      </form>
    {/if}
  </Card>

  <Card
    title="Heats"
    subtitle="Fill each round’s heats from its field, or build one by hand. Run them from Live control."
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
        {#each rounds as round (round.id)}
          <section class="heat-round" aria-label={`Heats for ${round.label}`}>
            <header class="heat-round-head">
              <div class="heat-round-title">
                <span class="round-label">{round.label}</span>
                <span class="meta-chip">
                  {round.classes.length === eventClasses.length && eventClasses.length > 1
                    ? 'All classes'
                    : round.classes.map(className).join(', ') || '—'}
                </span>
              </div>
              <div class="heat-round-actions">
                <Button
                  variant="ghost"
                  size="sm"
                  onclick={() => toggleStandings(round)}
                  aria-pressed={standingsRound === round.id}
                >
                  {standingsRound === round.id ? 'Hide standings' : 'Standings'}
                </Button>
                <Button
                  variant="secondary"
                  size="sm"
                  onclick={() => openAdvance(round)}
                  disabled={advanceRoundId !== undefined}
                >
                  Advance to bracket
                </Button>
                <Button
                  variant="primary"
                  size="sm"
                  onclick={() => fillRound(round)}
                  loading={fillingRound === round.id}
                  disabled={fillingRound !== undefined}
                >
                  Fill next heat
                </Button>
              </div>
            </header>

            {#if standingsRound === round.id}
              <div class="round-standings" aria-label={`Standings for ${round.label}`}>
                <h4 class="standings-title">Standings — seeds the bracket</h4>
                {#if standingsLoading}
                  <p class="empty small" role="status">Loading standings…</p>
                {:else if standingsError}
                  <p class="empty small" role="status">
                    No ranking yet — score this round's heats first.
                  </p>
                {:else if standingsRows.length === 0}
                  <p class="empty small" role="status">No ranked competitors yet.</p>
                {:else}
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

            {#if advanceRoundId === round.id}
              <form
                class="advance-form"
                aria-label={`Advance ${round.label} to bracket`}
                onsubmit={(e) => {
                  e.preventDefault();
                  submitAdvance(round);
                }}
              >
                <h4 class="standings-title">Advance to bracket</h4>
                <p class="advance-note">
                  Creates a <strong>single_elim</strong> round seeded from
                  <strong>{round.label}</strong>'s ranking, then fills the seeded bracket heats. The
                  bracket is editable afterward.
                </p>
                <div class="form-grid">
                  <Field label="Bracket label" required>
                    <Input bind:value={advanceLabel} aria-label="Bracket label" />
                  </Field>
                  <Field
                    label="Top N advance"
                    hint="Defaults to the largest power-of-two that fits the field."
                  >
                    <Input
                      type="number"
                      min="1"
                      bind:value={advanceTopN}
                      aria-label="Top N advance"
                    />
                  </Field>
                </div>
                <div class="form-actions">
                  <Button
                    variant="ghost"
                    type="button"
                    onclick={cancelAdvance}
                    disabled={advancing}
                  >
                    Cancel
                  </Button>
                  <Button
                    variant="primary"
                    type="submit"
                    loading={advancing}
                    disabled={advanceLabel.trim().length === 0}
                  >
                    Create &amp; fill bracket
                  </Button>
                </div>
              </form>
            {/if}

            {#if heatsByRound(round.id).length === 0}
              <p class="empty small" role="status">
                No heats yet — <strong>Fill next heat</strong> to draw the first from this round’s field.
              </p>
            {:else}
              <ol class="heat-list">
                {#each heatsByRound(round.id) as h (h.heat)}
                  {@const channels = channelByRef(h)}
                  <li class="heat-row" class:current={h.is_current}>
                    <div class="heat-main">
                      <div class="heat-head">
                        <span class="heat-id">{h.heat}</span>
                        {#if h.is_current}<span class="current-pill">Current</span>{/if}
                        <span class={`status-pill ${statusKind(h.phase)}`}>{statusLabel(h)}</span>
                      </div>
                      <div class="lineup">
                        {#each h.lineup as ref, i (ref)}
                          <span class="lineup-pilot">
                            <span class="lineup-num" aria-hidden="true">{i + 1}</span>
                            <span class="lineup-call">{callsign(ref)}</span>
                            <span class="lineup-chan" class:none={!channels.get(ref)}>
                              {channels.get(ref) ?? '—'}
                            </span>
                          </span>
                        {/each}
                        {#if h.lineup.length === 0}<span class="lineup-empty">— no pilots —</span
                          >{/if}
                      </div>
                    </div>
                  </li>
                {/each}
              </ol>
            {/if}
          </section>
        {/each}
      </div>
    {/if}

    {#if buildOpen}
      <form
        class="build-form"
        aria-label="Build heat"
        onsubmit={(e) => {
          e.preventDefault();
          submitBuild();
        }}
      >
        <h3 class="form-title">Build a heat by hand</h3>
        <div class="form-grid">
          <Field label="Round" required>
            <Select bind:value={buildRound} aria-label="Build round">
              <option value="" disabled>Choose a round…</option>
              {#each rounds as r (r.id)}
                <option value={r.id}>{r.label}</option>
              {/each}
            </Select>
          </Field>
          <Field label="Heat id" required>
            <Input bind:value={buildHeatId} placeholder="e.g. q-1" aria-label="Build heat id" />
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
                  onchange={() => toggleMember(pid)}
                  aria-label={`Select ${callsign(pid)}`}
                />
                <span>{callsign(pid)}</span>
              </label>
            {/each}
          </div>
        </Field>

        <div class="form-actions">
          <Button variant="ghost" type="button" onclick={cancelBuild} disabled={building}>
            Cancel
          </Button>
          <Button variant="primary" type="submit" loading={building} disabled={!canBuild}>
            Schedule heat
          </Button>
        </div>
      </form>
    {/if}
  </Card>
</section>

<style>
  .event-rounds {
    max-width: 52rem;
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

  .round-form {
    margin-top: var(--gf-space-4);
    padding-top: var(--gf-space-4);
    border-top: 1px solid var(--gf-border-subtle);
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .form-title {
    margin: 0;
    font-size: var(--gf-font-size-md);
    color: var(--gf-text);
  }
  .form-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(11rem, 1fr));
    gap: var(--gf-space-3);
  }
  .class-picker {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-2);
  }
  .class-chip {
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
  .class-chip input {
    width: 1.05rem;
    height: 1.05rem;
    accent-color: var(--gf-accent);
    cursor: pointer;
  }
  .params {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .param-row {
    display: grid;
    grid-template-columns: minmax(8rem, 1fr) 1fr auto;
    gap: var(--gf-space-2);
    align-items: center;
  }
  .param-name {
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-medium);
    color: var(--gf-text);
  }
  .param-input {
    min-width: 0;
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
  .param-add {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: var(--gf-space-2);
    align-items: center;
  }
  .inline-note {
    margin: 0;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
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
    gap: var(--gf-space-2);
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
  }
  .heat-round-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-3);
  }
  .heat-round-title {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: var(--gf-space-2);
  }
  .heat-round-actions {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
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
  .advance-form {
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-accent);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface);
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .advance-note {
    margin: 0;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-secondary);
    line-height: 1.5;
  }
  .advance-note strong {
    color: var(--gf-text);
  }
  .empty.small {
    font-size: var(--gf-font-size-sm);
  }
  .empty strong {
    color: var(--gf-text);
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
    margin-top: var(--gf-space-4);
    padding-top: var(--gf-space-4);
    border-top: 1px solid var(--gf-border-subtle);
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
