<script lang="ts">
  /**
   * RssiGraph — the RSSI-vs-time plot, in two modes.
   *
   * **`mode="review"` (#55, Marshaling Slice 4)** — the *signal-as-evidence* layer on top of the
   * lap-level Marshaling UI. For a finished RotorHazard heat that captured a trace, it renders the
   * per-competitor graph the marshal reviews against, so they can see *why* the timer called (or
   * missed) a lap (marshaling.html §3.2): a static dense trace on a race-relative axis, lap markers
   * that select in both directions with the lap list, and a zoom/pan view.
   *
   * **`mode="live"` (#355)** — the tuning view, over a timer's heartbeat: the same plot per NODE,
   * drawn against a rolling window pinned to the newest sample. No laps, no lap-relative axis, no
   * zoom (the window *is* the zoom). What it is for is the **crossing band**: a shaded region that
   * opens the moment the signal rises past `enter` and closes when it falls back past `exit`, the
   * still-open one running to now — RotorHazard's own tuning page in the same visual language as
   * marshaling's detection windows, because it is literally the same code
   * ({@link crossingWindows}).
   *
   * The modes differ in exactly four things — the axis source, whether laps exist, whether the view
   * zooms, and the chrome copy — and all four are resolved ONCE, at the boundary, into `chrome` and
   * {@link viewOf}. Nothing below that boundary asks which mode it is in.
   *
   * Shared by both, and the reason this is one component rather than two:
   *   • the **enter / exit threshold lines** and their draggable, keyboard-nudgeable handles — the
   *     RotorHazard-style live tuning (marshaling.html §5). Opt-in via `onthresholds`: when the
   *     parent supplies it the lines grow handles that emit the tuned levels back up, and `tuned`
   *     overrides the drawn levels for one competitor/node while the operator adjusts. The graph
   *     re-derives nothing and commits nothing — the parent runs the detection (`redetect.ts`) or
   *     the calibration write, on an explicit action.
   *   • the **crossing / detection band** rendering (above);
   *   • the axis + value projections, downsampling, hover crosshair and readout, and all styling.
   *
   * Review-only on top of that: the **lap markers** — verticals at each lap's gate-pass time
   * (`Lap.at`, the closing pass's source-clock instant, which shares the trace's clock). Clicking a
   * marker **selects that lap** in the Slice-3 action surface; the parent's lap-list selection
   * highlights the same marker (two-way). Plus `preview`, the re-detection diff — hollow dashed
   * markers for passes the new levels would ADD, and a struck/dimmed restyle on official lap markers
   * the new levels would REMOVE. Without the tuning props the behavior is exactly the old
   * display-only graph.
   *
   * Fidelity caveat made visible (Slice 1): in review the samples are **one per RH `node_data`
   * emit** at the streaming cadence, NOT RH's dense per-tick marshal history — a legend note says
   * so, so the coarse line is never mistaken for the realtime detector's signal.
   *
   * Readable on a sunlit laptop (the field-readability bar): a dark panel, high-contrast strokes,
   * and big hit targets for the markers.
   */
  import type { CompetitorTrace, Lap, LapList, CompetitorRef } from '@gridfpv/types';
  import { formatMicros } from '@gridfpv/components';
  import {
    W,
    H,
    PAD_L,
    PAD_R,
    PAD_T,
    plotW,
    plotH,
    crossingWindows,
    pointerX,
    polyline,
    rollingSpanOf,
    rssiAt,
    spanOf,
    timeAt,
    valueFromPointer,
    valueRange,
    xOf,
    yOf,
    type Range,
    type Span
  } from './rssiGraph.js';

  /** The live window a tuning operator wants by default: long enough to hold a whole gate pass. */
  const DEFAULT_LIVE_WINDOW_MICROS = 15_000_000;

  let {
    trace,
    laps,
    selected = null,
    onselect,
    onaddlap,
    canControl = false,
    nameFor = (r) => r,
    onthresholds,
    tuned,
    preview,
    mode = 'review',
    windowMicros = DEFAULT_LIVE_WINDOW_MICROS
  }: {
    /**
     * The traces to plot — in review, one entry per competitor that produced signal facts during
     * the heat; in live, one entry per timer NODE, holding the rolling sample buffer.
     */
    trace: { competitors: CompetitorTrace[] };
    /** REVIEW ONLY. The heat's lap list (the same one the lap-list selection drives), for the markers. */
    laps?: LapList;
    /** REVIEW ONLY. The currently-selected lap (mirrors the parent's selection), or `null`. */
    selected?: { competitor: CompetitorRef; lap: Lap } | null;
    /** REVIEW ONLY. Emit the lap a marker click selects (two-way with the lap-list selection). */
    onselect?: (competitor: CompetitorRef, lap: Lap) => void;
    /**
     * REVIEW ONLY. Add a brand-new lap for a competitor at a source-clock time (the cursor's
     * race-relative instant). Wired ONLY to the explicit, labelled "Add lap here" button in the
     * cursor readout below the plot — never to a bare click on the trace: stray clicks (and the
     * click the browser synthesizes when a threshold drag ends inside the svg) must not plant
     * phantom laps. Optional: when absent (or when `canControl` is false) the graph is review-only.
     */
    onaddlap?: (competitor: CompetitorRef, at: number) => void;
    /**
     * Whether the session may mutate (the role gate — read-only pilots can't add laps). When false
     * the "Add lap here" affordance is hidden, mirroring the parent's `canControl` boundary on
     * every other correction.
     */
    canControl?: boolean;
    /**
     * Resolve a competitor ref — or, in live mode, a node seat — to its human-facing display name,
     * so the trace label and aria-labels read as the pilot/seat, not the raw ref. Defaults to
     * identity so callers/tests that don't pass a resolver keep showing the ref unchanged.
     */
    nameFor?: (ref: CompetitorRef) => string;
    /**
     * Enables live threshold tuning (the RH-style "Recalculate", and the live tuning page's
     * calibration): when supplied, the enter/exit lines get draggable, keyboard-nudgeable handles
     * that emit the adjusted levels. Emitted per competitor/node — the parent owns the tuned values
     * and feeds them back via `tuned`.
     */
    onthresholds?: (competitor: CompetitorRef, enter: number, exit: number) => void;
    /**
     * The live tuned levels for ONE competitor/node (two-way with the parent's tuning inputs): while
     * present, this trace's threshold lines/handles — and its crossing band — draw at these levels
     * instead of the recorded ones. Other traces keep their recorded levels.
     */
    tuned?: { competitor: CompetitorRef; enter: number; exit: number };
    /**
     * REVIEW ONLY. The re-detection preview diff for ONE competitor: `added` pass times (µs) draw as
     * hollow dashed candidate markers; official lap markers whose closing pass ref is in
     * `removedRefs` restyle struck/dimmed (they would be voided on commit). Preview only — nothing
     * commits.
     */
    preview?: { competitor: CompetitorRef; added: number[]; removedRefs: number[] };
    /**
     * Which graph this is: `review` (a finished heat, marshaling) or `live` (a rolling window over
     * a timer's heartbeat, tuning). See the component doc — this is read in exactly two places.
     */
    mode?: 'review' | 'live';
    /** LIVE ONLY. How much of the recent past the rolling window shows (µs). */
    windowMicros?: number;
  } = $props();

  /** The empty preview — live mode never previews a re-detection, so it shares one instance. */
  const NO_PREVIEW: { added: number[]; removedRefs: Set<number> } = {
    added: [],
    removedRefs: new Set()
  };

  /** The laps for a given competitor, in order (empty if none / no lap list). */
  function lapsFor(ref: CompetitorRef): Lap[] {
    return laps?.competitors.find((c) => c.competitor.competitor === ref)?.laps ?? [];
  }

  /**
   * The enter/exit levels a trace is DRAWN (and its crossing windows shaded) against: the live
   * `tuned` values while this competitor/node is being adjusted, else its recorded thresholds.
   */
  function effectiveThresholds(t: CompetitorTrace): {
    enter: number | undefined;
    exit: number | undefined;
  } {
    if (tuned && tuned.competitor === t.competitor.competitor)
      return { enter: tuned.enter, exit: tuned.exit };
    return { enter: t.enter, exit: t.exit };
  }

  /** The preview diff for a competitor (empty when the preview prop targets someone else). */
  function previewFor(ref: CompetitorRef): { added: number[]; removedRefs: Set<number> } {
    if (!preview || preview.competitor !== ref) return NO_PREVIEW;
    return { added: preview.added, removedRefs: new Set(preview.removedRefs) };
  }

  // ── THE MODE BOUNDARY ─────────────────────────────────────────────────────────────────────────
  // `mode` is read here and nowhere else. Everything downstream — the markup, the pointer handlers,
  // the zoom state — reads the resolved `chrome` (mode-level copy) and `TraceView` (per-trace
  // geometry and data) instead, so no render path ever branches on which graph this is.

  /** The mode-level copy: legend wording and the empty state. */
  const chrome = $derived(
    mode === 'live'
      ? {
          signal: 'Signal (live)',
          showLaps: false,
          note: "Live signal — a rolling window of the timer's heartbeat, not a recorded trace.",
          empty: 'No signal from this node yet.'
        }
      : {
          signal: 'Signal (streaming cadence)',
          showLaps: true,
          note: "Streaming-cadence trace — one sample per timer emit, not RotorHazard's dense marshal history.",
          empty: 'No samples captured for this node.'
        }
  );

  /** Everything one trace's rendering needs, with every mode difference already resolved out. */
  type TraceView = {
    /** The competitor ref (review) or node seat (live) this trace belongs to. */
    ref: CompetitorRef;
    /** Its human-facing name — never the raw ref (project rule: friendly names only). */
    who: string;
    /** The plot's accessible label. */
    plotLabel: string;
    /** The lap markers to draw — always empty in live. */
    laps: Lap[];
    /** The whole drawable extent (zoom is clamped inside it). */
    fullSpan: Span;
    /** The extent actually drawn: the zoom window in review, the rolling window in live. */
    span: Span;
    /** Whether `span` is currently narrower than `fullSpan`. */
    zoomed: boolean;
    /** Whether the wheel / drag-pan / zoom buttons act at all. */
    zoomable: boolean;
    /** Whether the explicit "Add lap here" affordance is offered. */
    canAdd: boolean;
    /** The value extent, padded. */
    range: Range;
    /** The levels drawn against (tuned while adjusting, else recorded). */
    th: { enter: number | undefined; exit: number | undefined };
    /** The detection / crossing bands — the SAME hysteresis replay in both modes. */
    windows: Span[];
    /** The re-detection preview diff — always empty in live. */
    preview: { added: number[]; removedRefs: Set<number> };
    /** Render a source-clock instant on this mode's axis. */
    label: (micros: number) => string;
  };

  function viewOf(ct: CompetitorTrace): TraceView {
    const ref = ct.competitor.competitor;
    const who = nameFor(ref);
    const th = effectiveThresholds(ct);
    const shared = {
      ref,
      who,
      range: valueRange(ct, th.enter, th.exit),
      th,
      windows: crossingWindows(ct, th.enter, th.exit)
    };
    if (mode === 'live') {
      // A rolling window pinned to the newest sample. No laps, no preview, no add-lap — and no
      // zoom: the window IS the zoom, and panning an axis that is moving under you fights the
      // operator rather than helping them.
      const span = rollingSpanOf(ct, windowMicros);
      return {
        ...shared,
        plotLabel: `Live RSSI for ${who}`,
        laps: [],
        fullSpan: span,
        span,
        zoomed: false,
        zoomable: false,
        canAdd: false,
        preview: NO_PREVIEW,
        // Seconds behind the leading edge — the only axis that means anything while the window
        // slides. `0.0` is now.
        label: (micros) => `-${((span.to - micros) / 1_000_000).toFixed(1)}`
      };
    }
    const compLaps = lapsFor(ref);
    const fullSpan = spanOf(
      ct,
      compLaps.map((l) => l.at)
    );
    const span = viewSpanOf(ref, fullSpan);
    return {
      ...shared,
      plotLabel: `RSSI trace for ${who} with ${compLaps.length} lap markers`,
      laps: compLaps,
      fullSpan,
      span,
      zoomed: span.from > fullSpan.from || span.to < fullSpan.to,
      zoomable: true,
      canAdd: canControl && onaddlap != null,
      preview: previewFor(ref),
      // Race-relative seconds (`S.mmm`; ≥60s rolls to `M:SS.mmm`) — the axis the samples and lap
      // markers already live on.
      label: (micros) => formatMicros(Math.round(micros))
    };
  }

  function isSelected(ref: CompetitorRef, lap: Lap): boolean {
    return selected != null && selected.competitor === ref && selected.lap.end_ref === lap.end_ref;
  }

  // ── Hover crosshair + time/RSSI readout ───────────────────────────────────────────────────────
  // As the operator moves over a trace we show a vertical guide at the cursor and read out the exact
  // time + the RSSI sample there — the "where exactly is this?" the lap-add needs, and the "what is
  // the floor actually sitting at?" tuning needs. The cursor is tracked per-trace (`ref`) so each
  // plot owns its own crosshair.
  let hover = $state<{ ref: CompetitorRef; x: number; time: number; rssi: number } | null>(null);

  function onHover(e: MouseEvent, ct: CompetitorTrace, v: TraceView): void {
    const svg = e.currentTarget as SVGSVGElement;
    const px = pointerX(e, svg);
    const x = Math.min(PAD_L + plotW, Math.max(PAD_L, px));
    const time = timeAt(x, v.span);
    hover = { ref: v.ref, x, time, rssi: rssiAt(ct, time) };
  }

  function clearHover(): void {
    hover = null;
  }

  // ── Time-axis zoom & pan (review) ─────────────────────────────────────────────────────────────
  // Wheel over a trace zooms around the cursor's instant; when zoomed, dragging the plot pans and
  // the −/+/reset buttons in the caption do the same without a wheel. Pure VIEW state — zooming
  // narrows the span every projection already takes, so markers, windows, thresholds, hover and
  // the add-lap readout all follow for free. One trace zooms at a time (keyed by competitor).
  // Inert wherever `zoomable` is false (a live window has its own, moving, axis).
  const MIN_ZOOM_WINDOW_MICROS = 250_000; // never tighter than 0.25s — samples stay meaningful
  const WHEEL_ZOOM_FACTOR = 0.8; // one notch in → 80% of the window
  let zoom = $state<{ ref: CompetitorRef; from: number; to: number } | null>(null);

  /** The span a trace is DRAWN against: the zoom window (clamped inside the full span), else all. */
  function viewSpanOf(ref: CompetitorRef, full: Span): Span {
    if (!zoom || zoom.ref !== ref) return full;
    const fullW = full.to - full.from || 1;
    const w = Math.min(zoom.to - zoom.from, fullW);
    let from = Math.max(full.from, zoom.from);
    if (from + w > full.to) from = full.to - w;
    return { from, to: from + w };
  }

  /** Set (or clear) the zoom window: a window at/above the full span resets to unzoomed. */
  function setZoom(ref: CompetitorRef, full: Span, from: number, width: number): void {
    const fullW = full.to - full.from || 1;
    if (width >= fullW) {
      zoom = null;
      return;
    }
    const w = Math.max(Math.min(MIN_ZOOM_WINDOW_MICROS, fullW), width);
    let f = Math.max(full.from, Math.min(from, full.to - w));
    zoom = { ref, from: f, to: f + w };
  }

  /** Zoom by `factor` keeping `focusTime` at the same on-screen fraction (wheel-at-cursor). */
  function zoomAt(ref: CompetitorRef, full: Span, focusTime: number, factor: number): void {
    const view = viewSpanOf(ref, full);
    const curW = view.to - view.from || 1;
    const width = curW * factor;
    const frac = Math.min(1, Math.max(0, (focusTime - view.from) / curW));
    setZoom(ref, full, focusTime - frac * width, width);
  }

  /** The caption buttons: zoom in/out around the current view's center. */
  function zoomStep(v: TraceView, factor: number): void {
    if (!v.zoomable) return;
    zoomAt(v.ref, v.fullSpan, (v.span.from + v.span.to) / 2, factor);
  }

  function onWheel(e: WheelEvent, v: TraceView): void {
    if (!v.zoomable) return;
    e.preventDefault();
    const svg = e.currentTarget as SVGSVGElement;
    const x = Math.min(PAD_L + plotW, Math.max(PAD_L, pointerX(e, svg)));
    const focus = timeAt(x, v.span);
    zoomAt(v.ref, v.fullSpan, focus, e.deltaY > 0 ? 1 / WHEEL_ZOOM_FACTOR : WHEEL_ZOOM_FACTOR);
  }

  // Drag-to-pan while zoomed. Starts on the svg background (the threshold handles stop
  // propagation so their drags stay theirs). Pointer capture is DEFERRED until the pointer
  // actually moves a few units: capturing on pointerdown retargets the browser's
  // compatibility `click` to the svg, which silently broke lap-MARKER clicks whenever
  // zoomed — precisely when marshals click markers.
  const PAN_START_UNITS = 4;
  let panning = $state<{ ref: CompetitorRef; lastX: number; engaged: boolean } | null>(null);

  function startPan(e: PointerEvent, v: TraceView): void {
    if (!v.zoomable || !zoom || zoom.ref !== v.ref) return;
    const x = pointerX(e, e.currentTarget as SVGSVGElement);
    if (!Number.isFinite(x)) return; // ditto — never seed a drag from a coordinate-less event
    panning = { ref: v.ref, lastX: x, engaged: false };
  }

  function movePan(e: PointerEvent, v: TraceView): void {
    if (!panning || !zoom || zoom.ref !== panning.ref) return;
    const svg = e.currentTarget as SVGSVGElement;
    const x = pointerX(e, svg);
    const dx = x - panning.lastX;
    if (!Number.isFinite(dx)) return; // a coordinate-less synthetic event must not corrupt the view
    if (!panning.engaged) {
      if (Math.abs(dx) < PAN_START_UNITS) return; // a click in progress, not a pan
      // A real drag: NOW capture (safe — any click this gesture could produce is a drag end).
      svg.setPointerCapture?.(e.pointerId);
      panning = { ...panning, engaged: true, lastX: x };
      return;
    }
    if (dx === 0) return;
    panning = { ...panning, lastX: x };
    const view = viewSpanOf(panning.ref, v.fullSpan);
    const dt = (dx / plotW) * (view.to - view.from);
    setZoom(panning.ref, v.fullSpan, view.from - dt, view.to - view.from);
  }

  function endPan(): void {
    panning = null;
  }

  // There is deliberately NO click-on-the-trace add-lap path: a bare svg click is un-labelled and
  // misfires — every threshold drag that ends inside the svg makes the browser synthesize a click
  // on it, and stray clicks land there too, each planting a phantom "Lap inserted" ruling (live
  // 2026-07-03). The ONLY add path is the explicit "Add lap here" button in the cursor readout
  // below the plot.

  // ── Draggable enter/exit threshold handles (the RH-style tuning) ──────────────────────────────
  // Shared by both modes — the marshal re-detecting a finished heat and the RD calibrating a live
  // timer drag the same handle. Only wired when `onthresholds` is supplied. A pointer drag maps the
  // pointer's Y back to an RSSI level and emits BOTH levels (the dragged one replaced) so the
  // parent's tuning state stays a single (enter, exit) pair. Arrow keys nudge ±1 for keyboard
  // access. The graph never re-detects, calibrates or sends anything itself.
  let dragging = $state<{ ref: CompetitorRef; which: 'enter' | 'exit' } | null>(null);

  /** Emit the tuned pair with one level replaced by `value`. */
  function emitThreshold(ct: CompetitorTrace, which: 'enter' | 'exit', value: number): void {
    if (!onthresholds) return;
    const { enter, exit } = effectiveThresholds(ct);
    onthresholds(
      ct.competitor.competitor,
      which === 'enter' ? value : (enter ?? value),
      which === 'exit' ? value : (exit ?? value)
    );
  }

  function startThresholdDrag(e: PointerEvent, ct: CompetitorTrace, which: 'enter' | 'exit'): void {
    if (!onthresholds) return;
    e.stopPropagation(); // a handle drag must not also start a zoom pan on the svg beneath
    dragging = { ref: ct.competitor.competitor, which };
    // Keep receiving moves outside the handle while dragging (jsdom has no pointer capture).
    (e.currentTarget as Element).setPointerCapture?.(e.pointerId);
  }

  function moveThresholdDrag(
    e: PointerEvent,
    ct: CompetitorTrace,
    which: 'enter' | 'exit',
    range: Range
  ): void {
    if (!dragging || dragging.ref !== ct.competitor.competitor || dragging.which !== which) return;
    emitThreshold(ct, which, valueFromPointer(e, range));
  }

  function endThresholdDrag(): void {
    dragging = null;
  }

  /** Keyboard access: Arrow Up/Down nudges the focused threshold ±1 RSSI count. */
  function nudgeThreshold(e: KeyboardEvent, ct: CompetitorTrace, which: 'enter' | 'exit'): void {
    if (!onthresholds) return;
    const delta = e.key === 'ArrowUp' ? 1 : e.key === 'ArrowDown' ? -1 : 0;
    if (delta === 0) return;
    e.preventDefault();
    const current = effectiveThresholds(ct)[which];
    if (current == null) return;
    emitThreshold(ct, which, current + delta);
  }
</script>

<div class="rssi-graph" aria-label="RSSI signal graph">
  <div class="legend">
    <span class="swatch sample"></span>
    {chrome.signal}
    <span class="swatch enter"></span> Enter
    <span class="swatch exit"></span> Exit
    <span class="swatch band"></span> Detection window
    {#if chrome.showLaps}
      <span class="swatch marker"></span> Lap pass
    {/if}
    {#if preview}
      <span class="swatch preview"></span> Preview pass (uncommitted)
    {/if}
    <span class="cadence-note">{chrome.note}</span>
  </div>

  {#each trace.competitors as ct, traceIndex (ct.competitor.adapter + '/' + ct.competitor.competitor)}
    {@const v = viewOf(ct)}
    <figure class="trace" aria-label={`RSSI for ${v.who}`}>
      <figcaption>
        <span class="who">{v.who}</span>
        <span class="meta">
          {ct.samples.length} samples
          {#if v.th.enter != null}· enter {v.th.enter}{/if}
          {#if v.th.exit != null}· exit {v.th.exit}{/if}
        </span>
        {#if v.zoomable && ct.samples.length > 0}
          <span class="zoom-controls" role="group" aria-label={`Zoom for ${v.who}`}>
            <button
              type="button"
              onclick={() => zoomStep(v, WHEEL_ZOOM_FACTOR)}
              title="Zoom in (or scroll on the plot)"
              aria-label="Zoom in">+</button
            >
            <button
              type="button"
              onclick={() => zoomStep(v, 1 / WHEEL_ZOOM_FACTOR)}
              disabled={!v.zoomed}
              title="Zoom out"
              aria-label="Zoom out">−</button
            >
            <button
              type="button"
              onclick={() => (zoom = null)}
              disabled={!v.zoomed}
              title="Show the whole trace"
              aria-label="Reset zoom">Fit</button
            >
            {#if v.zoomed}
              <span class="zoom-note"
                >{v.label(v.span.from)}–{v.label(v.span.to)}s · drag to pan</span
              >
            {/if}
          </span>
        {/if}
      </figcaption>
      {#if ct.samples.length === 0}
        <p class="empty">{chrome.empty}</p>
      {:else}
        {@const isHover = hover != null && hover.ref === v.ref}
        <!-- The pointer handlers drive the hover crosshair/readout ONLY — the svg itself carries no
             click action (a bare click must never mutate; see the add-lap note in the script). The
             accessible, deliberate add path is the labelled "Add lap here" DOM button below the
             plot, so the SVG stays a non-interactive `role="img"` figure. -->
        <svg
          class="plot"
          class:panning={panning?.ref === v.ref && panning?.engaged}
          viewBox={`0 0 ${W} ${H}`}
          preserveAspectRatio="none"
          role="img"
          aria-label={v.plotLabel}
          onmousemove={(e: MouseEvent) => onHover(e, ct, v)}
          onmouseleave={clearHover}
          onwheel={(e: WheelEvent) => onWheel(e, v)}
          onpointerdown={(e: PointerEvent) => startPan(e, v)}
          onpointermove={(e: PointerEvent) => movePan(e, v)}
          onpointerup={endPan}
          onpointercancel={endPan}
        >
          <defs>
            <!-- Clip the zoomed content to the plot area (markers/windows outside the view
                 must not bleed over the axes). Keyed per trace — ids are document-global. -->
            <clipPath id={`rssi-plot-clip-${traceIndex}`}>
              <rect x={PAD_L} y={PAD_T} width={plotW} height={plotH} />
            </clipPath>
          </defs>
          <!-- Plot frame -->
          <rect class="frame" x={PAD_L} y={PAD_T} width={plotW} height={plotH} fill="none" />
          <g clip-path={`url(#rssi-plot-clip-${traceIndex})`}>
            <!-- Detection / crossing bands: one shaded vertical region per crossing — from the
               sample that rises above `enter` to the one that falls below `exit` (the timer's
               hysteresis), the still-open one running to the newest sample. Drawn behind the signal
               so the trace reads on top. In review it shows the marshal exactly what the
               lap-detection engine registered as a pass; live, it is the band that opens as the
               craft arrives and closes as it leaves — the direct answer to "do my thresholds
               actually bracket the pass?". -->
            {#each v.windows as cw (cw.from)}
              {@const x1 = xOf(cw.from, v.span)}
              {@const x2 = xOf(cw.to, v.span)}
              <rect class="crossing" x={x1} y={PAD_T} width={Math.max(1, x2 - x1)} height={plotH} />
            {/each}

            <!-- Threshold lines (horizontal). Display-only strokes here; the draggable handles
               render at the END of the svg (painted last = TOPMOST) so a dense heat's lap
               markers can never sit over the grab bands and eat the pointer — dead handles on
               lap-heavy pilots were exactly the bug (live 2026-07-03). -->
            {#if v.th.enter != null}
              {@const y = yOf(v.th.enter, v.range)}
              <line class="enter-line" x1={PAD_L} y1={y} x2={W - PAD_R} y2={y} />
            {/if}
            {#if v.th.exit != null}
              {@const y = yOf(v.th.exit, v.range)}
              <line class="exit-line" x1={PAD_L} y1={y} x2={W - PAD_R} y2={y} />
            {/if}

            <!-- The sample line -->
            <polyline class="signal" points={polyline(ct, v.span, v.range)} />

            <!-- Lap markers (vertical) at each lap's gate-pass time. Review only — `v.laps` is
               empty in live, so the whole block disappears without a mode check. -->
            {#each v.laps as lap (lap.end_ref)}
              {@const x = xOf(lap.at, v.span)}
              <g
                class="marker"
                class:selected={isSelected(v.ref, lap)}
                class:removed={v.preview.removedRefs.has(lap.end_ref)}
                role="button"
                tabindex="0"
                aria-pressed={isSelected(v.ref, lap)}
                aria-label={`Lap ${lap.number} at ${formatMicros(lap.duration_micros)} — select`}
                onclick={() => onselect?.(v.ref, lap)}
                onkeydown={(e: KeyboardEvent) => {
                  if (e.key === 'Enter' || e.key === ' ') {
                    e.preventDefault();
                    onselect?.(v.ref, lap);
                  }
                }}
              >
                <!-- Wide invisible hit target for the field (sunlit laptop, finger/trackpad). -->
                <line class="hit" x1={x} y1={PAD_T} x2={x} y2={PAD_T + plotH} />
                <line class="rule" x1={x} y1={PAD_T} x2={x} y2={PAD_T + plotH} />
                <text class="label" x={x + 3} y={PAD_T + 10}>{lap.number}</text>
              </g>
            {/each}

            <!-- PREVIEW pass markers: hollow/dashed candidates the tuned thresholds would ADD.
               Distinct from the solid official lap markers; non-interactive (commit is explicit,
               in the parent's tuning panel). Review only — empty in live. -->
            {#each v.preview.added as t (t)}
              {@const x = xOf(t, v.span)}
              <g class="preview-added" aria-hidden="true">
                <line x1={x} y1={PAD_T} x2={x} y2={PAD_T + plotH} />
                <text class="label" x={x + 3} y={PAD_T + plotH - 4}>+</text>
              </g>
            {/each}
          </g>

          <!-- Draggable threshold handles — LAST in paint order (topmost), so nothing (signal
               line, lap markers, preview markers) can intercept their pointer events. -->
          {#if onthresholds && v.th.enter != null}
            {@const y = yOf(v.th.enter, v.range)}
            <g
              class="th-handle enter"
              role="slider"
              tabindex="0"
              aria-label={`Enter threshold for ${v.who}`}
              aria-orientation="vertical"
              aria-valuenow={v.th.enter}
              aria-valuemin={Math.floor(v.range.lo)}
              aria-valuemax={Math.ceil(v.range.hi)}
              onpointerdown={(e: PointerEvent) => startThresholdDrag(e, ct, 'enter')}
              onpointermove={(e: PointerEvent) => moveThresholdDrag(e, ct, 'enter', v.range)}
              onpointerup={endThresholdDrag}
              onpointercancel={endThresholdDrag}
              onkeydown={(e: KeyboardEvent) => nudgeThreshold(e, ct, 'enter')}
            >
              <!-- Wide invisible grab band for the field (sunlit laptop, finger/trackpad). -->
              <line class="grab" x1={PAD_L} y1={y} x2={W - PAD_R} y2={y} />
              <rect class="knob" x={W - PAD_R - 30} y={y - 6} width="30" height="12" rx="3" />
              <text class="knob-label" x={W - PAD_R - 26} y={y + 3.5}>EN</text>
            </g>
          {/if}
          {#if onthresholds && v.th.exit != null}
            {@const y = yOf(v.th.exit, v.range)}
            <g
              class="th-handle exit"
              role="slider"
              tabindex="0"
              aria-label={`Exit threshold for ${v.who}`}
              aria-orientation="vertical"
              aria-valuenow={v.th.exit}
              aria-valuemin={Math.floor(v.range.lo)}
              aria-valuemax={Math.ceil(v.range.hi)}
              onpointerdown={(e: PointerEvent) => startThresholdDrag(e, ct, 'exit')}
              onpointermove={(e: PointerEvent) => moveThresholdDrag(e, ct, 'exit', v.range)}
              onpointerup={endThresholdDrag}
              onpointercancel={endThresholdDrag}
              onkeydown={(e: KeyboardEvent) => nudgeThreshold(e, ct, 'exit')}
            >
              <line class="grab" x1={PAD_L} y1={y} x2={W - PAD_R} y2={y} />
              <rect class="knob" x={W - PAD_R - 30} y={y - 6} width="30" height="12" rx="3" />
              <text class="knob-label" x={W - PAD_R - 26} y={y + 3.5}>EX</text>
            </g>
          {/if}

          <!-- Hover crosshair + time/RSSI readout: a vertical guide at the cursor, with a small
               dark, high-contrast chip reading the exact time + RSSI there. -->
          {#if isHover && hover}
            {@const hx = hover.x}
            {@const flip = hx > PAD_L + plotW * 0.62}
            <line class="crosshair" x1={hx} y1={PAD_T} x2={hx} y2={PAD_T + plotH} />
            <g
              class="readout"
              data-testid="rssi-readout"
              transform={`translate(${flip ? hx - 122 : hx + 6}, ${PAD_T + 4})`}
            >
              <rect class="readout-bg" x="0" y="0" width="116" height="34" rx="4" />
              <text class="readout-time" x="6" y="14">t {v.label(hover.time)}s</text>
              <text class="readout-rssi" x="6" y="28">rssi {Math.round(hover.rssi)}</text>
            </g>
            {#if v.canAdd}
              <!-- The add affordance is the real, labelled "Add lap here" button in the readout
                   BELOW the plot (clicking the trace itself never adds); this hint points at it. -->
              <text class="add-hint" x={flip ? hx - 6 : hx + 6} y={PAD_T + plotH - 6}
                >add lap ↓ below</text
              >
            {/if}
          {/if}
        </svg>
        {#if v.canAdd && isHover && hover}
          <p class="cursor-readout" aria-live="polite">
            Cursor: <strong>{v.label(hover.time)}s</strong> · RSSI
            <strong>{Math.round(hover.rssi)}</strong>
            <button
              type="button"
              class="add-here"
              onclick={() => onaddlap?.(v.ref, Math.round(hover!.time))}
              aria-label={`Add lap for ${v.who} at ${v.label(hover.time)} seconds`}
              >Add lap here</button
            >
          </p>
        {/if}
      {/if}
    </figure>
  {/each}
</div>

<style>
  .rssi-graph {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .legend {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--gf-space-2) var(--gf-space-3);
    font-size: var(--gf-font-size-2xs);
    color: var(--gf-text-muted);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
  }
  .swatch {
    display: inline-block;
    width: 1rem;
    height: 0.2rem;
    border-radius: 2px;
    margin-right: var(--gf-space-1);
    vertical-align: middle;
  }
  .swatch.sample {
    background: var(--gf-accent);
  }
  .swatch.enter {
    background: var(--gf-success);
  }
  .swatch.exit {
    background: var(--gf-danger);
  }
  .swatch.band {
    /* A taller swatch so the translucent fill reads as an area, not a line. */
    height: 0.7rem;
    background: rgba(120, 170, 255, 0.18);
    border: 1px solid rgba(120, 170, 255, 0.4);
  }
  .swatch.marker {
    background: var(--gf-text-secondary);
  }
  .swatch.preview {
    background: transparent;
    border: 1px dashed #ffd24a;
  }
  .cadence-note {
    flex-basis: 100%;
    text-transform: none;
    letter-spacing: normal;
    font-style: italic;
    color: var(--gf-text-faint);
  }
  .zoom-controls {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-1);
    margin-left: auto;
  }
  .zoom-controls button {
    min-width: 2rem;
    padding: 0.1rem 0.4rem;
    font-size: var(--gf-font-size-sm);
  }
  .zoom-note {
    font-size: var(--gf-font-size-2xs);
    color: var(--gf-text-muted);
  }
  .plot.panning {
    cursor: grabbing;
  }

  .trace {
    margin: 0;
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    /* Dark, high-contrast plotting surface — readable in sun. */
    background: #0c1118;
    box-shadow: var(--gf-shadow-xs);
  }
  figcaption {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--gf-space-2);
    margin-bottom: var(--gf-space-2);
  }
  .who {
    font-weight: var(--gf-font-weight-semibold);
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text);
  }
  .meta {
    font-family: var(--gf-font-mono);
    font-size: var(--gf-font-size-2xs);
    color: var(--gf-text-muted);
  }
  .empty {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    margin: 0;
  }
  .plot {
    display: block;
    width: 100%;
    height: 13rem;
    /* The crosshair cues the position READOUT only — a click on the plot never acts (adding a lap
       is the explicit, labelled button below the plot). */
    cursor: crosshair;
  }
  .frame {
    stroke: rgba(255, 255, 255, 0.12);
    stroke-width: 1;
  }
  .crossing {
    /* A detection window (enter→exit) — a light wash so it highlights the span the engine saw a
       crossing without competing with the signal trace or threshold lines drawn over it. */
    fill: rgba(120, 170, 255, 0.12);
  }
  .signal {
    fill: none;
    stroke: var(--gf-accent);
    stroke-width: 1.5;
    stroke-linejoin: round;
    stroke-linecap: round;
    vector-effect: non-scaling-stroke;
  }
  .enter-line {
    stroke: var(--gf-success);
    stroke-width: 1;
    stroke-dasharray: 6 4;
    vector-effect: non-scaling-stroke;
  }
  .exit-line {
    stroke: var(--gf-danger);
    stroke-width: 1;
    stroke-dasharray: 6 4;
    vector-effect: non-scaling-stroke;
  }
  .marker {
    cursor: pointer;
  }
  .marker .hit {
    stroke: transparent;
    stroke-width: 14;
    vector-effect: non-scaling-stroke;
  }
  .marker .rule {
    stroke: rgba(255, 255, 255, 0.45);
    stroke-width: 1;
    stroke-dasharray: 3 3;
    vector-effect: non-scaling-stroke;
  }
  .marker .label {
    fill: var(--gf-text-secondary);
    font-family: var(--gf-font-mono);
    font-size: 11px;
  }
  .marker:hover .rule {
    stroke: #fff;
  }
  .marker.selected .rule {
    stroke: var(--gf-accent);
    stroke-width: 2;
    stroke-dasharray: none;
  }
  .marker.selected .label {
    fill: var(--gf-accent);
    font-weight: var(--gf-font-weight-semibold);
  }
  .marker:focus-visible {
    outline: none;
  }
  .marker:focus-visible .rule {
    stroke: var(--gf-accent);
    stroke-width: 2;
  }
  /* An official lap marker the tuned thresholds would REMOVE: dimmed + struck (a short dash),
     so "this pass goes away on commit" reads at a glance without hiding the marker. */
  .marker.removed .rule {
    stroke: var(--gf-danger);
    opacity: 0.55;
    stroke-dasharray: 2 6;
  }
  .marker.removed .label {
    fill: var(--gf-danger);
    opacity: 0.7;
    text-decoration: line-through;
  }

  /* Draggable threshold handles (live tuning). Big grab bands — field-usable on a trackpad. */
  .th-handle {
    cursor: ns-resize;
  }
  .th-handle .grab {
    stroke: transparent;
    stroke-width: 16;
    vector-effect: non-scaling-stroke;
  }
  .th-handle .knob {
    stroke-width: 1;
    vector-effect: non-scaling-stroke;
  }
  .th-handle.enter .knob {
    fill: color-mix(in srgb, var(--gf-success) 30%, #0c1118);
    stroke: var(--gf-success);
  }
  .th-handle.exit .knob {
    fill: color-mix(in srgb, var(--gf-danger) 30%, #0c1118);
    stroke: var(--gf-danger);
  }
  .th-handle .knob-label {
    font-family: var(--gf-font-mono);
    font-size: 9px;
    fill: #fff;
    pointer-events: none;
  }
  .th-handle:focus-visible {
    outline: none;
  }
  .th-handle:focus-visible .knob {
    stroke-width: 2;
    filter: drop-shadow(0 0 3px currentColor);
  }

  /* Preview pass candidates (would be ADDED on commit): hollow dashed verticals, visually
     distinct from the solid official markers. */
  .preview-added line {
    stroke: #ffd24a;
    stroke-width: 1.5;
    stroke-dasharray: 5 4;
    vector-effect: non-scaling-stroke;
    pointer-events: none;
  }
  .preview-added .label {
    fill: #ffd24a;
    font-family: var(--gf-font-mono);
    font-size: 12px;
    font-weight: 600;
  }

  /* Hover crosshair + readout (the "where exactly is this?" guide). */
  .crosshair {
    stroke: #ffd24a;
    stroke-width: 1;
    stroke-dasharray: 2 2;
    vector-effect: non-scaling-stroke;
    pointer-events: none;
  }
  .readout {
    pointer-events: none;
  }
  .readout-bg {
    fill: rgba(6, 10, 16, 0.92);
    stroke: rgba(255, 255, 255, 0.25);
    stroke-width: 1;
    vector-effect: non-scaling-stroke;
  }
  .readout-time,
  .readout-rssi {
    font-family: var(--gf-font-mono);
    font-size: 12px;
    fill: #fff;
  }
  .readout-rssi {
    fill: var(--gf-accent);
  }
  .add-hint {
    fill: #ffd24a;
    font-family: var(--gf-font-mono);
    font-size: 11px;
    pointer-events: none;
  }
  /* The explicit, accessible "Add lap here" control under the plot (real DOM button). */
  .cursor-readout {
    margin: var(--gf-space-2) 0 0;
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    font-family: var(--gf-font-mono);
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-secondary);
  }
  .cursor-readout strong {
    color: var(--gf-text);
  }
  .add-here {
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-2xs);
    font-weight: var(--gf-font-weight-semibold);
    padding: 0.15rem var(--gf-space-3);
    border-radius: var(--gf-radius-sm);
    border: 1px solid var(--gf-accent);
    background: var(--gf-accent-soft);
    color: var(--gf-text);
    cursor: pointer;
  }
  .add-here:hover {
    background: var(--gf-accent);
    color: #061018;
  }
  .add-here:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
</style>
