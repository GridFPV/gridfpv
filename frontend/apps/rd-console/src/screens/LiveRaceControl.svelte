<script lang="ts">
  /**
   * Live race control (#54) — the heart of the console.
   *
   * Shows the current heat (`HeatSheet` + `RaceClock` + live `Leaderboard` derived from
   * the `LiveRaceState` stream) and drives the heat loop: one button per legal
   * transition, each calling `session.send` with the matching `Command`. Illegal
   * actions for the current `phase` are disabled up front (see `transitions.ts`);
   * destructive off-ramps (Abort/Restart/Discard) confirm before firing. A failed
   * `CommandAck` surfaces through the shared `ErrorBanner`.
   */
  import {
    HeatSheet,
    RaceClock,
    Leaderboard,
    Card,
    Button,
    formatMicros,
    toast
  } from '@gridfpv/components';
  import type {
    ChannelCatalogEntry,
    CompetitorRef,
    HeatId,
    HeatResult,
    HeatSummary,
    LiveRaceState,
    Pilot,
    PilotProgress,
    RoundDef
  } from '@gridfpv/types';
  import { nodeIndexOf } from '../lib/channels.js';
  import { collapseStore } from '../lib/collapse.svelte.js';
  import { buildCompetitorNames } from '../lib/competitorName.js';
  import { gateGroups } from '../lib/gateSignal.js';
  import GateSignalStrip from '../lib/GateSignalStrip.svelte';
  import { useSignalFeed } from '../lib/signalFeed.svelte.js';
  import { SIGNAL_POLL_MS, type FetchSignal, type StopSignal } from '../lib/tuning.js';
  import { heatDisplayName, heatNameById, isOpenPracticeRound } from '../lib/heats.js';
  import {
    actionDescription,
    actionsForKind,
    commandsForAction,
    isActionLegal,
    actionLabel,
    isDestructive,
    primaryAction,
    type HeatAction,
    type HeatKind
  } from '../lib/transitions.js';
  import type { Session } from '../lib/session.svelte.js';
  import { useRaceClock } from '../lib/raceClock.svelte.js';
  import { fixedEndWindowMicros } from '../lib/raceWindow.js';
  import { useStagingClock, formatStaging } from '../lib/stagingClock.svelte.js';
  import { useProtestClock, formatProtest } from '../lib/protestClock.svelte.js';
  import { useArmingClock, formatArming } from '../lib/armingClock.svelte.js';
  import { raceDayAudio } from '../lib/raceDayAudio.svelte.js';
  import ConfirmButton from '../lib/ConfirmButton.svelte';
  import ErrorBanner from '../lib/ErrorBanner.svelte';

  let {
    session,
    names = {},
    fetchSignal,
    stopSignal,
    signalPollMs = SIGNAL_POLL_MS
  }: {
    session: Session;
    names?: Record<string, string>;
    /** Test/host seam for the gate-signal poll; defaults to `GET /timers/{id}/signal` (#415). */
    fetchSignal?: FetchSignal;
    /** Test/host seam for releasing the gate-signal lease; defaults to `POST .../signal/stop`. */
    stopSignal?: StopSignal;
    /** Gate-signal poll cadence (ms) — the thing that renews the Director's lease. */
    signalPollMs?: number;
  } = $props();

  const live = $derived<LiveRaceState | undefined>(session.liveState);
  const phase = $derived(live?.phase ?? 'Scheduled');
  const heat = $derived<HeatId | undefined>(live?.current_heat);
  // Role-gate the heat transitions (marshaling Slice 5, mirroring Slice 3): the RD commits; a
  // read-only pilot session sees the live race + lifecycle but the transition controls (Finalize /
  // Revert / …) are hidden — the Director is the enforced boundary, this reflects it client-side.
  const canControl = $derived(session.canControl);

  // ── Per-heat channels (race redesign Slice 4b) ───────────────────────────────
  // The live stream carries only `LiveRaceState` (no frequencies), so resolve the current heat's
  // channel assignment by joining `current_heat` against the heats list (which carries the
  // `HeatScheduled.frequencies`), labelled through the standard catalog. Both are open reads,
  // re-fetched whenever the stream advances (so a freshly-staged OR freshly-scheduled heat appears).
  let catalog = $state<ChannelCatalogEntry[]>([]);
  let heats = $state<HeatSummary[]>([]);
  // A FAILED pilots/heats directory read must be visible (#340): swallowing it into an empty array
  // left every ref/heat-id rendering raw with no hint anything was wrong. Track a load-error flag
  // per read (keeping the last good data rather than wiping it), surface a "Couldn't load — retry"
  // state + a toast (the Results-screen pattern), and let the RD retry explicitly via the nonce.
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
    session
      .listChannels()
      .then((c) => (catalog = c))
      .catch(() => (catalog = []));
  });
  $effect(() => {
    // Re-read the heats list on every stream update, not only when `liveState` content changes:
    // filling/scheduling a heat does NOT move `current_heat` (fill-no-steal, #191) and often leaves
    // the whole `LiveRaceState` body unchanged, so keying off `liveState` alone would leave the heat
    // picker stale until the next transition. `protocolState` is reassigned on every stream envelope
    // (the backend force-emits one when a heat is scheduled), so touching it refreshes the picker the
    // moment a heat appears — without changing `current_heat` (no focus steal).
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

  const lineup = $derived<CompetitorRef[]>(live?.active_pilots ?? []);

  // ── Friendly names everywhere (heat names + pilot callsigns) ─────────────────────────────────
  // Live Control knows heats and competitors only as raw ids/refs (the live `LiveRaceState` carries
  // no human labels). Resolve them here, at the one call site that has the directory + round context,
  // and pass display strings down — so every panel reads callsigns / "<Round> Heat N", never an id.

  // The app-level pilots directory (callsigns). An open read like the heats/channels lists above;
  // re-read whenever the stream advances so a freshly-registered pilot's callsign appears live.
  let pilots = $state<Pilot[]>([]);
  $effect(() => {
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

  // ── The gate signal (#415) ───────────────────────────────────────────────────────────────────
  //
  // Read-only live RSSI for the heat on the timer. Mid-race is exactly when an RD cannot tune and
  // most needs to know what the gate is seeing: a lap that does not register looks identical on the
  // board whether the craft is producing no signal at all, crossing under the enter threshold, or
  // not crossing — three faults, three different responses, one unmoving lap count. The trace, the
  // threshold lines and the crossing marks are what tell them apart.
  //
  // The feed is a LEASE, not a read. The poll cadence is what keeps the Director streaming, and
  // `useSignalFeed` is the other half of that bargain — it gives the lease back on unmount, on the
  // route change that unmounts this screen, and on `visibilitychange → hidden`, so an RD who walks
  // to the gate with the phone in their pocket does not leave a timer streaming to nobody. Exactly
  // what the Tune page does, because it is literally the same helper.
  //
  // Bandwidth was raised as an objection and disproved by measurement: 9,155 bytes per poll on a
  // full 8-node ring is ~36 KB/s at this cadence — 0.03% of a 1 Gb race network — and the
  // timer→Director link is unchanged either way (the `node_data` heartbeat already flows for the
  // marshaling trace; the lease only decides whether the Director buffers it). So there is no
  // throttling or gating here on purpose.
  //
  // Held only by a CONTROLLING session. `GET /timers/{id}/signal` is `ControlAuth`-gated at the
  // Director, so a read-only / pilot session cannot have it at all — subscribing anyway would earn
  // them a 401 rendered as "lost the timer's signal feed", which is a false statement about the
  // hardware in front of somebody who cannot act on it either way.
  const gateTimer = $derived(canControl ? session.primaryTimer : undefined);
  const gateFeed = useSignalFeed({
    timer: () => gateTimer?.id,
    // Defaults to the SESSION's calls, not a bare `fetch`: both routes are `ControlAuth`-gated, so
    // a hand-rolled fetch would 401 against any token-gated Director — including the RD's own.
    read: () => fetchSignal ?? ((id, opts) => session.timerSignal(id, { signal: opts.signal })),
    release: () => stopSignal ?? ((id) => session.stopTimerSignal(id)),
    pollMs: () => signalPollMs
  });

  // ONE assembly of the resolver inputs (#416). Every screen hands `buildCompetitorNames` the
  // sources it has and consumes the result — the three independent constructions this screen, the
  // Rounds & Heats stage and Marshaling each used to do are what put `node-6` on one screen and
  // `Node 7` on another for the same seat. This screen now holds a live signal subscription too,
  // so it can hand over `NodeSignal.frequency_mhz` — what each node is ACTUALLY tuned to, and the
  // only channel source that works on a Flexible RotorHazard timer (whose pool is empty).
  //
  // `progress` carries an **explicit** `pilot` only when a `Register` command bound it (the
  // open-practice / manual-registration path); it is empty for the common roster-seeded heat, where
  // the ref IS the pilot id (see `competitorName.ts` for the full rule).
  const seatNames = $derived(
    buildCompetitorNames({
      pilots,
      progress: live?.progress,
      heat: heats.find((h) => h.heat === heat),
      catalog,
      signal: gateFeed.snapshot,
      // The registry's timer, NOT `gateTimer` — the name inputs are needed by every session, and
      // `gateTimer` is deliberately absent for a read-only one (see the subscription above).
      timer: session.primaryTimer,
      membership: session.currentEvent?.classes_membership
    })
  );

  // The heat's own gates, then every other node the timer reports (including the ones RotorHazard
  // has never reported at all — "is node 3 even alive?" is half the diagnostic). Attribution is a
  // join, never a guess: an open-practice lineup IS node seats, and a competition lineup is paired
  // by channel only where exactly one node and exactly one competitor claim that frequency.
  const gates = $derived(gateGroups(gateFeed.snapshot, lineup, seatNames.mhzFor));

  // Always-visible or a toggle? A toggle — but one whose CLOSED state still answers the first
  // question. The strip's collapsed header carries a live chip per gate (reporting / crossing / not
  // reporting), so a dead node is visible at a glance without spending vertical space on eight
  // plots; opening it is for "how close was that pass to the enter line?". The choice sticks per
  // event, so an RD who wants it open all meeting gets it open on every heat.
  const collapseEventId = $derived(session.currentEvent?.id ?? 'event');
  const collapseStores = new Map<string, ReturnType<typeof collapseStore>>();
  function collapse(sectionId: string, defaultOpen: boolean): ReturnType<typeof collapseStore> {
    const key = `${collapseEventId}:${sectionId}`;
    let s = collapseStores.get(key);
    if (!s) {
      s = collapseStore(collapseEventId, sectionId, defaultOpen);
      collapseStores.set(key, s);
    }
    return s;
  }
  const gateCollapse = $derived(collapse('race-gate-signal', false));
  const gateOthersCollapse = $derived(collapse('race-gate-signal-others', false));

  // `heatName(id)` → the friendly "<Round> Heat N" / "Open Practice Heat" name for a heat id (the
  // same helper the picker uses), falling back to the bare id for an untagged/free-text heat. Used
  // for the current-heat title and the on-deck heat.
  function heatName(id: HeatId | undefined): string {
    if (!id) return '';
    return heatNameById(id, heats, session.currentEvent?.rounds ?? []);
  }

  // `competitorName(ref)` → the **callsign** (directory pilot), else an open-practice seat's
  // `"Node 7 · Raceband R7"` label, else the bare ref. The single shared resolver every
  // pilot/lineup/leaderboard row goes through — the same one Marshaling and the Rounds & Heats
  // stage use, built from the same inputs (see `competitorName.ts` for the full rule and why the
  // binding comes from the always-available roster binding, not race progress, so a callsign shows
  // whether the heat is Scheduled, Staged, Running, or done).
  const competitorName = $derived.by<(ref: CompetitorRef) => string>(() => seatNames.name);
  /** The channel a lineup seat is on, or `undefined` when GridFPV genuinely does not know. */
  const channelOf = $derived.by<(ref: CompetitorRef) => string | undefined>(
    () => seatNames.channelFor
  );
  const hasChannels = $derived(lineup.some((ref) => channelOf(ref) !== undefined));

  // A plain ref → display-name record for the shared `HeatSheet` (which takes `names`). Built over
  // the union of the lineup and the progress rows so every rendered row resolves.
  const resolvedNames = $derived.by<Record<CompetitorRef, string>>(() => {
    const out: Record<CompetitorRef, string> = {};
    const refs = new Set<CompetitorRef>([
      ...lineup,
      ...(live?.running_order ?? []),
      ...(live?.progress ?? []).map((p) => p.competitor)
    ]);
    for (const ref of refs) out[ref] = competitorName(ref);
    // A caller-supplied `names` override still wins (e.g. a future explicit mapping).
    return { ...out, ...names };
  });

  // ── Heat picker (manual current-heat selection) ──────────────────────────────────────────────
  // Filling a new heat no longer steals Live control's focus (the backend's current_heat only moves
  // on a real transition or an explicit selection), so the RD picks which heat to show/control here.
  // Each option is labelled with the shared "<Round> Heat N" / "Open Practice Heat" name (the same
  // helper the Rounds & Heats stage uses), derived from the heat's round off `currentEvent.rounds`
  // and its position within that round's heats. Untagged/free-text heats fall back to the bare id.
  interface HeatOption {
    heat: HeatId;
    label: string;
    isCurrent: boolean;
  }
  const heatOptions = $derived.by<HeatOption[]>(() => {
    const rounds = session.currentEvent?.rounds ?? [];
    return heats.map((h) => {
      const round = h.round ? rounds.find((r) => r.id === h.round) : undefined;
      const inRound = round ? heats.filter((x) => x.round === round.id) : [];
      const label = round ? heatDisplayName(round, h, inRound) : h.heat;
      return { heat: h.heat, label, isCurrent: h.heat === heat };
    });
  });

  // The picker is **locked** once the current heat is mid-commit — its phase is Staged/Armed/Running.
  // After Stage you're committed to that race; you switch only by aborting it back to Scheduled or
  // after it finishes to Unofficial/Final (or when there is no current heat). This mirrors the
  // backend's authoritative rejection (control_handler.rs) so the disabled control matches what the
  // server would refuse.
  const pickerLocked = $derived(phase === 'Staged' || phase === 'Armed' || phase === 'Running');

  // The select's displayed value is a LOCAL `$state` pinned to the true current heat — never a raw
  // one-way `value={heat}` binding. A one-way binding only re-asserts the shown value when `heat`
  // itself changes, so if a pick is *rejected* (locked here, or the backend's
  // `reject_if_current_heat_committed`) or interacted with during the brief window before
  // `pickerLocked`/`phase` catch up from the stream, the `<select>` would stick on the user's drifted
  // choice — and that stale selection would then take effect once the picker unlocks (e.g. after an
  // Abort). Pinning `pickValue` and snapping it back closes that defer-apply gap.
  let pickValue = $state<HeatId | ''>('');
  $effect(() => {
    // Reset to the true current heat whenever it changes OR whenever the picker is locked: any drift
    // (a rejected/locked interaction, or a timing-window change) snaps straight back to `heat`, so no
    // stale/pending selection can survive to apply after the picker unlocks.
    void pickerLocked;
    pickValue = heat ?? '';
  });

  // Picking a heat records `SetCurrentHeat`; the live stream then re-folds and follows it. Only sends
  // when NOT locked and the target differs from the current heat; a no-op/locked attempt leaves the
  // pinned `pickValue` to snap back to the current heat via the effect above.
  async function pickHeat(target: HeatId) {
    if (!target || target === heat || pickerLocked) {
      // Re-pin immediately so a rejected/no-op attempt cannot leave a drifted selection behind.
      pickValue = heat ?? '';
      return;
    }
    await session.setCurrentHeat(target);
  }
  function onPick(e: Event & { currentTarget: HTMLSelectElement }) {
    void pickHeat(e.currentTarget.value as HeatId);
  }

  // ── Race clock (#62) ────────────────────────────────────────────────────────────────
  // The phase-driven elapsed clock lives in the shared `useRaceClock` helper so the persistent
  // ContextHeader (#85) and this screen drive the *same* clock from one place. It is
  // server-time-authoritative (#62 follow-up): it counts from the live state's `race_started_at`
  // / `race_ended_at` (the server's race-go / race-end instants), so this HUD and the header
  // agree exactly and the frozen value is the precise server-side duration. See raceClock.svelte.ts.
  const clock = useRaceClock(
    () => phase,
    () => live?.race_started_at,
    () => live?.race_ended_at,
    () => session.serverNowMs()
  );
  const elapsedMs = $derived(clock.elapsedMs);

  // ── The current heat's round config (heat-lifecycle Slice 3) ─────────────────────────────────
  // The staging countdown length, the start procedure, and the start-tone cue are per-round config
  // (`RoundDef`), not on the live `LiveRaceState`. Resolve them by joining the current heat to its
  // round: heat → `HeatSummary.round` → `RoundDef` off `currentEvent.rounds`. Absent (a sim / free-
  // text heat with no round tag, or pre-Slice-2 meta) ⇒ the engine defaults (5:00 staging, default
  // tone) apply, which is exactly what an absent round yields below.
  const currentRound = $derived.by<RoundDef | undefined>(() => {
    const summary = heats.find((h) => h.heat === heat);
    const roundId = summary?.round;
    if (!roundId) return undefined;
    return session.currentEvent?.rounds?.find((r) => r.id === roundId);
  });
  // The round's staging window in seconds (default 5:00) — the staging countdown counts down from it.
  const stagingSecs = $derived(currentRound?.staging_timer_secs ?? 300);

  // ── Open-practice per-channel board (open-practice Slice 2) ───────────────────────────────────
  // The casual **open-practice** format runs one open heat over the active **channels**: its live
  // `LiveRaceState` rows are unbound (`pilot: null`) with competitor refs `node-{i}` (the timer
  // seat). Rather than the pilot-keyed channels/heat-sheet panels, this heat reads as a per-channel
  // practice board — each row a seat, named `Node 7 · Raceband R7` through the shared seat-label
  // builder (#416), with its laps, last lap, and best lap.
  const isOpenPractice = $derived(currentRound ? isOpenPracticeRound(currentRound) : false);
  // The transition model's second axis (#393): an open-practice heat is scored by nobody, so it
  // drops the result-ceremony verbs (Finalize / Advance / Revert) and its `Restart` is spelled
  // "Run again". Everything else about the heat loop — phases, commands, the engine — is identical.
  const heatKind = $derived<HeatKind>(isOpenPractice ? 'Practice' : 'Competition');
  // The obvious next step for this heat: `Finalize` at the end of a competition run, `Run again`
  // (Restart) at the end of a practice one. Declared here, with the kind it depends on.
  const primary = $derived(primaryAction(phase, heatKind));

  // One per-channel board row: its node index, channel label, laps, last lap, and best lap (µs).
  interface ChannelRow {
    node: number;
    ref: CompetitorRef;
    label: string;
    laps: number;
    lastLapMicros: number | undefined;
    bestLapMicros: number | undefined;
  }
  // Best lap isn't carried on the live stream (`PilotProgress` is laps + last lap), so the board
  // tracks it client-side: the min `last_lap_micros` observed per channel over the run. It resets
  // whenever the heat changes, and a "Run again" (Restart) re-windows the heat's laps to the new
  // run — see the reset below, which mirrors that so the board's best lap is this run's best lap.
  //
  // Known gap: this is a fold over the FRAMES, not over the latest state, so a lap whose frame the
  // console never saw cannot enter it. A re-snapshot has always skipped laps this way, and since
  // #422 a reconnect that genuinely missed envelopes is delivered as one settled catch-up fold
  // rather than a per-offset replay, so those laps' times are not re-walked either. The count and
  // last lap stay correct; only the *best* can read slower than the run's true best. The real fix
  // is the server carrying best-lap in `PilotProgress` — until then, don't paper over it here by
  // re-deriving laps client-side.
  let bestByRef = $state<Map<CompetitorRef, number>>(new Map());
  let bestForHeat = $state<HeatId | undefined>(undefined);
  $effect(() => {
    // On a heat change — or a "Run again", which re-stages the SAME heat — wipe the accumulated
    // bests. `Restart` windows the heat's laps past the reset server-side (`heat_window_offsets`),
    // so the board must start the new run clean too rather than carrying the last run's best.
    if (heat !== bestForHeat || phase === 'Scheduled') {
      bestForHeat = heat;
      if (bestByRef.size > 0) bestByRef = new Map();
    }
    if (!isOpenPractice) return;
    let changed = false;
    const next = new Map(bestByRef);
    for (const p of live?.progress ?? []) {
      const last = p.last_lap_micros;
      if (last === undefined || last === null) continue;
      const prev = next.get(p.competitor);
      if (prev === undefined || last < prev) {
        next.set(p.competitor, last);
        changed = true;
      }
    }
    if (changed) bestByRef = next;
  });

  // The board rows, in node order: every active `node-{i}` channel with its live laps. A channel
  // with no laps yet still shows (a quiet seat reads "0 laps").
  const channelRows = $derived<ChannelRow[]>(buildChannelRows(live));
  function buildChannelRows(state: LiveRaceState | undefined): ChannelRow[] {
    const byRef = new Map<CompetitorRef, PilotProgress>(
      (state?.progress ?? []).map((p) => [p.competitor, p])
    );
    const refs = state?.active_pilots ?? [];
    const rows: ChannelRow[] = [];
    for (const ref of refs) {
      const node = nodeIndexOf(ref);
      if (node === undefined) continue; // Not an open-practice channel ref — skip defensively.
      const p = byRef.get(ref);
      rows.push({
        node,
        ref,
        // The seat's own name, through the shared builder — never `available_channels[node]`,
        // which is empty (and so answers "unknown" for every node) on a Flexible timer.
        label: seatNames.seatLabel(node),
        laps: p?.laps_completed ?? 0,
        lastLapMicros: p?.last_lap_micros ?? undefined,
        bestLapMicros: bestByRef.get(ref)
      });
    }
    return rows.sort((a, b) => a.node - b.node);
  }

  // ── Going again (#393) ────────────────────────────────────────────────────────────────────────
  // There is deliberately no second "new run" control here. The board used to carry one that
  // re-filled the round, on the pre-#398 theory that a fresh heat was how you cleared the
  // in-memory laps — but an open-practice round has exactly ONE heat, ever (`OpenPractice::next`
  // completes after it), so once the run had ended that fill scheduled nothing and still acked ok:
  // a button that reported success and did not clear the board. Re-running practice is the
  // transition row's **Run again** (the `Restart` command), which re-stages the one heat and
  // windows the previous run's laps away — one obvious action, in the place every other heat
  // action lives.

  // ── Staging countdown (heat-lifecycle Slice 3) ───────────────────────────────────────────────
  // While the heat is Staged, count down from the round's staging window. Informational only — no
  // auto-advance; it goes negative (over-time) past zero so the RD sees a field that isn't ready.
  const staging = useStagingClock(
    () => phase,
    () => stagingSecs,
    // Server-anchored (#62 family): every console counts the SAME staging window, from the
    // server's Staged instant — not each console's own mount time.
    () => live?.staged_at,
    () => session.serverNowMs()
  );

  // ── Auto-official countdown (marshaling Slice 5) ─────────────────────────────────────────────
  // While the heat is Unofficial (provisional) and the round armed a protest window, count the wall
  // clock down to the logged auto-official deadline so the RD sees "auto-official in M:SS". Inactive
  // (no countdown) when the window is Off — the result then stays provisional until manual Finalize.
  const protest = useProtestClock(
    () => live?.lifecycle,
    () => session.serverNowMs()
  );

  // ── Start-tone countdown (RD-console-only) ───────────────────────────────────────────────────
  // While the heat is Armed, count the wall clock down to the start tone (the server-authoritative
  // `tone_at`: when the heat auto-advances Armed → Running). The start delay is **intentionally
  // random to pilots**, so this countdown is RD-control-only: it is fed `tone_at` ONLY for a
  // controlling session, so a read-only / pilot session never arms it (and the banner below shows
  // them the generic "stand by" instead). `tone_at` is cleared by the backend once Running, so a
  // late join after race-go sees no stale countdown.
  const arming = useArmingClock(
    () => (canControl ? live?.tone_at : undefined),
    () => session.serverNowMs()
  );

  // ── Race-day audio — APP-WIDE (see lib/raceDayAudio.svelte.ts) ────────────────────────────────
  // The tones + callouts controller is mounted once in App.svelte and follows the live stream on
  // every page; this screen only exposes the Callouts toggle and unlocks audio on control clicks.
  const audio = raceDayAudio();

  // ── The known fixed race end (end-of-race tones + the countdown clock) ────────────────────────
  // Only a heat whose end instant the clock already knows gets a countdown: a Timed win-condition
  // round (window measured from race-go), or a Practice run with a time limit. First-to-N / BestLap
  // rounds have no fixed end. The derivation is shared (`raceWindow.ts`) so the tones, this HUD's
  // clock, and the header's clock all agree.
  const windowMicros = $derived(fixedEndWindowMicros(currentRound));

  // The HUD clock: a fixed-end heat counts DOWN from the window — past zero it runs negative
  // (the grace window: late crossings still score) and the RaceClock styles it red, with a
  // warn-yellow closing stretch before the buzzer. No fixed end ⇒ the classic count-up.
  const remainingMs = $derived(
    windowMicros !== undefined ? windowMicros / 1000 - elapsedMs : undefined
  );

  let muted = $state(audio.muted);
  function toggleMute() {
    // The toggle is itself a user gesture — the controller unlocks + warms the engine too.
    muted = audio.toggleMuted();
  }

  // A live, provisional leaderboard from the running order + per-pilot progress, so the
  // RD sees standings before the heat is scored. Built into a `HeatResult` so we reuse
  // the shared `Leaderboard` component (laps + last-lap-time as the metric).
  const liveResult = $derived<HeatResult | undefined>(buildLiveResult(live));

  function buildLiveResult(state: LiveRaceState | undefined): HeatResult | undefined {
    if (!state || !state.progress || state.progress.length === 0) return undefined;
    const order = state.running_order ?? state.active_pilots ?? [];
    const byRef = new Map(state.progress.map((p) => [p.competitor, p]));
    const ranked = (order.length > 0 ? order : state.progress.map((p) => p.competitor)).filter(
      (ref) => byRef.has(ref)
    );
    return {
      places: ranked.map((ref, i) => {
        const p = byRef.get(ref)!;
        return {
          competitor: { adapter: '', competitor: ref },
          position: i + 1,
          laps: p.laps_completed,
          metric: { BestLapMicros: p.last_lap_micros ?? null },
          // No per-pilot fastest lap in the live progress stream; this provisional board does not
          // tie-break on it, so leave it absent.
          best_lap_micros: null
        };
      })
    };
  }

  async function fire(action: HeatAction) {
    if (!heat) return;
    // Unlock the audio context on this user gesture (autoplay policy) so the later race-go tone is
    // audible — every transition click counts, well before the heat reaches Running.
    audio.resume();
    // Almost every action is one command; practice's "Run again" from an auto-finalized heat is two
    // (re-open, then reset), so the screen walks the sequence and stops on the first failure — the
    // error banner surfaces it, and a half-applied sequence leaves the heat where the engine put it.
    for (const command of commandsForAction(action, heat, phase, heatKind)) {
      const ack = await session.send(command);
      if (!ack.ok) return;
    }
    // Finalizing locks in the heat result; pull it so the Results screen has it to show. The
    // live stream only carries `LiveRaceState`, so the scored `HeatResult` is a separate
    // heat-scope fetch (`?projection=result`).
    if (action === 'Finalize') {
      await session.fetchHeatResult(heat);
    }
  }
</script>

<section class="live-control" aria-label="Race control">
  <header class="hud" data-phase={phase}>
    <div class="hud-heat">
      <span class="label">Current heat</span>
      <div class="heat-id">
        {#if heat}
          <span class="value">{heatName(heat)}</span>
        {:else}
          <span class="value none">— none on the timer —</span>
        {/if}
      </div>
      {#if heatOptions.length > 0}
        <!-- Heat picker: choose which heat Live control shows/controls (records SetCurrentHeat). The
             value is bound to the live current heat, so it follows transitions/selections too. -->
        <label class="heat-pick">
          <span class="visually-hidden">Select current heat</span>
          <select
            class="heat-pick-select"
            aria-label="Select current heat"
            bind:value={pickValue}
            onchange={onPick}
            disabled={pickerLocked}
          >
            {#if !heat}
              <option value="" disabled>— pick a heat —</option>
            {/if}
            {#each heatOptions as opt (opt.heat)}
              <option value={opt.heat}>{opt.label}{opt.isCurrent ? ' (current)' : ''}</option>
            {/each}
          </select>
        </label>
        {#if pickerLocked}
          <p class="heat-pick-hint" data-testid="heat-pick-lock-hint">
            Locked while a heat is staged/running — abort or finish to switch
          </p>
        {/if}
      {/if}
    </div>

    <div class="hud-phase">
      <span class="label">Phase</span>
      <div class="phase" data-phase={phase}>
        <span class="phase-dot" aria-hidden="true"></span>{phase}
      </div>
    </div>

    <div class="hud-clock">
      <span class="label">{remainingMs !== undefined ? 'Remaining' : 'Heat time'}</span>
      <div class="clock">
        {#if remainingMs !== undefined}
          <RaceClock {remainingMs} label="Time remaining" />
          <!-- The companion ELAPSED readout: lap times are elapsed-from-zero quantities, so a
               countdown alone makes "was that a 21 or a 24?" mental math — the small count-up
               keeps lap arithmetic one glance away. -->
          <div class="clock-elapsed" data-testid="elapsed-subclock">
            <span class="clock-elapsed-label">Elapsed</span>
            <RaceClock {elapsedMs} label="Elapsed" />
          </div>
        {:else}
          <RaceClock {elapsedMs} label="Heat time" />
        {/if}
      </div>
    </div>

    {#if live?.on_deck}
      <div class="hud-ondeck">
        <span class="label">On deck</span>
        <span class="value">{heatName(live.on_deck)}</span>
      </div>
    {/if}

    <!-- The "Callouts" toggle mutes ONLY the informational layer (the per-CROSSING pips — #397:
         every crossing including the holeshot and a floor-rejected pass — plus the spoken lap
         callouts). The procedure tones — start tone, end-of-race countdown, race-end buzzer — are
         always on and have no toggle (the old "Tone on/off" switch is gone). Per-tone enable /
         volume is #193's sounds panel, not this one switch. -->
    <div class="audio-tools">
      <button
        type="button"
        class="mute-toggle"
        onclick={toggleMute}
        aria-pressed={muted}
        title={muted
          ? 'Crossing tones and lap callouts muted — click to unmute. Race tones (start / countdown / end) always sound.'
          : 'Crossing tones and lap callouts on — click to mute. Race tones (start / countdown / end) always sound.'}
      >
        <span class="mute-icon" aria-hidden="true">{muted ? '🔇' : '🔊'}</span>
        <span class="mute-text">{muted ? 'Callouts off' : 'Callouts on'}</span>
      </button>
    </div>
  </header>

  {#if heat && phase === 'Staged'}
    <!-- Staging countdown (Slice 3): informational only — no auto-advance. Counts down from the
         round's staging window; turns red and shows "−M:SS" once over-time so the RD sees a field
         that isn't ready. -->
    <div
      class="staging"
      class:overtime={staging.overtime}
      role="status"
      aria-label="Staging countdown"
    >
      <div class="staging-head">
        <span class="staging-label">{staging.overtime ? 'Staging — over time' : 'Staging'}</span>
        <span class="staging-sub">
          {staging.overtime
            ? 'Pilots are over their staging slot. Call the line.'
            : 'Pilots to the line. Informational — Start when ready.'}
        </span>
      </div>
      <div class="staging-clock" aria-label="Staging time remaining">
        {formatStaging(staging.remainingMs)}
      </div>
    </div>
  {/if}

  {#if heat && phase === 'Armed'}
    <!-- Arming state (Slice 3): the runtime ran the start procedure and will auto-advance to
         Running after a random hold. The hold is **intentionally random to pilots**, so the
         precise tone countdown is RD-control-only: a controlling session sees "Tone in S.s"
         (`arming.active`, fed `tone_at` only for `canControl`); the read-only / pilot view keeps
         the generic "stand by" so the randomness the delay provides isn't defeated. -->
    <div class="arming" role="status" aria-label="Arming">
      <span class="arming-pulse" aria-hidden="true"></span>
      {#if arming.active}
        <div class="arming-copy">
          <span class="arming-title"
            >Tone in <span class="arming-countdown" data-testid="arming-countdown"
              >{formatArming(arming.remainingMs)}</span
            ></span
          >
          <span class="arming-sub">The race starts on its own — listen for the tone.</span>
        </div>
      {:else}
        <div class="arming-copy">
          <span class="arming-title">Arming… stand by</span>
          <span class="arming-sub">The race starts on its own — listen for the tone.</span>
        </div>
      {/if}
    </div>
  {/if}

  {#if heat && isOpenPractice && (phase === 'Unofficial' || phase === 'Final')}
    <!-- Practice has no result lifecycle (#393): nothing is provisional, nothing becomes official,
         and there is no protest window to count down. The run simply ended — its laps are on the
         log and on the board below — and the next step is Run again. -->
    <div class="lifecycle" role="status" aria-label="Practice run">
      <span class="lifecycle-dot" aria-hidden="true"></span>
      <div class="lifecycle-copy">
        <span class="lifecycle-title">Run complete</span>
        <span class="lifecycle-sub"
          >Practice isn’t scored — review the board, then Run again when you’re ready.</span
        >
      </div>
    </div>
  {:else if heat && (phase === 'Unofficial' || phase === 'Final')}
    <!-- Provisional → official lifecycle (marshaling Slice 5). Provisional (Unofficial): correctable;
         when a protest window is armed, count down to the auto-official deadline; the RD can finalize
         early via the Finalize transition below. Official (Final): the result is locked (Revert
         re-opens it). Pure display — read-only pilots see the state but the transitions enforce the
         role. -->
    <div
      class="lifecycle"
      class:official={phase === 'Final'}
      role="status"
      aria-label="Result lifecycle"
    >
      <span class="lifecycle-dot" aria-hidden="true"></span>
      {#if phase === 'Final'}
        <div class="lifecycle-copy">
          <span class="lifecycle-title">Official</span>
          <span class="lifecycle-sub"
            >The result is locked. Revert to re-open it for correction.</span
          >
        </div>
      {:else if protest.active}
        <div class="lifecycle-copy">
          <span class="lifecycle-title"
            >Provisional — auto-official in <span class="lifecycle-countdown"
              >{formatProtest(protest.remainingMs)}</span
            ></span
          >
          <span class="lifecycle-sub">Correct now if needed, or finalize early.</span>
        </div>
      {:else}
        <div class="lifecycle-copy">
          <span class="lifecycle-title">Provisional</span>
          <span class="lifecycle-sub">Correctable. Finalize when the result is settled.</span>
        </div>
      {/if}
    </div>
  {/if}

  {#if session.lastCommandError}
    <ErrorBanner error={session.lastCommandError} ondismiss={() => session.clearCommandError()} />
  {/if}

  {#if directoryError}
    <!-- A failed pilots/heats directory read (#340): without it, names silently fall back to raw
         refs with no hint anything went wrong. Visible error + explicit retry (Results pattern). -->
    <div class="dir-error" role="alert">
      <p>Couldn’t load the pilot/heat directory — names may show as raw ids.</p>
      <Button variant="secondary" size="sm" onclick={retryDirectory}>Try again</Button>
    </div>
  {/if}

  {#if canControl}
    <div class="controls" role="group" aria-label="Heat transitions">
      <span class="controls-label">Transitions</span>
      <div class="controls-row">
        <!-- Only the actions this KIND of heat can ever use (#393): a practice heat never draws the
             ceremony verbs at all, since a greyed-out Finalize still reads as the thing to do next. -->
        {#each actionsForKind(heatKind) as action (action)}
          {@const legal = isActionLegal(phase, action, heatKind)}
          <ConfirmButton
            onconfirm={() => fire(action)}
            confirm={isDestructive(action)}
            disabled={!legal || !heat}
            variant={action === primary ? 'primary' : isDestructive(action) ? 'danger' : 'default'}
            title={actionDescription(action, heatKind)}
          >
            <span class="action-btn">{actionLabel(action, heatKind)}</span>
          </ConfirmButton>
        {/each}
      </div>
    </div>
  {/if}

  {#if gateTimer}
    <!-- The gate signal (#415), read-only.
         PLACEMENT, decided by the RD: **not** in the leaderboard rows. A per-row sparkline was
         considered and rejected — Race control is the highest-stakes screen and the leaderboard is
         what an RD actually reads during a heat, so graphs embedded in it compete with the thing
         they are meant to support. It sits here instead, between the transition row and the board:
         in the same glance as the standings it explains, and above them so it reads as the cause
         and they read as the effect. Collapsed it costs one header row, so the board barely moves;
         expanding it is a deliberate act, and at that moment the gates SHOULD be the prominent
         thing on screen.
         Only rendered with a primary timer at all: a sim-only event has no gate to show, and no
         lease worth holding. -->
    <GateSignalStrip
      groups={gates}
      signal={gateFeed.snapshot}
      streaming={gateFeed.streaming}
      everLoaded={gateFeed.everLoaded}
      error={gateFeed.error}
      timerName={gateTimer.name}
      nameFor={competitorName}
      seatLabel={seatNames.seatLabel}
      bind:open={gateCollapse.open}
      bind:othersOpen={gateOthersCollapse.open}
    />
  {/if}

  {#if isOpenPractice}
    <!-- Open-practice per-channel board (open-practice Slice 2): one row per active channel
         (`node-{i}` → the timer's available channel), each with its laps + last/best lap. The
         pilot-keyed channels/heat-sheet/standing panels are replaced by this practice board. -->
    <Card title="Practice board">
      {#if !heat}
        <p class="empty pad">— no practice heat on the timer —</p>
      {:else if channelRows.length === 0}
        <p class="empty pad">
          No active channels — pick some on the round in Rounds &amp; Heats, then stage the heat.
        </p>
      {:else}
        <ul class="practice-board" aria-label="Per-channel practice board">
          {#each channelRows as row (row.ref)}
            <li class="practice-row" aria-label={`Channel ${row.label}`}>
              <span class="practice-node" aria-hidden="true">{row.node + 1}</span>
              <span class="practice-channel">{row.label}</span>
              <span class="practice-laps">
                <span class="practice-laps-n">{row.laps}</span>
                <span class="practice-laps-l">laps</span>
              </span>
              <span class="practice-metric">
                <span class="practice-metric-l">Last</span>
                <span class="practice-metric-v">{formatMicros(row.lastLapMicros)}</span>
              </span>
              <span class="practice-metric best">
                <span class="practice-metric-l">Best</span>
                <span class="practice-metric-v">{formatMicros(row.bestLapMicros)}</span>
              </span>
            </li>
          {/each}
        </ul>
      {/if}
    </Card>
  {:else}
    {#if heat && lineup.length > 0}
      <Card title="Channels">
        <ul class="channels" aria-label="Heat channels">
          {#each lineup as ref (ref)}
            <li class="channel-row">
              <span class="channel-pilot">{competitorName(ref)}</span>
              <span class="channel-label" class:none={!channelOf(ref)}>
                {channelOf(ref) ?? 'Channel unknown'}
              </span>
            </li>
          {/each}
        </ul>
        {#if !hasChannels}
          <!-- Unknown is not "none" (#416). GridFPV knows a seat's channel from the heat's own
               assignment, from what the node reports it is tuned to, or from the timer's
               configured pool — a Flexible RotorHazard timer with no pool configured supplies
               none of those, and saying "no channels" there would be a false statement about
               the hardware. -->
          <p class="channels-note">
            No channel known for this heat — a sim heat tunes none, and a timer with no channel pool
            configured has not told GridFPV what its nodes are on.
          </p>
        {/if}
      </Card>
    {/if}

    <div class="panels">
      <Card title="Heat sheet" pad={false}>
        {#if live}
          <HeatSheet state={live} names={resolvedNames} heatName={heatName(heat)} />
        {:else}
          <p class="empty">Waiting for a live heat…</p>
        {/if}
      </Card>
      <Card title="Live standing" pad={false}>
        {#if liveResult}
          <Leaderboard result={liveResult} metricLabel="Last lap" nameFor={competitorName} />
        {:else}
          <p class="empty">No laps yet.</p>
        {/if}
      </Card>
    </div>
  {/if}
</section>

<style>
  .live-control {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
  }

  /* ── HUD: the race-ops command bar ───────────────────────────────────────── */
  .hud {
    --_phase: var(--gf-phase-scheduled);
    display: grid;
    grid-template-columns: 1fr auto auto auto auto;
    align-items: center;
    gap: var(--gf-space-8);
    padding: var(--gf-space-5) var(--gf-space-6);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    background: linear-gradient(
      100deg,
      color-mix(in srgb, var(--_phase) 10%, var(--gf-elevated)),
      var(--gf-elevated) 60%
    );
    box-shadow: var(--gf-shadow-sm), var(--gf-shadow-inset);
    position: relative;
    overflow: hidden;
  }
  .hud::before {
    content: '';
    position: absolute;
    left: 0;
    top: 0;
    bottom: 0;
    width: 4px;
    background: var(--_phase);
  }
  .hud[data-phase='Scheduled'] {
    --_phase: var(--gf-phase-scheduled);
  }
  .hud[data-phase='Staged'] {
    --_phase: var(--gf-phase-staged);
  }
  .hud[data-phase='Armed'] {
    --_phase: var(--gf-phase-armed);
  }
  .hud[data-phase='Running'] {
    --_phase: var(--gf-phase-running);
  }
  .hud[data-phase='Unofficial'] {
    --_phase: var(--gf-phase-finished);
  }
  .hud[data-phase='Final'] {
    --_phase: var(--gf-phase-scored);
  }
  .label {
    display: block;
    font-size: var(--gf-font-size-2xs);
    color: var(--gf-text-muted);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    font-weight: var(--gf-font-weight-semibold);
    margin-bottom: var(--gf-space-1);
  }
  .value {
    font-size: var(--gf-font-size-xl);
    font-weight: var(--gf-font-weight-bold);
    letter-spacing: var(--gf-tracking-tight);
    font-variant-numeric: tabular-nums;
  }
  .value.none {
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-medium);
    color: var(--gf-text-faint);
  }

  /* The countdown's companion elapsed readout: same tabular clock, a size down and muted, with
     its own tiny caps label — clearly subordinate to the big remaining time above it. */
  .clock-elapsed {
    display: flex;
    align-items: baseline;
    gap: var(--gf-space-2);
    margin-top: var(--gf-space-1);
  }
  .clock-elapsed-label {
    font-size: var(--gf-font-size-2xs);
    color: var(--gf-text-muted);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    font-weight: var(--gf-font-weight-semibold);
  }
  .clock-elapsed :global(.gridfpv-race-clock) {
    font-size: var(--gf-font-size-lg);
    color: var(--gf-text-muted);
  }

  /* ── Heat picker ─────────────────────────────────────────────────────────── */
  .heat-pick {
    display: block;
    margin-top: var(--gf-space-2);
  }
  .heat-pick-select {
    width: 100%;
    max-width: 18rem;
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    background: var(--gf-surface);
    color: var(--gf-text);
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
    cursor: pointer;
  }
  .heat-pick-select:hover:not(:disabled) {
    border-color: var(--gf-accent);
  }
  .heat-pick-select:disabled {
    cursor: not-allowed;
    opacity: 0.55;
  }
  .heat-pick-hint {
    margin: var(--gf-space-1) 0 0;
    max-width: 18rem;
    font-size: var(--gf-font-size-2xs);
    color: var(--gf-text-muted);
  }
  .visually-hidden {
    position: absolute;
    width: 1px;
    height: 1px;
    margin: -1px;
    padding: 0;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
    border: 0;
  }
  .phase {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    padding: var(--gf-space-2) var(--gf-space-4);
    border-radius: var(--gf-radius-pill);
    background: color-mix(in srgb, var(--_phase) 16%, transparent);
    border: 1px solid color-mix(in srgb, var(--_phase) 45%, transparent);
    color: color-mix(in srgb, var(--_phase) 90%, var(--gf-text));
    font-weight: var(--gf-font-weight-bold);
    font-size: var(--gf-font-size-md);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-wide);
  }
  .phase-dot {
    width: 0.55em;
    height: 0.55em;
    border-radius: 50%;
    background: var(--_phase);
  }
  .hud[data-phase='Running'] .phase-dot {
    animation: hud-pulse 1.4s var(--gf-ease-out) infinite;
  }
  @keyframes hud-pulse {
    0% {
      box-shadow: 0 0 0 0 color-mix(in srgb, var(--_phase) 70%, transparent);
    }
    70% {
      box-shadow: 0 0 0 0.6em transparent;
    }
    100% {
      box-shadow: 0 0 0 0 transparent;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .hud[data-phase='Running'] .phase-dot {
      animation: none;
    }
  }

  /* ── Transition controls ─────────────────────────────────────────────────── */
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
  .controls {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
    padding: var(--gf-space-4) var(--gf-space-5);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    background: var(--gf-surface);
  }
  .controls-label {
    font-size: var(--gf-font-size-2xs);
    color: var(--gf-text-muted);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    font-weight: var(--gf-font-weight-semibold);
  }
  .controls-row {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-3);
  }
  .action-btn {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
  }
  /* ── Staging countdown ───────────────────────────────────────────────────── */
  .staging {
    --_stage: var(--gf-phase-staged);
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-5);
    padding: var(--gf-space-4) var(--gf-space-6);
    border: 1px solid color-mix(in srgb, var(--_stage) 45%, var(--gf-border));
    border-radius: var(--gf-radius-lg);
    background: linear-gradient(
      100deg,
      color-mix(in srgb, var(--_stage) 12%, var(--gf-elevated)),
      var(--gf-elevated) 60%
    );
  }
  .staging.overtime {
    --_stage: var(--gf-danger, #e5484d);
    animation: staging-flash 1.1s var(--gf-ease-out) infinite;
  }
  @keyframes staging-flash {
    0%,
    100% {
      border-color: color-mix(in srgb, var(--_stage) 55%, var(--gf-border));
    }
    50% {
      border-color: var(--_stage);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .staging.overtime {
      animation: none;
    }
  }
  .staging-head {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
    min-width: 0;
  }
  .staging-label {
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-bold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-wide);
    color: color-mix(in srgb, var(--_stage) 90%, var(--gf-text));
  }
  .staging-sub {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .staging-clock {
    font-size: var(--gf-font-size-3xl, 2.5rem);
    font-weight: var(--gf-font-weight-bold);
    font-variant-numeric: tabular-nums;
    letter-spacing: var(--gf-tracking-tight);
    color: color-mix(in srgb, var(--_stage) 92%, var(--gf-text));
    white-space: nowrap;
  }

  /* ── Arming state ────────────────────────────────────────────────────────── */
  .arming {
    --_arm: var(--gf-phase-armed);
    display: flex;
    align-items: center;
    gap: var(--gf-space-4);
    padding: var(--gf-space-5) var(--gf-space-6);
    border: 1px solid color-mix(in srgb, var(--_arm) 45%, var(--gf-border));
    border-radius: var(--gf-radius-lg);
    background: linear-gradient(
      100deg,
      color-mix(in srgb, var(--_arm) 14%, var(--gf-elevated)),
      var(--gf-elevated) 60%
    );
  }
  .arming-pulse {
    flex-shrink: 0;
    width: 1.1rem;
    height: 1.1rem;
    border-radius: 50%;
    background: var(--_arm);
    animation: arming-pulse 1s var(--gf-ease-out) infinite;
  }
  @keyframes arming-pulse {
    0% {
      box-shadow: 0 0 0 0 color-mix(in srgb, var(--_arm) 70%, transparent);
    }
    70% {
      box-shadow: 0 0 0 0.8em transparent;
    }
    100% {
      box-shadow: 0 0 0 0 transparent;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .arming-pulse {
      animation: none;
    }
  }
  .arming-copy {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
  }
  .arming-title {
    font-size: var(--gf-font-size-xl);
    font-weight: var(--gf-font-weight-bold);
    letter-spacing: var(--gf-tracking-tight);
    color: color-mix(in srgb, var(--_arm) 90%, var(--gf-text));
  }
  .arming-sub {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .arming-countdown {
    font-variant-numeric: tabular-nums;
  }

  /* ── Result lifecycle (provisional → official, marshaling Slice 5) ─────────── */
  .lifecycle {
    --_life: var(--gf-phase-finished); /* provisional (Unofficial) — blue */
    display: flex;
    align-items: center;
    gap: var(--gf-space-4);
    padding: var(--gf-space-5) var(--gf-space-6);
    border: 1px solid color-mix(in srgb, var(--_life) 45%, var(--gf-border));
    border-radius: var(--gf-radius-lg);
    background: linear-gradient(
      100deg,
      color-mix(in srgb, var(--_life) 14%, var(--gf-elevated)),
      var(--gf-elevated) 60%
    );
  }
  .lifecycle.official {
    --_life: var(--gf-phase-scored); /* official (Final) — violet */
  }
  .lifecycle-dot {
    flex-shrink: 0;
    width: 1.1rem;
    height: 1.1rem;
    border-radius: 50%;
    background: var(--_life);
  }
  .lifecycle-copy {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
  }
  .lifecycle-title {
    font-size: var(--gf-font-size-xl);
    font-weight: var(--gf-font-weight-bold);
    letter-spacing: var(--gf-tracking-tight);
    color: color-mix(in srgb, var(--_life) 90%, var(--gf-text));
  }
  .lifecycle-countdown {
    font-variant-numeric: tabular-nums;
  }
  .lifecycle-sub {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }

  /* ── Audio tools (mute toggle) ───────────────────────────────────────────── */
  .audio-tools {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-3);
  }

  /* ── Mute toggle ─────────────────────────────────────────────────────────── */
  .mute-toggle {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    background: var(--gf-surface);
    color: var(--gf-text-secondary);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
    cursor: pointer;
    white-space: nowrap;
  }
  .mute-toggle:hover {
    border-color: var(--gf-accent);
    color: var(--gf-text);
  }
  .mute-toggle[aria-pressed='true'] {
    color: var(--gf-text-faint);
  }
  .mute-icon {
    font-size: var(--gf-font-size-md);
    line-height: 1;
  }

  .channels {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(12rem, 1fr));
    gap: var(--gf-space-3);
  }
  .channel-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-3);
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    background: var(--gf-surface);
  }
  .channel-pilot {
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .channel-label {
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-bold);
    color: var(--gf-accent);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .channel-label.none {
    color: var(--gf-text-faint);
    font-weight: var(--gf-font-weight-regular);
  }
  .channels-note {
    margin: var(--gf-space-3) 0 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }

  /* ── Open-practice per-channel board ─────────────────────────────────────── */
  .empty.pad {
    padding: var(--gf-space-5);
  }
  .practice-board {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .practice-row {
    display: grid;
    grid-template-columns: auto minmax(8rem, 1fr) auto auto auto;
    align-items: center;
    gap: var(--gf-space-5);
    padding: var(--gf-space-3) var(--gf-space-4);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    background: var(--gf-surface);
  }
  .practice-node {
    display: inline-grid;
    place-items: center;
    /* Sized to its content, not to one digit. #108 raised `lg` to 21px, at which a fixed 2.2rem
       square clipped a two-digit node number — and an 8+ node timer has those. `min-width` keeps
       the square look for a single digit while letting `10`+ widen instead of truncate. */
    min-width: 2.4rem;
    min-height: 2.4rem;
    padding: 0 var(--gf-space-1);
    flex-shrink: 0;
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-bold);
    font-variant-numeric: tabular-nums;
  }
  .practice-channel {
    font-size: var(--gf-font-size-xl);
    font-weight: var(--gf-font-weight-bold);
    letter-spacing: var(--gf-tracking-tight);
    color: var(--gf-accent);
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .practice-laps {
    display: inline-flex;
    align-items: baseline;
    gap: var(--gf-space-2);
    white-space: nowrap;
  }
  .practice-laps-n {
    font-size: var(--gf-font-size-2xl, 1.75rem);
    font-weight: var(--gf-font-weight-bold);
    font-variant-numeric: tabular-nums;
    color: var(--gf-text);
  }
  .practice-laps-l {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
  }
  .practice-metric {
    display: inline-flex;
    flex-direction: column;
    align-items: flex-end;
    gap: var(--gf-space-1);
    white-space: nowrap;
  }
  .practice-metric-l {
    font-size: var(--gf-font-size-2xs);
    color: var(--gf-text-muted);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    font-weight: var(--gf-font-weight-semibold);
  }
  .practice-metric-v {
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-semibold);
    font-variant-numeric: tabular-nums;
    color: var(--gf-text);
  }
  .practice-metric.best .practice-metric-v {
    color: var(--gf-phase-running, var(--gf-accent));
  }
  @media (max-width: 60rem) {
    .practice-row {
      grid-template-columns: auto 1fr auto;
      gap: var(--gf-space-3);
    }
  }

  .panels {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--gf-space-5);
  }
  .empty {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    padding: var(--gf-space-5);
  }
  @media (max-width: 75rem) {
    .hud {
      grid-template-columns: 1fr 1fr;
      gap: var(--gf-space-5);
    }
  }
  @media (max-width: 60rem) {
    .panels {
      grid-template-columns: 1fr;
    }
  }
</style>
