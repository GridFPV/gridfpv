<script lang="ts">
  /**
   * RaceOverlay — the OBS browser source (`#/overlay/race`): a timer in the video, for any event.
   *
   * It follows the Director's **active event** — a club night, a one-pilot time trial at home, an
   * open-practice session — and shows the current heat: the race clock (a countdown when the round
   * has a fixed end, exactly as the console's clocks do), the phase, the heat's name, and a row per
   * pilot with their laps off the served lap list. A heat with a **single pilot** gets the long
   * form instead of a row: the recent laps, with the best run of laps in a row highlighted when the
   * round scores one. Everything is read; it controls nothing (streaming.html §2). Close OBS and
   * the race carries on exactly as before.
   *
   * It renders idle — the brand and "waiting for a race" — with no event or no current heat, which
   * is what a browser source sitting in a scene should do rather than an error.
   *
   * `theme` is the panel's contrast for the footage it sits over: light text on a dark glass panel
   * (the default — reads over most DVR), or dark text on a light one for a bright room camera. The
   * light variant is the component library's `data-theme="light"` token set scoped to this page,
   * plus this page's own glass values flipped.
   */
  import { RaceClock, formatMicros } from '@gridfpv/components';
  import type {
    CompetitorRef,
    EventMeta,
    HeatId,
    HeatSummary,
    LapList,
    LiveRaceState,
    Pilot,
    RoundStanding
  } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import { useRaceClock } from '../lib/raceClock.svelte.js';
  import { buildCompetitorNames } from '../lib/competitorName.js';
  import { heatNameById } from '../lib/heats.js';
  import { fixedEndWindowMicros } from '../lib/raceWindow.js';
  import {
    bestConsecutive,
    competitorLaps,
    consecutiveWindow,
    heatRound,
    overlayOrder,
    servedRoundBestMicros,
    type ConsecutiveWindow
  } from '../lib/overlay.js';
  import type { OverlayTheme } from '../lib/route.js';

  let { session, theme = 'dark' }: { session: Session; theme?: OverlayTheme } = $props();

  // The page keys on transparency: flag the document while mounted (app.css lifts the canvas).
  $effect(() => {
    document.documentElement.dataset.overlay = 'race';
    return () => {
      delete document.documentElement.dataset.overlay;
    };
  });

  const live = $derived<LiveRaceState | undefined>(session.liveState);
  const phase = $derived(live?.phase ?? 'Scheduled');
  const heat = $derived<HeatId | undefined>(live?.current_heat);
  const flying = $derived(phase === 'Running' || phase === 'Armed');

  // ── Following the program (streaming.html §2.1) ────────────────────────────────────────────
  // An overlay sits in an OBS scene for the whole evening, so it cannot assume the event it loaded
  // with is the event on the timer, nor that the event's shape is still what it was. Two reads keep
  // it honest without a reload of the browser source:
  //   • the Director's **active event** is polled, and a change re-roots the session onto the new
  //     one (the live stream is scoped per event — only `resolveActiveEvent` moves it);
  //   • the event's **meta** (its rounds, and so the heat's scoring window and race time) is
  //     re-read whenever the current heat or its phase moves, since a round the RD just created on
  //     the console never reaches this client any other way. The session's own `currentEvent` is
  //     the fallback until the first fresh read lands.
  const ACTIVE_POLL_MS = 5_000;
  $effect(() => {
    const check = async () => {
      const id = await session.getActiveEventId();
      if (id !== undefined && id !== session.currentEvent?.id) await session.resolveActiveEvent();
    };
    const timer = setInterval(() => void check(), ACTIVE_POLL_MS);
    return () => clearInterval(timer);
  });
  let freshMeta = $state<EventMeta | undefined>(undefined);
  $effect(() => {
    const id = session.currentEvent?.id;
    if (!id) {
      freshMeta = undefined;
      return;
    }
    void heat;
    void phase;
    void session
      .listEvents()
      .then((list) => {
        const found = list.find((e) => e.id === id);
        if (found) freshMeta = found;
      })
      .catch(() => {});
  });
  const event = $derived<EventMeta | undefined>(
    freshMeta?.id === session.currentEvent?.id ? freshMeta : session.currentEvent
  );
  const active = $derived(event !== undefined && heat !== undefined);

  const clock = useRaceClock(
    () => phase,
    () => live?.race_started_at,
    () => live?.race_ended_at,
    () => session.serverNowMs()
  );

  // ── The event's heats and pilots: the heat's name, round and lineup, and callsigns ─────────
  // Re-read when the current heat or its phase moves — a heat that just got scheduled, or a round
  // that just got created, must name itself without a reload of the browser source.
  let heats = $state<HeatSummary[]>([]);
  $effect(() => {
    if (!event) {
      heats = [];
      return;
    }
    void heat;
    void phase;
    void session
      .listHeats()
      .then((list) => (heats = list))
      .catch(() => {});
  });
  let pilots = $state<Pilot[]>([]);
  $effect(() => {
    if (!event) {
      pilots = [];
      return;
    }
    void session
      .listPilots()
      .then((p) => (pilots = p))
      .catch(() => (pilots = []));
  });

  const summary = $derived(heats.find((h) => h.heat === heat));
  const heatName = $derived(heat ? heatNameById(heat, heats) : '');
  const round = $derived(heatRound(event, heats, heat));
  /** The consecutive window the round scores (`BestConsecutive { n }`), else none. */
  const windowN = $derived(consecutiveWindow(round?.win_condition));
  /** A fixed race end makes the primary clock a countdown, as it is on the console (#504). */
  const windowMicros = $derived(fixedEndWindowMicros(round));
  const remainingMs = $derived(
    windowMicros === undefined ? undefined : windowMicros / 1000 - clock.elapsedMs
  );

  // ONE assembly of the resolver inputs (#416): hand `buildCompetitorNames` what this page has.
  const names = $derived(
    buildCompetitorNames({
      pilots,
      progress: live?.progress,
      heat: summary,
      heats,
      timer: session.primaryTimer,
      membership: event?.classes_membership,
      layouts: event?.channel_layouts
    })
  );

  // ── The current heat's laps, re-read on every stream tick (served, marshaling-aware) ──────
  let laps = $state<LapList | undefined>(undefined);
  $effect(() => {
    void session.protocolState;
    const current = heat;
    if (!current) {
      laps = undefined;
      return;
    }
    void session.fetchHeatLaps(current).then((list) => {
      if (list) laps = list;
    });
  });

  // ── The round-wide best per pilot — re-read when a heat finishes (its phase leaves Running) ─
  let standings = $state<RoundStanding[]>([]);
  $effect(() => {
    const r = round;
    if (!r) {
      standings = [];
      return;
    }
    void phase;
    void session
      .roundStandings(r.id)
      .then((s) => (standings = s))
      .catch(() => {});
  });

  /** One pilot's line in the frame. */
  interface Row {
    ref: CompetitorRef;
    name: string;
    /** Served laps (durations, µs), in order. */
    lapMicros: number[];
    laps: number;
    last: number | undefined;
    best: number | undefined;
    /** This heat's best `windowN` laps in a row, when the round scores one. */
    window: ConsecutiveWindow | undefined;
    /** The served round-wide figure for this pilot (best window, or best lap). */
    roundBest: number | undefined;
  }

  const order = $derived(
    overlayOrder([
      live?.running_order,
      live?.active_pilots,
      summary?.lineup,
      laps?.competitors.map((c) => c.competitor.competitor)
    ])
  );
  const rows = $derived<Row[]>(
    order.map((ref) => {
      const served = competitorLaps(laps, ref);
      const progress = live?.progress?.find((p) => p.competitor === ref);
      const lapMicros = served.map((l) => l.duration_micros);
      return {
        ref,
        name: names.name(ref),
        lapMicros,
        laps: served.length || (progress?.laps_completed ?? 0),
        last: lapMicros.at(-1) ?? progress?.last_lap_micros,
        best: lapMicros.length ? Math.min(...lapMicros) : progress?.best_lap_micros,
        window: windowN === undefined ? undefined : bestConsecutive(lapMicros, windowN),
        roundBest: servedRoundBestMicros(standings, ref)
      };
    })
  );
  /** A heat flown by one pilot gets the long form: their recent laps, not a one-row table. */
  const solo = $derived(rows.length === 1 ? rows[0] : undefined);
  /** The last few laps, newest last — a video frame has room for a handful. */
  const SHOWN = 6;
  const recent = $derived(solo ? solo.lapMicros.slice(-SHOWN) : []);
  const recentOffset = $derived(solo ? Math.max(0, solo.lapMicros.length - SHOWN) : 0);

  function inWindow(index: number, w: ConsecutiveWindow | undefined): boolean {
    return w !== undefined && index >= w.start && index < w.start + w.laps.length;
  }
  function micros(value: number | undefined): string {
    return value === undefined ? '—' : formatMicros(value);
  }
  const phaseLabel = $derived.by(() => {
    switch (phase) {
      case 'Running':
        return 'LIVE';
      case 'Armed':
        return 'READY';
      case 'Staged':
        return 'STAGED';
      case 'Unofficial':
        return 'FINISHED';
      case 'Final':
        return 'FINAL';
      default:
        return '—';
    }
  });
</script>

<div
  class="overlay gridfpv-overlay"
  data-flying={flying}
  data-theme={theme}
  aria-label="Race overlay"
>
  <div class="panel">
    <div class="head">
      <span class="brand">Grid<span class="fpv">FPV</span></span>
      {#if active && event}
        <span class="track">{event.name}{heatName ? ` · ${heatName}` : ''}</span>
      {:else if event}
        <span class="track">{event.name} · waiting for a race…</span>
      {:else}
        <span class="track">waiting for a race…</span>
      {/if}
    </div>

    {#if active}
      <div class="clock-row">
        {#if remainingMs !== undefined}
          <RaceClock {remainingMs} label="Time remaining" />
          <span class="elapsed" aria-label="Race time elapsed"
            >{formatMicros(clock.elapsedMs * 1000)}</span
          >
        {:else}
          <RaceClock elapsedMs={clock.elapsedMs} label="Race time" />
        {/if}
        <span class="phase">{phaseLabel}</span>
      </div>

      {#if solo}
        <div class="solo-name">{solo.name}</div>
        <div class="times">
          {#if windowN !== undefined}
            <div class="stat">
              <span class="stat-label">Best {windowN} this heat</span>
              <span class="stat-value" aria-label="This heat's best {windowN} consecutive laps">
                {micros(solo.window?.totalMicros)}
              </span>
            </div>
            <div class="stat">
              <span class="stat-label">Best {windowN} today</span>
              <span class="stat-value" aria-label="Best {windowN} consecutive laps, all heats">
                {micros(solo.roundBest)}
              </span>
            </div>
          {:else}
            <div class="stat">
              <span class="stat-label">Laps</span>
              <span class="stat-value" aria-label="Laps this heat">{solo.laps}</span>
            </div>
            <div class="stat">
              <span class="stat-label">Best lap</span>
              <span class="stat-value" aria-label="Best lap this heat">{micros(solo.best)}</span>
            </div>
          {/if}
        </div>

        <ol class="laps" aria-label="Recent laps">
          {#each recent as lap, i (recentOffset + i)}
            <li class="lap" class:in-window={inWindow(recentOffset + i, solo.window)}>
              <span class="lap-n">L{recentOffset + i + 1}</span>
              <span class="lap-t">{formatMicros(lap)}</span>
            </li>
          {/each}
        </ol>
      {:else if rows.length > 0}
        <table class="field" aria-label="Pilots in this heat">
          <thead>
            <tr>
              <th class="pos" aria-label="Position"></th>
              <th class="name">Pilot</th>
              <th class="num">Laps</th>
              <th class="num">Last</th>
              <th class="num">{windowN === undefined ? 'Best' : `Best ${windowN}`}</th>
            </tr>
          </thead>
          <tbody>
            {#each rows as row, i (row.ref)}
              <tr>
                <td class="pos">{i + 1}</td>
                <td class="name">{row.name}</td>
                <td class="num">{row.laps}</td>
                <td class="num">{micros(row.last)}</td>
                <td class="num best">
                  {windowN === undefined ? micros(row.best) : micros(row.window?.totalMicros)}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {:else}
        <p class="empty">No pilots in this heat yet.</p>
      {/if}
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
  .elapsed {
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
    font-size: var(--gf-font-size-md);
    color: var(--gf-text-muted);
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
  .solo-name {
    font-weight: 700;
    font-size: var(--gf-font-size-lg);
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
  .field {
    border-collapse: collapse;
    width: 100%;
    font-size: var(--gf-font-size-md);
  }
  .field th {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: 0.12em;
    color: var(--gf-text-muted);
    font-weight: 600;
    text-align: left;
    padding: 2px 8px;
  }
  .field td {
    padding: 3px 8px;
    border-radius: 8px;
  }
  .field .pos {
    width: 1.5rem;
    color: var(--gf-text-muted);
    font-variant-numeric: tabular-nums;
  }
  .field .name {
    font-weight: 700;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 14rem;
  }
  .field .num {
    text-align: right;
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .field .best {
    color: #fff;
    font-weight: 700;
  }
  .empty {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }

  /* Dark text on a light panel (`#/overlay/race/light`). The semantic tokens flip through the
   * library's `[data-theme='light']` set; only this page's own glass values need restating. */
  .overlay[data-theme='light'] .panel {
    background: rgba(255, 255, 255, 0.82);
    border-color: rgba(0, 0, 0, 0.14);
    text-shadow: none;
  }
  .overlay[data-theme='light'] .clock-row :global(.gridfpv-race-clock),
  .overlay[data-theme='light'] .field .best {
    color: var(--gf-text);
  }
  .overlay[data-theme='light'] .fpv,
  .overlay[data-theme='light'][data-flying='true'] .phase {
    color: var(--gf-brand-600);
  }
  .overlay[data-theme='light'] .lap.in-window {
    background: rgba(0, 0, 0, 0.08);
    color: var(--gf-text);
  }
</style>
