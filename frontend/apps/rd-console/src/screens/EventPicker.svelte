<script lang="ts">
  /**
   * EventPicker — the home hub's **Events** page (#118; was the landing screen, #72 Slice 1b).
   *
   * In the two-level IA the home hub is the landing and this is the **Events page** (Home ›
   * Events) — the select/create surface. On load it reads the open event list (`listEvents()`,
   * no token) and renders **Practice** prominently as the no-friction "just try it" entry, then
   * the list of created (persistent) events. Selecting one enters its workspace. "+ New event"
   * opens a name `Dialog` → `createEvent(name)` (which obtains the RD token lazily) → enters it.
   *
   * The old picker-header "Timers" modal entry is **gone** — app-level timer management is now its
   * own page in the hub ({@link TimersPage}). Breadcrumbs (Home › Events) + a Home button get you
   * back to the hub from here.
   *
   * Auth is lazy here: listing/browsing needs no token; only **creating** an event prompts
   * for the RD token (handled by the session's token provider). The Director address is the
   * page's own origin — there is no address to type.
   */
  import { Badge, Button, Card, Dialog, Field, Input, toast } from '@gridfpv/components';
  import type { EventMeta } from '@gridfpv/types';
  import type { Session, CreateEventFields } from '../lib/session.svelte.js';
  import { PRACTICE_EVENT_ID } from '../lib/session.svelte.js';
  import Breadcrumbs from '../Breadcrumbs.svelte';

  let {
    session,
    onhome,
    onsetup = undefined
  }: {
    session: Session;
    onhome: () => void;
    /** Called after a brand-new event is created + entered, when the RD opted to set it up (the
     *  guided wizard, race redesign Slice 7). The shell opens the wizard once the workspace mounts. */
    onsetup?: () => void;
  } = $props();

  type LoadState =
    | { kind: 'loading' }
    | { kind: 'error'; message: string }
    | { kind: 'ready'; events: EventMeta[] };

  let loadState = $state<LoadState>({ kind: 'loading' });

  /**
   * The id of the Director's **active event** (#91), if any — the event currently live. Read on
   * load (open, no token) so the picker can mark it with an "Active" pill, even when this console
   * is browsing the picker after a "Switch event" (which leaves the server's active event set).
   * `undefined` when nothing is active or the read fails: no pill, never blocks the list.
   */
  let activeEventId = $state<string | undefined>(undefined);

  // The **hard delete** confirmation dialog (the papercut fix). Deleting an event is permanent
  // and unrecoverable — it wipes the event and ALL its data — so the RD must re-type the event's
  // exact name before the red "Delete permanently" button enables. This is a deliberate friction
  // gate, not a one-click confirm.
  let deleteTarget = $state<EventMeta | undefined>(undefined);
  let deleteConfirmName = $state('');
  let deleting = $state(false);
  let deleteError = $state<string | undefined>(undefined);
  // The typed name must match the target's name exactly (trimmed) before delete is allowed.
  const deleteNameMatches = $derived(
    !!deleteTarget && deleteConfirmName.trim() === deleteTarget.name.trim()
  );

  function openDelete(ev: EventMeta) {
    deleteTarget = ev;
    deleteConfirmName = '';
    deleteError = undefined;
  }

  function closeDelete() {
    deleteTarget = undefined;
    deleteConfirmName = '';
    deleteError = undefined;
  }

  async function confirmDelete() {
    const target = deleteTarget;
    if (!target || !deleteNameMatches || deleting) return;
    deleting = true;
    deleteError = undefined;
    try {
      const ok = await session.deleteEvent(target.id);
      if (!ok) {
        // A gated Director's token prompt was cancelled — keep the dialog open, hint why.
        deleteError = 'A control token is required to delete an event.';
        return;
      }
      closeDelete();
      toast.success(`Deleted “${target.name}” and all of its data.`);
      // If the deleted event was the active one, clear the local pill, then refresh the list.
      if (activeEventId === target.id) activeEventId = undefined;
      await load();
    } catch (e) {
      deleteError = e instanceof Error ? e.message : String(e);
    } finally {
      deleting = false;
    }
  }

  // The "new event" dialog. Name-only is the one-click default (#72, Slice 1b C); the
  // optional descriptive fields live behind a collapsible "Add details" section.
  let newOpen = $state(false);
  let newName = $state('');
  let newDate = $state('');
  let newLocation = $state('');
  let newDescription = $state('');
  let newOrganizer = $state('');
  let detailsOpen = $state(false);
  let creating = $state(false);
  let newError = $state<string | undefined>(undefined);
  // Offer the guided setup wizard for a brand-new event (race redesign Slice 7), default on — a new
  // event has nothing configured, so walking the stages is the natural next step. Editable later.
  let setUpAfter = $state(true);

  async function load() {
    loadState = { kind: 'loading' };
    // Read the active event id alongside the list (#91): it's an independent, best-effort read
    // (no token; resolves undefined on failure), so a slow/failed active-event read never holds
    // up — or breaks — the list. The pill simply appears once both have settled.
    void session.getActiveEventId().then((id) => (activeEventId = id));
    try {
      const events = await session.listEvents();
      loadState = { kind: 'ready', events };
    } catch (e) {
      loadState = { kind: 'error', message: e instanceof Error ? e.message : String(e) };
    }
  }

  // Kick off the open list read once on mount.
  $effect(() => {
    void load();
  });

  /** The built-in Practice event, pulled out of the list (server lists it first). */
  const practice = $derived(
    loadState.kind === 'ready'
      ? loadState.events.find((e) => e.id === PRACTICE_EVENT_ID)
      : undefined
  );
  /** Every other (created) event, Practice removed. */
  const others = $derived(
    loadState.kind === 'ready' ? loadState.events.filter((e) => e.id !== PRACTICE_EVENT_ID) : []
  );

  function formatDate(ms: number): string {
    try {
      return new Date(ms).toLocaleDateString(undefined, {
        year: 'numeric',
        month: 'short',
        day: 'numeric'
      });
    } catch {
      return '';
    }
  }

  /**
   * The subtle row subtitle: show the RD-supplied **date** and **location** where present
   * (#72, Slice 1b C), falling back to the created date. The full context header is #85.
   */
  function rowSubtitle(ev: EventMeta): string {
    const parts: string[] = [];
    if (ev.date?.trim()) parts.push(ev.date.trim());
    if (ev.location?.trim()) parts.push(ev.location.trim());
    if (parts.length > 0) return parts.join(' · ');
    return `Created ${formatDate(ev.created_at)}`;
  }

  async function enter(meta: EventMeta) {
    // Persist the choice as the Director's active event (#90) so a reload/another client resumes
    // into it, then enter. A gated Director may prompt for the RD token (handled in the session);
    // a cancelled prompt or a transport error surfaces as a toast and stays on the picker.
    try {
      const chosen = await session.chooseEvent(meta);
      if (!chosen) toast.info('A control token is required to switch the active event.');
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    }
  }

  function openNew() {
    newName = '';
    newDate = '';
    newLocation = '';
    newDescription = '';
    newOrganizer = '';
    detailsOpen = false;
    newError = undefined;
    setUpAfter = true;
    newOpen = true;
  }

  /** Collect the optional detail fields, omitting blanks (server normalizes too). */
  function detailFields(): CreateEventFields | undefined {
    const fields: CreateEventFields = {};
    if (newDate.trim()) fields.date = newDate.trim();
    if (newLocation.trim()) fields.location = newLocation.trim();
    if (newDescription.trim()) fields.description = newDescription.trim();
    if (newOrganizer.trim()) fields.organizer = newOrganizer.trim();
    return Object.keys(fields).length > 0 ? fields : undefined;
  }

  async function submitNew(e: Event) {
    e.preventDefault();
    const name = newName.trim();
    if (!name || creating) return;
    creating = true;
    newError = undefined;
    try {
      const meta = await session.createEventAndEnter(name, detailFields());
      if (!meta) {
        // The RD cancelled the lazy token prompt — keep the dialog open, hint why.
        newError = 'A control token is required to create an event.';
        return;
      }
      newOpen = false;
      toast.success(`Created “${meta.name}”.`);
      // Opt-in: kick off the guided setup wizard for the freshly-created event. The shell defers
      // opening it until the workspace mounts (the embedded stages need the active event).
      if (setUpAfter) onsetup?.();
    } catch (err) {
      newError = err instanceof Error ? err.message : String(err);
    } finally {
      creating = false;
    }
  }
</script>

<div class="picker">
  <div class="picker-inner">
    <Breadcrumbs crumbs={[{ label: 'Home', onclick: onhome }, { label: 'Events' }]} />

    <header class="head">
      <h1 class="title">Choose an event</h1>
      <div class="head-actions">
        <Button variant="primary" onclick={openNew}>+ New event</Button>
      </div>
    </header>

    {#if loadState.kind === 'loading'}
      <div class="state-msg" role="status">
        <span class="spinner" aria-hidden="true"></span>
        Loading events…
      </div>
    {:else if loadState.kind === 'error'}
      <Card elevation="sm">
        <div class="state-error">
          <p>Couldn't reach the Director.</p>
          <code>{loadState.message}</code>
          <Button variant="secondary" onclick={load}>Try again</Button>
        </div>
      </Card>
    {:else}
      <section class="created" aria-label="Events">
        <h2 class="section-title">Your events</h2>
        {#if others.length === 0}
          <div class="empty">
            <p>No events yet.</p>
            <p class="empty-sub">Create one to keep heats, registration, and results.</p>
            <Button variant="secondary" onclick={openNew}>+ New event</Button>
          </div>
        {:else}
          <ul class="event-list">
            {#each others as ev (ev.id)}
              <li class="event-item">
                <button type="button" class="event-row" onclick={() => enter(ev)}>
                  <span class="event-icon" aria-hidden="true">●</span>
                  <span class="event-main">
                    <span class="event-name">{ev.name}</span>
                    <span class="event-sub">{rowSubtitle(ev)}</span>
                  </span>
                  {#if ev.id === activeEventId}
                    <span class="active-pill"><Badge tone="success" dot>Active</Badge></span>
                  {/if}
                  <span class="event-go" aria-hidden="true">→</span>
                </button>
                <button
                  type="button"
                  class="event-delete"
                  title={`Delete “${ev.name}” permanently`}
                  aria-label={`Delete ${ev.name}`}
                  onclick={() => openDelete(ev)}
                >
                  Delete
                </button>
              </li>
            {/each}
          </ul>
        {/if}
      </section>

      {#if practice}
        <section class="practice-wrap" aria-label="Practice">
          <h2 class="section-title">Practice</h2>
          <button
            type="button"
            class="event-row practice"
            onclick={() => enter(practice)}
            title="No setup — just fly. Nothing here is saved."
          >
            <span class="event-icon practice-icon" aria-hidden="true">▶</span>
            <span class="event-main">
              <span class="event-name">{practice.name}</span>
            </span>
            {#if practice.id === activeEventId}
              <span class="active-pill"><Badge tone="success" dot>Active</Badge></span>
            {/if}
            <span class="event-go" aria-hidden="true">→</span>
          </button>
        </section>
      {/if}
    {/if}
  </div>
</div>

<Dialog bind:open={newOpen} title="New event" onclose={() => (newError = undefined)}>
  <form class="new-form" onsubmit={submitNew} aria-label="New event">
    <Field label="Event name" error={newError}>
      <Input
        bind:value={newName}
        placeholder="e.g. Friday Night Series"
        aria-label="Event name"
        autocomplete="off"
      />
    </Field>
    <p class="new-hint">
      A persistent event keeps its heats, registration, and results. The id is generated for you.
    </p>

    <!-- Optional details behind a collapsible section — name-only stays one-click (#72 C). -->
    <details class="details" bind:open={detailsOpen}>
      <summary>Add details (optional)</summary>
      <div class="details-grid">
        <Field label="Date">
          <Input type="date" bind:value={newDate} aria-label="Event date" />
        </Field>
        <Field label="Location">
          <Input
            bind:value={newLocation}
            placeholder="e.g. Main field"
            aria-label="Event location"
            autocomplete="off"
          />
        </Field>
        <Field label="Organizer">
          <Input
            bind:value={newOrganizer}
            placeholder="e.g. City FPV Club"
            aria-label="Event organizer"
            autocomplete="off"
          />
        </Field>
        <Field label="Description">
          <Input
            bind:value={newDescription}
            placeholder="A short note about the event"
            aria-label="Event description"
            autocomplete="off"
          />
        </Field>
      </div>
    </details>

    <!-- Opt into the guided setup wizard after creating (race redesign Slice 7) — a first-pass over
         the event's stages (classes, roster, timer, first round). Everything stays editable later. -->
    <label class="setup-opt">
      <input type="checkbox" bind:checked={setUpAfter} aria-label="Set up event after creating" />
      <span class="setup-opt-text">
        <span class="setup-opt-title">Set up event</span>
        <span class="setup-opt-sub"
          >Walk classes, roster, timer, and a first round after creating.</span
        >
      </span>
    </label>
  </form>
  {#snippet footer()}
    <Button variant="ghost" onclick={() => (newOpen = false)} disabled={creating}>Cancel</Button>
    <Button variant="primary" onclick={submitNew} disabled={!newName.trim() || creating}>
      {creating ? 'Creating…' : 'Create & enter'}
    </Button>
  {/snippet}
</Dialog>

<!-- The HARD delete confirmation (the papercut fix). An unmissable, irreversible warning: the RD
     must re-type the event's exact name before the red "Delete permanently" button enables. -->
<Dialog open={deleteTarget !== undefined} title="Delete event permanently?" onclose={closeDelete}>
  {#if deleteTarget}
    <div class="delete-body" aria-label="Delete event confirmation">
      <p class="delete-warning">
        This <strong>permanently deletes</strong> the event
        <strong>“{deleteTarget.name}”</strong> and <strong>all of its data</strong> — every heat,
        registration, lap, result, and round. This <strong>cannot be undone</strong> and the data is
        <strong>unrecoverable</strong>.
      </p>
      <Field label={`Type the event name to confirm: ${deleteTarget.name}`} error={deleteError}>
        <Input
          bind:value={deleteConfirmName}
          placeholder={deleteTarget.name}
          aria-label="Confirm event name"
          autocomplete="off"
        />
      </Field>
    </div>
  {/if}
  {#snippet footer()}
    <Button variant="ghost" onclick={closeDelete} disabled={deleting}>Cancel</Button>
    <Button variant="danger" onclick={confirmDelete} disabled={!deleteNameMatches || deleting}>
      {deleting ? 'Deleting…' : 'Delete permanently'}
    </Button>
  {/snippet}
</Dialog>

<style>
  .picker {
    display: grid;
    place-items: start center;
    min-height: 100vh;
    padding: var(--gf-space-8) var(--gf-space-6);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    overflow: auto;
  }
  .picker-inner {
    width: min(44rem, 100%);
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
    padding-top: var(--gf-space-6);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-4);
  }
  .head-actions {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
  }
  .title {
    margin: 0;
    font-size: var(--gf-font-size-2xl);
    letter-spacing: var(--gf-tracking-tight);
  }

  .state-msg {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    padding: var(--gf-space-6) 0;
  }
  .spinner {
    width: 1.1rem;
    height: 1.1rem;
    border-radius: 50%;
    border: 2px solid var(--gf-border);
    border-top-color: var(--gf-accent);
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .spinner {
      animation-duration: 2s;
    }
  }
  .state-error {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
    align-items: flex-start;
  }
  .state-error p {
    margin: 0;
    font-weight: var(--gf-font-weight-semibold);
  }
  .state-error code {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-danger);
    word-break: break-all;
  }

  /* ── Event rows ──────────────────────────────────────────────────────────── */
  /* A row + its delete affordance sit side by side; the row flexes, delete is fixed. */
  .event-item {
    display: flex;
    align-items: stretch;
    gap: var(--gf-space-2);
  }
  .event-item .event-row {
    flex: 1;
    min-width: 0;
  }
  .event-delete {
    flex-shrink: 0;
    padding: 0 var(--gf-space-4);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    background: var(--gf-surface);
    color: var(--gf-text-muted);
    font-family: inherit;
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-medium);
    cursor: pointer;
    transition:
      border-color var(--gf-motion-fast) var(--gf-ease-out),
      background var(--gf-motion-fast) var(--gf-ease-out),
      color var(--gf-motion-fast) var(--gf-ease-out);
  }
  .event-delete:hover {
    border-color: var(--gf-danger);
    color: var(--gf-danger);
    background: color-mix(in srgb, var(--gf-danger) 8%, var(--gf-surface));
  }
  .event-delete:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }

  /* The hard-delete dialog body: an unmissable warning + the type-to-confirm field. */
  .delete-body {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .delete-warning {
    margin: 0;
    font-size: var(--gf-font-size-sm);
    line-height: var(--gf-line-height-relaxed, 1.5);
    color: var(--gf-text);
    padding: var(--gf-space-3) var(--gf-space-4);
    border: 1px solid color-mix(in srgb, var(--gf-danger) 40%, var(--gf-border));
    border-radius: var(--gf-radius-md);
    background: color-mix(in srgb, var(--gf-danger) 8%, var(--gf-surface));
  }

  .event-row {
    display: flex;
    align-items: center;
    gap: var(--gf-space-4);
    width: 100%;
    text-align: left;
    padding: var(--gf-space-4) var(--gf-space-5);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    background: var(--gf-surface);
    color: inherit;
    font-family: inherit;
    cursor: pointer;
    transition:
      border-color var(--gf-motion-fast) var(--gf-ease-out),
      background var(--gf-motion-fast) var(--gf-ease-out),
      transform var(--gf-motion-fast) var(--gf-ease-out);
  }
  .event-row:hover {
    border-color: var(--gf-accent);
    background: var(--gf-elevated);
  }
  .event-row:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
  /* Practice hero — a warm red-orange wash (#91), not the brand green: green reads as
     "live / brand", so the warm accent signals a friendly, non-persistent scratch space. */
  .event-row.practice {
    background: linear-gradient(
      180deg,
      var(--gf-accent-practice-soft),
      color-mix(in srgb, var(--gf-surface) 88%, transparent)
    );
    border-color: color-mix(in srgb, var(--gf-accent-practice) 45%, var(--gf-border));
    padding: var(--gf-space-5);
  }
  .event-row.practice:hover {
    border-color: var(--gf-accent-practice);
  }
  .event-row.practice:hover .event-go {
    color: var(--gf-accent-practice);
  }
  .event-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 2.25rem;
    height: 2.25rem;
    flex-shrink: 0;
    border-radius: var(--gf-radius-md);
    background: var(--gf-surface-sunken);
    color: var(--gf-text-faint);
    font-size: var(--gf-font-size-xs);
  }
  .event-icon.practice-icon {
    background: var(--gf-accent-practice-soft);
    color: var(--gf-accent-practice);
    font-size: var(--gf-font-size-md);
  }
  /* The "Active" pill (#91): sits between the name and the arrow, never shrinking. */
  .active-pill {
    flex-shrink: 0;
  }
  .event-main {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1;
    min-width: 0;
  }
  .event-name {
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .event-sub {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-muted);
  }
  .event-go {
    color: var(--gf-text-faint);
    font-size: var(--gf-font-size-lg);
    flex-shrink: 0;
    transition: transform var(--gf-motion-fast) var(--gf-ease-out);
  }
  .event-row:hover .event-go {
    color: var(--gf-accent);
    transform: translateX(2px);
  }

  .created {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .section-title {
    margin: var(--gf-space-2) 0 0;
    font-size: var(--gf-font-size-xs);
    font-weight: var(--gf-font-weight-semibold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .event-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .empty {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
    align-items: flex-start;
    padding: var(--gf-space-6);
    border: 1px dashed var(--gf-border);
    border-radius: var(--gf-radius-lg);
    background: var(--gf-surface-alt);
  }
  .empty p {
    margin: 0;
    font-weight: var(--gf-font-weight-medium);
  }
  .empty-sub {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-regular) !important;
  }

  .new-form {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .new-hint {
    margin: 0;
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-muted);
  }
  .details {
    border-top: 1px solid var(--gf-border-subtle);
    padding-top: var(--gf-space-3);
  }
  .details > summary {
    cursor: pointer;
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-medium);
    color: var(--gf-text-secondary);
    list-style: revert;
    user-select: none;
  }
  .details > summary:hover {
    color: var(--gf-text);
  }
  .details-grid {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
    margin-top: var(--gf-space-3);
  }
  .setup-opt {
    display: flex;
    align-items: flex-start;
    gap: var(--gf-space-3);
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-md);
    background: var(--gf-surface-alt);
    cursor: pointer;
  }
  .setup-opt input {
    width: 1.1rem;
    height: 1.1rem;
    margin-top: 0.15rem;
    accent-color: var(--gf-accent);
    flex-shrink: 0;
    cursor: pointer;
  }
  .setup-opt-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .setup-opt-title {
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
    color: var(--gf-text);
  }
  .setup-opt-sub {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-muted);
  }
</style>
