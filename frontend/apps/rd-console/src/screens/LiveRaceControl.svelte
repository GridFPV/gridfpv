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
  import { HeatSheet, RaceClock, Leaderboard } from '@gridfpv/components';
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
  import ConfirmButton from '../lib/ConfirmButton.svelte';
  import ErrorBanner from '../lib/ErrorBanner.svelte';
  import NewHeat from './NewHeat.svelte';

  let { session, names = {} }: { session: Session; names?: Record<string, string> } = $props();

  const live = $derived<LiveRaceState | undefined>(session.liveState);
  const phase = $derived(live?.phase ?? 'Scheduled');
  const heat = $derived<HeatId | undefined>(live?.current_heat);
  const primary = $derived(primaryAction(phase));

  // ── Race clock (#62) ────────────────────────────────────────────────────────────────
  // Drive the `RaceClock`'s `elapsedMs` client-side off the live `phase`. Behaviour:
  //   • Running                → tick a wall-clock timer (`Date.now() - start`) every 50ms.
  //   • Finished / Scored      → freeze the clock at its last ticked value (stop ticking).
  //   • Scheduled/Staged/Armed → reset to 0 (a fresh/idle heat, or no heat at all).
  // The off-ramps Abort/Restart/Discard aren't phases of their own — they fold back onto
  // one of the above (typically Scheduled), so they fall out of these same rules: an abort
  // back to Scheduled resets, a restart back to Scheduled resets, etc.
  //
  // This is the v1 fix and is *approximate*: on a late join (the heat is already Running
  // when this screen mounts, or after a reconnect) we start counting from "now", not from
  // the real heat start, so the displayed time can be short. The fuller fix (#62 follow-up)
  // is a server-authoritative start time in `LiveRaceState` — the recorded-at of the
  // Running transition — which makes the clock exact and reconnect-safe.
  let elapsedMs = $state(0);

  $effect(() => {
    // `phase` is the only reactive read in this effect, so it re-runs *only* on a phase
    // change — not on every render — which keeps the clock from restarting spuriously.
    if (phase === 'Running') {
      const startedAt = Date.now();
      const advance = () => {
        elapsedMs = Date.now() - startedAt;
      };
      advance();
      const id = setInterval(advance, 50);
      // Teardown: stop ticking when the phase leaves Running (or on unmount). The next
      // effect run applies the freeze (Finished/Scored) or reset (everything else).
      return () => clearInterval(id);
    }
    if (phase === 'Finished' || phase === 'Scored') {
      // Freeze: leave `elapsedMs` at its last Running value.
      return;
    }
    // Scheduled / Staged / Armed / no heat (incl. where Abort/Restart/Discard land): reset.
    elapsedMs = 0;
  });

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
  <header class="top">
    <div class="heat-id">
      {#if heat}
        <span class="label">Current heat</span>
        <span class="value">{heat}</span>
      {:else}
        <span class="label">No heat on the timer</span>
      {/if}
    </div>
    <div class="phase" data-phase={phase}>{phase}</div>
    <div class="clock">
      <RaceClock {elapsedMs} label="Heat time" />
    </div>
    {#if live?.on_deck}
      <div class="on-deck">
        <span class="label">On deck</span>
        <span class="value">{live.on_deck}</span>
      </div>
    {/if}
  </header>

  {#if session.lastCommandError}
    <ErrorBanner error={session.lastCommandError} ondismiss={() => session.clearCommandError()} />
  {/if}

  <NewHeat {session} />

  <div class="controls" role="group" aria-label="Heat transitions">
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

  <div class="panels">
    <div class="panel">
      <h3>Heat sheet</h3>
      {#if live}
        <HeatSheet state={live} {names} />
      {:else}
        <p class="empty">Waiting for a live heat…</p>
      {/if}
    </div>
    <div class="panel">
      <h3>Live standing</h3>
      {#if liveResult}
        <Leaderboard result={liveResult} metricLabel="Last lap" />
      {:else}
        <p class="empty">No laps yet.</p>
      {/if}
    </div>
  </div>
</section>

<style>
  .live-control {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .top {
    display: flex;
    align-items: center;
    gap: var(--gf-space-8);
    flex-wrap: wrap;
  }
  .label {
    display: block;
    font-size: var(--gf-font-size-xs);
    color: var(--gf-color-text-muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .value {
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-bold);
  }
  .phase {
    padding: var(--gf-space-2) var(--gf-space-4);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-color-surface-alt);
    border: 1px solid var(--gf-color-border);
    font-weight: var(--gf-font-weight-bold);
  }
  .phase[data-phase='Running'] {
    background: var(--gf-color-live);
    color: var(--gf-color-accent-contrast);
    border-color: var(--gf-color-live);
  }
  .controls {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-3);
    padding: var(--gf-space-4);
    border: 1px solid var(--gf-color-border);
    border-radius: var(--gf-radius-md);
    background: var(--gf-color-surface);
  }
  .panels {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--gf-space-6);
  }
  .panel h3 {
    font-size: var(--gf-font-size-md);
    margin: 0 0 var(--gf-space-3);
  }
  .empty {
    color: var(--gf-color-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  @media (max-width: 60rem) {
    .panels {
      grid-template-columns: 1fr;
    }
  }
</style>
