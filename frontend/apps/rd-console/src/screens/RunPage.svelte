<script lang="ts">
  /**
   * RunPage — run the heats, one at a time.
   *
   * Top to bottom: which heat is on the timer (a phase, then one of its heats), the clock
   * with the one big action, a card per seat, the heat's ranking, and its earlier races.
   * With no heat named in the URL the page opens on the heat being raced, or the next one.
   * The big action follows the race:
   *
   *   Start → (countdown) Call off → (racing) Stop → Save or Discard → ready for the next
   *
   * Nothing is saved by itself: a stopped race waits until the director saves or discards
   * it. Everything shown comes from the live event store, so a second console on the same
   * heat sees the same thing. The clock runs on the Director's clock, not this browser's.
   *
   * A command the Director refuses shows its sentence in the banner at the top, as it is.
   *
   * The buttons have keys (`lib/shortcuts.ts`), and each new lap is spoken
   * (`lib/callouts.ts`); both can be switched off for a browser that only shows the page.
   */
  import { onDestroy, untrack } from 'svelte';
  import {
    Badge,
    Banner,
    Button,
    Card,
    Input,
    RaceClock,
    Select,
    formatMicros
  } from '@gridfpv/components';
  import type {
    AdapterId,
    Command,
    DiscardReason,
    PhaseId,
    PilotId,
    ProtocolError,
    RaceId,
    SeatRef
  } from '@gridfpv/types';
  import Page from '../Page.svelte';
  import { Caller, readCalloutsOn, saveCalloutsOn } from '../lib/callouts.js';
  import ErrorBanner from '../lib/ErrorBanner.svelte';
  import { toProtocolError, type EventLive } from '../lib/eventLive.svelte.js';
  import { directorEndsHeat, isRanked } from '../lib/formats.js';
  import { bestLapRun, bestRunInHeat, consecutiveLaps } from '../lib/overlay.js';
  import {
    currentRace,
    defaultHeat,
    groupOfHeat,
    heatGroups,
    isBye,
    isHeat,
    racesOfHeat
  } from '../lib/phases.js';
  import { formatHash } from '../lib/route.js';
  import { isBench } from '../lib/seats.js';
  import {
    DISCARD_ARM_MS,
    RUN_KEYS,
    runShortcut,
    shortcutFree,
    type RunCaps
  } from '../lib/shortcuts.js';
  import {
    finishOrder,
    lineupSeatRows,
    raceSeatRows,
    raceStateLabel,
    readClock,
    type SeatRow
  } from '../lib/run.js';
  import { StartCue, unlockAudio, type CuePhase } from '../lib/startTone.js';
  import DiscardDialog from './DiscardDialog.svelte';
  import RunSeatCard from './RunSeatCard.svelte';

  let {
    live,
    heat: named,
    onrun,
    ontune
  }: {
    live: EventLive;
    /**
     * The heat to run, when the URL names one. A wire handle; its label is what is shown.
     * With none named the page runs the heat being raced, or the next one.
     */
    heat?: PhaseId;
    /** Put another heat on the timer. */
    onrun: (heat: PhaseId) => void;
    /** Open a seat's Tune page. */
    ontune?: (timer: AdapterId, seat: SeatRef) => void;
  } = $props();

  const names = $derived(live.names);
  const view = $derived(live.view);
  /** The heat on this page: the one named, or the one the Director would race next. */
  const heatId = $derived(named ?? (view ? defaultHeat(view) : undefined));
  const phase = $derived(view?.phases.find((p) => p.def.id === heatId && isHeat(p.def)));
  const races = $derived(view && heatId !== undefined ? racesOfHeat(view, heatId) : []);

  // ── On the timer: a phase, then one of its heats ───────────────────────────
  const groups = $derived(view ? heatGroups(view.phases) : []);
  const group = $derived(heatId === undefined ? undefined : groupOfHeat(groups, heatId));
  /** Moving to another phase puts its first heat that can still be raced on the timer. */
  function pickGroup(key: string) {
    const next = groups.find((g) => g.key === key);
    if (!next || next.key === group?.key) return;
    const open = next.heats.find((h) => !h.complete && !h.locked) ?? next.heats[0];
    onrun(open.def.id);
  }
  /** What a heat is called in the picker: its name, and where it stands when that matters. */
  function heatOption(heat: (typeof groups)[number]['heats'][number]): string {
    const flown = view ? racesOfHeat(view, heat.def.id).filter((r) => r.state === 'Saved') : [];
    if (heat.locked) return `${heat.def.label} (locked)`;
    if (heat.complete) return `${heat.def.label} (complete)`;
    if (heat.waiting) return `${heat.def.label} (waiting)`;
    if (flown.length > 0) {
      return `${heat.def.label} (${flown.length} ${flown.length === 1 ? 'race' : 'races'})`;
    }
    return heat.def.label;
  }
  /** The one race in the event that is not saved or discarded yet, if any. */
  const undecided = $derived(view ? currentRace(view) : undefined);
  /** That race, when it belongs to this heat. */
  const race = $derived(undecided?.heat === heatId ? undecided : undefined);
  /** That race, when it belongs to another heat: nothing can start here until it is decided. */
  const elsewhere = $derived(undecided && undecided.heat !== heatId ? undecided : undefined);
  const decided = $derived(races.filter((r) => r.state === 'Saved' || r.state === 'Discarded'));
  const savedCount = $derived(races.filter((r) => r.state === 'Saved').length);

  /** The number people call a race by: its place among the heat's races. Never its id. */
  const raceNumber = (id: string): number => races.findIndex((r) => r.race === id) + 1;

  const canStart = $derived(
    !!phase && !undecided && !phase.locked && !phase.waiting && !phase.complete
  );
  /** The phases this heat's seats are still waiting on, by name. */
  const waitingOn = $derived([
    ...new Set(
      (phase?.lineup ?? []).flatMap((seat) =>
        typeof seat.who === 'object' && 'Waiting' in seat.who
          ? [names.phase(seat.who.Waiting.phase)]
          : []
      )
    )
  ]);

  /**
   * Whether the director may end the heat by hand right now, and what the button says. A
   * format where the director decides always allows it. A bye (one pilot, nobody to race)
   * allows it before any race, so the pilot moves on without flying.
   */
  const endHeat = $derived.by(() => {
    if (!phase || undecided || phase.locked || phase.waiting || phase.complete) return undefined;
    if (isBye(phase) && savedCount === 0) return 'Advance without flying';
    if (directorEndsHeat(phase.format)) return 'Call heat done';
    return undefined;
  });

  const rows: SeatRow[] = $derived(
    race ? raceSeatRows(race, names) : phase ? lineupSeatRows(phase, names) : []
  );
  /**
   * What times this heat, by name, and what is wrong with it when something is. A heat is
   * normally on one timer; one put together by hand could be on more, so each is listed.
   */
  const timedBy = $derived(
    [...new Set(rows.map((row) => row.adapter))].map((adapter) => {
      const name = names.timer(adapter);
      if (isBench(adapter)) return { adapter, name };
      const found = live.timers.find((t) => t.timer.id === adapter);
      // Before the first read of the timer list nothing is known, so nothing is claimed.
      if (!live.timersLoaded) return { adapter, name };
      if (!found) {
        return {
          adapter,
          name,
          trouble: 'The timer for this heat was removed.',
          detail: 'Its seats cannot be timed. Add the heat again on a timer that is in the list.'
        };
      }
      if (found.status.connected) return { adapter, name };
      return {
        adapter,
        name,
        trouble: `${name} is not connected.`,
        detail: found.status.problem ?? 'The Director has not reached it yet.'
      };
    })
  );
  /** A finished heat needs no timer, so it carries no warning. */
  const timerTrouble = $derived(
    phase && !phase.complete && !phase.locked ? timedBy.filter((t) => t.trouble !== undefined) : []
  );
  const showRanking = $derived(!!phase && isRanked(phase.format) && savedCount > 0);

  // ── Best N laps in a row ───────────────────────────────────────────────────
  /** The N of a heat ranked on its best N laps in a row, else `undefined`. */
  const runLength = $derived(consecutiveLaps(phase?.format));
  /** Each seat's best run in the race on screen, in row order. */
  const runs = $derived(
    race && runLength !== undefined
      ? race.seats.map((seat) => bestLapRun(seat.laps, runLength))
      : []
  );
  /** A pilot's best run in this heat so far: its saved races and the one on screen. */
  const heatBest = (pilot: PilotId | undefined): number | undefined =>
    view && heatId !== undefined && runLength !== undefined && pilot !== undefined
      ? bestRunInHeat(view, heatId, pilot, runLength, race)
      : undefined;

  // ── The clock ──────────────────────────────────────────────────────────────
  // `now` is the Director's clock. It only ticks while something is moving.
  let now = $state(untrack(() => live.nowMicros()));
  const moving = $derived(race?.state === 'Staging' || race?.state === 'Running');
  $effect(() => {
    now = live.nowMicros();
    if (!moving) return;
    const timer = setInterval(() => (now = live.nowMicros()), 50);
    return () => clearInterval(timer);
  });
  const clock = $derived(readClock(race, now));

  // ── The start tone ─────────────────────────────────────────────────────────
  const cue = new StartCue();
  const cuePhase: CuePhase = $derived(
    race?.state === 'Staging' ? 'counting' : race ? 'started' : 'off'
  );
  const toneAt = $derived(race?.state === 'Staging' ? race.tone_at : undefined);
  $effect(() => {
    // Only the race's state and its tone time re-run this, never the ticking clock.
    cue.update(
      cuePhase,
      toneAt,
      untrack(() => live.nowMicros())
    );
  });
  onDestroy(() => cue.dispose());

  // ── Actions ────────────────────────────────────────────────────────────────
  let busy = $state(false);
  let error = $state<ProtocolError | undefined>(undefined);
  /** The race the discard dialog is open for: the stopped one, or a saved one from the list. */
  let discardTarget = $state<RaceId | undefined>(undefined);
  const discarding = $derived(races.find((r) => r.race === discardTarget));
  /** The save options. One click on Save still saves; these only add to it. */
  let needsLook = $state(false);
  let saveNote = $state('');

  function save() {
    if (!race) return;
    const note = saveNote.trim();
    void act({
      SaveRace: { race: race.race, needs_marshal: needsLook, ...(note ? { note } : {}) }
    }).then(() => {
      // A refused save keeps what was typed.
      if (error) return;
      needsLook = false;
      saveNote = '';
    });
  }

  /** Send one command. A refusal lands in the banner in the Director's words. */
  async function act(command: Command) {
    if (busy) return;
    busy = true;
    error = undefined;
    try {
      await live.send(command);
    } catch (e) {
      error = toProtocolError(e);
    } finally {
      busy = false;
    }
  }

  function start() {
    if (heatId === undefined) return;
    // The click is what lets the browser play sound.
    unlockAudio();
    void act({ StartRace: { heat: heatId } });
  }

  // ── Lap callouts ───────────────────────────────────────────────────────────
  const caller = new Caller();
  let calloutsOn = $state(readCalloutsOn(globalThis.localStorage));
  $effect(() => {
    // A pilot's phonetic spelling, where they have one, is how their callsign is said.
    const pilots = live.pilots;
    caller.update(
      race?.state === 'Running' ? race.race : undefined,
      rows,
      calloutsOn,
      (row) => pilots.find((pilot) => pilot.id === row.pilot)?.phonetic
    );
  });
  onDestroy(() => caller.dispose());
  function setCallouts(on: boolean) {
    calloutsOn = on;
    saveCalloutsOn(globalThis.localStorage, on);
  }

  // ── Keyboard ───────────────────────────────────────────────────────────────
  let legendOpen = $state(false);
  /** Set by a first Discard press; a second press before it runs out discards. */
  let discardArmed = $state(false);
  let disarm: ReturnType<typeof setTimeout> | undefined;
  onDestroy(() => clearTimeout(disarm));
  const caps: RunCaps = $derived({
    start: !race && canStart && !busy,
    stop: !busy && (race?.state === 'Running' || race?.state === 'Staging'),
    save: !busy && race?.state === 'Stopped',
    discard: !busy && race?.state === 'Stopped'
  });
  function onkeydown(e: KeyboardEvent) {
    if (!shortcutFree(e, document)) return;
    const action = runShortcut(e.key, caps);
    if (action === null) return;
    // Space would otherwise scroll the page or press the focused button a second time.
    e.preventDefault();
    if (action !== 'discard') {
      discardArmed = false;
      clearTimeout(disarm);
    }
    if (action === 'legend') legendOpen = !legendOpen;
    else if (action === 'start') start();
    else if (action === 'save') save();
    else if (action === 'stop' && race) {
      void act(
        race.state === 'Staging'
          ? { DiscardRace: { race: race.race } }
          : { StopRace: { race: race.race } }
      );
    } else if (action === 'discard' && race) {
      if (discardArmed) {
        discardArmed = false;
        clearTimeout(disarm);
        void act({ DiscardRace: { race: race.race } });
      } else {
        discardArmed = true;
        clearTimeout(disarm);
        disarm = setTimeout(() => (discardArmed = false), DISCARD_ARM_MS);
      }
    }
  }

  async function discard(reason: DiscardReason | undefined, falseStartBy?: PilotId) {
    const target = discarding;
    if (!target) return;
    // The dialog shows a failure itself, so these are not caught here.
    if (falseStartBy !== undefined) {
      const recorded = await live.send({
        Record: { FalseStart: { race: target.race, pilot: falseStartBy } }
      });
      if (!recorded) return;
    }
    const done = await live.send({
      DiscardRace: reason === undefined ? { race: target.race } : { race: target.race, reason }
    });
    if (done) discardTarget = undefined;
  }

  /** The bench tool: log a crossing on a seat, stamped with this browser's clock. */
  async function crossing(row: SeatRow) {
    try {
      await live.postPass({ adapter: row.adapter, seat: row.seat, at: Date.now() * 1000 });
    } catch (e) {
      error = toProtocolError(e);
    }
  }

  const STATE_TONES = {
    Staging: 'warn',
    Running: 'success',
    Stopped: 'info',
    Saved: 'success',
    Discarded: 'neutral'
  } as const;
</script>

<svelte:window {onkeydown} />

<Page
  title="Run"
  help="run"
  lede="Race day in one screen: put a heat on the timer, start it, stop it, then save or discard."
>
  <div class="page-inner">
    <ErrorBanner {error} ondismiss={() => (error = undefined)} />
    {#if live.loadError}
      <ErrorBanner error={{ code: 'Internal', message: live.loadError }} />
    {/if}

    {#each timerTrouble as t (t.adapter)}
      <Banner tone="warn" title={t.trouble}>{t.detail}</Banner>
    {/each}

    {#if !view}
      {#if !live.loadError}<p class="muted" role="status">Loading…</p>{/if}
    {:else if groups.length === 0}
      <Card elevation="sm">
        <p class="muted" data-testid="no-heats">
          There is no heat to run yet.
          <a href={formatHash({ kind: 'event', section: 'phases' })}>Add one on the Phases page.</a>
        </p>
      </Card>
    {:else if !phase}
      <Card elevation="sm">
        <p class="muted">This heat is not in the event.</p>
        <Button onclick={() => view && onrun(defaultHeat(view) ?? groups[0].heats[0].def.id)}>
          Go to the next heat
        </Button>
      </Card>
    {:else}
      <Card elevation="sm">
        <div class="timerhead">
          <div class="what">
            <div class="on-timer">
              <span class="eyebrow">On the timer</span>
              <div class="pickers">
                <Select
                  aria-label="Phase"
                  value={group?.key ?? ''}
                  disabled={!!race}
                  onchange={(e: Event) => pickGroup((e.currentTarget as HTMLSelectElement).value)}
                >
                  {#each groups as g (g.key)}
                    <option value={g.key}>{g.label}</option>
                  {/each}
                </Select>
                <Select
                  aria-label="Heat"
                  value={heatId}
                  disabled={!!race}
                  onchange={(e: Event) => onrun((e.currentTarget as HTMLSelectElement).value)}
                >
                  {#each group?.heats ?? [] as h (h.def.id)}
                    <option value={h.def.id}>{heatOption(h)}</option>
                  {/each}
                </Select>
              </div>
              <p class="rules" data-testid="run-rules">
                {#if phase.format}
                  <a href={formatHash({ kind: 'page', page: 'formats' })}>{phase.format.name}</a>
                {:else}
                  No format
                {/if}
                {#if timedBy.length > 0}
                  <span aria-hidden="true">·</span>
                  <span data-testid="timed-by">
                    Timed by {timedBy.map((t) => t.name).join(', ')}
                  </span>
                {/if}
              </p>
            </div>
            <span class="eyebrow">
              {#if race}
                Race {raceNumber(race.race)}
              {:else if phase.complete || phase.locked}
                Heat
              {:else}
                Next: race {races.length + 1}
              {/if}
            </span>
            <p class="status" role="status" data-testid="run-status">
              {#if race?.state === 'Staging'}
                Counting down to the start.
              {:else if race?.state === 'Running'}
                Racing.
              {:else if race?.state === 'Stopped'}
                Stopped. Save the race or discard it.
              {:else if phase.locked}
                This heat is locked. Its result is official.
              {:else if phase.complete}
                This heat is complete.
              {:else if phase.waiting}
                {#if waitingOn.length > 0}
                  Waiting on {waitingOn.join(', ')}.
                {:else}
                  Waiting on other heats to finish.
                {/if}
              {:else if elsewhere}
                {names.phase(elsewhere.heat)} has a race that is not saved or discarded yet.
              {:else if races.length > 0}
                Ready for the next race.
              {:else}
                Ready to start.
              {/if}
            </p>
            {#if elsewhere && !race}
              <Button size="sm" onclick={() => onrun(elsewhere.heat)}>
                Go to {names.phase(elsewhere.heat)}
              </Button>
            {/if}
            {#if endHeat}
              <Button
                size="sm"
                variant="secondary"
                disabled={busy}
                onclick={() => act({ Record: { HeatDone: { heat: phase.def.id } } })}
                >{endHeat}</Button
              >
            {/if}
          </div>

          <div class="clockcol">
            <span class="eyebrow">{clock.label}</span>
            <div class="clock">
              <RaceClock
                elapsedMs={clock.elapsedMs}
                remainingMs={clock.remainingMs}
                label={clock.label}
              />
            </div>
            {#if race}
              <Badge tone={STATE_TONES[race.state]} dot={race.state === 'Running'}>
                {raceStateLabel(race)}
              </Badge>
            {/if}
            <div class="controls">
              {#if !race}
                <!-- A finished heat has no next race, so it offers no Start at all. -->
                {#if !phase.complete && !phase.locked}
                  <Button variant="primary" size="lg" disabled={!canStart || busy} onclick={start}>
                    Start
                  </Button>
                {/if}
              {:else if race.state === 'Staging'}
                <Button
                  variant="danger"
                  size="lg"
                  disabled={busy}
                  onclick={() => act({ DiscardRace: { race: race.race } })}>Call off</Button
                >
              {:else if race.state === 'Running'}
                <Button
                  variant="danger"
                  size="lg"
                  disabled={busy}
                  onclick={() => act({ StopRace: { race: race.race } })}>Stop</Button
                >
              {:else}
                <Button variant="primary" size="lg" disabled={busy} onclick={save}>Save</Button>
                <Button
                  variant="ghost"
                  size="lg"
                  disabled={busy}
                  onclick={() => (discardTarget = race.race)}
                  >{discardArmed ? 'Discard (d again)' : 'Discard'}</Button
                >
              {/if}
            </div>
            {#if race?.state === 'Stopped'}
              <!-- Optional. Save works without opening this. -->
              <details class="save-options">
                <summary>Save options</summary>
                <label class="look">
                  <input type="checkbox" bind:checked={needsLook} disabled={busy} />
                  Needs a look
                </label>
                <p class="muted">Puts the race on the Marshal page's to-do list.</p>
                <Input
                  bind:value={saveNote}
                  placeholder="Note (optional)"
                  aria-label="Note for this race"
                  autocomplete="off"
                  disabled={busy}
                />
              </details>
            {/if}
          </div>
        </div>
      </Card>

      <div class="options">
        <label class="option">
          <input
            type="checkbox"
            checked={calloutsOn}
            onchange={(e) => setCallouts(e.currentTarget.checked)}
          />
          Call out laps
        </label>
        <button
          type="button"
          class="linkish"
          aria-expanded={legendOpen}
          onclick={() => (legendOpen = !legendOpen)}>Keyboard shortcuts</button
        >
      </div>
      {#if legendOpen}
        <dl class="legend" data-testid="run-keys">
          {#each RUN_KEYS as entry (entry.does)}
            <div>
              <dt>
                {#each entry.keys as key, index (index)}<kbd>{key}</kbd>{/each}
              </dt>
              <dd>{entry.does}</dd>
            </div>
          {/each}
        </dl>
      {/if}

      <ul class="seats" aria-label="Seats">
        {#each rows as row, index (`${row.adapter}/${row.seat}`)}
          <RunSeatCard
            {row}
            racing={race?.state === 'Running'}
            {runLength}
            run={runs[index]}
            heatBestMicros={heatBest(row.pilot)}
            tuneHref={ontune && !row.bench
              ? formatHash({ kind: 'capture', timer: row.adapter, seat: row.seat })
              : undefined}
            oncrossing={() => crossing(row)}
          />
        {/each}
      </ul>

      {#if showRanking}
        <Card title="Ranking" elevation="sm">
          <ol class="ranking" aria-label="Heat ranking">
            {#each phase.ranking as placed (placed.pilot)}
              <li>
                <span class="place">{placed.position}</span>
                <span class="who">{names.pilot(placed.pilot)}</span>
                {#if runLength !== undefined}
                  <span class="num strong" data-stat="rank-run">
                    best {runLength}
                    {formatMicros(heatBest(placed.pilot))}
                  </span>
                {/if}
                <span class="num">{placed.laps} {placed.laps === 1 ? 'lap' : 'laps'}</span>
                <span class="num">best lap {formatMicros(placed.best_lap_micros)}</span>
                {#if placed.disqualified}<Badge tone="danger">Disqualified</Badge>{/if}
                {#if !placed.flew}<Badge tone="neutral">Did not fly</Badge>{/if}
              </li>
            {/each}
          </ol>
        </Card>
      {/if}

      {#if decided.length > 0}
        <Card title="Earlier races" elevation="sm">
          <ol class="earlier" aria-label="Earlier races">
            {#each decided as past (past.race)}
              <li>
                <span class="race-name">Race {raceNumber(past.race)}</span>
                <Badge tone={STATE_TONES[past.state]}>{raceStateLabel(past)}</Badge>
                <span class="result">
                  {#each finishOrder(past) as seat, index (index)}
                    {@const laps = seat.laps.filter((lap) => !lap.restart).length}
                    <span class="finisher">
                      {seat.finish}. {seat.pilot === undefined
                        ? names.who('Open')
                        : names.pilot(seat.pilot)}
                      <span class="muted">({laps} {laps === 1 ? 'lap' : 'laps'})</span>
                    </span>
                  {/each}
                </span>
                {#if past.needs_marshal && past.state === 'Saved'}
                  <Badge tone="warn">Needs a look</Badge>
                {/if}
                {#if past.note}<span class="muted">{past.note}</span>{/if}
                {#if past.state === 'Saved' && !past.locked}
                  <span class="past-action">
                    <a
                      class="fix"
                      href={formatHash({ kind: 'marshal', race: past.race })}
                      aria-label="Fix the laps of race {raceNumber(past.race)}">Fix the laps</a
                    >
                    <Button
                      size="sm"
                      variant="ghost"
                      disabled={busy}
                      aria-label="Discard race {raceNumber(past.race)}"
                      onclick={() => (discardTarget = past.race)}>Discard</Button
                    >
                  </span>
                {/if}
              </li>
            {/each}
          </ol>
        </Card>
      {/if}
    {/if}
  </div>
</Page>

{#if discarding}
  <DiscardDialog
    pilots={discarding.seats.flatMap((seat) =>
      seat.pilot === undefined ? [] : [{ id: seat.pilot, callsign: names.pilot(seat.pilot) }]
    )}
    ondiscard={discard}
    onclose={() => (discardTarget = undefined)}
  />
{/if}

<style>
  .page-inner {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .on-timer {
    display: grid;
    gap: var(--gf-space-2);
    width: 100%;
    margin-bottom: var(--gf-space-3);
  }
  .pickers {
    display: grid;
    gap: var(--gf-space-2);
  }
  .rules {
    margin: 0;
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-2);
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .rules a,
  .muted a,
  .fix {
    color: var(--gf-accent);
  }
  .options {
    display: flex;
    align-items: center;
    gap: var(--gf-space-5);
    flex-wrap: wrap;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-secondary);
  }
  .option {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
  }
  .linkish {
    padding: 0;
    border: none;
    background: none;
    font: inherit;
    color: var(--gf-accent);
    cursor: pointer;
  }
  .linkish:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
  .legend {
    margin: 0;
    display: grid;
    gap: var(--gf-space-1);
    font-size: var(--gf-font-size-sm);
  }
  .legend div {
    display: grid;
    grid-template-columns: 6rem 1fr;
    gap: var(--gf-space-3);
    align-items: baseline;
  }
  .legend dd {
    margin: 0;
    color: var(--gf-text-secondary);
  }
  kbd {
    display: inline-block;
    margin-right: var(--gf-space-1);
    padding: 0 0.4em;
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-xs);
    background: var(--gf-surface-sunken);
    font-family: var(--gf-font-mono);
    font-size: var(--gf-font-size-xs);
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .muted {
    margin: 0;
  }
  .timerhead {
    display: grid;
    grid-template-columns: minmax(14rem, 1fr) 2fr;
    gap: var(--gf-space-6);
    align-items: center;
  }
  .what {
    display: grid;
    gap: var(--gf-space-2);
    justify-items: start;
  }
  .eyebrow {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .status {
    margin: 0;
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-semibold);
  }
  .clockcol {
    display: grid;
    justify-items: center;
    gap: var(--gf-space-2);
  }
  /* The clock is read from across the pits, so it is much bigger here than its default. */
  .clock :global(.gridfpv-race-clock) {
    font-size: clamp(3rem, 9vw, 5.5rem);
  }
  .controls {
    display: flex;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
    justify-content: center;
    margin-top: var(--gf-space-2);
  }
  .controls :global(.gf-btn) {
    min-width: 9rem;
  }
  .seats {
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(17rem, 1fr));
    gap: var(--gf-space-3);
  }
  .ranking,
  .earlier {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .ranking li,
  .earlier li {
    display: flex;
    align-items: baseline;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
  }
  .place {
    min-width: 1.5rem;
    font-family: var(--gf-font-mono);
    font-weight: var(--gf-font-weight-bold);
    color: var(--gf-text-muted);
  }
  .ranking .who,
  .race-name {
    font-weight: var(--gf-font-weight-semibold);
  }
  .num {
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-secondary);
  }
  .num.strong {
    color: var(--gf-text);
    font-weight: var(--gf-font-weight-bold);
  }
  .result {
    display: flex;
    gap: var(--gf-space-1) var(--gf-space-4);
    flex-wrap: wrap;
    font-size: var(--gf-font-size-sm);
  }
  .past-action {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-3);
    font-size: var(--gf-font-size-sm);
  }
  .save-options {
    width: min(22rem, 100%);
    font-size: var(--gf-font-size-sm);
  }
  .save-options summary {
    cursor: pointer;
    color: var(--gf-text-secondary);
    text-align: center;
  }
  .save-options[open] {
    display: grid;
    gap: var(--gf-space-2);
  }
  .look {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    font-weight: var(--gf-font-weight-semibold);
  }
  @media (max-width: 48rem) {
    .timerhead {
      grid-template-columns: 1fr;
    }
    .what {
      justify-items: center;
      text-align: center;
    }
  }
</style>
