<script lang="ts">
  /**
   * RssiGraph (#55, Marshaling Slice 4) — the **signal-as-evidence** layer on top of the
   * lap-level Marshaling UI. For a RotorHazard heat that captured a trace, it renders the
   * per-competitor RSSI-vs-time graph the marshal reviews against, so they can see *why* the
   * timer called (or missed) a lap (marshaling.html §3.2).
   *
   * The graph itself stays display-only by default. The RotorHazard-style "Recalculate with
   * draggable thresholds" (marshaling.html §5) is now opt-in via the tuning props: when the
   * parent supplies `onthresholds`, the enter/exit lines grow draggable (and keyboard-nudgeable)
   * handles that emit the tuned levels back up; `tuned` overrides the drawn levels for one
   * competitor while the marshal adjusts; and `preview` draws the re-detection diff — hollow
   * dashed markers for passes the new levels would ADD, and a struck/dimmed restyle on official
   * lap markers the new levels would REMOVE. The graph still re-derives nothing and commits
   * nothing — the parent runs the detection (`redetect.ts`) and sends the commands on an
   * explicit Commit. Without the new props the behavior is exactly the old display-only graph.
   *
   * What it draws, per competitor trace ([`CompetitorTrace`]):
   *   • the **sample line** — the streaming-cadence RSSI samples placed on the source clock
   *     (sample `i` at `from + i·period_micros`);
   *   • the **enter / exit threshold lines** — horizontal, the levels the timer detected against;
   *   • the **lap markers** — verticals at each lap's gate-pass time (`Lap.at`, the closing pass's
   *     source-clock instant, which shares the trace's clock). Clicking a marker **selects that lap**
   *     in the Slice-3 action surface; the parent's lap-list selection highlights the same marker
   *     (two-way) — same commands, no re-detection.
   *
   * Fidelity caveat made visible (Slice 1): the samples are **one per RH `node_data` emit** at the
   * streaming cadence, NOT RH's dense per-tick marshal history — a legend note says so, so the
   * coarse line is never mistaken for the realtime detector's signal.
   *
   * Readable on a sunlit laptop (the field-readability bar): a dark panel, high-contrast strokes,
   * and big hit targets for the markers.
   */
  import type { CompetitorTrace, Lap, LapList, CompetitorRef } from '@gridfpv/types';
  import { formatMicros } from '@gridfpv/components';

  let {
    trace,
    laps,
    selected,
    onselect,
    onaddlap,
    canControl = false,
    nameFor = (r) => r,
    onthresholds,
    tuned,
    preview
  }: {
    /** The captured trace for the heat — one entry per competitor that produced signal facts. */
    trace: { competitors: CompetitorTrace[] };
    /** The heat's lap list (the same one the lap-list selection drives), for the markers. */
    laps: LapList | undefined;
    /** The currently-selected lap (mirrors the parent's selection), or `null`. */
    selected: { competitor: CompetitorRef; lap: Lap } | null;
    /** Emit the lap a marker click selects (two-way with the lap-list selection). */
    onselect: (competitor: CompetitorRef, lap: Lap) => void;
    /**
     * Add a brand-new lap for a competitor at a source-clock time (the cursor's race-relative
     * instant). Wired ONLY to the explicit, labelled "Add lap here" button in the cursor readout
     * below the plot — never to a bare click on the trace: stray clicks (and the click the browser
     * synthesizes when a threshold drag ends inside the svg) must not plant phantom laps.
     * Optional: when absent (or when `canControl` is false) the graph is review-only.
     */
    onaddlap?: (competitor: CompetitorRef, at: number) => void;
    /**
     * Whether the session may mutate (the role gate — read-only pilots can't add laps). When false
     * the "Add lap here" affordance is hidden, mirroring the parent's `canControl` boundary on
     * every other correction.
     */
    canControl?: boolean;
    /**
     * Resolve a competitor ref to its human-facing display name (the callsign), so the trace label
     * and aria-labels read as the pilot, not the raw ref. Defaults to identity so callers/tests that
     * don't pass a resolver keep showing the ref unchanged.
     */
    nameFor?: (ref: CompetitorRef) => string;
    /**
     * Enables live threshold tuning (the RH-style "Recalculate"): when supplied, the enter/exit
     * lines get draggable, keyboard-nudgeable handles that emit the adjusted levels. Emitted per
     * competitor — the parent owns the tuned values and feeds them back via `tuned`.
     */
    onthresholds?: (competitor: CompetitorRef, enter: number, exit: number) => void;
    /**
     * The live tuned levels for ONE competitor (two-way with the parent's tuning inputs): while
     * present, this competitor's threshold lines/handles draw at these levels instead of the
     * trace's recorded ones. Other competitors keep their recorded levels.
     */
    tuned?: { competitor: CompetitorRef; enter: number; exit: number };
    /**
     * The re-detection preview diff for ONE competitor: `added` pass times (µs) draw as hollow
     * dashed candidate markers; official lap markers whose closing pass ref is in `removedRefs`
     * restyle struck/dimmed (they would be voided on commit). Preview only — nothing commits.
     */
    preview?: { competitor: CompetitorRef; added: number[]; removedRefs: number[] };
  } = $props();

  // Plot geometry. A fixed viewBox keeps the SVG crisp at any rendered size; strokes are in
  // user units. Left/bottom gutters leave room for the axis labels.
  const W = 1000;
  const H = 220;
  const PAD_L = 8;
  const PAD_R = 8;
  const PAD_T = 10;
  const PAD_B = 18;
  const plotW = W - PAD_L - PAD_R;
  const plotH = H - PAD_T - PAD_B;

  // Cap the points we actually draw — a long heat at the streaming cadence can be thousands of
  // samples; more than one point per horizontal pixel is invisible. Downsample for the canvas
  // only (the raw `trace` keeps full fidelity); we stride-pick rather than average to keep peaks.
  const MAX_POINTS = 1200;

  /** The laps for a given competitor, in order (empty if none / no lap list). */
  function lapsFor(ref: CompetitorRef): Lap[] {
    return laps?.competitors.find((c) => c.competitor.competitor === ref)?.laps ?? [];
  }

  /**
   * A sample's source-clock time (µs). Dense traces carry the **actual** per-sample `times` (RH's
   * marshal history is bursty — clustered around each crossing — so the uniform `from + i·period`
   * grid badly compresses it); the coarse streaming path has no `times`, so its exact uniform grid
   * is used instead.
   */
  function sampleTimeOf(t: CompetitorTrace, i: number): number {
    const explicit = t.times?.[i];
    return explicit ?? (t.from ?? 0) + i * t.period_micros;
  }

  /**
   * A trace's plotted source-clock span — the first to the last sample's real instant — **widened to
   * include every lap's pass time** so every lap still lands inside the plot and gets a marker. Using
   * the real sample times (not the uniform grid) keeps the signal spanning its true duration, so it
   * lines up with the lap markers instead of compressing into the left. Falls back to a unit span.
   */
  function spanOf(t: CompetitorTrace, laps: Lap[] = []): { from: number; to: number } {
    const n = t.samples.length;
    let from = n > 0 ? sampleTimeOf(t, 0) : (t.from ?? 0);
    let to = n > 1 ? sampleTimeOf(t, n - 1) : from + 1;
    for (const lap of laps) {
      if (lap.at < from) from = lap.at;
      if (lap.at > to) to = lap.at;
    }
    return { from, to };
  }

  /**
   * The enter/exit levels a trace is DRAWN (and its crossing windows shaded) against: the live
   * `tuned` values while this competitor is being adjusted, else the trace's recorded thresholds.
   */
  function effectiveThresholds(t: CompetitorTrace): {
    enter: number | undefined;
    exit: number | undefined;
  } {
    if (tuned && tuned.competitor === t.competitor.competitor)
      return { enter: tuned.enter, exit: tuned.exit };
    return { enter: t.enter, exit: t.exit };
  }

  /** RSSI value range for a trace, padded so the thresholds and peaks aren't flush to the edge. */
  function valueRange(
    t: CompetitorTrace,
    enter: number | undefined = t.enter,
    exit: number | undefined = t.exit
  ): { lo: number; hi: number } {
    let lo = Infinity;
    let hi = -Infinity;
    for (const v of t.samples) {
      if (v < lo) lo = v;
      if (v > hi) hi = v;
    }
    for (const th of [t.enter, t.exit, enter, exit]) {
      if (th != null) {
        if (th < lo) lo = th;
        if (th > hi) hi = th;
      }
    }
    if (!isFinite(lo) || !isFinite(hi)) return { lo: 0, hi: 1 };
    if (lo === hi) {
      lo -= 1;
      hi += 1;
    }
    const pad = (hi - lo) * 0.08;
    return { lo: lo - pad, hi: hi + pad };
  }

  /** Project a source-clock time onto the plot's X (user units). */
  function xOf(time: number, span: { from: number; to: number }): number {
    const w = span.to - span.from || 1;
    return PAD_L + ((time - span.from) / w) * plotW;
  }

  /** Project an RSSI value onto the plot's Y (user units; higher value = higher on screen). */
  function yOf(value: number, range: { lo: number; hi: number }): number {
    const h = range.hi - range.lo || 1;
    return PAD_T + plotH - ((value - range.lo) / h) * plotH;
  }

  /** The downsampled sample polyline as an SVG points string. */
  function polyline(
    t: CompetitorTrace,
    span: { from: number; to: number },
    range: { lo: number; hi: number }
  ): string {
    const n = t.samples.length;
    if (n === 0) return '';
    const stride = n > MAX_POINTS ? Math.ceil(n / MAX_POINTS) : 1;
    const pts: string[] = [];
    for (let i = 0; i < n; i += stride) {
      pts.push(
        `${xOf(sampleTimeOf(t, i), span).toFixed(1)},${yOf(t.samples[i], range).toFixed(1)}`
      );
    }
    // Always include the last sample so the line reaches the end of the span.
    pts.push(
      `${xOf(sampleTimeOf(t, n - 1), span).toFixed(1)},${yOf(t.samples[n - 1], range).toFixed(1)}`
    );
    return pts.join(' ');
  }

  /**
   * The time windows the lap-detection engine "sees" a crossing, replaying the timer's own
   * enter→exit hysteresis over the captured samples: a window OPENS at the first sample that rises
   * to/above `enter` and CLOSES at the first subsequent sample that falls to/below `exit` (one window
   * per detected pass). A window still open at the trace end extends to the last sample. Empty unless
   * both levels are present. Display-only — this visualises what the detector saw (at the tuned
   * levels while adjusting), it does not re-detect or change any lap.
   */
  function crossingWindows(
    t: CompetitorTrace,
    enter: number | undefined = t.enter,
    exit: number | undefined = t.exit
  ): { from: number; to: number }[] {
    if (enter == null || exit == null) return [];
    const n = t.samples.length;
    const out: { from: number; to: number }[] = [];
    let inCrossing = false;
    let start = 0;
    for (let i = 0; i < n; i++) {
      const v = t.samples[i];
      const time = sampleTimeOf(t, i);
      if (!inCrossing) {
        if (v >= enter) {
          inCrossing = true;
          start = time;
        }
      } else if (v <= exit) {
        out.push({ from: start, to: time });
        inCrossing = false;
      }
    }
    if (inCrossing && n > 0) out.push({ from: start, to: sampleTimeOf(t, n - 1) });
    return out;
  }

  function isSelected(ref: CompetitorRef, lap: Lap): boolean {
    return selected != null && selected.competitor === ref && selected.lap.end_ref === lap.end_ref;
  }

  // ── Hover crosshair + time/RSSI readout ───────────────────────────────────────────────────────
  // As the marshal moves over a trace we show a vertical guide at the cursor and read out the exact
  // race-relative time + the RSSI sample there — the "where exactly is this?" the lap-add needs. The
  // cursor is tracked per-competitor (`ref`) so each trace owns its own crosshair.
  let hover = $state<{ ref: CompetitorRef; x: number; time: number; rssi: number } | null>(null);

  /** Invert {@link xOf}: a plot X (user units) back to a source-clock time, clamped to the span. */
  function timeAt(x: number, span: { from: number; to: number }): number {
    const w = span.to - span.from || 1;
    const frac = Math.min(1, Math.max(0, (x - PAD_L) / plotW));
    return span.from + frac * w;
  }

  /** The RSSI sample nearest a source-clock time. For a dense trace (explicit, non-uniform `times`)
   *  it scans for the closest instant; for the coarse uniform grid it indexes by `from`/`period`. */
  function rssiAt(t: CompetitorTrace, time: number): number {
    const n = t.samples.length;
    if (n === 0) return 0;
    if (t.times) {
      let best = 0;
      let bestDist = Infinity;
      for (let i = 0; i < n; i++) {
        const d = Math.abs(t.times[i] - time);
        if (d < bestDist) {
          bestDist = d;
          best = i;
        }
      }
      return t.samples[best];
    }
    const from = t.from ?? 0;
    const i = Math.min(n - 1, Math.max(0, Math.round((time - from) / (t.period_micros || 1))));
    return t.samples[i];
  }

  /**
   * Format a source-clock microsecond instant as race-relative seconds (`S.mmm`) — the same axis the
   * samples + lap markers live on. Reuses {@link formatMicros} (≥60s rolls to `M:SS.mmm`).
   */
  function formatTime(micros: number): string {
    return formatMicros(Math.round(micros));
  }

  /** Map a mouse event to the plot's user-unit X (the SVG is stretched to its rendered box). */
  function pointerX(e: MouseEvent, svg: SVGSVGElement): number {
    const rect = svg.getBoundingClientRect();
    if (rect.width === 0) return PAD_L;
    return ((e.clientX - rect.left) / rect.width) * W;
  }

  function onHover(e: MouseEvent, ct: CompetitorTrace, span: { from: number; to: number }): void {
    const svg = e.currentTarget as SVGSVGElement;
    const px = pointerX(e, svg);
    const x = Math.min(PAD_L + plotW, Math.max(PAD_L, px));
    const time = timeAt(x, span);
    hover = { ref: ct.competitor.competitor, x, time, rssi: rssiAt(ct, time) };
  }

  function clearHover(): void {
    hover = null;
  }

  // There is deliberately NO click-on-the-trace add-lap path: a bare svg click is un-labelled and
  // misfires — every threshold drag that ends inside the svg makes the browser synthesize a click
  // on it, and stray clicks land there too, each planting a phantom "Lap inserted" ruling (live
  // 2026-07-03). The ONLY add path is the explicit "Add lap here" button in the cursor readout
  // below the plot.

  // ── Draggable enter/exit threshold handles (the RH-style live tuning) ─────────────────────────
  // Only wired when `onthresholds` is supplied. A pointer drag on a threshold handle maps the
  // pointer's Y back to an RSSI level and emits BOTH levels (the dragged one replaced) so the
  // parent's tuning state stays a single (enter, exit) pair. Arrow keys nudge ±1 for keyboard
  // access. All preview-only — the graph never re-detects or sends anything itself.
  let dragging = $state<{ ref: CompetitorRef; which: 'enter' | 'exit' } | null>(null);

  /** Invert {@link yOf}: a pointer event's Y back to an RSSI level (rounded — RSSI is integral). */
  function valueFromPointer(e: PointerEvent, range: { lo: number; hi: number }): number {
    const svg = (e.currentTarget as Element).closest('svg');
    if (!svg) return range.lo;
    const rect = svg.getBoundingClientRect();
    const y = rect.height === 0 ? PAD_T : ((e.clientY - rect.top) / rect.height) * H;
    const frac = Math.min(1, Math.max(0, (PAD_T + plotH - y) / plotH));
    return Math.round(range.lo + frac * (range.hi - range.lo));
  }

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
    dragging = { ref: ct.competitor.competitor, which };
    // Keep receiving moves outside the handle while dragging (jsdom has no pointer capture).
    (e.currentTarget as Element).setPointerCapture?.(e.pointerId);
  }

  function moveThresholdDrag(
    e: PointerEvent,
    ct: CompetitorTrace,
    which: 'enter' | 'exit',
    range: { lo: number; hi: number }
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

  /** The preview diff for a competitor (empty when the preview prop targets someone else). */
  function previewFor(ref: CompetitorRef): { added: number[]; removedRefs: Set<number> } {
    if (!preview || preview.competitor !== ref) return { added: [], removedRefs: new Set() };
    return { added: preview.added, removedRefs: new Set(preview.removedRefs) };
  }
</script>

<div class="rssi-graph" aria-label="RSSI signal graph">
  <div class="legend">
    <span class="swatch sample"></span> Signal (streaming cadence)
    <span class="swatch enter"></span> Enter
    <span class="swatch exit"></span> Exit
    <span class="swatch band"></span> Detection window
    <span class="swatch marker"></span> Lap pass
    {#if preview}
      <span class="swatch preview"></span> Preview pass (uncommitted)
    {/if}
    <span class="cadence-note"
      >Streaming-cadence trace — one sample per timer emit, not RotorHazard's dense marshal history.</span
    >
  </div>

  {#each trace.competitors as ct (ct.competitor.adapter + '/' + ct.competitor.competitor)}
    {@const ref = ct.competitor.competitor}
    {@const compLaps = lapsFor(ref)}
    {@const span = spanOf(ct, compLaps)}
    {@const th = effectiveThresholds(ct)}
    {@const range = valueRange(ct, th.enter, th.exit)}
    {@const who = nameFor(ref)}
    {@const pv = previewFor(ref)}
    <figure class="trace" aria-label={`RSSI for ${who}`}>
      <figcaption>
        <span class="who">{who}</span>
        <span class="meta">
          {ct.samples.length} samples
          {#if th.enter != null}· enter {th.enter}{/if}
          {#if th.exit != null}· exit {th.exit}{/if}
        </span>
      </figcaption>
      {#if ct.samples.length === 0}
        <p class="empty">No samples captured for this node.</p>
      {:else}
        {@const isHover = hover != null && hover.ref === ref}
        <!-- The pointer handlers drive the hover crosshair/readout ONLY — the svg itself carries no
             click action (a bare click must never mutate; see the add-lap note in the script). The
             accessible, deliberate add path is the labelled "Add lap here" DOM button below the
             plot, so the SVG stays a non-interactive `role="img"` figure. -->
        <svg
          class="plot"
          viewBox={`0 0 ${W} ${H}`}
          preserveAspectRatio="none"
          role="img"
          aria-label={`RSSI trace for ${who} with ${compLaps.length} lap markers`}
          onmousemove={(e: MouseEvent) => onHover(e, ct, span)}
          onmouseleave={clearHover}
        >
          <!-- Plot frame -->
          <rect class="frame" x={PAD_L} y={PAD_T} width={plotW} height={plotH} fill="none" />

          <!-- Detection windows: one shaded vertical region per crossing the engine sees — from the
               sample that rises above `enter` to the one that falls below `exit` (the timer's
               hysteresis). Drawn behind the signal so the trace reads on top; lets the marshal see
               exactly what the lap-detection engine registered as a pass. -->
          {#each crossingWindows(ct, th.enter, th.exit) as cw (cw.from)}
            {@const x1 = xOf(cw.from, span)}
            {@const x2 = xOf(cw.to, span)}
            <rect class="crossing" x={x1} y={PAD_T} width={Math.max(1, x2 - x1)} height={plotH} />
          {/each}

          <!-- Threshold lines (horizontal). Display-only strokes here; the draggable handles
               render at the END of the svg (painted last = TOPMOST) so a dense heat's lap
               markers can never sit over the grab bands and eat the pointer — dead handles on
               lap-heavy pilots were exactly the bug (live 2026-07-03). -->
          {#if th.enter != null}
            {@const y = yOf(th.enter, range)}
            <line class="enter-line" x1={PAD_L} y1={y} x2={W - PAD_R} y2={y} />
          {/if}
          {#if th.exit != null}
            {@const y = yOf(th.exit, range)}
            <line class="exit-line" x1={PAD_L} y1={y} x2={W - PAD_R} y2={y} />
          {/if}

          <!-- The sample line -->
          <polyline class="signal" points={polyline(ct, span, range)} />

          <!-- Lap markers (vertical) at each lap's gate-pass time -->
          {#each compLaps as lap (lap.end_ref)}
            {@const x = xOf(lap.at, span)}
            <g
              class="marker"
              class:selected={isSelected(ref, lap)}
              class:removed={pv.removedRefs.has(lap.end_ref)}
              role="button"
              tabindex="0"
              aria-pressed={isSelected(ref, lap)}
              aria-label={`Lap ${lap.number} at ${formatMicros(lap.duration_micros)} — select`}
              onclick={() => onselect(ref, lap)}
              onkeydown={(e: KeyboardEvent) => {
                if (e.key === 'Enter' || e.key === ' ') {
                  e.preventDefault();
                  onselect(ref, lap);
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
               in the parent's tuning panel). -->
          {#each pv.added as t (t)}
            {@const x = xOf(t, span)}
            <g class="preview-added" aria-hidden="true">
              <line x1={x} y1={PAD_T} x2={x} y2={PAD_T + plotH} />
              <text class="label" x={x + 3} y={PAD_T + plotH - 4}>+</text>
            </g>
          {/each}

          <!-- Draggable threshold handles — LAST in paint order (topmost), so nothing (signal
               line, lap markers, preview markers) can intercept their pointer events. -->
          {#if onthresholds && th.enter != null}
            {@const y = yOf(th.enter, range)}
            <g
              class="th-handle enter"
              role="slider"
              tabindex="0"
              aria-label={`Enter threshold for ${who}`}
              aria-orientation="vertical"
              aria-valuenow={th.enter}
              aria-valuemin={Math.floor(range.lo)}
              aria-valuemax={Math.ceil(range.hi)}
              onpointerdown={(e: PointerEvent) => startThresholdDrag(e, ct, 'enter')}
              onpointermove={(e: PointerEvent) => moveThresholdDrag(e, ct, 'enter', range)}
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
          {#if onthresholds && th.exit != null}
            {@const y = yOf(th.exit, range)}
            <g
              class="th-handle exit"
              role="slider"
              tabindex="0"
              aria-label={`Exit threshold for ${who}`}
              aria-orientation="vertical"
              aria-valuenow={th.exit}
              aria-valuemin={Math.floor(range.lo)}
              aria-valuemax={Math.ceil(range.hi)}
              onpointerdown={(e: PointerEvent) => startThresholdDrag(e, ct, 'exit')}
              onpointermove={(e: PointerEvent) => moveThresholdDrag(e, ct, 'exit', range)}
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
               dark, high-contrast chip reading the exact race-relative time + RSSI there. -->
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
              <text class="readout-time" x="6" y="14">t {formatTime(hover.time)}s</text>
              <text class="readout-rssi" x="6" y="28">rssi {Math.round(hover.rssi)}</text>
            </g>
            {#if canControl && onaddlap}
              <!-- The add affordance is the real, labelled "Add lap here" button in the readout
                   BELOW the plot (clicking the trace itself never adds); this hint points at it. -->
              <text class="add-hint" x={flip ? hx - 6 : hx + 6} y={PAD_T + plotH - 6}
                >add lap ↓ below</text
              >
            {/if}
          {/if}
        </svg>
        {#if canControl && onaddlap && isHover && hover}
          <p class="cursor-readout" aria-live="polite">
            Cursor: <strong>{formatTime(hover.time)}s</strong> · RSSI
            <strong>{Math.round(hover.rssi)}</strong>
            <button
              type="button"
              class="add-here"
              onclick={() => onaddlap?.(ref, Math.round(hover!.time))}
              aria-label={`Add lap for ${who} at ${formatTime(hover.time)} seconds`}
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
