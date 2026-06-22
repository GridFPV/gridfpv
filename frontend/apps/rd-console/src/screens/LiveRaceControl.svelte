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
    PilotProgress,
    RoundDef
  } from '@gridfpv/types';
  import { channelLabel, nodeChannelLabel, nodeIndexOf } from '../lib/channels.js';
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
  import { StartTonePlayer } from '../lib/startTone.js';
  import ConfirmButton from '../lib/ConfirmButton.svelte';
  import ErrorBanner from '../lib/ErrorBanner.svelte';

  let { session, names = {} }: { session: Session; names?: Record<string, string> } = $props();

  const live = $derived<LiveRaceState | undefined>(session.liveState);
  const phase = $derived(live?.phase ?? 'Scheduled');
  const heat = $derived<HeatId | undefined>(live?.current_heat);
  const primary = $derived(primaryAction(phase));

  // ── Per-heat channels (race redesign Slice 4b) ───────────────────────────────
  // The live stream carries only `LiveRaceState` (no frequencies), so resolve the current heat's
  // channel assignment by joining `current_heat` against the heats list (which carries the
  // `HeatScheduled.frequencies`), labelled through the standard catalog. Both are open reads,
  // re-fetched whenever the live state advances (so a freshly-staged heat's channels appear).
  let catalog = $state<ChannelCatalogEntry[]>([]);
  let heats = $state<HeatSummary[]>([]);
  $effect(() => {
    session
      .listChannels()
      .then((c) => (catalog = c))
      .catch(() => (catalog = []));
  });
  $effect(() => {
    void session.liveState;
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

  // ── Race clock (#62) ────────────────────────────────────────────────────────────────
  // The phase-driven elapsed clock lives in the shared `useRaceClock` helper so the persistent
  // ContextHeader (#85) and this screen drive the *same* clock from one place. It is
  // server-time-authoritative (#62 follow-up): it counts from the live state's `race_started_at`
  // / `race_ended_at` (the server's race-go / race-end instants), so this HUD and the header
  // agree exactly and the frozen value is the precise server-side duration. See raceClock.svelte.ts.
  const clock = useRaceClock(
    () => phase,
    () => live?.race_started_at,
    () => live?.race_ended_at
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

<section class="live-control" aria-label="Live race control">
  <header class="hud" data-phase={phase}>
    <div class="hud-heat">
      <span class="label">Current heat</span>
      <div class="heat-id">
        {#if heat}
          <span class="value">{heat}</span>
        {:else}
          <span class="value none">— none on the timer —</span>
        {/if}
      </div>
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
        <span class="value">{live.on_deck}</span>
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
         Running after a HIDDEN random hold. We deliberately show a generic "stand by" — not a
         precise countdown — so the randomness that the delay is meant to provide isn't defeated. -->
    <div class="arming" role="status" aria-label="Arming">
      <span class="arming-pulse" aria-hidden="true"></span>
      <div class="arming-copy">
        <span class="arming-title">Arming… stand by</span>
        <span class="arming-sub">The race starts on its own — listen for the tone.</span>
      </div>
    </div>
  {/if}

  {#if session.lastCommandError}
    <ErrorBanner error={session.lastCommandError} ondismiss={() => session.clearCommandError()} />
  {/if}

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
              <span class="channel-pilot">{names[ref] ?? ref}</span>
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
          <HeatSheet state={live} {names} />
        {:else}
          <p class="empty">Waiting for a live heat…</p>
        {/if}
      </Card>
      <Card title="Live standing" pad={false}>
        {#if liveResult}
          <Leaderboard result={liveResult} metricLabel="Last lap" />
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
