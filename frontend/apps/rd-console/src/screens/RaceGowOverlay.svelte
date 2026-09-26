<script lang="ts">
  /**
   * RaceGowOverlay — the OBS browser source (`#/overlay/racegow`): a timer in the video.
   *
   * RaceGOW's rules want the recording to show a timer in view. This page is that timer: the run
   * clock, the laps as they land, this run's best 3-lap window and the best of the session, on a
   * transparent background sized for a video frame. It is a **read-only protocol client** like
   * every overlay in the streaming design (streaming.html §2): it follows the Director's active
   * event, subscribes to the same live stream the console does, reads the same served lap list,
   * and controls nothing. Close OBS and the run carries on exactly as before.
   *
   * It renders idle — the brand and "waiting for a run" — with no RaceGOW event active, which is
   * what a browser source sitting in a scene should do rather than an error.
   */
  import { RaceClock, formatMicros } from '@gridfpv/components';
  import type { HeatId, Lap, LiveRaceState, Pilot, RoundStanding } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import { useRaceClock } from '../lib/raceClock.svelte.js';
  import {
    bestConsecutive,
    isRaceGowEvent,
    raceGowRound,
    runLaps,
    servedBestMicros,
    RACEGOW_LAPS,
    type ConsecutiveWindow
  } from '../lib/racegow.js';

  let { session }: { session: Session } = $props();

  // The page keys on transparency: flag the document while mounted (app.css lifts the canvas).
  $effect(() => {
    document.documentElement.dataset.overlay = 'racegow';
    return () => {
      delete document.documentElement.dataset.overlay;
    };
  });

  const event = $derived(session.currentEvent);
  const active = $derived(isRaceGowEvent(event));
  const round = $derived(raceGowRound(event));
  const live = $derived<LiveRaceState | undefined>(session.liveState);
  const phase = $derived(live?.phase ?? 'Scheduled');
  const heat = $derived<HeatId | undefined>(live?.current_heat);
  const flying = $derived(phase === 'Running' || phase === 'Armed');

  const clock = useRaceClock(
    () => phase,
    () => live?.race_started_at,
    () => live?.race_ended_at,
    () => session.serverNowMs()
  );

  let pilots = $state<Pilot[]>([]);
  $effect(() => {
    if (!active) return;
    void session
      .listPilots()
      .then((p) => (pilots = p))
      .catch(() => (pilots = []));
  });
  const callsign = $derived(pilots.find((p) => p.id === event?.roster?.[0])?.callsign ?? '');

  // The current run's laps, re-read on every stream tick (served, marshaling-aware).
  let laps = $state<Lap[]>([]);
  let lapsHeat: HeatId | undefined;
  $effect(() => {
    void session.protocolState;
    const current = heat;
    if (!current) {
      laps = [];
      lapsHeat = undefined;
      return;
    }
    void session.fetchHeatLaps(current).then((list) => {
      if (!list) return;
      if (lapsHeat !== current) lapsHeat = current;
      laps = runLaps(list);
    });
  });
  const best = $derived(bestConsecutive(laps.map((l) => l.duration_micros)));
  /** The last few laps, newest last — a video frame has room for a handful. */
  const SHOWN = 6;
  const recent = $derived(laps.slice(-SHOWN));
  const recentOffset = $derived(Math.max(0, laps.length - SHOWN));

  // The session best — re-read when a run finishes (the heat leaves Running).
  let standings = $state<RoundStanding[]>([]);
  $effect(() => {
    if (!round) return;
    void phase;
    void session
      .roundStandings(round.id)
      .then((s) => (standings = s))
      .catch(() => {});
  });
  const sessionBest = $derived(servedBestMicros(standings));

  function inWindow(index: number, w: ConsecutiveWindow | undefined): boolean {
    return w !== undefined && index >= w.start && index < w.start + w.laps.length;
  }
</script>

<div class="overlay gridfpv-overlay" data-flying={flying} aria-label="RaceGOW overlay">
  <div class="panel">
    <div class="head">
      <span class="brand">Grid<span class="fpv">FPV</span></span>
      {#if active && event}
        <span class="track">{event.name}{callsign ? ` · ${callsign}` : ''}</span>
      {:else}
        <span class="track">waiting for a run…</span>
      {/if}
    </div>

    {#if active}
      <div class="clock-row">
        <RaceClock elapsedMs={clock.elapsedMs} label="Run time" />
        <span class="phase">
          {#if phase === 'Running'}
            LIVE
          {:else if phase === 'Armed'}
            READY
          {:else if phase === 'Unofficial' || phase === 'Final'}
            STOPPED
          {:else}
            —
          {/if}
        </span>
      </div>

      <div class="times">
        <div class="stat">
          <span class="stat-label">Best {RACEGOW_LAPS} this run</span>
          <span class="stat-value" aria-label="This run's best 3 consecutive laps">
            {best ? formatMicros(best.totalMicros) : '—'}
          </span>
        </div>
        <div class="stat">
          <span class="stat-label">Best {RACEGOW_LAPS} today</span>
          <span class="stat-value" aria-label="Best 3 consecutive laps, all runs">
            {sessionBest !== undefined ? formatMicros(sessionBest) : '—'}
          </span>
        </div>
      </div>

      <ol class="laps" aria-label="Recent laps">
        {#each recent as lap, i (lap.end_ref)}
          <li class="lap" class:in-window={inWindow(recentOffset + i, best)}>
            <span class="lap-n">L{recentOffset + i + 1}</span>
            <span class="lap-t">{formatMicros(lap.duration_micros)}</span>
          </li>
        {/each}
      </ol>
    {/if}
  </div>
</div>

<style>
  .overlay {
    min-height: 100vh;
    background: transparent;
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    display: flex;
    align-items: flex-start;
    justify-content: flex-start;
    padding: 24px;
  }
  .panel {
    display: flex;
    flex-direction: column;
    gap: 12px;
    min-width: 22rem;
    padding: 18px 22px;
    border-radius: 18px;
    background: rgba(0, 0, 0, 0.55);
    border: 1px solid rgba(255, 255, 255, 0.18);
    backdrop-filter: blur(6px);
    text-shadow: 0 1px 2px rgba(0, 0, 0, 0.6);
  }
  .head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 16px;
  }
  .brand {
    font-weight: 800;
    letter-spacing: -0.02em;
    font-size: var(--gf-font-size-md);
  }
  .fpv {
    color: var(--gf-brand-400);
  }
  .track {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 26rem;
  }
  .clock-row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 16px;
  }
  .clock-row :global(.gridfpv-race-clock) {
    font-size: 3.5rem;
    color: #fff;
  }
  .phase {
    font-weight: 800;
    letter-spacing: 0.14em;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .overlay[data-flying='true'] .phase {
    color: var(--gf-brand-400);
  }
  .times {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 16px;
  }
  .stat {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .stat-label {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: 0.12em;
    color: var(--gf-text-muted);
  }
  .stat-value {
    font-family: var(--gf-font-mono);
    font-size: 2rem;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    line-height: 1.1;
  }
  .laps {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .lap {
    display: flex;
    justify-content: space-between;
    gap: 16px;
    padding: 2px 8px;
    border-radius: 8px;
    font-family: var(--gf-font-mono);
    font-size: var(--gf-font-size-md);
    font-variant-numeric: tabular-nums;
    color: var(--gf-text-muted);
  }
  .lap.in-window {
    background: rgba(255, 255, 255, 0.12);
    color: #fff;
    font-weight: 700;
  }
</style>
