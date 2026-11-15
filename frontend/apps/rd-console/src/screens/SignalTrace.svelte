<script lang="ts">
  /**
   * SignalTrace — one seat's live signal, scrolling: the last ten seconds, newest on the right.
   *
   * Drawn on a canvas, again on every animation frame while the page is open. On it:
   *
   * - the signal, thinned for the screen only (`lib/signal.ts`);
   * - the **enter** level (solid line) and the **exit** level (dashed line) the timer is using
   *   right now, and its **noise floor** (dotted, fainter);
   * - a shaded band over each stretch where the signal is in a crossing against those levels.
   *
   * Colours come from the design tokens, read from the canvas's own computed style, so the
   * plot follows the theme. Each line is also named on the plot and has its own dash pattern,
   * so nothing depends on colour alone. The numbers are shown as text by the caller; the canvas
   * is a picture of them, with a label that says what it shows.
   */
  import type { SeatLevels, SignalUnit } from '@gridfpv/types';
  import {
    crossingSpans,
    thin,
    TRACE_WINDOW_MICROS,
    verticalRange,
    type SignalRing,
    type TimerClock
  } from '../lib/signal.js';
  import { formatLevel } from '../lib/tune.js';

  let {
    ring,
    clock,
    levels = undefined,
    unit,
    label
  }: {
    /** The seat's samples. */
    ring: SignalRing;
    /** Where "now" is on the timer's clock. */
    clock: TimerClock;
    /** The levels the timer is using for this seat right now. */
    levels?: SeatLevels | undefined;
    unit: SignalUnit;
    /** What the plot shows, for someone who cannot see it. */
    label: string;
  } = $props();

  let canvas = $state<HTMLCanvasElement>();
  /** Whether any signal is on screen. Says "Waiting for signal" in words when not. */
  let showing = $state(false);

  /** How long a gap in the signal must be before the line is broken across it. */
  const GAP_MICROS = 250_000;
  /** How often the theme's colours are read again. */
  const COLOUR_REFRESH_MS = 1000;

  interface Colours {
    trace: string;
    enter: string;
    exit: string;
    floor: string;
    crossing: string;
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
      floor: token('--gf-text-muted', '#9aa4b2'),
      crossing: token('--gf-success', '#3ddc84'),
      text: token('--gf-text-muted', '#9aa4b2'),
      font: `12px ${token('--gf-font-family', 'system-ui, sans-serif')}`
    };
  }

  $effect(() => {
    const element = canvas;
    if (!element || typeof requestAnimationFrame !== 'function') return;
    const context = element.getContext('2d');
    if (!context) return;

    let colours = readColours(element);
    let coloursReadAt = performance.now();
    let frame = 0;

    function draw(nowMs: number) {
      frame = requestAnimationFrame(draw);
      if (!context || !element) return;
      if (nowMs - coloursReadAt > COLOUR_REFRESH_MS) {
        colours = readColours(element);
        coloursReadAt = nowMs;
      }

      // Match the backing store to the element's size, so lines stay sharp.
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

      const to = clock.now(performance.now()) ?? ring.latest ?? 0;
      const from = to - TRACE_WINDOW_MICROS;
      const columns = Math.floor(width);
      const thinned = thin(ring, from, to, columns);
      const range = verticalRange(unit, thinned.lo, thinned.hi, [
        levels?.enter,
        levels?.exit,
        levels?.floor
      ]);
      const x = (time: number) => ((time - from) / TRACE_WINDOW_MICROS) * width;
      const y = (level: number) =>
        height - ((level - range.min) / (range.max - range.min)) * height;

      const hasSignal = thinned.lo !== undefined;
      if (hasSignal !== showing) showing = hasSignal;

      // Crossings first, so the lines and the signal sit on top of the shading.
      if (levels) {
        context.fillStyle = colours.crossing;
        context.globalAlpha = 0.22;
        for (const span of crossingSpans(ring, from, to, levels.enter, levels.exit)) {
          const left = x(span.from);
          context.fillRect(left, 0, Math.max(2, x(span.to) - left), height);
        }
        context.globalAlpha = 1;
      }

      context.font = colours.font;
      context.textBaseline = 'alphabetic';

      // The noise floor, then exit, then enter: each a different dash pattern, each named.
      const line = (
        level: number | undefined,
        colour: string,
        dash: number[],
        name: string,
        above: boolean,
        alpha: number
      ) => {
        if (level === undefined) return;
        const at = Math.round(y(level)) + 0.5;
        context.globalAlpha = alpha;
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
        const textAt = above ? Math.max(12, at - 4) : Math.min(height - 3, at + 13);
        context.fillText(name, width - 4, textAt);
        context.globalAlpha = 1;
      };
      line(levels?.floor, colours.floor, [2, 4], 'Noise floor', false, 0.8);
      line(levels?.exit, colours.exit, [6, 4], 'Exit', false, 1);
      line(levels?.enter, colours.enter, [], 'Enter', true, 1);

      // The signal: each column draws from its highest sample to its lowest, joined to the
      // column before it unless the signal stopped in between.
      const gapColumns = (GAP_MICROS / TRACE_WINDOW_MICROS) * columns;
      context.strokeStyle = colours.trace;
      context.lineWidth = 1.25;
      context.lineJoin = 'round';
      context.beginPath();
      let lastColumn = -Infinity;
      for (let column = 0; column < columns; column += 1) {
        const top = thinned.max[column];
        if (Number.isNaN(top)) continue;
        const at = column + 0.5;
        if (column - lastColumn > gapColumns) context.moveTo(at, y(top));
        else context.lineTo(at, y(top));
        context.lineTo(at, y(thinned.min[column]));
        lastColumn = column;
      }
      context.stroke();

      // The range, top and bottom, so the height of the plot means something.
      context.fillStyle = colours.text;
      context.textAlign = 'left';
      context.fillText(formatLevel(range.max, unit), 4, 13);
      context.fillText(formatLevel(range.min, unit), 4, height - 4);
    }

    frame = requestAnimationFrame(draw);
    return () => cancelAnimationFrame(frame);
  });
</script>

<div class="trace">
  <!-- The picture is named on a wrapper: a canvas cannot itself carry the image role. -->
  <div role="img" aria-label={label}>
    <canvas bind:this={canvas} aria-hidden="true"></canvas>
  </div>
  {#if !showing}
    <p class="waiting">Waiting for signal…</p>
  {/if}
</div>

<style>
  .trace {
    position: relative;
    min-width: 0;
  }
  canvas {
    display: block;
    width: 100%;
    height: 9rem;
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
  }
  .waiting {
    position: absolute;
    inset: 0;
    margin: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    pointer-events: none;
  }
</style>
