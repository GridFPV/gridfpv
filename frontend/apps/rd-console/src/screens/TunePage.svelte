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
   * ## The confirmation is a POLL, not a response
   *
   * `POST /timers/{id}/calibration` answers "accepted" and nothing more. RotorHazard does not echo a
   * level set synchronously — it broadcasts `enter_and_exit_at_levels`, which comes back to this page
   * as `NodeSignal.enter_at`/`exit_at` on a **later** `GET /signal`. So `Sending…` is not "waiting
   * for the HTTP response" (that already returned); it is waiting for the feed this page is already
   * polling to show the new level. If the polls keep disagreeing past `CONFIRM_TIMEOUT_MS` the
   * threshold says `Not taken`, loudly, on that node.
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
    HeatSummary,
    NodeSignal,
    Pilot,
    PilotId,
    PilotProgress,
    Timer,
    TimerId,
    TimerSignal
  } from '@gridfpv/types';
  import type { Action } from 'svelte/action';
  import type { Session } from '../lib/session.svelte.js';
  import RssiGraph from '../lib/RssiGraph.svelte';
  import Brand from '../Brand.svelte';
  import Breadcrumbs from '../Breadcrumbs.svelte';
  import { createCompetitorNameResolver } from '../lib/competitorName.js';
  import { isOpenPracticeRound } from '../lib/heats.js';
  import {
    CONFIRM_TIMEOUT_MS,
    RSSI_MAX,
    RSSI_MIN,
    SIGNAL_POLL_MS,
    clampLevel,
    foldPolled,
    isParsableLevel,
    markSent,
    nodeCountOf,
    nodeTraceOf,
    nodeTuneLabel,
    phaseLabel,
    phaseTone,
    plottable,
    readoutsOf,
    seedThreshold,
    writeGate,
    type ApplyLevels,
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
    onhome,
    ontimers,
    fetchSignal,
    applyLevels,
    stopSignal,
    pollMs = SIGNAL_POLL_MS,
    confirmMs = CONFIRM_TIMEOUT_MS
  }: {
    session: Session;
    /** The timer being tuned, resolved from the route by the shell (never a bare id here). */
    timer: Timer;
    /** Leave to the home hub (the brand mark + the first breadcrumb). */
    onhome: () => void;
    /** Leave to the Timers page (the second breadcrumb — where this page is entered from). */
    ontimers: () => void;
    /** Test/host seam for the signal poll; defaults to the session's `GET /timers/{id}/signal`. */
    fetchSignal?: FetchSignal;
    /** Test/host seam for the calibration write; defaults to the session's `setCalibration`. */
    applyLevels?: ApplyLevels;
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
  }

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
    return nodeById.get(node)?.frequency_mhz ?? timer.available_channels?.[node];
  }

  /** `Node 1 · Raceband R7` — the seat's own name, band+channel resolved through `channels.ts`. */
  function nodeLabel(node: number): string {
    return nodeTuneLabel(node, frequencyOf(node), catalog);
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
  const channelByRef = $derived(
    new Map<CompetitorRef, string>(
      (signal?.nodes ?? []).map((n) => [n.seat, nodeLabel(n.node)] as const)
    )
  );
  const competitorName = $derived.by<(ref: CompetitorRef) => string>(() =>
    createCompetitorNameResolver({ pilotById, explicitPilotByRef, channelByRef })
  );

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
