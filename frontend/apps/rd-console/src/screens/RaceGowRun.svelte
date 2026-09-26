<script lang="ts">
  /**
   * RaceGowRun — the solo run screen (`#/racegow/run`): start, fly, stop, read the result.
   *
   * One page for the whole RaceGOW loop, over the **active** RaceGOW event. It is Race control
   * collapsed to a single pilot's needs: one big button that does the right next thing (fill the
   * next heat + stage + start, or stop + finalize), the live clock and lap list, this run's best
   * 3-lap window as it forms, and the round's best across every run — the number RaceGOW wants —
   * with a copy-and-paste submission line and the OBS overlay URL beside it.
   *
   * **Nothing here is a second implementation of scoring.** Each run is an ordinary heat of the
   * event's time-trial round: `FillRound` draws it, `Stage`/`Start` run it, `ForceEnd`/`Finalize`
   * close it, and the laps are read back off the served lap-list projection (marshaling-aware, min-
   * lap floored). The round-wide best is the served round standings. The client only slides a
   * 3-lap window over served laps for the *live* readout (`bestConsecutive`), and the "Open in the
   * full console" link is always one click away for marshaling, audit, or the results export.
   */
  import { Badge, Banner, Button, Card, RaceClock, formatMicros, toast } from '@gridfpv/components';
  import type {
    HeatId,
    HeatSummary,
    Lap,
    LiveRaceState,
    Pilot,
    RoundStanding
  } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import Brand from '../Brand.svelte';
  import Breadcrumbs from '../Breadcrumbs.svelte';
  import ErrorBanner from '../lib/ErrorBanner.svelte';
  import { useRaceClock } from '../lib/raceClock.svelte.js';
  import { raceDayAudio } from '../lib/raceDayAudio.svelte.js';
  import { isTimerConnected, kindLabel } from '../lib/timers.js';
  import {
    bestConsecutive,
    overlayUrl,
    raceGowRound,
    runLaps,
    servedBestMicros,
    submissionText,
    RACEGOW_LAPS,
    type ConsecutiveWindow
  } from '../lib/racegow.js';

  let {
    session,
    onhome,
    onback,
    onconsole,
    ontimers
  }: {
    session: Session;
    onhome: () => void;
    /** Back to the RaceGOW page (all runs, start another track). */
    onback: () => void;
    /** Open this event in the full RD console (Race control). */
    onconsole: () => void;
    /** The Timers page, offered when the run's timer is not connected. */
    ontimers: () => void;
  } = $props();

  const audio = raceDayAudio();

  // ── Live state ──────────────────────────────────────────────────────────────────────────────
  const event = $derived(session.currentEvent);
  const round = $derived(raceGowRound(event));
  const live = $derived<LiveRaceState | undefined>(session.liveState);
  const phase = $derived(live?.phase ?? 'Scheduled');
  const heat = $derived<HeatId | undefined>(live?.current_heat);

  const clock = useRaceClock(
    () => phase,
    () => live?.race_started_at,
    () => live?.race_ended_at,
    () => session.serverNowMs()
  );

  /**
   * What the one button does next. `idle` = nothing on the timer, or the last run is saved;
   * `ready` = a run is drawn but not started (a refresh mid-start); the rest follow the heat loop.
   */
  type View = 'idle' | 'ready' | 'staged' | 'arming' | 'running' | 'stopped';
  const view = $derived.by<View>(() => {
    if (!heat) return 'idle';
    switch (phase) {
      case 'Scheduled':
        return 'ready';
      case 'Staged':
        return 'staged';
      case 'Armed':
        return 'arming';
      case 'Running':
        return 'running';
      case 'Unofficial':
        return 'stopped';
      default:
        return 'idle';
    }
  });

  // ── Directory reads (open, no token) ────────────────────────────────────────────────────────
  let pilots = $state<Pilot[]>([]);
  $effect(() => {
    void session
      .listPilots()
      .then((p) => (pilots = p))
      .catch(() => (pilots = []));
  });
  const callsign = $derived(pilots.find((p) => p.id === event?.roster?.[0])?.callsign ?? 'Pilot');
  const timer = $derived(session.primaryTimer);
  const timerLabel = $derived(timer ? `${timer.name} (${kindLabel(timer.kind)})` : 'the timer');
  const timerDown = $derived(timer !== undefined && !isTimerConnected(timer));

  // The round's heats — every run so far, in the order they were drawn. Re-read on every stream
  // tick, like Race control does: a fill schedules a heat without moving `current_heat`.
  let heats = $state<HeatSummary[]>([]);
  $effect(() => {
    void session.protocolState;
    void session
      .listHeats()
      .then((h) => (heats = h))
      .catch(() => {});
  });
  const runHeats = $derived(round ? heats.filter((h) => h.round === round.id) : []);

  // ── Laps, per run ───────────────────────────────────────────────────────────────────────────
  // The current heat's laps are re-read on every stream tick (a crossing is a tick); a finished
  // run's laps are read once. Served, never accumulated — a voided lap simply disappears.
  let lapsByHeat = $state<ReadonlyMap<HeatId, Lap[]>>(new Map());
  function storeLaps(id: HeatId, laps: Lap[]): void {
    const next = new Map(lapsByHeat);
    next.set(id, laps);
    lapsByHeat = next;
  }
  $effect(() => {
    void session.protocolState;
    const current = heat;
    if (!current) return;
    void session.fetchHeatLaps(current).then((list) => {
      if (list) storeLaps(current, runLaps(list));
    });
  });
  $effect(() => {
    for (const h of runHeats) {
      if (h.phase === 'Final' && !lapsByHeat.has(h.heat)) {
        void session.fetchHeatLaps(h.heat).then((list) => {
          if (list) storeLaps(h.heat, runLaps(list));
        });
      }
    }
  });

  const currentLaps = $derived<Lap[]>(heat ? (lapsByHeat.get(heat) ?? []) : []);
  const currentBest = $derived(bestConsecutive(currentLaps.map((l) => l.duration_micros)));

  /** One row of the runs table. */
  interface RunRow {
    heat: HeatId;
    number: number;
    name: string;
    phase: HeatSummary['phase'];
    laps: number;
    best: ConsecutiveWindow | undefined;
  }
  const runRows = $derived<RunRow[]>(
    runHeats.map((h, i) => {
      const laps = lapsByHeat.get(h.heat) ?? [];
      return {
        heat: h.heat,
        number: i + 1,
        name: h.name,
        phase: h.phase,
        laps: laps.length,
        best: bestConsecutive(laps.map((l) => l.duration_micros))
      };
    })
  );
  /** The best saved run, client-side — which run and which laps the headline came from. */
  const bestRun = $derived.by<RunRow | undefined>(() => {
    let best: RunRow | undefined;
    for (const row of runRows) {
      if (row.phase !== 'Final' || !row.best) continue;
      if (!best || row.best.totalMicros < best.best!.totalMicros) best = row;
    }
    return best;
  });

  // ── The served round-wide best (the RaceGOW number) ─────────────────────────────────────────
  let standings = $state<RoundStanding[]>([]);
  async function refreshStandings(): Promise<void> {
    if (!round) return;
    try {
      standings = await session.roundStandings(round.id);
    } catch {
      /* keep the last value; the client-side best still renders */
    }
  }
  $effect(() => {
    // Once on mount, and again whenever a run reaches Final (its laps then count).
    void runHeats.filter((h) => h.phase === 'Final').length;
    void refreshStandings();
  });
  const servedBest = $derived(servedBestMicros(standings));
  const headlineMicros = $derived(servedBest ?? bestRun?.best?.totalMicros);

  // ── Actions ─────────────────────────────────────────────────────────────────────────────────
  let busy = $state(false);
  let actionError = $state<string | undefined>(undefined);
  /** When each run was stopped on this console — the submission line's "flown at". */
  let stoppedAt = $state<ReadonlyMap<HeatId, Date>>(new Map());

  /**
   * Start the next run: draw the round's next heat (unless one is already drawn and waiting),
   * make it the current heat, stage it and start it. Stops at the first failed ack — the error
   * banner names it, and the heat is left wherever the engine put it.
   */
  async function startRun(): Promise<void> {
    if (!round || busy) return;
    audio.resume();
    busy = true;
    actionError = undefined;
    try {
      let target = heat && phase === 'Scheduled' ? heat : undefined;
      if (!target) {
        const filled = await session.fillRound(round.id, 'Next');
        if (!filled.ok) return;
        const list = await session.listHeats();
        heats = list;
        const drawn = list.filter((h) => h.round === round.id && h.phase === 'Scheduled').at(-1);
        if (!drawn) {
          actionError = 'The round did not draw a new run. Open the full console to check it.';
          return;
        }
        target = drawn.heat;
        const focused = await session.setCurrentHeat(target);
        if (!focused.ok) return;
      }
      const staged = await session.send({ Stage: { heat: target } });
      if (!staged.ok) return;
      const started = await session.send({ Start: { heat: target } });
      if (!started.ok) return;
    } finally {
      busy = false;
    }
  }

  /** Stop the run and save it: `ForceEnd` closes the heat, `Finalize` makes its laps count. */
  async function stopRun(): Promise<void> {
    if (!heat || busy) return;
    busy = true;
    actionError = undefined;
    const target = heat;
    try {
      if (phase === 'Running') {
        const ended = await session.send({ ForceEnd: { heat: target } });
        if (!ended.ok) return;
      }
      const saved = await session.send({ Finalize: { heat: target } });
      if (!saved.ok) return;
      const next = new Map(stoppedAt);
      next.set(target, new Date());
      stoppedAt = next;
      const list = await session.fetchHeatLaps(target);
      if (list) storeLaps(target, runLaps(list));
      await refreshStandings();
    } finally {
      busy = false;
    }
  }

  /** A run that stopped but did not save (a failed Finalize, or a refresh in between). */
  async function saveRun(): Promise<void> {
    await stopRun();
  }

  // ── Submission ──────────────────────────────────────────────────────────────────────────────
  const submission = $derived.by(() => {
    if (!event || !bestRun?.best) return undefined;
    return submissionText({
      track: event.name,
      callsign,
      best: bestRun.best,
      run: bestRun.number,
      runs: runRows.length,
      flownAt: stoppedAt.get(bestRun.heat) ?? new Date(event.created_at),
      timedBy: timerLabel
    });
  });

  const overlay = $derived(
    typeof location !== 'undefined' ? overlayUrl(location) : '#/overlay/racegow'
  );

  async function copy(text: string, what: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(text);
      toast.success(`${what} copied.`);
    } catch {
      // The Clipboard API needs a secure context; over plain LAN HTTP it throws. The text sits in
      // a selectable box right there, so say so rather than fail silently.
      toast.info(`Couldn’t copy automatically — select the ${what.toLowerCase()} and copy it.`);
    }
  }

  function lapLabel(index: number): string {
    return `Lap ${index + 1}`;
  }
  function inWindow(index: number, w: ConsecutiveWindow | undefined): boolean {
    return w !== undefined && index >= w.start && index < w.start + w.laps.length;
  }
  function phaseLabel(p: HeatSummary['phase']): string {
    switch (p) {
      case 'Final':
        return 'saved';
      case 'Running':
        return 'flying';
      case 'Unofficial':
        return 'stopped';
      default:
        return p.toLowerCase();
    }
  }
</script>

<div class="page">
  <div class="page-inner">
    <div class="brand-row"><Brand onclick={onhome} /></div>
    <Breadcrumbs
      crumbs={[
        { label: 'Home', onclick: onhome },
        { label: 'RaceGOW', onclick: onback },
        { label: event?.name ?? 'Run' }
      ]}
    />

    <header class="page-head">
      <div class="page-titles">
        <h1 class="page-title">{event?.name ?? 'RaceGOW run'}</h1>
        <p class="page-sub">
          {callsign} on {timerLabel}
          {#if timer}
            · <Badge tone={timerDown ? 'danger' : 'success'} dot>
              {timerDown ? 'timer not connected' : 'timer connected'}
            </Badge>
          {/if}
        </p>
      </div>
      <div class="page-actions">
        <Button variant="ghost" size="sm" onclick={onconsole}>Open in the full console</Button>
      </div>
    </header>

    {#if !round}
      <Banner tone="danger" title="This run has no RaceGOW round.">
        The event was changed in the full console. Add a Time Trials round scored by best 3 laps, or
        start a new run.
      </Banner>
    {/if}
    {#if timerDown}
      <Banner tone="warn" title={`${timer?.name ?? 'The timer'} is not connected.`}>
        Laps cannot arrive until it is.
        {#snippet actions()}
          <Button size="sm" variant="secondary" onclick={ontimers}>Timers</Button>
        {/snippet}
      </Banner>
    {/if}
    <ErrorBanner error={session.lastCommandError} ondismiss={() => session.clearCommandError()} />
    {#if actionError}
      <Banner tone="danger" title="That didn’t work." ondismiss={() => (actionError = undefined)}>
        {actionError}
      </Banner>
    {/if}

    <!-- ── The run: one big button, the clock, and this run's laps ─────────────────────────── -->
    <Card elevation="md">
      <section class="run" aria-label="Current run" data-view={view}>
        <div class="run-head">
          <div class="run-state">
            <span class="kicker">
              {#if view === 'idle'}
                {runRows.length === 0
                  ? 'Ready when you are'
                  : `${runRows.length} ${runRows.length === 1 ? 'run' : 'runs'} flown`}
              {:else if view === 'ready'}
                Run {runRows.length} is drawn
              {:else if view === 'staged'}
                Run {runRows.length} · staged
              {:else if view === 'arming'}
                Run {runRows.length} · timer going live…
              {:else if view === 'running'}
                Run {runRows.length} · flying
              {:else}
                Run {runRows.length} · stopped
              {/if}
            </span>
            <RaceClock elapsedMs={clock.elapsedMs} label="Run time" />
          </div>
          <div class="run-controls">
            {#if view === 'idle'}
              <Button variant="primary" size="lg" onclick={startRun} disabled={busy || !round}>
                {busy ? 'Starting…' : runRows.length === 0 ? 'Start run' : 'Fly again'}
              </Button>
            {:else if view === 'ready' || view === 'staged'}
              <Button variant="primary" size="lg" onclick={startRun} disabled={busy}>
                {busy ? 'Starting…' : 'Start run'}
              </Button>
            {:else if view === 'arming'}
              <Button variant="primary" size="lg" disabled>Going live…</Button>
            {:else if view === 'running'}
              <Button variant="danger" size="lg" onclick={stopRun} disabled={busy}>
                {busy ? 'Stopping…' : 'Stop run'}
              </Button>
            {:else}
              <Button variant="primary" size="lg" onclick={saveRun} disabled={busy}>
                {busy ? 'Saving…' : 'Save run'}
              </Button>
            {/if}
          </div>
        </div>

        <div class="run-body">
          <div class="run-best">
            <span class="kicker">This run · best {RACEGOW_LAPS} laps</span>
            <span class="big-time" aria-label="This run's best 3 consecutive laps">
              {currentBest ? formatMicros(currentBest.totalMicros) : '—'}
            </span>
            {#if currentBest}
              <span class="split">{currentBest.laps.map((l) => formatMicros(l)).join(' · ')}</span>
            {:else if view === 'running' || view === 'arming'}
              <span class="split muted">Fly {RACEGOW_LAPS} laps in a row to get a time.</span>
            {/if}
          </div>

          <ol class="laps" aria-label="This run's laps">
            {#each currentLaps as lap, i (lap.end_ref)}
              <li class="lap" class:in-window={inWindow(i, currentBest)}>
                <span class="lap-n">{lapLabel(i)}</span>
                <span class="lap-t">{formatMicros(lap.duration_micros)}</span>
              </li>
            {/each}
            {#if currentLaps.length === 0 && heat}
              <li class="lap muted-row">No laps yet.</li>
            {/if}
          </ol>
        </div>
      </section>
    </Card>

    <!-- ── The result: the round-wide best, the submission line, the overlay ──────────────── -->
    <div class="two-col">
      <Card title="Your best so far" elevation="sm">
        <div class="result">
          <span class="big-time headline" aria-label="Best 3 consecutive laps, all runs">
            {headlineMicros !== undefined ? formatMicros(headlineMicros) : '—'}
          </span>
          {#if bestRun?.best}
            <span class="split">
              Run {bestRun.number}, laps {bestRun.best.start + 1}–{bestRun.best.start +
                bestRun.best.laps.length}: {bestRun.best.laps
                .map((l) => formatMicros(l))
                .join(' · ')}
            </span>
          {:else}
            <span class="split muted"
              >Save a run with {RACEGOW_LAPS} laps in a row and it shows here.</span
            >
          {/if}
          {#if submission}
            <label class="sub-label" for="racegow-submission">For the RaceGOW form</label>
            <textarea
              id="racegow-submission"
              class="submission"
              readonly
              rows="4"
              value={submission}
              aria-label="Submission text"
            ></textarea>
            <div class="row-actions">
              <Button
                size="sm"
                variant="secondary"
                onclick={() => copy(submission, 'Submission text')}
              >
                Copy
              </Button>
            </div>
          {/if}
        </div>
      </Card>

      <Card title="Timer in your video" elevation="sm">
        <div class="overlay-help">
          <p class="muted">
            RaceGOW wants a timer in view. Add this page to OBS as a <strong>Browser source</strong>
            over your goggles’ DVR or a camera on the track, and record. It shows the clock, the laps
            and the best 3 so far, on a transparent background.
          </p>
          <code class="url" aria-label="Overlay URL">{overlay}</code>
          <div class="row-actions">
            <Button size="sm" variant="secondary" onclick={() => copy(overlay, 'Overlay URL')}>
              Copy URL
            </Button>
            <a class="open-link" href={overlay} target="_blank" rel="noopener">Open overlay</a>
          </div>
        </div>
      </Card>
    </div>

    <Card title="Runs" elevation="sm">
      {#if runRows.length === 0}
        <p class="muted">Every run you fly on this track is listed here with its best 3 laps.</p>
      {:else}
        <table class="runs" aria-label="Runs">
          <thead>
            <tr>
              <th>Run</th>
              <th>State</th>
              <th class="num">Laps</th>
              <th class="num">Best {RACEGOW_LAPS}</th>
            </tr>
          </thead>
          <tbody>
            {#each runRows as row (row.heat)}
              <tr class:best={bestRun?.heat === row.heat}>
                <td>Run {row.number}</td>
                <td>{phaseLabel(row.phase)}</td>
                <td class="num">{row.laps}</td>
                <td class="num">{row.best ? formatMicros(row.best.totalMicros) : '—'}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </Card>
  </div>
</div>

<style>
  .brand-row {
    margin-bottom: var(--gf-space-4);
  }
  .page {
    min-height: 100vh;
    padding: var(--gf-space-6) var(--gf-space-8) var(--gf-space-8);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    overflow: auto;
  }
  .page-inner {
    width: min(56rem, 100%);
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
  }
  .page-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--gf-space-4);
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
  .page-sub {
    margin: 0;
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
    color: var(--gf-text-muted);
  }
  .page-actions {
    flex-shrink: 0;
  }

  /* ── The run panel ─────────────────────────────────────────────────────── */
  .run {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
  }
  .run-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-5);
    flex-wrap: wrap;
  }
  .run-state {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .run-state :global(.gridfpv-race-clock) {
    font-size: var(--gf-font-size-3xl);
  }
  .kicker {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .run[data-view='running'] .kicker {
    color: var(--gf-accent);
  }
  .run-controls :global(button) {
    min-width: 11rem;
    font-size: var(--gf-font-size-lg);
  }
  .run-body {
    display: grid;
    grid-template-columns: minmax(14rem, 1fr) minmax(12rem, 1fr);
    gap: var(--gf-space-6);
  }
  @media (max-width: 40rem) {
    .run-body {
      grid-template-columns: 1fr;
    }
  }
  .run-best,
  .result {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .big-time {
    font-family: var(--gf-font-mono);
    font-size: var(--gf-font-size-3xl);
    font-weight: var(--gf-font-weight-bold);
    font-variant-numeric: tabular-nums;
    letter-spacing: -0.02em;
    line-height: 1.1;
  }
  .big-time.headline {
    color: var(--gf-accent);
  }
  .split {
    font-family: var(--gf-font-mono);
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .muted {
    color: var(--gf-text-muted);
    margin: 0;
  }

  .laps {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    max-height: 18rem;
    overflow: auto;
  }
  .lap {
    display: flex;
    justify-content: space-between;
    padding: var(--gf-space-2) var(--gf-space-3);
    border-radius: var(--gf-radius-sm);
    font-variant-numeric: tabular-nums;
  }
  .lap.in-window {
    background: var(--gf-accent-soft);
    color: var(--gf-accent);
    font-weight: var(--gf-font-weight-semibold);
  }
  .lap-t {
    font-family: var(--gf-font-mono);
  }
  .muted-row {
    color: var(--gf-text-faint);
  }

  /* ── Result + overlay ──────────────────────────────────────────────────── */
  .two-col {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--gf-space-5);
  }
  @media (max-width: 48rem) {
    .two-col {
      grid-template-columns: 1fr;
    }
  }
  .sub-label {
    margin-top: var(--gf-space-3);
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .submission,
  .url {
    display: block;
    width: 100%;
    padding: var(--gf-space-3);
    border-radius: var(--gf-radius-sm);
    border: 1px solid var(--gf-border);
    background: var(--gf-surface-sunken);
    color: var(--gf-text);
    font-family: var(--gf-font-mono);
    font-size: var(--gf-font-size-xs);
    line-height: 1.5;
    resize: vertical;
    user-select: all;
    overflow-wrap: anywhere;
  }
  .row-actions {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    margin-top: var(--gf-space-2);
  }
  .overlay-help {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .open-link {
    color: var(--gf-accent);
    font-size: var(--gf-font-size-sm);
  }

  /* ── Runs table ────────────────────────────────────────────────────────── */
  .runs {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--gf-font-size-sm);
  }
  .runs th,
  .runs td {
    text-align: left;
    padding: var(--gf-space-2) var(--gf-space-3);
    border-bottom: 1px solid var(--gf-border-subtle);
  }
  .runs th {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .runs .num {
    text-align: right;
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
  }
  .runs tr.best td {
    color: var(--gf-accent);
    font-weight: var(--gf-font-weight-semibold);
  }
</style>
