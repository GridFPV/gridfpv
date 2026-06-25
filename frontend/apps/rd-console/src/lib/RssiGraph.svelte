<script lang="ts">
  /**
   * RssiGraph (#55, Marshaling Slice 4) — the **signal-as-evidence** layer on top of the
   * lap-level Marshaling UI. For a RotorHazard heat that captured a trace, it renders the
   * per-competitor RSSI-vs-time graph the marshal reviews against, so they can see *why* the
   * timer called (or missed) a lap (marshaling.html §3.2). Display only — there is **no
   * re-detection** here: the RotorHazard-style "Recalculate" with draggable thresholds is
   * explicitly deferred (marshaling.html §5). We draw, we do not re-derive.
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
    onselect
  }: {
    /** The captured trace for the heat — one entry per competitor that produced signal facts. */
    trace: { competitors: CompetitorTrace[] };
    /** The heat's lap list (the same one the lap-list selection drives), for the markers. */
    laps: LapList | undefined;
    /** The currently-selected lap (mirrors the parent's selection), or `null`. */
    selected: { competitor: CompetitorRef; lap: Lap } | null;
    /** Emit the lap a marker click selects (two-way with the lap-list selection). */
    onselect: (competitor: CompetitorRef, lap: Lap) => void;
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

  /** A trace's source-clock span `[from, from + (n-1)·period]`; falls back to a unit span. */
  function spanOf(t: CompetitorTrace): { from: number; to: number } {
    const from = t.from ?? 0;
    const n = t.samples.length;
    const to = n > 1 ? from + (n - 1) * t.period_micros : from + 1;
    return { from, to };
  }

  /** RSSI value range for a trace, padded so the thresholds and peaks aren't flush to the edge. */
  function valueRange(t: CompetitorTrace): { lo: number; hi: number } {
    let lo = Infinity;
    let hi = -Infinity;
    for (const v of t.samples) {
      if (v < lo) lo = v;
      if (v > hi) hi = v;
    }
    for (const th of [t.enter, t.exit]) {
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
      const time = (t.from ?? 0) + i * t.period_micros;
      pts.push(`${xOf(time, span).toFixed(1)},${yOf(t.samples[i], range).toFixed(1)}`);
    }
    // Always include the last sample so the line reaches the end of the span.
    const lastTime = (t.from ?? 0) + (n - 1) * t.period_micros;
    pts.push(`${xOf(lastTime, span).toFixed(1)},${yOf(t.samples[n - 1], range).toFixed(1)}`);
    return pts.join(' ');
  }

  function isSelected(ref: CompetitorRef, lap: Lap): boolean {
    return selected != null && selected.competitor === ref && selected.lap.end_ref === lap.end_ref;
  }
</script>

<div class="rssi-graph" aria-label="RSSI signal graph">
  <div class="legend">
    <span class="swatch sample"></span> Signal (streaming cadence)
    <span class="swatch enter"></span> Enter
    <span class="swatch exit"></span> Exit
    <span class="swatch marker"></span> Lap pass
    <span class="cadence-note"
      >Streaming-cadence trace — one sample per timer emit, not RotorHazard's dense marshal history.</span
    >
  </div>

  {#each trace.competitors as ct (ct.competitor.adapter + '/' + ct.competitor.competitor)}
    {@const ref = ct.competitor.competitor}
    {@const span = spanOf(ct)}
    {@const range = valueRange(ct)}
    {@const compLaps = lapsFor(ref)}
    <figure class="trace" aria-label={`RSSI for ${ref}`}>
      <figcaption>
        <span class="who">{ref}</span>
        <span class="meta">
          {ct.samples.length} samples
          {#if ct.enter != null}· enter {ct.enter}{/if}
          {#if ct.exit != null}· exit {ct.exit}{/if}
        </span>
      </figcaption>
      {#if ct.samples.length === 0}
        <p class="empty">No samples captured for this node.</p>
      {:else}
        <svg
          class="plot"
          viewBox={`0 0 ${W} ${H}`}
          preserveAspectRatio="none"
          role="img"
          aria-label={`RSSI trace for ${ref} with ${compLaps.length} lap markers`}
        >
          <!-- Plot frame -->
          <rect class="frame" x={PAD_L} y={PAD_T} width={plotW} height={plotH} fill="none" />

          <!-- Threshold lines (horizontal) -->
          {#if ct.enter != null}
            {@const y = yOf(ct.enter, range)}
            <line class="enter-line" x1={PAD_L} y1={y} x2={W - PAD_R} y2={y} />
          {/if}
          {#if ct.exit != null}
            {@const y = yOf(ct.exit, range)}
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
        </svg>
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
  .swatch.marker {
    background: var(--gf-text-secondary);
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
  }
  .frame {
    stroke: rgba(255, 255, 255, 0.12);
    stroke-width: 1;
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
</style>
