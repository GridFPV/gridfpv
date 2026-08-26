<script lang="ts">
  /**
   * GateSignalStrip (#415) — the **read-only** live RSSI on Race control.
   *
   * Mid-race is exactly when an RD cannot tune and most needs to know what the gate is seeing. A
   * lap that does not register has three completely different causes — no signal at all, a craft
   * crossing under the enter threshold, or no crossing at all — and on the board they are one and
   * the same picture: a lap count that does not move. The trace, the threshold lines and the
   * crossing marks are what separate them, and today they only become visible at marshaling, after
   * the heat is over.
   *
   * ## Just the graph, and nothing that writes
   *
   * Trace, enter/exit lines, crossing marks. Deliberately **not** the Tune page's readout stack
   * (node peak / nadir / pass peak / pass nadir / pass count) — this is a glance surface on the
   * highest-stakes screen in the console, not a diagnostic bench.
   *
   * And no controls. `RssiGraph` grows draggable threshold handles only when a parent supplies
   * `onthresholds`; this one never does, so the levels are drawn and not touchable. #355 already
   * refuses threshold writes during a scored heat and this surface must not imply otherwise —
   * seeing an unfixable problem is still worth it, because it tells the RD to void and re-run
   * rather than discover the cause afterwards.
   *
   * ## Not in the leaderboard rows
   *
   * Decided by the RD: a per-row sparkline was considered and rejected. Race control is the highest
   * stakes screen and the leaderboard is what an RD actually reads during a heat, so graphs
   * embedded in it compete with the thing they are meant to support. This is a strip of its own.
   *
   * ## Collapsed by default, with the summary still live
   *
   * The strip remembers its open state per event (`collapseStore`), so an RD who wants it open all
   * meeting gets it open every heat. Collapsed it is one header row — but the header still carries a
   * live chip per gate, so "is the gate seeing anything at all?" is answered without opening
   * anything, and the plot is what you open for "how close was it?".
   *
   * The graphs themselves are only mounted while it is open: eight live SVGs re-rendering four
   * times a second behind a `hidden` region is work nobody can see.
   *
   * ## An unseen node is DEAD, not quiet
   *
   * The Director samples every node on the same pass and fills an unreported node's slot with
   * `0.0`, so a node RotorHazard has never reported arrives with a full, perfectly plottable ring
   * of zeroes — drawn, a flat trace along the floor, indistinguishable from a live node over a
   * quiet gate. That is the exact confusion this strip exists to remove, so an unseen node gets no
   * plot at all and says why instead ([`NodeSignal.seen`], #412).
   */
  import { Badge, Banner, Collapsible } from '@gridfpv/components';
  import type { CompetitorRef, TimerSignal } from '@gridfpv/types';

  import RssiGraph from './RssiGraph.svelte';
  import { deadCount, type GateGroups, type GateRow } from './gateSignal.js';
  import { nodeTraceOf } from './tuning.js';

  let {
    groups,
    signal,
    streaming,
    everLoaded,
    error,
    timerName,
    nameFor,
    seatLabel,
    open = $bindable(false),
    othersOpen = $bindable(false)
  }: {
    /** The heat's own gates and every other node the timer reports — see `gateSignal.ts`. */
    groups: GateGroups;
    /**
     * The whole snapshot, because the sample time base is **shared** across nodes rather than
     * carried per node — {@link nodeTraceOf} needs both halves to build a trace.
     */
    signal?: TimerSignal;
    /**
     * Whether a live connection is actually feeding the snapshot. `false` is **no link** (the timer
     * is not connected, or just dropped), which is a different fault from a live feed over a quiet
     * gate and has a different fix.
     */
    streaming: boolean;
    /** Whether a first snapshot has landed — separates "connecting" from "reports no nodes". */
    everLoaded: boolean;
    /** The last poll failure: the DIRECTOR did not answer, so nothing below is current. */
    error?: string;
    /** The timer's friendly name, for the no-link copy (never its id). */
    timerName: string;
    /**
     * The shared competitor/seat resolver (`buildCompetitorNames().name`), assembled ONCE by the
     * screen. A raw `node-{i}` or a bare pilot ref must never reach the strip's markup (CLAUDE.md).
     */
    nameFor: (ref: CompetitorRef) => string;
    /** The seat's own name — `Node 3 · Raceband R7` — from the same shared builder. */
    seatLabel: (node: number) => string;
    /** Open state, bindable so the screen can persist it per event. */
    open?: boolean;
    /** Open state of the secondary "other nodes" group. */
    othersOpen?: boolean;
  } = $props();

  /**
   * Whether the heat's gates could be identified at all. When they could not — a sim heat, or a
   * Flexible RotorHazard timer that has told GridFPV no channels — every node is shown as one
   * group rather than pretending the heat has no gates.
   */
  const attributed = $derived(groups.racing.length > 0);
  /** The gates shown up front; the rest are the secondary group. */
  const primary = $derived(attributed ? groups.racing : groups.others);
  const secondary = $derived(attributed ? groups.others : []);
  /** Unreported nodes hiding in the secondary group — worth saying while it is still closed. */
  const secondaryDead = $derived(deadCount(secondary));

  /**
   * A gate's headline name: the competitor it is timing where GridFPV can say which, else the
   * seat's own label. Both come from the shared resolver, so neither is ever a raw ref.
   */
  function titleOf(row: GateRow): string {
    return row.competitor === undefined ? seatLabel(row.node) : nameFor(row.competitor);
  }

  /** The seat line under the headline — suppressed when it would just repeat it. */
  function subtitleOf(row: GateRow): string | undefined {
    const seat = seatLabel(row.node);
    return titleOf(row) === seat ? undefined : seat;
  }

  /** One node's rolling window in the `{ competitors: [...] }` shape `RssiGraph` live mode takes. */
  function traceOf(row: GateRow) {
    return { competitors: signal ? [nodeTraceOf(signal, row.signal)] : [] };
  }
</script>

<Collapsible title="Gate signal" id="race-gate-signal" bind:open>
  {#snippet summary()}
    <!-- The collapsed strip still answers the first question. One chip per gate: who it is timing
         and whether the timer is hearing anything from it — so a dead node is visible without
         opening anything, and the plot is what you open for "how close was it?". -->
    <span class="gate-chips" data-testid="gate-chips">
      {#if error}
        <span class="gate-chip err">Feed lost</span>
      {:else if everLoaded && !streaming}
        <span class="gate-chip err">No link</span>
      {:else if !everLoaded}
        <span class="gate-chip quiet">Reading the timer…</span>
      {/if}
      {#each primary as row (row.node)}
        <span
          class="gate-chip"
          class:dead={row.state === 'dead'}
          class:crossing={row.state === 'crossing'}
          data-testid={`gate-chip-${row.node}`}
          data-state={row.state}
        >
          <span class="gate-dot" aria-hidden="true"></span>
          {titleOf(row)}
          {#if row.state === 'dead'}<span class="gate-chip-note">not reporting</span>{/if}
        </span>
      {/each}
      {#if secondaryDead > 0}
        <span class="gate-chip dead" data-testid="gate-chips-others-dead">
          <span class="gate-dot" aria-hidden="true"></span>
          {secondaryDead} other {secondaryDead === 1 ? 'node' : 'nodes'} not reporting
        </span>
      {/if}
    </span>
  {/snippet}

  <!-- Said once, up front, and it is the point of the surface rather than a caveat: this reads the
       gate, it never writes one. There is no threshold control anywhere below. -->
  <p class="gate-readonly" data-testid="gate-readonly">
    Read-only. Race control never writes a threshold — levels are tuned on the Tune page between
    heats. If a gate is wrong here, void the heat and re-run it.
  </p>

  {#if error}
    <!-- The FEED itself failed — the Director did not answer. Nothing below is current. -->
    <Banner tone="danger" title="Lost the timer's signal feed.">{error}</Banner>
  {:else if everLoaded && !streaming}
    <!-- The Director answered fine; nothing is feeding it. "No link", not "no signal" — the
         distinction an RD chasing a dead gate is here to make. -->
    <Banner tone="warn" title="No link to this timer.">
      The Director is answering, but nothing is arriving from {timerName}. These plots are the last
      thing it sent, not what the gates are seeing now.
    </Banner>
  {/if}

  {#if primary.length === 0}
    <p class="gate-empty">
      {everLoaded ? 'This timer reports no nodes.' : 'Reading the timer…'}
    </p>
  {:else}
    {#if !attributed && groups.others.length > 0}
      <!-- Unknown is not "nobody" (#416). GridFPV pairs a gate to a pilot from the heat's own
           channel assignment against what each node reports it is tuned to; a sim heat assigns
           none, and two nodes on one frequency is ambiguous rather than wrong. Say so instead of
           pinning a callsign to a gate we cannot prove. -->
      <p class="gate-note" data-testid="gate-unattributed">
        GridFPV can’t tell which gate is timing which pilot for this heat — the seats and the nodes
        share no channel it knows. Every node the timer reports is shown below, by seat.
      </p>
    {/if}
    <div class="gates" data-testid="gate-grid">
      {#each primary as row (row.node)}
        {@render gate(row)}
      {/each}
    </div>
  {/if}

  {#if secondary.length > 0}
    <!-- "Is node 3 even alive?" is a question an RD asks mid-event, and the snapshot carries
         unseated nodes precisely so it can be answered without leaving Race control. Secondary and
         closed by default, so it never competes with the heat's own gates — but its header states
         the unreported count while still closed, which is the answer most of the time. -->
    <div class="gate-others">
      <Collapsible
        title="Other nodes on this timer"
        id="race-gate-signal-others"
        bind:open={othersOpen}
      >
        {#snippet summary()}
          <span class="gate-others-count" data-testid="gate-others-summary">
            {secondary.length}
            {secondary.length === 1 ? 'node' : 'nodes'}{secondaryDead > 0
              ? ` · ${secondaryDead} not reporting`
              : ''}
          </span>
        {/snippet}
        <div class="gates" data-testid="gate-grid-others">
          {#each secondary as row (row.node)}
            {@render gate(row)}
          {/each}
        </div>
      </Collapsible>
    </div>
  {/if}
</Collapsible>

{#snippet gate(row: GateRow)}
  <section
    class="gate"
    class:dead={row.state === 'dead'}
    aria-label={`Gate signal for ${titleOf(row)}`}
    data-testid={`gate-${row.node}`}
  >
    <header class="gate-head">
      <h3 class="gate-name">{titleOf(row)}</h3>
      {#if subtitleOf(row)}
        <span class="gate-seat">{subtitleOf(row)}</span>
      {/if}
      {#if row.state === 'dead'}
        <Badge tone="danger" variant="outline">Not reporting</Badge>
      {:else if row.signal.crossing}
        <Badge tone="accent">In gate</Badge>
      {:else if row.signal.crossed_recently}
        <!-- The STICKY flag, which survives the Director's decimation: a fast pass between two
             samples lights this even though `crossing` was false at both of them. Reading
             `crossing` alone would miss exactly the passes an RD is squinting for — and it is the
             visual half of #397's crossing tone, so the RD hears the gate and sees it agree. -->
        <Badge tone="accent" variant="outline">Crossed</Badge>
      {/if}
    </header>

    {#if row.state === 'dead'}
      <!-- No plot, deliberately: this node's ring is all zeroes, and drawing it would be the exact
           picture of a live node over a quiet gate. -->
      <p class="gate-dead" data-testid={`gate-dead-${row.node}`} role="status">
        The timer has never heard from this node. Not a quiet gate — there is nothing there to be
        quiet.
      </p>
    {:else if open}
      <!-- Mounted only while the strip is open: `Collapsible` keeps its region in the DOM, and a
           row of live SVGs re-rendering behind `hidden` is work nobody can see.
           No `onthresholds` — that prop is what grows the draggable handles, so without it the
           enter/exit lines are drawn and untouchable. That is the read-only guarantee. -->
      <div class="gate-plot">
        <RssiGraph mode="live" trace={traceOf(row)} {nameFor} />
      </div>
    {/if}
  </section>
{/snippet}

<style>
  /* ── Collapsed header chips ─────────────────────────────────────────────── */
  .gate-chips {
    display: inline-flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--gf-space-2);
  }
  .gate-chip {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    padding: 0 var(--gf-space-2);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-pill);
    background: var(--gf-surface);
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-2xs);
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
    white-space: nowrap;
    line-height: 1.8;
  }
  .gate-chip-note {
    font-weight: var(--gf-font-weight-medium);
    opacity: 0.8;
  }
  .gate-dot {
    width: 0.5rem;
    height: 0.5rem;
    border-radius: 50%;
    background: var(--gf-success);
    flex: none;
  }
  .gate-chip.dead {
    border-color: var(--gf-danger);
    color: var(--gf-danger);
  }
  .gate-chip.dead .gate-dot {
    background: var(--gf-danger);
  }
  .gate-chip.crossing {
    border-color: var(--gf-accent);
    color: var(--gf-accent);
  }
  .gate-chip.crossing .gate-dot {
    background: var(--gf-accent);
  }
  .gate-chip.err {
    border-color: var(--gf-warn);
    color: var(--gf-warn);
  }
  .gate-chip.quiet {
    font-weight: var(--gf-font-weight-medium);
  }

  /* ── Body ───────────────────────────────────────────────────────────────── */
  .gate-readonly,
  .gate-note {
    margin: 0 0 var(--gf-space-3);
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    max-width: 72ch;
  }
  .gate-empty {
    margin: 0;
    color: var(--gf-text-faint);
    font-size: var(--gf-font-size-md);
  }

  /* One column per gate, wrapping — the same shape the Tune page lays its nodes out in, so the
     two surfaces read alike. Wide enough that the rolling window is legible at arm's length. */
  .gates {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(20rem, 1fr));
    gap: var(--gf-space-4);
  }
  .gate {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    background: var(--gf-elevated);
    min-width: 0;
  }
  .gate.dead {
    border-color: color-mix(in srgb, var(--gf-danger) 45%, var(--gf-border));
  }
  .gate-head {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: var(--gf-space-2);
  }
  .gate-name {
    margin: 0;
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-bold);
    letter-spacing: var(--gf-tracking-tight);
  }
  .gate-seat {
    font-size: var(--gf-font-size-2xs);
    color: var(--gf-text-muted);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    font-weight: var(--gf-font-weight-semibold);
  }
  .gate-dead {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .gate-plot {
    min-width: 0;
  }
  .gate-others {
    margin-top: var(--gf-space-4);
  }
  .gate-others-count {
    font-size: var(--gf-font-size-2xs);
    color: var(--gf-text-muted);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    font-weight: var(--gf-font-weight-semibold);
  }
</style>
