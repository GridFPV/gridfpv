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
    PilotId,
    PilotProgress,
    RoundDef
  } from '@gridfpv/types';
  import { channelLabel, nodeChannelLabel, nodeIndexOf } from '../lib/channels.js';
  import { createCompetitorNameResolver } from '../lib/competitorName.js';
  import { heatDisplayName, heatNameById } from '../lib/heats.js';
  import {
    ACTION_ORDER,
    actionDescription,
    commandForAction,
    isActionLegal,
    isDestructive,
    isOverride,
    primaryAction,
    type HeatAction
  } from '../lib/transitions.js';
  import type { Session } from '../lib/session.svelte.js';
  import { useRaceClock } from '../lib/raceClock.svelte.js';
  import { useStagingClock, formatStaging } from '../lib/stagingClock.svelte.js';
  import { useProtestClock, formatProtest } from '../lib/protestClock.svelte.js';
  import { useArmingClock, formatArming } from '../lib/armingClock.svelte.js';
  import { StartTonePlayer } from '../lib/startTone.js';
  import ConfirmButton from '../lib/ConfirmButton.svelte';
  import ErrorBanner from '../lib/ErrorBanner.svelte';

  let { session, names = {} }: { session: Session; names?: Record<string, string> } = $props();

  const live = $derived<LiveRaceState | undefined>(session.liveState);
  const phase = $derived(live?.phase ?? 'Scheduled');
  const heat = $derived<HeatId | undefined>(live?.current_heat);
  const primary = $derived(primaryAction(phase));
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
    session
      .listHeats()
      .then((h) => (heats = h))
      .catch(() => (heats = []));
  });

  // The current heat's ref → channel-label map (race redesign Slice 4b). Empty for a sim/free-text
  // heat (no frequencies assigned), in which case the channels panel shows "—".
  const currentChannels = $derived.by(() => {
    const summary = heats.find((h) => h.heat === heat);
    const map = new Map<CompetitorRef, string>();
    for (const [ref, mhz] of summary?.frequencies ?? []) map.set(ref, channelLabel(mhz, catalog));
    return map;
  });
  const lineup = $derived<CompetitorRef[]>(live?.active_pilots ?? []);
  const hasChannels = $derived(currentChannels.size > 0);

  // ── Friendly names everywhere (heat names + pilot callsigns) ─────────────────────────────────
  // Live Control knows heats and competitors only as raw ids/refs (the live `LiveRaceState` carries
  // no human labels). Resolve them here, at the one call site that has the directory + round context,
  // and pass display strings down — so every panel reads callsigns / "<Round> Heat N", never an id.

  // The app-level pilots directory (callsigns). An open read like the heats/channels lists above;
  // re-read whenever the stream advances so a freshly-registered pilot's callsign appears live.
  let pilots = $state<Pilot[]>([]);
  $effect(() => {
    void session.protocolState;
    session
      .listPilots()
      .then((p) => (pilots = p))
      .catch(() => (pilots = []));
  });
  const pilotById = $derived(new Map<PilotId, Pilot>(pilots.map((p) => [p.id, p])));
  // A competitor ref → its bound pilot id from the live `progress`, which carries an **explicit**
  // `pilot` only when a `Register` command bound it (the open-practice / manual-registration path).
  // This is empty for the common roster-seeded heat: `FromRoster` seeding makes each competitor ref
  // *equal to the pilot id itself* and emits **no** `CompetitorRegistered` event, so `progress.pilot`
  // stays `null` in every phase (Scheduled → Running) — see `competitorName` for the roster fallback.
  const explicitPilotByRef = $derived(
    new Map<CompetitorRef, PilotId>(
      (live?.progress ?? [])
        .filter((p): p is PilotProgress & { pilot: PilotId } => p.pilot != null)
        .map((p) => [p.competitor, p.pilot])
    )
  );

  // `heatName(id)` → the friendly "<Round> Heat N" / "Open Practice Heat" name for a heat id (the
  // same helper the picker uses), falling back to the bare id for an untagged/free-text heat. Used
  // for the current-heat title and the on-deck heat.
  function heatName(id: HeatId | undefined): string {
    if (!id) return '';
    return heatNameById(id, heats, session.currentEvent?.rounds ?? []);
  }

  // `competitorName(ref)` → the **callsign** (directory pilot), else an open-practice `node-{i}`
  // seat's **channel label**, else the bare ref. The single shared resolver every pilot/lineup/
  // leaderboard row goes through — the same one Marshaling uses (see `competitorName.ts` for the
  // full rule and why the binding comes from the always-available roster binding, not race progress,
  // so a callsign shows whether the heat is Scheduled, Staged, Running, or done).
  const competitorName = $derived.by<(ref: CompetitorRef) => string>(() =>
    createCompetitorNameResolver({
      pilotById,
      explicitPilotByRef,
      channelByRef: currentChannels
    })
  );

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
  // The round's start-tone cue (pitch/length), when configured; else the player's default tone.
  const toneCue = $derived(currentRound?.start_procedure?.tone);

  // ── Open-practice per-channel board (open-practice Slice 2) ───────────────────────────────────
  // The casual **open-practice** format runs one open heat over the active **channels**: its live
  // `LiveRaceState` rows are unbound (`pilot: null`) with competitor refs `node-{i}` (the timer
  // seat). Rather than the pilot-keyed channels/heat-sheet panels, this heat reads as a per-channel
  // practice board — each row a channel (resolved `node-{i}` → the primary timer's
  // `available_channels[i]` → catalog label) with its laps, last lap, and best lap.
  const OPEN_PRACTICE = 'open_practice';
  const isOpenPractice = $derived(currentRound?.format === OPEN_PRACTICE);
  // The primary timer (its `available_channels` resolve each `node-{i}` seat to a channel label).
  const availableChannels = $derived<number[]>(session.primaryTimer?.available_channels ?? []);

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
  // whenever the heat changes (a fresh practice run / Reset starts a clean board — the backend
  // clears its in-memory laps on the new heat, and this mirrors that).
  let bestByRef = $state<Map<CompetitorRef, number>>(new Map());
  let bestForHeat = $state<HeatId | undefined>(undefined);
  $effect(() => {
    // On a heat change, wipe the accumulated bests (matches the backend clearing its lap store).
    if (heat !== bestForHeat) {
      bestForHeat = heat;
      bestByRef = new Map();
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
  const channelRows = $derived<ChannelRow[]>(buildChannelRows(live, availableChannels));
  function buildChannelRows(state: LiveRaceState | undefined, avail: number[]): ChannelRow[] {
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
        label: nodeChannelLabel(node, avail, catalog),
        laps: p?.laps_completed ?? 0,
        lastLapMicros: p?.last_lap_micros ?? undefined,
        bestLapMicros: bestByRef.get(ref)
      });
    }
    return rows.sort((a, b) => a.node - b.node);
  }

  // ── Reset / new practice run (open-practice Slice 2) ──────────────────────────────────────────
  // A fresh run re-fills the open-practice round to mint a new heat, which clears the backend's
  // in-memory laps (per the format) — wiping the board between practice sessions. The new heat
  // arrives on the live stream; the best-lap tracker resets on the heat change (above).
  let resetting = $state(false);
  async function startFreshRun() {
    const roundId = currentRound?.id;
    if (!roundId || resetting) return;
    resetting = true;
    try {
      const ack = await session.fillRound(roundId);
      if (!ack.ok) return; // The error banner surfaces session.lastCommandError.
      toast.success('Fresh practice run — board cleared.');
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      resetting = false;
    }
  }

  // ── Staging countdown (heat-lifecycle Slice 3) ───────────────────────────────────────────────
  // While the heat is Staged, count down from the round's staging window. Informational only — no
  // auto-advance; it goes negative (over-time) past zero so the RD sees a field that isn't ready.
  const staging = useStagingClock(
    () => phase,
    () => stagingSecs
  );

  // ── Auto-official countdown (marshaling Slice 5) ─────────────────────────────────────────────
  // While the heat is Unofficial (provisional) and the round armed a protest window, count the wall
  // clock down to the logged auto-official deadline so the RD sees "auto-official in M:SS". Inactive
  // (no countdown) when the window is Off — the result then stays provisional until manual Finalize.
  const protest = useProtestClock(() => live?.lifecycle);

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

  // ── Start tone synced to race-go (heat-lifecycle Slice 3; robustness + late-join fix) ──────────
  // A short Web-Audio beep the moment a heat goes live (race-go). The runtime logs
  // `HeatStarting { delay_ms }` then auto-appends the Running transition after the (hidden) random
  // hold; the console plays the tone when the *live phase* turns Running.
  //
  // ── Fire on an OBSERVED transition into Running — not on a late join ──────────────────────────
  // The tone is a race-go cue for the RD *watching* the heat go live. It must fire when the heat
  // crosses **into** `Running` from a pre-Running phase the console actually saw this mount — i.e.
  // a genuine race-go (Stage → Start → … → Running) or a fast/batched Armed → Running (we'd still
  // have seen Staged/Armed first). It must **not** fire when the RD merely **navigates to the Live
  // page of an already-running heat** (a late join): there the first phase the console observes for
  // that heat *is* `Running`, with no prior pre-Running phase seen — an unwanted buzz on every
  // navigation. So we only fire on `Running` if a pre-Running phase (`Scheduled`/`Staged`/`Armed`)
  // was observed for this heat first; a heat seen Running as its first phase is suppressed.
  //
  // Per heat we track two things, both reset on a heat change: whether a pre-Running phase was seen
  // (`tonePreRunningForHeat`), and whether the tone already fired (`toneFiredForHeat`, so repeated
  // Running snapshots / progress updates don't re-fire). The next heat resets and fires its own.
  const tone = new StartTonePlayer();
  $effect(() => () => tone.dispose());
  let toneFiredForHeat = $state<HeatId | undefined>(undefined);
  let tonePreRunningForHeat = $state<HeatId | undefined>(undefined);
  $effect(() => {
    const p = phase;
    const h = heat;
    if (h === undefined) return;
    if (p !== 'Running') {
      // A pre-Running phase for this heat (or a fold back out of Running): remember that we observed
      // a non-Running phase first, and (on a heat change) clear the fired flag so the next heat that
      // enters Running fires its own tone. We only *arm* on an actual pre-Running phase so a heat
      // that started Running then folded back to Unofficial/Final doesn't arm a spurious tone.
      if (p === 'Scheduled' || p === 'Staged' || p === 'Armed') tonePreRunningForHeat = h;
      if (toneFiredForHeat !== h) toneFiredForHeat = undefined;
      return;
    }
    // p === 'Running'. Fire once — but only if a pre-Running phase for THIS heat was observed first
    // (a genuine race-go we watched), not when Running is the first phase seen (a late join / page
    // load onto an in-progress heat).
    if (toneFiredForHeat !== h && tonePreRunningForHeat === h) {
      toneFiredForHeat = h;
      tone.play(toneCue);
    }
  });
  let muted = $state(tone.muted);
  function toggleMute() {
    // The toggle is itself a user gesture — unlock the audio context here too so an RD who only
    // ever touches the mute button (never a transition) still gets an audible race-go tone.
    void tone.resume();
    muted = tone.toggleMuted();
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
          metric: { BestLapMicros: p.last_lap_micros ?? null }
        };
      })
    };
  }

  async function fire(action: HeatAction) {
    if (!heat) return;
    // Unlock the audio context on this user gesture (autoplay policy) so the later race-go tone is
    // audible — every transition click counts, well before the heat reaches Running.
    void tone.resume();
    const ack = await session.send(commandForAction(action, heat));
    // Finalizing locks in the heat result; pull it so the Results screen has it to show. The
    // live stream only carries `LiveRaceState`, so the scored `HeatResult` is a separate
    // heat-scope fetch (`?projection=result`).
    if (ack.ok && action === 'Finalize') {
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
      <span class="label">Heat time</span>
      <div class="clock">
        <RaceClock {elapsedMs} label="Heat time" />
      </div>
    </div>

    {#if live?.on_deck}
      <div class="hud-ondeck">
        <span class="label">On deck</span>
        <span class="value">{heatName(live.on_deck)}</span>
      </div>
    {/if}

    <div class="audio-tools">
      <button
        type="button"
        class="mute-toggle"
        onclick={toggleMute}
        aria-pressed={muted}
        title={muted ? 'Start tone muted — click to unmute' : 'Start tone on — click to mute'}
      >
        <span class="mute-icon" aria-hidden="true">{muted ? '🔇' : '🔊'}</span>
        <span class="mute-text">{muted ? 'Tone off' : 'Tone on'}</span>
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

  {#if heat && (phase === 'Unofficial' || phase === 'Final')}
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

  {#if canControl}
    <div class="controls" role="group" aria-label="Heat transitions">
      <span class="controls-label">Transitions</span>
      <div class="controls-row">
        {#each ACTION_ORDER as action (action)}
          {@const legal = isActionLegal(phase, action)}
          <ConfirmButton
            onconfirm={() => fire(action)}
            confirm={isDestructive(action)}
            disabled={!legal || !heat}
            variant={action === primary ? 'primary' : isDestructive(action) ? 'danger' : 'default'}
            title={actionDescription(action)}
          >
            <span class="action-btn" class:override={isOverride(action)}>
              {#if isOverride(action)}<span class="override-tag" aria-hidden="true">override</span
                >{/if}{action}
            </span>
          </ConfirmButton>
        {/each}
      </div>
    </div>
  {/if}

  {#if isOpenPractice}
    <!-- Open-practice per-channel board (open-practice Slice 2): one row per active channel
         (`node-{i}` → the timer's available channel), each with its laps + last/best lap. The
         pilot-keyed channels/heat-sheet/standing panels are replaced by this practice board. -->
    <Card title="Practice board">
      {#snippet actions()}
        <Button
          variant="secondary"
          size="sm"
          onclick={startFreshRun}
          loading={resetting}
          disabled={!heat || resetting}
          title="Mint a fresh open-practice heat — clears the live board"
        >
          New run · clear board
        </Button>
      {/snippet}

      {#if !heat}
        <p class="empty pad">— no practice heat on the timer —</p>
      {:else if channelRows.length === 0}
        <p class="empty pad">No active channels — fill the round to start a practice run.</p>
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
              <span class="channel-label" class:none={!currentChannels.get(ref)}>
                {currentChannels.get(ref) ?? '—'}
              </span>
            </li>
          {/each}
        </ul>
        {#if !hasChannels}
          <p class="channels-note">No channels assigned (a sim heat tunes none).</p>
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
  /* Override actions (SkipCountdown/ForceEnd): a clear, smaller "override" tag prefixes the label
     so they read as secondary escape hatches, not the obvious forward step. */
  .override-tag {
    font-size: var(--gf-font-size-2xs);
    font-weight: var(--gf-font-weight-bold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    padding: 0.05rem var(--gf-space-2);
    border-radius: var(--gf-radius-pill);
    background: color-mix(in srgb, var(--gf-warning, #d08700) 22%, transparent);
    color: var(--gf-warning, #d08700);
    border: 1px solid color-mix(in srgb, var(--gf-warning, #d08700) 45%, transparent);
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
    width: 2.2rem;
    height: 2.2rem;
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
