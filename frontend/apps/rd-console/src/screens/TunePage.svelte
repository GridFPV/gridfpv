<script lang="ts">
  /**
   * TunePage — the per-timer gate **tuning** surface (#355, slice 2b).
   *
   * The page an RD opens when a heat recorded zero laps and nobody can tell whether the gate is
   * mistuned, miswired, or simply was not flown through. Per node it shows the live RSSI, the
   * enter/exit thresholds, and — the thing that actually answers the question — the **crossing
   * band**: a shaded region that opens the moment the signal rises past `enter` and closes when it
   * falls back past `exit`. That is what says whether your thresholds bracket the pass; a bare RSSI
   * number cannot.
   *
   * Modelled on RotorHazard's own tuning page, which the RD already reads: **one column per node**,
   * the live plot on top, the two thresholds under it, the six `node_data` readouts at the bottom.
   * A stacked variant is a layout toggle over the same markup — a look to compare, not a fork.
   *
   * ## A page, not a modal — and why the URL matters
   *
   * Tuning is a two-location activity: set a threshold, walk to the gate, wave a quad through, walk
   * back, read the graph. The console is served over the LAN, so `#/timers/<id>/tune` can be opened
   * **on a phone at the gate** — watching the band open and close in your hand instead of walking
   * back for every adjustment. A modal has no URL, so that case is simply unavailable. It also
   * survives a reload, which mid-tuning on a race day is the worst moment to lose.
   *
   * ## One value, three editors
   *
   * Per (node, threshold) there is exactly **one** number, held in {@link levels}. Three controls
   * edit it — the numeric box (a value you know), the slider (one you are feeling out), and the
   * draggable handle on the graph (one you can see) — and all three *read* it. None of them binds
   * to another: three controls syncing pairwise is how you get a feedback loop or a drifting third
   * view. Every edit funnels through `tuning.ts`'s `clampLevel`, so the clamp and the rounding
   * happen **once, at the state** — otherwise the box holds `90.4`, the slider sits at `90`, and
   * the graph draws a third position.
   *
   * ## No Apply button
   *
   * An adjustment goes to the timer as soon as the interaction ends. A commit button inserts a step
   * into a loop the RD runs dozens of times, and one they will forget — leaving them testing
   * against a threshold the timer never received.
   *
   * The cost of that is **write cadence**: a drag emits dozens of values a second and every write
   * also costs a readback (`set_enter_at_level` does not echo — verified on RH 4.3.0). So:
   * **draw continuously, write once on interaction end.** The crossing band responds under the RD's
   * hand with zero writes, because the band is drawn client-side from the pending value; the
   * timer's real threshold only matters when a quad crosses, and by then the drag has ended and the
   * write has landed. Mid-drag throttled writes are deliberately NOT done — they buy nothing
   * visible and cost a write-plus-readback per tick on the socket that also carries lap ingest.
   *
   * What replaces the commit step is the **confirmation**: each threshold shows `Adjusting` →
   * `Sending…` → `On timer`, and says so loudly if the readback disagrees or never arrives. A
   * silent failure means tuning against a value the hardware never took (#403's failure class).
   *
   * ## Gates and lifetimes
   *
   * Every adjustment is a write, so the practice-only gate (`writeGate`) is checked **per write**,
   * not once at load: a heat that goes `Running` while the RD is at the gate starts refusing
   * mid-session. And the signal poll stops on unmount **and on `visibilitychange`** — the endpoint
   * holds a TTL lease and a hidden tab must not keep a timer streaming.
   */
  import { Badge, Banner, Button, Card, toast } from '@gridfpv/components';
  import type {
    ChannelCatalogEntry,
    CompetitorRef,
    HeatSummary,
    Pilot,
    PilotId,
    PilotProgress,
    Timer,
    TimerId
  } from '@gridfpv/types';
  import type { Action } from 'svelte/action';
  import type { Session } from '../lib/session.svelte.js';
  import RssiGraph from '../lib/RssiGraph.svelte';
  import Brand from '../Brand.svelte';
  import Breadcrumbs from '../Breadcrumbs.svelte';
  import { createCompetitorNameResolver } from '../lib/competitorName.js';
  import { isOpenPracticeRound } from '../lib/heats.js';
  import {
    RSSI_MAX,
    RSSI_MIN,
    adoptReported,
    clampLevel,
    foldReadback,
    isParsableLevel,
    nodeCountOf,
    nodeRefOf,
    nodeTraceOf,
    nodeTuneLabel,
    phaseLabel,
    phaseTone,
    readoutsOf,
    seedThreshold,
    writeGate,
    type ApplyLevels,
    type CalibrationReadback,
    type FetchSignal,
    type Threshold,
    type ThresholdState,
    type TimerSignal,
    type TimerSignalNode
  } from '../lib/tuning.js';

  /** How often to poll the signal endpoint. Fast enough for a rolling plot, slow enough for a LAN. */
  const DEFAULT_POLL_MS = 250;

  /**
   * How long typing in the numeric box may pause before it counts as "done" and writes. Blur and
   * Enter write immediately; this only catches the RD who types a value and then looks up at the
   * gate without leaving the field.
   */
  const TYPING_IDLE_MS = 300;

  let {
    session,
    timer,
    onhome,
    ontimers,
    fetchSignal,
    applyLevels,
    pollMs = DEFAULT_POLL_MS
  }: {
    session: Session;
    /** The timer being tuned, resolved from the route by the shell (never a bare id here). */
    timer: Timer;
    /** Leave to the home hub (the brand mark + the first breadcrumb). */
    onhome: () => void;
    /** Leave to the Timers page (the second breadcrumb — where this page is entered from). */
    ontimers: () => void;
    /** Test/host seam for the signal poll; defaults to `GET /timers/{id}/signal`. */
    fetchSignal?: FetchSignal;
    /** Test/host seam for the calibration write; defaults to `POST /timers/{id}/calibration`. */
    applyLevels?: ApplyLevels;
    /** Poll cadence (ms). */
    pollMs?: number;
  } = $props();

  const base = $derived(
    session.baseUrl.endsWith('/') ? session.baseUrl.slice(0, -1) : session.baseUrl
  );

  const readSignal = $derived<FetchSignal>(
    fetchSignal ??
      (async (id, opts) => {
        const resp = await globalThis.fetch(`${base}/timers/${encodeURIComponent(id)}/signal`, {
          signal: opts.signal
        });
        if (!resp.ok) throw new Error(`The timer's signal feed answered ${resp.status}.`);
        return (await resp.json()) as TimerSignal;
      })
  );

  const writeLevels = $derived<ApplyLevels>(
    applyLevels ??
      (async (id, node, body) => {
        const resp = await globalThis.fetch(
          `${base}/timers/${encodeURIComponent(id)}/calibration`,
          {
            method: 'POST',
            headers: { 'content-type': 'application/json' },
            body: JSON.stringify({ node, ...body })
          }
        );
        if (!resp.ok) throw new Error(`The timer refused the change (${resp.status}).`);
        return (await resp.json()) as CalibrationReadback;
      })
  );

  // ── The live signal poll ────────────────────────────────────────────────────────────────────
  // On-demand and lease-held: the Director only streams while somebody is watching, so the poll
  // must stop the moment nobody is. `visibilitychange` matters as much as unmount here — the RD
  // walks to the gate with the phone in a pocket, and a backgrounded tab that kept polling would
  // hold the lease (and the 10 Hz socket parse) open indefinitely.
  let signal = $state.raw<TimerSignal | undefined>(undefined);
  let signalError = $state<string | undefined>(undefined);
  /** Whether a first snapshot has landed — distinguishes "connecting" from "no nodes". */
  let everLoaded = $state(false);

  let poll: ReturnType<typeof setInterval> | undefined;
  let inflightPoll: AbortController | undefined;

  async function pollOnce(id: TimerId): Promise<void> {
    inflightPoll?.abort();
    const ctl = new AbortController();
    inflightPoll = ctl;
    try {
      const snap = await readSignal(id, { signal: ctl.signal });
      if (ctl.signal.aborted) return;
      ingest(snap);
      signalError = undefined;
      everLoaded = true;
    } catch (e) {
      if (ctl.signal.aborted) return;
      signalError = e instanceof Error ? e.message : String(e);
    } finally {
      if (inflightPoll === ctl) inflightPoll = undefined;
    }
  }

  function startPolling(id: TimerId): void {
    if (poll !== undefined) return;
    void pollOnce(id);
    poll = setInterval(() => void pollOnce(id), pollMs);
  }

  function stopPolling(): void {
    if (poll !== undefined) {
      clearInterval(poll);
      poll = undefined;
    }
    inflightPoll?.abort();
    inflightPoll = undefined;
  }

  $effect(() => {
    const id = timer.id;
    const doc = typeof document === 'undefined' ? undefined : document;
    const sync = () => {
      if (doc?.visibilityState === 'hidden') stopPolling();
      else startPolling(id);
    };
    sync();
    doc?.addEventListener('visibilitychange', sync);
    return () => {
      doc?.removeEventListener('visibilitychange', sync);
      stopPolling();
    };
  });

  // ── The ONE value per (node, threshold) ─────────────────────────────────────────────────────
  // Keyed by node index. Absent until the timer has actually reported that node's levels: a control
  // sitting on a made-up default is a control the RD can drag away from without realising they
  // never saw the real one.
  let levels = $state<Record<number, { enter: ThresholdState; exit: ThresholdState }>>({});

  /**
   * A per-(node, threshold) write sequence. Non-reactive on purpose — it exists only so a readback
   * that lands *after* the RD has started adjusting again is dropped instead of stamping a stale
   * value over the live one.
   */
  const writeSeq = new Map<string, number>();
  /** The pending "typing stopped" timers, one per (node, threshold). */
  const idleTimers = new Map<string, ReturnType<typeof setTimeout>>();
  const keyOf = (node: number, th: Threshold) => `${node}:${th}`;

  /**
   * The nodes with a pointer currently held down on one of their controls. A drag is the one
   * interaction whose end is signalled explicitly (`pointerup`), so while it is in progress the
   * typing-idle net is suppressed: a slow drag that pauses for a beat must not fire a write and
   * then another on release. Cleared by every path that ends an interaction — pointer up, pointer
   * cancel, `change`, blur — so a lost pointerup cannot wedge it.
   */
  const dragging = new Set<number>();
  const beginDrag = (node: number) => dragging.add(node);
  const endDrag = (node: number) => dragging.delete(node);

  function ingest(snap: TimerSignal): void {
    signal = snap;
    for (const n of snap.nodes) {
      const enter = n.enter_at_level;
      const exit = n.exit_at_level;
      if (enter === undefined || exit === undefined) continue;
      const held = levels[n.node];
      if (!held) {
        levels[n.node] = { enter: seedThreshold(enter), exit: seedThreshold(exit) };
        continue;
      }
      // The hardware is authoritative whenever we are not mid-edit — the RD may have tuned in
      // RotorHazard's own UI, or a profile switch may have moved the levels underneath us.
      held.enter = adoptReported(held.enter, enter);
      held.exit = adoptReported(held.exit, exit);
    }
  }

  const nodeCount = $derived(nodeCountOf(signal, timer.node_count ?? 0));
  const nodeIndices = $derived(Array.from({ length: nodeCount }, (_, i) => i));
  const nodeById = $derived(
    new Map<number, TimerSignalNode>((signal?.nodes ?? []).map((n) => [n.node, n]))
  );

  /**
   * Set the one value. **Every** editor comes through here and nowhere else clamps — that is the
   * whole guarantee. Marks the threshold `Adjusting` and (re)arms the typing-idle write, so a value
   * typed and then left alone still reaches the timer.
   */
  function adjust(node: number, th: Threshold, raw: unknown): void {
    const held = levels[node];
    if (!held) return;
    const next = clampLevel(raw, held[th].value);
    held[th] = { ...held[th], value: next, phase: 'pending', detail: undefined };
    armIdle(node, th);
  }

  function armIdle(node: number, th: Threshold): void {
    // A drag says when it is finished; it does not need (or want) the idle net.
    if (dragging.has(node)) return;
    const key = keyOf(node, th);
    const existing = idleTimers.get(key);
    if (existing !== undefined) clearTimeout(existing);
    idleTimers.set(
      key,
      setTimeout(() => {
        idleTimers.delete(key);
        void commit(node, th);
      }, TYPING_IDLE_MS)
    );
  }

  /**
   * The interaction ended (pointer up, Enter, blur, or typing went quiet) — push the value at the
   * timer, unless it is already what the timer holds.
   */
  async function commit(node: number, th: Threshold): Promise<void> {
    const key = keyOf(node, th);
    const idle = idleTimers.get(key);
    if (idle !== undefined) {
      clearTimeout(idle);
      idleTimers.delete(key);
    }
    const held = levels[node];
    if (!held) return;
    const state = held[th];
    if (state.phase === 'sent') return;
    if (state.value === state.confirmed) {
      // Dragged out and back, or re-blurred — nothing to write, and the phase returns to rest.
      if (state.phase !== 'confirmed')
        held[th] = { ...state, phase: 'confirmed', detail: undefined };
      return;
    }

    // Per WRITE, not per page load: a heat can go Running while the RD is standing at the gate.
    const gate = writeGate(session.liveState?.phase, heatKind);
    if (!gate.allowed) {
      held[th] = { ...state, phase: 'refused', detail: gate.reason };
      return;
    }

    const sent = state.value;
    const seq = (writeSeq.get(key) ?? 0) + 1;
    writeSeq.set(key, seq);
    held[th] = { ...state, phase: 'sent', detail: undefined };

    try {
      const readback = await writeLevels(
        timer.id,
        node,
        th === 'enter' ? { enter_at_level: sent } : { exit_at_level: sent }
      );
      if (writeSeq.get(key) !== seq) return; // superseded by a newer adjustment — drop this answer
      const current = levels[node];
      if (!current) return;
      const back = th === 'enter' ? readback.enter_at_level : readback.exit_at_level;
      current[th] = foldReadback(current[th], sent, back);
      // The readback carries BOTH levels, so it also refreshes the *other* threshold's idea of what
      // the hardware holds — but only while that one is at rest.
      const other: Threshold = th === 'enter' ? 'exit' : 'enter';
      const otherBack = other === 'enter' ? readback.enter_at_level : readback.exit_at_level;
      current[other] = adoptReported(current[other], otherBack);
    } catch (e) {
      if (writeSeq.get(key) !== seq) return;
      const current = levels[node];
      if (!current) return;
      const message = e instanceof Error ? e.message : String(e);
      current[th] = { ...current[th], phase: 'failed', detail: message };
      toast.error(`${nodeLabel(node)}: the ${th} threshold did not reach the timer. ${message}`);
    }
  }

  /**
   * Observe the END of a threshold drag on the graph.
   *
   * `RssiGraph`'s handles emit `onthresholds` on every pointer move but have no "drag finished"
   * callback (slice 1 owns that component and this page must not edit it), and the write cadence
   * hangs entirely on knowing when the RD let go. The handle takes pointer capture, so its
   * `pointerup` — and the `keyup` ending an arrow-key nudge — bubbles through this wrapper: an
   * **action**, not `onpointerup` on the div, because the div is a passive observer of a
   * descendant's interaction, not an interactive element itself.
   *
   * (A follow-up worth raising with slice 1: an `onthresholdscommit` callback would make this
   * unnecessary and would serve marshaling's re-detect just as well.)
   */
  const commitOnRelease: Action<HTMLElement, number> = (el, node) => {
    const handler = () => commitNode(node);
    const down = () => beginDrag(node);
    el.addEventListener('pointerdown', down);
    el.addEventListener('pointerup', handler);
    el.addEventListener('pointercancel', handler);
    el.addEventListener('keyup', handler);
    return {
      destroy() {
        el.removeEventListener('pointerdown', down);
        el.removeEventListener('pointerup', handler);
        el.removeEventListener('pointercancel', handler);
        el.removeEventListener('keyup', handler);
      }
    };
  };

  /** Commit every threshold on a node that is mid-adjustment — the graph's drag/keyboard end. */
  function commitNode(node: number): void {
    endDrag(node);
    const held = levels[node];
    if (!held) return;
    if (held.enter.phase === 'pending') void commit(node, 'enter');
    if (held.exit.phase === 'pending') void commit(node, 'exit');
  }

  /**
   * The graph emits BOTH levels on every pointer move, so only the one that actually moved is
   * marked as being adjusted — otherwise dragging `enter` would show `exit` as pending and write it
   * on release for no reason.
   */
  function onGraphThresholds(node: number, enter: number, exit: number): void {
    const held = levels[node];
    if (!held) return;
    if (clampLevel(enter, held.enter.value) !== held.enter.value) adjust(node, 'enter', enter);
    if (clampLevel(exit, held.exit.value) !== held.exit.value) adjust(node, 'exit', exit);
  }

  // ── The write gate's inputs (the current heat, and what kind it is) ─────────────────────────
  // Tuning is refused while a COMPETITION heat is running — a threshold change rewrites what the
  // gate counts as a lap. Open practice is excluded from scoring (#398), so there is nothing to
  // corrupt, and pilots in the air is the natural moment to tune. Idle/staged/finished: fine.
  let heats = $state<HeatSummary[]>([]);
  $effect(() => {
    // Re-read when the event or the heat on the timer changes, so a heat that starts mid-session is
    // classified correctly rather than against a stale list.
    void session.liveState?.current_heat;
    if (!session.currentEvent) {
      heats = [];
      return;
    }
    session
      .listHeats()
      .then((h) => (heats = h))
      .catch(() => {});
  });

  /**
   * What kind of heat is on the timer: `practice`, `competition`, or `undefined` for none. **Fails
   * closed** — a running heat whose round cannot be resolved counts as competition, because the
   * cost of refusing a legitimate tune is a shrug and the cost of allowing a competition one is a
   * corrupted result.
   */
  const heatKind = $derived.by<'practice' | 'competition' | undefined>(() => {
    const current = session.liveState?.current_heat;
    if (!current) return undefined;
    const summary = heats.find((h) => h.heat === current);
    const round = summary?.round
      ? session.currentEvent?.rounds?.find((r) => r.id === summary.round)
      : undefined;
    if (!round) return 'competition';
    return isOpenPracticeRound(round) ? 'practice' : 'competition';
  });

  /** The page-level gate readout — the same rule the per-write check uses, shown before it bites. */
  const gate = $derived(writeGate(session.liveState?.phase, heatKind));

  // ── Names (CLAUDE.md: never a raw seat, never a bare frequency) ─────────────────────────────
  let catalog = $state<ChannelCatalogEntry[]>([]);
  $effect(() => {
    session
      .listChannels()
      .then((c) => (catalog = c))
      .catch(() => (catalog = []));
  });

  let pilots = $state<Pilot[]>([]);
  $effect(() => {
    void session.protocolState;
    session
      .listPilots()
      .then((p) => (pilots = p))
      .catch(() => {});
  });
  const pilotById = $derived(new Map<PilotId, Pilot>(pilots.map((p) => [p.id, p])));
  const explicitPilotByRef = $derived(
    new Map<CompetitorRef, PilotId>(
      (session.liveState?.progress ?? [])
        .filter((p): p is PilotProgress & { pilot: PilotId } => p.pilot != null)
        .map((p) => [p.competitor, p.pilot])
    )
  );

  /** The frequency a node is actually on: the live heartbeat's, else the timer's configured pool. */
  function frequencyOf(node: number): number | undefined {
    return nodeById.get(node)?.frequency ?? timer.available_channels?.[node];
  }

  /** `Node 1 · Raceband R7` — the seat's own name, band+channel resolved through `channels.ts`. */
  function nodeLabel(node: number): string {
    return nodeTuneLabel(node, frequencyOf(node), catalog);
  }

  // The shared resolver (CLAUDE.md), with the node labels as the seat fallback: a seat bound to a
  // pilot reads as the callsign, an unbound one as `Node 1 · Raceband R7`, and a raw `node-0` or a
  // bare `5880` never reaches the screen.
  const channelByRef = $derived(
    new Map<CompetitorRef, string>(nodeIndices.map((i) => [nodeRefOf(i), nodeLabel(i)]))
  );
  const competitorName = $derived.by<(ref: CompetitorRef) => string>(() =>
    createCompetitorNameResolver({ pilotById, explicitPilotByRef, channelByRef })
  );

  /** The seated pilot's callsign for a node, or `undefined` when the seat is unbound. */
  function seatedPilot(node: number): string | undefined {
    const resolved = competitorName(nodeRefOf(node));
    return resolved === nodeLabel(node) || resolved === nodeRefOf(node) ? undefined : resolved;
  }

  // ── Layout ──────────────────────────────────────────────────────────────────────────────────
  // Columns is the decision; stacked is a look the RD wants to compare against. Same markup, one
  // class — deliberately not a second component, which is how two layouts start to diverge.
  let layout = $state<'columns' | 'stacked'>('columns');

  /** The live trace for one node, in the `{ competitors: [...] }` shape `RssiGraph` consumes. */
  function traceFor(node: number) {
    const snap = nodeById.get(node);
    return {
      competitors: snap
        ? [nodeTraceOf(timer.id, snap)]
        : [
            {
              competitor: { adapter: timer.id, competitor: nodeRefOf(node) },
              from: 0,
              period_micros: 200_000,
              samples: []
            }
          ]
    };
  }
</script>

<div class="page">
  <div class="page-inner">
    <div class="brand-row"><Brand onclick={onhome} /></div>
    <Breadcrumbs
      crumbs={[
        { label: 'Home', onclick: onhome },
        { label: 'Timers', onclick: ontimers },
        { label: `Tune ${timer.name}` }
      ]}
    />

    <header class="page-head">
      <div class="page-titles">
        <h1 class="page-title">Tune {timer.name}</h1>
        <p class="lead">
          Set each gate's <strong>enter</strong> and <strong>exit</strong> levels while a quad flies
          through. The shaded band opens when the signal rises past enter and closes when it falls
          back past exit — if it does not bracket the pass, the timer will miss the lap.
          <strong>Changes go to the timer as you make them</strong>; there is nothing to press.
        </p>
      </div>
      <div class="layout-toggle" role="group" aria-label="Layout">
        <Button
          variant={layout === 'columns' ? 'primary' : 'ghost'}
          size="sm"
          aria-pressed={layout === 'columns'}
          onclick={() => (layout = 'columns')}>Columns</Button
        >
        <Button
          variant={layout === 'stacked' ? 'primary' : 'ghost'}
          size="sm"
          aria-pressed={layout === 'stacked'}
          onclick={() => (layout = 'stacked')}>Stacked</Button
        >
      </div>
    </header>

    {#if !gate.allowed}
      <!-- Stated up front as well as per write: the RD should know before they drag, not after. -->
      <div class="gate-banner" role="status">{gate.reason}</div>
    {/if}

    {#if signalError}
      <Banner tone="danger" title="Lost the timer's signal feed.">{signalError}</Banner>
    {/if}

    {#if nodeIndices.length === 0}
      <Card elevation="sm">
        <div class="empty">
          <p>{everLoaded ? 'This timer reports no nodes to tune.' : 'Reading the timer…'}</p>
          <p class="empty-sub">
            Tuning needs a live connection to the timer. Connect it on the Timers page, then come
            back.
          </p>
        </div>
      </Card>
    {:else}
      <div class="nodes" class:stacked={layout === 'stacked'} data-layout={layout}>
        {#each nodeIndices as node (node)}
          {@const held = levels[node]}
          {@const snap = nodeById.get(node)}
          <section class="node" aria-label={nodeLabel(node)}>
            <header class="node-head">
              <h2 class="node-name">{nodeLabel(node)}</h2>
              {#if seatedPilot(node)}
                <span class="node-pilot">{seatedPilot(node)}</span>
              {/if}
              {#if snap?.crossing_flag}
                <Badge tone="accent">In gate</Badge>
              {/if}
            </header>

            <!-- `use:commitOnRelease` catches the end of a threshold drag on the graph (see the
                 action) — that release is what triggers the single write. -->
            <div class="plot" use:commitOnRelease={node}>
              <RssiGraph
                mode="live"
                trace={traceFor(node)}
                nameFor={competitorName}
                onthresholds={held
                  ? (_ref, enter, exit) => onGraphThresholds(node, enter, exit)
                  : undefined}
                tuned={held
                  ? { competitor: nodeRefOf(node), enter: held.enter.value, exit: held.exit.value }
                  : undefined}
              />
            </div>

            {#if held}
              <div class="thresholds">
                {#each [{ th: 'enter' as Threshold, label: 'Enter at' }, { th: 'exit' as Threshold, label: 'Exit at' }] as row (row.th)}
                  {@const state = held[row.th]}
                  <div class="threshold" data-testid={`threshold-${node}-${row.th}`}>
                    <div class="threshold-head">
                      <label class="threshold-label" for={`level-${node}-${row.th}`}>
                        {row.label}
                      </label>
                      <Badge tone={phaseTone(state.phase)} variant="outline">
                        {phaseLabel(state.phase)}
                      </Badge>
                    </div>
                    <div class="threshold-controls">
                      <!-- Editor 1: the box, for a value you know. `value` is bound one-way FROM
                           the state and every keystroke goes back through `adjust` — binding it
                           would make the input a second source of truth holding raw text. -->
                      <input
                        class="level-box"
                        id={`level-${node}-${row.th}`}
                        type="number"
                        inputmode="numeric"
                        min={RSSI_MIN}
                        max={RSSI_MAX}
                        step="1"
                        aria-label={`${row.label} level for ${nodeLabel(node)}`}
                        value={state.value}
                        oninput={(e) => {
                          const raw = e.currentTarget.value;
                          // A half-typed / emptied box is not a number: leave the state alone
                          // rather than clamping "" to the minimum under the RD's fingers.
                          if (isParsableLevel(raw)) adjust(node, row.th, raw);
                        }}
                        onkeydown={(e) => {
                          if (e.key === 'Enter') void commit(node, row.th);
                        }}
                        onblur={(e) => {
                          // Re-sync the DOM from the state, so an abandoned half-typed box snaps
                          // back to the one value rather than lingering as a third view.
                          e.currentTarget.value = String(state.value);
                          void commit(node, row.th);
                        }}
                      />
                      <!-- Editor 2: the slider, for a value you are feeling out. -->
                      <input
                        class="level-slider"
                        type="range"
                        min={RSSI_MIN}
                        max={RSSI_MAX}
                        step="1"
                        aria-label={`${row.label} slider for ${nodeLabel(node)}`}
                        value={state.value}
                        oninput={(e) => adjust(node, row.th, e.currentTarget.value)}
                        onpointerdown={() => beginDrag(node)}
                        onchange={() => {
                          endDrag(node);
                          void commit(node, row.th);
                        }}
                        onpointerup={() => {
                          endDrag(node);
                          void commit(node, row.th);
                        }}
                        onpointercancel={() => {
                          endDrag(node);
                          void commit(node, row.th);
                        }}
                        onkeyup={() => void commit(node, row.th)}
                        onblur={() => {
                          endDrag(node);
                          void commit(node, row.th);
                        }}
                      />
                    </div>
                    {#if state.detail}
                      <p class="threshold-detail" role="status">{state.detail}</p>
                    {/if}
                  </div>
                {/each}
              </div>
            {:else}
              <p class="node-waiting" role="status">Waiting for this node to report its levels…</p>
            {/if}

            <dl class="readouts">
              {#each readoutsOf(snap) as r (r.key)}
                <div class="readout" data-testid={`readout-${node}-${r.key}`}>
                  <dt>{r.label}</dt>
                  <dd>{r.value}</dd>
                </div>
              {/each}
            </dl>
          </section>
        {/each}
      </div>
    {/if}
  </div>
</div>

<style>
  .page {
    min-height: 100vh;
    padding: var(--gf-space-6) var(--gf-space-6) var(--gf-space-8);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    overflow: auto;
  }
  /* Deliberately NOT the narrow measure the other directory pages use: this is a working surface,
     not a document. Four-to-eight columns of plot + controls + six readouts wants the viewport. */
  .page-inner {
    width: 100%;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
  }
  .brand-row {
    margin-bottom: var(--gf-space-4);
  }
  .page-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--gf-space-4);
    flex-wrap: wrap;
  }
  .page-titles {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
    min-width: 0;
  }
  .page-title {
    margin: 0;
    font-size: var(--gf-font-size-2xl);
    letter-spacing: var(--gf-tracking-tight);
  }
  .lead {
    margin: 0;
    max-width: 46rem;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .lead strong {
    color: var(--gf-text-secondary);
    font-weight: var(--gf-font-weight-semibold);
  }
  .layout-toggle {
    display: flex;
    gap: var(--gf-space-1);
    align-items: center;
  }

  .gate-banner {
    padding: var(--gf-space-3) var(--gf-space-4);
    border: 1px solid var(--gf-warn, var(--gf-border));
    border-radius: var(--gf-radius-md);
    background: var(--gf-surface-2);
    color: var(--gf-text-secondary);
    font-size: var(--gf-font-size-sm);
  }

  .empty {
    padding: var(--gf-space-6);
    text-align: center;
  }
  .empty p {
    margin: 0 0 var(--gf-space-2);
  }
  .empty-sub {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }

  /* Columns: one node per column, side by side, each wide enough to read a plot in. Overflowing
     horizontally is correct for eight nodes on a laptop — the alternative is eight plots too
     narrow to see a crossing in. Stacked is the same markup with the grid turned on its side. */
  .nodes {
    display: grid;
    grid-auto-flow: column;
    grid-auto-columns: minmax(20rem, 1fr);
    gap: var(--gf-space-4);
    overflow-x: auto;
    padding-bottom: var(--gf-space-2);
  }
  .nodes.stacked {
    grid-auto-flow: row;
    grid-auto-columns: auto;
    overflow-x: visible;
  }

  .node {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
    padding: var(--gf-space-4);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    background: var(--gf-surface-1);
    min-width: 0;
  }
  .node-head {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
  }
  .node-name {
    margin: 0;
    font-size: var(--gf-font-size-base);
    letter-spacing: var(--gf-tracking-tight);
  }
  .node-pilot {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .node-waiting {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }

  .plot {
    min-width: 0;
  }

  .thresholds {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
    padding-top: var(--gf-space-3);
    border-top: 1px solid var(--gf-border);
  }
  .threshold {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .threshold-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-2);
  }
  .threshold-label {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-secondary);
    font-weight: var(--gf-font-weight-semibold);
  }
  .threshold-controls {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
  }
  /* Big enough to read and hit from arm's length — this is used at a gate, not at a desk. */
  .level-box {
    width: 5rem;
    padding: var(--gf-space-2);
    font-size: var(--gf-font-size-lg);
    font-variant-numeric: tabular-nums;
    text-align: center;
    color: var(--gf-text);
    background: var(--gf-surface-2);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
  }
  .level-slider {
    flex: 1 1 auto;
    min-width: 0;
    accent-color: var(--gf-accent);
    height: 1.75rem;
  }
  .threshold-detail {
    margin: 0;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }

  .readouts {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--gf-space-2) var(--gf-space-3);
    margin: 0;
    padding-top: var(--gf-space-3);
    border-top: 1px solid var(--gf-border);
  }
  .readout {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--gf-space-2);
    min-width: 0;
  }
  .readout dt {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-xs);
    white-space: nowrap;
  }
  .readout dd {
    margin: 0;
    font-variant-numeric: tabular-nums;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text);
  }
</style>
