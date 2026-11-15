<script lang="ts">
  /**
   * CaptureTrace — a recorded signal with the enter and exit levels drawn as lines that can
   * be dragged, and the passes those levels would make shaded underneath.
   *
   * Pure SVG. The signal is thinned for the screen only (`thin` in `lib/signal.ts`): each
   * column draws the lowest and highest sample in it, so a peak is never lost. Moving a
   * line only moves the line; the caller works out the passes again and hands them back.
   *
   * A line dragged past the other one pushes it along, so exit is always below enter.
   *
   * Nothing here depends on colour alone: each line is named on the plot, the enter line is
   * solid and the exit line dashed, and both are sliders a keyboard can move (arrow keys;
   * Shift for bigger steps).
   */
  import type { SignalUnit } from '@gridfpv/types';
  import { levelGap, moveLevel, type Pass, type Recording } from '../lib/capture.js';
  import { thin, verticalRange } from '../lib/signal.js';
  import { formatLevel } from '../lib/tune.js';

  let {
    recording,
    version,
    enter = $bindable(),
    exit = $bindable(),
    passes,
    unit,
    readonly = false,
    height = 280
  }: {
    recording: Recording;
    /** Goes up each time the recording grows. The recording itself is not reactive. */
    version: number;
    /** The enter level, in the timer's unit (tenths of a dB). */
    enter: number;
    exit: number;
    /** The passes these levels make of the recording. */
    passes: readonly Pass[];
    unit: SignalUnit;
    /** While recording: the lines do not move and the newest signal stays in view. */
    readonly?: boolean;
    height?: number;
  } = $props();

  const W = 1000; // viewBox units; the SVG scales to its container
  const PAD = { top: 18, right: 12, bottom: 24, left: 64 };
  const COLUMNS = 500;
  const SECOND = 1_000_000;

  const t0 = $derived((void version, recording.from ?? 0));
  const span = $derived((void version, Math.max(recording.spanMicros, SECOND / 1000)));

  // The plot shows a chosen number of seconds and scrolls when the recording is longer;
  // "All" fits the whole recording. A short recording stretched over the whole width reads
  // as jagged, so the default is a fixed 30 seconds.
  const SCALES: readonly { label: string; seconds: number | null }[] = [
    { label: '10 s', seconds: 10 },
    { label: '30 s', seconds: 30 },
    { label: '60 s', seconds: 60 },
    { label: 'All', seconds: null }
  ];
  let scaleSeconds = $state<number | null>(30);
  let offset = $state(0);
  const viewSpan = $derived(scaleSeconds === null ? span : scaleSeconds * SECOND);
  const maxOffset = $derived(Math.max(0, span - viewSpan));
  const viewStart = $derived(t0 + Math.min(Math.max(0, offset), maxOffset));
  const viewEnd = $derived(viewStart + viewSpan);
  // While recording, keep the newest samples in view.
  $effect(() => {
    if (readonly) offset = maxOffset;
  });

  // The vertical range follows the recording and both lines, with a little room. While a
  // line is dragged it is frozen: if it followed the line, moving the line would rescale
  // the plot, which would move the line under the pointer.
  const liveRange = $derived.by(() => {
    void version;
    const found = recording.range();
    return verticalRange(unit, found?.lo, found?.hi, [enter, exit]);
  });
  let frozen = $state<{ min: number; max: number } | undefined>(undefined);
  const range = $derived(frozen ?? liveRange);

  const plotW = W - PAD.left - PAD.right;
  const plotH = $derived(height - PAD.top - PAD.bottom);
  const x = (t: number): number => PAD.left + ((t - viewStart) / viewSpan) * plotW;
  const y = (v: number): number =>
    PAD.top + (1 - (v - range.min) / (range.max - range.min)) * plotH;
  const levelAtY = (py: number): number =>
    range.max - ((py - PAD.top) / plotH) * (range.max - range.min);
  const clampX = (px: number): number => Math.max(PAD.left, Math.min(W - PAD.right, px));
  const inView = (from: number, to: number): boolean => to >= viewStart && from <= viewEnd;

  /** The signal as one closed band: along the highs, then back along the lows. */
  const band = $derived.by(() => {
    void version;
    const thinned = thin(recording, viewStart, viewEnd, COLUMNS);
    const top: string[] = [];
    const bottom: string[] = [];
    for (let column = 0; column < COLUMNS; column += 1) {
      if (Number.isNaN(thinned.max[column])) continue;
      const px = (PAD.left + ((column + 0.5) / COLUMNS) * plotW).toFixed(1);
      top.push(`${px},${y(thinned.max[column]).toFixed(1)}`);
      bottom.push(`${px},${y(thinned.min[column]).toFixed(1)}`);
    }
    if (top.length === 0) return '';
    return `M${top.join(' L')} L${bottom.reverse().join(' L')} Z`;
  });

  const yTicks = $derived.by(() => {
    const tall = range.max - range.min;
    const step = unit === 'Counts' ? (tall > 120 ? 40 : 20) : tall > 400 ? 100 : 50;
    const ticks: number[] = [];
    for (let v = Math.ceil(range.min / step) * step; v <= range.max; v += step) ticks.push(v);
    return ticks;
  });
  const secondsIn = (t: number): string => `${((t - t0) / SECOND).toFixed(1)} s`;

  // ── Dragging a line ────────────────────────────────────────────────────────
  let svg = $state<SVGSVGElement>();
  let dragging = $state<'enter' | 'exit' | undefined>(undefined);

  function move(which: 'enter' | 'exit', value: number) {
    const next = moveLevel(which, Math.round(value), enter, exit, levelGap(unit));
    enter = next.enter;
    exit = next.exit;
  }
  function startDrag(which: 'enter' | 'exit', e: PointerEvent) {
    if (readonly) return;
    dragging = which;
    frozen = { ...liveRange };
    (e.currentTarget as Element).setPointerCapture(e.pointerId);
  }
  function drag(e: PointerEvent) {
    if (!dragging || !svg) return;
    const rect = svg.getBoundingClientRect();
    const py = ((e.clientY - rect.top) / rect.height) * height;
    // Kept inside the frozen plot. Let go and drag again to go further: the range grows.
    move(dragging, Math.max(range.min + 1, Math.min(range.max - 1, levelAtY(py))));
  }
  function endDrag() {
    dragging = undefined;
    frozen = undefined;
  }
  function nudge(which: 'enter' | 'exit', e: KeyboardEvent) {
    if (readonly) return;
    const step = (e.shiftKey ? 5 : 1) * (unit === 'Counts' ? 1 : 10);
    const delta = e.key === 'ArrowUp' ? step : e.key === 'ArrowDown' ? -step : 0;
    if (delta === 0) return;
    e.preventDefault();
    move(which, (which === 'enter' ? enter : exit) + delta);
  }
</script>

<div class="trace" class:dragging={dragging !== undefined}>
  <div class="toolbar">
    <span class="muted">Show</span>
    {#each SCALES as scale (scale.label)}
      <button
        type="button"
        class="scale"
        aria-pressed={scaleSeconds === scale.seconds}
        onclick={() => (scaleSeconds = scale.seconds)}>{scale.label}</button
      >
    {/each}
    {#if maxOffset > 0}
      <input
        class="scroll"
        type="range"
        min="0"
        max={maxOffset}
        step={SECOND / 10}
        bind:value={offset}
        disabled={readonly}
        aria-label="Scroll the recording"
      />
    {/if}
  </div>
  <svg
    bind:this={svg}
    viewBox="0 0 {W} {height}"
    preserveAspectRatio="none"
    role="img"
    aria-label="The recorded signal, with the enter and exit levels"
    onpointermove={drag}
    onpointerup={endDrag}
    onpointercancel={endDrag}
  >
    {#each yTicks as tick (tick)}
      <line class="grid" x1={PAD.left} x2={W - PAD.right} y1={y(tick)} y2={y(tick)} />
      <text class="axis" x={PAD.left - 6} y={y(tick) + 4} text-anchor="end"
        >{formatLevel(tick, unit)}</text
      >
    {/each}
    <text class="axis" x={PAD.left} y={height - 6}>{secondsIn(viewStart)}</text>
    <text class="axis" x={W - PAD.right} y={height - 6} text-anchor="end">{secondsIn(viewEnd)}</text
    >

    <!-- The passes these levels would make: a band each, with the instant the lap is timed at. -->
    {#each passes as pass, index (index)}
      {#if inView(pass.start, pass.end)}
        <rect
          class="pass"
          class:open={pass.open}
          x={clampX(x(pass.start))}
          y={PAD.top}
          width={Math.max(clampX(x(pass.end)) - clampX(x(pass.start)), 2)}
          height={plotH}
        />
        {#if inView(pass.at, pass.at)}
          <line
            class="pass-at"
            x1={x(pass.at)}
            x2={x(pass.at)}
            y1={PAD.top}
            y2={height - PAD.bottom}
          />
        {/if}
      {/if}
    {/each}

    {#if band}<path class="signal" d={band} />{/if}

    <line class="enter" x1={PAD.left} x2={W - PAD.right} y1={y(enter)} y2={y(enter)} />
    <line class="exit" x1={PAD.left} x2={W - PAD.right} y1={y(exit)} y2={y(exit)} />
    <text class="label enter" x={W - PAD.right - 4} y={y(enter) - 5} text-anchor="end"
      >Enter {formatLevel(enter, unit)}</text
    >
    <text class="label exit" x={W - PAD.right - 4} y={y(exit) + 14} text-anchor="end"
      >Exit {formatLevel(exit, unit)}</text
    >
    {#if !readonly}
      <!-- Wide unseen strips, so a line can be caught with a finger. Also keyboard sliders. -->
      <rect
        class="handle"
        role="slider"
        tabindex="0"
        aria-label="Enter level"
        aria-valuenow={enter}
        aria-valuetext={formatLevel(enter, unit)}
        aria-valuemin={exit + 1}
        aria-valuemax={range.max}
        x={PAD.left}
        y={y(enter) - 12}
        width={plotW}
        height={24}
        onpointerdown={(e) => startDrag('enter', e)}
        onkeydown={(e) => nudge('enter', e)}
      />
      <rect
        class="handle"
        role="slider"
        tabindex="0"
        aria-label="Exit level"
        aria-valuenow={exit}
        aria-valuetext={formatLevel(exit, unit)}
        aria-valuemin={range.min}
        aria-valuemax={enter - 1}
        x={PAD.left}
        y={y(exit) - 12}
        width={plotW}
        height={24}
        onpointerdown={(e) => startDrag('exit', e)}
        onkeydown={(e) => nudge('exit', e)}
      />
    {/if}
  </svg>
</div>

<style>
  .trace {
    width: 100%;
    touch-action: none;
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: var(--gf-space-1);
    margin-bottom: var(--gf-space-2);
    font-size: var(--gf-font-size-sm);
  }
  .muted {
    color: var(--gf-text-muted);
    margin-right: var(--gf-space-1);
  }
  .scale {
    font: inherit;
    font-size: var(--gf-font-size-xs);
    padding: 2px 8px;
    border-radius: var(--gf-radius-md);
    border: 1px solid var(--gf-border);
    background: transparent;
    color: var(--gf-text-muted);
    cursor: pointer;
  }
  .scale[aria-pressed='true'] {
    color: var(--gf-text);
    background: var(--gf-surface);
    border-color: var(--gf-accent);
  }
  .scale:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
  .scroll {
    flex: 1;
    min-width: 120px;
    margin-left: var(--gf-space-2);
  }
  svg {
    display: block;
    width: 100%;
    height: auto;
    background: var(--gf-surface-sunken);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
  }
  .grid {
    stroke: var(--gf-border);
    stroke-width: 1;
    vector-effect: non-scaling-stroke;
  }
  .axis {
    fill: var(--gf-text-muted);
    font-size: 12px;
    font-family: var(--gf-font-family);
  }
  .signal {
    fill: color-mix(in srgb, var(--gf-accent) 55%, transparent);
    stroke: var(--gf-accent);
    stroke-width: 1;
    stroke-linejoin: round;
    vector-effect: non-scaling-stroke;
  }
  .pass {
    fill: color-mix(in srgb, var(--gf-success) 18%, transparent);
  }
  .pass.open {
    fill: color-mix(in srgb, var(--gf-warn) 18%, transparent);
  }
  .pass-at {
    stroke: var(--gf-success);
    stroke-width: 1;
    stroke-dasharray: 4 3;
    vector-effect: non-scaling-stroke;
  }
  .enter {
    stroke: var(--gf-danger);
    stroke-width: 1.5;
    vector-effect: non-scaling-stroke;
  }
  .exit {
    stroke: var(--gf-warn);
    stroke-width: 1.5;
    stroke-dasharray: 8 4;
    vector-effect: non-scaling-stroke;
  }
  .label {
    font-size: 12px;
    font-family: var(--gf-font-family);
    font-weight: 600;
  }
  .label.enter {
    fill: var(--gf-danger);
  }
  .label.exit {
    fill: var(--gf-warn);
  }
  .handle {
    fill: transparent;
    cursor: ns-resize;
  }
  .handle:focus-visible {
    outline: none;
    fill: color-mix(in srgb, var(--gf-accent) 12%, transparent);
  }
  .dragging {
    cursor: ns-resize;
    user-select: none;
  }
</style>
