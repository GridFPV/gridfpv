<script lang="ts">
  /**
   * RaceGowPage — the RaceGOW landing (`#/racegow`): start a new track run, or reopen one.
   *
   * An app-level page like Pilots / Timers, reached from the hub. It does not need an event; it
   * *creates* one. The form asks only what the preset cannot decide for itself — which track,
   * who is flying, on which timer and channel — and {@link createRaceGowRun} builds the rest
   * through the ordinary event commands (see `lib/racegow.ts`). Every run it made before is listed
   * below the form so a pilot coming back for another pack reopens the same track.
   *
   * Field-readable like the hub: big controls, one column, nothing hidden behind a wizard.
   */
  import { Button, Card, Field, Input, Select, toast } from '@gridfpv/components';
  import type { ChannelCatalogEntry, EventMeta, Pilot, Timer } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import Brand from '../Brand.svelte';
  import Breadcrumbs from '../Breadcrumbs.svelte';
  import { channelOptionLabel } from '../lib/channels.js';
  import { isTimerConnected, kindLabel, kindTag } from '../lib/timers.js';
  import { createRaceGowRun, DEFAULT_TRACK_NAME, isRaceGowEvent } from '../lib/racegow.js';

  let {
    session,
    onhome,
    onrun,
    ontimers
  }: {
    session: Session;
    onhome: () => void;
    /** Called once a run is created (or reopened) and entered — the shell shows the run screen. */
    onrun: () => void;
    /** The Timers page — where a timer's channels are configured when it offers none. */
    ontimers: () => void;
  } = $props();

  // ── The directory reads the form offers (open, no token) ────────────────────────────────────
  let pilots = $state<Pilot[]>([]);
  let timers = $state<Timer[]>([]);
  let catalog = $state<ChannelCatalogEntry[]>([]);
  let runs = $state<EventMeta[] | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);

  async function load(): Promise<void> {
    try {
      const [p, t, c, e] = await Promise.all([
        session.listPilots(),
        session.listTimers(),
        session.listChannels().catch(() => [] as ChannelCatalogEntry[]),
        session.listEvents()
      ]);
      pilots = p;
      timers = t;
      catalog = c;
      runs = e.filter(isRaceGowEvent).sort((a, b) => b.created_at - a.created_at);
      loadError = undefined;
      seedDefaults();
    } catch (e) {
      loadError = e instanceof Error ? e.message : String(e);
    }
  }
  $effect(() => {
    void load();
  });

  // ── The form ────────────────────────────────────────────────────────────────────────────────
  let track = $state(DEFAULT_TRACK_NAME);
  let pilotId = $state('');
  /** `'__new'` in the pilot picker switches to the inline callsign field. */
  const NEW_PILOT = '__new';
  let newCallsign = $state('');
  let timerId = $state('');
  let channel = $state('');
  let creating = $state(false);
  let formError = $state<string | undefined>(undefined);

  /**
   * Sensible defaults once the directories land: the only pilot (or the new-pilot field when
   * there is none), and the first **connected RotorHazard** — else the first connected timer,
   * else the first timer at all. A run wants real hardware, but a Simulator is offered too: it
   * is how the whole path is rehearsed with no gate on the table.
   */
  function seedDefaults(): void {
    if (pilotId === '')
      pilotId = pilots.length === 1 ? pilots[0].id : pilots.length === 0 ? NEW_PILOT : '';
    if (timerId === '') {
      const rh = timers.find((t) => kindTag(t.kind) === 'Rotorhazard' && isTimerConnected(t));
      const any = timers.find(isTimerConnected);
      timerId = (rh ?? any ?? timers[0])?.id ?? '';
    }
  }

  const timer = $derived(timers.find((t) => t.id === timerId));
  /** The chosen timer's configured channels — the pool the pilot's channel comes from. */
  const channels = $derived(timer?.available_channels ?? []);
  // Re-seed the channel whenever the timer changes: the first of its pool, or nothing.
  $effect(() => {
    const pool = channels;
    if (!pool.includes(Number(channel))) channel = pool.length ? String(pool[0]) : '';
  });
  const timerHasNoChannels = $derived(timer !== undefined && channels.length === 0);

  const canSubmit = $derived(
    track.trim().length > 0 &&
      (pilotId === NEW_PILOT ? newCallsign.trim().length > 0 : pilotId !== '') &&
      timerId !== '' &&
      !timerHasNoChannels &&
      !creating
  );

  function timerOptionLabel(t: Timer): string {
    const live = isTimerConnected(t) ? 'connected' : String(t.status).toLowerCase();
    return `${t.name} — ${kindLabel(t.kind)}, ${live}`;
  }

  async function submit(e?: Event): Promise<void> {
    e?.preventDefault();
    if (!canSubmit) return;
    creating = true;
    formError = undefined;
    try {
      let pilot = pilotId;
      if (pilot === NEW_PILOT) {
        const made = await session.createPilot({ callsign: newCallsign.trim(), vtx_types: [] });
        if (!made) {
          formError = 'A control token is required to add a pilot.';
          return;
        }
        pilot = made.id;
      }
      const meta = await createRaceGowRun(session, {
        track,
        pilot,
        timer: timerId,
        channel: channel === '' ? undefined : Number(channel)
      });
      if (!meta) {
        formError = 'A control token is required to start a run.';
        return;
      }
      toast.success(`Ready to fly “${meta.name}”.`);
      onrun();
    } catch (err) {
      formError = err instanceof Error ? err.message : String(err);
    } finally {
      creating = false;
    }
  }

  // ── Reopening an earlier run ────────────────────────────────────────────────────────────────
  let opening = $state<string | undefined>(undefined);
  async function reopen(meta: EventMeta): Promise<void> {
    opening = meta.id;
    try {
      const entered = await session.chooseEvent(meta);
      if (!entered) {
        toast.info('A control token is required to open a run.');
        return;
      }
      onrun();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : String(err));
    } finally {
      opening = undefined;
    }
  }

  function whenLabel(meta: EventMeta): string {
    return new Date(meta.created_at).toLocaleDateString(undefined, {
      month: 'short',
      day: 'numeric'
    });
  }
</script>

<div class="page">
  <div class="page-inner">
    <div class="brand-row"><Brand onclick={onhome} /></div>
    <Breadcrumbs crumbs={[{ label: 'Home', onclick: onhome }, { label: 'RaceGOW' }]} />

    <header class="page-head">
      <div class="page-titles">
        <h1 class="page-title">RaceGOW</h1>
        <p class="page-sub">
          Time your track runs at home. Fly as many packs as you like — GridFPV keeps every lap and
          finds your fastest <strong>3 laps in a row</strong>, the time RaceGOW asks for.
        </p>
      </div>
    </header>

    {#if isRaceGowEvent(session.currentEvent)}
      <!-- A run is already open on this console: the obvious next step is to go back to it. -->
      <Card elevation="md">
        <div class="resume">
          <div class="resume-text">
            <span class="resume-kicker">Open now</span>
            <span class="resume-name">{session.currentEvent.name}</span>
          </div>
          <Button variant="primary" onclick={onrun}>Back to the run</Button>
        </div>
      </Card>
    {/if}

    <Card title="New track run" elevation="md">
      <form class="run-form" onsubmit={submit} aria-label="New track run">
        <Field
          label="Track"
          required
          hint="Name it the way RaceGOW does, so the result is easy to file."
        >
          <Input bind:value={track} aria-label="Track" autocomplete="off" />
        </Field>

        <Field label="Pilot" required>
          <Select bind:value={pilotId} aria-label="Pilot">
            <option value="" disabled>Choose a pilot…</option>
            {#each pilots as p (p.id)}
              <option value={p.id}>{p.callsign}</option>
            {/each}
            <option value={NEW_PILOT}>+ New pilot…</option>
          </Select>
        </Field>
        {#if pilotId === NEW_PILOT}
          <Field label="Callsign" required hint="Added to your pilots list; you only type it once.">
            <Input bind:value={newCallsign} aria-label="Callsign" autocomplete="off" />
          </Field>
        {/if}

        <Field
          label="Timer"
          required
          error={timerHasNoChannels ? 'This timer has no channels set yet.' : undefined}
          hint={timers.length === 0
            ? 'No timers yet — add your RotorHazard on the Timers page first.'
            : 'Your RotorHazard gate. A Simulator flies a pretend run, handy for checking the setup.'}
        >
          <Select bind:value={timerId} aria-label="Timer">
            <option value="" disabled>Choose a timer…</option>
            {#each timers as t (t.id)}
              <option value={t.id}>{timerOptionLabel(t)}</option>
            {/each}
          </Select>
        </Field>
        {#if timerHasNoChannels}
          <p class="fix-hint">
            Pick the channels it may use on the
            <button type="button" class="link" onclick={ontimers}>Timers page</button>, then come
            back.
          </p>
        {/if}

        {#if channels.length > 0}
          <Field label="Your channel" required hint="The channel your whoop's video is on.">
            <Select bind:value={channel} aria-label="Your channel">
              {#each channels as mhz (mhz)}
                <option value={String(mhz)}>{channelOptionLabel(mhz, catalog)}</option>
              {/each}
            </Select>
          </Field>
        {/if}

        {#if formError}
          <p class="form-error" role="alert">{formError}</p>
        {/if}
        {#if loadError}
          <p class="form-error" role="alert">Couldn’t load your pilots and timers: {loadError}</p>
        {/if}

        <div class="form-actions">
          <Button variant="primary" type="submit" disabled={!canSubmit}>
            {creating ? 'Setting up…' : 'Set up run'}
          </Button>
        </div>
      </form>
    </Card>

    <Card title="Your runs" elevation="sm">
      {#if runs === undefined}
        <p class="muted">Loading…</p>
      {:else if runs.length === 0}
        <p class="muted">No runs yet. Your first one shows up here.</p>
      {:else}
        <ul class="runs" aria-label="Your runs">
          {#each runs as r (r.id)}
            <li class="run-row">
              <div class="run-text">
                <span class="run-name">{r.name}</span>
                <span class="run-when">{whenLabel(r)}</span>
              </div>
              <Button
                variant="secondary"
                size="sm"
                onclick={() => reopen(r)}
                disabled={opening !== undefined}
                aria-label={`Open ${r.name}`}
              >
                {opening === r.id ? 'Opening…' : 'Open'}
              </Button>
            </li>
          {/each}
        </ul>
        <p class="muted small">
          A run is an ordinary event: rename or delete it on the Events page.
        </p>
      {/if}
    </Card>
  </div>
</div>

<style>
  .brand-row {
    margin-bottom: var(--gf-space-4);
  }
  .page {
    min-height: 100vh;
    padding: var(--gf-space-6) var(--gf-space-8) var(--gf-space-8);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    overflow: auto;
  }
  .page-inner {
    width: min(44rem, 100%);
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
  }
  .page-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--gf-space-4);
  }
  .page-titles {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
    min-width: 0;
  }
  .page-title {
    margin: 0;
    font-size: var(--gf-font-size-2xl);
    letter-spacing: var(--gf-tracking-tight);
  }
  .page-sub {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-md);
    line-height: var(--gf-line-normal);
  }

  .resume {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-4);
  }
  .resume-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .resume-kicker {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-accent);
  }
  .resume-name {
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-semibold);
  }

  .run-form {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .form-actions {
    display: flex;
    justify-content: flex-end;
  }
  .form-error {
    margin: 0;
    color: var(--gf-danger);
    font-size: var(--gf-font-size-sm);
  }
  .fix-hint {
    margin: calc(-1 * var(--gf-space-2)) 0 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .link {
    padding: 0;
    border: none;
    background: none;
    color: var(--gf-accent);
    font: inherit;
    cursor: pointer;
    text-decoration: underline;
  }

  .runs {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }
  .run-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-4);
    padding: var(--gf-space-3) 0;
    border-bottom: 1px solid var(--gf-border-subtle);
  }
  .run-row:last-child {
    border-bottom: none;
  }
  .run-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .run-name {
    font-weight: var(--gf-font-weight-semibold);
  }
  .run-when {
    color: var(--gf-text-faint);
    font-size: var(--gf-font-size-xs);
  }
  .muted {
    margin: 0;
    color: var(--gf-text-muted);
  }
  .small {
    margin-top: var(--gf-space-3);
    font-size: var(--gf-font-size-xs);
  }
</style>
