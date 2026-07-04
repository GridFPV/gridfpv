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
  import { Badge, Button, Card, Collapsible, Select, toast } from '@gridfpv/components';
  import type { Class, ClassId, MemberSlot, Pilot, PilotId, TimerId } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import { assignChannelsRoundRobin, channelLabel } from '../lib/channels.js';
  import { collapseStore } from '../lib/collapse.svelte.js';
  import { AutoSaver } from '../lib/autosave.js';
  import ClassManager from './ClassManager.svelte';
  import PilotManager from './PilotManager.svelte';

  let { session }: { session: Session } = $props();

  // ── Collapse persistence ────────────────────────────────────────────────────
  // The roster block and each class's placement block can be collapsed to tame the density of a
  // 20-pilots-across-classes event. The choice is persisted per stable section id, namespaced by the
  // event (gf.collapse.<event>.<section>), so it sticks across navigation/reload. The event id is
  // stable for the workspace; an absent event (the picker) is never on this screen.
  const eventId = $derived(session.currentEvent?.id ?? 'event');
  // One persisted store per section, created lazily and cached so the same section keeps one store
  // across re-renders. A finished/placed class can default collapsed; active sections default open.
  const collapseStores = new Map<string, ReturnType<typeof collapseStore>>();
  function collapse(sectionId: string, defaultOpen = true): ReturnType<typeof collapseStore> {
    const key = `${eventId}:${sectionId}`;
    let s = collapseStores.get(key);
    if (!s) {
      s = collapseStore(eventId, sectionId, defaultOpen);
      collapseStores.set(key, s);
    }
    return s;
  }

  // The two top-level boxes (Classes / Pilots) each persist their own collapse state.
  const classesBox = $derived(collapse('classes-box'));
  const pilotsBox = $derived(collapse('pilots-box'));

  let classManager = $state<ClassManager | undefined>(undefined);
  let pilotManager = $state<PilotManager | undefined>(undefined);

  // ── Auto-save (debounced, optimistic, wholesale set-the-full-selection) ─────
  // Each box persists on every change rather than behind a Save button. Every save here is a
  // *wholesale* set (`setEventClasses` / `setEventRoster` / `setClassMembership` send the entire
  // current selection, not a delta), so coalescing rapid clicks into one trailing save per target
  // is safe last-write-wins. The UI flips optimistically; a failed save reverts that target. The
  // targets are: 'classes', 'roster', and one per class id for membership.
  const autosaver = new AutoSaver();

  // ── 1. Class selection (top) ───────────────────────────────────────────────
  // The directory list (kept in sync by the manager via `bind:classes`) and the working selection
  // (a set of class ids), seeded from the event. Toggling a class edits the set optimistically and
  // auto-saves the full selection (debounced); a failed save reverts to the last-saved set.
  let classes = $state<Class[]>([]);
  let selectedClasses = $state<Set<ClassId>>(new Set());

  function syncClassesFromEvent() {
    selectedClasses = new Set(session.currentEvent?.classes ?? []);
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
  const orderedClassSelection = $derived(
    classes.filter((c) => selectedClasses.has(c.id)).map((c) => c.id)
  );
  function toggleClass(id: ClassId) {
    // Optimistic flip, then schedule a wholesale save of the full class selection.
    const next = new Set(selectedClasses);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    selectedClasses = next;
    autosaver.schedule('classes', {
      compute: () => orderedClassSelection,
      save: (ids) => session.setEventClasses(ids),
      onUnsaved: () => {
        toast.info('A control token is required to set the event’s classes.');
        syncClassesFromEvent();
      },
      onError: (e) => {
        syncClassesFromEvent();
        toast.error(e instanceof Error ? e.message : String(e));
      }
    });
  }

  // ── 2. Present pilots (the roster) ─────────────────────────────────────────
  let pilots = $state<Pilot[]>([]);
  let selectedPilots = $state<Set<PilotId>>(new Set());
  let savedRoster = $state<PilotId[]>([]);

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
  const orderedRoster = $derived(pilots.filter((p) => selectedPilots.has(p.id)).map((p) => p.id));
  // Schedule a debounced wholesale save of the present-pilots roster. Computed at flush time so
  // rapid checks (or a select-all then a tweak) coalesce into one trailing `setEventRoster`.
  function scheduleRosterSave() {
    autosaver.schedule('roster', {
      compute: () => orderedRoster,
      save: (ids) => session.setEventRoster(ids),
      onUnsaved: () => {
        toast.info('A control token is required to set the event’s roster.');
        syncRosterFromEvent();
      },
      onError: (e) => {
        syncRosterFromEvent();
        toast.error(e instanceof Error ? e.message : String(e));
      }
    });
  }
  function togglePilot(id: PilotId) {
    const next = new Set(selectedPilots);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    selectedPilots = next;
    scheduleRosterSave();
  }
  // Select-all / Unselect-all for the roster — one fast toggle for a 20-pilot field. Select-all marks
  // every directory pilot present; unselect-all clears the roster selection. Both edit the selection
  // optimistically and auto-save (debounced), like the per-row checkboxes.
  const allPilotsSelected = $derived(pilots.length > 0 && selectedPilots.size === pilots.length);
  function selectAllPilots() {
    selectedPilots = new Set(pilots.map((p) => p.id));
    scheduleRosterSave();
  }
  function unselectAllPilots() {
    selectedPilots = new Set();
    scheduleRosterSave();
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
  // When it changes the membership we auto-save it too (so placement persists with only the roster
  // ticked — the wizard's Next-only flow needs no Save click).
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
      // Only persist once the auto-filled membership actually differs from what's saved (avoids a
      // redundant save when re-seeding from an event that already carries this membership).
      if (membershipDiffers(cls)) scheduleMembershipSave(cls);
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
    scheduleMembershipSave(classId);
  }
  // Place-all / clear-all for one class — fast bulk placement across the present roster. Place-all
  // adds every roster pilot not already a member (channel unset, preserving any chosen channel);
  // clear-all empties the class. Each edits optimistically and auto-saves the class's membership.
  function placeAll(classId: ClassId) {
    const next = new Map(membership);
    const inner = new Map(next.get(classId) ?? []);
    for (const id of eventRoster) if (!inner.has(id)) inner.set(id, undefined);
    next.set(classId, inner);
    membership = next;
    scheduleMembershipSave(classId);
  }
  function clearAll(classId: ClassId) {
    const next = new Map(membership);
    next.set(classId, new Map());
    membership = next;
    scheduleMembershipSave(classId);
  }
  function allPlaced(classId: ClassId): boolean {
    const inner = membersOf(classId);
    return rosterPilots.length > 0 && rosterPilots.every((p) => inner.has(p.id));
  }
  // Set (or clear, on '') a member's channel. The `<select>` value is the raw MHz as a string.
  function setChannel(classId: ClassId, pilotId: PilotId, raw: string) {
    const next = new Map(membership);
    const inner = new Map(next.get(classId) ?? []);
    inner.set(pilotId, raw === '' ? undefined : Number(raw));
    next.set(classId, inner);
    membership = next;
    scheduleMembershipSave(classId);
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
  // Whether a class's working membership differs from the event's saved membership — guards the
  // auto-fill save (and skips a no-op `setClassMembership` round-trip).
  function membershipDiffers(classId: ClassId): boolean {
    const next = orderedSlots(classId);
    const prev = savedMembership.get(classId) ?? new Map<PilotId, number | undefined>();
    if (next.length !== prev.size) return true;
    return next.some((s) => !prev.has(s.pilot) || (prev.get(s.pilot) ?? undefined) !== s.channel);
  }

  // Schedule a debounced wholesale save of one class's membership (`setClassMembership`). Keyed by
  // class id so different classes save independently; the slots are computed at flush time so rapid
  // edits to the same class coalesce into one trailing save.
  function scheduleMembershipSave(classId: ClassId) {
    autosaver.schedule(`membership:${classId}`, {
      compute: () => orderedSlots(classId),
      save: (slots) => session.setClassMembership(classId, slots),
      onUnsaved: () => {
        toast.info('A control token is required to set class membership.');
        revertMembership();
      },
      onError: (e) => {
        revertMembership();
        toast.error(e instanceof Error ? e.message : String(e));
      }
    });
  }
  // Revert the working membership to the event's last-saved state by forcing the membershipKey
  // effect to re-seed (the saved membership is unchanged on a failed/declined save).
  function revertMembership() {
    lastMembershipKey = ' ';
  }

  // ── Auto-assign channels (per class) ───────────────────────────────────────
  // Deterministically fill one class's *placed* pilots' channels from the source timer's available
  // pool (round-robin / first-fit, in roster order): the i-th placed pilot gets pool[i % N], so the
  // class's pilots spread across the channels and repeats are fine (they fly in different heats). Then
  // save that class's membership via the existing setClassMembership. Manual per-pilot overrides
  // afterwards stay intact — this only fills the class in one batch.
  let autoAssigning = $state<ClassId | undefined>(undefined);
  function placedCount(classId: ClassId): number {
    return membersOf(classId).size;
  }
  function canAutoAssign(classId: ClassId): boolean {
    return hasChannelPool && placedCount(classId) > 0 && autoAssigning === undefined;
  }
  async function autoAssignChannels(classId: ClassId) {
    if (!canAutoAssign(classId)) return;
    autoAssigning = classId;
    try {
      const pool = channelPool;
      const inner = membersOf(classId);
      // Stable roster order — the order the slots save in, so the assignment is reproducible.
      const placed = eventRoster.filter((id) => inner.has(id));
      if (placed.length === 0) return;
      const assigned = assignChannelsRoundRobin(placed, pool);
      const filled = new Map(inner);
      for (const [id, mhz] of assigned) filled.set(id, mhz);
      const next = new Map(membership);
      next.set(classId, filled);
      membership = next;

      // Drop any pending debounced membership save for this class — auto-assign saves it itself,
      // immediately, with the freshly-assigned channels (so we don't double-save).
      autosaver.cancel(`membership:${classId}`);
      const saved = await session.setClassMembership(classId, orderedSlots(classId));
      if (!saved) {
        toast.info('A control token is required to assign channels.');
        return;
      }
      toast.success(`Auto-assigned channels for ${classNameOf(classId)}.`);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      autoAssigning = undefined;
    }
  }
</script>

<section class="classes-roster" aria-label="Classes and roster">
  <!-- ════ BOX 1 · Classes ════════════════════════════════════════════════ -->
  <Collapsible title="Classes" id="box-classes" bind:open={classesBox.open}>
    {#snippet summary()}
      <Badge tone="neutral">{orderedClassSelection.length} of {classes.length} selected</Badge>
    {/snippet}
    {#snippet actions()}
      <Button variant="secondary" size="sm" onclick={() => classManager?.openAdd()}>
        + Add class
      </Button>
    {/snippet}

    <Card
      subtitle="Add, edit, or remove classes in the directory, and tick which ones this event runs."
    >
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
        filterHidden
        keepRow={(c) => selectedClasses.has(c.id)}
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
            <span class="count" aria-live="polite">
              {orderedClassSelection.length} selected · saved automatically
            </span>
          </div>
        {/snippet}
      </ClassManager>
    </Card>
  </Collapsible>

  <!-- ════ BOX 2 · Pilots (roster + per-class placement) ══════════════════ -->
  <Collapsible title="Pilots" id="box-pilots" bind:open={pilotsBox.open}>
    {#snippet summary()}
      <Badge tone="neutral">{orderedRoster.length} of {pilots.length} present</Badge>
    {/snippet}
    {#snippet actions()}
      <Button variant="secondary" size="sm" onclick={() => pilotManager?.openAdd()}>
        + Add pilot
      </Button>
    {/snippet}

    <!-- Roster -->
    <Card
      title="Present pilots"
      subtitle="Who is here at the event. Tick a directory pilot to mark them present, or add a new one — sim players appear automatically as they join."
    >
      {#snippet actions()}
        <div class="bulk-actions">
          <Button
            variant="secondary"
            size="sm"
            onclick={selectAllPilots}
            disabled={pilots.length === 0 || allPilotsSelected}
          >
            Select all
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onclick={unselectAllPilots}
            disabled={selectedPilots.size === 0}
          >
            Unselect all
          </Button>
        </div>
      {/snippet}

      {@const rosterCollapse = collapse('roster')}
      <Collapsible title="Roster" bind:open={rosterCollapse.open}>
        {#snippet summary()}
          <Badge tone="neutral">
            {orderedRoster.length} of {pilots.length} present
          </Badge>
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
              <span class="count" aria-live="polite">
                {orderedRoster.length} present · saved automatically
              </span>
            </div>
          {/snippet}
        </PilotManager>
      </Collapsible>
    </Card>

    <!-- Per-class channels (placement + each placed pilot's fixed channel) -->
    <Card
      title="Channels"
      subtitle="Place present pilots into each class and give each one a fixed channel for time-trial / qualifying rounds. Auto-assign spreads the available channels across a class's placed pilots, or set any pilot's channel by hand."
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
          <p class="nudge-sub">Tick the classes it runs in the Classes box above first.</p>
        </div>
      {:else if rosterPilots.length === 0}
        <div class="nudge" role="status">
          <p>No pilots are present yet.</p>
          <p class="nudge-sub">Mark some pilots present above, then place them into classes.</p>
        </div>
      {:else if singleClass}
        {@const sc = collapse(`place:${singleClass}`)}
        <!-- Single-class auto-fill: every present pilot is a member; just assign channels. -->
        <Collapsible
          title={classNameOf(singleClass)}
          id={`place-${singleClass}`}
          bind:open={sc.open}
        >
          {#snippet summary()}
            <Badge tone="neutral">{rosterPilots.length} placed</Badge>
          {/snippet}
          <fieldset class="class-grid" aria-label={`Placement for ${classNameOf(singleClass)}`}>
            <div class="grid-bulk">
              <p class="auto-tag">all present pilots (single class)</p>
              <Button
                variant="secondary"
                size="sm"
                onclick={() => autoAssignChannels(singleClass)}
                loading={autoAssigning === singleClass}
                disabled={!canAutoAssign(singleClass)}
              >
                Auto-assign channels
              </Button>
            </div>
            {#if !hasChannelPool}
              <p class="auto-tag">
                No channels to assign yet — configure a timer's available channels on the Timers
                tab.
              </p>
            {/if}
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
          </fieldset>
        </Collapsible>
      {:else}
        <!-- ≥2 classes: a per-class placement grid (checkbox per roster pilot + a channel). -->
        <div class="class-grids">
          {#each eventClasses as classId (classId)}
            {@const cc = collapse(`place:${classId}`)}
            <Collapsible title={classNameOf(classId)} id={`place-${classId}`} bind:open={cc.open}>
              {#snippet summary()}
                <Badge tone="neutral">{membersOf(classId).size} placed</Badge>
              {/snippet}
              <fieldset class="class-grid" aria-label={`Placement for ${classNameOf(classId)}`}>
                <div class="grid-bulk">
                  <Button
                    variant="secondary"
                    size="sm"
                    onclick={() => placeAll(classId)}
                    disabled={allPlaced(classId)}
                  >
                    Place all
                  </Button>
                  <Button
                    variant="ghost"
                    size="sm"
                    onclick={() => clearAll(classId)}
                    disabled={membersOf(classId).size === 0}
                  >
                    Clear all
                  </Button>
                  <Button
                    variant="secondary"
                    size="sm"
                    onclick={() => autoAssignChannels(classId)}
                    loading={autoAssigning === classId}
                    disabled={!canAutoAssign(classId)}
                  >
                    Auto-assign channels
                  </Button>
                </div>
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
              </fieldset>
            </Collapsible>
          {/each}
        </div>
      {/if}
    </Card>
  </Collapsible>
</section>

<style>
  .classes-roster {
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

  /* Two Cards stacked inside the Pilots box (roster + placement) want breathing room. */
  .classes-roster :global(.gf-collapsible-body > .gf-card + .gf-card) {
    margin-top: var(--gf-space-4);
  }

  /* ── Bulk select / place actions ──────────────────────────────────────── */
  .bulk-actions,
  .grid-bulk {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
  }
  .grid-bulk {
    flex-wrap: wrap;
    padding-bottom: var(--gf-space-1);
  }
  /* The single-class auto-tag fills, pushing the auto-assign button to the right edge. */
  .grid-bulk .auto-tag {
    flex: 1;
  }

  /* ── Channel source picker (Card action) ──────────────────────────────── */
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
    padding: 0;
    border: none;
    min-inline-size: 0;
  }
  .auto-tag {
    margin: 0;
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
</style>
