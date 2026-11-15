<script lang="ts">
  /**
   * RacePanel — the race overlay's picture, and nothing else: the heat's name, the race's
   * state in a word, the race clock, and one row per seat in lineup order with the pilot's
   * place, laps, last lap and best lap. A heat ranked on best N consecutive shows the best
   * N-lap run where the best lap would be. A heat with one seat gets the long form: big
   * numbers and the last few laps.
   *
   * It draws the race it is handed, as that race stands at `now`. The live overlay
   * (`RaceOverlay.svelte`) hands it the event's current race and the Director's clock. The
   * replay overlay (`ReplayOverlay.svelte`) hands it a finished race cut back to a moment of
   * its recording. Both get the same picture, which is what lets a redrawn video match one
   * filmed live.
   *
   * With no race it shows the brand and `eventName`, or the brand alone.
   *
   * `theme` is the contrast for the video underneath: `'dark'` (the default) is light text
   * on a dark panel, `'light'` is dark text on a light panel.
   */
  import { RaceClock, formatMicros } from '@gridfpv/components';
  import type { EventView, RaceView } from '@gridfpv/types';
  import type { Names } from '../lib/names.js';
  import {
    bestRunInHeat,
    consecutiveLaps,
    inRun,
    overlayRows,
    overlayStateWord
  } from '../lib/overlay.js';
  import type { OverlayTheme } from '../lib/route.js';
  import { readClock } from '../lib/run.js';

  /** How many of a lone pilot's laps are listed. A video frame has room for a handful. */
  const SHOWN_LAPS = 6;

  let {
    theme = 'dark',
    race,
    names,
    view,
    now,
    eventName
  }: {
    theme?: OverlayTheme;
    /** The race to draw. None: the idle mark. */
    race?: RaceView;
    names?: Names;
    /** The event the race is in: its phases (for the format) and its other races. */
    view?: EventView;
    /** The Director's clock, in epoch microseconds. */
    now: number;
    /** Shown beside the brand when there is no race. */
    eventName?: string;
  } = $props();

  const clock = $derived(readClock(race, now));
  const phase = $derived(
    race ? view?.phases.find((candidate) => candidate.def.id === race.heat) : undefined
  );
  /** The N of a heat ranked on best N consecutive, else `undefined`. */
  const runLength = $derived(consecutiveLaps(phase?.format));
  const rows = $derived(race && names ? overlayRows(race, names, runLength) : []);
  /** A heat with one seat gets the long form. */
  const solo = $derived(rows.length === 1 ? rows[0] : undefined);
  const recent = $derived(solo ? solo.lapMicros.slice(-SHOWN_LAPS) : []);
  const recentFrom = $derived(solo ? Math.max(0, solo.lapMicros.length - SHOWN_LAPS) : 0);
  /** The lone pilot's best run across every saved race of the heat, and this one. */
  const heatBest = $derived(
    solo?.pilot !== undefined && runLength !== undefined && race && view
      ? bestRunInHeat(view, race.heat, solo.pilot, runLength, race)
      : undefined
  );
</script>

<div
  class="overlay"
  data-theme={theme}
  data-state={race?.state ?? 'idle'}
  role="region"
  aria-label="Race overlay"
>
  <div class="panel">
    <div class="head">
      <span class="brand">Grid<span class="fpv">FPV</span></span>
      {#if race && names}
        <span class="heat" data-testid="overlay-heat">{names.phase(race.heat)}</span>
      {:else if eventName !== undefined}
        <span class="heat idle" data-testid="overlay-idle">{eventName}</span>
      {/if}
    </div>

    {#if race && names}
      <div class="clock-row">
        {#if clock.remainingMs !== undefined}
          <RaceClock remainingMs={clock.remainingMs} label={clock.label} />
        {:else}
          <RaceClock elapsedMs={clock.elapsedMs ?? 0} label={clock.label} />
        {/if}
        <span class="state">
          <span class="state-word" data-testid="overlay-state">{overlayStateWord(race)}</span>
          <span class="clock-label">{clock.label}</span>
        </span>
      </div>

      {#if solo}
        <div class="solo" data-testid="overlay-solo" aria-label={solo.name}>
          <div class="solo-name">{solo.name}</div>
          <dl class="stats">
            <div class="stat">
              <dt>Laps</dt>
              <dd data-stat="laps">{solo.laps}</dd>
            </div>
            <div class="stat">
              <dt>Last lap</dt>
              <dd data-stat="last">{formatMicros(solo.lastLapMicros)}</dd>
            </div>
            {#if runLength === undefined}
              <div class="stat">
                <dt>Best lap</dt>
                <dd data-stat="best">{formatMicros(solo.bestLapMicros)}</dd>
              </div>
            {:else}
              <div class="stat">
                <dt>Best {runLength} laps</dt>
                <dd data-stat="best-run">{formatMicros(solo.bestRun?.totalMicros)}</dd>
              </div>
              <div class="stat">
                <dt>Best {runLength} this heat</dt>
                <dd data-stat="heat-best-run">{formatMicros(heatBest)}</dd>
              </div>
            {/if}
          </dl>
          {#if recent.length > 0}
            <ol class="laps" aria-label="Last laps">
              {#each recent as micros, index (recentFrom + index)}
                <li class:in-run={inRun(recentFrom + index, solo.bestRun)}>
                  <span class="lap-n">Lap {recentFrom + index + 1}</span>
                  <span class="lap-t">{formatMicros(micros)}</span>
                </li>
              {/each}
            </ol>
          {/if}
        </div>
      {:else if rows.length > 0}
        <table class="field" aria-label="Pilots in this heat">
          <thead>
            <tr>
              <th class="pos" aria-label="Place"></th>
              <th class="name">Pilot</th>
              <th class="num">Laps</th>
              <th class="num">Last</th>
              <th class="num">{runLength === undefined ? 'Best' : `Best ${runLength}`}</th>
            </tr>
          </thead>
          <tbody>
            {#each rows as row (row.key)}
              <tr aria-label={row.name}>
                <td class="pos" data-stat="place">{row.place ?? '–'}</td>
                <td class="name">{row.name}</td>
                <td class="num" data-stat="laps">{row.laps}</td>
                <td class="num" data-stat="last">{formatMicros(row.lastLapMicros)}</td>
                <td class="num best" data-stat={runLength === undefined ? 'best' : 'best-run'}>
                  {runLength === undefined
                    ? formatMicros(row.bestLapMicros)
                    : formatMicros(row.bestRun?.totalMicros)}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    {/if}
  </div>
</div>

<style>
  /* Sizes are in pixels on purpose: the page is laid out for a 1920x1080 browser source, and
   * must not follow the console's dense type scale. */
  .overlay {
    --ov-text: #fff;
    --ov-muted: rgba(255, 255, 255, 0.78);
    --ov-panel: rgba(0, 0, 0, 0.58);
    --ov-edge: rgba(255, 255, 255, 0.2);
    --ov-mark: rgba(255, 255, 255, 0.14);
    --ov-accent: var(--gf-brand-400);
    --ov-shadow: 0 2px 4px rgba(0, 0, 0, 0.85), 0 0 2px rgba(0, 0, 0, 0.9);
    min-height: 100vh;
    background: transparent;
    color: var(--ov-text);
    font-family: var(--gf-font-family);
    font-size: 30px;
    line-height: 1.2;
    display: flex;
    align-items: flex-start;
    justify-content: flex-start;
    padding: 40px;
  }
  /* Dark text on a light panel, for bright video. */
  .overlay[data-theme='light'] {
    --ov-text: #0b0f16;
    --ov-muted: rgba(11, 15, 22, 0.74);
    --ov-panel: rgba(255, 255, 255, 0.86);
    --ov-edge: rgba(0, 0, 0, 0.16);
    --ov-mark: rgba(0, 0, 0, 0.09);
    --ov-accent: var(--gf-brand-600);
    --ov-shadow: 0 1px 2px rgba(255, 255, 255, 0.9);
  }
  .panel {
    display: flex;
    flex-direction: column;
    gap: 18px;
    min-width: 0;
    padding: 24px 30px;
    border-radius: 22px;
    background: var(--ov-panel);
    border: 2px solid var(--ov-edge);
    backdrop-filter: blur(8px);
    text-shadow: var(--ov-shadow);
  }
  .head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 28px;
  }
  .brand {
    font-weight: 800;
    letter-spacing: -0.02em;
    font-size: 26px;
  }
  .fpv {
    color: var(--ov-accent);
  }
  .heat {
    font-size: 32px;
    font-weight: 700;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 760px;
  }
  .heat.idle {
    font-size: 24px;
    font-weight: 600;
    color: var(--ov-muted);
  }
  .clock-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 36px;
  }
  .clock-row :global(.gridfpv-race-clock) {
    font-size: 96px;
    color: var(--ov-text);
  }
  .clock-row :global(.gridfpv-race-clock[data-urgency='closing']) {
    color: var(--gf-warn);
  }
  .state {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 4px;
  }
  .state-word {
    font-size: 30px;
    font-weight: 800;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--ov-muted);
  }
  .overlay[data-state='Running'] .state-word,
  .overlay[data-state='Staging'] .state-word {
    color: var(--ov-accent);
  }
  .clock-label {
    font-size: 20px;
    text-transform: uppercase;
    letter-spacing: 0.12em;
    color: var(--ov-muted);
  }

  /* The table: one row per seat. */
  .field {
    border-collapse: collapse;
    width: 100%;
    font-size: 32px;
  }
  .field th {
    font-size: 20px;
    text-transform: uppercase;
    letter-spacing: 0.12em;
    color: var(--ov-muted);
    font-weight: 600;
    text-align: left;
    padding: 4px 14px;
  }
  .field td {
    padding: 6px 14px;
  }
  .field .pos {
    width: 48px;
    color: var(--ov-muted);
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }
  .field .name {
    font-weight: 700;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 420px;
  }
  .field .num {
    text-align: right;
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .field .best {
    font-weight: 700;
  }

  /* The long form: one pilot. */
  .solo {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .solo-name {
    font-size: 44px;
    font-weight: 800;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 900px;
  }
  .stats {
    margin: 0;
    display: flex;
    gap: 44px;
  }
  .stat {
    display: flex;
    flex-direction: column-reverse;
    gap: 4px;
  }
  .stat dt {
    font-size: 20px;
    text-transform: uppercase;
    letter-spacing: 0.12em;
    color: var(--ov-muted);
    white-space: nowrap;
  }
  .stat dd {
    margin: 0;
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
    font-size: 64px;
    font-weight: 700;
    line-height: 1.05;
  }
  .laps {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
    max-width: 420px;
  }
  .laps li {
    display: flex;
    justify-content: space-between;
    gap: 32px;
    padding: 4px 14px;
    border-radius: 10px;
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
    font-size: 30px;
    color: var(--ov-muted);
  }
  .laps li.in-run {
    background: var(--ov-mark);
    color: var(--ov-text);
    font-weight: 700;
  }
</style>
