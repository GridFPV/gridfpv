<script lang="ts">
  /**
   * Audit — the event-wide, searchable "defensible results" review surface.
   *
   * Marshaling is the *do the work* page; this is the *review the work* page: every heat's
   * marshaling audit fold, heat-tagged and merged newest-first by the Director
   * (`GET /events/{id}/audit`), rendered as one filterable ruling history. Filtering is
   * client-side (pilot / heat / kind / free text) over the full list — an event's ruling count is
   * human-scale, so no server query surface is warranted. Each row's heat and pilot render as
   * **clickable chips** that set the corresponding filter, and other screens jump here
   * pre-filtered through the {@link consumeAuditPrefilter} seam (Marshaling's "View full audit →",
   * a Results row's pilot audit link).
   *
   * Pure read, role-agnostic: a read-only pilot session sees exactly what the RD sees — the
   * accountability surface is *for* the pilots as much as the RD. Friendly names everywhere
   * (CLAUDE.md): competitor refs resolve through the shared resolver, heat ids through
   * `heatNameById`, and the server-baked "(ref N)" clauses are re-composed by the shared #337
   * helpers (`auditRender.ts`) — the same rendering Marshaling's strip uses, so the two surfaces
   * can never disagree on a line.
   */
  import type {
    ChannelCatalogEntry,
    CompetitorRef,
    EventAuditEntry,
    HeatId,
    HeatSummary,
    Pilot,
    PilotId,
    PilotProgress
  } from '@gridfpv/types';
  import { toast } from '@gridfpv/components';
  import {
    auditActionText,
    auditBadge,
    auditCompetitor,
    auditTime,
    type AuditRenderInputs
  } from '../lib/auditRender.js';
  import { consumeAuditPrefilter } from '../lib/auditFilter.svelte.js';
  import { channelLabel } from '../lib/channels.js';
  import { createCompetitorNameResolver } from '../lib/competitorName.js';
  import { heatNameById } from '../lib/heats.js';
  import type { Session } from '../lib/session.svelte.js';

  let { session }: { session: Session } = $props();

  // ── The event-wide audit read ───────────────────────────────────────────────────────────────
  // Fetched on mount and re-fetched whenever the stream advances (`protocolState` — a fresh
  // ruling appended by this or another client re-folds server-side) or the event settles
  // (`currentEvent` — the mount-during-resolve race, see Marshaling's #236 note). A FAILED read
  // must be visible (#340): no silent [] — the error state renders with an explicit retry and
  // the last good list is kept rather than wiped.
  let entries = $state<EventAuditEntry[]>([]);
  let entriesLoaded = $state(false);
  let auditError = $state(false);
  let pilotsError = $state(false);
  let heatsError = $state(false);
  let retryNonce = $state(0);
  const loadError = $derived(auditError || pilotsError || heatsError);
  function retry(): void {
    retryNonce += 1;
  }
  /** Toast once on the transition INTO the error state (the effects re-run on every tick). */
  function noteLoadError(alreadyFailing: boolean): void {
    if (!alreadyFailing) toast.error('Couldn’t load the audit trail — showing the last good data.');
  }
  $effect(() => {
    void session.currentEvent;
    void session.protocolState;
    void retryNonce;
    session
      .eventAudit()
      .then((list) => ((entries = list), (entriesLoaded = true), (auditError = false)))
      .catch(() => {
        noteLoadError(loadError);
        auditError = true;
      });
  });

  // The directories the resolvers read (the Marshaling pattern): pilots (callsigns), heats
  // (friendly heat names + the channel map), channel catalog. Same #340 treatment.
  let pilots = $state<Pilot[]>([]);
  let heats = $state<HeatSummary[]>([]);
  let catalog = $state<ChannelCatalogEntry[]>([]);
  $effect(() => {
    void session.currentEvent;
    void session.protocolState;
    void retryNonce;
    session
      .listPilots()
      .then((p) => ((pilots = p), (pilotsError = false)))
      .catch(() => {
        noteLoadError(loadError);
        pilotsError = true;
      });
  });
  $effect(() => {
    void session.currentEvent;
    void session.protocolState;
    void retryNonce;
    session
      .listHeats()
      .then((h) => ((heats = h), (heatsError = false)))
      .catch(() => {
        noteLoadError(loadError);
        heatsError = true;
      });
  });
  $effect(() => {
    session
      .listChannels()
      .then((c) => (catalog = c))
      .catch(() => (catalog = []));
  });

  // ── Friendly names (the shared resolvers) ───────────────────────────────────────────────────
  // Event-wide, so the channel map is the UNION of every heat's frequency assignment and the
  // explicit bindings come from the live stream's current heat (the Results pattern) — the bulk
  // of refs are roster-seeded pilot ids, which resolve from the directory alone.
  const pilotById = $derived(new Map<PilotId, Pilot>(pilots.map((p) => [p.id, p])));
  const explicitPilotByRef = $derived(
    new Map<CompetitorRef, PilotId>(
      (session.liveState?.progress ?? [])
        .filter((p): p is PilotProgress & { pilot: PilotId } => p.pilot != null)
        .map((p) => [p.competitor, p.pilot])
    )
  );
  const channelByRef = $derived.by(() => {
    const map = new Map<CompetitorRef, string>();
    for (const h of heats)
      for (const [ref, mhz] of h.frequencies ?? []) map.set(ref, channelLabel(mhz, catalog));
    return map;
  });
  const competitorName = $derived.by<(ref: CompetitorRef) => string>(() =>
    createCompetitorNameResolver({ pilotById, explicitPilotByRef, channelByRef })
  );
  const heatName = (heat: HeatId): string =>
    heatNameById(heat, heats, session.currentEvent?.rounds ?? []);

  // The shared #337 line-composition inputs: the by-offset index lets a protest resolution /
  // reversal chase the entry it targeted for its pilot. No lap lists in hand event-wide, so
  // `competitorForPassRef` is omitted — a lap-addressed void simply shows no pilot chip.
  const renderInputs = $derived<AuditRenderInputs>({
    byRef: new Map(entries.map((e) => [e.at_ref, e])),
    competitorName
  });
  const rowPilot = (entry: EventAuditEntry): CompetitorRef | undefined =>
    auditCompetitor(entry, renderInputs);

  // ── Filters (client-side) ───────────────────────────────────────────────────────────────────
  // '' means All. Options are derived from the entries actually present, so the dropdowns never
  // offer a filter that matches nothing.
  let pilotFilter = $state<CompetitorRef | ''>('');
  let heatFilter = $state<HeatId | ''>('');
  let kindFilter = $state<string | ''>('');
  let textFilter = $state('');

  // Cross-screen prefilter (the auditFilter seam): consumed ONCE on mount, then cleared — a
  // later manual visit starts unfiltered.
  const prefilter = consumeAuditPrefilter();
  if (prefilter?.heat) heatFilter = prefilter.heat;
  if (prefilter?.pilot) pilotFilter = prefilter.pilot;

  const pilotOptions = $derived.by<CompetitorRef[]>(() => {
    const refs: CompetitorRef[] = [];
    for (const entry of entries) {
      const ref = rowPilot(entry);
      if (ref !== undefined && !refs.includes(ref)) refs.push(ref);
    }
    // Keep the pinned prefilter/selection offered even when no current entry resolves to it.
    if (pilotFilter !== '' && !refs.includes(pilotFilter)) refs.push(pilotFilter);
    return refs.sort((a, b) => competitorName(a).localeCompare(competitorName(b)));
  });
  const heatOptions = $derived.by<HeatId[]>(() => {
    const ids: HeatId[] = [];
    for (const entry of entries) if (!ids.includes(entry.heat)) ids.push(entry.heat);
    if (heatFilter !== '' && !ids.includes(heatFilter)) ids.push(heatFilter);
    return ids;
  });
  const kindOptions = $derived.by<string[]>(() => {
    const kinds: string[] = [];
    for (const entry of entries) {
      const badge = auditBadge(entry);
      if (!kinds.includes(badge)) kinds.push(badge);
    }
    return kinds;
  });

  /** The full rendered row text the free-text box matches against (what the user *sees*). */
  function rowText(entry: EventAuditEntry): string {
    const ref = rowPilot(entry);
    return [
      auditBadge(entry),
      ref !== undefined ? competitorName(ref) : '',
      heatName(entry.heat),
      auditActionText(entry),
      auditTime(entry.at)
    ]
      .join(' ')
      .toLowerCase();
  }

  const filtered = $derived(
    entries.filter((entry) => {
      if (heatFilter !== '' && entry.heat !== heatFilter) return false;
      if (pilotFilter !== '' && rowPilot(entry) !== pilotFilter) return false;
      if (kindFilter !== '' && auditBadge(entry) !== kindFilter) return false;
      const needle = textFilter.trim().toLowerCase();
      if (needle !== '' && !rowText(entry).includes(needle)) return false;
      return true;
    })
  );
  const anyFilter = $derived(
    pilotFilter !== '' || heatFilter !== '' || kindFilter !== '' || textFilter.trim() !== ''
  );
  function clearFilters(): void {
    pilotFilter = '';
    heatFilter = '';
    kindFilter = '';
    textFilter = '';
  }
</script>

<section class="audit-page" aria-label="Audit">
  <header>
    <h2>Audit</h2>
    <p class="muted">
      Every ruling across the event, newest first — each one a recorded, reversible fact. Click a
      pilot or heat to filter to it.
    </p>
  </header>

  {#if loadError}
    <!-- A failed read must surface (#340): visible error + explicit retry, last good data kept. -->
    <div class="load-error" role="alert">
      <p>Couldn’t load the audit trail{pilotsError || heatsError ? ' directory' : ''}.</p>
      <button type="button" onclick={retry}>Try again</button>
    </div>
  {/if}

  <div class="filters">
    <label
      >Pilot
      <select bind:value={pilotFilter} aria-label="Filter by pilot">
        <option value="">All pilots</option>
        {#each pilotOptions as ref (ref)}
          <option value={ref}>{competitorName(ref)}</option>
        {/each}
      </select>
    </label>
    <label
      >Heat
      <select bind:value={heatFilter} aria-label="Filter by heat">
        <option value="">All heats</option>
        {#each heatOptions as id (id)}
          <option value={id}>{heatName(id)}</option>
        {/each}
      </select>
    </label>
    <label
      >Kind
      <select bind:value={kindFilter} aria-label="Filter by kind">
        <option value="">All kinds</option>
        {#each kindOptions as kind (kind)}
          <option value={kind}>{kind}</option>
        {/each}
      </select>
    </label>
    <label class="grow"
      >Search
      <input
        type="search"
        bind:value={textFilter}
        placeholder="Match any text on a row"
        aria-label="Search audit entries"
      />
    </label>
    {#if anyFilter}
      <button type="button" class="clear" onclick={clearFilters}>Clear filters</button>
    {/if}
  </div>

  {#if filtered.length > 0}
    <ol class="entries" aria-label="Audit entries">
      {#each filtered as entry (entry.at_ref)}
        {@const pilot = rowPilot(entry)}
        <li class="entry kind-{entry.kind}">
          <span class="badge">{auditBadge(entry)}</span>
          <span class="who-what">
            {#if pilot !== undefined}
              <button
                type="button"
                class="chip pilot-chip"
                onclick={() => (pilotFilter = pilot)}
                title="Filter to this pilot"
              >
                {competitorName(pilot)}
              </button>
            {/if}
            <button
              type="button"
              class="chip heat-chip"
              onclick={() => (heatFilter = entry.heat)}
              title="Filter to this heat"
            >
              {heatName(entry.heat)}
            </button>
            <span class="summary">{auditActionText(entry)}</span>
          </span>
          <span class="detail">
            {#if entry.at != null}
              <span class="at">{auditTime(entry.at)}</span>
            {/if}
            <span class="ref">#{entry.at_ref}</span>
          </span>
        </li>
      {/each}
    </ol>
  {:else if !entriesLoaded && !auditError}
    <p class="empty" role="status">Loading the audit trail…</p>
  {:else if entries.length > 0}
    <p class="empty" role="status">No entries match the current filters.</p>
  {:else}
    <p class="empty" role="status">
      No rulings in this event yet — the raw timer output stands everywhere.
    </p>
  {/if}
</section>

<style>
  .audit-page {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
  }
  h2 {
    font-size: var(--gf-font-size-xl);
    margin: 0;
    letter-spacing: var(--gf-tracking-tight);
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    margin: var(--gf-space-1) 0 0;
  }
  /* The failed-read state (#340): visible, with an explicit retry. */
  .load-error {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--gf-space-3);
    padding: var(--gf-space-3) var(--gf-space-4);
    border: 1px solid color-mix(in srgb, var(--gf-danger) 45%, var(--gf-border));
    border-radius: var(--gf-radius-md);
    background: var(--gf-danger-soft);
  }
  .load-error p {
    margin: 0;
    color: var(--gf-text);
    font-size: var(--gf-font-size-sm);
  }

  .filters {
    display: flex;
    flex-wrap: wrap;
    align-items: end;
    gap: var(--gf-space-3);
    padding: var(--gf-space-4) var(--gf-space-5);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    background: var(--gf-elevated);
  }
  label {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
    font-size: var(--gf-font-size-2xs);
    font-weight: var(--gf-font-weight-semibold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .grow {
    flex: 1 1 14rem;
  }
  .grow input {
    width: 100%;
  }
  input,
  select {
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-normal);
    text-transform: none;
    letter-spacing: normal;
    height: 2.1rem;
    padding: 0 var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    color: var(--gf-text);
  }
  input:focus,
  select:focus {
    outline: none;
    border-color: var(--gf-accent);
    box-shadow: 0 0 0 3px var(--gf-accent-soft);
  }
  button {
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
    height: 2.1rem;
    padding: 0 var(--gf-space-4);
    border-radius: var(--gf-radius-sm);
    border: 1px solid var(--gf-border);
    background: var(--gf-elevated);
    color: var(--gf-text);
    cursor: pointer;
  }
  button:hover {
    background: var(--gf-elevated-hover);
    border-color: var(--gf-border-strong);
  }
  button:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }

  /* The entry rows: field-readable — normal-size body text, generous spacing, strong left-edge
     kind coloring (the same convention Marshaling's audit entries used). */
  .entries {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .entry {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--gf-space-2) var(--gf-space-3);
    padding: var(--gf-space-3) var(--gf-space-4);
    border-left: 3px solid var(--gf-border-strong);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-elevated);
    font-size: var(--gf-font-size-md);
  }
  .entry.kind-Voided,
  .entry.kind-HeatVoided {
    border-left-color: var(--gf-danger);
  }
  .entry.kind-RulingReversed {
    border-left-color: var(--gf-accent);
  }
  .badge {
    font-weight: var(--gf-font-weight-semibold);
    font-size: var(--gf-font-size-2xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
    white-space: nowrap;
    min-width: 8.5em;
  }
  .who-what {
    flex: 1 1 20rem;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--gf-space-2);
    min-width: 0;
  }
  .chip {
    height: auto;
    padding: 0.1em 0.6em;
    border-radius: var(--gf-radius-pill);
    font-size: var(--gf-font-size-sm);
    background: var(--gf-surface-sunken);
    color: var(--gf-text-secondary);
  }
  .chip:hover {
    border-color: var(--gf-accent);
    color: var(--gf-accent);
    background: var(--gf-surface-sunken);
  }
  .pilot-chip {
    font-weight: var(--gf-font-weight-semibold);
    color: var(--gf-text);
  }
  .summary {
    color: var(--gf-text);
  }
  .detail {
    display: flex;
    align-items: baseline;
    gap: var(--gf-space-3);
    margin-left: auto;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-2xs);
    font-family: var(--gf-font-mono);
    white-space: nowrap;
  }
  .empty {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-md);
    margin: 0;
  }
</style>
