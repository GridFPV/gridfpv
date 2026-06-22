<script lang="ts">
  /**
   * Live race control (#54) — the heart of the console.
   *
   * Shows the current heat (`HeatSheet` + `RaceClock` + live `Leaderboard` derived from
   * the `LiveRaceState` stream) and drives the heat loop: one button per legal
   * transition, each calling `session.send` with the matching `Command`. Illegal
   * actions for the current `phase` are disabled up front (see `transitions.ts`);
   * destructive off-ramps (Abort/Restart/Discard) confirm before firing. A failed
   * `CommandAck` surfaces through the shared `ErrorBanner`.
   */
  import { HeatSheet, RaceClock, Leaderboard, Card } from '@gridfpv/components';
  import type { HeatId, HeatResult, LiveRaceState } from '@gridfpv/types';
  import {
    ACTION_ORDER,
    actionDescription,
    commandForAction,
    isActionLegal,
    isDestructive,
    primaryAction,
    type HeatAction
  } from '../lib/transitions.js';
  import type { Session } from '../lib/session.svelte.js';
  import { useRaceClock } from '../lib/raceClock.svelte.js';
  import ConfirmButton from '../lib/ConfirmButton.svelte';
  import ErrorBanner from '../lib/ErrorBanner.svelte';

  let { session, names = {} }: { session: Session; names?: Record<string, string> } = $props();

  const live = $derived<LiveRaceState | undefined>(session.liveState);
  const phase = $derived(live?.phase ?? 'Scheduled');
  const heat = $derived<HeatId | undefined>(live?.current_heat);
  const primary = $derived(primaryAction(phase));

  // ── Race clock (#62) ────────────────────────────────────────────────────────────────
  // The phase-driven elapsed clock now lives in the shared `useRaceClock` helper so the
  // persistent ContextHeader (#85) and this screen drive the *same* clock from one place
  // (ticks while Running, freezes on Finished/Scored, resets otherwise). See raceClock.svelte.ts.
  const clock = useRaceClock(() => phase);
  const elapsedMs = $derived(clock.elapsedMs);

  // A live, provisional leaderboard from the running order + per-pilot progress, so the
  // RD sees standings before the heat is scored. Built into a `HeatResult` so we reuse
  // the shared `Leaderboard` component (laps + last-lap-time as the metric).
  const liveResult = $derived<HeatResult | undefined>(buildLiveResult(live));

  function buildLiveResult(state: LiveRaceState | undefined): HeatResult | undefined {
    if (!state || !state.progress || state.progress.length === 0) return undefined;
    const order = state.running_order ?? state.active_pilots ?? [];
    const byRef = new Map(state.progress.map((p) => [p.competitor, p]));
    const ranked = (order.length > 0 ? order : state.progress.map((p) => p.competitor)).filter(
      (ref) => byRef.has(ref)
    );
    return {
      places: ranked.map((ref, i) => {
        const p = byRef.get(ref)!;
        return {
          competitor: { adapter: '', competitor: ref },
          position: i + 1,
          laps: p.laps_completed,
          metric: { BestLapMicros: p.last_lap_micros ?? null }
        };
      })
    };
  }

  async function fire(action: HeatAction) {
    if (!heat) return;
    const ack = await session.send(commandForAction(action, heat));
    // Scoring locks in the heat result; pull it so the Results screen has it to show. The
    // live stream only carries `LiveRaceState`, so the scored `HeatResult` is a separate
    // heat-scope fetch (`?projection=result`).
    if (ack.ok && action === 'Score') {
      await session.fetchHeatResult(heat);
    }
  }
</script>

<section class="live-control" aria-label="Live race control">
  <header class="hud" data-phase={phase}>
    <div class="hud-heat">
      <span class="label">Current heat</span>
      <div class="heat-id">
        {#if heat}
          <span class="value">{heat}</span>
        {:else}
          <span class="value none">— none on the timer —</span>
        {/if}
      </div>
    </div>

    <div class="hud-phase">
      <span class="label">Phase</span>
      <div class="phase" data-phase={phase}>
        <span class="phase-dot" aria-hidden="true"></span>{phase}
      </div>
    </div>

    <div class="hud-clock">
      <span class="label">Heat time</span>
      <div class="clock">
        <RaceClock {elapsedMs} label="Heat time" />
      </div>
    </div>

    {#if live?.on_deck}
      <div class="hud-ondeck">
        <span class="label">On deck</span>
        <span class="value">{live.on_deck}</span>
      </div>
    {/if}
  </header>

  {#if session.lastCommandError}
    <ErrorBanner error={session.lastCommandError} ondismiss={() => session.clearCommandError()} />
  {/if}

  <div class="controls" role="group" aria-label="Heat transitions">
    <span class="controls-label">Transitions</span>
    <div class="controls-row">
      {#each ACTION_ORDER as action (action)}
        {@const legal = isActionLegal(phase, action)}
        <ConfirmButton
          onconfirm={() => fire(action)}
          confirm={isDestructive(action)}
          disabled={!legal || !heat}
          variant={action === primary ? 'primary' : isDestructive(action) ? 'danger' : 'default'}
          title={actionDescription(action)}
        >
          {action}
        </ConfirmButton>
      {/each}
    </div>
  </div>

  <div class="panels">
    <Card title="Heat sheet" pad={false}>
      {#if live}
        <HeatSheet state={live} {names} />
      {:else}
        <p class="empty">Waiting for a live heat…</p>
      {/if}
    </Card>
    <Card title="Live standing" pad={false}>
      {#if liveResult}
        <Leaderboard result={liveResult} metricLabel="Last lap" />
      {:else}
        <p class="empty">No laps yet.</p>
      {/if}
    </Card>
  </div>
</section>

<style>
  .live-control {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
  }

  /* ── HUD: the race-ops command bar ───────────────────────────────────────── */
  .hud {
    --_phase: var(--gf-phase-scheduled);
    display: grid;
    grid-template-columns: 1fr auto auto auto;
    align-items: center;
    gap: var(--gf-space-8);
    padding: var(--gf-space-5) var(--gf-space-6);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    background: linear-gradient(
      100deg,
      color-mix(in srgb, var(--_phase) 10%, var(--gf-elevated)),
      var(--gf-elevated) 60%
    );
    box-shadow: var(--gf-shadow-sm), var(--gf-shadow-inset);
    position: relative;
    overflow: hidden;
  }
  .hud::before {
    content: '';
    position: absolute;
    left: 0;
    top: 0;
    bottom: 0;
    width: 4px;
    background: var(--_phase);
  }
  .hud[data-phase='Scheduled'] {
    --_phase: var(--gf-phase-scheduled);
  }
  .hud[data-phase='Staged'] {
    --_phase: var(--gf-phase-staged);
  }
  .hud[data-phase='Armed'] {
    --_phase: var(--gf-phase-armed);
  }
  .hud[data-phase='Running'] {
    --_phase: var(--gf-phase-running);
  }
  .hud[data-phase='Finished'] {
    --_phase: var(--gf-phase-finished);
  }
  .hud[data-phase='Scored'] {
    --_phase: var(--gf-phase-scored);
  }
  .label {
    display: block;
    font-size: var(--gf-font-size-2xs);
    color: var(--gf-text-muted);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    font-weight: var(--gf-font-weight-semibold);
    margin-bottom: var(--gf-space-1);
  }
  .value {
    font-size: var(--gf-font-size-xl);
    font-weight: var(--gf-font-weight-bold);
    letter-spacing: var(--gf-tracking-tight);
    font-variant-numeric: tabular-nums;
  }
  .value.none {
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-medium);
    color: var(--gf-text-faint);
  }
  .phase {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    padding: var(--gf-space-2) var(--gf-space-4);
    border-radius: var(--gf-radius-pill);
    background: color-mix(in srgb, var(--_phase) 16%, transparent);
    border: 1px solid color-mix(in srgb, var(--_phase) 45%, transparent);
    color: color-mix(in srgb, var(--_phase) 90%, var(--gf-text));
    font-weight: var(--gf-font-weight-bold);
    font-size: var(--gf-font-size-md);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-wide);
  }
  .phase-dot {
    width: 0.55em;
    height: 0.55em;
    border-radius: 50%;
    background: var(--_phase);
  }
  .hud[data-phase='Running'] .phase-dot {
    animation: hud-pulse 1.4s var(--gf-ease-out) infinite;
  }
  @keyframes hud-pulse {
    0% {
      box-shadow: 0 0 0 0 color-mix(in srgb, var(--_phase) 70%, transparent);
    }
    70% {
      box-shadow: 0 0 0 0.6em transparent;
    }
    100% {
      box-shadow: 0 0 0 0 transparent;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .hud[data-phase='Running'] .phase-dot {
      animation: none;
    }
  }

  /* ── Transition controls ─────────────────────────────────────────────────── */
  .controls {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
    padding: var(--gf-space-4) var(--gf-space-5);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    background: var(--gf-surface);
  }
  .controls-label {
    font-size: var(--gf-font-size-2xs);
    color: var(--gf-text-muted);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    font-weight: var(--gf-font-weight-semibold);
  }
  .controls-row {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-3);
  }

  .panels {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--gf-space-5);
  }
  .empty {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    padding: var(--gf-space-5);
  }
  @media (max-width: 75rem) {
    .hud {
      grid-template-columns: 1fr 1fr;
      gap: var(--gf-space-5);
    }
  }
  @media (max-width: 60rem) {
    .panels {
      grid-template-columns: 1fr;
    }
  }
</style>
