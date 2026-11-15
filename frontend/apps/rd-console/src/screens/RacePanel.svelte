<script lang="ts">
  /**
   * RacePanel — the race overlay's picture, and nothing else: a glass sheet over the video
   * with the title line, the clock and the state word, and the pilots. One pilot gets the
   * long form (their recent laps, with the best run of laps in a row lit up). Several get a
   * table.
   *
   * It is the panel grid-rh's race overlay drew, on Grid's data. It draws the race it is
   * handed, as that race stands at `now`. The live overlay (`RaceOverlay.svelte`) hands it
   * the event's current race and the Director's clock. The replay overlay
   * (`ReplayOverlay.svelte`) hands it a finished race cut back to a moment of its recording.
   * Both get the same picture, which is what lets a redrawn video match one filmed live.
   *
   * `placement` is where the panel sits: the corner, the distance in from it, the size and
   * how solid the background is. `logos` are the pictures for the two ends of the title
   * line; without a left one the GridFPV wordmark stands there.
   *
   * `theme` is the contrast for the video underneath: `'dark'` (the default) is light text
   * on dark glass, `'light'` is dark text on a light panel.
   */
  import type { EventView, OverlayPlacement, RaceView } from '@gridfpv/types';
  import type { Names } from '../lib/names.js';
  import {
    consecutiveLaps,
    formatLapMicros,
    inRun,
    overlayClock,
    overlayRows,
    overlayStateWord,
    overlayTitle
  } from '../lib/overlay.js';
  import { DEFAULT_PLACEMENT, placementStyle } from '../lib/overlayPlacement.js';
  import type { OverlayTheme } from '../lib/route.js';

  /** How many of a lone pilot's laps are listed. */
  const SHOWN = 6;

  let {
    theme = 'dark',
    race,
    names,
    view,
    now,
    eventName,
    placement = DEFAULT_PLACEMENT,
    logos = {},
    sample
  }: {
    theme?: OverlayTheme;
    /** The race to draw. None: the panel waits. */
    race?: RaceView;
    names?: Names;
    /** The event the race is in: its phases (for the format) and its other races. */
    view?: EventView;
    /** The Director's clock, in epoch microseconds. */
    now: number;
    /** The event's name. It leads the title, and stands alone when there is no race. */
    eventName?: string;
    placement?: OverlayPlacement;
    /** Where each logo is served from, for the sides that have one. */
    logos?: { left?: string; right?: string };
    /**
     * For a made-up race, which has no event behind it to read these from: the title line,
     * and the N of "best N laps in a row".
     */
    sample?: { title: string; runLength?: number };
  } = $props();

  const clock = $derived(overlayClock(race, now));
  const phase = $derived(
    race ? view?.phases.find((candidate) => candidate.def.id === race.heat) : undefined
  );
  /** The N of a heat ranked on best N consecutive, else `undefined`. */
  const runLength = $derived(sample ? sample.runLength : consecutiveLaps(phase?.format));
  const rows = $derived(race && names ? overlayRows(race, names, runLength) : []);
  /** A heat with one seat gets the long form. */
  const solo = $derived(rows.length === 1 ? rows[0] : undefined);
  const title = $derived(
    sample
      ? sample.title
      : race && names && view
        ? overlayTitle(view, names, race, eventName)
        : undefined
  );

  /** The pilot's laps in order, the hole shot first as "HS"; `lap` counts from 0 (-1 for HS). */
  const entries = $derived(
    solo
      ? [
          ...(solo.holeShotMicros !== undefined
            ? [{ label: 'HS', micros: solo.holeShotMicros, lap: -1 }]
            : []),
          ...solo.lapMicros.map((micros, i) => ({ label: `L${i + 1}`, micros, lap: i }))
        ]
      : []
  );
  const recent = $derived(entries.slice(-SHOWN));
  /** The head's unscaled height, for a bottom corner: the laps hang below it. */
  let headPx = $state(0);

  // A logo that will not load gives its place up.
  let leftBroken = $state(false);
  let rightBroken = $state(false);
  $effect(() => {
    void logos.left;
    leftBroken = false;
  });
  $effect(() => {
    void logos.right;
    rightBroken = false;
  });
</script>

<div
  class="overlay"
  data-theme={theme}
  data-state={race?.state ?? 'idle'}
  data-flying={race?.state === 'Running'}
  role="region"
  aria-label="Race overlay"
>
  <div
    class="panel"
    style={placementStyle(placement, { headPx, hangs: recent.length > 0 })}
    data-corner={placement.corner}
    data-testid="overlay-panel"
  >
    <div class="headblock" bind:clientHeight={headPx}>
      <div class="head">
        {#if logos.left && !leftBroken}
          <img class="logo" src={logos.left} alt="" onerror={() => (leftBroken = true)} />
        {:else}
          <span class="brand">Grid<span class="fpv">FPV</span></span>
        {/if}
        {#if title !== undefined}
          <span class="track" data-testid="overlay-heat">{title}</span>
        {:else}
          <span class="track" data-testid="overlay-idle">{eventName ?? 'waiting for a race…'}</span>
        {/if}
        {#if logos.right && !rightBroken}
          <img class="logo right" src={logos.right} alt="" onerror={() => (rightBroken = true)} />
        {/if}
      </div>

      <div class="clockrow">
        <span class="clock" role="timer" aria-label="Race clock" data-phase={clock.phase}
          >{clock.text}</span
        >
        {#if clock.elapsed}<span class="elapsed" data-testid="overlay-elapsed">{clock.elapsed}</span
          >{/if}
        <span class="phase" data-testid="overlay-state">{overlayStateWord(race, clock.phase)}</span>
      </div>

      {#if solo}
        <div
          class="solo"
          data-testid="overlay-solo"
          aria-label={solo.name}
          style={solo.color ? `--pilot: ${solo.color}` : ''}
        >
          {#if solo.color}<span class="swatch"></span>{/if}{solo.name}
        </div>
        <div class="stats">
          <div class="stat">
            <span class="l">Laps</span>
            <span class="n" data-stat="laps">{solo.laps}</span>
          </div>
          {#if runLength !== undefined}
            <div class="stat">
              <span class="l">Best {runLength} this race</span>
              <span class="n" data-stat="best-run"
                >{formatLapMicros(solo.bestRun?.totalMicros)}</span
              >
            </div>
          {:else}
            <div class="stat">
              <span class="l">Best lap</span>
              <span class="n" data-stat="best">{formatLapMicros(solo.bestLapMicros)}</span>
            </div>
          {/if}
        </div>
      {:else if rows.length}
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
            {#each rows as row, i (row.key)}
              <tr aria-label={row.name} style={row.color ? `--pilot: ${row.color}` : ''}>
                <td class="pos" data-stat="place">{row.place ?? i + 1}</td>
                <td class="name"
                  >{#if row.color}<span class="swatch"></span>{/if}{row.name}</td
                >
                <td class="num" data-stat="laps">{row.laps}</td>
                <td class="num" data-stat="last">{formatLapMicros(row.lastLapMicros)}</td>
                <td class="num best" data-stat={runLength === undefined ? 'best' : 'best-run'}
                  >{runLength === undefined
                    ? formatLapMicros(row.bestLapMicros)
                    : formatLapMicros(row.bestRun?.totalMicros)}</td
                >
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </div>
    {#if solo && recent.length}
      <ol class="laps" aria-label="Recent laps">
        {#each recent as e (e.label)}
          <li class="lap" class:lit={e.lap >= 0 && inRun(e.lap, solo.bestRun)}>
            <span class="ln">{e.label}</span>
            <span class="lt">{formatLapMicros(e.micros)}</span>
          </li>
        {/each}
      </ol>
    {/if}
  </div>
</div>

<style>
  /* Sizes are in pixels on purpose. They are the sizes grid-rh's overlay had, so a video
   * made with Grid lines up with one made before; the page must not follow the console's
   * type scale. The size setting scales the whole panel from these. */
  .overlay {
    --ov-text: #f7f9fb;
    --ov-muted: #788699;
    --ov-strong: #fff;
    position: relative;
    min-height: 100vh;
    background: transparent;
    color: var(--ov-text);
    font-family: var(--gf-font-family);
    font-size: 17px;
    line-height: var(--gf-line-normal);
  }
  /* Pinned to a corner, scaled about it, and its glass as solid as the placement says
     (`--panel-alpha`, inline style); at 0 the text stands alone over the video. */
  .panel {
    position: absolute;
    display: flex;
    flex-direction: column;
    min-width: 352px;
    max-width: 544px;
    border-radius: 18px;
    background: rgba(8, 10, 14, var(--panel-alpha, 0.9));
    border: 1px solid rgba(255, 255, 255, 0.14);
    backdrop-filter: blur(6px);
    text-shadow: 0 1px 2px rgba(0, 0, 0, 0.6);
  }
  /* The head is what the placement puts in the corner; the laps hang below it. */
  .headblock {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 16px 22px 18px;
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
    font-size: 17px;
  }
  .fpv {
    color: var(--gf-brand-400);
  }
  .logo {
    height: 44px;
    width: auto;
    flex: none;
  }
  .logo.right {
    margin-left: auto;
    /* A mark that is black on a see-through background (the ReconFPV crosshair is) would
       vanish on the dark panel, so the right logo sits on a grey disc. */
    background: #4f4f4f;
    border-radius: 50%;
    padding: 2px;
  }
  .track {
    font-size: 15px;
    color: var(--ov-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 416px;
  }
  .clockrow {
    display: flex;
    align-items: baseline;
    gap: 14px;
  }
  .clock {
    font-size: 57.6px;
    font-weight: 800;
    font-variant-numeric: tabular-nums;
    line-height: 1;
    letter-spacing: -0.02em;
    color: var(--ov-strong);
  }
  .clock[data-phase='staging'] {
    color: var(--gf-amber-400);
  }
  .clock[data-phase='overtime'] {
    color: var(--gf-red-400);
  }
  .elapsed {
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
    font-size: 17px;
    color: var(--ov-muted);
  }
  .phase {
    margin-left: auto;
    font-weight: 800;
    letter-spacing: 0.14em;
    font-size: 15px;
    color: var(--ov-muted);
  }
  .overlay[data-flying='true'] .phase {
    color: var(--gf-brand-400);
  }
  .solo {
    font-weight: 700;
    font-size: 21px;
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .swatch {
    display: inline-block;
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--pilot, var(--ov-muted));
    margin-right: 6px;
    vertical-align: middle;
  }
  .solo .swatch {
    margin-right: 0;
  }
  .stats {
    display: grid;
    grid-auto-flow: column;
    grid-auto-columns: 1fr;
    gap: 16px;
  }
  .stat {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .stat .l {
    font-size: 14px;
    text-transform: uppercase;
    letter-spacing: 0.12em;
    color: var(--ov-muted);
    white-space: nowrap;
  }
  .stat .n {
    font-family: var(--gf-font-mono);
    font-size: 30.4px;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    line-height: 1.1;
  }
  .laps {
    list-style: none;
    margin: -8px 0 0;
    padding: 0 22px 18px;
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
    font-size: 17px;
    font-variant-numeric: tabular-nums;
    color: var(--ov-muted);
  }
  .lap.lit {
    background: rgba(255, 255, 255, 0.12);
    color: var(--ov-strong);
    font-weight: 700;
  }
  .field {
    border-collapse: collapse;
    width: 100%;
    font-size: 17px;
  }
  .field th {
    font-size: 14px;
    text-transform: uppercase;
    letter-spacing: 0.12em;
    color: var(--ov-muted);
    font-weight: 600;
    text-align: left;
    padding: 2px 8px;
  }
  .field td {
    padding: 3px 8px;
  }
  .field .pos {
    width: 24px;
    color: var(--ov-muted);
    font-variant-numeric: tabular-nums;
  }
  .field .name {
    font-weight: 700;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 224px;
  }
  .field .num {
    text-align: right;
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .field .best {
    color: var(--ov-strong);
    font-weight: 700;
  }

  /* Dark text on a light panel, for bright video. */
  .overlay[data-theme='light'] {
    --ov-text: #0c1118;
    --ov-muted: #5d6775;
    --ov-strong: #0c1118;
  }
  .overlay[data-theme='light'] .panel {
    background: rgba(255, 255, 255, var(--panel-alpha, 0.9));
    border-color: rgba(0, 0, 0, 0.14);
    text-shadow: none;
  }
  .overlay[data-theme='light'] .clock[data-phase='staging'] {
    color: #b7791f;
  }
  .overlay[data-theme='light'] .clock[data-phase='overtime'] {
    color: #c53030;
  }
  .overlay[data-theme='light'] .fpv,
  .overlay[data-theme='light'][data-flying='true'] .phase {
    color: var(--gf-brand-600);
  }
  .overlay[data-theme='light'] .lap.lit {
    background: rgba(0, 0, 0, 0.08);
  }
</style>
