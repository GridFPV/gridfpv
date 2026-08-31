<script lang="ts">
  /**
   * ContextHeader — the persistent event/race context bar (#85).
   *
   * A slim bar across **every in-event page** (live control, registration, marshaling,
   * results, setup) so the RD always sees *which event* they're in and *what's happening
   * on the timer*, regardless of which screen is open. It is rendered by the shell only
   * inside the event workspace; the event picker (no-event state) never shows it.
   *
   * Contents (maintainer-approved spec):
   *   • Event name — prominent; clicking it goes to **Live control** (not the picker).
   *   • Current heat + phase — the active heat id and a {@link StatusPill} phase pill
   *     (Slice 0 phase colors), plus the shared {@link RaceClock}: it ticks while the heat is
   *     Running and stays visible *frozen* at the race-end time through Unofficial/Final, so the
   *     header clock matches the live screen's heat clock rather than vanishing when the race ends.
   *   • Connection status — the existing connection pill.
   *   (The old "← Switch event" button is gone — the breadcrumb's Home/Events crumbs are
   *   the way back, and the header stays purely informational on the right.)
   * (No date/location hover — the maintainer explicitly dropped that.)
   *
   * The clock is the shared #62 logic via {@link useRaceClock}, so it ticks while Running,
   * freezes on Unofficial/Final, and resets when there's no live heat — everywhere, not just
   * on the live screen, which now drives its clock from the same source.
   */
  import { StatusPill, RaceClock, toast } from '@gridfpv/components';
  import type { HeatSummary } from '@gridfpv/types';
  import type { Session } from './lib/session.svelte.js';
  import { useRaceClock } from './lib/raceClock.svelte.js';
  import { heatNameById } from './lib/heats.js';
  import { fixedEndWindowMicros } from './lib/raceWindow.js';

  let {
    session,
    /** Go to the Live control screen (the event-name click target). */
    ongolive
    /** Return to the event picker (the only way out of the workspace). */
  }: {
    session: Session;
    ongolive: () => void;
  } = $props();

  const eventName = $derived(session.currentEvent?.name ?? '');
  const live = $derived(session.liveState);
  const heat = $derived(live?.current_heat);

  // The friendly current-heat name ("‹Round› Heat N") — the persistent header sits on every screen,
  // so the raw heat id leaked "everywhere" until resolved here. We join the live heat id → its
  // {@link HeatSummary} → the name the server resolved onto it (#456) via {@link heatNameById}, the
  // SAME helper the Live-control title and the Marshaling header use, so the name never drifts.
  // Falls back to the raw id only when the heat is genuinely unresolvable (a sim / free-text heat,
  // or before the reads land).
  //
  // The scheduled-heats read re-runs on `session.currentEvent` AND `session.protocolState` (the
  // #242 cold-load fix mirrored from Marshaling): `listHeats()` resolves `[]` until an event is in
  // hand, and the header mounts on a cold reload while the active event is still resolving. Keyed off
  // the stream alone, the read would never re-fire when `currentEvent` finally appeared — a quiet
  // heat emits no further tick — so the header would show the raw id until the next stream advance.
  // Touching `currentEvent` makes it re-read the instant the event settles. Rounds come straight off
  // the event, so the `$derived` name re-resolves the moment either heats or the event changes.
  let heats = $state<HeatSummary[]>([]);
  // A FAILED heats read must be visible (#340): swallowing it into an empty array made the header
  // silently render the raw heat id with no hint anything was wrong. Track a load-error flag
  // (keeping the last good data rather than wiping it), show a compact "Couldn't load — retry"
  // state in place of the heat name, and toast once on the transition into the error state.
  let heatsError = $state(false);
  let heatsRetryNonce = $state(0);
  function retryHeats(): void {
    heatsRetryNonce += 1;
  }
  $effect(() => {
    void session.currentEvent;
    void session.protocolState;
    void heatsRetryNonce;
    session
      .listHeats()
      .then((h) => ((heats = h), (heatsError = false)))
      .catch(() => {
        if (!heatsError)
          toast.error('Couldn’t load the heats directory — heat names may not resolve.');
        heatsError = true;
      });
  });
  const heatName = $derived(heat ? heatNameById(heat, heats) : '');
  // The active event's selected timers with their live (polled) connection status (#73, Slice 2b).
  // A compact pill per timer keeps "is the timer still connected?" answerable from any in-event page.
  const timers = $derived(session.selectedTimers);
  // The effective primary (issue #112) — marked subtly on its pill when 2+ timers feed the event,
  // so "which one is live?" is answerable at a glance. A single timer is trivially primary (no marker).
  const primaryId = $derived(session.primaryTimerId);
  const showRoles = $derived(timers.length >= 2);
  // Only show a phase once there's a live heat on the timer; otherwise the event is idle.
  const phase = $derived(heat ? (live?.phase ?? 'Scheduled') : undefined);

  // The shared race clock (#62): ticks while Running, freezes on Unofficial/Final, resets
  // otherwise. Server-time-authoritative (#62 follow-up) — it counts from the live state's
  // `race_started_at` / `race_ended_at` (the server's race-go / race-end instants), the SAME
  // anchor the live screen uses, so the header and the HUD read identically and accurately
  // regardless of when each mounted (no more per-client wall-clock drift).
  const clock = useRaceClock(
    () => phase,
    () => live?.race_started_at,
    () => live?.race_ended_at,
    () => session.serverNowMs()
  );
  // Show the heat clock while it's meaningful: ticking during the race (Running) and frozen at
  // the race-end value once the race has closed (Unofficial) and through finalize (Final). Before
  // the race starts (Scheduled/Staged/Armed) the clock reads 0, so we keep it hidden to avoid a
  // misleading "0:00.000" — it appears the moment the race goes live and persists, frozen, after.
  const showClock = $derived(phase === 'Running' || phase === 'Unofficial' || phase === 'Final');

  // A fixed-end heat (a Timed round's window; a Practice time limit) counts DOWN here too — the
  // SAME shared derivation the live HUD and the end tones use (`raceWindow.ts`), so every clock on
  // screen agrees. Past zero the readout runs negative through the grace window and the RaceClock
  // styles it red (warn-yellow in the closing seconds). No fixed end ⇒ the classic count-up.
  const currentRound = $derived.by(() => {
    const summary = heats.find((h) => h.heat === heat);
    if (!summary?.round) return undefined;
    return (session.currentEvent?.rounds ?? []).find((r) => r.id === summary.round);
  });
  const windowMicros = $derived(fixedEndWindowMicros(currentRound));
  const remainingMs = $derived(
    windowMicros !== undefined ? windowMicros / 1000 - clock.elapsedMs : undefined
  );
  // The GRACE countdown (#505) — the same derivation the HUD uses: once the server logs the
  // RaceExpired marker its deadline supersedes the (now-negative) window clock.
  const graceRemainingMs = $derived(
    live?.grace_deadline != null && live?.race_started_at != null
      ? (live.grace_deadline - live.race_started_at) / 1000 - clock.elapsedMs
      : undefined
  );
</script>

<div class="ctx-bar" aria-label="Event context">
  <div class="ctx-left">
    <button
      type="button"
      class="ctx-event"
      onclick={ongolive}
      title={`${eventName} — go to race control`}
    >
      <span class="ctx-event-name">{eventName}</span>
    </button>

    {#if heat}
      <span class="ctx-sep" aria-hidden="true"></span>
      <div class="ctx-heat">
        <span class="ctx-heat-label">Heat</span>
        {#if heatsError}
          <!-- The heats read failed (#340): show the error state (with retry) instead of silently
               falling back to the raw heat id. -->
          <button
            type="button"
            class="ctx-load-error"
            onclick={retryHeats}
            title="Couldn’t load the heats directory — heat names may not resolve. Click to retry."
            >Couldn’t load — retry</button
          >
        {:else}
          <span class="ctx-heat-id">{heatName}</span>
        {/if}
      </div>
      {#if phase}
        <span class="ctx-phase"><StatusPill {phase} size="sm" /></span>
      {/if}
      {#if showClock}
        <span class="ctx-clock">
          {#if graceRemainingMs !== undefined}
            <RaceClock remainingMs={graceRemainingMs} label="Grace remaining" />
          {:else if remainingMs !== undefined}
            <RaceClock {remainingMs} label="Time remaining" />
          {:else}
            <RaceClock elapsedMs={clock.elapsedMs} label="Heat time" />
          {/if}
        </span>
      {/if}
    {:else}
      <span class="ctx-sep" aria-hidden="true"></span>
      <span class="ctx-idle">No heat on the timer</span>
    {/if}
  </div>

  <div class="ctx-right">
    {#if timers.length}
      <div class="ctx-timers" aria-label="Timer status">
        {#each timers as timer (timer.id)}
          {@const isPrimary = showRoles && timer.id === primaryId}
          <span
            class="ctx-timer"
            title={`${timer.name}: ${timer.status}${
              showRoles ? (isPrimary ? ' (primary)' : ' (alternate)') : ''
            }`}
          >
            {#if showRoles}
              <span class="ctx-role" class:role-primary={isPrimary} aria-hidden="true">
                {isPrimary ? 'P' : 'A'}
              </span>
            {/if}
            <span class="ctx-timer-name">{timer.name}</span>
            <StatusPill status={timer.status} label={timer.status} size="sm" />
          </span>
        {/each}
      </div>
      <span class="ctx-sep" aria-hidden="true"></span>
    {/if}

    <div class="ctx-conn" title={`Read stream: ${session.connectionStatus}`}>
      <StatusPill status={session.connectionStatus} size="sm" />
      <!-- Text hook for the e2e (`.conn-label` === status text); visually folded into the pill. -->
      <span class="conn-label">{session.connectionStatus}</span>
    </div>
  </div>
</div>

<style>
  .ctx-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-4);
    min-width: 0;
  }
  .ctx-left,
  .ctx-right {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    min-width: 0;
  }

  /* ── Event name → live control ─────────────────────────────────────────────── */
  .ctx-event {
    display: inline-flex;
    align-items: center;
    max-width: 18rem;
    padding: var(--gf-space-1) var(--gf-space-2);
    margin-left: calc(-1 * var(--gf-space-2));
    border: 1px solid transparent;
    border-radius: var(--gf-radius-sm);
    background: transparent;
    color: var(--gf-text);
    font-family: inherit;
    cursor: pointer;
    transition:
      border-color var(--gf-motion-fast) var(--gf-ease-out),
      background var(--gf-motion-fast) var(--gf-ease-out),
      color var(--gf-motion-fast) var(--gf-ease-out);
  }
  .ctx-event:hover {
    background: var(--gf-elevated);
    border-color: var(--gf-border);
  }
  .ctx-event-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-bold);
    letter-spacing: var(--gf-tracking-tight);
  }

  .ctx-sep {
    width: 1px;
    height: 1.4rem;
    background: var(--gf-border);
    flex-shrink: 0;
  }

  /* ── Heat + phase + clock ──────────────────────────────────────────────────── */
  .ctx-heat {
    display: inline-flex;
    align-items: baseline;
    gap: var(--gf-space-2);
    min-width: 0;
  }
  .ctx-heat-label {
    font-size: var(--gf-font-size-2xs);
    color: var(--gf-text-muted);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    font-weight: var(--gf-font-weight-semibold);
  }
  .ctx-heat-id {
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-bold);
    letter-spacing: var(--gf-tracking-tight);
    font-variant-numeric: tabular-nums;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ctx-idle {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-faint);
  }
  /* The heats-directory load-error state (#340): compact, in place of the heat name. */
  .ctx-load-error {
    background: var(--gf-danger-soft);
    border: 1px solid color-mix(in srgb, var(--gf-danger) 45%, var(--gf-border));
    border-radius: var(--gf-radius-sm);
    padding: var(--gf-space-1) var(--gf-space-2);
    color: var(--gf-text);
    font-family: inherit;
    font-size: var(--gf-font-size-xs);
    cursor: pointer;
    white-space: nowrap;
  }
  .ctx-phase {
    display: inline-flex;
  }
  .ctx-clock {
    display: inline-flex;
    align-items: center;
    /* The shared RaceClock is sized for a big HUD; scale it down for the slim bar. */
    font-size: 0;
  }
  .ctx-clock :global(.gridfpv-race-clock) {
    font-size: var(--gf-font-size-lg);
  }

  /* ── Timer connection status (#73, Slice 2b) ───────────────────────────────── */
  .ctx-timers {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-3);
    min-width: 0;
    overflow: hidden;
  }
  .ctx-timer {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    min-width: 0;
  }
  .ctx-timer-name {
    /* The bound timer's name is config data — bumped one step (2xs → xs) for sunlight readability. */
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-muted);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    font-weight: var(--gf-font-weight-semibold);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 8rem;
  }
  /* Subtle primary/alternate marker (#112): a tiny "P"/"A" chip on each pill when 2+ timers feed. */
  .ctx-role {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 1rem;
    height: 1rem;
    flex-shrink: 0;
    font-size: var(--gf-font-size-2xs);
    font-weight: var(--gf-font-weight-semibold);
    border-radius: var(--gf-radius-pill);
    color: var(--gf-text-muted);
    background: var(--gf-surface-sunken);
  }
  .ctx-role.role-primary {
    color: var(--gf-success);
    background: var(--gf-success-soft);
  }

  /* ── Connection + switch ───────────────────────────────────────────────────── */
  .ctx-conn {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
  }
  .conn-label {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }
  @media (max-width: 60rem) {
    .ctx-heat-label,
    .ctx-timer-name {
      display: none;
    }
  }
</style>
