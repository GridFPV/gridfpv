<script lang="ts">
  /**
   * CapturePage — tune one seat on a recording of its own signal.
   *
   * Start a capture, fly a few passes through the gate, stop, then drag the enter and exit
   * lines on the recording until each pass is one clean crossing. The shaded bands are the
   * passes the levels on screen would make, worked out by the same rule the timer follows.
   * Nothing reaches the timer until **Apply to timer**; the timer's answer is read back.
   *
   * On a timer that takes levels per seat, the levels are written to this seat alone. A
   * timer with one setting for all its seats gets them for every seat, and the page says so.
   *
   * The recording is kept in this page only. It is gone when the page is left.
   */
  import { onDestroy, untrack } from 'svelte';
  import { Badge, Banner, Button, Card, Select, toast } from '@gridfpv/components';
  import type {
    AdapterId,
    SeatLevels,
    SeatRef,
    SignalChunk,
    SignalUnit,
    TimerConfig,
    TimerThreshold,
    TimerView
  } from '@gridfpv/types';
  import Page from '../Page.svelte';
  import { detectPasses, levelGap, moveLevel, Recording } from '../lib/capture.js';
  import { channelLabel } from '../lib/channels.js';
  import { buildNames } from '../lib/names.js';
  import { formatHash } from '../lib/route.js';
  import type { Session } from '../lib/session.svelte.js';
  import { TIMER_POLL_MS } from '../lib/timers.js';
  import { formatDb, formatLevel } from '../lib/tune.js';
  import CaptureTrace from './CaptureTrace.svelte';

  let {
    session,
    timer,
    seat
  }: {
    session: Session;
    /** The timer's id and the seat's handle. Wire handles: neither is ever shown. */
    timer: AdapterId;
    seat: SeatRef;
  } = $props();

  const say = (e: unknown): string => (e instanceof Error ? e.message : String(e));
  const SECOND = 1_000_000;

  // ── The timer, its settings, and this seat in them ─────────────────────────
  let timers = $state.raw<TimerView[] | undefined>(undefined);
  let config = $state.raw<TimerConfig | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);

  async function loadTimers() {
    try {
      timers = await session.listTimers();
    } catch (e) {
      loadError = say(e);
    }
  }
  async function loadConfig() {
    try {
      config = await session.getTimerConfig(timer);
      loadError = undefined;
    } catch (e) {
      loadError = say(e);
    }
  }
  $effect(() => {
    void loadTimers();
    void loadConfig();
    const poll = setInterval(() => void loadTimers(), TIMER_POLL_MS);
    return () => clearInterval(poll);
  });

  const names = $derived(buildNames([], [], timers ?? []));
  const view = $derived(timers?.find((candidate) => candidate.timer.id === timer));
  const connected = $derived(view?.status.connected ?? false);
  /** Whether this timer takes levels for one seat. If not, the levels are for every seat. */
  const perSeat = $derived(view?.status.seat_thresholds ?? false);
  const position = $derived(config?.seats.findIndex((s) => s.seat === seat) ?? -1);
  const seatConfig = $derived(position >= 0 ? config?.seats[position] : undefined);
  const seatName = $derived(names.seat(timer, seat, position >= 0 ? position : undefined));
  const channel = $derived(
    seatConfig
      ? (channelLabel(seatConfig.freq_mhz) ?? '') +
          (channelLabel(seatConfig.freq_mhz) ? ' · ' : '') +
          `${seatConfig.freq_mhz} MHz`
      : ''
  );
  const seatsHref = $derived(formatHash({ kind: 'seats', timer }));

  // ── The live feed: the seat's level now, and the recording ─────────────────
  let unit = $state<SignalUnit>('TenthsDb');
  let live = $state.raw<SeatLevels | undefined>(undefined);
  let liveError = $state<string | undefined>(undefined);
  /** The recording on screen. Not reactive: `version` says when it grew. */
  let recording = $state.raw<Recording | undefined>(undefined);
  let version = $state(0);
  let recordingNow = $state(false);
  /** Whether new samples arrived since the plot was last told. */
  let grew = false;

  function onSignal(chunk: SignalChunk) {
    if (chunk.seat !== seat) return;
    if (chunk.unit !== unit) unit = chunk.unit;
    if (!recordingNow || !recording) return;
    recording.push(chunk.from, chunk.period_micros, chunk.levels);
    grew = true;
    if (recording.full) stop();
  }
  function onLevels(list: SeatLevels[]) {
    live = list.find((entry) => entry.seat === seat);
  }

  $effect(() => {
    const id = timer;
    try {
      const unsubscribe = session.subscribeTimerLive(id, { onSignal, onLevels });
      liveError = undefined;
      return unsubscribe;
    } catch (e) {
      liveError = say(e);
    }
  });

  // Samples arrive hundreds of times a second. The plot is told a few times a second.
  const redraw = setInterval(() => {
    if (!grew) return;
    grew = false;
    version += 1;
  }, 200);
  onDestroy(() => clearInterval(redraw));

  function start() {
    recording = new Recording();
    version += 1;
    recordingNow = true;
  }
  function stop() {
    recordingNow = false;
    grew = false;
    version += 1;
  }

  // ── The levels being tuned ─────────────────────────────────────────────────
  // They start from what the timer is using for this seat right now, then follow the drag.
  let enter = $state(0);
  let exit = $state(0);
  let seeded = $state(false);
  $effect(() => {
    if (seeded) return;
    const own = seatConfig?.threshold ?? config?.threshold;
    const fixed = own && 'Manual' in own ? own.Manual : undefined;
    const from = live ?? fixed;
    if (!from) return;
    untrack(() => {
      enter = from.enter;
      exit = from.exit;
      seeded = true;
    });
  });

  const holdMicros = $derived((config?.exit_hold_ms ?? 0) * 1000);
  const passes = $derived.by(() => {
    void version;
    return recording && seeded ? detectPasses(recording, enter, exit, holdMicros) : [];
  });
  const samples = $derived((void version, recording?.length ?? 0));
  const recordedSeconds = $derived((void version, (recording?.spanMicros ?? 0) / SECOND));

  /** One step of a nudge button: 1 dB, or one count on a timer that reports counts. */
  const step = $derived(unit === 'Counts' ? 1 : 10);
  function nudge(which: 'enter' | 'exit', delta: number) {
    const next = moveLevel(
      which,
      (which === 'enter' ? enter : exit) + delta,
      enter,
      exit,
      levelGap(unit)
    );
    enter = next.enter;
    exit = next.exit;
  }

  // ── Apply ──────────────────────────────────────────────────────────────────
  /** How the levels are written: as they are, or as a distance above the noise floor. */
  let how = $state<'fixed' | 'floor'>('fixed');
  const floor = $derived(live?.floor);
  /** The threshold Apply would write. */
  const wanted = $derived.by((): TimerThreshold | undefined => {
    if (!seeded) return undefined;
    if (how === 'floor') {
      if (floor === undefined || enter <= floor) return undefined;
      return { Auto: { snr_enter: enter - floor, hysteresis: enter - exit } };
    }
    return { Manual: { enter, exit } };
  });
  const inForce = $derived(
    perSeat ? (seatConfig?.threshold ?? config?.threshold) : config?.threshold
  );
  const applied = $derived(
    wanted !== undefined &&
      inForce !== undefined &&
      JSON.stringify(wanted) === JSON.stringify(inForce)
  );
  let applying = $state(false);
  let applyError = $state<string | undefined>(undefined);

  async function apply() {
    const now = config;
    const threshold = wanted;
    if (applying || !now || !threshold || position < 0) return;
    const next: TimerConfig = perSeat
      ? {
          ...now,
          seats: now.seats.map((entry) => (entry.seat === seat ? { ...entry, threshold } : entry))
        }
      : { ...now, threshold };
    applying = true;
    applyError = undefined;
    try {
      const kept = await session.putTimerConfig(timer, next);
      if (!kept) return;
      config = kept;
      toast.success(`${seatName} is set on ${names.timer(timer)}.`);
    } catch (e) {
      applyError = say(e);
    } finally {
      applying = false;
    }
  }
</script>

<Page
  title={`Tune ${seatName}`}
  help="tune"
  lede="Record the seat's signal while a pilot flies a few passes. Then set the enter and exit levels on the recording."
>
  <p class="where">
    <a href={seatsHref}>Back to Seats</a>
    {#if view}
      <span>{view.timer.name}</span>
    {/if}
    {#if channel}<span>{channel}</span>{/if}
  </p>

  {#if loadError}
    <Banner tone="danger" title="The timer's settings could not be read.">
      {loadError}
      {#snippet actions()}
        <Button variant="secondary" onclick={() => void loadConfig()}>Try again</Button>
      {/snippet}
    </Banner>
  {/if}
  {#if liveError}
    <Banner tone="danger" title="The live signal cannot be shown.">{liveError}</Banner>
  {/if}
  {#if view && !connected}
    <Banner tone="warn" title="{view.timer.name} is not connected.">
      {view.status.problem ?? 'The Director is still trying to reach it.'} Nothing can be recorded until
      it is back.
    </Banner>
  {/if}

  {#if timers !== undefined && !view}
    <Banner tone="warn" title="This timer is not in the list any more.">
      It may have been removed.
    </Banner>
  {:else if config && !seatConfig}
    <Banner tone="warn" title="This timer has no such seat.">
      <a href={seatsHref}>Pick a seat on the Seats page.</a>
    </Banner>
  {:else if seatConfig && !seatConfig.enabled}
    <Banner tone="warn" title="{seatName} is off.">
      Turn it on and apply that on the <a href={seatsHref}>Seats page</a> first.
    </Banner>
  {:else if seatConfig}
    <Card
      title="Capture"
      subtitle="Record the signal, fly a few passes through the gate, then stop."
      elevation="sm"
    >
      <div class="controls">
        {#if recordingNow}
          <Button variant="danger" onclick={stop}>Stop capture</Button>
          <Badge tone="danger" dot>
            Recording · {recordedSeconds.toFixed(0)} s · {samples} samples
          </Badge>
        {:else}
          <Button variant="primary" disabled={!connected} onclick={start}>
            {recording ? 'Capture again' : 'Start capture'}
          </Button>
          {#if recording}
            <Badge tone="success">
              Captured · {recordedSeconds.toFixed(1)} s · {samples} samples
            </Badge>
          {/if}
        {/if}
        <span class="live" data-testid="live-level">
          Level now <strong>{formatLevel(live?.level, unit)}</strong>
          · Noise floor <strong>{formatLevel(live?.floor, unit)}</strong>
          {#if live?.crossing}· <Badge tone="success" dot>Crossing now</Badge>{/if}
        </span>
      </div>
    </Card>

    <Card
      title="Recording"
      subtitle="Drag the lines, or use the buttons. A green band is a pass these levels would make. An orange one was still open when the recording ended."
      elevation="sm"
    >
      {#if !recording || samples === 0}
        <p class="muted" data-testid="no-recording">
          {recordingNow
            ? 'Waiting for the first samples…'
            : 'Nothing is recorded yet. Start a capture above.'}
        </p>
      {:else}
        <CaptureTrace
          {recording}
          {version}
          bind:enter
          bind:exit
          {passes}
          {unit}
          readonly={recordingNow}
        />
      {/if}
      {#if seeded}
        <div class="levels">
          <div class="level">
            <span class="lbl enter">Enter</span>
            <Button
              size="sm"
              variant="ghost"
              aria-label="Lower the enter level"
              onclick={() => nudge('enter', -step)}>−</Button
            >
            <strong data-testid="enter-level">{formatLevel(enter, unit)}</strong>
            <Button
              size="sm"
              variant="ghost"
              aria-label="Raise the enter level"
              onclick={() => nudge('enter', step)}>+</Button
            >
          </div>
          <div class="level">
            <span class="lbl exit">Exit</span>
            <Button
              size="sm"
              variant="ghost"
              aria-label="Lower the exit level"
              onclick={() => nudge('exit', -step)}>−</Button
            >
            <strong data-testid="exit-level">{formatLevel(exit, unit)}</strong>
            <Button
              size="sm"
              variant="ghost"
              aria-label="Raise the exit level"
              onclick={() => nudge('exit', step)}>+</Button
            >
          </div>
          {#if recording && samples > 0}
            <span class="muted" data-testid="pass-count">
              {passes.length}
              {passes.length === 1 ? 'pass' : 'passes'} with these levels
            </span>
          {/if}
        </div>
      {:else}
        <p class="muted">Waiting for the timer to report this seat's levels…</p>
      {/if}
      {#if passes.length > 0 && recording}
        <ol class="passes" aria-label="Passes with these levels">
          {#each passes as pass, index (index)}
            <li>
              <span>Pass {index + 1}</span>
              <span class="muted"
                >at {((pass.at - (recording.from ?? 0)) / SECOND).toFixed(2)} s</span
              >
              <span class="muted">peak {formatLevel(pass.peak, unit)}</span>
              <span class="muted">{((pass.end - pass.start) / SECOND).toFixed(2)} s wide</span>
              {#if pass.open}<Badge tone="warn">Still open</Badge>{/if}
            </li>
          {/each}
        </ol>
      {/if}
    </Card>

    {#if seeded}
      <Card title="Apply" elevation="sm">
        {#if applyError}
          <Banner
            tone="danger"
            title="The timer was not changed."
            ondismiss={() => (applyError = undefined)}>{applyError}</Banner
          >
        {/if}
        <div class="apply">
          <label class="how">
            <span class="eyebrow">Write them as</span>
            <Select bind:value={how} aria-label="Write the levels as" disabled={applying}>
              <option value="fixed">Fixed levels, exactly as set</option>
              <option value="floor" disabled={floor === undefined}>
                A distance above the noise floor
              </option>
            </Select>
          </label>
          <p class="apply-says" role="status" data-testid="apply-state">
            {#if how === 'floor' && wanted && 'Auto' in wanted}
              Enter {formatDb(wanted.Auto.snr_enter)} above the noise floor, exit
              {formatDb(wanted.Auto.hysteresis)} below that. The levels then move with the noise.
            {:else if how === 'floor'}
              The enter level must be above the noise floor for this.
            {:else}
              Enter at {formatLevel(enter, unit)}, exit at {formatLevel(exit, unit)}.
            {/if}
            {#if !perSeat}
              This timer has one setting for all its seats, so every seat gets these levels.
            {/if}
            {#if applied}<Badge tone="success">On the timer</Badge>{/if}
          </p>
          <Button
            variant="primary"
            disabled={applying || recordingNow || !wanted || applied}
            onclick={() => void apply()}
          >
            {applying ? 'Applying…' : 'Apply to timer'}
          </Button>
        </div>
      </Card>
    {/if}
  {:else if !loadError}
    <p class="muted" role="status">Reading the timer's settings…</p>
  {/if}
</Page>

<style>
  .where {
    margin: 0;
    display: flex;
    gap: var(--gf-space-4);
    flex-wrap: wrap;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .where a,
  :global(.gf-banner) a {
    color: var(--gf-accent);
  }
  .muted {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .controls {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
  }
  .live {
    margin-left: auto;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
    font-variant-numeric: tabular-nums;
  }
  .live strong {
    color: var(--gf-text);
  }
  .levels {
    display: flex;
    align-items: center;
    gap: var(--gf-space-5);
    flex-wrap: wrap;
    margin-top: var(--gf-space-3);
  }
  .level {
    display: flex;
    align-items: center;
    gap: var(--gf-space-1);
    font-variant-numeric: tabular-nums;
  }
  .level strong {
    min-width: 6.5ch;
    text-align: center;
  }
  .lbl,
  .eyebrow {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    font-weight: var(--gf-font-weight-bold);
  }
  .eyebrow {
    font-weight: var(--gf-font-weight-regular);
    color: var(--gf-text-muted);
  }
  .lbl.enter {
    color: var(--gf-danger);
  }
  .lbl.exit {
    color: var(--gf-warn);
  }
  .passes {
    list-style: none;
    margin: var(--gf-space-3) 0 0;
    padding: 0;
    display: grid;
    gap: var(--gf-space-1);
    font-size: var(--gf-font-size-sm);
  }
  .passes li {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
    font-variant-numeric: tabular-nums;
  }
  .apply {
    display: flex;
    align-items: flex-end;
    gap: var(--gf-space-4);
    flex-wrap: wrap;
  }
  .how {
    display: grid;
    gap: var(--gf-space-1);
    min-width: 16rem;
  }
  .apply-says {
    flex: 1 1 16rem;
    margin: 0;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-secondary);
  }
</style>
