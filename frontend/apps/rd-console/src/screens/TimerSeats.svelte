<script lang="ts">
  /**
   * TimerSeats — one timer's seats and settings: the body of the Seats page.
   *
   * The director watches each seat's live signal and sets the timer so every pass makes one
   * crossing. Top to bottom:
   *
   * - **Profile**: a saved set of this timer's settings. Picking one writes it to the timer.
   *   The settings on the timer can be saved back to it, or saved under a new name.
   * - **Seats**: each seat's frequency, whether it is on, and its live numbers. On a timer
   *   that takes levels per seat, also whether the seat uses the timer's levels or its own.
   *   **Tune** opens the seat's own page, where its signal is recorded and its levels are
   *   set on the recording.
   * - **Signal**: a scrolling trace for each seat that is on, with the enter and exit levels
   *   and the noise floor drawn on it and each crossing shaded.
   * - **Timer settings**: how the enter and exit levels are set, the exit hold and the lap
   *   cooldown. These belong to the timer, whatever is flown on it.
   * - **Apply**: edits are a draft. Nothing reaches the timer until "Apply to timer" sends the
   *   whole of the settings. What comes back is what the timer is doing, and replaces the draft.
   *
   * The settings live on the timer. They are read from it when the page opens, after every
   * apply and on "Discard changes". The live signal comes over the timer's live feed, which
   * stops when the page closes. The timer list (names and status) is the Seats page's and is
   * handed in.
   *
   * A timer whose status says it takes levels per seat lets a seat have its own; every other
   * seat uses the timer's. A timer that does not take them has one setting for all its seats,
   * and the page offers no choice per seat.
   */
  import { untrack } from 'svelte';
  import {
    Badge,
    Banner,
    Button,
    Card,
    Dialog,
    Field,
    Input,
    Select,
    toast
  } from '@gridfpv/components';
  import type {
    AdapterId,
    SeatLevels,
    SeatRef,
    SignalChunk,
    SignalUnit,
    TimerConfig,
    TimerProfiles,
    TimerView
  } from '@gridfpv/types';
  import { CHANNEL_GROUPS, channelLabel } from '../lib/channels.js';
  import ConfirmButton from '../lib/ConfirmButton.svelte';
  import { buildNames } from '../lib/names.js';
  import type { Session } from '../lib/session.svelte.js';
  import { formatHash } from '../lib/route.js';
  import { SignalRing, TimerClock } from '../lib/signal.js';
  import {
    addSeat,
    draftConfig,
    draftFrom,
    fieldNumber,
    formatLevel,
    isDirty,
    levelsInUse,
    nextSeat,
    ownLevels,
    sameConfig,
    type TuneDraft
  } from '../lib/tune.js';
  import LevelsFields from './LevelsFields.svelte';
  import SignalTrace from './SignalTrace.svelte';

  let {
    session,
    timer: timerProp,
    timers
  }: {
    session: Session;
    /** The timer's id. A wire handle: it is never shown. */
    timer: AdapterId;
    /** The timer list, as the Seats page last read it. This timer is in it. */
    timers: readonly TimerView[];
  } = $props();

  const say = (e: unknown): string => (e instanceof Error ? e.message : String(e));

  // The id as a value. The Seats page reads the timer list again every couple of seconds and
  // hands down a new one each time; read straight from the prop, every such list would look
  // like a new timer to the effects below, which would close and reopen the live feed and
  // read the profiles again on every poll.
  const timer = $derived(timerProp);

  const names = $derived(buildNames([], [], timers));
  const view = $derived(timers.find((candidate) => candidate.timer.id === timer));
  /** The name people see. */
  const timerLabel = $derived(names.timer(timer));
  const connected = $derived(view?.status.connected ?? false);
  /** Whether this timer takes levels per seat. When it does not, none is offered or sent. */
  const seatLevels = $derived(view?.status.seat_thresholds ?? false);
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

  const result = $derived(draft ? draftConfig(draft, seatName, seatLevels) : undefined);
  const dirty = $derived(draft && config ? isDirty(draft, config, seatLevels) : false);

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

  /**
   * Give a seat its own levels, or put it back on the timer's. Its own start the way the
   * timer's are set, with the levels the seat is using right now as its fixed levels.
   */
  function setOwnLevels(position: number, own: boolean) {
    if (!draft) return;
    const seat = draft.seats[position];
    if (own === (seat.levels !== undefined)) return;
    seat.levels = own ? ownLevels(draft.levels, levels.get(seat.seat)) : undefined;
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

  // ── Profiles ───────────────────────────────────────────────────────────────
  // A profile is a saved copy of the settings. Picking one writes it to the timer; the other
  // buttons only change what is saved, never the timer.
  let profiles = $state.raw<TimerProfiles | undefined>(undefined);
  let profileBusy = $state(false);
  let profileError = $state<string | undefined>(undefined);
  const activeProfile = $derived(
    profiles?.profiles.find((profile) => profile.id === profiles?.active)
  );
  /** Whether the timer has moved away from its active profile since that was saved. */
  const profileChanged = $derived(
    !!activeProfile && !!config && !sameConfig(activeProfile.config, config)
  );

  async function loadProfiles() {
    try {
      profiles = await session.listProfiles(timer);
    } catch (e) {
      profileError = say(e);
    }
  }
  $effect(() => {
    void loadProfiles();
  });

  /** Run one profile change, then read the list again. A failure is shown in the banner. */
  async function changeProfiles(change: () => Promise<unknown>): Promise<boolean> {
    if (profileBusy) return false;
    profileBusy = true;
    profileError = undefined;
    try {
      const done = await change();
      await loadProfiles();
      return done !== undefined;
    } catch (e) {
      profileError = say(e);
      return false;
    } finally {
      profileBusy = false;
    }
  }

  /** Write a profile to the timer. What the timer then reports replaces the draft. */
  async function applyProfile(id: string) {
    const picked = profiles?.profiles.find((profile) => profile.id === id);
    if (!picked) return;
    await changeProfiles(async () => {
      const kept = await session.applyProfile(timer, id);
      if (!kept) return undefined;
      reads += 1; // an older read must not land on top of this
      loading = false;
      config = kept;
      draft = draftFrom(kept);
      loadError = undefined;
      toast.success(`“${picked.name}” is on ${timerLabel}.`);
      return kept;
    });
  }

  /** Save what the timer has now over the active profile. */
  async function saveProfile() {
    const profile = activeProfile;
    const now = config;
    if (!profile || !now) return;
    const done = await changeProfiles(() =>
      session.updateProfile(timer, profile.id, { name: profile.name, config: now })
    );
    if (done) toast.success(`Saved “${profile.name}”.`);
  }

  async function deleteProfile() {
    const profile = activeProfile;
    if (!profile) return;
    const done = await changeProfiles(() => session.deleteProfile(timer, profile.id));
    if (done) toast.success(`Deleted “${profile.name}”. The timer is left as it is.`);
  }

  // The name dialog, for a new profile and for renaming the active one.
  let nameOpen = $state(false);
  let nameFor = $state<'new' | 'rename'>('new');
  let nameText = $state('');
  let nameError = $state<string | undefined>(undefined);

  function openNameDialog(what: 'new' | 'rename') {
    nameFor = what;
    nameText = what === 'rename' ? (activeProfile?.name ?? '') : '';
    nameError = undefined;
    nameOpen = true;
  }

  async function submitName() {
    const name = nameText.trim();
    const now = config;
    if (!name || !now || profileBusy) return;
    profileBusy = true;
    nameError = undefined;
    try {
      const saved =
        nameFor === 'rename' && activeProfile
          ? await session.updateProfile(timer, activeProfile.id, {
              name,
              config: activeProfile.config
            })
          : await session.createProfile(timer, { name, config: now });
      if (!saved) return;
      await loadProfiles();
      nameOpen = false;
      toast.success(nameFor === 'rename' ? `Renamed to “${name}”.` : `Saved “${name}”.`);
    } catch (e) {
      // The Director's sentence (a name already taken) goes under the field.
      nameError = say(e);
    } finally {
      profileBusy = false;
    }
  }

  async function discard() {
    applyError = undefined;
    if (config) draft = draftFrom(config);
    await loadConfig();
  }
</script>

<div class="timer-seats">
  {#if view}
    {#if !connected}
      <div data-testid="not-connected">
        <Banner tone="warn" title="{view.timer.name} is not connected.">
          {view.status.problem ?? 'The Director is still trying to reach it.'}
        </Banner>
      </div>
    {/if}
    {#if profileError}
      <div data-testid="profile-problem">
        <Banner
          tone="danger"
          title="The profile was not changed."
          ondismiss={() => (profileError = undefined)}>{profileError}</Banner
        >
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
      <!-- ── Profile ───────────────────────────────────────────────────── -->
      <Card elevation="sm">
        <div class="profile" data-testid="profile-bar">
          <label class="profile-pick">
            <span class="eyebrow">Profile</span>
            <Select
              aria-label="Profile"
              value={profiles?.active ?? ''}
              disabled={profileBusy || dirty || !profiles}
              title={dirty ? 'Apply or discard the changes first.' : undefined}
              onchange={async (e: Event) => {
                const select = e.currentTarget as HTMLSelectElement;
                await applyProfile(select.value);
                // A refused or cancelled change leaves the timer on the profile it had, and
                // the box must say so: the bound value did not change, so it is put back by hand.
                select.value = profiles?.active ?? '';
              }}
            >
              {#if !activeProfile}<option value="">No profile</option>{/if}
              {#each profiles?.profiles ?? [] as profile (profile.id)}
                <option value={profile.id}>{profile.name}</option>
              {/each}
            </Select>
          </label>
          <p class="profile-state" role="status" data-testid="profile-state">
            {#if !profiles}
              Reading the profiles…
            {:else if !activeProfile}
              {profiles.profiles.length === 0
                ? 'A profile keeps these settings under a name, so they can be put back later.'
                : 'The timer is not on a saved profile.'}
            {:else if profileChanged}
              <Badge tone="warn" dot>Changed</Badge>
              The timer has been changed since this profile was saved.
            {:else}
              The timer is set as this profile says.
            {/if}
          </p>
          <div class="profile-actions">
            {#if activeProfile}
              <Button
                variant="secondary"
                disabled={profileBusy || dirty || !profileChanged}
                title={dirty ? 'Apply or discard the changes first.' : undefined}
                onclick={() => void saveProfile()}>Save</Button
              >
              <Button
                variant="ghost"
                disabled={profileBusy}
                onclick={() => openNameDialog('rename')}>Rename</Button
              >
            {/if}
            <Button
              variant={activeProfile ? 'ghost' : 'secondary'}
              disabled={profileBusy || dirty || !profiles}
              title={dirty ? 'Apply or discard the changes first.' : undefined}
              onclick={() => openNameDialog('new')}>Save as new…</Button
            >
            {#if activeProfile}
              <ConfirmButton
                variant="danger"
                disabled={profileBusy}
                title="Delete the profile {activeProfile.name}"
                onconfirm={() => void deleteProfile()}>Delete</ConfirmButton
              >
            {/if}
          </div>
        </div>
      </Card>

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
                <div class="seat-id">
                  <span class="seat-name">{label}</span>
                  {#if onTimer?.enabled}
                    <a
                      class="tune"
                      href={formatHash({ kind: 'capture', timer, seat: seat.seat })}
                      aria-label="Tune {label}"
                      title="Record {label}'s signal and set its levels on the recording">Tune</a
                    >
                  {/if}
                </div>
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
                {#if seatLevels}
                  <div class="own">
                    <div class="own-choice">
                      <span>This seat's levels</span>
                      <Select
                        size="sm"
                        aria-label="{label}: this seat's levels"
                        value={seat.levels ? 'own' : 'timer'}
                        onchange={(e: Event) =>
                          setOwnLevels(
                            position,
                            (e.currentTarget as HTMLSelectElement).value === 'own'
                          )}
                      >
                        <option value="timer">Same as the timer</option>
                        <option value="own">Its own</option>
                      </Select>
                    </div>
                    {#if seat.levels}
                      <LevelsFields
                        bind:levels={seat.levels}
                        inUse={live}
                        group="levels-{position}"
                        legend="{label} enter and exit levels"
                        who={label}
                      />
                    {/if}
                  </div>
                {/if}
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

      <!-- ── Timer settings ────────────────────────────────────────────── -->
      <Card
        title="Timer settings"
        subtitle={seatLevels
          ? "When the timer counts a crossing. These levels are the timer's own. Every seat that has none of its own uses them."
          : 'When the timer counts a crossing. These settings are for every seat on this timer.'}
      >
        <LevelsFields
          bind:levels={draft.levels}
          inUse={levelsInUse([...levels.values()], config)}
          group="threshold-mode"
          legend="Enter and exit levels"
        >
          {#if draft.exitHoldMs !== undefined}
            <Field
              label="Exit hold (milliseconds)"
              hint="The signal must stay below the exit level this long before the crossing ends. A longer hold stops one pass from being split in two."
            >
              <Input
                type="number"
                inputmode="numeric"
                min="0"
                step="1"
                aria-label="Exit hold, in milliseconds"
                bind:value={draft.exitHoldMs}
              />
            </Field>
          {/if}
          {#if draft.lapCooldownS !== undefined}
            <Field
              label="Lap cooldown (seconds)"
              hint="After a crossing, the timer reports no other on that seat for this long. It swallows the bounce of one pass. Keep it shorter than the event's minimum lap time, which is set on the Event page."
            >
              <Input
                type="number"
                inputmode="decimal"
                min="0"
                step="0.1"
                aria-label="Lap cooldown, in seconds"
                bind:value={draft.lapCooldownS}
              />
            </Field>
          {/if}
        </LevelsFields>
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
                {result && !result.ok ? result.problem : 'These changes are not on the timer yet.'}
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

<Dialog
  bind:open={nameOpen}
  title={nameFor === 'rename' ? 'Rename the profile' : 'Save as a new profile'}
  onclose={() => (nameError = undefined)}
>
  <form
    aria-label={nameFor === 'rename' ? 'Rename the profile' : 'Save as a new profile'}
    onsubmit={(e) => {
      e.preventDefault();
      void submitName();
    }}
  >
    <Field
      label="Profile name"
      error={nameError}
      hint={nameFor === 'new'
        ? `Keeps the settings ${timerLabel} has now: channels, levels and timer settings.`
        : undefined}
    >
      <Input
        bind:value={nameText}
        placeholder="e.g. Garage whoops"
        aria-label="Profile name"
        autocomplete="off"
      />
    </Field>
  </form>
  {#snippet footer()}
    <Button variant="ghost" disabled={profileBusy} onclick={() => (nameOpen = false)}>Cancel</Button
    >
    <Button
      variant="primary"
      disabled={profileBusy || !nameText.trim()}
      onclick={() => void submitName()}
    >
      {nameFor === 'rename' ? 'Rename' : 'Save profile'}
    </Button>
  {/snippet}
</Dialog>

<style>
  .timer-seats {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .profile {
    display: flex;
    align-items: flex-end;
    gap: var(--gf-space-4);
    flex-wrap: wrap;
  }
  .profile-pick {
    display: grid;
    gap: var(--gf-space-1);
    min-width: 14rem;
  }
  .eyebrow {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .profile-state {
    flex: 1 1 14rem;
    margin: 0;
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .profile-actions {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
  }
  .seat-id {
    display: grid;
    gap: var(--gf-space-1);
    justify-items: start;
  }
  .tune {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-accent);
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
  .switch {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    cursor: pointer;
  }
  .switch input {
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
  /* A seat's own levels: a second line in the seat's row, the whole width. */
  .own {
    grid-column: 1 / -1;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
    padding-top: var(--gf-space-3);
    border-top: 1px solid var(--gf-border);
  }
  .own-choice {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .own-choice :global(.gf-select) {
    width: 12.5rem;
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
