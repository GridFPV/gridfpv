<script lang="ts">
  /**
   * EventPicker — the RD console's landing screen (#72, Slice 1b).
   *
   * The event is the outer container: you can't act outside an event, so the picker *is*
   * the home screen. On load it reads the open event list (`listEvents()`, no token) and
   * renders **Practice** prominently as the no-friction "just try it" entry, then the list
   * of created (persistent) events. Selecting one enters its workspace. "+ New event" opens
   * a name `Dialog` → `createEvent(name)` (which obtains the RD token lazily) → enters it.
   *
   * Auth is lazy here: listing/browsing needs no token; only **creating** an event prompts
   * for the RD token (handled by the session's token provider). The Director address is the
   * page's own origin — there is no address to type.
   */
  import { Button, Card, Badge, Dialog, Field, Input, toast } from '@gridfpv/components';
  import type { EventMeta } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import { PRACTICE_EVENT_ID } from '../lib/session.svelte.js';

  let { session }: { session: Session } = $props();

  type LoadState =
    | { kind: 'loading' }
    | { kind: 'error'; message: string }
    | { kind: 'ready'; events: EventMeta[] };

  let loadState = $state<LoadState>({ kind: 'loading' });

  // The "new event" dialog.
  let newOpen = $state(false);
  let newName = $state('');
  let creating = $state(false);
  let newError = $state<string | undefined>(undefined);

  async function load() {
    loadState = { kind: 'loading' };
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

  function enter(meta: EventMeta) {
    session.selectEvent(meta);
  }

  function openNew() {
    newName = '';
    newError = undefined;
    newOpen = true;
  }

  async function submitNew(e: Event) {
    e.preventDefault();
    const name = newName.trim();
    if (!name || creating) return;
    creating = true;
    newError = undefined;
    try {
      const meta = await session.createEventAndEnter(name);
      if (!meta) {
        // The RD cancelled the lazy token prompt — keep the dialog open, hint why.
        newError = 'A control token is required to create an event.';
        return;
      }
      newOpen = false;
      toast.success(`Created “${meta.name}”.`);
    } catch (err) {
      newError = err instanceof Error ? err.message : String(err);
    } finally {
      creating = false;
    }
  }
</script>

<div class="picker">
  <div class="picker-inner">
    <header class="head">
      <div class="brand">
        <span class="logo" aria-hidden="true">
          <svg viewBox="0 0 32 32" width="40" height="40">
            <rect x="2" y="2" width="28" height="28" rx="8" fill="var(--gf-accent-soft)" />
            <path
              d="M16 6 L25 11 L25 21 L16 26 L7 21 L7 11 Z"
              fill="none"
              stroke="var(--gf-accent)"
              stroke-width="2"
              stroke-linejoin="round"
            />
            <circle cx="16" cy="16" r="3" fill="var(--gf-accent)" />
          </svg>
        </span>
        <div class="brand-text">
          <span class="name">GridFPV</span>
          <span class="kicker">Race Director Console</span>
        </div>
      </div>
      <Button variant="primary" onclick={openNew}>+ New event</Button>
    </header>

    <h1 class="title">Choose an event</h1>
    <p class="lede">
      An event is the container for everything you do. Pick <strong>Practice</strong> to jump straight
      in, open one you've created, or start a new event.
    </p>

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
      {#if practice}
        <section class="practice-wrap" aria-label="Practice">
          <button type="button" class="event-row practice" onclick={() => enter(practice)}>
            <span class="event-icon practice-icon" aria-hidden="true">▶</span>
            <span class="event-main">
              <span class="event-name">{practice.name}</span>
              <span class="event-sub">No setup — just fly. Nothing here is saved.</span>
            </span>
            <Badge tone="info" variant="soft">Practice</Badge>
            <span class="event-go" aria-hidden="true">→</span>
          </button>
        </section>
      {/if}

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
              <li>
                <button type="button" class="event-row" onclick={() => enter(ev)}>
                  <span class="event-icon" aria-hidden="true">●</span>
                  <span class="event-main">
                    <span class="event-name">{ev.name}</span>
                    <span class="event-sub">Created {formatDate(ev.created_at)}</span>
                  </span>
                  <Badge tone={ev.persistent ? 'accent' : 'neutral'} variant="soft">
                    {ev.persistent ? 'Persistent' : 'Ephemeral'}
                  </Badge>
                  <span class="event-go" aria-hidden="true">→</span>
                </button>
              </li>
            {/each}
          </ul>
        {/if}
      </section>
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
  </form>
  {#snippet footer()}
    <Button variant="ghost" onclick={() => (newOpen = false)} disabled={creating}>Cancel</Button>
    <Button variant="primary" onclick={submitNew} disabled={!newName.trim() || creating}>
      {creating ? 'Creating…' : 'Create & enter'}
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
    padding-top: var(--gf-space-8);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-4);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
  }
  .brand-text {
    display: flex;
    flex-direction: column;
    line-height: 1.15;
  }
  .brand-text .name {
    font-weight: var(--gf-font-weight-bold);
    font-size: var(--gf-font-size-lg);
    letter-spacing: var(--gf-tracking-tight);
  }
  .brand-text .kicker {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-muted);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
  }
  .title {
    margin: var(--gf-space-4) 0 0;
    font-size: var(--gf-font-size-2xl);
    letter-spacing: var(--gf-tracking-tight);
  }
  .lede {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    max-width: 40ch;
  }
  .lede strong {
    color: var(--gf-text);
    font-weight: var(--gf-font-weight-semibold);
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
  .event-row.practice {
    background: linear-gradient(
      180deg,
      var(--gf-accent-soft),
      color-mix(in srgb, var(--gf-surface) 88%, transparent)
    );
    border-color: color-mix(in srgb, var(--gf-accent) 45%, var(--gf-border));
    padding: var(--gf-space-5);
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
    background: var(--gf-accent-soft);
    color: var(--gf-accent);
    font-size: var(--gf-font-size-md);
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
</style>
