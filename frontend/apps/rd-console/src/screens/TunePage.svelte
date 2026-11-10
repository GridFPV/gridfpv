<script lang="ts">
  /**
   * TunePage — one timer's **Tune** page (Home › Timers › ‹timer name›).
   *
   * The director watches each seat's live signal and sets the timer so every pass makes one
   * crossing. The page has four parts:
   *
   * - **Seats**: each seat's frequency, whether it is on, and its live numbers.
   * - **Signal**: a scrolling trace for each seat that is on, with the enter and exit levels
   *   and the noise floor drawn on it and each crossing shaded.
   * - **Detection**: how the enter and exit levels are set, the minimum lap time and how many
   *   seats share a receiver.
   * - **Apply**: edits are a draft. Nothing reaches the timer until "Apply to timer" sends the
   *   whole of the settings. What comes back is what the timer is doing, and replaces the draft.
   *
   * The settings live on the timer. They are read from it when the page opens, after every
   * apply and on "Discard changes". The live signal comes over the timer's live feed; the
   * timer's name and status come from the timer list, read again every {@link TIMER_POLL_MS}.
   * The feed and the reads both stop when the page closes.
   *
   * In this version of the timer's protocol the enter and exit setting is one for the whole
   * timer, so Detection is not per seat.
   */
  import { untrack } from 'svelte';
  import {
    Badge,
    Banner,
    Button,
    Card,
    Field,
    Input,
    Select,
    StatusPill,
    toast
  } from '@gridfpv/components';
  import type {
    AdapterId,
    SeatLevels,
    SeatRef,
    SignalChunk,
    SignalUnit,
    TimerConfig,
    TimerView
  } from '@gridfpv/types';
  import Brand from '../Brand.svelte';
  import Breadcrumbs from '../Breadcrumbs.svelte';
  import { CHANNEL_GROUPS, channelLabel } from '../lib/channels.js';
  import { buildNames } from '../lib/names.js';
  import type { Session } from '../lib/session.svelte.js';
  import { SignalRing, TimerClock } from '../lib/signal.js';
  import { TIMER_POLL_MS } from '../lib/timers.js';
  import {
    addSeat,
    draftConfig,
    draftFrom,
    fieldNumber,
    formatLevel,
    isDirty,
    levelsInUse,
    nextSeat,
    withMode,
    type ThresholdMode,
    type TuneDraft
  } from '../lib/tune.js';
  import SignalTrace from './SignalTrace.svelte';

  let {
    session,
    timer,
    onhome,
    ontimers
  }: {
    session: Session;
    /** The timer's id. A wire handle: it is never shown. */
    timer: AdapterId;
    onhome: () => void;
    ontimers: () => void;
  } = $props();

  const say = (e: unknown): string => (e instanceof Error ? e.message : String(e));

  // ── The timer's name and status, from the timer list ───────────────────────
  let timers = $state.raw<TimerView[] | undefined>(undefined);
  let listError = $state<string | undefined>(undefined);

  async function loadTimers() {
    try {
      timers = await session.listTimers();
      listError = undefined;
    } catch (e) {
      listError = say(e);
    }
  }

  $effect(() => {
    void loadTimers();
    const poll = setInterval(() => void loadTimers(), TIMER_POLL_MS);
    return () => clearInterval(poll);
  });

  const names = $derived(buildNames([], [], timers ?? []));
  const view = $derived(timers?.find((candidate) => candidate.timer.id === timer));
  /** The name people see. Before the list lands there is none to show yet. */
  const timerLabel = $derived(timers === undefined ? 'Timer' : names.timer(timer));
  const connected = $derived(view?.status.connected ?? false);
  const seatName = (seat: SeatRef, position: number): string => names.seat(timer, seat, position);

  // ── The settings: what the timer has, and the draft ────────────────────────
  /** What the timer reported last. `undefined` until the first read lands. */
  let config = $state.raw<TimerConfig | undefined>(undefined);
  /** What the director is editing. */
  let draft = $state<TuneDraft | undefined>(undefined);
  let loading = $state(false);
  /** Why the last read failed, in the Director's words. */
  let loadError = $state<string | undefined>(undefined);
  /** Counts reads, so an answer that comes back late is dropped. */
  let reads = 0;

  async function loadConfig() {
    const read = (reads += 1);
    loading = true;
    try {
      const fresh = await session.getTimerConfig(timer);
      if (read !== reads) return;
      config = fresh;
      draft = draftFrom(fresh);
      loadError = undefined;
    } catch (e) {
      if (read !== reads) return;
      loadError = say(e);
    } finally {
      if (read === reads) loading = false;
    }
  }

  // Read the settings when the page opens, and again when a timer that was not connected
  // connects while the page still has nothing to show.
  let wasConnected: boolean | undefined;
  $effect(() => {
    const now = connected;
    const first = wasConnected === undefined;
    const justConnected = now && wasConnected === false;
    wasConnected = now;
    if (!first && !justConnected) return;
    untrack(() => {
      if (!config && !loading) void loadConfig();
    });
  });

  const result = $derived(draft ? draftConfig(draft, seatName) : undefined);
  const dirty = $derived(draft && config ? isDirty(draft, config) : false);

  // ── The live feed ──────────────────────────────────────────────────────────
  // Samples arrive hundreds of times a second, so they go into plain ring buffers the traces
  // read on each frame, never into reactive state. Levels arrive about once a second and are
  // shown as text, so they are state.
  const rings = new Map<SeatRef, SignalRing>();
  const clock = new TimerClock();
  let unit = $state<SignalUnit>('TenthsDb');
  let levels = $state.raw<ReadonlyMap<SeatRef, SeatLevels>>(new Map());
  let liveError = $state<string | undefined>(undefined);

  function ringFor(seat: SeatRef): SignalRing {
    let ring = rings.get(seat);
    if (!ring) {
      ring = new SignalRing();
      rings.set(seat, ring);
    }
    return ring;
  }

  function onSignal(chunk: SignalChunk) {
    ringFor(chunk.seat).push(chunk.from, chunk.period_micros, chunk.levels);
    if (chunk.levels.length > 0) {
      clock.observe(
        chunk.from + (chunk.levels.length - 1) * chunk.period_micros,
        performance.now()
      );
    }
    if (chunk.unit !== unit) unit = chunk.unit;
  }

  function onLevels(list: SeatLevels[]) {
    levels = new Map(list.map((seat) => [seat.seat, seat]));
  }

  $effect(() => {
    const id = timer;
    try {
      const stop = session.subscribeTimerLive(id, { onSignal, onLevels });
      liveError = undefined;
      return stop;
    } catch (e) {
      liveError = say(e);
    }
  });

  // Nothing arrives while the timer is not connected, so the last levels would be stale.
  $effect(() => {
    if (!connected && untrack(() => levels.size > 0)) levels = new Map();
  });

  // ── Editing ────────────────────────────────────────────────────────────────
  /** The picker's value for a frequency: the frequency when it is a channel, else "other". */
  function pickerValue(mhz: number | undefined): string {
    return mhz !== undefined && channelLabel(mhz) ? String(mhz) : 'other';
  }

  function pickChannel(position: number, value: string) {
    if (!draft) return;
    // "Other" empties the field, so the director types the frequency.
    draft.seats[position].mhz = value === 'other' ? '' : Number(value);
  }

  function setMode(mode: ThresholdMode) {
    if (!draft || !config) return;
    draft = withMode(draft, mode, levelsInUse([...levels.values()], config));
  }

  function add() {
    if (draft) draft = addSeat(draft);
  }

  // ── Apply and discard ──────────────────────────────────────────────────────
  let applying = $state(false);
  /** Why the last apply failed, in the Director's words. */
  let applyError = $state<string | undefined>(undefined);

  async function apply() {
    if (applying || !result?.ok) return;
    applying = true;
    applyError = undefined;
    try {
      const kept = await session.putTimerConfig(timer, result.config);
      if (!kept) return;
      reads += 1; // an older read must not land on top of this
      loading = false;
      config = kept;
      draft = draftFrom(kept);
      loadError = undefined;
      toast.success(`Applied to “${timerLabel}”.`);
    } catch (e) {
      applyError = say(e);
    } finally {
      applying = false;
    }
  }

  async function discard() {
    applyError = undefined;
    if (config) draft = draftFrom(config);
    await loadConfig();
  }
</script>

<div class="page">
  <div class="page-inner">
    <div class="brand-row"><Brand onclick={onhome} /></div>
    <Breadcrumbs
      crumbs={[
        { label: 'Home', onclick: onhome },
        { label: 'Timers', onclick: ontimers },
        { label: timerLabel }
      ]}
    />

    <header class="page-head">
      <div class="headline">
        <h1 class="page-title">{timerLabel}</h1>
        {#if view}
          {#if connected}
            <StatusPill status="Connected" label="Connected" size="sm" />
          {:else}
            <StatusPill status="Disconnected" label="Not connected" size="sm" />
          {/if}
        {/if}
      </div>
      {#if view}<span class="address">{view.timer.address}</span>{/if}
    </header>

    {#if listError}
      <Banner tone="danger" title="The timers could not be read.">{listError}</Banner>
    {/if}

    {#if timers === undefined}
      {#if !listError}<p class="muted" role="status">Loading…</p>{/if}
    {:else if !view}
      <Banner tone="warn" title="This timer is not in the list any more.">
        It may have been removed.
        {#snippet actions()}
          <Button variant="secondary" onclick={ontimers}>Back to Timers</Button>
        {/snippet}
      </Banner>
    {:else}
      {#if !connected}
        <div data-testid="not-connected">
          <Banner tone="warn" title="{view.timer.name} is not connected.">
            {view.status.problem ?? 'The Director is still trying to reach it.'}
          </Banner>
        </div>
      {/if}
      {#if liveError}
        <Banner tone="danger" title="The live signal cannot be shown.">{liveError}</Banner>
      {/if}
      {#if loadError}
        <div data-testid="settings-problem">
          <Banner tone="danger" title="The timer's settings could not be read.">
            {loadError}
            {#snippet actions()}
              <Button variant="secondary" disabled={loading} onclick={() => void loadConfig()}>
                {loading ? 'Reading…' : 'Try again'}
              </Button>
            {/snippet}
          </Banner>
        </div>
      {/if}

      {#if !config || !draft}
        {#if loading && !loadError}
          <p class="muted" role="status">Reading the timer's settings…</p>
        {/if}
      {:else}
        <!-- ── Seats ─────────────────────────────────────────────────────── -->
        <Card title="Seats" subtitle="The frequency each seat listens on, and whether it is on.">
          {#snippet actions()}
            <Button
              variant="secondary"
              disabled={nextSeat(draft!) === undefined}
              title={nextSeat(draft!) === undefined
                ? 'This timer already has all 8 seats.'
                : undefined}
              onclick={add}>+ Add seat</Button
            >
          {/snippet}
          {#if draft.seats.length === 0}
            <p class="muted">This timer has no seats yet. Add one to start.</p>
          {:else}
            <ul class="seats" aria-label="Seats">
              {#each draft.seats as seat, position (seat.seat)}
                {@const label = seatName(seat.seat, position)}
                {@const mhz = fieldNumber(seat.mhz)}
                {@const channel = mhz === undefined ? undefined : channelLabel(mhz)}
                {@const live = levels.get(seat.seat)}
                {@const onTimer = config.seats.find((kept) => kept.seat === seat.seat)}
                <li class="seat" aria-label={label}>
                  <span class="seat-name">{label}</span>
                  <div class="frequency">
                    <Select
                      size="sm"
                      aria-label="{label} channel"
                      value={pickerValue(mhz)}
                      onchange={(e: Event) =>
                        pickChannel(position, (e.currentTarget as HTMLSelectElement).value)}
                    >
                      {#each CHANNEL_GROUPS as group (group.band)}
                        <optgroup label={group.band}>
                          {#each group.choices as choice (choice.mhz)}
                            <option value={String(choice.mhz)}>{choice.label}</option>
                          {/each}
                        </optgroup>
                      {/each}
                      <option value="other">Other frequency</option>
                    </Select>
                    <span class="mhz">
                      <Input
                        size="sm"
                        type="number"
                        inputmode="numeric"
                        min="1"
                        step="1"
                        aria-label="{label} frequency in MHz"
                        bind:value={seat.mhz}
                      />
                      <span class="unit">MHz</span>
                    </span>
                    <span class="channel" data-testid="seat-channel">{channel ?? ''}</span>
                  </div>
                  <label class="switch">
                    <input
                      type="checkbox"
                      role="switch"
                      aria-label="{label} on"
                      bind:checked={seat.enabled}
                    />
                    <span>{seat.enabled ? 'On' : 'Off'}</span>
                  </label>
                  <div class="live" data-testid="seat-live">
                    {#if !onTimer}
                      <span class="muted">Not on the timer yet</span>
                    {:else if !onTimer.enabled}
                      <span class="muted">Off on the timer</span>
                    {:else if !live}
                      <span class="muted">
                        {connected ? 'Waiting for levels…' : 'No signal while not connected'}
                      </span>
                    {:else}
                      <span class="number">
                        <span class="muted">Level</span>
                        <strong>{formatLevel(live.level, unit)}</strong>
                      </span>
                      <span class="number">
                        <span class="muted">Noise floor</span>
                        <strong>{formatLevel(live.floor, unit)}</strong>
                      </span>
                      <span class="lamp" class:lit={live.crossing} data-crossing={live.crossing}>
                        <span class="lamp-dot" aria-hidden="true"></span>
                        {live.crossing ? 'Crossing now' : 'No crossing'}
                      </span>
                    {/if}
                  </div>
                </li>
              {/each}
            </ul>
          {/if}
        </Card>

        <!-- ── Signal ────────────────────────────────────────────────────── -->
        <Card
          title="Signal"
          subtitle="The last 10 seconds on each seat that is on. A shaded band is a crossing."
        >
          {@const shown = config.seats
            .map((seat, position) => ({ seat, position }))
            .filter(({ seat }) => seat.enabled)}
          {#if shown.length === 0}
            <p class="muted">No seat is on. Turn a seat on and apply to see its signal.</p>
          {:else}
            {#if !connected}
              <p class="muted">No signal arrives while the timer is not connected.</p>
            {/if}
            <ul class="traces" aria-label="Signal">
              {#each shown as { seat, position } (seat.seat)}
                {@const label = seatName(seat.seat, position)}
                {@const live = levels.get(seat.seat)}
                <li class="trace-row" aria-label={label}>
                  <div class="trace-head">
                    <span class="seat-name">{label}</span>
                    <span class="muted">
                      {channelLabel(seat.freq_mhz)
                        ? `${channelLabel(seat.freq_mhz)} · ${seat.freq_mhz} MHz`
                        : `${seat.freq_mhz} MHz`}
                    </span>
                  </div>
                  <SignalTrace
                    ring={ringFor(seat.seat)}
                    {clock}
                    levels={live}
                    {unit}
                    label="Signal on {label} over the last 10 seconds"
                  />
                  <dl class="summary" aria-label="{label} levels">
                    <div>
                      <dt>Level</dt>
                      <dd>{formatLevel(live?.level, unit)}</dd>
                    </div>
                    <div>
                      <dt>Noise floor</dt>
                      <dd>{formatLevel(live?.floor, unit)}</dd>
                    </div>
                    <div>
                      <dt class="enter">Enter</dt>
                      <dd>{formatLevel(live?.enter, unit)}</dd>
                    </div>
                    <div>
                      <dt class="exit">Exit</dt>
                      <dd>{formatLevel(live?.exit, unit)}</dd>
                    </div>
                  </dl>
                </li>
              {/each}
            </ul>
          {/if}
        </Card>

        <!-- ── Detection ─────────────────────────────────────────────────── -->
        <Card
          title="Detection"
          subtitle="When the timer counts a crossing. These settings are for every seat on this timer."
        >
          <div class="detection">
            <fieldset class="mode">
              <legend>Enter and exit levels</legend>
              <label class="radio">
                <input
                  type="radio"
                  name="threshold-mode"
                  checked={draft.mode === 'Auto'}
                  onchange={() => setMode('Auto')}
                />
                <span>Follow the noise floor</span>
              </label>
              <label class="radio">
                <input
                  type="radio"
                  name="threshold-mode"
                  checked={draft.mode === 'Manual'}
                  onchange={() => setMode('Manual')}
                />
                <span>Fixed levels</span>
              </label>
            </fieldset>

            <div class="fields">
              {#if draft.mode === 'Auto'}
                <Field
                  label="Enter this far above the floor (dB)"
                  hint="A crossing starts when the signal rises this far above the noise floor."
                >
                  <Input
                    type="number"
                    inputmode="decimal"
                    step="0.5"
                    aria-label="Enter this far above the floor, in dB"
                    bind:value={draft.aboveFloorDb}
                  />
                </Field>
                <Field
                  label="Exit this far below enter (dB)"
                  hint="The crossing ends when the signal falls this far below the enter level."
                >
                  <Input
                    type="number"
                    inputmode="decimal"
                    step="0.5"
                    aria-label="Exit this far below enter, in dB"
                    bind:value={draft.belowEnterDb}
                  />
                </Field>
              {:else}
                <Field label="Enter level (dB)" hint="A crossing starts at this signal level.">
                  <Input
                    type="number"
                    inputmode="decimal"
                    step="0.5"
                    aria-label="Enter level, in dB"
                    bind:value={draft.enterDb}
                  />
                </Field>
                <Field
                  label="Exit level (dB)"
                  hint="The crossing ends when the signal falls below this level."
                >
                  <Input
                    type="number"
                    inputmode="decimal"
                    step="0.5"
                    aria-label="Exit level, in dB"
                    bind:value={draft.exitDb}
                  />
                </Field>
              {/if}
              <Field
                label="Minimum lap time (seconds)"
                hint="A second crossing on a seat sooner than this is ignored."
              >
                <Input
                  type="number"
                  inputmode="decimal"
                  min="0"
                  step="0.1"
                  aria-label="Minimum lap time, in seconds"
                  bind:value={draft.minLapSeconds}
                />
              </Field>
              <Field
                label="Seats per receiver"
                hint="More seats on one receiver means each seat's signal is read less often."
              >
                <Input
                  type="number"
                  inputmode="numeric"
                  min="1"
                  step="1"
                  aria-label="Seats per receiver"
                  bind:value={draft.seatsPerReceiver}
                />
              </Field>
            </div>
          </div>
        </Card>

        <!-- ── Apply ─────────────────────────────────────────────────────── -->
        <div class="apply" data-dirty={dirty}>
          {#if applyError}
            <div data-testid="apply-problem">
              <Banner
                tone="danger"
                title="The timer was not changed."
                ondismiss={() => (applyError = undefined)}>{applyError}</Banner
              >
            </div>
          {/if}
          <div class="apply-row">
            <p class="apply-state" role="status">
              {#if dirty}
                <Badge tone="warn" dot>Not applied</Badge>
                <span>
                  {result && !result.ok
                    ? result.problem
                    : 'These changes are not on the timer yet.'}
                </span>
              {:else}
                <span class="muted">This is what the timer is using.</span>
              {/if}
            </p>
            <div class="apply-actions">
              <Button variant="ghost" disabled={!dirty || applying} onclick={() => void discard()}>
                Discard changes
              </Button>
              <Button
                variant="primary"
                disabled={!dirty || applying || !result?.ok}
                onclick={() => void apply()}
              >
                {applying ? 'Applying…' : 'Apply to timer'}
              </Button>
            </div>
          </div>
        </div>
      {/if}
    {/if}
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
    width: min(64rem, 100%);
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
  }
  .page-head {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
  }
  .headline {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
  }
  .page-title {
    margin: 0;
    font-size: var(--gf-font-size-2xl);
    letter-spacing: var(--gf-tracking-tight);
    overflow-wrap: anywhere;
  }
  .address {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    font-variant-numeric: tabular-nums;
    overflow-wrap: anywhere;
  }
  .muted {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }

  /* Seats */
  .seats,
  .traces {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .seat {
    display: grid;
    grid-template-columns: 4.5rem minmax(0, auto) auto minmax(0, 1fr);
    align-items: center;
    gap: var(--gf-space-3) var(--gf-space-4);
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
  }
  .seat-name {
    font-weight: var(--gf-font-weight-semibold);
  }
  .frequency {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
  }
  .frequency :global(.gf-select) {
    width: 12.5rem;
    flex-shrink: 0;
  }
  .mhz {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-1);
    width: 8.5rem;
  }
  .unit {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .channel {
    min-width: 4.5rem;
    font-weight: var(--gf-font-weight-semibold);
    font-variant-numeric: tabular-nums;
  }
  .switch,
  .radio {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    cursor: pointer;
  }
  .switch input,
  .radio input {
    width: 1.15rem;
    height: 1.15rem;
    accent-color: var(--gf-accent);
  }
  .switch span {
    min-width: 2rem;
  }
  .live {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--gf-space-4);
    flex-wrap: wrap;
    font-size: var(--gf-font-size-sm);
    font-variant-numeric: tabular-nums;
  }
  .number {
    display: inline-flex;
    gap: var(--gf-space-2);
    align-items: baseline;
  }
  .lamp {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    min-width: 8.5rem;
    padding: var(--gf-space-1) var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-pill);
    color: var(--gf-text-muted);
  }
  .lamp-dot {
    width: 0.7rem;
    height: 0.7rem;
    border-radius: 50%;
    background: var(--gf-border-strong);
  }
  .lamp.lit {
    border-color: var(--gf-success);
    background: var(--gf-success-soft);
    color: var(--gf-text);
    font-weight: var(--gf-font-weight-semibold);
  }
  .lamp.lit .lamp-dot {
    background: var(--gf-success);
    box-shadow: 0 0 0.5rem var(--gf-success);
  }

  /* Signal */
  .traces {
    gap: var(--gf-space-4);
  }
  .trace-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 11rem;
    grid-template-areas:
      'head head'
      'plot numbers';
    gap: var(--gf-space-2) var(--gf-space-4);
    align-items: start;
  }
  .trace-head {
    grid-area: head;
    display: flex;
    align-items: baseline;
    gap: var(--gf-space-3);
  }
  .summary {
    grid-area: numbers;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
    font-size: var(--gf-font-size-sm);
    font-variant-numeric: tabular-nums;
  }
  .summary div {
    display: flex;
    justify-content: space-between;
    gap: var(--gf-space-3);
  }
  .summary dt {
    color: var(--gf-text-muted);
  }
  .summary dt.enter {
    color: var(--gf-danger);
  }
  .summary dt.exit {
    color: var(--gf-warn);
  }
  .summary dd {
    margin: 0;
    font-weight: var(--gf-font-weight-semibold);
  }

  /* Detection */
  .detection {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .mode {
    margin: 0;
    padding: 0;
    border: 0;
    display: flex;
    align-items: center;
    gap: var(--gf-space-5);
    flex-wrap: wrap;
  }
  .mode legend {
    padding: 0;
    margin-bottom: var(--gf-space-2);
    font-weight: var(--gf-font-weight-semibold);
  }
  .fields {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(15rem, 1fr));
    gap: var(--gf-space-4);
  }

  /* Apply: stays in reach at the bottom of the window while the director scrolls. */
  .apply {
    position: sticky;
    bottom: var(--gf-space-3);
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
    padding: var(--gf-space-3) var(--gf-space-4);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    background: var(--gf-elevated);
    box-shadow: var(--gf-shadow-md);
  }
  .apply[data-dirty='true'] {
    border-color: var(--gf-warn);
  }
  .apply-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-4);
    flex-wrap: wrap;
  }
  .apply-state {
    margin: 0;
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
  }
  .apply-actions {
    display: flex;
    gap: var(--gf-space-2);
  }

  @media (max-width: 48rem) {
    .seat {
      grid-template-columns: 1fr auto;
    }
    .frequency,
    .live {
      grid-column: 1 / -1;
      justify-content: flex-start;
    }
    .frequency {
      flex-wrap: wrap;
    }
    .trace-row {
      grid-template-columns: minmax(0, 1fr);
      grid-template-areas:
        'head'
        'plot'
        'numbers';
    }
  }
</style>
