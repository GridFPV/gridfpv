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
   *     (Slice 0 phase colors), plus a live {@link RaceClock} while the heat is Running.
   *   • Connection status — the existing connection pill.
   *   • "← Switch event" — the only way back to the picker.
   * (No date/location hover — the maintainer explicitly dropped that.)
   *
   * The clock is the shared #62 logic via {@link useRaceClock}, so it ticks while Running,
   * freezes on Unofficial/Final, and resets when there's no live heat — everywhere, not just
   * on the live screen, which now drives its clock from the same source.
   */
  import { StatusPill, RaceClock } from '@gridfpv/components';
  import type { Session } from './lib/session.svelte.js';
  import { useRaceClock } from './lib/raceClock.svelte.js';

  let {
    session,
    /** Go to the Live control screen (the event-name click target). */
    ongolive,
    /** Return to the event picker (the only way out of the workspace). */
    onswitchevent
  }: {
    session: Session;
    ongolive: () => void;
    onswitchevent: () => void;
  } = $props();

  const eventName = $derived(session.currentEvent?.name ?? '');
  const live = $derived(session.liveState);
  const heat = $derived(live?.current_heat);
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
  // otherwise. Reads the live phase reactively so it tracks the heat from one place.
  const clock = useRaceClock(() => phase);
  const running = $derived(phase === 'Running');
</script>

<div class="ctx-bar" aria-label="Event context">
  <div class="ctx-left">
    <button
      type="button"
      class="ctx-event"
      onclick={ongolive}
      title={`${eventName} — go to live control`}
    >
      <span class="ctx-event-name">{eventName}</span>
    </button>

    {#if heat}
      <span class="ctx-sep" aria-hidden="true"></span>
      <div class="ctx-heat">
        <span class="ctx-heat-label">Heat</span>
        <span class="ctx-heat-id">{heat}</span>
      </div>
      {#if phase}
        <span class="ctx-phase"><StatusPill {phase} size="sm" /></span>
      {/if}
      {#if running}
        <span class="ctx-clock"><RaceClock elapsedMs={clock.elapsedMs} label="Heat time" /></span>
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
    <button type="button" class="ctx-switch" onclick={onswitchevent}>← Switch event</button>
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
    font-size: var(--gf-font-size-2xs);
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
  .ctx-switch {
    flex-shrink: 0;
    background: transparent;
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    padding: var(--gf-space-1) var(--gf-space-3);
    color: var(--gf-text-secondary);
    font-family: inherit;
    font-size: var(--gf-font-size-xs);
    cursor: pointer;
    white-space: nowrap;
    transition:
      border-color var(--gf-motion-fast) var(--gf-ease-out),
      color var(--gf-motion-fast) var(--gf-ease-out);
  }
  .ctx-switch:hover {
    border-color: var(--gf-border-strong);
    color: var(--gf-text);
  }

  @media (max-width: 60rem) {
    .ctx-heat-label,
    .ctx-timer-name {
      display: none;
    }
  }
</style>
