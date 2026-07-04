<script lang="ts">
  /**
   * Marshaling (#55, Slice 3) — the timer-agnostic lap-level review surface.
   *
   * The RD reviews each competitor's laps (from the heat's `LapList` projection, marshaling already
   * folded in) and corrects them by **selecting a lap** and acting on it — never by hand-typing a log
   * offset. Each lap carries the global `start_ref`/`end_ref` offsets of its bounding passes, so a
   * selected lap maps to the exact `LogRef` a correction command targets (void / split / edit-time on
   * the lap's end pass; insert-after between this lap and the next). Per-competitor rulings (DQ, time
   * penalty, reverse a ruling) act on the competitor.
   *
   * Every correction *appends* a marshaling event; the projection re-folds and the corrected lap list,
   * the audit trail, AND the standings (the live stream) all refresh by the same append→re-fold→re-read
   * path — nothing reconciled locally (architecture.html §3). Marshaling is the **do the work** page:
   * the full reverse-chronological audit history lives on the event-wide **Audit page** now; this
   * screen keeps a compact **Recent rulings** strip (the marshaled heat's latest entries, same shared
   * rendering) plus a "View full audit →" jump pre-filtered to the marshaled heat (the auditFilter
   * seam). The heat-scoped audit read stays — the Reverse-ruling / Resolve-protest pickers and the
   * open-protest Finalize gate all derive from it. Mutating controls are **role-gated**: a
   * read-only-pilot session sees the laps + strip but every action is hidden (the Director enforces
   * the boundary; this mirrors it).
   */
  import type {
    AuditEntry,
    AuditKind,
    ChannelCatalogEntry,
    CompetitorRef,
    CompetitorTrace,
    HeatId,
    HeatSummary,
    Lap,
    LapList,
    LogRef,
    Pilot,
    PilotId,
    PilotProgress,
    SignalTraceView
  } from '@gridfpv/types';
  import { formatMicros, Select, toast } from '@gridfpv/components';
  import type { AuditPrefilter } from '../lib/auditFilter.svelte.js';
  import {
    auditKindLabel,
    auditSummaryLine,
    auditTime,
    rulingOptionLabel,
    summaryTargetRef,
    type AuditRenderInputs
  } from '../lib/auditRender.js';
  import { channelLabel } from '../lib/channels.js';
  import { createCompetitorNameResolver } from '../lib/competitorName.js';
  import { heatNameById } from '../lib/heats.js';
  import {
    adjustLapCommand,
    applyPenaltyCommand,
    deductPointsCommand,
    disqualifyPenalty,
    fileProtestCommand,
    insertLapCommand,
    resolveProtestCommand,
    reverseRulingCommand,
    secondsToSourceTime,
    splitLapCommand,
    throwOutLapCommand,
    timeAddedPenalty,
    voidDetectionCommand,
    voidHeatCommand
  } from '../lib/marshaling.js';
  import type { ProtestOutcome } from '@gridfpv/types';
  import {
    DEFAULT_MATCH_TOLERANCE_MICROS,
    defaultThresholds,
    detectPasses,
    diffPasses,
    officialPasses,
    previewRows
  } from '../lib/redetect.js';
  import { commandForAction } from '../lib/transitions.js';
  import type { Session } from '../lib/session.svelte.js';
  import { useProtestClock, formatProtest } from '../lib/protestClock.svelte.js';
  import ConfirmButton from '../lib/ConfirmButton.svelte';
  import ErrorBanner from '../lib/ErrorBanner.svelte';
  import RssiGraph from '../lib/RssiGraph.svelte';

  let {
    session,
    adapter = 'rh-1',
    onviewaudit = undefined
  }: {
    session: Session;
    adapter?: string;
    /**
     * Jump to the event-wide Audit page pre-filtered (the "View full audit →" strip action).
     * The shell wires this to the auditFilter seam (`openAudit(setTab, prefilter)`), the same way
     * sibling screens receive navigation callbacks.
     */
    onviewaudit?: (prefilter: AuditPrefilter) => void;
  } = $props();

  // Which heat to marshal. Defaults to — and tracks — Race Control's current heat, but the RD can
  // pin ANY heat to marshal it. Marshaling issues no `SetCurrentHeat`, so switching the marshaled
  // heat here does NOT change what Race Control shows/controls (the explicit requirement). An empty
  // pin (`undefined`) means "follow the live current heat".
  let marshalHeatPin = $state<HeatId | undefined>(undefined);
  const currentHeat = $derived<HeatId | undefined>(session.liveState?.current_heat);
  const heat = $derived<HeatId | undefined>(marshalHeatPin ?? currentHeat);
  const laps = $derived<LapList | undefined>(session.lapList);
  const audit = $derived<AuditEntry[] | undefined>(session.marshalingAudit);
  const canControl = $derived(session.canControl);

  // The MARSHALED heat's own live state — it may NOT be Race Control's current heat (the heat
  // picker, #4). Prefer the heat-scoped fold (`refreshMarshaling` pulls ?projection=live over the
  // marshaled heat); fall back to the global stream only when the marshaled heat IS the current one.
  // So the lifecycle badge + the result transitions read the heat *under review*, not Race Control's.
  const marshalLive = $derived(
    session.heatLiveState?.current_heat === heat
      ? session.heatLiveState
      : session.liveState?.current_heat === heat
        ? session.liveState
        : undefined
  );
  // The marshaled heat's loop phase — drives which result transition (Finalize / Revert) is offered.
  const marshalPhase = $derived(marshalLive?.phase);
  // An OFFICIAL (Final) result is LOCKED: the Director rejects every result-changing marshaling
  // command on it (`require_not_final`, control_handler.rs) — mirror that gate here so the screen
  // never offers a correction the server will bounce. Revert (the sanctioned re-open) is surfaced
  // prominently in the lock banner; PROTESTS stay available (filing/resolving changes no result,
  // so the Director exempts them). Inspection (laps, audit, tune preview) stays live too —
  // committing is what's locked.
  const resultLocked = $derived(marshalPhase === 'Final');
  // The result lifecycle (marshaling Slice 5): Provisional (Unofficial) vs Official (Final), with the
  // auto-official countdown when the round armed a protest window. The Finalize/Revert transitions
  // below now act on it from here too (B) — targeting the marshaled heat, never Race Control's.
  const lifecycle = $derived(marshalLive?.lifecycle);
  const protest = useProtestClock(
    () => marshalLive?.lifecycle,
    () => session.serverNowMs()
  );

  // The captured RSSI trace for this heat (`?projection=signal`, Slice 1), pulled alongside the
  // lap list + audit by `refreshMarshaling`. A heat that captured signal (a RotorHazard heat) has
  // one or more competitor traces; a **sim heat** has none — `hasTrace` is then false and the
  // signal-as-evidence graph is skipped, leaving today's lap-only layout (marshaling.html §3.2).
  const signalTrace = $derived<SignalTraceView | undefined>(session.signalTrace);
  const hasTrace = $derived((signalTrace?.competitors.length ?? 0) > 0);

  // ── Friendly names everywhere (heat name + pilot callsigns) ───────────────────────────────────
  // Marshaling — like Live control — knows the heat and its competitors only as raw ids/refs (the
  // live stream + lap list carry no human labels). It resolves them here, at the call site that has
  // the directory + round context, and renders display strings: the heat's "<Round> Heat N" name and
  // every competitor's callsign (lap headings, the selection legend, the ruling/protest dropdowns,
  // and the audit lines). The competitor resolution is the SHARED resolver Live control uses
  // (`competitorName.ts`) so the two never drift (the raw-ref bug, #214 follow-up).

  // The app-level directories (callsigns) + scheduled heats + channel catalog — open reads, re-pulled
  // whenever the stream advances so a freshly-registered pilot / scheduled heat resolves live.
  //
  // Both reads re-run on `session.currentEvent` as well as `session.protocolState`. This is the fix
  // for the raw-id regression (#236 follow-up): `listHeats()` resolves `[]` until an event is
  // selected, and Marshaling can mount **while the active event is still resolving** (a cold reload
  // straight onto the Marshaling tab, or a remount before the stream's first envelope). Keying the
  // re-read off `protocolState` alone left `heats` (and so the friendly heat name) — and `pilots`
  // (the callsigns) — empty whenever that first read raced ahead of `currentEvent`, since a quiet
  // Unofficial heat then emits no further stream tick to retrigger it. Touching `currentEvent` makes
  // the read fire again the moment the event resolves, so the header heat name and the lap-list
  // headings populate even when no stream advance follows. Live control reads the same way; it just
  // never hit this because it is the default tab that mounts after the event is already in hand.
  let pilots = $state<Pilot[]>([]);
  let heats = $state<HeatSummary[]>([]);
  let catalog = $state<ChannelCatalogEntry[]>([]);
  // A FAILED pilots/heats directory read must be visible (#340): swallowing it into an empty array
  // left every ref rendering raw with no hint anything was wrong. Track a load-error flag per read
  // (keeping the last good data rather than wiping it), surface a "Couldn't load — retry" state +
  // a toast (the Results-screen pattern), and let the RD retry explicitly via the nonce.
  let pilotsError = $state(false);
  let heatsError = $state(false);
  let directoryRetryNonce = $state(0);
  const directoryError = $derived(pilotsError || heatsError);
  function retryDirectory(): void {
    directoryRetryNonce += 1;
  }
  /** Toast once on the transition INTO the error state (the effects re-run on every stream tick). */
  function noteDirectoryError(alreadyFailing: boolean): void {
    if (!alreadyFailing)
      toast.error('Couldn’t load the pilot/heat directory — names may show as raw ids.');
  }
  $effect(() => {
    void session.currentEvent;
    void session.protocolState;
    void directoryRetryNonce;
    session
      .listPilots()
      .then((p) => ((pilots = p), (pilotsError = false)))
      .catch(() => {
        noteDirectoryError(directoryError);
        pilotsError = true;
      });
  });
  $effect(() => {
    void session.currentEvent;
    void session.protocolState;
    void directoryRetryNonce;
    session
      .listHeats()
      .then((h) => ((heats = h), (heatsError = false)))
      .catch(() => {
        noteDirectoryError(directoryError);
        heatsError = true;
      });
  });
  $effect(() => {
    session
      .listChannels()
      .then((c) => (catalog = c))
      .catch(() => (catalog = []));
  });

  const pilotById = $derived(new Map<PilotId, Pilot>(pilots.map((p) => [p.id, p])));
  // A competitor ref → its explicitly-bound pilot id, from the heat's **durable** registration
  // binding (`progress.pilot`). Sourced from the MARSHALED heat's own live-state fold
  // (`session.heatLiveState`, `?projection=live` over that heat's window — pulled by
  // `refreshMarshaling`), NOT the global live stream's current heat (`session.liveState`). The
  // marshaled heat is frequently NOT the current one — a finished / non-current / node-seeded heat
  // under review — so the global stream carries no progress for it and a `node-0` ref fell through
  // to the raw "node-0" label. The heat-window fold carries the heat's `CompetitorRegistered` binds,
  // so a bound `node-0 → pilot` resolves its callsign for ANY heat (the raw-ref bug, #214 follow-up).
  // The global stream's progress is merged underneath as a fallback so the current heat still
  // resolves immediately on first mount, before the heat-scope snapshot lands.
  const explicitPilotByRef = $derived.by(() => {
    const map = new Map<CompetitorRef, PilotId>();
    const add = (progress: readonly PilotProgress[] | undefined): void => {
      for (const p of progress ?? []) if (p.pilot != null) map.set(p.competitor, p.pilot);
    };
    // Fallback: the global stream — but ONLY when its current heat IS the marshaled heat. Node-seat
    // refs (`node-0`) are reused across heats, so merging the live heat's bindings while marshaling
    // a DIFFERENT heat captioned this heat's laps with the other heat's pilots (worse than raw).
    if (session.liveState?.current_heat === heat) add(session.liveState?.progress);
    // Authoritative: the marshaled heat's durable binding — but only once the heat-scope fold is for
    // THIS heat (a stale fold from a just-deselected heat could re-bind a reused `node-0` ref wrong).
    if (session.heatLiveState?.current_heat === heat) add(session.heatLiveState?.progress);
    return map;
  });
  // The current heat's competitor ref → channel-label map (for the open-practice `node-{i}` seat
  // fallback), joined off the heat's `frequencies` like Live control's channels panel.
  const currentChannels = $derived.by(() => {
    const summary = heats.find((h) => h.heat === heat);
    const map = new Map<CompetitorRef, string>();
    for (const [ref, mhz] of summary?.frequencies ?? []) map.set(ref, channelLabel(mhz, catalog));
    return map;
  });

  // The shared competitor → callsign resolver (same rule as Live control).
  const competitorName = $derived.by<(ref: CompetitorRef) => string>(() =>
    createCompetitorNameResolver({ pilotById, explicitPilotByRef, channelByRef: currentChannels })
  );
  // The current heat's friendly "<Round> Heat N" / "Open Practice Heat" name (the raw id otherwise).
  const heatName = $derived(
    heat ? heatNameById(heat, heats, session.currentEvent?.rounds ?? []) : ''
  );

  // Drive the marshaling reads off the live stream: whenever the current heat (or the stream's
  // cursor — a new appended event, e.g. a correction we or another client made) changes, re-pull
  // the heat's lap list + audit so corrections flow in live.
  let lastKey = $state<string | undefined>(undefined);
  $effect(() => {
    const h = heat;
    const cursor = session.protocolState?.cursor;
    if (!h) return;
    const key = `${h}:${cursor ?? ''}`;
    if (key === lastKey) return;
    lastKey = key;
    void session.refreshMarshaling(h);
  });

  // ── Lap selection ──
  // A selection identifies a competitor + a lap by its end_ref (the lap's stable identity / the
  // natural correction target). `null` when nothing is selected.
  let selected = $state<{ competitor: CompetitorRef; lap: Lap } | null>(null);

  function selectLap(competitor: CompetitorRef, lap: Lap): void {
    if (selected && selected.competitor === competitor && selected.lap.end_ref === lap.end_ref) {
      selected = null; // toggle off
    } else {
      selected = { competitor, lap };
    }
  }

  function isSelected(competitor: CompetitorRef, lap: Lap): boolean {
    return (
      selected !== null &&
      selected.competitor === competitor &&
      selected.lap.end_ref === lap.end_ref
    );
  }

  // Clear a stale selection if the lap it pointed at no longer exists after a re-fold.
  $effect(() => {
    if (!selected || !laps) return;
    const sel = selected;
    const cl = laps.competitors.find((c) => c.competitor.competitor === sel.competitor);
    const stillThere = cl?.laps.some((l) => l.end_ref === sel.lap.end_ref);
    if (!stillThere) selected = null;
  });

  // ── Inline corrections on the SELECTED lap ──
  // Edit-time / split / insert-after take a time input (seconds, source clock). The entered value
  // must be a POSITIVE number: an empty/zeroed input reads as 0, and sending it (`AdjustLap at: 0`)
  // re-times the pass to the race start and wrecks the whole lap chain. The buttons disable on an
  // invalid value (with the explaining title) and the handlers refuse with a toast as a backstop —
  // never a silent send of 0. (An emptied number input binds `null`, hence the typeof check.)
  let editSeconds = $state<number | null>(0);
  const editSecondsValid = $derived(typeof editSeconds === 'number' && editSeconds > 0);
  const editSecondsTitle = 'Enter a positive time (s) first';
  /** The validated time input (µs), or `undefined` — toasting the refusal (visible feedback). */
  function editTimeMicros(): number | undefined {
    const seconds = editSeconds;
    if (typeof seconds !== 'number' || !(seconds > 0)) {
      toast.error('Enter a positive time (seconds) first.');
      return undefined;
    }
    return secondsToSourceTime(seconds);
  }

  // One shared in-flight guard for the correction / ruling / lifecycle submits (the `committing`
  // pattern): every correction APPENDS a ruling, so a double-clicked Apply lands the penalty
  // TWICE. Each handler runs under this — a click while a previous send is still in flight
  // no-ops — and the buttons disable on it so the double-submit can't even be attempted.
  let busy = $state(false);
  async function submitCorrection(run: () => Promise<void>): Promise<void> {
    if (busy) return;
    busy = true;
    try {
      await run();
    } finally {
      busy = false;
    }
  }

  async function afterCorrection(): Promise<void> {
    // The send already recorded any error; on success the stream cursor advances and the
    // $effect re-pulls. Re-pull immediately too so the panel updates without waiting for the
    // next stream tick (idempotent — a fresh snapshot).
    if (heat) await session.refreshMarshaling(heat);
  }

  function doSplitSelected(): Promise<void> {
    return submitCorrection(async () => {
      if (!selected) return;
      const at = editTimeMicros();
      if (at === undefined) return;
      const ack = await session.send(splitLapCommand(selected.lap.end_ref, at));
      if (ack.ok) await afterCorrection();
    });
  }

  function doEditTimeSelected(): Promise<void> {
    return submitCorrection(async () => {
      if (!selected) return;
      const at = editTimeMicros();
      if (at === undefined) return;
      const ack = await session.send(adjustLapCommand(selected.lap.end_ref, at));
      if (ack.ok) await afterCorrection();
    });
  }

  function doInsertAfterSelected(): Promise<void> {
    return submitCorrection(async () => {
      if (!selected || !heat) return;
      const at = editTimeMicros();
      if (at === undefined) return;
      const ack = await session.send(insertLapCommand(adapter, selected.competitor, at, heat));
      if (ack.ok) await afterCorrection();
    });
  }

  // ── Add a brand-new lap (not an edit of an existing one) ──
  // The `InsertLap` path adds a missed/never-detected crossing for a competitor at a source-clock
  // time — so it works even when the competitor has ZERO laps. Two entry points feed it the time:
  //   • the graph: the explicit "Add lap here" button in the cursor readout under the trace passes
  //     the cursor's race-relative source-time (never a bare click on the trace — stray clicks and
  //     drag-end synthesized clicks must not plant laps);
  //   • the per-competitor control: an "Add lap" button at a typed time (seconds), for sim heats
  //     with no trace/graph.
  // Role-gated by `canControl` like every other correction (the parent only renders these when the
  // session may control; the Director re-checks).

  /** Add a lap for a competitor at an exact source-clock time (µs) — the graph's button path. */
  function insertLap(competitor: CompetitorRef, at: number): Promise<void> {
    return submitCorrection(async () => {
      if (!canControl || resultLocked || !heat) return;
      const ack = await session.send(insertLapCommand(adapter, competitor, Math.round(at), heat));
      if (ack.ok) await afterCorrection();
    });
  }

  // The "Add lap" control lives IN the lap-times box (one per competitor entry): a small
  // toggle that opens an inline typed-time row — no separate panel, no competitor picker (the
  // box IS the competitor). Works with zero existing laps (sim heats included).
  let addLapOpen = $state(false);
  let addLapSeconds = $state<number | null>(0);
  const addLapSecondsValid = $derived(typeof addLapSeconds === 'number' && addLapSeconds > 0);
  async function doAddLapAtTime(competitor: CompetitorRef): Promise<void> {
    if (!canControl || typeof addLapSeconds !== 'number' || !(addLapSeconds > 0)) {
      toast.error('Enter a positive time (seconds) first.');
      return;
    }
    await insertLap(competitor, secondsToSourceTime(addLapSeconds));
    addLapOpen = false;
  }

  /** Remove (void) a lap straight from its row — the one-click removal on the lap list. */
  function doVoidLap(competitor: CompetitorRef, lap: Lap): Promise<void> {
    return submitCorrection(async () => {
      const ack = await session.send(voidDetectionCommand(lap.end_ref));
      if (ack.ok) {
        if (selected && selected.competitor === competitor && selected.lap.end_ref === lap.end_ref)
          selected = null;
        await afterCorrection();
      }
    });
  }

  // Throw out the selected lap from the SCORED count — distinct from "Remove (void)": the lap stays
  // a real lap, it just no longer counts (marshaling.html §3.3). Targets the lap's end pass.
  function doThrowOutSelected(): Promise<void> {
    return submitCorrection(async () => {
      if (!selected) return;
      const ack = await session.send(throwOutLapCommand(selected.lap.end_ref));
      if (ack.ok) {
        selected = null;
        await afterCorrection();
      }
    });
  }

  // ── Per-competitor rulings ──
  let penaltyTarget = $state<CompetitorRef | ''>('');
  // 'dq' = disqualify (status), 'time' = added time (per-heat), 'points' = standings deduction.
  let penaltyKind = $state<'dq' | 'time' | 'points'>('dq');
  // The amount inputs bind `null` when emptied, so they're validated (positive) before any send.
  let penaltySeconds = $state<number | null>(2);
  let penaltyPoints = $state<number | null>(1);
  let dqReason = $state('');
  function doPenalty(): Promise<void> {
    return submitCorrection(async () => {
      if (!heat || !penaltyTarget) return;
      // An invalid amount is REFUSED with visible feedback — never a silent no-op (the old early
      // return read as "it worked"): a non-positive time "penalty" would credit time, and a
      // 0-point DeductPoints would append a no-op ruling to the record.
      const target = penaltyTarget;
      let ack;
      if (penaltyKind === 'points') {
        const points = typeof penaltyPoints === 'number' ? Math.round(penaltyPoints) : NaN;
        if (!(points > 0)) {
          toast.error('Enter a positive whole number of points to deduct.');
          return;
        }
        // Points affect SEASON/EVENT standings, not the per-heat lap result.
        ack = await session.send(deductPointsCommand(heat, target, points));
      } else if (penaltyKind === 'time') {
        const seconds = penaltySeconds;
        if (typeof seconds !== 'number' || !(seconds > 0)) {
          toast.error('Enter a positive number of seconds for a time penalty.');
          return;
        }
        ack = await session.send(applyPenaltyCommand(heat, target, timeAddedPenalty(seconds)));
      } else {
        ack = await session.send(applyPenaltyCommand(heat, target, disqualifyPenalty(dqReason)));
      }
      if (ack.ok) {
        dqReason = '';
        await afterCorrection();
      }
    });
  }

  // Reverse any prior reversible ruling — a penalty, a lap throw-out, a protest resolution, or a
  // heat-void — selected from the audit trail (generalized reversibility, marshaling.html §3.3).
  const REVERSIBLE_KINDS: AuditKind[] = [
    'PenaltyApplied',
    'LapThrownOut',
    'ProtestResolved',
    'HeatVoided'
  ];
  // Already-reversed rulings are excluded — offering them again invited a double
  // ReverseRuling on the same target (a confusing rejection at best).
  const reversibleRulings = $derived(
    (audit ?? []).filter(
      (e) => REVERSIBLE_KINDS.includes(e.kind) && !reversedRulingTargets.has(e.at_ref)
    )
  );
  let reverseTargetRef = $state<number | ''>('');
  function doReverse(): Promise<void> {
    return submitCorrection(async () => {
      if (reverseTargetRef === '') return;
      const ack = await session.send(reverseRulingCommand(reverseTargetRef as LogRef));
      if (ack.ok) {
        reverseTargetRef = '';
        await afterCorrection();
      }
    });
  }

  // ── Protests (file → resolve) ──
  let protestTarget = $state<CompetitorRef | ''>('');
  let protestNote = $state('');
  function doFileProtest(): Promise<void> {
    return submitCorrection(async () => {
      if (!heat || !protestTarget || protestNote.trim() === '') return;
      const ack = await session.send(fileProtestCommand(heat, protestTarget, protestNote.trim()));
      if (ack.ok) {
        protestNote = '';
        protestTarget = '';
        await afterCorrection();
      }
    });
  }
  // Filed protests are resolvable by their log offset (the audit entry's `at_ref`).
  const filedProtests = $derived((audit ?? []).filter((e) => e.kind === 'ProtestFiled'));
  // Ruling offsets a later `RulingReversed` undid. The audit entry carries its target only inside
  // the server-baked summary ("Ruling reversed (ref N)"), so it is parsed back out here.
  const reversedRulingTargets = $derived.by(() => {
    const targets = new Set<number>();
    for (const e of audit ?? []) {
      if (e.kind !== 'RulingReversed') continue;
      const t = summaryTargetRef(e.summary);
      if (t !== undefined) targets.add(t);
    }
    return targets;
  });
  // Open (unresolved) protests, matching the server's Finalize gate (`open_protest_count`,
  // control_handler.rs — the source of truth): a filing is closed only by an EFFECTIVE resolution —
  // a `ProtestResolved` targeting it that was NOT itself undone by a `RulingReversed`. The old
  // filed-minus-resolved count diverged the moment a resolution was reversed: the server counted
  // the protest as open again and rejected Finalize while the UI still offered it (#340).
  // Finalizing while a protest is open would lock a result that's still under dispute, so the
  // Finalize action is gated on this being zero.
  const openProtestCount = $derived.by(() => {
    const resolvedFilings = new Set<number>();
    for (const e of audit ?? []) {
      if (e.kind !== 'ProtestResolved' || reversedRulingTargets.has(e.at_ref)) continue;
      const t = summaryTargetRef(e.summary);
      if (t !== undefined) resolvedFilings.add(t);
    }
    return filedProtests.filter((e) => !resolvedFilings.has(e.at_ref)).length;
  });
  let resolveProtestRef = $state<number | ''>('');
  let protestOutcome = $state<ProtestOutcome>('Upheld');
  function doResolveProtest(): Promise<void> {
    return submitCorrection(async () => {
      if (resolveProtestRef === '') return;
      const ack = await session.send(
        resolveProtestCommand(resolveProtestRef as LogRef, protestOutcome)
      );
      if (ack.ok) {
        resolveProtestRef = '';
        await afterCorrection();
      }
    });
  }

  function doVoidHeat(): Promise<void> {
    return submitCorrection(async () => {
      if (!heat) return;
      const ack = await session.send(voidHeatCommand(heat));
      if (ack.ok) await afterCorrection();
    });
  }

  // Result-lifecycle transitions on the MARSHALED heat (B). These act on `heat` (the picker's heat),
  // never Race Control's current heat — marshaling issues no SetCurrentHeat. Same append→re-fold path
  // as every correction, so the badge + buttons update via afterCorrection.
  function doFinalize(): Promise<void> {
    return submitCorrection(async () => {
      if (!heat || openProtestCount > 0) return;
      const ack = await session.send(commandForAction('Finalize', heat));
      if (ack.ok) await afterCorrection();
    });
  }

  function doRevert(): Promise<void> {
    return submitCorrection(async () => {
      if (!heat) return;
      const ack = await session.send(commandForAction('Revert', heat));
      if (ack.ok) await afterCorrection();
    });
  }

  // Competitors that can be acted on: those in the lap list, else THE MARSHALED HEAT's own
  // scheduled lineup (the heats directory). Never the global live stream's `active_pilots` —
  // that is whichever heat happens to be running NOW, and marshaling frequently pins a
  // different heat: the old fallback offered the live heat's pilots in the ruling/protest/
  // add-lap dropdowns, letting the RD record a DQ against a pilot who never flew this heat.
  // DE-DUPLICATED by ref: the same competitor can appear under TWO adapters in one heat
  // (a mid-heat source failover — or historically a re-raced heat before the current-run
  // window fix), and duplicate refs crashed every keyed {#each} over this list.
  const competitors = $derived<CompetitorRef[]>(
    laps && laps.competitors.length > 0
      ? [...new Set(laps.competitors.map((c) => c.competitor.competitor))]
      : (heats.find((h) => h.heat === heat)?.lineup ?? [])
  );

  // Marshal one pilot at a time (declutter): a dropdown picks the pilot, and the graph + lap list
  // below scope to just them. `shownPilot` is the chosen pilot if still in the field, else the first
  // competitor — so it self-heals when the heat changes without an extra effect.
  let selectedPilot = $state<CompetitorRef | undefined>(undefined);
  const shownPilot = $derived<CompetitorRef | undefined>(
    selectedPilot !== undefined && competitors.includes(selectedPilot)
      ? selectedPilot
      : competitors[0]
  );
  const shownTrace = $derived<SignalTraceView | undefined>(
    signalTrace
      ? {
          competitors: signalTrace.competitors.filter((c) => c.competitor.competitor === shownPilot)
        }
      : undefined
  );
  const hasShownTrace = $derived((shownTrace?.competitors.length ?? 0) > 0);
  const shownLaps = $derived<LapList | undefined>(
    laps
      ? {
          ...laps,
          competitors: laps.competitors.filter((c) => c.competitor.competitor === shownPilot)
        }
      : undefined
  );

  // ── Tune detection (the RH-style threshold re-detection, marshaling.html §5) ──────────────────
  // The RD moves the enter/exit levels live — number inputs here, two-way with the graph's drag
  // handles — and the screen re-runs the timer's hysteresis over the CAPTURED trace (redetect.ts),
  // previewing the resulting lap list + the diff against the current official passes. NOTHING is
  // sent while adjusting: an explicit Commit turns the diff into the existing marshaling primitives
  // (a `VoidDetection` per removed pass, a heat-tagged `InsertLap` per added one). The thresholds
  // themselves are a UI/preview concern only — they are NEVER written back to the timer; pushing
  // calibration to RotorHazard via the plugin is a separate future feature. Scoped to the shown
  // pilot (the "Marshal pilot" picker already selects exactly one).
  const tuneTrace = $derived<CompetitorTrace | undefined>(
    signalTrace?.competitors.find((c) => c.competitor.competitor === shownPilot)
  );
  let tuneFor = $state<CompetitorRef | undefined>(undefined);
  let tuneEnter = $state(0);
  let tuneExit = $state(0);
  // Whether the RD has ACTIVELY moved the levels this session (a drag or a typed edit). The
  // lap box only switches into re-detection preview on explicit intent — a trace whose
  // recorded thresholds happen to disagree with the official laps must not hijack the
  // interactive lap list on its own.
  let tuneTouched = $state(false);

  /** The trace's recorded thresholds; an unset trace falls back to a percentile derivation. */
  function recordedThresholds(t: CompetitorTrace): { enter: number; exit: number } {
    const fallback = defaultThresholds(t);
    return {
      enter: t.enter ?? fallback?.enter ?? 0,
      exit: t.exit ?? fallback?.exit ?? 0
    };
  }

  // (Re-)seed the tuning levels whenever the tuned competitor changes (pilot picked / heat
  // switched / trace arrives) — but never while the SAME competitor is being adjusted.
  $effect(() => {
    const t = tuneTrace;
    if (!t) {
      tuneFor = undefined;
      return;
    }
    if (tuneFor === t.competitor.competitor) return;
    tuneFor = t.competitor.competitor;
    tuneTouched = false;
    const rec = recordedThresholds(t);
    tuneEnter = rec.enter;
    tuneExit = rec.exit;
  });

  /** The graph's drag handles emit here — two-way with the number inputs. */
  function onGraphThresholds(competitor: CompetitorRef, enter: number, exit: number): void {
    if (competitor !== shownPilot) return;
    tuneTouched = true;
    tuneEnter = enter;
    tuneExit = exit;
  }

  // The LIVE preview: re-detect at the tuned levels, diff against the current official passes
  // (lap 1's opening pass + every lap's closing pass, from the marshaling-corrected lap list).
  const tuneValid = $derived(tuneEnter > tuneExit);
  // Flatten across entries: the shown pilot can hold several lap-list entries (one per
  // adapter after a mid-heat failover) — the tune diff must see ALL their official passes.
  const shownPilotLaps = $derived<Lap[]>((shownLaps?.competitors ?? []).flatMap((c) => c.laps));
  const detectedPassTimes = $derived<number[]>(
    tuneTrace && tuneValid && tuneFor === shownPilot
      ? detectPasses(tuneTrace, tuneEnter, tuneExit)
      : []
  );
  // The RD's removal record (`CompetitorLaps.voided`) — the lap list and the tuner share this
  // data, so a crossing the marshal explicitly voided is SUPPRESSED from re-detection (the
  // trace still shows it; without this the tuner kept offering the removed lap back as an add).
  const shownVoidedAt = $derived<number[]>(
    (shownLaps?.competitors ?? []).flatMap((c) => (c.voided ?? []).map((v) => v.at))
  );
  const redetectDiff = $derived(
    diffPasses(
      officialPasses(shownPilotLaps),
      detectedPassTimes,
      DEFAULT_MATCH_TOLERANCE_MICROS,
      shownVoidedAt
    )
  );
  const redetectDirty = $derived(redetectDiff.added.length > 0 || redetectDiff.removed.length > 0);
  // The UNIFIED preview: one chronological row list — kept/added laps interleaved with the
  // dropped official passes and the RD-voided crossings (which stay removed). Rendered IN the
  // lap-times box while tuning, so the lap list itself is the live detection readout.
  const previewRowList = $derived(
    previewRows(
      officialPasses(shownPilotLaps),
      detectedPassTimes,
      DEFAULT_MATCH_TOLERANCE_MICROS,
      shownVoidedAt
    )
  );
  const previewLapCount = $derived(
    previewRowList.filter((r) => r.status === 'kept' || r.status === 'added').length
  );
  /** Whether the lap box is in live-preview mode: the RD actively moved the levels AND the
   *  re-detection differs from the official record. Never entered passively. */
  const tuningPreview = $derived(
    canControl &&
      tuneTouched &&
      tuneTrace != null &&
      tuneFor === shownPilot &&
      tuneValid &&
      redetectDirty
  );

  function doResetThresholds(): void {
    if (!tuneTrace) return;
    tuneTouched = false;
    const rec = recordedThresholds(tuneTrace);
    tuneEnter = rec.enter;
    tuneExit = rec.exit;
  }

  // Commit: turn the previewed diff into the marshaling command batch — voids first (the passes
  // the tuned levels no longer see), then heat-tagged inserts (the newly detected ones) —
  // sequentially, so the audit reads in a sane order and a mid-batch failure stops cleanly
  // (the error banner shows; the refresh reflects whatever landed).
  let committing = $state(false);
  async function doCommitRedetect(): Promise<void> {
    if (
      !canControl ||
      resultLocked ||
      !heat ||
      !tuneTrace ||
      !tuneValid ||
      !redetectDirty ||
      committing
    )
      return;
    committing = true;
    try {
      const key = tuneTrace.competitor;
      const { added, removed } = redetectDiff;
      let ok = true;
      for (const pass of removed) {
        const ack = await session.send(voidDetectionCommand(pass.ref));
        if (!ack.ok) {
          ok = false;
          break;
        }
      }
      if (ok) {
        for (const at of added) {
          const ack = await session.send(
            insertLapCommand(key.adapter, key.competitor, Math.round(at), heat)
          );
          if (!ack.ok) {
            ok = false;
            break;
          }
        }
      }
      await afterCorrection();
      if (ok) {
        tuneTouched = false;
        const plus = added.length === 1 ? '+1 pass' : `+${added.length} passes`;
        toast.success(
          `Committed re-detection for ${competitorName(key.competitor)}: ${plus}, −${removed.length} removed`
        );
      }
    } finally {
      committing = false;
    }
  }

  // ── Audit rendering (the SHARED #337 recomposition — `lib/auditRender.ts`) ──
  // The line composition (resolved callsign first, the server-baked "(ref N)" stripped, the target
  // offset only trailing) is shared with the event-wide Audit page so the two never drift. This
  // screen supplies the one input only it has: the marshaled heat's LAP LIST, which resolves a
  // lap-addressed ruling's pass ref to the competitor whose lap it bounds.

  /** The competitor whose lap a pass offset (a lap's start/end ref) bounds, from the lap list. */
  function competitorForPassRef(target: number): CompetitorRef | undefined {
    for (const cl of laps?.competitors ?? [])
      for (const l of cl.laps)
        if (l.end_ref === target || l.start_ref === target) return cl.competitor.competitor;
    return undefined;
  }

  const renderInputs = $derived<AuditRenderInputs>({
    byRef: new Map<number, AuditEntry>((audit ?? []).map((e) => [e.at_ref, e])),
    competitorName,
    competitorForPassRef
  });

  // The Recent-rulings strip: the marshaled heat's latest few entries (the trail arrives newest
  // first). The FULL history — searchable, event-wide — lives on the Audit page; the strip only
  // answers "what just changed here?" at a glance.
  const RECENT_RULINGS = 3;
  const recentRulings = $derived((audit ?? []).slice(0, RECENT_RULINGS));
</script>

<section class="marshaling" aria-label="Marshaling">
  <header>
    <h2>
      Marshaling{#if heat}<span class="heat"> — {heatName}</span>{/if}
      {#if lifecycle}
        {#if lifecycle === 'Official'}
          <span class="lifecycle-badge official" aria-label="Result lifecycle">Official</span>
        {:else if protest.active}
          <span class="lifecycle-badge provisional" aria-label="Result lifecycle"
            >Provisional — auto-official in {formatProtest(protest.remainingMs)}</span
          >
        {:else}
          <span class="lifecycle-badge provisional" aria-label="Result lifecycle">Provisional</span>
        {/if}
      {/if}
    </h2>
    <p class="muted">
      {#if canControl}
        Select a lap to correct it. Every change is a recorded, reversible fact.
      {:else}
        Read-only — you can review the laps and audit trail, but not change them.
      {/if}
    </p>
  </header>

  {#if session.lastCommandError}
    <ErrorBanner error={session.lastCommandError} ondismiss={() => session.clearCommandError()} />
  {/if}

  {#if directoryError}
    <!-- A failed pilots/heats directory read (#340): without it, names silently fall back to raw
         refs with no hint anything went wrong. Visible error + explicit retry (Results pattern). -->
    <div class="dir-error" role="alert">
      <p>Couldn’t load the pilot/heat directory — names may show as raw ids.</p>
      <button type="button" onclick={retryDirectory}>Try again</button>
    </div>
  {/if}

  <div class="layout">
    <div class="main">
      {#if heats.length > 0}
        <!-- Marshal any heat: defaults to / follows Race Control's current heat, but the RD can pin
             another to marshal it without moving the current heat. -->
        <div class="heat-picker">
          <label for="marshal-heat">Marshal heat</label>
          <Select
            id="marshal-heat"
            value={marshalHeatPin ?? ''}
            aria-label="Heat to marshal"
            onchange={(e: Event) =>
              (marshalHeatPin = (e.currentTarget as HTMLSelectElement).value || undefined)}
          >
            <option value=""
              >Current heat (live){currentHeat
                ? ` — ${heatNameById(currentHeat, heats, session.currentEvent?.rounds ?? [])}`
                : ''}</option
            >
            {#each heats as h (h.heat)}
              <option value={h.heat}
                >{heatNameById(h.heat, heats, session.currentEvent?.rounds ?? [])}</option
              >
            {/each}
          </Select>
        </div>
      {/if}

      {#if competitors.length > 0}
        <!-- Marshal one pilot at a time: pick whose signal + laps to review (declutter). -->
        <div class="pilot-picker">
          <label for="marshal-pilot">Marshal pilot</label>
          <Select
            id="marshal-pilot"
            value={shownPilot ?? ''}
            aria-label="Pilot to marshal"
            onchange={(e: Event) => (selectedPilot = (e.currentTarget as HTMLSelectElement).value)}
          >
            {#each competitors as ref (ref)}
              <option value={ref}>{competitorName(ref)}</option>
            {/each}
          </Select>
        </div>
      {/if}

      {#if hasShownTrace && shownTrace}
        {@const tuning = canControl && tuneTrace != null && tuneFor === shownPilot}
        <!-- Signal-as-evidence (Slice 4): the RSSI graph for the selected pilot's captured trace. A
             marker click selects that lap in the action surface below; the lap-list selection
             highlights the same marker (two-way — `selectLap` is the one shared selection). With
             control, the graph also carries the LIVE tuning surface (marshaling.html §5): draggable
             enter/exit handles two-way with the "Tune detection" inputs below, plus the preview
             pass markers of the uncommitted re-detection diff. Sim heats (no trace) skip this. -->
        <RssiGraph
          trace={shownTrace}
          laps={shownLaps}
          {selected}
          onselect={selectLap}
          onaddlap={resultLocked ? undefined : insertLap}
          {canControl}
          nameFor={competitorName}
          onthresholds={tuning ? onGraphThresholds : undefined}
          tuned={tuning && shownPilot !== undefined
            ? { competitor: shownPilot, enter: tuneEnter, exit: tuneExit }
            : undefined}
          preview={tuning && shownPilot !== undefined && tuneValid
            ? {
                competitor: shownPilot,
                added: redetectDiff.added,
                removedRefs: redetectDiff.removed.map((p) => p.ref)
              }
            : undefined}
        />
      {/if}

      {#if canControl && hasShownTrace && tuneTrace && shownPilot !== undefined}
        <!-- Tune detection (RH-style re-detection): move the levels, watch the preview, COMMIT to
             make it official. The levels are a preview concern only — never written to the timer
             (pushing calibration to RotorHazard via the plugin is a separate future feature). -->
        <fieldset class="tune-detection">
          <legend>Tune detection — {competitorName(shownPilot)}</legend>
          <p class="muted hint">
            Drag the enter/exit handles on the graph or type levels here. Laps re-detect live as a
            preview — nothing changes until you commit.
          </p>
          <div class="row">
            <label
              >Enter
              <input
                type="number"
                step="1"
                bind:value={tuneEnter}
                oninput={() => (tuneTouched = true)}
                aria-label="Enter threshold"
              /></label
            >
            <label
              >Exit
              <input
                type="number"
                step="1"
                bind:value={tuneExit}
                oninput={() => (tuneTouched = true)}
                aria-label="Exit threshold"
              />
            </label>
            <button type="button" onclick={doResetThresholds}>Reset</button>
            <!-- The lock disables COMMITTING only — the sliders + preview above stay live, so an
                 official result can still be inspected at other levels without changing it. -->
            <button
              type="button"
              class="commit"
              onclick={doCommitRedetect}
              disabled={resultLocked || !tuneValid || !redetectDirty || committing}
              title={resultLocked
                ? 'This result is official — Revert it to make corrections.'
                : !tuneValid
                  ? 'Enter must be above exit'
                  : !redetectDirty
                    ? 'No change to commit — the re-detection matches the official passes'
                    : undefined}>Commit re-detection</button
            >
          </div>
          {#if !tuneValid}
            <p class="tune-invalid" role="status">
              Enter must be above exit — these levels detect nothing.
            </p>
          {:else}
            <!-- The panel is PURELY the enter/exit levels: the lap-times box below is the live
                 detection readout (it switches into preview mode while the diff is dirty). -->
            <p class="tune-summary" role="status" data-testid="redetect-summary">
              Would be {previewLapCount} lap{previewLapCount === 1 ? '' : 's'}
              (+{redetectDiff.added.length} added, −{redetectDiff.removed.length} removed{redetectDiff
                .suppressed.length > 0
                ? `, ${redetectDiff.suppressed.length} voided by you stay removed`
                : ''})
            </p>
          {/if}
        </fieldset>
      {/if}

      {#if shownLaps && shownLaps.competitors.length > 0}
        <div class="laps">
          <!-- Keyed by the FULL competitor key: the same ref can appear under two adapters
               (mid-heat failover), and a bare-ref key crashed the whole screen on it. -->
          {#each shownLaps.competitors as cl (cl.competitor.adapter + '/' + cl.competitor.competitor)}
            {@const canCorrect = canControl && !resultLocked}
            <div class="comp">
              <h4>
                {competitorName(cl.competitor.competitor)}
                {#if tuningPreview}<span class="preview-badge">re-detection preview</span>{/if}
              </h4>
              {#if tuningPreview}
                <!-- LIVE detection readout: the lap list the tuned levels would produce — new
                     laps marked +, official passes the levels drop struck −, and crossings the
                     RD already voided flagged (they STAY removed; the tuner never re-adds
                     them). Commit lives in the Tune panel above. -->
                <ol
                  class="preview-rows"
                  aria-label={`Re-detection preview for ${competitorName(cl.competitor.competitor)}`}
                >
                  {#each previewRowList as row (row.status === 'removed' ? `x${row.ref}` : `${row.status}${row.at}`)}
                    {#if row.status === 'removed'}
                      <li class="removed">
                        <span class="mark" aria-hidden="true">−</span>
                        <span class="what">pass at {formatMicros(row.at)}s — removed</span>
                      </li>
                    {:else if row.status === 'voided'}
                      <li class="voided">
                        <span class="mark" aria-hidden="true">∅</span>
                        <span class="what"
                          >crossing at {formatMicros(row.at)}s — voided by you, stays removed</span
                        >
                      </li>
                    {:else}
                      <li class={row.status}>
                        <span class="mark" aria-hidden="true"
                          >{row.status === 'added' ? '+' : ''}</span
                        >
                        <span class="lap-num">Lap {row.number}</span>
                        <span class="lap-time">{formatMicros(row.durationMicros)}</span>
                      </li>
                    {/if}
                  {/each}
                </ol>
              {:else}
                {#if cl.laps.length === 0 && (cl.voided ?? []).length === 0}
                  <p class="empty">No laps yet.</p>
                {:else}
                  <ol>
                    {#each cl.laps as lap (lap.end_ref)}
                      <li class="lap-row">
                        <button
                          type="button"
                          class="lap"
                          class:selected={isSelected(cl.competitor.competitor, lap)}
                          aria-pressed={isSelected(cl.competitor.competitor, lap)}
                          onclick={() => selectLap(cl.competitor.competitor, lap)}
                          title="Select to edit (split / re-time / insert / throw out)"
                        >
                          <span class="lap-num">Lap {lap.number}</span>
                          <span class="lap-time">{formatMicros(lap.duration_micros)}</span>
                        </button>
                        {#if canCorrect}
                          <button
                            type="button"
                            class="lap-remove"
                            onclick={() => doVoidLap(cl.competitor.competitor, lap)}
                            disabled={busy}
                            aria-label={`Remove lap ${lap.number}`}
                            title="Remove (void) this lap's closing pass">Remove</button
                          >
                        {/if}
                      </li>
                      {#if canCorrect && isSelected(cl.competitor.competitor, lap)}
                        <!-- The inline editor for the selected lap — the old separate
                             corrections box, moved to where the lap is. -->
                        <li class="lap-editor" aria-label={`Edit lap ${lap.number}`}>
                          <label class="time"
                            >Time (s)
                            <input
                              type="number"
                              step="0.001"
                              min="0.001"
                              bind:value={editSeconds}
                              aria-label="Correction time"
                            />
                          </label>
                          <button
                            type="button"
                            onclick={doSplitSelected}
                            disabled={busy || !editSecondsValid}
                            title={!editSecondsValid ? editSecondsTitle : undefined}>Split</button
                          >
                          <button
                            type="button"
                            onclick={doEditTimeSelected}
                            disabled={busy || !editSecondsValid}
                            title={!editSecondsValid ? editSecondsTitle : undefined}
                            >Edit time</button
                          >
                          <button
                            type="button"
                            onclick={doInsertAfterSelected}
                            disabled={busy || !editSecondsValid}
                            title={!editSecondsValid ? editSecondsTitle : undefined}
                            >Insert after</button
                          >
                          <button
                            type="button"
                            onclick={doThrowOutSelected}
                            disabled={busy}
                            title="Exclude this valid lap from the scored count (the lap stays real)"
                            >Throw out</button
                          >
                        </li>
                      {/if}
                    {/each}
                    {#each cl.voided ?? [] as v (v.pass_ref)}
                      <!-- The RD's removal record, kept visible where the lap was: the shared
                           data that also stops re-detection from re-proposing the crossing. -->
                      <li class="voided-row">
                        <span class="mark" aria-hidden="true">∅</span>
                        <span class="what"
                          >removed pass at {formatMicros(v.at)}s — stays removed</span
                        >
                      </li>
                    {/each}
                  </ol>
                {/if}
                {#if canCorrect}
                  <div class="add-lap-row">
                    {#if addLapOpen}
                      <label class="time"
                        >Time (s)
                        <input
                          type="number"
                          step="0.001"
                          min="0.001"
                          bind:value={addLapSeconds}
                          aria-label="Add-lap time"
                        />
                      </label>
                      <button
                        type="button"
                        onclick={() => doAddLapAtTime(cl.competitor.competitor)}
                        disabled={busy || !addLapSecondsValid}
                        title={!addLapSecondsValid ? editSecondsTitle : undefined}>Add</button
                      >
                      <button type="button" onclick={() => (addLapOpen = false)}>Cancel</button>
                    {:else}
                      <button
                        type="button"
                        class="add-lap"
                        onclick={() => (addLapOpen = true)}
                        title={hasTrace
                          ? 'Add a missed lap at a typed time — or click the trace on the graph'
                          : 'Add a missed lap at a typed time (source clock)'}>+ Add lap</button
                      >
                    {/if}
                  </div>
                {/if}
              {/if}
            </div>
          {/each}
        </div>
      {:else}
        <p class="empty">No lap list for this heat yet.</p>
        {#if canControl && !resultLocked && shownPilot !== undefined}
          <!-- A pilot with no recorded passes still needs the add path (a sim heat, a totally
               missed run): the same inline control, targeting the shown pilot. -->
          <div class="add-lap-row">
            {#if addLapOpen}
              <label class="time"
                >Time (s)
                <input
                  type="number"
                  step="0.001"
                  min="0.001"
                  bind:value={addLapSeconds}
                  aria-label="Add-lap time"
                />
              </label>
              <button
                type="button"
                onclick={() => doAddLapAtTime(shownPilot!)}
                disabled={busy || !addLapSecondsValid}
                title={!addLapSecondsValid ? editSecondsTitle : undefined}>Add</button
              >
              <button type="button" onclick={() => (addLapOpen = false)}>Cancel</button>
            {:else}
              <button type="button" class="add-lap" onclick={() => (addLapOpen = true)}
                >+ Add lap</button
              >
            {/if}
          </div>
        {/if}
      {/if}

      {#if canControl}
        {#if resultLocked}
          <!-- The official-result lock: the Director rejects every result-changing ruling on a
               Final heat, so the correction surfaces below are withheld entirely and the ONE way
               forward — Revert (the sanctioned re-open) — lives right here in the banner. Same
               handler + confirm semantics as the result-lifecycle control. Protests stay open
               below (filing/resolving changes no result; the server exempts them). -->
          <div class="official-lock" role="status" aria-label="Official result lock">
            <p>
              This result is official — Revert it to make corrections. Protests may still be filed.
            </p>
            <ConfirmButton onconfirm={doRevert} disabled={!heat || busy}
              >Revert → Unofficial</ConfirmButton
            >
          </div>
        {:else}
          <!-- Per-competitor rulings -->
          <fieldset>
            <legend>Competitor ruling</legend>
            <div class="row">
              <label
                >Competitor
                <select bind:value={penaltyTarget} aria-label="Ruling competitor">
                  <option value="" disabled>—</option>
                  {#each competitors as c (c)}<option value={c}>{competitorName(c)}</option>{/each}
                </select>
              </label>
              <label
                >Kind
                <select bind:value={penaltyKind} aria-label="Penalty kind">
                  <option value="dq">Disqualify</option>
                  <option value="time">Time added</option>
                  <option value="points">Points deducted</option>
                </select>
              </label>
              {#if penaltyKind === 'time'}
                <!-- A time penalty only worsens a result: a negative amount would silently CREDIT
                   time (improving the competitor), so the input floors at 0.1s and doPenalty
                   REFUSES a non-positive/empty amount with a visible toast (never a silent
                   no-op send). -->
                <label>
                  Seconds
                  <input type="number" step="0.1" min="0.1" bind:value={penaltySeconds} />
                </label>
              {:else if penaltyKind === 'points'}
                <label
                  >Points
                  <input
                    type="number"
                    step="1"
                    min="0"
                    bind:value={penaltyPoints}
                    aria-label="Points to deduct"
                  />
                </label>
              {:else}
                <label
                  >Reason (optional)
                  <input
                    type="text"
                    bind:value={dqReason}
                    placeholder="e.g. cut the course"
                    aria-label="DQ reason"
                  />
                </label>
              {/if}
              <button type="button" onclick={doPenalty} disabled={!penaltyTarget || !heat || busy}
                >Apply</button
              >
            </div>
            {#if penaltyKind === 'points'}
              <p class="muted hint">
                Points affect season / event standings, not this heat's laps.
              </p>
            {/if}
            <div class="row">
              <label
                >Reverse a ruling
                <select bind:value={reverseTargetRef} aria-label="Reverse ruling">
                  <option value="" disabled>—</option>
                  {#each reversibleRulings as p (p.at_ref)}
                    <option value={p.at_ref}>{rulingOptionLabel(p, renderInputs)}</option>
                  {/each}
                </select>
              </label>
              <button
                type="button"
                onclick={doReverse}
                disabled={reverseTargetRef === '' || reversibleRulings.length === 0 || busy}
                >Reverse ruling</button
              >
            </div>
          </fieldset>
        {/if}

        <!-- Protest sub-panel: file → resolve (append-only fact pair). NOT inside the Final lock:
             protests change no result, so they stay open on an official one (the Director allows
             them; an upheld protest is then acted on via Revert). -->
        <fieldset>
          <legend>Protests</legend>
          <div class="row">
            <label
              >Against
              <select bind:value={protestTarget} aria-label="Protest competitor">
                <option value="" disabled>—</option>
                {#each competitors as c (c)}<option value={c}>{competitorName(c)}</option>{/each}
              </select>
            </label>
            <label class="grow"
              >Note
              <input
                type="text"
                bind:value={protestNote}
                placeholder="What is being protested"
                aria-label="Protest note"
              />
            </label>
            <button
              type="button"
              onclick={doFileProtest}
              disabled={!protestTarget || protestNote.trim() === '' || !heat || busy}
              >File protest</button
            >
          </div>
          <div class="row">
            <label
              >Resolve
              <select bind:value={resolveProtestRef} aria-label="Resolve protest">
                <option value="" disabled>—</option>
                {#each filedProtests as p (p.at_ref)}
                  <option value={p.at_ref}>{rulingOptionLabel(p, renderInputs)}</option>
                {/each}
              </select>
            </label>
            <label
              >Outcome
              <select bind:value={protestOutcome} aria-label="Protest outcome">
                <option value="Upheld">Upheld</option>
                <option value="Denied">Denied</option>
                <option value="Withdrawn">Withdrawn</option>
              </select>
            </label>
            <button
              type="button"
              onclick={doResolveProtest}
              disabled={resolveProtestRef === '' || filedProtests.length === 0 || busy}
              >Resolve protest</button
            >
          </div>
        </fieldset>

        <fieldset class="result-actions">
          <legend>Heat result</legend>
          {#if marshalPhase === 'Unofficial'}
            {#if openProtestCount > 0}
              <p class="muted protest-block" role="status">
                Resolve {openProtestCount} open protest{openProtestCount === 1 ? '' : 's'} before finalizing.
              </p>
            {:else}
              <p class="muted">Provisional — finalize to lock the result as official.</p>
            {/if}
            <button
              type="button"
              class="finalize"
              onclick={doFinalize}
              disabled={!heat || openProtestCount > 0 || busy}
              title={openProtestCount > 0
                ? `Resolve ${openProtestCount} open protest(s) first`
                : undefined}>Finalize → Official</button
            >
          {:else if marshalPhase === 'Final'}
            <!-- The Revert control moved into the lock banner above (the one affordance, not two
                 identical buttons) — this panel just states the lifecycle. -->
            <p class="muted">Official — use Revert (in the banner above) to re-open the result.</p>
          {:else}
            <p class="muted">
              No result to finalize yet — this heat hasn’t finished (it’s {marshalPhase ??
                'not running'}).
            </p>
          {/if}
        </fieldset>

        {#if !resultLocked}
          <fieldset class="danger-zone">
            <legend>Void the heat</legend>
            <p class="muted">Throws out the whole heat — it will not count.</p>
            <ConfirmButton onconfirm={doVoidHeat} variant="danger" disabled={!heat || busy}>
              Void heat
            </ConfirmButton>
          </fieldset>
        {/if}
      {/if}
    </div>

    <!-- Recent rulings: the marshaled heat's latest few audit entries, at a glance. The FULL
         reverse-chronological history moved to the event-wide Audit page (searchable, filterable);
         the button below jumps there pre-filtered to this heat via the auditFilter seam. -->
    <aside class="audit" aria-label="Recent rulings">
      <h3>Recent rulings</h3>
      {#if recentRulings.length > 0}
        <ol class="audit-list">
          {#each recentRulings as entry (entry.at_ref)}
            <li class="audit-entry kind-{entry.kind}">
              <span class="audit-kind">{auditKindLabel(entry.kind)}</span>
              <span class="audit-summary">{auditSummaryLine(entry, renderInputs)}</span>
              {#if entry.at != null}
                <span class="audit-at">{auditTime(entry.at)}</span>
              {/if}
            </li>
          {/each}
        </ol>
      {:else}
        <p class="empty">No corrections yet — the raw timer output stands.</p>
      {/if}
      <button
        type="button"
        class="view-audit"
        onclick={() => onviewaudit?.(heat ? { heat } : {})}
        title="Open the event-wide audit trail, filtered to this heat"
      >
        View full audit →
      </button>
    </aside>
  </div>
</section>

<style>
  .marshaling {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
  }
  h2 {
    font-size: var(--gf-font-size-xl);
    margin: 0;
    letter-spacing: var(--gf-tracking-tight);
  }
  .heat {
    color: var(--gf-text-muted);
    font-weight: var(--gf-font-weight-normal);
  }
  /* Result lifecycle badge (marshaling Slice 5): Provisional (blue) vs Official (violet). */
  .lifecycle-badge {
    display: inline-block;
    margin-left: var(--gf-space-3);
    padding: 0.1em 0.6em;
    border-radius: var(--gf-radius-md);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-normal);
    vertical-align: middle;
    font-variant-numeric: tabular-nums;
  }
  .lifecycle-badge.provisional {
    color: color-mix(in srgb, var(--gf-phase-finished) 92%, var(--gf-text));
    background: color-mix(in srgb, var(--gf-phase-finished) 18%, var(--gf-elevated));
    border: 1px solid color-mix(in srgb, var(--gf-phase-finished) 45%, var(--gf-border));
  }
  .lifecycle-badge.official {
    color: color-mix(in srgb, var(--gf-phase-scored) 92%, var(--gf-text));
    background: color-mix(in srgb, var(--gf-phase-scored) 18%, var(--gf-elevated));
    border: 1px solid color-mix(in srgb, var(--gf-phase-scored) 45%, var(--gf-border));
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    margin: var(--gf-space-1) 0 0;
  }
  /* The directory-load error state (#340): visible, with an explicit retry. */
  .dir-error {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--gf-space-3);
    padding: var(--gf-space-3) var(--gf-space-4);
    border: 1px solid color-mix(in srgb, var(--gf-danger) 45%, var(--gf-border));
    border-radius: var(--gf-radius-md);
    background: var(--gf-danger-soft);
  }
  .dir-error p {
    margin: 0;
    color: var(--gf-text);
    font-size: var(--gf-font-size-sm);
  }
  /* The official-result lock banner: prominent (the field-readability bar), in the Official
     violet so it reads as the same lifecycle the header badge shows, with the Revert affordance
     right in it. */
  .official-lock {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-3);
    padding: var(--gf-space-3) var(--gf-space-4);
    border: 1px solid color-mix(in srgb, var(--gf-phase-scored) 45%, var(--gf-border));
    border-radius: var(--gf-radius-md);
    background: color-mix(in srgb, var(--gf-phase-scored) 14%, var(--gf-elevated));
  }
  .official-lock p {
    margin: 0;
    color: var(--gf-text);
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
  }
  .layout {
    display: grid;
    grid-template-columns: 2fr 1fr;
    gap: var(--gf-space-5);
    align-items: start;
  }
  .main {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .heat-picker,
  .pilot-picker {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    max-width: 22rem;
  }
  .heat-picker label,
  .pilot-picker label {
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
    color: var(--gf-text-secondary);
    white-space: nowrap;
  }
  .laps {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-4);
  }
  .comp {
    flex: 1 1 14rem;
    padding: var(--gf-space-4);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    background: var(--gf-elevated);
    box-shadow: var(--gf-shadow-xs);
  }
  .comp h4 {
    margin: 0 0 var(--gf-space-2);
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
  }
  .comp ol {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
  }
  .lap {
    width: 100%;
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: var(--gf-space-3);
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid transparent;
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    color: var(--gf-text-secondary);
    font-family: var(--gf-font-mono);
    font-size: var(--gf-font-size-sm);
    cursor: pointer;
    text-align: left;
  }
  .lap:hover {
    border-color: var(--gf-border-strong);
  }
  .lap.selected {
    border-color: var(--gf-accent);
    background: var(--gf-accent-soft);
    color: var(--gf-text);
  }
  .lap-num {
    font-weight: var(--gf-font-weight-semibold);
  }
  .empty {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  fieldset {
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    padding: var(--gf-space-4) var(--gf-space-5);
    background: var(--gf-elevated);
  }
  fieldset:disabled {
    opacity: 0.55;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: end;
    gap: var(--gf-space-3);
  }
  .row + .row {
    margin-top: var(--gf-space-3);
  }
  .grow {
    flex: 1 1 12rem;
  }
  .grow input {
    width: 100%;
  }
  .hint {
    margin: var(--gf-space-2) 0 0;
  }
  legend {
    font-weight: var(--gf-font-weight-semibold);
    font-size: var(--gf-font-size-sm);
    padding: 0 var(--gf-space-2);
  }
  label {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
    font-size: var(--gf-font-size-2xs);
    font-weight: var(--gf-font-weight-semibold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  input,
  select {
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-normal);
    text-transform: none;
    letter-spacing: normal;
    height: 2.1rem;
    padding: 0 var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    color: var(--gf-text);
  }
  input:focus,
  select:focus {
    outline: none;
    border-color: var(--gf-accent);
    box-shadow: 0 0 0 3px var(--gf-accent-soft);
  }
  button {
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
    height: 2.1rem;
    padding: 0 var(--gf-space-4);
    border-radius: var(--gf-radius-sm);
    border: 1px solid var(--gf-border);
    background: var(--gf-elevated);
    color: var(--gf-text);
    cursor: pointer;
    transition:
      background var(--gf-motion-fast) var(--gf-ease-out),
      border-color var(--gf-motion-fast) var(--gf-ease-out);
  }
  button:hover:not(:disabled) {
    background: var(--gf-elevated-hover);
    border-color: var(--gf-border-strong);
  }
  button:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
  button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .danger-zone {
    border-color: color-mix(in srgb, var(--gf-danger) 45%, var(--gf-border));
    background: var(--gf-danger-soft);
  }
  /* Tune detection: the live re-detection panel. Big readable summary — the "+A / −R" is what
     the marshal decides on, outdoors on a laptop (the field-readability bar). */
  .tune-detection {
    border-color: color-mix(in srgb, var(--gf-accent) 35%, var(--gf-border));
  }
  .tune-detection input[type='number'] {
    width: 6rem;
    font-size: var(--gf-font-size-md);
    font-family: var(--gf-font-mono);
  }
  .tune-summary {
    margin: var(--gf-space-3) 0 0;
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
    color: var(--gf-text);
    font-variant-numeric: tabular-nums;
  }
  .tune-invalid {
    margin: var(--gf-space-3) 0 0;
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
    color: color-mix(in srgb, var(--gf-danger) 80%, var(--gf-text));
  }
  .commit {
    border: 1px solid var(--gf-accent);
    background: var(--gf-accent-soft);
  }
  .commit:hover:not(:disabled) {
    background: var(--gf-accent);
    color: #061018;
  }
  /* The unified re-detection preview: one chronological list, big mono rows (sunlit-laptop
     readable). Kept rows read plain; added rows carry the accent "+" chip styling; removed
     rows read struck/dimmed danger — "this pass leaves the record on commit" at a glance. */
  .preview-badge {
    margin-left: var(--gf-space-2);
    font-size: var(--gf-font-size-2xs);
    font-weight: var(--gf-font-weight-semibold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-warn);
  }
  .lap-row {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
  }
  .lap-row .lap {
    flex: 1;
  }
  .lap-remove {
    flex: none;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-danger);
  }
  .lap-editor {
    display: flex;
    align-items: end;
    flex-wrap: wrap;
    gap: var(--gf-space-2);
    padding: var(--gf-space-2);
    margin: 0 0 var(--gf-space-2);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    background: var(--gf-surface-sunken);
    list-style: none;
  }
  .voided-row,
  .preview-rows .voided {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    color: var(--gf-text-faint);
    text-decoration: line-through;
    list-style: none;
    padding: var(--gf-space-1) 0;
  }
  .add-lap-row {
    display: flex;
    align-items: end;
    gap: var(--gf-space-2);
    margin-top: var(--gf-space-2);
  }

  .preview-rows {
    margin: var(--gf-space-2) 0 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
    max-width: 26rem;
  }
  .preview-rows li {
    display: flex;
    align-items: baseline;
    gap: var(--gf-space-2);
    padding: var(--gf-space-1) var(--gf-space-3);
    border: 1px solid transparent;
    border-radius: var(--gf-radius-sm);
    font-family: var(--gf-font-mono);
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-secondary);
  }
  .preview-rows .mark {
    /* Fixed-width status column so the lap labels align across kept/added/removed rows. */
    min-width: 1ch;
    font-weight: var(--gf-font-weight-semibold);
  }
  .preview-rows .lap-num {
    font-weight: var(--gf-font-weight-semibold);
  }
  .preview-rows li.added {
    border-color: color-mix(in srgb, var(--gf-accent) 55%, var(--gf-border));
    border-style: dashed;
    background: var(--gf-surface-sunken);
    color: var(--gf-text);
  }
  .preview-rows li.added .mark {
    color: var(--gf-accent);
  }
  .preview-rows li.removed {
    color: color-mix(in srgb, var(--gf-danger) 65%, var(--gf-text-muted));
    opacity: 0.8;
  }
  .preview-rows li.removed .what {
    text-decoration: line-through;
  }
  .result-actions {
    border-color: color-mix(in srgb, var(--gf-accent) 35%, var(--gf-border));
  }
  .protest-block {
    color: color-mix(in srgb, var(--gf-danger) 80%, var(--gf-text));
    font-weight: var(--gf-font-weight-semibold);
  }
  .finalize {
    border: 1px solid var(--gf-accent);
    background: var(--gf-accent-soft);
    color: var(--gf-text);
    font-weight: var(--gf-font-weight-semibold);
    padding: 0.3rem var(--gf-space-3);
    border-radius: var(--gf-radius-sm);
    cursor: pointer;
  }
  .finalize:hover:not(:disabled) {
    background: var(--gf-accent);
    color: #061018;
  }
  .audit {
    padding: var(--gf-space-4);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    background: var(--gf-elevated);
    box-shadow: var(--gf-shadow-xs);
    position: sticky;
    top: var(--gf-space-4);
  }
  .audit h3 {
    margin: 0 0 var(--gf-space-3);
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
  }
  .audit-list {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .audit-entry {
    display: grid;
    grid-template-columns: auto 1fr;
    grid-template-areas: 'kind summary' 'at at';
    gap: var(--gf-space-1) var(--gf-space-2);
    padding: var(--gf-space-2) var(--gf-space-3);
    border-left: 3px solid var(--gf-border-strong);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    font-size: var(--gf-font-size-sm);
  }
  .audit-entry.kind-Voided,
  .audit-entry.kind-HeatVoided {
    border-left-color: var(--gf-danger);
  }
  .audit-entry.kind-RulingReversed {
    border-left-color: var(--gf-accent);
  }
  .audit-kind {
    grid-area: kind;
    font-weight: var(--gf-font-weight-semibold);
    font-size: var(--gf-font-size-2xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .audit-summary {
    grid-area: summary;
    color: var(--gf-text);
  }
  .audit-at {
    grid-area: at;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-2xs);
    font-family: var(--gf-font-mono);
  }
  /* The jump to the event-wide Audit page (pre-filtered to the marshaled heat). */
  .view-audit {
    margin-top: var(--gf-space-3);
    width: 100%;
    border-color: color-mix(in srgb, var(--gf-accent) 35%, var(--gf-border));
  }
  .view-audit:hover:not(:disabled) {
    border-color: var(--gf-accent);
    color: var(--gf-accent);
    background: var(--gf-elevated);
  }
  @media (max-width: 70rem) {
    .layout {
      grid-template-columns: 1fr;
    }
    .audit {
      position: static;
    }
  }
</style>
