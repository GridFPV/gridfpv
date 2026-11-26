<script lang="ts">
  /**
   * RaceTrace — one seat's logged signal over a whole race. It stands still (it is a record,
   * not a live feed) and can be zoomed and panned: the wheel zooms where the pointer is, a
   * drag slides it, and the buttons under it do the same for a keyboard or a touch screen.
   * Zoomed in, a scroll bar under the plot shows which part of the race is on screen and
   * slides it; a sideways scroll (a trackpad, or Shift and the wheel) slides it too.
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
   * With the race's video open, `cursor` is where the video is, drawn as a line that moves as
   * it plays. Any click also tells the caller the time clicked (`onseek`), which moves the
   * video there.
   *
   * A crossing can be dragged to a new time when the caller takes moves (`onmove`). The drag
   * is a preview only: the caller is told the time, draws nothing in the log, and hands the
   * preview back as `moving` to be drawn until the marshal confirms or cancels.
   *
   * A finger works like the mouse: a drag slides the plot or a crossing, a tap is a click.
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
    thumbOf,
    timeAtX,
    verticalRange,
    wholeWindow,
    windowAtThumb,
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
    onpick = undefined,
    cursor = undefined,
    onseek = undefined,
    moving = undefined,
    onmove = undefined
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
    /** Where the race's video is, on the timer's clock. Left out with no video open. */
    cursor?: SourceTime | undefined;
    /** The plot was clicked at this time: move the video there. */
    onseek?: ((at: SourceTime) => void) | undefined;
    /** A crossing the marshal is moving, and where to. Drawn as a dashed line until it is settled. */
    moving?: { crossing: LogRef; at: SourceTime } | undefined;
    /** A crossing was dragged to this time. Left out when crossings cannot be moved. */
    onmove?: ((crossing: LogRef, at: SourceTime) => void) | undefined;
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
  /** Names the plot for the scroll bar that slides it. */
  const plotId = $props.id();
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

  // ── Pointer: drag to pan or to move a crossing, click to pick ──────────────
  let dragFrom: { x: number; window: TimeWindow; moved: boolean; mark?: TraceMark } | undefined;

  function widthOf(element: HTMLElement): number {
    return element.getBoundingClientRect().width;
  }
  function xIn(element: HTMLElement, clientX: number): number {
    return clientX - element.getBoundingClientRect().left;
  }
  /** The crossing within reach of a spot on the plot, if there is one. */
  function markAt(element: HTMLElement, clientX: number, among: readonly TraceMark[]) {
    const width = widthOf(element);
    if (width <= 0) return undefined;
    const at = timeAtX(shown, width, xIn(element, clientX));
    const reach = (REACH_PX / width) * (shown.to - shown.from);
    return { at, hit: nearestMark(among, at, reach) };
  }
  const crossings = $derived(marks.filter((mark) => mark.crossing !== undefined));
  /** A crossing that does not count has no lap to re-time, so it is not dragged. */
  const movable = $derived(crossings.filter((mark) => mark.kind !== 'uncounted'));

  function down(clientX: number) {
    const mark = onmove && canvas ? markAt(canvas, clientX, movable)?.hit : undefined;
    dragFrom = { x: clientX, window: shown, moved: false, mark };
  }
  function move(clientX: number) {
    if (!dragFrom || !canvas) return;
    const dx = clientX - dragFrom.x;
    if (Math.abs(dx) < 4 && !dragFrom.moved) return;
    dragFrom.moved = true;
    const width = widthOf(canvas);
    if (width <= 0) return;
    if (dragFrom.mark?.crossing !== undefined) {
      const at = Math.round(timeAtX(shown, width, xIn(canvas, clientX)));
      onmove?.(dragFrom.mark.crossing, Math.min(bounds.to, Math.max(bounds.from, at)));
      return;
    }
    const span = dragFrom.window.to - dragFrom.window.from;
    chosen = panWindow(dragFrom.window, bounds, (-dx / width) * span);
  }
  function up(clientX: number) {
    const drag = dragFrom;
    dragFrom = undefined;
    if (!drag || drag.moved || !canvas) return;
    const found = markAt(canvas, clientX, crossings);
    if (!found) return;
    const { at, hit } = found;
    if (hit?.crossing !== undefined) onselect(hit.crossing);
    else onpick?.(at);
    // A click on a crossing goes to the crossing itself, not to the pixel beside it.
    onseek?.(hit?.crossing !== undefined ? hit.at : at);
  }

  /** True while the plot itself is being slid, to show the closed hand. */
  let sliding = $state(false);

  /**
   * Follow the mouse over the whole window until the button comes up, so a drag that leaves
   * the plot (or the bar) carries on instead of being dropped.
   */
  function follow(each: (clientX: number) => void, last: (clientX: number) => void) {
    const moved = (event: MouseEvent) => each(event.clientX);
    const done = (event: MouseEvent) => {
      globalThis.removeEventListener('mousemove', moved);
      globalThis.removeEventListener('mouseup', done);
      last(event.clientX);
    };
    globalThis.addEventListener('mousemove', moved);
    globalThis.addEventListener('mouseup', done);
  }

  function onmousedown(event: MouseEvent) {
    if (event.button !== 0) return;
    // Keeps the drag from selecting the text around the plot.
    event.preventDefault();
    down(event.clientX);
    follow(
      (x) => {
        move(x);
        sliding = dragFrom?.moved === true && dragFrom.mark === undefined;
      },
      (x) => {
        sliding = false;
        up(x);
      }
    );
  }
  // One finger does what the mouse does. Two fingers are left to the browser.
  function ontouchstart(event: TouchEvent) {
    if (event.touches.length === 1) down(event.touches[0].clientX);
    else dragFrom = undefined;
  }
  function ontouchmove(event: TouchEvent) {
    if (event.touches.length === 1) move(event.touches[0].clientX);
  }
  function ontouchend(event: TouchEvent) {
    const touch = event.changedTouches[0];
    if (!touch || !dragFrom) return;
    // Stops the browser from sending the same tap again as a mouse click.
    event.preventDefault();
    up(touch.clientX);
  }

  /** Where a time sits across the plot, as a share of its width. Off the plot: none. */
  function shareOf(time: number | undefined): number | undefined {
    if (time === undefined) return undefined;
    const span = shown.to - shown.from;
    if (span <= 0 || time < shown.from || time > shown.to) return undefined;
    return (time - shown.from) / span;
  }
  const movingShare = $derived(shareOf(moving?.at));
  const movingText = $derived(
    moving ? marks.find((mark) => mark.crossing === moving.crossing)?.text : undefined
  );

  /** Where the video's line sits across the plot, as a share of its width. Off the plot: none. */
  const cursorShare = $derived(shareOf(cursor));

  // ── The scroll bar ─────────────────────────────────────────────────────────
  let bar = $state<HTMLElement>();
  /** The bar's width, measured when it is laid out. */
  let barWidth = $state(0);
  const thumb = $derived(thumbOf(shown, bounds, barWidth));
  /** How far along the race the plot is, 0 to 100, for a screen reader. */
  const along = $derived.by(() => {
    const room = bounds.to - bounds.from - (shown.to - shown.from);
    return room <= 0 ? 0 : Math.round(((shown.from - bounds.from) / room) * 100);
  });
  /** Where on the handle it was grabbed, in pixels from its left edge. */
  let grab: number | undefined;

  function barDown(clientX: number) {
    if (!bar) return;
    const width = widthOf(bar);
    const x = xIn(bar, clientX);
    const at = thumbOf(shown, bounds, width);
    // A press on the handle grabs it where it was pressed; a press beside it brings the
    // middle of the handle there.
    grab = x >= at.left && x <= at.left + at.width ? x - at.left : at.width / 2;
    barMove(clientX);
  }
  function barMove(clientX: number) {
    if (grab === undefined || !bar) return;
    chosen = windowAtThumb(shown, bounds, widthOf(bar), xIn(bar, clientX) - grab);
  }
  function barMouseDown(event: MouseEvent) {
    if (event.button !== 0) return;
    event.preventDefault();
    bar?.focus();
    barDown(event.clientX);
    follow(barMove, () => (grab = undefined));
  }
  function barTouchStart(event: TouchEvent) {
    if (event.touches.length === 1) barDown(event.touches[0].clientX);
  }
  function barTouchMove(event: TouchEvent) {
    if (event.touches.length === 1) barMove(event.touches[0].clientX);
  }
  function barKey(event: KeyboardEvent) {
    const full = bounds.to - bounds.from;
    const step: Record<string, number> = {
      ArrowLeft: -0.1,
      ArrowRight: 0.1,
      PageUp: -1,
      PageDown: 1
    };
    if (event.key in step) pan(step[event.key]);
    else if (event.key === 'Home') chosen = panWindow(shown, bounds, -full);
    else if (event.key === 'End') chosen = panWindow(shown, bounds, full);
    else return;
    event.preventDefault();
  }

  $effect(() => {
    const element = bar;
    if (!element) return;
    barWidth = widthOf(element);
    if (typeof ResizeObserver !== 'function') return;
    const observer = new ResizeObserver(() => (barWidth = widthOf(element)));
    observer.observe(element);
    return () => observer.disconnect();
  });

  // The wheel must be able to stop the page from scrolling, so it is not a passive listener.
  $effect(() => {
    const element = canvas;
    if (!element) return;
    const onwheel = (event: WheelEvent) => {
      event.preventDefault();
      const width = widthOf(element);
      // A sideways scroll slides the plot: a trackpad, a tilt wheel, or Shift and the wheel
      // (which some browsers hand over as sideways already, and some do not).
      const sideways = Math.abs(event.deltaX) > Math.abs(event.deltaY);
      if (sideways || event.shiftKey) {
        const delta = sideways ? event.deltaX : event.deltaY;
        // A wheel that counts in lines or pages moves a fixed share for each click.
        const pixels = event.deltaMode === 0 ? delta : Math.sign(delta) * width * 0.1;
        if (width > 0)
          chosen = panWindow(shown, bounds, (pixels / width) * (shown.to - shown.from));
        return;
      }
      if (event.deltaY === 0) return;
      const anchor = width > 0 ? timeAtX(shown, width, xIn(element, event.clientX)) : undefined;
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
  <div class="plot" role="img" aria-label={label}>
    {#if cursorShare !== undefined}
      <!-- A line of its own over the picture: it moves every frame of the video, and the
           picture under it does not have to be drawn again. -->
      <span
        class="cursor"
        data-testid="trace-cursor"
        style:left="{cursorShare * 100}%"
        aria-hidden="true"
      ></span>
    {/if}
    {#if movingShare !== undefined}
      <span
        class="moving"
        data-testid="trace-moving"
        style:left="{movingShare * 100}%"
        aria-hidden="true"><b>{movingText ?? ''}</b></span
      >
    {/if}
    <canvas
      bind:this={canvas}
      aria-hidden="true"
      id={plotId}
      class:can-add={onpick !== undefined}
      class:sliding
      {onmousedown}
      {ontouchstart}
      {ontouchmove}
      {ontouchend}
      ontouchcancel={() => (dragFrom = undefined)}
    ></canvas>
  </div>
  {#if zoomed}
    <!-- Which part of the race is on screen. Drag the handle, or press beside it. -->
    <div
      bind:this={bar}
      class="bar"
      role="scrollbar"
      tabindex="0"
      aria-label="Slide the trace along the race"
      aria-controls={plotId}
      aria-orientation="horizontal"
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={along}
      onmousedown={barMouseDown}
      ontouchstart={barTouchStart}
      ontouchmove={barTouchMove}
      ontouchend={() => (grab = undefined)}
      ontouchcancel={() => (grab = undefined)}
      onkeydown={barKey}
    >
      <span
        class="thumb"
        data-testid="trace-thumb"
        style:left="{thumb.left}px"
        style:width="{thumb.width}px"
      ></span>
    </div>
  {/if}
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
    /* A finger sliding sideways moves the plot; up and down still scrolls the page. */
    touch-action: pan-y;
  }
  canvas.can-add {
    cursor: crosshair;
  }
  canvas.sliding {
    cursor: grabbing;
  }
  /* The scroll bar: the track is the whole race, the handle is the part on screen. */
  .bar {
    position: relative;
    height: 0.9rem;
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    border: 1px solid var(--gf-border);
    cursor: pointer;
    /* The bar takes a sideways finger itself. */
    touch-action: none;
  }
  .bar:focus-visible {
    outline: 2px solid var(--gf-accent);
    outline-offset: 2px;
  }
  .thumb {
    position: absolute;
    top: 1px;
    bottom: 1px;
    border-radius: var(--gf-radius-sm);
    background: var(--gf-text-muted);
    cursor: grab;
  }
  .bar:hover .thumb,
  .bar:focus-visible .thumb {
    background: var(--gf-text);
  }
  .plot {
    position: relative;
  }
  /* Where the video is. Clicks pass through it to the picture. */
  .cursor {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 2px;
    margin-left: -1px;
    background: var(--gf-accent);
    box-shadow: 0 0 0 1px rgba(0, 0, 0, 0.5);
    pointer-events: none;
  }
  /* Where a crossing would go. Dashed, and it carries the crossing's own label. */
  .moving {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 0;
    border-left: 2px dashed var(--gf-accent);
    margin-left: -1px;
    pointer-events: none;
  }
  .moving b {
    position: absolute;
    top: 2px;
    left: 4px;
    padding: 0 4px;
    border-radius: var(--gf-radius-sm);
    background: var(--gf-accent);
    color: var(--gf-on-accent, #fff);
    font-size: var(--gf-font-size-xs);
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
