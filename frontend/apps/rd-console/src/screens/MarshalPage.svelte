<script lang="ts">
  /**
   * MarshalPage — look at races again and make rulings.
   *
   * On the left, the **to-do list**: saved races flagged "needs a look", races with an open
   * protest, and stopped races that are not saved or discarded yet, each with why it is
   * listed. Under it, the other races by heat, newest first. That list shows one phase at a
   * time, the one being raced unless another is picked, and leaves discarded races out
   * unless they are asked for, so the race just flown is never far to find.
   *
   * With no race named in the URL the page opens on the first race that needs a look, or
   * else on the newest race.
   *
   * On the right, the **race review** for the chosen race: each seat with its trace and laps
   * (`MarshalSeat`), the heat's ranking as the Director works it out after each ruling, and
   * the race's protests.
   *
   * A ruling is always a person's decision. Each one is sent to the Director as a fact, and
   * what is shown afterwards is the Director's new snapshot. A refusal is shown in the
   * Director's own words. A race under a locked phase is shown read-only, with the name of
   * the phase to unlock.
   *
   * A race is always named by its heat and its number in that heat. Its id is only in the URL.
   */
  import { Badge, Banner, Button, Card, Input, Select, formatMicros } from '@gridfpv/components';
  import type { Command, PhaseId, ProtocolError, RaceId, RaceSignal } from '@gridfpv/types';
  import Page from '../Page.svelte';
  import ErrorBanner from '../lib/ErrorBanner.svelte';
  import { toProtocolError, type EventLive } from '../lib/eventLive.svelte.js';
  import {
    canRule,
    defaultRace,
    lockedBy,
    otherRaces,
    PROTEST_OUTCOMES,
    protestStateLabel,
    raceName,
    signalForSeat,
    todoList
  } from '../lib/marshal.js';
  import { bestRunInHeat, consecutiveLaps } from '../lib/overlay.js';
  import { defaultHeat, groupOfHeat, heatGroups } from '../lib/phases.js';
  import { raceStateLabel } from '../lib/run.js';
  import MarshalSeat from './MarshalSeat.svelte';

  let {
    live,
    race: named = undefined,
    onopen,
    onrun
  }: {
    live: EventLive;
    /**
     * The race under review, when the URL names one. A wire handle; its name is what is
     * shown. With none named the page opens on the race that most needs a look.
     */
    race?: RaceId | undefined;
    /** Open a race for review. */
    onopen: (race: RaceId) => void;
    /** Open a heat's Run view. */
    onrun: (heat: PhaseId) => void;
  } = $props();

  const names = $derived(live.names);
  const view = $derived(live.view);
  const todo = $derived(view ? todoList(view, names) : []);
  /** The race on the right: the one named, or the one that most needs a look. */
  const raceId = $derived(named ?? (view ? defaultRace(view, names) : undefined));
  const race = $derived(view?.races.find((r) => r.race === raceId));

  // ── Which races the list shows ─────────────────────────────────────────────
  const ALL_PHASES = '*';
  const groups = $derived(view ? heatGroups(view.phases) : []);
  /** The director's pick. Until they pick, the list follows the phase being raced. */
  let pickedPhase = $state<string | undefined>(undefined);
  let showDiscarded = $state(false);
  const currentPhase = $derived.by(() => {
    if (!view) return ALL_PHASES;
    const heat = race?.heat ?? defaultHeat(view);
    return (heat === undefined ? undefined : groupOfHeat(groups, heat)?.key) ?? ALL_PHASES;
  });
  const phaseFilter = $derived(pickedPhase ?? currentPhase);
  const others = $derived.by(() => {
    if (!view) return [];
    const group = groups.find((g) => g.key === phaseFilter);
    return otherRaces(view, names, {
      heats: group ? new Set(group.heats.map((h) => h.def.id)) : undefined,
      discarded: showDiscarded
    });
  });
  const heat = $derived(view?.phases.find((p) => p.def.id === race?.heat));
  /** The N of a heat ranked on its best N laps in a row, else `undefined`. */
  const runLength = $derived(consecutiveLaps(heat?.format));
  const title = $derived(view && race ? raceName(view, names, race) : undefined);
  const editable = $derived(!!race && canRule(race));
  const lockPhase = $derived(view && race?.locked ? lockedBy(view.phases, race.heat) : undefined);
  const protests = $derived(view?.protests.filter((p) => p.race === raceId) ?? []);

  // ── The race's signal: read when the race is opened, never kept in the live store ──
  let signal = $state.raw<RaceSignal | undefined>(undefined);
  let signalState = $state<'loading' | 'ready' | 'failed'>('loading');
  /** Whether the chosen race is in the snapshot yet. The signal is read once it is. */
  const raceKnown = $derived(race !== undefined);
  $effect(() => {
    const id = raceId;
    signal = undefined;
    if (id === undefined || !raceKnown) return;
    signalState = 'loading';
    let dropped = false;
    live.raceSignal(id).then(
      (read) => {
        if (dropped) return;
        signal = read;
        signalState = 'ready';
      },
      () => {
        if (!dropped) signalState = 'failed';
      }
    );
    return () => {
      dropped = true;
    };
  });

  // ── Rulings ────────────────────────────────────────────────────────────────
  let busy = $state(false);
  let error = $state<ProtocolError | undefined>(undefined);

  /** Send one ruling. A refusal lands in the banner in the Director's words. */
  async function act(command: Command): Promise<boolean> {
    if (busy) return false;
    busy = true;
    error = undefined;
    try {
      return await live.send(command);
    } catch (e) {
      error = toProtocolError(e);
      return false;
    } finally {
      busy = false;
    }
  }

  // ── Protests ───────────────────────────────────────────────────────────────
  /** "" means nobody in particular. */
  let protestBy = $state('');
  let protestNote = $state('');
  /** The note typed for each open protest's outcome, by protest. */
  let outcomeNotes = $state<Record<string, string>>({});

  async function fileProtest(e: Event) {
    e.preventDefault();
    if (!race) return;
    const note = protestNote.trim();
    const done = await act({
      FileProtest: {
        race: race.race,
        ...(protestBy ? { by: protestBy } : {}),
        ...(note ? { note } : {})
      }
    });
    if (done) {
      protestBy = '';
      protestNote = '';
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

<Page
  title="Marshal"
  help="marshal"
  lede="Look at a race again, fix its laps, and settle protests."
>
  <div class="page-inner">
    <ErrorBanner {error} ondismiss={() => (error = undefined)} />
    {#if live.loadError}
      <ErrorBanner error={{ code: 'Internal', message: live.loadError }} />
    {/if}

    {#if !view}
      {#if !live.loadError}<p class="muted" role="status">Loading…</p>{/if}
    {:else}
      <div class="columns">
        <nav class="lists" aria-label="Races">
          <Card title="To do" elevation="sm">
            {#if todo.length === 0}
              <p class="muted">Nothing needs a look.</p>
            {:else}
              <ul class="todo" aria-label="Races that need a look">
                {#each todo as item (item.race)}
                  <li>
                    <button
                      type="button"
                      class="race-link"
                      aria-current={item.race === raceId ? 'true' : undefined}
                      onclick={() => onopen(item.race)}
                    >
                      <span class="race-name">{item.name}</span>
                      {#each item.reasons as reason (reason)}
                        <span class="reason">{reason}</span>
                      {/each}
                      {#if item.note}<span class="note">“{item.note}”</span>{/if}
                    </button>
                  </li>
                {/each}
              </ul>
            {/if}
          </Card>

          <Card title="Races" elevation="sm">
            <div class="filter">
              {#if groups.length > 1}
                <Select
                  size="sm"
                  aria-label="Show the races of"
                  value={phaseFilter}
                  onchange={(e: Event) =>
                    (pickedPhase = (e.currentTarget as HTMLSelectElement).value)}
                >
                  <option value={ALL_PHASES}>All phases</option>
                  {#each groups as g (g.key)}
                    <option value={g.key}>{g.label}</option>
                  {/each}
                </Select>
              {/if}
              <label class="option">
                <input type="checkbox" bind:checked={showDiscarded} />
                Show discarded races
              </label>
            </div>
            {#if others.length === 0}
              <p class="muted">
                {#if view.races.length === 0}
                  No race has been flown yet.
                {:else if phaseFilter !== ALL_PHASES && groups.length > 1}
                  No other race in this phase.
                {:else}
                  No other race to show.
                {/if}
              </p>
            {:else}
              {#each others as group (group.heat)}
                <section class="heat-group" aria-label={group.label}>
                  <h3>{group.label}</h3>
                  <ul>
                    {#each group.races as entry (entry.race)}
                      <li>
                        <button
                          type="button"
                          class="race-link row"
                          aria-current={entry.race === raceId ? 'true' : undefined}
                          aria-label="{group.label} · {entry.name.toLowerCase()}, {entry.state}"
                          onclick={() => onopen(entry.race)}
                        >
                          <span class="race-name">{entry.name}</span>
                          <span class="reason">{entry.state}</span>
                        </button>
                      </li>
                    {/each}
                  </ul>
                </section>
              {/each}
            {/if}
          </Card>
        </nav>

        <section class="review" aria-label="Race review">
          {#if raceId === undefined}
            <Card elevation="sm">
              <p class="muted">No race has been flown yet. There is nothing to look at.</p>
            </Card>
          {:else if !race || !title}
            <Card elevation="sm">
              <p class="muted">This race is not in the event.</p>
            </Card>
          {:else}
            <Card elevation="sm">
              <div class="race-head">
                <h2 data-testid="review-title">{title}</h2>
                <Badge tone={STATE_TONES[race.state]}>{raceStateLabel(race)}</Badge>
                {#if race.state === 'Saved' && race.needs_marshal}
                  <Badge tone="warn">Needs a look</Badge>
                {/if}
                {#if race.locked}<Badge tone="info">Locked</Badge>{/if}
                <span class="head-actions">
                  {#if race.state === 'Saved' && !race.locked}
                    {#if race.needs_marshal}
                      <Button
                        size="sm"
                        variant="primary"
                        disabled={busy}
                        onclick={() =>
                          act({
                            Record: { MarshalFlagSet: { race: race.race, needs_marshal: false } }
                          })}>Looked at</Button
                      >
                    {:else}
                      <Button
                        size="sm"
                        variant="ghost"
                        disabled={busy}
                        onclick={() =>
                          act({
                            Record: { MarshalFlagSet: { race: race.race, needs_marshal: true } }
                          })}>Needs a look</Button
                      >
                    {/if}
                  {/if}
                  <Button size="sm" variant="ghost" onclick={() => onrun(race.heat)}>
                    Run this heat
                  </Button>
                </span>
              </div>
              {#if race.note}<p class="race-note">Note: {race.note}</p>{/if}
              <p class="muted" data-testid="review-status">
                {#if race.locked}
                  {lockPhase === undefined ? 'A phase above this race' : names.phase(lockPhase)} is locked,
                  so this race is official and cannot be changed. Unlock it on the Phases page to make
                  a ruling.
                {:else if race.state === 'Discarded'}
                  This race was discarded. Its laps do not count, so there is nothing to rule on.
                {:else if race.state === 'Staging' || race.state === 'Running'}
                  This race is still going. Rulings can be made once it is stopped.
                {:else if race.state === 'Stopped'}
                  This race is stopped and waits to be saved or discarded. Do that on the heat's Run
                  page.
                {:else}
                  Removing a lap joins its time onto the lap after it. Marking a restart gives that
                  lap no time and leaves the laps either side as they were.
                {/if}
              </p>
            </Card>

            <ul class="seats" aria-label="Seats">
              {#each race.seats as seat, index (`${seat.adapter}/${seat.seat}`)}
                <MarshalSeat
                  {race}
                  {seat}
                  {index}
                  {names}
                  signal={signalForSeat(signal?.seats, seat)}
                  {signalState}
                  {editable}
                  {busy}
                  {act}
                />
              {/each}
            </ul>

            {#if heat && heat.ranking.length > 0}
              <Card title="Ranking of {heat.def.label}" elevation="sm">
                <ol class="ranking" aria-label="Heat ranking">
                  {#each heat.ranking as placed (placed.pilot)}
                    <li>
                      <span class="place">{placed.position}</span>
                      <span class="who">{names.pilot(placed.pilot)}</span>
                      {#if runLength !== undefined && view}
                        <span class="num strong" data-stat="rank-run">
                          best {runLength}
                          {formatMicros(bestRunInHeat(view, heat.def.id, placed.pilot, runLength))}
                        </span>
                      {/if}
                      <span class="num">{placed.laps} {placed.laps === 1 ? 'lap' : 'laps'}</span>
                      <span class="num">best lap {formatMicros(placed.best_lap_micros)}</span>
                      {#if placed.position_set}<Badge tone="violet">Set by hand</Badge>{/if}
                      {#if placed.disqualified}<Badge tone="danger">Disqualified</Badge>{/if}
                      {#if !placed.flew}<Badge tone="neutral">Did not fly</Badge>{/if}
                    </li>
                  {/each}
                </ol>
              </Card>
            {/if}

            <Card title="Protests" elevation="sm">
              {#if protests.length === 0}
                <p class="muted">No protest has been filed against this race.</p>
              {:else}
                <ol class="protests" aria-label="Protests against this race">
                  {#each protests as protest, index (protest.protest)}
                    <li aria-label="Protest {index + 1}">
                      <div class="protest-head">
                        <span class="race-name">Protest {index + 1}</span>
                        <Badge tone={protest.outcome === undefined ? 'warn' : 'neutral'}>
                          {protestStateLabel(protest)}
                        </Badge>
                        <span class="muted">
                          {protest.by === undefined
                            ? 'No pilot named'
                            : `By ${names.pilot(protest.by)}`}
                        </span>
                      </div>
                      {#if protest.note}<p class="race-note">{protest.note}</p>{/if}
                      {#if protest.outcome === undefined}
                        <div class="resolve">
                          <span class="resolve-note">
                            <Input
                              size="sm"
                              bind:value={
                                () => outcomeNotes[protest.protest] ?? '',
                                (text) => (outcomeNotes[protest.protest] = String(text))
                              }
                              placeholder="Note (optional)"
                              aria-label="Note for the outcome of protest {index + 1}"
                              autocomplete="off"
                            />
                          </span>
                          {#each PROTEST_OUTCOMES as entry (entry.outcome)}
                            <Button
                              size="sm"
                              disabled={busy}
                              aria-label="{entry.label}: protest {index + 1}"
                              onclick={() => {
                                const note = (outcomeNotes[protest.protest] ?? '').trim();
                                void act({
                                  Record: {
                                    ProtestResolved: {
                                      protest: protest.protest,
                                      outcome: entry.outcome,
                                      ...(note ? { note } : {})
                                    }
                                  }
                                });
                              }}>{entry.label}</Button
                            >
                          {/each}
                        </div>
                      {/if}
                    </li>
                  {/each}
                </ol>
              {/if}

              {#if !race.locked}
                <form class="file" aria-label="File a protest" onsubmit={fileProtest}>
                  <label class="file-by">
                    <span>Protest by</span>
                    <Select bind:value={protestBy} size="sm" aria-label="Protest by">
                      <option value="">No pilot named</option>
                      {#each race.seats as seat (`${seat.adapter}/${seat.seat}`)}
                        {#if seat.pilot !== undefined}
                          <option value={seat.pilot}>{names.pilot(seat.pilot)}</option>
                        {/if}
                      {/each}
                    </Select>
                  </label>
                  <span class="file-note">
                    <Input
                      size="sm"
                      bind:value={protestNote}
                      placeholder="What is protested (optional)"
                      aria-label="What is protested"
                      autocomplete="off"
                    />
                  </span>
                  <Button size="sm" type="submit" disabled={busy}>File protest</Button>
                </form>
                <p class="muted">A phase cannot be locked while a protest in it is open.</p>
              {:else}
                <Banner tone="info">A locked race takes no new protest.</Banner>
              {/if}
            </Card>
          {/if}
        </section>
      </div>
    {/if}
  </div>
</Page>

<style>
  .filter {
    display: grid;
    gap: var(--gf-space-2);
    margin-bottom: var(--gf-space-3);
  }
  .option {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-secondary);
  }
  .page-inner {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .muted {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .columns {
    display: grid;
    grid-template-columns: minmax(16rem, 22rem) minmax(0, 1fr);
    gap: var(--gf-space-5);
    align-items: start;
  }
  .lists,
  .review {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
    min-width: 0;
  }
  .todo,
  .heat-group ul,
  .seats,
  .ranking,
  .protests {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .heat-group + .heat-group {
    margin-top: var(--gf-space-3);
  }
  .heat-group h3 {
    margin: 0 0 var(--gf-space-1);
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .race-link {
    width: 100%;
    display: grid;
    gap: 2px;
    text-align: left;
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-border-subtle);
    border-left: 3px solid var(--gf-border-strong);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface);
    color: inherit;
    font: inherit;
    cursor: pointer;
  }
  .race-link.row {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: var(--gf-space-3);
  }
  .race-link:hover {
    border-color: var(--gf-border-strong);
  }
  .race-link:focus-visible {
    outline: 2px solid var(--gf-accent);
    outline-offset: 2px;
  }
  .race-link[aria-current='true'] {
    border-left-color: var(--gf-accent);
    background: var(--gf-surface-sunken);
  }
  .race-name {
    font-weight: var(--gf-font-weight-semibold);
    overflow-wrap: anywhere;
  }
  .reason {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-secondary);
  }
  .note {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
    font-style: italic;
    overflow-wrap: anywhere;
  }
  .race-head {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
  }
  .race-head h2 {
    margin: 0;
    font-size: var(--gf-font-size-xl);
  }
  .head-actions {
    margin-left: auto;
    display: flex;
    gap: var(--gf-space-2);
  }
  .race-note {
    margin: var(--gf-space-2) 0;
    overflow-wrap: anywhere;
  }
  .ranking li,
  .protest-head,
  .resolve,
  .file {
    display: flex;
    align-items: baseline;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
  }
  .resolve,
  .file {
    align-items: center;
  }
  .num.strong {
    color: var(--gf-text);
    font-weight: var(--gf-font-weight-bold);
  }
  .place {
    min-width: 1.5rem;
    font-family: var(--gf-font-mono);
    font-weight: var(--gf-font-weight-bold);
    color: var(--gf-text-muted);
  }
  .who {
    font-weight: var(--gf-font-weight-semibold);
  }
  .num {
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-secondary);
  }
  .protests li {
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-sm);
    display: grid;
    gap: var(--gf-space-2);
  }
  .protests .race-note {
    margin: 0;
  }
  .resolve-note,
  .file-note {
    flex: 1;
    min-width: 12rem;
  }
  .file {
    margin-top: var(--gf-space-4);
    margin-bottom: var(--gf-space-2);
  }
  .file-by {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
    white-space: nowrap;
  }
  @media (max-width: 60rem) {
    .columns {
      grid-template-columns: 1fr;
    }
  }
</style>
