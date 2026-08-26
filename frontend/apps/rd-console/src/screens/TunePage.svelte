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
   * ## The URL also carries the SCOPE (#411)
   *
   * Two routes reach this page: `#/timers/<id>/tune` — the timer's own baseline, reachable before any
   * event exists — and `#/events/<eventId>/timers/<id>/tune`, entered from inside an event workspace
   * and returning there on back. The page states which it is editing ("editing: Saturday Race · Track
   * RH") **from the route alone**, so a reload, or this link opened on the phone at the gate, keeps
   * the scope it was opened with rather than guessing from whatever is active.
   *
   * Both scopes write through the same calibration path today: the per-event tune layer does not
   * exist yet, so the difference is the scope and the labelling, not two write paths — and the scope
   * line says exactly that rather than implying an isolation the system cannot deliver.
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
   * ## The confirmation is a POLL, not a response
   *
   * `POST /timers/{id}/calibration` answers "accepted" and nothing more. RotorHazard does not echo a
   * level set synchronously — it broadcasts `enter_and_exit_at_levels`, which comes back to this page
   * as `NodeSignal.enter_at`/`exit_at` on a **later** `GET /signal`. So `Sending…` is not "waiting
   * for the HTTP response" (that already returned); it is waiting for the feed this page is already
   * polling to show the new level. If the polls keep disagreeing past `CONFIRM_TIMEOUT_MS` the
   * threshold says `Not taken`, loudly, on that node.
   *
   * ## The channel is settable, not just shown (#413)
   *
   * Tuning a gate is meaningless until the node is listening on the channel it will race, so the
   * frequency this page already *showed* is a dropdown. Three things about it are load-bearing:
   *
   *  1. **The options do NOT come from `available_channels`.** Both real RotorHazard timers on the
   *     bench report `Flexible` with an **empty** pool, which means "no restriction", not "no
   *     channels" — a dropdown bound to it renders empty on exactly the timer this is for. The
   *     source is the *capability*: `Fixed` → its declared set, `Flexible` → the whole catalog,
   *     plus any custom raw MHz the RD added. See `tuning.ts`'s `channelOptions`.
   *  2. **Band and channel go with the frequency.** RotorHazard stores them on its profile, and the
   *     RD validates this work by refreshing RotorHazard's own page — where a bare number with no
   *     `R7` beside it reads as "it half worked".
   *  3. **A heat will overwrite it, and that is correct.** Heat setup re-tunes every node to its
   *     assigned channel. This page says so rather than trying to make the bench value win.
   *
   * And the thing nothing else announces: `on_set_frequency` writes the frequency into the **same
   * profile row** that holds `enter_ats`/`exit_ats`, so a channel change leaves the thresholds
   * untouched — tuned for the channel the node just left. The levels look unchanged and therefore
   * fine. The node says otherwise, factually, until the RD has flown a pass on the new channel.
   *
   * ## Gates and lifetimes
   *
   * Every adjustment is a write, so the practice-only gate (`writeGate`) is checked **per write**,
   * not once at load: a heat that goes `Running` while the RD is at the gate starts refusing
   * mid-session. And control authority (`session.canControl`) disables the editors up front rather
   * than letting the RD drag a slider whose write cannot possibly land.
   *
   * ## The feed is LEASED, and this page is what holds it
   *
   * `GET /timers/{id}/signal` is a subscription, not a read: the Director streams a timer's telemetry
   * only while somebody is looking, each `GET` renews a ~5 s lease, and the stream stops by itself
   * once the calls stop. So the poll cadence is not merely a refresh rate — it is the thing keeping
   * the feed alive, which is why it sits an order of magnitude inside the lease (`holdsLease`).
   *
   * The other half of that bargain is giving it back. The page `POST`s `signal/stop` when it goes
   * away — unmount, route change, or the tab being hidden — because "the RD walked to the gate with
   * the phone in their pocket" must not leave a timer parsing telemetry into nobody's screen. The
   * lease is the backstop if that call never lands; it is not the plan.
   *
   * ## `streaming: false` is not "no signal"
   *
   * A snapshot can arrive perfectly well while nothing is feeding it. `TimerSignal.streaming` is the
   * difference between **no signal** (the feed is live; the gate is quiet) and **no link** (the timer
   * is not connected, or just dropped) — and an RD chasing a dead gate needs to be able to tell those
   * apart, because they have opposite fixes. Likewise `NodeSignal.seen`: a node RotorHazard has never
   * reported arrives carrying a full ring of zeroes, and drawing that would be a flat live-looking
   * trace along the floor. Those nodes are rendered as **dead**, not as quiet.
   */
  import { Badge, Banner, Button, Card, toast } from '@gridfpv/components';
  import type {
    ChannelCatalogEntry,
    CompetitorRef,
    EventMeta,
    HeatSummary,
    NodeSignal,
    Pilot,
    Timer,
    TimerId,
    TimerNodes,
    TimerSignal
  } from '@gridfpv/types';
  import type { Action } from 'svelte/action';
  import type { Session } from '../lib/session.svelte.js';
  import RssiGraph from '../lib/RssiGraph.svelte';
  import Brand from '../Brand.svelte';
  import Breadcrumbs from '../Breadcrumbs.svelte';
  import { buildCompetitorNames } from '../lib/competitorName.js';
  import { poolChannel } from '../lib/channels.js';
  import { isOpenPracticeRound } from '../lib/heats.js';
  import {
    CONFIRM_TIMEOUT_MS,
    HEAT_OVERWRITES_CHANNEL,
    RSSI_MAX,
    RSSI_MIN,
    SIGNAL_POLL_MS,
    channelGate,
    channelOptions,
    clampLevel,
    duplicateChannelNodes,
    duplicateChannelNote,
    foldPolled,
    foldPolledChannel,
    isParsableLevel,
    markChannelSent,
    markSent,
    nodeCountOf,
    nodeTraceOf,
    offerableNodes,
    phaseLabel,
    phaseTone,
    plottable,
    readoutsOf,
    seedChannel,
    seedThreshold,
    staleThresholdNote,
    writeGate,
    type ApplyChannel,
    type ApplyLevels,
    type ChannelState,
    type FetchNodes,
    type FetchSignal,
    type StopSignal,
    type Threshold,
    type ThresholdState
  } from '../lib/tuning.js';

  /**
   * How long typing in the numeric box may pause before it counts as "done" and writes. Blur and
   * Enter write immediately; this only catches the RD who types a value and then looks up at the
   * gate without leaving the field.
   */
  const TYPING_IDLE_MS = 300;

  let {
    session,
    timer,
    scopeEvent,
    onhome,
    ontimers,
    onevent,
    fetchSignal,
    applyLevels,
    applyChannel,
    fetchNodes,
    stopSignal,
    pollMs = SIGNAL_POLL_MS,
    confirmMs = CONFIRM_TIMEOUT_MS
  }: {
    session: Session;
    /** The timer being tuned, resolved from the route by the shell (never a bare id here). */
    timer: Timer;
    /**
     * The **event whose tune this is** (#411), when the page was entered from inside an event
     * workspace — resolved from the route's event id by the shell, so the page can name its scope
     * by the event's friendly name (CLAUDE.md) rather than an id. Absent on the app-level route,
     * where the scope is the timer's own baseline.
     */
    scopeEvent?: EventMeta;
    /** Leave to the home hub (the brand mark + the first breadcrumb). */
    onhome: () => void;
    /** Leave to the Timers page (the second breadcrumb — where this page is entered from). */
    ontimers: () => void;
    /**
     * Return to the **event workspace** this page was opened from (the event crumb). Only supplied
     * with {@link scopeEvent}: back out of an event-scoped tune belongs in the event, not on the
     * global Timers page.
     */
    onevent?: () => void;
    /** Test/host seam for the signal poll; defaults to the session's `GET /timers/{id}/signal`. */
    fetchSignal?: FetchSignal;
    /** Test/host seam for the calibration write; defaults to the session's `setCalibration`. */
    applyLevels?: ApplyLevels;
    /** Test/host seam for the channel write (#413); defaults to the session's `setNodeChannel`. */
    applyChannel?: ApplyChannel;
    /** Test/host seam for the node-set read (#412); defaults to the session's `timerNodes`. */
    fetchNodes?: FetchNodes;
    /** Test/host seam for releasing the feed; defaults to the session's `signal/stop`. */
    stopSignal?: StopSignal;
    /** Poll cadence (ms). */
    pollMs?: number;
    /** How long a write may go unconfirmed by the poll before it reads `Not taken` (ms). */
    confirmMs?: number;
  } = $props();

  // All three seams default to the SESSION's calls, not a bare `fetch`: every one of these routes
  // is `ControlAuth`-gated, so a hand-rolled fetch would 401 against any token-gated Director —
  // including the RD's real one — and the Director's refusal messages (which name the timer and the
  // heat by their friendly names) would be replaced by a status code.
  const readSignal = $derived<FetchSignal>(
    fetchSignal ?? ((id, opts) => session.timerSignal(id, { signal: opts.signal }))
  );
  const writeLevels = $derived<ApplyLevels>(
    applyLevels ??
      (async (id, body) => {
        await session.setCalibration(id, body);
      })
  );
  const releaseSignal = $derived<StopSignal>(stopSignal ?? ((id) => session.stopTimerSignal(id)));
  const writeChannel = $derived<ApplyChannel>(
    applyChannel ?? ((id, body) => session.setNodeChannel(id, body))
  );
  const readNodes = $derived<FetchNodes>(fetchNodes ?? ((id) => session.timerNodes(id)));

  // ── The live signal poll ────────────────────────────────────────────────────────────────────
  // The poll IS the subscription. The Director streams a timer only while somebody is watching, and
  // every `GET /signal` renews a ~5 s lease on that; so this cadence is not a refresh rate, it is
  // the thing keeping the feed alive, and it stays an order of magnitude inside the lease.
  //
  // The reverse obligation is `signal/stop`, fired on every path that ends the watch: unmount, a
  // route change, and `visibilitychange` — the RD walks to the gate with the phone in a pocket, and
  // a backgrounded tab must not leave a timer parsing telemetry into nobody's screen. The lease is
  // the backstop for a stop that never lands (a killed tab, a dead network), not the plan.
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

  /**
   * Stop watching `id`: end the cadence, abandon anything in flight, and **tell the Director** so
   * the stream stops now rather than when the lease runs out.
   *
   * The stop is fire-and-forget on purpose. It runs from teardown, where there is nobody left to
   * show an error to and nothing useful to do about one — and the lease already guarantees the
   * outcome if it never arrives. Not firing it at all is the thing that would be wrong.
   */
  function stopWatching(id: TimerId): void {
    if (poll !== undefined) {
      clearInterval(poll);
      poll = undefined;
    }
    inflightPoll?.abort();
    inflightPoll = undefined;
    void releaseSignal(id).catch(() => {});
  }

  $effect(() => {
    const id = timer.id;
    const doc = typeof document === 'undefined' ? undefined : document;
    const sync = () => {
      if (doc?.visibilityState === 'hidden') stopWatching(id);
      else startPolling(id);
    };
    sync();
    doc?.addEventListener('visibilitychange', sync);
    // Unmount is also how the ROUTE leaves — the shell swaps TunePage out on a hash change — so
    // this cleanup is the one that has to release the feed when the RD navigates away.
    return () => {
      doc?.removeEventListener('visibilitychange', sync);
      stopWatching(id);
    };
  });

  // ── The ONE value per (node, threshold) ─────────────────────────────────────────────────────
  // Keyed by node index. Absent until the timer has actually reported that node's levels: a control
  // sitting on a made-up default is a control the RD can drag away from without realising they
  // never saw the real one.
  let levels = $state<Record<number, { enter: ThresholdState; exit: ThresholdState }>>({});

  // ── The ONE value per node's CHANNEL (#413) ─────────────────────────────────────────────────
  // Same shape and same lifecycle as a threshold, for the same reason: there is no Apply button, so
  // the dropdown's state IS the confirmation. Absent until the node has reported a channel.
  let channels = $state<Record<number, ChannelState>>({});
  /** Per-node write sequence for the channel, so a superseded answer never stamps a stale value. */
  const channelSeq = new Map<number, number>();
  /** The "this should have come back by now" backstops, one per node (see {@link confirmTimers}). */
  const channelTimers = new Map<number, ReturnType<typeof setTimeout>>();

  /**
   * A per-(node, threshold) write sequence. Non-reactive on purpose — it exists only so a write
   * whose answer lands *after* the RD has started adjusting again is dropped instead of stamping a
   * stale value over the live one.
   */
  const writeSeq = new Map<string, number>();
  /** The pending "typing stopped" timers, one per (node, threshold). */
  const idleTimers = new Map<string, ReturnType<typeof setTimeout>>();
  /**
   * The pending "this should have shown up by now" timers, one per (node, threshold).
   *
   * A write is confirmed by the poll, so in the ordinary case {@link ingest} settles it and this
   * never fires. It exists for the case the poll itself has stopped — the feed errored, the tab was
   * hidden, the Director went away — where waiting on a poll that is not coming would leave the
   * threshold reading `Sending…` forever. Same verdict either way, via the same `foldPolled`.
   */
  const confirmTimers = new Map<string, ReturnType<typeof setTimeout>>();
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

  /**
   * Fold one poll into the page. This is where a write is **confirmed** — `POST /calibration` only
   * says "accepted", and the level itself comes back through `enter_at`/`exit_at` on a later
   * snapshot — and where the hardware reclaims a threshold the RD is not touching.
   */
  function ingest(snap: TimerSignal): void {
    signal = snap;
    const now = Date.now();
    for (const n of snap.nodes) {
      const held = levels[n.node];
      if (!held) {
        // Seed only from a node that has actually reported BOTH levels. A control sitting on a
        // made-up default is one the RD can drag away from without ever having seen the real one.
        if (n.enter_at === undefined || n.exit_at === undefined) continue;
        levels[n.node] = { enter: seedThreshold(n.enter_at), exit: seedThreshold(n.exit_at) };
        continue;
      }
      // A threshold at rest follows the hardware (the RD may have tuned in RotorHazard's own UI, or
      // a profile switch moved the levels underneath us); one with a write in flight is comparing
      // against what it asked for. `foldPolled` is both, and leaves everything else alone.
      held.enter = foldPolled(held.enter, n.enter_at, now, confirmMs);
      held.exit = foldPolled(held.exit, n.exit_at, now, confirmMs);
    }
    // The channel is confirmed the same way (#413) — `POST /channel` only says "accepted", and the
    // value comes back on the heartbeat as `frequency_mhz`. At rest this also FOLLOWS the hardware
    // on purpose: a heat legitimately re-tunes every node, and the page must show that rather than
    // keep displaying a bench value the node is no longer on.
    for (const n of snap.nodes) {
      const held = channels[n.node];
      if (!held) {
        // Seed only once the node has actually reported a channel. A dropdown resting on a
        // fabricated default is one the RD can change away from without ever seeing the real one.
        if (n.frequency_mhz === undefined) continue;
        channels[n.node] = seedChannel(n.frequency_mhz);
        continue;
      }
      channels[n.node] = foldPolledChannel(held, n.frequency_mhz, now, catalog, confirmMs);
    }
  }

  /**
   * The Director's own node view (#412): the effective width and which nodes the RD has left
   * enabled. Read once per timer — it is configuration, not telemetry, and it changes when the RD
   * edits it on the Timers page, not while they stand at the gate.
   *
   * Only the **channel** control consults it, and it **fails closed**: with no view yet, no node is
   * offered a channel dropdown. RotorHazard validates `0 <= node < num_nodes` and otherwise only
   * writes a log line, so offering a node that does not exist would produce a write that looks
   * accepted and lands nowhere — the #403 failure class this page exists to remove.
   */
  let nodeView = $state.raw<TimerNodes | undefined>(undefined);
  $effect(() => {
    const id = timer.id;
    let live = true;
    readNodes(id)
      .then((view) => {
        if (live) nodeView = view;
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  });
  const offerable = $derived(offerableNodes(nodeView));

  const nodeCount = $derived(nodeCountOf(signal, timer.node_count ?? 0));
  const nodeIndices = $derived(Array.from({ length: nodeCount }, (_, i) => i));
  const nodeById = $derived(
    new Map<number, NodeSignal>((signal?.nodes ?? []).map((n) => [n.node, n]))
  );

  /**
   * Whether the Director is actually being fed right now. `false` with a perfectly valid snapshot
   * is **no link** — the timer is not connected, or has just dropped — which is a different fault
   * from a live feed showing a quiet gate, and has a different fix. An RD chasing a dead gate needs
   * to be able to tell them apart, so the page says which one it is rather than showing a flat
   * trace and letting them guess.
   */
  const streaming = $derived(signal?.streaming ?? false);

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
    // Control authority is checked here too, as a backstop — the editors are already disabled
    // without it (see `canControl`), but a keyboard path or a role that changes mid-session must
    // not slip a write past a Director that will only answer 401.
    const gate = canControl
      ? writeGate(session.liveState?.phase, heatKind)
      : ({ allowed: false, reason: NO_CONTROL } as const);
    if (!gate.allowed) {
      held[th] = { ...state, phase: 'refused', detail: gate.reason };
      return;
    }

    const sent = state.value;
    const seq = (writeSeq.get(key) ?? 0) + 1;
    writeSeq.set(key, seq);
    // Only the threshold that MOVED is sent; omitting the other means "leave it where it is".
    const body = th === 'enter' ? { node, enter_at: sent } : { node, exit_at: sent };

    try {
      await writeLevels(timer.id, body);
      if (writeSeq.get(key) !== seq) return; // superseded by a newer adjustment — drop this answer
      const current = levels[node];
      if (!current) return;
      // Accepted is NOT applied. RotorHazard does not echo a level set; it broadcasts
      // `enter_and_exit_at_levels`, which reaches this page as `enter_at`/`exit_at` on a later
      // poll. So the write is only half done here — `ingest` finishes it.
      current[th] = markSent(current[th], sent, Date.now());
      armConfirm(node, th, seq);
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
   * Arm the backstop for a write the poll has not confirmed. See {@link confirmTimers}: the poll is
   * the confirmation, and this only decides the case where the poll never comes.
   */
  function armConfirm(node: number, th: Threshold, seq: number): void {
    const key = keyOf(node, th);
    const existing = confirmTimers.get(key);
    if (existing !== undefined) clearTimeout(existing);
    confirmTimers.set(
      key,
      setTimeout(() => {
        confirmTimers.delete(key);
        if (writeSeq.get(key) !== seq) return;
        const held = levels[node];
        if (!held || held[th].phase !== 'sent') return;
        const snap = nodeById.get(node);
        held[th] = foldPolled(
          held[th],
          th === 'enter' ? snap?.enter_at : snap?.exit_at,
          Date.now(),
          0
        );
      }, confirmMs)
    );
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

  // ── The channel write (#413) ────────────────────────────────────────────────────────────────

  /**
   * The RD picked a channel for a node — retune it now.
   *
   * A dropdown has no drag and no half-typed state, so unlike a threshold there is nothing to
   * debounce: `change` *is* the end of the interaction, and this is the whole write path.
   *
   * The **band and channel travel with the frequency**: RotorHazard stores them on its profile, and
   * an unlabelled channel on the page the RD refreshes to check this worked reads as a half-failure.
   * The Director validates the pair against its own catalog, so nothing invented can reach a timer.
   */
  async function commitChannel(node: number, mhz: number): Promise<void> {
    const held = channels[node];
    if (!held || held.phase === 'sent') return;
    if (mhz === held.confirmed) {
      // Re-picked what it is already on — nothing to write, and the phase returns to rest.
      if (held.phase !== 'confirmed')
        channels[node] = { ...held, phase: 'confirmed', detail: undefined };
      return;
    }

    // Per WRITE, exactly like a threshold: a heat can go Running while the RD is at the gate.
    const gate = canControl
      ? channelGate(session.liveState?.phase, heatKind)
      : ({ allowed: false, reason: NO_CONTROL } as const);
    if (!gate.allowed) {
      channels[node] = { ...held, phase: 'refused', detail: gate.reason };
      return;
    }

    const option = optionsFor(node).find((o) => o.mhz === mhz);
    const seq = (channelSeq.get(node) ?? 0) + 1;
    channelSeq.set(node, seq);
    try {
      const dispatch = await writeChannel(timer.id, {
        node,
        mhz,
        // Omitted, never sent empty, for a custom raw MHz: RotorHazard keeps whatever label it had
        // rather than being handed a blank one.
        ...(option?.band && option?.channel ? { band: option.band, channel: option.channel } : {})
      });
      if (channelSeq.get(node) !== seq) return; // superseded by a newer pick — drop this answer
      const current = channels[node];
      if (!current) return;
      // Accepted is NOT applied — `ingest` finishes it when the heartbeat brings the channel back.
      //
      // Whether the thresholds are now stale is the Director's answer, not ours: it holds the
      // record of what GridFPV set and when. `undefined` (a cancelled token prompt) is not a "no".
      const stale = dispatch?.thresholds_tuned_on_another_channel ?? false;
      channels[node] = markChannelSent(current, mhz, Date.now(), stale);
      armChannelConfirm(node, seq);
    } catch (e) {
      if (channelSeq.get(node) !== seq) return;
      const current = channels[node];
      if (!current) return;
      const message = e instanceof Error ? e.message : String(e);
      channels[node] = { ...current, phase: 'failed', detail: message };
      toast.error(`${nodeLabel(node)}: the channel change did not reach the timer. ${message}`);
    }
  }

  /** The backstop for a channel the poll has not confirmed — {@link armConfirm}'s twin. */
  function armChannelConfirm(node: number, seq: number): void {
    const existing = channelTimers.get(node);
    if (existing !== undefined) clearTimeout(existing);
    channelTimers.set(
      node,
      setTimeout(() => {
        channelTimers.delete(node);
        if (channelSeq.get(node) !== seq) return;
        const held = channels[node];
        if (!held || held.phase !== 'sent') return;
        channels[node] = foldPolledChannel(
          held,
          nodeById.get(node)?.frequency_mhz,
          Date.now(),
          catalog,
          0
        );
      }, confirmMs)
    );
  }

  /**
   * The options one node's dropdown offers.
   *
   * **Not `available_channels`** — see `channelOptions`. That field is the per-heat allocation
   * *pool*, and both real RotorHazard timers on the bench report `Flexible` with it empty, which
   * means "no restriction". It contributes exactly one thing here: the RD's **custom** raw-MHz
   * entries, which they asked to see alongside the catalog.
   */
  function optionsFor(node: number) {
    return channelOptions(
      timer.channel_capability,
      catalog,
      timer.available_channels ?? [],
      channels[node]?.mhz ?? frequencyOf(node)
    );
  }

  /**
   * Whether this node may be offered a channel at all (#412 + #413): the Director says it exists
   * and is enabled, and the timer has actually reported a channel to change *from*.
   */
  function channelSettable(node: number): boolean {
    return offerable.has(node) && channels[node] !== undefined;
  }

  /**
   * The nodes currently sharing a channel — flagged, never blocked (#413).
   *
   * Keyed on the **effective** channel: what the RD just picked while a write is in flight, else
   * what the timer reports. Using the reported value alone would leave the clash invisible for the
   * one poll where it matters most — the moment the RD makes it.
   */
  const clashingNodes = $derived.by(() => {
    const byNode = new Map<number, number | undefined>();
    // Only the nodes a heat can actually be seated on: a **disabled** node's receiver may well sit
    // on the same frequency, but nobody flies it, so calling that a clash would be a warning about
    // a situation that cannot cost anyone a lap.
    for (const node of nodeIndices.filter((n) => offerable.has(n))) {
      byNode.set(node, channels[node]?.mhz ?? nodeById.get(node)?.frequency_mhz);
    }
    return duplicateChannelNodes(byNode);
  });

  /** The nodes sharing `node`'s channel, for the note that names them. */
  function sharingWith(node: number): number[] {
    const mhz = channels[node]?.mhz ?? nodeById.get(node)?.frequency_mhz;
    if (mhz === undefined) return [];
    return nodeIndices.filter(
      (n) => offerable.has(n) && (channels[n]?.mhz ?? nodeById.get(n)?.frequency_mhz) === mhz
    );
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
  /** The same rule for a channel change (#413), in the words of what a retune actually does. */
  const chanGate = $derived(channelGate(session.liveState?.phase, heatKind));

  /**
   * Whether this session may write at all. `POST /timers/{id}/calibration` is `ControlAuth`-gated,
   * so a read-only session's every adjustment would come back 401 — and a 401 is a *different*
   * thing from "the timer refused" or "the write did not land": the RD needs to know they lack
   * authority, not that their gate is broken. Better to disable the editors up front than to let
   * someone drag a slider that cannot possibly apply and then explain the wreckage.
   */
  const canControl = $derived(session.canControl);
  const NO_CONTROL =
    'This session is read-only, so it cannot change a timer’s thresholds. Sign in with the Director’s control token to tune.';

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
  // ONE assembly of the resolver inputs, shared with every other screen (#416). This page is the
  // only one that holds a live `TimerSignal` subscription, so it is the one that can hand over
  // `NodeSignal.frequency_mhz` — what each node is ACTUALLY tuned to, and the only channel source
  // that works on a Flexible RotorHazard timer (whose `available_channels` is empty).
  const names = $derived(
    buildCompetitorNames({
      pilots,
      progress: session.liveState?.progress,
      catalog,
      signal,
      timer,
      membership: session.currentEvent?.classes_membership
    })
  );

  /**
   * The frequency a node is actually on: the live heartbeat's reading, else the timer's configured
   * pool. Via `poolChannel`, which refuses to index an **empty** pool — empty means "unrestricted"
   * on a Flexible RotorHazard timer, not "no channels", and indexing it would invent a frequency.
   */
  function frequencyOf(node: number): number | undefined {
    return nodeById.get(node)?.frequency_mhz ?? poolChannel(node, timer.available_channels);
  }

  /** `Node 1 · Raceband R7` — the seat's own name, band+channel resolved through `channels.ts`. */
  function nodeLabel(node: number): string {
    return names.seatLabel(node);
  }

  /**
   * The competitor ref a node plots under — **the timer's own `seat`**, never a locally re-spelled
   * `node-{i}`. That handle is what a heat's registration binds a pilot to, so re-deriving it here
   * is precisely the resolver drift CLAUDE.md exists to prevent. `undefined` before the first
   * snapshot: until the timer has told us its seats, the page does not have them.
   */
  function seatOf(node: number): CompetitorRef | undefined {
    return nodeById.get(node)?.seat;
  }

  // The shared resolver (CLAUDE.md), with the node labels as the seat fallback: a seat bound to a
  // pilot reads as the callsign, an unbound one as `Node 1 · Raceband R7`, and a raw `node-0` or a
  // bare `5880` never reaches the screen.
  const competitorName = $derived.by<(ref: CompetitorRef) => string>(() => names.name);

  /** The seated pilot's callsign for a node, or `undefined` when the seat is unbound. */
  function seatedPilot(node: number): string | undefined {
    const seat = seatOf(node);
    if (seat === undefined) return undefined;
    const resolved = competitorName(seat);
    return resolved === nodeLabel(node) || resolved === seat ? undefined : resolved;
  }

  // Both timer maps outlive any single render, so they are cleared with the component — a
  // typing-idle write or a confirmation backstop firing into a page the RD has already left has
  // nothing left to write to and nobody left to tell.
  $effect(() => () => {
    for (const t of idleTimers.values()) clearTimeout(t);
    idleTimers.clear();
    for (const t of confirmTimers.values()) clearTimeout(t);
    confirmTimers.clear();
    for (const t of channelTimers.values()) clearTimeout(t);
    channelTimers.clear();
  });

  // ── Layout ──────────────────────────────────────────────────────────────────────────────────
  // Columns is the decision; stacked is a look the RD wants to compare against. Same markup, one
  // class — deliberately not a second component, which is how two layouts start to diverge.
  let layout = $state<'columns' | 'stacked'>('columns');

  /**
   * The live trace for one node, in the `{ competitors: [...] }` shape `RssiGraph` consumes.
   *
   * **Empty unless the node has actually been seen.** The Director samples every node on the same
   * pass and fills an unreported one's slot with `0.0`, so a dead or unseated node arrives with a
   * full, perfectly plottable ring of zeroes — which drawn is a flat trace along the floor,
   * indistinguishable from a live node watching an empty gate. Those are the two states an RD is on
   * this page to tell apart, so the unseen node gets no plot at all and says why instead.
   */
  function traceFor(node: number) {
    const snap = nodeById.get(node);
    const snapshot = signal;
    return {
      competitors: snapshot && plottable(snap) ? [nodeTraceOf(snapshot, snap)] : []
    };
  }

  // ── Which tuning layer this page is editing (#411) ──────────────────────────────────────────
  //
  // The scope comes from the route: the event-scoped route carries an event, the app-level one does
  // not. Named by the event's friendly name, never its id (CLAUDE.md). Profiles (#411) are NOT
  // built, so the page says the *event* — it must not invent a profile name it cannot back up.
  const scopeLabel = $derived(scopeEvent ? `${scopeEvent.name} · ${timer.name}` : timer.name);
  // And the honest small print: both scopes write the same calibration today. The event tune layer
  // does not exist yet, so claiming these levels are "only for this event" would be a lie.
  const scopeNote = $derived(
    scopeEvent
      ? `Opened from ${scopeEvent.name}. These are still the timer's own levels — a per-event tune layer does not exist yet.`
      : "No event in scope — the timer's own levels."
  );
</script>

<div class="page">
  <div class="page-inner">
    <div class="brand-row"><Brand onclick={onhome} /></div>
    <!-- The trail is the SCOPE (#411). Entered from the Timers page the tune belongs to the timer
         itself, so the trail runs Home › Timers › Tune ‹timer›. Entered from inside an event it
         belongs to that event, so the middle crumb is the event — by name, never its id — and it
         goes back into the event workspace the RD came from, which is the whole point of the
         event-scoped route ("as long as when we click back we are back in the event"). -->
    <Breadcrumbs
      crumbs={scopeEvent && onevent
        ? [
            { label: 'Home', onclick: onhome },
            { label: scopeEvent.name, onclick: onevent },
            { label: `Tune ${timer.name}` }
          ]
        : [
            { label: 'Home', onclick: onhome },
            { label: 'Timers', onclick: ontimers },
            { label: `Tune ${timer.name}` }
          ]}
    />

    <header class="page-head">
      <div class="page-titles">
        <h1 class="page-title">Tune {timer.name}</h1>
        <!-- Which layer am I editing? (#411's first open question.) Answered from the ROUTE, not
             from whatever happens to be active — so a reload, or this link opened on a phone at the
             gate, states the same scope it was opened with. -->
        <p class="scope" data-testid="tune-scope">
          <span class="scope-label">editing:</span>
          <strong class="scope-target">{scopeLabel}</strong>
          <span class="scope-note">{scopeNote}</span>
        </p>
        <p class="lead">
          Set each gate's <strong>enter</strong> and <strong>exit</strong> levels while a quad flies
          through. The shaded band opens when the signal rises past enter and closes when it falls
          back past exit — if it does not bracket the pass, the timer will miss the lap.
          <strong>Changes go to the timer as you make them</strong>; there is nothing to press.
        </p>
      </div>
      <div class="head-controls">
        {#if signal}
          <!-- The feed's own state, distinct from any node's. `streaming: false` means NO LINK —
               the timer is not connected, or just dropped — as against a live feed over a quiet
               gate. Opposite faults, opposite fixes, so the page says which. The lease behind the
               tooltip is what the poll is renewing on every tick; it is the RD's only clue that
               this page is holding the stream open. -->
          <span
            class="feed-status"
            data-testid="feed-status"
            title={`The signal feed's lease renews on every poll — ${Math.max(
              0,
              Math.round(signal.lease_ms_remaining / 1000)
            )}s left on the current one.`}
          >
            <Badge tone={streaming ? 'success' : 'warn'} variant="outline">
              {streaming ? 'Feed live' : 'No link'}
            </Badge>
          </span>
        {/if}
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
      </div>
    </header>

    {#if !canControl}
      <!-- Authority, not health: every editor below is disabled, so say why once, up front. -->
      <Banner tone="warn" title="Read-only session.">{NO_CONTROL}</Banner>
    {:else if !gate.allowed}
      <!-- Stated up front as well as per write: the RD should know before they drag, not after. -->
      <div class="gate-banner" role="status">{gate.reason}</div>
      <!-- The same heat blocks a retune, and for its own reason — said in its own words so the RD
           is not left guessing whether the channel dropdown is covered by the sentence above. -->
      {#if !chanGate.allowed}
        <div class="gate-banner" role="status" data-testid="channel-gate">{chanGate.reason}</div>
      {/if}
    {/if}

    {#if signalError}
      <!-- The FEED itself failed — the Director did not answer. Nothing below is current. -->
      <Banner tone="danger" title="Lost the timer's signal feed.">{signalError}</Banner>
    {:else if everLoaded && !streaming}
      <!-- The Director answered fine; nothing is feeding it. That is "no link", not "no signal" —
           the distinction an RD chasing a dead gate is here to make. The plots below are the last
           thing the timer said before it went quiet, not what it is saying now. -->
      <Banner tone="warn" title="No link to this timer.">
        The Director is answering, but nothing is arriving from {timer.name}. Connect it on the
        Timers page — until then these readings are the last ones it sent, not live ones.
      </Banner>
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
          {@const seat = seatOf(node)}
          {@const dead = snap !== undefined && !snap.seen}
          <section class="node" class:dead aria-label={nodeLabel(node)}>
            <header class="node-head">
              <h2 class="node-name">{nodeLabel(node)}</h2>
              {#if seatedPilot(node)}
                <span class="node-pilot">{seatedPilot(node)}</span>
              {/if}
              {#if dead}
                <Badge tone="danger" variant="outline">Not reporting</Badge>
              {:else if snap?.crossing}
                <Badge tone="accent">In gate</Badge>
              {:else if snap?.crossed_recently}
                <!-- The sticky flag, which survives the Director's decimation: a fast pass between
                     two samples lights this even though `crossing` was false at both of them. -->
                <Badge tone="accent" variant="outline">Crossed</Badge>
              {/if}
            </header>

            {#if dead}
              <!-- A node RotorHazard has never reported. It arrives with a full ring of ZEROES —
                   the Director samples every node on the same pass — so plotting it would draw a
                   flat live-looking trace along the floor, exactly the picture of a quiet gate.
                   That is the one confusion this page exists to remove, so it gets no plot. -->
              <p class="node-dead" data-testid={`node-dead-${node}`} role="status">
                This node has not reported to the timer at all. It is not a quiet gate — there is
                nothing there to be quiet. Check the node is fitted and the timer sees it.
              </p>
            {:else}
              <!-- `use:commitOnRelease` catches the end of a threshold drag on the graph (see the
                   action) — that release is what triggers the single write. -->
              <div class="plot" use:commitOnRelease={node}>
                <RssiGraph
                  mode="live"
                  trace={traceFor(node)}
                  nameFor={competitorName}
                  onthresholds={held && seat !== undefined && canControl
                    ? (_ref, enter, exit) => onGraphThresholds(node, enter, exit)
                    : undefined}
                  tuned={held && seat !== undefined
                    ? { competitor: seat, enter: held.enter.value, exit: held.exit.value }
                    : undefined}
                />
              </div>
            {/if}

            {#if dead}
              <!-- No controls: there is no node there to retune or to write a threshold to. -->
            {:else if channelSettable(node)}
              {@const chan = channels[node]}
              <!-- The CHANNEL (#413). The frequency this page already showed, made settable, because
                   tuning a gate means nothing until its node is on the channel it will race.
                   Offered only for a node the Director says exists and the RD has left enabled
                   (#412): RotorHazard drops an out-of-range seat index with nothing but a log line,
                   so a write to a node that is not there would look accepted and land nowhere. -->
              <div class="channel" data-testid={`channel-${node}`}>
                <div class="channel-head">
                  <label class="channel-label" for={`channel-select-${node}`}>Channel</label>
                  <Badge tone={phaseTone(chan.phase)} variant="outline">
                    {phaseLabel(chan.phase)}
                  </Badge>
                </div>
                <select
                  class="channel-select"
                  id={`channel-select-${node}`}
                  aria-label={`Channel for ${nodeLabel(node)}`}
                  value={chan.mhz}
                  disabled={!canControl || !chanGate.allowed}
                  onchange={(e) => void commitChannel(node, Number(e.currentTarget.value))}
                >
                  <!-- The option VALUE is the raw MHz (a wire handle); the LABEL is always the
                       band+channel name, resolved through `channels.ts` (CLAUDE.md). -->
                  {#each optionsFor(node) as option (option.mhz)}
                    <option value={option.mhz}>{option.label}</option>
                  {/each}
                </select>
                {#if chan.detail}
                  <p class="channel-detail" role="status">{chan.detail}</p>
                {/if}
                {#if chan.tunedOn !== undefined && chan.tunedOn !== chan.mhz}
                  <!-- The thing nothing else announces: `on_set_frequency` writes the frequency into
                       the SAME profile row that holds enter_ats/exit_ats, so the levels came through
                       this change untouched — tuned for the channel the node just left. Factual, not
                       alarming: they are unverified here, not necessarily wrong. -->
                  <p class="channel-stale" data-testid={`channel-stale-${node}`} role="status">
                    {staleThresholdNote(chan.tunedOn, chan.mhz, catalog)}
                  </p>
                {/if}
                {#if clashingNodes.has(node)}
                  <!-- Flagged, never blocked: two gates on one frequency is a real mistake, and also
                       exactly what a bench swap looks like halfway through. -->
                  <p class="channel-clash" data-testid={`channel-clash-${node}`} role="status">
                    {duplicateChannelNote(node, sharingWith(node))}
                  </p>
                {/if}
                <p class="channel-note">{HEAT_OVERWRITES_CHANNEL}</p>
              </div>
            {:else if offerable.has(node)}
              <!-- The node exists and is enabled, but the timer has not said what it is tuned to.
                   No dropdown resting on a fabricated default: the RD would change away from a
                   channel they never actually saw. Saying so beats an unexplained gap. -->
              <p class="channel-waiting" data-testid={`channel-waiting-${node}`} role="status">
                This node has not reported a channel yet.
              </p>
            {/if}

            {#if dead}
              <!-- No controls: there is no node there to write a threshold to. -->
            {:else if held}
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
                        disabled={!canControl}
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
                        disabled={!canControl}
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
              <p class="node-waiting" role="status">
                {everLoaded
                  ? 'This node has not reported its thresholds yet.'
                  : 'Waiting for this node to report its levels…'}
              </p>
            {/if}

            <!-- Kept even for a dead node: six dashes say "nothing reported", which is information.
                 Six zeroes would be a lie, and it is the lie an RD would tune against. -->
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
  /* The channel control (#413). Sits above the thresholds because it is the question that comes
     first: a level tuned on the wrong channel is not a tuned gate. */
  .channel {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .channel-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-2);
  }
  .channel-label {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .channel-select {
    width: 100%;
    min-width: 0;
    padding: var(--gf-space-2);
    font: inherit;
    color: var(--gf-text);
    background: var(--gf-surface-2);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
  }
  .channel-select:disabled {
    opacity: 0.6;
  }
  .channel-detail,
  .channel-stale,
  .channel-clash,
  .channel-note,
  .channel-waiting {
    margin: 0;
    font-size: var(--gf-font-size-xs);
    line-height: 1.4;
  }
  .channel-detail {
    color: var(--gf-danger);
  }
  /* Both of these are NOTICES, not failures: the thresholds are unverified rather than wrong, and a
     shared channel is legitimate mid-swap. Toned as such — an alarm here would train the RD to
     ignore the one that matters. */
  .channel-stale,
  .channel-clash {
    color: var(--gf-warn);
  }
  .channel-note,
  .channel-waiting {
    color: var(--gf-text-muted);
  }

  /* The scope line (#411): which layer this page is editing, stated plainly under the title. Sized
     like data rather than chrome — it is the answer to "am I changing tonight or forever". */
  .scope {
    margin: 0;
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: var(--gf-space-2);
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-secondary);
  }
  .scope-label {
    color: var(--gf-text-muted);
    text-transform: uppercase;
    font-size: var(--gf-font-size-2xs);
    letter-spacing: 0.06em;
  }
  .scope-target {
    font-weight: var(--gf-font-weight-semibold);
    color: var(--gf-text);
  }
  .scope-note {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-xs);
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
  .head-controls {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
  }
  .layout-toggle {
    display: flex;
    gap: var(--gf-space-1);
    align-items: center;
  }
  .feed-status {
    display: inline-flex;
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

  /* **Never wider than four.** Past four nodes the grid wraps to a new row rather than growing
     sideways: an eight-node timer scrolls VERTICALLY, which is the natural direction, instead of
     hiding half its nodes off the right edge. Each cell keeps a 20rem floor so a plot stays wide
     enough to read a crossing in; `auto-fit` collapses the count on narrow viewports, so a small
     laptop lands on two or three rather than four cramped ones. Horizontal overflow is not a
     fallback here — it is the thing being removed. */
  .nodes {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(20rem, 1fr));
    gap: var(--gf-space-4);
    padding-bottom: var(--gf-space-2);
    /* The hard four-wide cap. `auto-fit` alone would keep adding columns on a wide monitor, so the
       track width is bounded to exactly four minimum cells plus their three gaps; beyond that the
       grid wraps downward instead of outward. Narrower viewports simply fit fewer. */
    max-width: calc(4 * 20rem + 3 * var(--gf-space-4));
  }
  /* Stacked is the same markup pinned to a single column — one node per row, full width. */
  .nodes.stacked {
    grid-template-columns: minmax(0, 1fr);
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
  /* A node the timer has never reported. Recessed rather than alarming: the column still holds its
     place in the row (so the node numbering stays readable across eight of them) but reads at a
     glance as absent, not as a live node sitting quiet. */
  .node.dead {
    border-style: dashed;
    background: var(--gf-surface-2);
  }
  .node-dead {
    margin: 0;
    padding: var(--gf-space-4) 0;
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
