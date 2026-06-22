<script lang="ts">
  /**
   * EventRoster — the in-event **Roster stage** (race redesign Slice 1b).
   *
   * The Roster stage is the single screen where the RD settles *who races this event, and in which
   * class*, then binds the timing source to those pilots. It has three layers, from coarse to fine:
   *
   *  1. **Present pilots** — the event's `roster`: the pilots actually present at the event. The RD
   *     adds directory pilots (IRL), via the embedded {@link PilotManager} + the existing
   *     `setEventRoster`; a brand-new pilot can be registered without leaving the event. For a **sim**
   *     event the auto-presence reconciler adds the Velocidrone players it sees to the roster and
   *     binds them, so present pilots appear **live** off the active-event snapshot/stream
   *     (`session.currentEvent.roster` updates) with no RD action — we re-seed the working set from
   *     the event whenever it changes (without clobbering an in-progress edit).
   *
   *  2. **Per-class membership** — for each of the event's selected `classes` (resolved to names via
   *     the class directory), a grid placing roster pilots into that class (a checkbox per roster
   *     pilot). Saving a class pushes `setClassMembership(class, pilotIds)`, reflecting
   *     `EventMeta.classes_membership`. A pilot may be a member of several classes. With no classes
   *     selected yet, the section nudges to the Classes tab first.
   *
   *  3. **Binding** — each present pilot's timing-source binding status. The live race-state stream
   *     carries the *current heat's* registrations (`PilotProgress.pilot`), so a pilot bound there
   *     (or auto-bound by the sim) shows **bound** to its competitor ref; otherwise **unbound**. A
   *     small manual form binds an unbound `(adapter, competitor) → pilot` via `Command::Register`
   *     (the IRL path — the sim auto-binds). There is no event-wide registrations projection in the
   *     live snapshot, so this surfaces what the stream knows and lets the RD bind by hand — kept
   *     usable, deliberately not over-built.
   *
   * Field-readable (large text, dark) and consistent with the other stage screens.
   */
  import { Badge, Button, Card, Field, Input, toast } from '@gridfpv/components';
  import type { AdapterId, Class, ClassId, CompetitorRef, Pilot, PilotId } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import { SIM_ADAPTER } from '../lib/heat.js';
  import PilotManager from './PilotManager.svelte';

  let { session }: { session: Session } = $props();

  let manager = $state<PilotManager | undefined>(undefined);

  // ── Present pilots (the event roster) ──────────────────────────────────────
  // The directory list (kept in sync by the manager via `bind:pilots`) and the working selection
  // (a set of pilot ids), seeded from the event and edited locally until the RD saves. We snapshot
  // the event's saved roster so "Save"/"changed" can compare.
  let pilots = $state<Pilot[]>([]);
  let selected = $state<Set<PilotId>>(new Set());
  let savedRoster = $state<PilotId[]>([]);
  let saving = $state(false);

  // The saved roster as the event reports it — the `$effect` below re-seeds from this. Tracked as a
  // string key so an unchanged event snapshot (same ids, same order) doesn't re-fire the seed.
  const eventRoster = $derived(session.currentEvent?.roster ?? []);
  const eventRosterKey = $derived(eventRoster.join(','));

  function syncFromEvent() {
    savedRoster = [...eventRoster];
    selected = new Set(eventRoster);
  }

  // Re-seed the working set whenever the event's saved roster changes — on mount, and **live** when
  // the sim auto-presence reconciler adds players to the roster off the active-event stream (so
  // sim-present pilots appear without RD action). We only re-seed when the *saved* roster differs
  // from our last snapshot, so a live update never clobbers an unsaved tick the RD is mid-edit on a
  // roster that itself hasn't changed.
  $effect(() => {
    // Reading `eventRosterKey` registers this effect's dependency, so it re-runs when the event
    // roster changes (incl. live, when the sim reconciler grows the roster off the stream).
    if (eventRosterKey !== savedRoster.join(',')) syncFromEvent();
  });

  /**
   * Reconcile the working set against the fresh directory after a create/edit/delete: drop any
   * selected ids that no longer exist (e.g. a removed pilot). Newly added pilots just become
   * available to tick — we don't auto-select them.
   */
  function onDirectoryChange(list: Pilot[]) {
    const present = new Set(list.map((p) => p.id));
    const next = new Set<PilotId>();
    for (const id of selected) if (present.has(id)) next.add(id);
    selected = next;
  }

  function toggle(id: PilotId) {
    const next = new Set(selected);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    selected = next;
  }

  // The ids to save, in the directory's listed order (a stable, sensible order).
  const orderedSelection = $derived(pilots.filter((p) => selected.has(p.id)).map((p) => p.id));

  const changed = $derived(
    orderedSelection.length !== savedRoster.length ||
      orderedSelection.some((id, i) => id !== savedRoster[i])
  );

  async function save() {
    if (saving || !changed) return;
    saving = true;
    try {
      const updated = await session.setEventRoster(orderedSelection);
      if (!updated) {
        toast.info('A control token is required to set the event’s roster.');
        return;
      }
      syncFromEvent();
      toast.success('Present pilots saved.');
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      saving = false;
    }
  }

  function reset() {
    syncFromEvent();
  }

  // ── Class directory (to resolve the event's selected class ids → names) ─────
  let classDir = $state<Class[]>([]);
  $effect(() => {
    void (async () => {
      try {
        classDir = await session.listClasses();
      } catch {
        /* a class-name lookup failure just falls back to the raw id; the section still works */
      }
    })();
  });
  const classNameOf = $derived((id: ClassId) => classDir.find((c) => c.id === id)?.name ?? id);

  // The event's selected classes, in order. Membership placement only makes sense for a present
  // (rostered) pilot, so each class grid lists the roster pilots.
  const eventClasses = $derived(session.currentEvent?.classes ?? []);
  // The roster pilots, resolved to their directory `Pilot` (in roster order), for the grids + binding
  // — a pilot still appearing in the roster but not (yet) in the loaded directory is skipped.
  const rosterPilots = $derived(
    eventRoster
      .map((id) => pilots.find((p) => p.id === id))
      .filter((p): p is Pilot => p !== undefined)
  );

  // The saved per-class membership, as a map `classId → Set<pilotId>`, off the event.
  const savedMembership = $derived.by(() => {
    const map = new Map<ClassId, Set<PilotId>>();
    for (const m of session.currentEvent?.classes_membership ?? []) {
      // Membership entries are member **slots** (`{ pilot, channel? }`) — Slice 7a; key on the
      // pilot id (the channel picker is the Classes & Roster follow-up).
      map.set(m.class, new Set(m.pilots.map((s) => s.pilot)));
    }
    return map;
  });

  // The working per-class membership the RD edits before saving each class. Seeded from the event,
  // re-seeded whenever the saved membership changes (a save re-homes `currentEvent`).
  let membership = $state<Map<ClassId, Set<PilotId>>>(new Map());
  const membershipKey = $derived(
    (session.currentEvent?.classes_membership ?? [])
      .map((m) => `${m.class}:${m.pilots.map((s) => s.pilot).join('.')}`)
      .join('|')
  );
  let lastMembershipKey = $state('');
  $effect(() => {
    if (membershipKey !== lastMembershipKey) {
      const next = new Map<ClassId, Set<PilotId>>();
      for (const [cls, ids] of savedMembership) next.set(cls, new Set(ids));
      membership = next;
      lastMembershipKey = membershipKey;
    }
  });

  function working(classId: ClassId): Set<PilotId> {
    return membership.get(classId) ?? new Set();
  }

  function toggleMember(classId: ClassId, pilotId: PilotId) {
    const next = new Map(membership);
    const set = new Set(next.get(classId) ?? []);
    if (set.has(pilotId)) set.delete(pilotId);
    else set.add(pilotId);
    next.set(classId, set);
    membership = next;
  }

  // The ids to save for a class, in roster order (a stable order), plus whether it differs from saved.
  function orderedMembers(classId: ClassId): PilotId[] {
    const set = working(classId);
    return eventRoster.filter((id) => set.has(id));
  }
  function membershipChanged(classId: ClassId): boolean {
    const next = orderedMembers(classId);
    const prev = [...(savedMembership.get(classId) ?? [])];
    return next.length !== prev.length || next.some((id) => !savedMembership.get(classId)?.has(id));
  }

  let savingClass = $state<ClassId | undefined>(undefined);
  async function saveMembership(classId: ClassId) {
    if (savingClass) return;
    savingClass = classId;
    try {
      const updated = await session.setClassMembership(classId, orderedMembers(classId));
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

  // ── Binding (timing source → pilot) ────────────────────────────────────────
  // The live race-state stream carries the *current heat's* registrations (`PilotProgress.pilot`).
  // We fold those into a `pilotId → competitorRef` map so a present pilot bound there (or auto-bound
  // by the sim) shows as bound; everyone else is "unbound" until the RD binds them. There is no
  // event-wide registrations projection in the snapshot, so this is best-effort over what the stream
  // knows — the manual bind form below covers the rest.
  const boundRefByPilot = $derived.by(() => {
    const map = new Map<PilotId, CompetitorRef>();
    for (const pr of session.liveState?.progress ?? []) {
      if (pr.pilot) map.set(pr.pilot, pr.competitor);
    }
    return map;
  });

  // The manual bind form: adapter (defaults to the sim adapter), a source-local competitor ref, and
  // which present pilot to bind it to.
  // `PilotId`/`AdapterId`/`CompetitorRef` are all `string`; an empty string is the "nothing chosen"
  // sentinel for the pilot picker (a real pilot id is never empty).
  let bindAdapter = $state<AdapterId>(SIM_ADAPTER);
  let bindCompetitor = $state<CompetitorRef>('');
  let bindPilot = $state<PilotId>('');
  let binding = $state(false);

  const canBind = $derived(
    bindAdapter.trim().length > 0 && bindCompetitor.trim().length > 0 && bindPilot !== ''
  );

  async function bind(e?: Event) {
    e?.preventDefault();
    if (binding || !canBind || bindPilot === '') return;
    binding = true;
    try {
      const ack = await session.registerCompetitor(
        bindAdapter.trim(),
        bindCompetitor.trim(),
        bindPilot
      );
      if (!ack.ok) {
        toast.error(ack.error?.message ?? 'Bind was rejected.');
        return;
      }
      toast.success('Competitor bound to pilot.');
      bindCompetitor = '';
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      binding = false;
    }
  }

  function callsignOf(id: PilotId): string {
    return pilots.find((p) => p.id === id)?.callsign ?? id;
  }
</script>

<section class="roster-stage" aria-label="Event roster">
  <!-- ── 1. Present pilots ────────────────────────────────────────────────── -->
  <Card
    title="Present pilots"
    subtitle="Who is here at the event. Tick a directory pilot to mark them present, or add a new one — sim players appear automatically as they join."
  >
    {#snippet actions()}
      <Button variant="secondary" size="sm" onclick={() => manager?.openAdd()}>+ Add pilot</Button>
    {/snippet}

    <!-- The roster count always shows (even on an empty directory, where it reads "0 of 0" and the
         manager nudges to add pilots) — so it lives here, above the manager, not in `listHeader`
         (which only renders when the directory has pilots). -->
    <p class="roster-count" aria-live="polite">
      <strong>{orderedSelection.length}</strong> of {pilots.length}
      {pilots.length === 1 ? 'pilot' : 'pilots'} present at this event
    </p>

    <PilotManager
      bind:this={manager}
      {session}
      bind:pilots
      onchange={onDirectoryChange}
      rowChecked={(p) => selected.has(p.id)}
    >
      {#snippet rowLead(pilot)}
        <input
          type="checkbox"
          class="select-box"
          checked={selected.has(pilot.id)}
          onchange={() => toggle(pilot.id)}
          aria-label={`Roster ${pilot.callsign}`}
        />
      {/snippet}

      {#snippet listFooter()}
        <div class="foot">
          <span class="count" aria-live="polite">
            {orderedSelection.length} present
          </span>
          <div class="foot-actions">
            {#if changed}
              <Button variant="ghost" onclick={reset} disabled={saving}>Reset</Button>
            {/if}
            <Button variant="primary" onclick={save} loading={saving} disabled={!changed}>
              Save roster
            </Button>
          </div>
        </div>
      {/snippet}
    </PilotManager>
  </Card>

  <!-- ── 2. Per-class membership ──────────────────────────────────────────── -->
  <Card
    title="Class membership"
    subtitle="Place present pilots into each class this event runs. A pilot can race more than one class."
  >
    {#if eventClasses.length === 0}
      <div class="nudge" role="status">
        <p>This event has no classes selected yet.</p>
        <p class="nudge-sub">Pick the classes it runs on the <strong>Classes</strong> tab first.</p>
      </div>
    {:else if rosterPilots.length === 0}
      <div class="nudge" role="status">
        <p>No pilots are present yet.</p>
        <p class="nudge-sub">Mark some pilots present above, then place them into classes.</p>
      </div>
    {:else}
      <div class="class-grids">
        {#each eventClasses as classId (classId)}
          {@const members = working(classId)}
          {@const dirty = membershipChanged(classId)}
          <fieldset class="class-grid" aria-label={`Membership for ${classNameOf(classId)}`}>
            <legend class="class-legend">
              <span class="class-name">{classNameOf(classId)}</span>
              <Badge tone="neutral">{members.size} {members.size === 1 ? 'pilot' : 'pilots'}</Badge>
            </legend>
            <ul class="member-list">
              {#each rosterPilots as pilot (pilot.id)}
                <li class="member-row">
                  <label class="member-label">
                    <input
                      type="checkbox"
                      class="select-box"
                      checked={members.has(pilot.id)}
                      onchange={() => toggleMember(classId, pilot.id)}
                      aria-label={`Place ${pilot.callsign} in ${classNameOf(classId)}`}
                    />
                    <span class="member-callsign">{pilot.callsign}</span>
                  </label>
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
                Save membership
              </Button>
            </div>
          </fieldset>
        {/each}
      </div>
    {/if}
  </Card>

  <!-- ── 3. Binding (timing source → pilot) ───────────────────────────────── -->
  <Card
    title="Timing binding"
    subtitle="Bind a timing-source competitor to a pilot so their laps are attributed. Sim players bind automatically; bind RotorHazard competitors here."
  >
    {#if rosterPilots.length === 0}
      <div class="nudge" role="status">
        <p>No present pilots to bind yet.</p>
      </div>
    {:else}
      <ul class="bind-list" aria-label="Pilot bindings">
        {#each rosterPilots as pilot (pilot.id)}
          {@const ref = boundRefByPilot.get(pilot.id)}
          <li class="bind-row">
            <span class="bind-callsign">{pilot.callsign}</span>
            {#if ref}
              <Badge tone="success">bound → {ref}</Badge>
            {:else}
              <Badge tone="warn">unbound</Badge>
            {/if}
          </li>
        {/each}
      </ul>

      <form class="bind-form" onsubmit={bind} aria-label="Bind a competitor to a pilot">
        <div class="bind-grid">
          <Field label="Adapter" hint="The timing source (e.g. a RotorHazard id, or “sim”).">
            <Input bind:value={bindAdapter} aria-label="Bind adapter" autocomplete="off" />
          </Field>
          <Field label="Competitor" hint="The source-local handle (node seat / player name).">
            <Input
              bind:value={bindCompetitor}
              placeholder="e.g. node-1"
              aria-label="Bind competitor"
              autocomplete="off"
            />
          </Field>
          <Field label="Pilot">
            <select class="bind-select" bind:value={bindPilot} aria-label="Bind pilot">
              <option value="" disabled>Choose a pilot…</option>
              {#each rosterPilots as pilot (pilot.id)}
                <option value={pilot.id}>{pilot.callsign}</option>
              {/each}
            </select>
          </Field>
        </div>
        <div class="bind-actions">
          <Button variant="primary" onclick={bind} loading={binding} disabled={!canBind}>
            Bind
          </Button>
          {#if bindPilot !== ''}
            <span class="bind-preview">
              {bindAdapter || '—'} / {bindCompetitor || '…'} → {callsignOf(bindPilot)}
            </span>
          {/if}
        </div>
      </form>
    {/if}
  </Card>
</section>

<style>
  .roster-stage {
    max-width: 48rem;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
  }
  .roster-count {
    margin: 0 0 var(--gf-space-1);
    font-size: var(--gf-font-size-md);
    color: var(--gf-text-secondary);
  }
  .roster-count strong {
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

  /* ── Per-class membership grids ───────────────────────────────────────── */
  .class-grids {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(16rem, 1fr));
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
    gap: var(--gf-space-2);
    padding: 0;
  }
  .class-name {
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
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
  }
  .member-label {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    width: 100%;
    padding: var(--gf-space-2) var(--gf-space-2);
    border-radius: var(--gf-radius-sm);
    cursor: pointer;
    transition: background var(--gf-motion-fast) var(--gf-ease-out);
  }
  .member-label:hover {
    background: var(--gf-elevated);
  }
  .member-callsign {
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-medium);
  }
  .class-foot {
    display: flex;
    justify-content: flex-end;
    padding-top: var(--gf-space-2);
    border-top: 1px solid var(--gf-border-subtle);
  }

  /* ── Binding ──────────────────────────────────────────────────────────── */
  .bind-list {
    list-style: none;
    margin: 0 0 var(--gf-space-4);
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .bind-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-3);
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    background: var(--gf-surface);
  }
  .bind-callsign {
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
  }
  .bind-form {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
    padding-top: var(--gf-space-4);
    border-top: 1px solid var(--gf-border-subtle);
  }
  .bind-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(11rem, 1fr));
    gap: var(--gf-space-3);
  }
  .bind-select {
    width: 100%;
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    color: var(--gf-text);
    font: inherit;
    font-size: var(--gf-font-size-md);
  }
  .bind-actions {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
  }
  .bind-preview {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
    font-variant-numeric: tabular-nums;
  }
</style>
