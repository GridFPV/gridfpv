<script lang="ts">
  /**
   * EventClassesRoster — the combined **Classes & Roster** stage (race redesign Slice 7b).
   *
   * This single screen merges what used to be two tabs (Classes + Roster) into one: pick the
   * **classes** this event runs (top), then settle the **roster** and per-class **placement** with
   * each placed pilot's **fixed channel** (below). Both the class CRUD and the pilot CRUD stay
   * embedded (`ClassManager` / `PilotManager`) so the RD never leaves the event to add one.
   *
   * Three things this stage does that the old split didn't:
   *
   *  1. **Single-class auto-fill** — when the event has *exactly one* class, every present pilot is
   *     automatically a member of it (no per-class checkboxes to tick); the placement just tracks the
   *     roster. With ≥2 classes the per-class placement grid returns.
   *  2. **Per-pilot channel** — each placed pilot gets a channel dropdown drawn from the event's
   *     **primary timer**'s `available_channels` (resolved via {@link Session.primaryTimerId} →
   *     {@link Session.selectedTimers} → the channel catalog from `GET /channels` for labels). Saving
   *     membership sends `MemberSlot { pilot, channel }`.
   *  3. **Binding folded into the channel** — the old manual `(adapter, competitor) → pilot` bind form
   *     is gone: the channel *is* the static binding. The timing **source** defaults to the primary
   *     timer and only surfaces (a source picker) when the event has more than one timing source.
   *
   * Field-readable (large text, dark) and consistent with the other stage screens.
   */
  import { Badge, Button, Card, Select, toast } from '@gridfpv/components';
  import type { Class, ClassId, MemberSlot, Pilot, PilotId, TimerId } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import { channelLabel } from '../lib/channels.js';
  import ClassManager from './ClassManager.svelte';
  import PilotManager from './PilotManager.svelte';

  let { session }: { session: Session } = $props();

  let classManager = $state<ClassManager | undefined>(undefined);
  let pilotManager = $state<PilotManager | undefined>(undefined);

  // ── 1. Class selection (top) ───────────────────────────────────────────────
  // The directory list (kept in sync by the manager via `bind:classes`) and the working selection
  // (a set of class ids), seeded from the event and edited locally until the RD saves.
  let classes = $state<Class[]>([]);
  let selectedClasses = $state<Set<ClassId>>(new Set());
  let savedClasses = $state<ClassId[]>([]);
  let savingClasses = $state(false);

  function syncClassesFromEvent() {
    const ids = session.currentEvent?.classes ?? [];
    savedClasses = [...ids];
    selectedClasses = new Set(ids);
  }
  $effect(() => {
    syncClassesFromEvent();
  });

  function onClassDirectoryChange(list: Class[]) {
    const present = new Set(list.map((c) => c.id));
    const next = new Set<ClassId>();
    for (const id of selectedClasses) if (present.has(id)) next.add(id);
    selectedClasses = next;
  }
  function toggleClass(id: ClassId) {
    const next = new Set(selectedClasses);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    selectedClasses = next;
  }
  const orderedClassSelection = $derived(
    classes.filter((c) => selectedClasses.has(c.id)).map((c) => c.id)
  );
  const classesChanged = $derived(
    orderedClassSelection.length !== savedClasses.length ||
      orderedClassSelection.some((id, i) => id !== savedClasses[i])
  );
  async function saveClasses() {
    if (savingClasses || !classesChanged) return;
    savingClasses = true;
    try {
      const updated = await session.setEventClasses(orderedClassSelection);
      if (!updated) {
        toast.info('A control token is required to set the event’s classes.');
        return;
      }
      syncClassesFromEvent();
      toast.success('Event classes saved.');
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      savingClasses = false;
    }
  }
  function resetClasses() {
    syncClassesFromEvent();
  }

  // ── 2. Present pilots (the roster) ─────────────────────────────────────────
  let pilots = $state<Pilot[]>([]);
  let selectedPilots = $state<Set<PilotId>>(new Set());
  let savedRoster = $state<PilotId[]>([]);
  let savingRoster = $state(false);

  const eventRoster = $derived(session.currentEvent?.roster ?? []);
  const eventRosterKey = $derived(eventRoster.join(','));

  function syncRosterFromEvent() {
    savedRoster = [...eventRoster];
    selectedPilots = new Set(eventRoster);
  }
  // Re-seed whenever the saved roster changes — on mount, and live when the sim reconciler grows
  // the roster off the stream (mirrors the old EventRoster behaviour).
  $effect(() => {
    if (eventRosterKey !== savedRoster.join(',')) syncRosterFromEvent();
  });

  function onPilotDirectoryChange(list: Pilot[]) {
    const present = new Set(list.map((p) => p.id));
    const next = new Set<PilotId>();
    for (const id of selectedPilots) if (present.has(id)) next.add(id);
    selectedPilots = next;
  }
  function togglePilot(id: PilotId) {
    const next = new Set(selectedPilots);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    selectedPilots = next;
  }
  const orderedRoster = $derived(pilots.filter((p) => selectedPilots.has(p.id)).map((p) => p.id));
  const rosterChanged = $derived(
    orderedRoster.length !== savedRoster.length ||
      orderedRoster.some((id, i) => id !== savedRoster[i])
  );
  async function saveRoster() {
    if (savingRoster || !rosterChanged) return;
    savingRoster = true;
    try {
      const updated = await session.setEventRoster(orderedRoster);
      if (!updated) {
        toast.info('A control token is required to set the event’s roster.');
        return;
      }
      syncRosterFromEvent();
      toast.success('Present pilots saved.');
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      savingRoster = false;
    }
  }
  function resetRoster() {
    syncRosterFromEvent();
  }

  // ── Class directory (resolve the event's selected class ids → names) ────────
  let classDir = $state<Class[]>([]);
  $effect(() => {
    void (async () => {
      try {
        classDir = await session.listClasses();
      } catch {
        /* a name lookup failure just falls back to the raw id */
      }
    })();
  });
  const classNameOf = $derived((id: ClassId) => classDir.find((c) => c.id === id)?.name ?? id);

  // The event's selected classes, in order; placement only makes sense for a present pilot.
  const eventClasses = $derived(session.currentEvent?.classes ?? []);
  const singleClass = $derived(eventClasses.length === 1 ? eventClasses[0] : undefined);
  const rosterPilots = $derived(
    eventRoster
      .map((id) => pilots.find((p) => p.id === id))
      .filter((p): p is Pilot => p !== undefined)
  );

  // ── 3. Channel pool (the primary timer's available channels) ───────────────
  // The catalog labels a raw-MHz channel as "band channel" (e.g. "Raceband R1"). An open read.
  let catalog = $state<import('@gridfpv/types').ChannelCatalogEntry[]>([]);
  $effect(() => {
    void session
      .listChannels()
      .then((list) => (catalog = list))
      .catch(() => (catalog = []));
  });

  // The event's selected timers, the effective primary, and which timing source we draw channels
  // from. The source defaults to the primary timer; the picker only surfaces with >1 source.
  const selectedTimers = $derived(session.selectedTimers);
  const primaryTimerId = $derived(session.primaryTimerId);
  let channelSource = $state<TimerId | undefined>(undefined);
  // Keep the chosen source pinned to the primary by default, and valid as the selection changes.
  $effect(() => {
    const ids = selectedTimers.map((t) => t.id);
    if (channelSource === undefined || !ids.includes(channelSource)) {
      channelSource = primaryTimerId;
    }
  });
  const sourceTimer = $derived(selectedTimers.find((t) => t.id === channelSource));
  // The raw-MHz channel pool the dropdowns offer, plus a label for each.
  const channelPool = $derived(sourceTimer?.available_channels ?? []);
  const channelOptions = $derived(
    channelPool.map((mhz) => ({ mhz, label: channelLabel(mhz, catalog) }))
  );
  const hasChannelPool = $derived(channelOptions.length > 0);

  // ── Per-class membership (with each member's channel) ──────────────────────
  // The saved membership off the event: `classId → (pilotId → channel?)`.
  const savedMembership = $derived.by(() => {
    const map = new Map<ClassId, Map<PilotId, number | undefined>>();
    for (const m of session.currentEvent?.classes_membership ?? []) {
      const inner = new Map<PilotId, number | undefined>();
      for (const s of m.pilots) inner.set(s.pilot, s.channel);
      map.set(m.class, inner);
    }
    return map;
  });

  // The working membership the RD edits. Seeded from the event, re-seeded when the saved membership
  // changes (a save re-homes `currentEvent`). For a *single*-class event we additionally auto-fill
  // the lone class with every roster pilot (preserving any channel the RD already chose).
  let membership = $state<Map<ClassId, Map<PilotId, number | undefined>>>(new Map());
  const membershipKey = $derived(
    (session.currentEvent?.classes_membership ?? [])
      .map((m) => `${m.class}:${m.pilots.map((s) => `${s.pilot}#${s.channel ?? ''}`).join('.')}`)
      .join('|')
  );
  let lastMembershipKey = $state(' ');
  $effect(() => {
    if (membershipKey !== lastMembershipKey) {
      const next = new Map<ClassId, Map<PilotId, number | undefined>>();
      for (const [cls, inner] of savedMembership) next.set(cls, new Map(inner));
      membership = next;
      lastMembershipKey = membershipKey;
    }
  });

  // Single-class auto-fill: keep the lone class's membership in sync with the roster — add every
  // present pilot that isn't a member yet (channel unset), and drop members no longer rostered.
  // Runs whenever the roster or the single-class identity changes; never clobbers a chosen channel.
  $effect(() => {
    const cls = singleClass;
    if (!cls) return;
    const rosterIds = eventRoster;
    const current = membership.get(cls) ?? new Map<PilotId, number | undefined>();
    let changed = false;
    const next = new Map(current);
    for (const id of rosterIds)
      if (!next.has(id)) {
        next.set(id, undefined);
        changed = true;
      }
    for (const id of [...next.keys()])
      if (!rosterIds.includes(id)) {
        next.delete(id);
        changed = true;
      }
    if (changed) {
      const m = new Map(membership);
      m.set(cls, next);
      membership = m;
    }
  });

  function membersOf(classId: ClassId): Map<PilotId, number | undefined> {
    return membership.get(classId) ?? new Map();
  }
  function isMember(classId: ClassId, pilotId: PilotId): boolean {
    return membersOf(classId).has(pilotId);
  }
  function toggleMember(classId: ClassId, pilotId: PilotId) {
    const next = new Map(membership);
    const inner = new Map(next.get(classId) ?? []);
    if (inner.has(pilotId)) inner.delete(pilotId);
    else inner.set(pilotId, undefined);
    next.set(classId, inner);
    membership = next;
  }
  // Set (or clear, on '') a member's channel. The `<select>` value is the raw MHz as a string.
  function setChannel(classId: ClassId, pilotId: PilotId, raw: string) {
    const next = new Map(membership);
    const inner = new Map(next.get(classId) ?? []);
    inner.set(pilotId, raw === '' ? undefined : Number(raw));
    next.set(classId, inner);
    membership = next;
  }
  function channelOf(classId: ClassId, pilotId: PilotId): number | undefined {
    return membersOf(classId).get(pilotId);
  }

  // The slots to save for a class, in roster order (a stable order), each with its channel.
  function orderedSlots(classId: ClassId): MemberSlot[] {
    const inner = membersOf(classId);
    return eventRoster
      .filter((id) => inner.has(id))
      .map((id) => {
        const channel = inner.get(id);
        return channel === undefined ? { pilot: id } : { pilot: id, channel };
      });
  }
  function membershipChanged(classId: ClassId): boolean {
    const next = orderedSlots(classId);
    const prev = savedMembership.get(classId) ?? new Map<PilotId, number | undefined>();
    if (next.length !== prev.size) return true;
    return next.some((s) => !prev.has(s.pilot) || (prev.get(s.pilot) ?? undefined) !== s.channel);
  }

  let savingClass = $state<ClassId | undefined>(undefined);
  async function saveMembership(classId: ClassId) {
    if (savingClass) return;
    savingClass = classId;
    try {
      const updated = await session.setClassMembership(classId, orderedSlots(classId));
      if (!updated) {
        toast.info('A control token is required to set class membership.');
        return;
      }
      toast.success(`Membership for “${classNameOf(classId)}” saved.`);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      savingClass = undefined;
    }
  }
</script>

<section class="classes-roster" aria-label="Classes and roster">
  <!-- ── 1. Classes ───────────────────────────────────────────────────────── -->
  <Card
    title="Classes for this event"
    subtitle="Add, edit, or remove classes in the directory, and tick which ones this event runs."
  >
    {#snippet actions()}
      <Button variant="secondary" size="sm" onclick={() => classManager?.openAdd()}>
        + Add class
      </Button>
    {/snippet}

    <p class="count-line" aria-live="polite">
      <strong>{orderedClassSelection.length}</strong> of {classes.length}
      {classes.length === 1 ? 'class' : 'classes'} selected for this event
    </p>

    <ClassManager
      bind:this={classManager}
      {session}
      bind:classes
      onchange={onClassDirectoryChange}
      rowChecked={(c) => selectedClasses.has(c.id)}
    >
      {#snippet rowLead(cls)}
        <input
          type="checkbox"
          class="select-box"
          checked={selectedClasses.has(cls.id)}
          onchange={() => toggleClass(cls.id)}
          aria-label={`Select ${cls.name}`}
        />
      {/snippet}

      {#snippet listFooter()}
        <div class="foot">
          <span class="count" aria-live="polite">{orderedClassSelection.length} selected</span>
          <div class="foot-actions">
            {#if classesChanged}
              <Button variant="ghost" onclick={resetClasses} disabled={savingClasses}>Reset</Button>
            {/if}
            <Button
              variant="primary"
              onclick={saveClasses}
              loading={savingClasses}
              disabled={!classesChanged}
            >
              Save classes
            </Button>
          </div>
        </div>
      {/snippet}
    </ClassManager>
  </Card>

  <!-- ── 2. Present pilots (roster) ───────────────────────────────────────── -->
  <Card
    title="Present pilots"
    subtitle="Who is here at the event. Tick a directory pilot to mark them present, or add a new one — sim players appear automatically as they join."
  >
    {#snippet actions()}
      <Button variant="secondary" size="sm" onclick={() => pilotManager?.openAdd()}>
        + Add pilot
      </Button>
    {/snippet}

    <p class="count-line" aria-live="polite">
      <strong>{orderedRoster.length}</strong> of {pilots.length}
      {pilots.length === 1 ? 'pilot' : 'pilots'} present at this event
    </p>

    <PilotManager
      bind:this={pilotManager}
      {session}
      bind:pilots
      onchange={onPilotDirectoryChange}
      rowChecked={(p) => selectedPilots.has(p.id)}
    >
      {#snippet rowLead(pilot)}
        <input
          type="checkbox"
          class="select-box"
          checked={selectedPilots.has(pilot.id)}
          onchange={() => togglePilot(pilot.id)}
          aria-label={`Roster ${pilot.callsign}`}
        />
      {/snippet}

      {#snippet listFooter()}
        <div class="foot">
          <span class="count" aria-live="polite">{orderedRoster.length} present</span>
          <div class="foot-actions">
            {#if rosterChanged}
              <Button variant="ghost" onclick={resetRoster} disabled={savingRoster}>Reset</Button>
            {/if}
            <Button
              variant="primary"
              onclick={saveRoster}
              loading={savingRoster}
              disabled={!rosterChanged}
            >
              Save roster
            </Button>
          </div>
        </div>
      {/snippet}
    </PilotManager>
  </Card>

  <!-- ── 3. Placement + channels ──────────────────────────────────────────── -->
  <Card
    title="Placement & channels"
    subtitle="Place present pilots into each class, and assign the channel each one flies. The channel is the pilot's fixed binding for time-trial / qualifying rounds."
  >
    {#snippet actions()}
      {#if selectedTimers.length > 1}
        <label class="source-pick">
          <span class="source-label">Channels from</span>
          <Select bind:value={channelSource} size="sm" aria-label="Channel source timer">
            {#each selectedTimers as t (t.id)}
              <option value={t.id}>{t.name}{t.id === primaryTimerId ? ' (primary)' : ''}</option>
            {/each}
          </Select>
        </label>
      {/if}
    {/snippet}

    {#if eventClasses.length === 0}
      <div class="nudge" role="status">
        <p>This event has no classes selected yet.</p>
        <p class="nudge-sub">Tick the classes it runs above first.</p>
      </div>
    {:else if rosterPilots.length === 0}
      <div class="nudge" role="status">
        <p>No pilots are present yet.</p>
        <p class="nudge-sub">Mark some pilots present above, then place them into classes.</p>
      </div>
    {:else}
      {#if !hasChannelPool}
        <div class="nudge subtle" role="status">
          <p>No channels to assign yet.</p>
          <p class="nudge-sub">
            Pick a timer and give it some <strong>available channels</strong> on the
            <strong>Timers</strong> tab — then each pilot can be assigned one here.
          </p>
        </div>
      {:else if sourceTimer}
        <p class="source-note" aria-live="polite">
          Channels drawn from <strong>{sourceTimer.name}</strong>
          {sourceTimer.id === primaryTimerId ? '(the primary timer)' : ''} — {channelOptions.length}
          available.
        </p>
      {/if}

      {#if singleClass}
        <!-- Single-class auto-fill: every present pilot is a member; just assign channels. -->
        <fieldset class="class-grid" aria-label={`Placement for ${classNameOf(singleClass)}`}>
          <legend class="class-legend">
            <span class="class-name">{classNameOf(singleClass)}</span>
            <Badge tone="neutral">{rosterPilots.length} pilots</Badge>
            <span class="auto-tag">all present pilots (single class)</span>
          </legend>
          <ul class="member-list">
            {#each rosterPilots as pilot (pilot.id)}
              <li class="member-row">
                <span class="member-callsign">{pilot.callsign}</span>
                <div class="member-chan">
                  <Select
                    value={String(channelOf(singleClass, pilot.id) ?? '')}
                    size="sm"
                    disabled={!hasChannelPool}
                    aria-label={`Channel for ${pilot.callsign}`}
                    onchange={(e: Event) =>
                      setChannel(
                        singleClass,
                        pilot.id,
                        (e.currentTarget as HTMLSelectElement).value
                      )}
                  >
                    <option value="">No channel</option>
                    {#each channelOptions as opt (opt.mhz)}
                      <option value={String(opt.mhz)}>{opt.label} · {opt.mhz} MHz</option>
                    {/each}
                  </Select>
                </div>
              </li>
            {/each}
          </ul>
          <div class="class-foot">
            <Button
              variant="primary"
              size="sm"
              onclick={() => saveMembership(singleClass)}
              loading={savingClass === singleClass}
              disabled={!membershipChanged(singleClass) || savingClass !== undefined}
            >
              Save placement
            </Button>
          </div>
        </fieldset>
      {:else}
        <!-- ≥2 classes: a per-class placement grid (checkbox per roster pilot + a channel). -->
        <div class="class-grids">
          {#each eventClasses as classId (classId)}
            {@const dirty = membershipChanged(classId)}
            <fieldset class="class-grid" aria-label={`Placement for ${classNameOf(classId)}`}>
              <legend class="class-legend">
                <span class="class-name">{classNameOf(classId)}</span>
                <Badge tone="neutral">
                  {membersOf(classId).size}
                  {membersOf(classId).size === 1 ? 'pilot' : 'pilots'}
                </Badge>
              </legend>
              <ul class="member-list">
                {#each rosterPilots as pilot (pilot.id)}
                  {@const member = isMember(classId, pilot.id)}
                  <li class="member-row">
                    <label class="member-label">
                      <input
                        type="checkbox"
                        class="select-box"
                        checked={member}
                        onchange={() => toggleMember(classId, pilot.id)}
                        aria-label={`Place ${pilot.callsign} in ${classNameOf(classId)}`}
                      />
                      <span class="member-callsign">{pilot.callsign}</span>
                    </label>
                    {#if member}
                      <div class="member-chan">
                        <Select
                          value={String(channelOf(classId, pilot.id) ?? '')}
                          size="sm"
                          disabled={!hasChannelPool}
                          aria-label={`Channel for ${pilot.callsign} in ${classNameOf(classId)}`}
                          onchange={(e: Event) =>
                            setChannel(
                              classId,
                              pilot.id,
                              (e.currentTarget as HTMLSelectElement).value
                            )}
                        >
                          <option value="">No channel</option>
                          {#each channelOptions as opt (opt.mhz)}
                            <option value={String(opt.mhz)}>{opt.label} · {opt.mhz} MHz</option>
                          {/each}
                        </Select>
                      </div>
                    {/if}
                  </li>
                {/each}
              </ul>
              <div class="class-foot">
                <Button
                  variant="primary"
                  size="sm"
                  onclick={() => saveMembership(classId)}
                  loading={savingClass === classId}
                  disabled={!dirty || savingClass !== undefined}
                >
                  Save placement
                </Button>
              </div>
            </fieldset>
          {/each}
        </div>
      {/if}
    {/if}
  </Card>
</section>

<style>
  .classes-roster {
    max-width: 48rem;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
  }
  .count-line {
    margin: 0 0 var(--gf-space-1);
    font-size: var(--gf-font-size-md);
    color: var(--gf-text-secondary);
  }
  .count-line strong {
    color: var(--gf-text);
    font-variant-numeric: tabular-nums;
  }
  .select-box {
    width: 1.15rem;
    height: 1.15rem;
    accent-color: var(--gf-accent);
    flex-shrink: 0;
    cursor: pointer;
  }
  .foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-3);
    margin-top: var(--gf-space-1);
    padding-top: var(--gf-space-4);
    border-top: 1px solid var(--gf-border-subtle);
  }
  .count {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-muted);
  }
  .foot-actions {
    display: flex;
    gap: var(--gf-space-2);
  }

  .source-pick {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
  }
  .source-label {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
    white-space: nowrap;
  }
  .source-note {
    margin: 0 0 var(--gf-space-2);
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .source-note strong {
    color: var(--gf-text-secondary);
  }

  /* ── Nudge / empty states ─────────────────────────────────────────────── */
  .nudge {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
    padding: var(--gf-space-5);
    border: 1px dashed var(--gf-border);
    border-radius: var(--gf-radius-lg);
    background: var(--gf-surface-alt);
  }
  .nudge.subtle {
    margin-bottom: var(--gf-space-4);
    padding: var(--gf-space-4);
  }
  .nudge p {
    margin: 0;
    font-weight: var(--gf-font-weight-medium);
  }
  .nudge-sub {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-regular) !important;
  }

  /* ── Per-class placement grids ────────────────────────────────────────── */
  .class-grids {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(20rem, 1fr));
    gap: var(--gf-space-4);
  }
  .class-grid {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
    margin: 0;
    padding: var(--gf-space-4);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    background: var(--gf-surface);
  }
  .class-legend {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--gf-space-2);
    padding: 0;
  }
  .class-name {
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
  }
  .auto-tag {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-muted);
  }
  .member-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
  }
  .member-row {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    padding: var(--gf-space-2);
    border-radius: var(--gf-radius-sm);
  }
  .member-row:hover {
    background: var(--gf-elevated);
  }
  .member-label {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    flex: 1;
    min-width: 0;
    cursor: pointer;
  }
  .member-callsign {
    flex: 1;
    min-width: 0;
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-medium);
  }
  .member-chan {
    flex-shrink: 0;
    width: 12rem;
    max-width: 50%;
  }
  .class-foot {
    display: flex;
    justify-content: flex-end;
    padding-top: var(--gf-space-2);
    border-top: 1px solid var(--gf-border-subtle);
  }
</style>
