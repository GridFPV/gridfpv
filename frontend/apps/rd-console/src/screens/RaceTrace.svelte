<script lang="ts">
  /**
   * RaceTrace — one seat's logged signal over a whole race. It stands still (it is a record,
   * not a live feed) and can be zoomed and panned: the wheel zooms where the pointer is, a
   * drag slides it, and the buttons under it do the same for a keyboard or a touch screen.
   *
   * On it:
   *
   * - the signal, thinned for the screen only (`lib/signal.ts`);
   * - the **enter** level (solid line) and the **exit** level (dashed line) the timer was using;
   * - a shaded band where the signal was in a crossing against those levels;
   * - the **start** of the race, each **counted crossing** with its lap number, a marked
   *   **restart** ("R"), and each crossing that **does not count** ("×", dotted and fainter).
   *
   * Clicking a crossing picks it (the caller highlights the same lap in its list). Clicking an
   * empty spot hands the caller the exact time there, to offer "Add a lap here".
   *
   * The plot is a picture of what the caller's lap list already says in words, so the list is
   * its text alternative; the wrapper carries a label that says what the picture shows.
   */
  import { Button } from '@gridfpv/components';
  import type { LogRef, SeatSignal, SourceTime } from '@gridfpv/types';
  import { formatOffset, type TraceMark } from '../lib/marshal.js';
  import {
    crossingSpans,
    nearestMark,
    panWindow,
    RaceTrace,
    thin,
    timeAtX,
    verticalRange,
    wholeWindow,
    xAtTime,
    zoomWindow,
    type TimeWindow
  } from '../lib/signal.js';
  import { formatLevel } from '../lib/tune.js';

  let {
    signal,
    marks,
    base,
    selected = undefined,
    pending = undefined,
    label,
    onselect,
    onpick = undefined
  }: {
    /** The seat's logged signal. */
    signal: SeatSignal;
    /** The start and every crossing, in time order. */
    marks: readonly TraceMark[];
    /** The moment times are written from (the race's start), on the timer's clock. */
    base: SourceTime;
    /** The crossing picked in the lap list, drawn highlighted. */
    selected?: LogRef | undefined;
    /** A time the marshal is about to add a lap at, drawn as a dashed marker. */
    pending?: SourceTime | undefined;
    /** What the plot shows, for someone who cannot see it. */
    label: string;
    /** A crossing was clicked. */
    onselect: (crossing: LogRef) => void;
    /** An empty spot was clicked. Left out when laps cannot be added. */
    onpick?: ((at: SourceTime) => void) | undefined;
  } = $props();

  const trace = $derived(new RaceTrace(signal.chunks));
  const unit = $derived(signal.chunks[0]?.unit ?? 'TenthsDbm');
  /** The levels in force at the end of the race. */
  const levels = $derived(signal.thresholds.at(-1));
  const levelsChanged = $derived(
    signal.thresholds.some((t) => t.enter !== levels?.enter || t.exit !== levels?.exit)
  );
  const bounds = $derived(wholeWindow(trace, marks) ?? { from: 0, to: 1 });

  /** The stretch the marshal zoomed or panned to. The whole race until they do. */
  let chosen = $state<TimeWindow | undefined>(undefined);
  const shown = $derived(chosen ? panWindow(chosen, bounds, 0) : bounds);
  const zoomed = $derived(shown.to - shown.from < bounds.to - bounds.from);

  let canvas = $state<HTMLCanvasElement>();
  /** Bumped when the canvas changes size, so it is drawn again. */
  let resized = $state(0);
  /** How close to a mark a click must be to pick it, in pixels. */
  const REACH_PX = 8;
  /** How long a gap in the signal must be before the line is broken across it. */
  const GAP_MICROS = 250_000;

  function zoom(factor: number, anchor?: number) {
    chosen = zoomWindow(shown, bounds, factor, anchor);
  }
  function pan(share: number) {
    chosen = panWindow(shown, bounds, (shown.to - shown.from) * share);
  }

  // ── Pointer: drag to pan, click to pick ────────────────────────────────────
  let dragFrom: { x: number; window: TimeWindow; moved: boolean } | undefined;

  function widthOf(element: HTMLElement): number {
    return element.getBoundingClientRect().width;
  }
  function xIn(element: HTMLElement, event: MouseEvent): number {
    return event.clientX - element.getBoundingClientRect().left;
  }

  function onmousedown(event: MouseEvent) {
    if (event.button !== 0) return;
    dragFrom = { x: event.clientX, window: shown, moved: false };
  }
  function onmousemove(event: MouseEvent) {
    if (!dragFrom || !canvas) return;
    const dx = event.clientX - dragFrom.x;
    if (Math.abs(dx) < 4 && !dragFrom.moved) return;
    dragFrom.moved = true;
    const width = widthOf(canvas);
    if (width <= 0) return;
    const span = dragFrom.window.to - dragFrom.window.from;
    chosen = panWindow(dragFrom.window, bounds, (-dx / width) * span);
  }
  function onmouseup(event: MouseEvent) {
    const drag = dragFrom;
    dragFrom = undefined;
    if (!drag || drag.moved || !canvas) return;
    const width = widthOf(canvas);
    if (width <= 0) return;
    const at = timeAtX(shown, width, xIn(canvas, event));
    const reach = (REACH_PX / width) * (shown.to - shown.from);
    const hit = nearestMark(
      marks.filter((mark) => mark.crossing !== undefined),
      at,
      reach
    );
    if (hit?.crossing !== undefined) onselect(hit.crossing);
    else onpick?.(at);
  }

  // The wheel must be able to stop the page from scrolling, so it is not a passive listener.
  $effect(() => {
    const element = canvas;
    if (!element) return;
    const onwheel = (event: WheelEvent) => {
      event.preventDefault();
      const width = widthOf(element);
      const anchor = width > 0 ? timeAtX(shown, width, xIn(element, event)) : undefined;
      zoom(event.deltaY > 0 ? 1.25 : 0.8, anchor);
    };
    element.addEventListener('wheel', onwheel, { passive: false });
    return () => element.removeEventListener('wheel', onwheel);
  });

  $effect(() => {
    const element = canvas;
    if (!element || typeof ResizeObserver !== 'function') return;
    const observer = new ResizeObserver(() => (resized += 1));
    observer.observe(element);
    return () => observer.disconnect();
  });

  // ── Drawing ────────────────────────────────────────────────────────────────
  interface Colours {
    trace: string;
    enter: string;
    exit: string;
    crossing: string;
    lap: string;
    start: string;
    faint: string;
    selected: string;
    text: string;
    font: string;
  }

  function readColours(element: HTMLElement): Colours {
    const style = getComputedStyle(element);
    const token = (name: string, fallback: string) =>
      style.getPropertyValue(name).trim() || fallback;
    return {
      trace: token('--gf-text', '#ffffff'),
      enter: token('--gf-danger', '#ff5c5c'),
      exit: token('--gf-warn', '#ffb020'),
      crossing: token('--gf-success', '#3ddc84'),
      lap: token('--gf-success', '#3ddc84'),
      start: token('--gf-info', '#4da3ff'),
      faint: token('--gf-text-muted', '#9aa4b2'),
      selected: token('--gf-accent', '#7c5cff'),
      text: token('--gf-text-muted', '#9aa4b2'),
      font: `12px ${token('--gf-font-family', 'system-ui, sans-serif')}`
    };
  }

  $effect(() => {
    const element = canvas;
    // Read everything the picture depends on, so a change to any of it draws again.
    const window = shown;
    const source = trace;
    const drawn = marks;
    const picked = selected;
    const adding = pending;
    const lines = levels;
    void resized;
    if (!element) return;
    const context = element.getContext('2d');
    if (!context) return;

    const colours = readColours(element);
    const width = element.clientWidth;
    const height = element.clientHeight;
    const ratio = globalThis.devicePixelRatio || 1;
    const pixelWidth = Math.round(width * ratio);
    const pixelHeight = Math.round(height * ratio);
    if (element.width !== pixelWidth || element.height !== pixelHeight) {
      element.width = pixelWidth;
      element.height = pixelHeight;
    }
    context.setTransform(ratio, 0, 0, ratio, 0, 0);
    context.clearRect(0, 0, width, height);
    if (width <= 0 || height <= 0) return;

    const span = window.to - window.from;
    const columns = Math.floor(width);
    const thinned = thin(source, window.from, window.to, columns);
    const range = verticalRange(unit, thinned.lo, thinned.hi, [lines?.enter, lines?.exit]);
    // The top strip is kept for the mark labels.
    const top = 16;
    const x = (time: number) => xAtTime(window, width, time);
    const y = (level: number) =>
      height - ((level - range.min) / (range.max - range.min)) * (height - top);

    if (lines) {
      context.fillStyle = colours.crossing;
      context.globalAlpha = 0.18;
      for (const band of crossingSpans(source, window.from, window.to, lines.enter, lines.exit)) {
        const left = x(band.from);
        context.fillRect(left, top, Math.max(2, x(band.to) - left), height - top);
      }
      context.globalAlpha = 1;
    }

    context.font = colours.font;
    context.textBaseline = 'alphabetic';

    const level = (value: number | undefined, colour: string, dash: number[], name: string) => {
      if (value === undefined) return;
      const at = Math.round(y(value)) + 0.5;
      context.strokeStyle = colour;
      context.lineWidth = 1.5;
      context.setLineDash(dash);
      context.beginPath();
      context.moveTo(0, at);
      context.lineTo(width, at);
      context.stroke();
      context.setLineDash([]);
      context.fillStyle = colour;
      context.textAlign = 'right';
      context.fillText(name, width - 4, Math.max(top + 12, at - 4));
    };
    level(lines?.exit, colours.exit, [6, 4], 'Exit');
    level(lines?.enter, colours.enter, [], 'Enter');

    // The signal.
    const gapColumns = span > 0 ? (GAP_MICROS / span) * columns : Infinity;
    context.strokeStyle = colours.trace;
    context.lineWidth = 1.25;
    context.lineJoin = 'round';
    context.beginPath();
    let lastColumn = -Infinity;
    for (let column = 0; column < columns; column += 1) {
      const high = thinned.max[column];
      if (Number.isNaN(high)) continue;
      const at = column + 0.5;
      if (column - lastColumn > Math.max(1, gapColumns)) context.moveTo(at, y(high));
      else context.lineTo(at, y(high));
      context.lineTo(at, y(thinned.min[column]));
      lastColumn = column;
    }
    context.stroke();

    // The marks. Each kind has its own dash pattern and its own text, so none depends on colour.
    const mark = (at: number, colour: string, dash: number[], text: string, strong: boolean) => {
      const left = Math.round(x(at)) + 0.5;
      if (left < -1 || left > width + 1) return;
      if (strong) {
        context.fillStyle = colours.selected;
        context.globalAlpha = 0.25;
        context.fillRect(left - 5, 0, 10, height);
        context.globalAlpha = 1;
      }
      context.strokeStyle = strong ? colours.selected : colour;
      context.lineWidth = strong ? 2.5 : 1.5;
      context.setLineDash(dash);
      context.beginPath();
      context.moveTo(left, top);
      context.lineTo(left, height);
      context.stroke();
      context.setLineDash([]);
      context.fillStyle = strong ? colours.selected : colour;
      context.textAlign = 'center';
      context.fillText(text, Math.min(width - 12, Math.max(12, left)), 12);
    };
    for (const entry of drawn) {
      const strong = entry.crossing !== undefined && entry.crossing === picked;
      if (entry.kind === 'start') mark(entry.at, colours.start, [], entry.text, false);
      else if (entry.kind === 'uncounted')
        mark(entry.at, colours.faint, [2, 4], entry.text, strong);
      else if (entry.kind === 'restart' || entry.kind === 'timerRestart')
        mark(entry.at, colours.exit, [6, 4], entry.text, strong);
      else mark(entry.at, colours.lap, [], entry.text, strong);
    }
    if (adding !== undefined) mark(adding, colours.selected, [3, 3], '+', false);

    // The range, and the times at each end.
    context.fillStyle = colours.text;
    context.textAlign = 'left';
    context.fillText(formatLevel(range.max, unit), 4, top + 12);
    context.fillText(`${formatOffset(window.from, base)} s`, 4, height - 4);
    context.textAlign = 'right';
    context.fillText(`${formatOffset(window.to, base)} s`, width - 4, height - 4);
  });
</script>

<div class="trace">
  <!-- The picture is named on a wrapper: a canvas cannot itself carry the image role. The lap
       list beside it does everything a click here does, for a keyboard or a screen reader. -->
  <div role="img" aria-label={label}>
    <canvas
      bind:this={canvas}
      aria-hidden="true"
      class:can-add={onpick !== undefined}
      {onmousedown}
      {onmousemove}
      {onmouseup}
      onmouseleave={() => (dragFrom = undefined)}
    ></canvas>
  </div>
  <div class="tools" role="group" aria-label="Move around the trace">
    <Button size="sm" variant="ghost" aria-label="Zoom in" onclick={() => zoom(0.5)}>+</Button>
    <Button
      size="sm"
      variant="ghost"
      aria-label="Zoom out"
      disabled={!zoomed}
      onclick={() => zoom(2)}>−</Button
    >
    <Button
      size="sm"
      variant="ghost"
      aria-label="Earlier"
      disabled={shown.from <= bounds.from}
      onclick={() => pan(-0.5)}>◀</Button
    >
    <Button
      size="sm"
      variant="ghost"
      aria-label="Later"
      disabled={shown.to >= bounds.to}
      onclick={() => pan(0.5)}>▶</Button
    >
    <Button size="sm" variant="ghost" disabled={!zoomed} onclick={() => (chosen = undefined)}>
      Whole race
    </Button>
    <span class="showing" data-testid="trace-window">
      {formatOffset(shown.from, base)} s to {formatOffset(shown.to, base)} s
    </span>
  </div>
  {#if levelsChanged}
    <p class="note">The enter and exit levels changed during this race. The last ones are drawn.</p>
  {/if}
</div>

<style>
  .trace {
    min-width: 0;
    display: grid;
    gap: var(--gf-space-2);
  }
  canvas {
    display: block;
    width: 100%;
    height: 11rem;
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    cursor: grab;
  }
  canvas.can-add {
    cursor: crosshair;
  }
  .tools {
    display: flex;
    align-items: center;
    gap: var(--gf-space-1);
    flex-wrap: wrap;
  }
  .showing,
  .note {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-xs);
  }
  .showing {
    margin-left: auto;
    font-family: var(--gf-font-mono);
  }
</style>
