<script lang="ts">
  /**
   * MarshalSeat — one seat of the race under review: who flew it, their place, the trace of
   * its signal, every lap with the rulings a marshal can make on it, the crossings that do
   * not count, and the tool that adds a lap.
   *
   * Every ruling is a person's choice and goes to the Director as one fact. The laps shown
   * afterwards are the Director's, worked out again; nothing is worked out here.
   *
   * - **Remove** takes a lap out. Its time joins the lap after it; the lap before it stays
   *   as it was. The reason is a label only.
   * - **Mark as restart** is the other case: that lap has no time, and the laps either side
   *   stay as they were.
   *
   * A lap picked in the list is highlighted on the trace, and a crossing clicked on the trace
   * is picked in the list.
   */
  import { Badge, Button, Input, formatMicros } from '@gridfpv/components';
  import type { Command, LogRef, RaceSeatView, RaceView, SeatSignal } from '@gridfpv/types';
  import {
    countedLaps,
    formatOffset,
    lapRows,
    parseOffset,
    REMOVAL_REASONS,
    timeBase,
    traceMarks,
    uncountedLabel
  } from '../lib/marshal.js';
  import { ordinal, type Names } from '../lib/names.js';
  import { isBench } from '../lib/seats.js';
  import RaceTrace from './RaceTrace.svelte';

  let {
    race,
    seat,
    index,
    names,
    signal = undefined,
    signalState,
    editable,
    busy,
    act
  }: {
    race: RaceView;
    seat: RaceSeatView;
    /** The seat's place in the lineup, from 0. */
    index: number;
    names: Names;
    /** The seat's logged signal, once it has been read. */
    signal?: SeatSignal | undefined;
    /** Where the read of the race's signal stands. */
    signalState: 'loading' | 'ready' | 'failed';
    /** Whether rulings can be made on this race. */
    editable: boolean;
    busy: boolean;
    /** Send one ruling. Resolves `true` when the Director took it. */
    act: (command: Command) => Promise<boolean>;
  } = $props();

  const who = $derived(seat.pilot === undefined ? names.who('Open') : names.pilot(seat.pilot));
  const seatName = $derived(names.seat(seat.adapter, seat.seat, index));
  const timer = $derived(names.timer(seat.adapter));
  const rows = $derived(lapRows(seat));
  const base = $derived(timeBase(race, seat, signal));
  const hasTrace = $derived((signal?.chunks.length ?? 0) > 0);
  const marks = $derived(traceMarks(seat, signal?.start_at));

  /** The crossing picked in the list or on the trace. */
  let selected = $state<LogRef | undefined>(undefined);
  /** The row whose Remove was pressed and is waiting for a reason. */
  let removing = $state<LogRef | undefined>(undefined);
  /** What is typed in "add a lap", and the exact time when it was picked on the trace. */
  let addText = $state('');
  let pickedAt = $state<number | undefined>(undefined);
  let addError = $state<string | undefined>(undefined);

  const addAt = $derived(
    pickedAt ?? (base && addText.trim() !== '' ? parseOffset(addText, base.at) : undefined)
  );

  function pick(crossing: LogRef) {
    selected = selected === crossing ? undefined : crossing;
  }

  function pickOnTrace(at: number) {
    if (!base) return;
    pickedAt = at;
    addText = ((at - base.at) / 1_000_000).toFixed(6);
    addError = undefined;
  }

  /** The race every ruling here names. */
  const ref = $derived({ race: race.race });

  async function remove(crossing: LogRef, reason?: (typeof REMOVAL_REASONS)[number]['reason']) {
    const done = await act({
      Record: {
        LapRemoved: reason === undefined ? { ...ref, crossing } : { ...ref, crossing, reason }
      }
    });
    if (done) removing = undefined;
  }

  async function addLap(e?: Event) {
    e?.preventDefault();
    addError = undefined;
    if (addAt === undefined) {
      addError = 'Type a time in seconds, like 12.5.';
      return;
    }
    const done = await act({
      Record: { LapInserted: { ...ref, adapter: seat.adapter, seat: seat.seat, at: addAt } }
    });
    if (done) {
      addText = '';
      pickedAt = undefined;
    }
  }

  const rowName = (row: (typeof rows)[number]): string =>
    row.kind === 'holeshot'
      ? 'the hole shot'
      : row.kind === 'restart'
        ? 'the restart'
        : row.kind === 'timerRestart'
          ? 'the crossing after the timer restarted'
          : `lap ${row.number}`;
</script>

<li class="seat" aria-label="{seatName}, {who}">
  <div class="head">
    <span class="who">{who}</span>
    <span class="where">{seatName} · {timer}</span>
    {#if seat.finish !== undefined}
      <Badge tone="accent"><span data-stat="place">{ordinal(seat.finish)}</span></Badge>
    {/if}
    <span class="num" data-stat="laps">
      {countedLaps(seat)}
      {countedLaps(seat) === 1 ? 'lap' : 'laps'}
    </span>
    {#if seat.best_lap_micros !== undefined}
      <span class="num">best {formatMicros(seat.best_lap_micros)}</span>
    {/if}
    {#if seat.total_micros !== undefined}
      <span class="num">total {formatMicros(seat.total_micros)}</span>
    {/if}
    {#if seat.false_start}<Badge tone="danger">False start</Badge>{/if}
    {#if editable && seat.pilot !== undefined}
      {@const pilot = seat.pilot}
      <span class="head-action">
        {#if seat.false_start}
          <Button
            size="sm"
            variant="ghost"
            disabled={busy}
            aria-label="Clear the false start for {who}"
            onclick={() => act({ Record: { FalseStartCleared: { ...ref, pilot } } })}
            >Clear false start</Button
          >
        {:else}
          <Button
            size="sm"
            variant="ghost"
            disabled={busy}
            aria-label="False start by {who}"
            onclick={() => act({ Record: { FalseStart: { ...ref, pilot } } })}>False start</Button
          >
        {/if}
      </span>
    {/if}
  </div>

  {#if hasTrace && signal && base}
    <RaceTrace
      {signal}
      {marks}
      base={base.at}
      {selected}
      pending={pickedAt}
      label="Signal for {who} on {seatName} over the whole race, with the start, each crossing and the enter and exit levels. The lap list below says the same in words."
      onselect={pick}
      onpick={editable ? pickOnTrace : undefined}
    />
  {:else if signalState === 'loading'}
    <p class="muted" role="status">Reading the signal…</p>
  {:else if signalState === 'failed'}
    <p class="muted">The signal for this race could not be read. The lap list still works.</p>
  {:else if isBench(seat.adapter)}
    <p class="muted" data-testid="no-signal">
      Bench crossings are logged by hand, so there is no signal trace for this seat.
    </p>
  {:else}
    <p class="muted" data-testid="no-signal">
      {timer} logged no signal for this seat, so there is no trace.
    </p>
  {/if}

  {#if rows.length === 0}
    <p class="muted">No laps.</p>
  {:else}
    <table class="laps" aria-label="Laps for {who}">
      <thead>
        <tr>
          <th scope="col">Lap</th>
          <th scope="col">Time</th>
          <th scope="col">{base?.words === 'into the race' ? 'Into the race' : 'Crossed at'}</th>
          <th scope="col"><span class="visually-hidden">Rulings</span></th>
        </tr>
      </thead>
      <tbody>
        {#each rows as row (row.crossing)}
          <tr class:selected={selected === row.crossing} data-kind={row.kind}>
            <th scope="row">
              <button
                type="button"
                class="pick"
                aria-pressed={selected === row.crossing}
                title="Show on the trace"
                onclick={() => pick(row.crossing)}
              >
                {#if row.kind === 'holeshot'}Hole shot{:else if row.kind === 'restart'}Restart{:else if row.kind === 'timerRestart'}Timer
                  restarted{:else}Lap
                  {row.number}{/if}
              </button>
            </th>
            <td class="num">
              {#if row.kind === 'lap'}
                <span data-lap-time>{formatMicros(row.micros)}</span>
                {#if row.best}<Badge tone="success">Best</Badge>{/if}
              {:else if row.kind === 'restart'}
                <span class="muted">No lap time</span>
              {:else if row.kind === 'timerRestart'}
                <span class="muted">No lap time: the timer's clock started again</span>
              {:else}
                <span class="muted">Starts lap 1</span>
              {/if}
            </td>
            <td class="num">{base ? `${formatOffset(row.at, base.at)} s` : '—'}</td>
            <td class="actions">
              {#if editable}
                {#if removing === row.crossing}
                  <span class="reasons" role="group" aria-label="Why remove {rowName(row)}?">
                    {#each REMOVAL_REASONS as entry (entry.reason)}
                      <Button
                        size="sm"
                        disabled={busy}
                        onclick={() => remove(row.crossing, entry.reason)}>{entry.label}</Button
                      >
                    {/each}
                    <Button size="sm" disabled={busy} onclick={() => remove(row.crossing)}>
                      No reason
                    </Button>
                    <Button size="sm" variant="ghost" onclick={() => (removing = undefined)}>
                      Keep it
                    </Button>
                  </span>
                {:else}
                  {#if row.kind === 'lap'}
                    <Button
                      size="sm"
                      variant="ghost"
                      disabled={busy}
                      aria-label="Mark {rowName(row)} of {who} as a restart"
                      onclick={() =>
                        act({ Record: { RestartMarked: { ...ref, crossing: row.crossing } } })}
                      >Mark as restart</Button
                    >
                  {:else if row.kind === 'restart'}
                    <Button
                      size="sm"
                      variant="ghost"
                      disabled={busy}
                      aria-label="Unmark the restart of {who}"
                      onclick={() =>
                        act({ Record: { RestartUnmarked: { ...ref, crossing: row.crossing } } })}
                      >Unmark restart</Button
                    >
                  {/if}
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={busy}
                    aria-label="Remove {rowName(row)} of {who}"
                    onclick={() => (removing = row.crossing)}>Remove</Button
                  >
                {/if}
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}

  {#if seat.uncounted.length > 0}
    <div class="uncounted">
      <h4>Crossings that do not count</h4>
      <ul aria-label="Crossings of {who} that do not count">
        {#each seat.uncounted as crossing (crossing.crossing)}
          <li class:selected={selected === crossing.crossing}>
            <button
              type="button"
              class="pick"
              aria-pressed={selected === crossing.crossing}
              title="Show on the trace"
              onclick={() => pick(crossing.crossing)}
            >
              {base ? `${formatOffset(crossing.at, base.at)} s` : 'Crossing'}
            </button>
            <span class="why">{uncountedLabel(crossing.why, crossing.reason)}</span>
            {#if editable && crossing.why === 'Removed'}
              <Button
                size="sm"
                variant="ghost"
                disabled={busy}
                aria-label="Restore the crossing of {who}{base
                  ? ` at ${formatOffset(crossing.at, base.at)} s`
                  : ''}"
                onclick={() =>
                  act({ Record: { LapRestored: { ...ref, crossing: crossing.crossing } } })}
                >Restore</Button
              >
            {/if}
          </li>
        {/each}
      </ul>
    </div>
  {/if}

  {#if editable}
    {#if base}
      <form class="add" aria-label="Add a lap for {who}" onsubmit={addLap}>
        <label class="add-label" for="add-{index}">
          Add a lap: seconds {base.words}
        </label>
        <span class="add-input">
          <Input
            id="add-{index}"
            bind:value={addText}
            size="sm"
            inputmode="decimal"
            placeholder="e.g. 12.5"
            autocomplete="off"
            aria-label="Seconds {base.words}, for {who}"
            oninput={() => (pickedAt = undefined)}
            disabled={busy}
          />
        </span>
        <Button size="sm" type="submit" disabled={busy || addText.trim() === ''}>
          {pickedAt === undefined ? 'Add lap' : 'Add a lap here'}
        </Button>
        {#if hasTrace}<span class="muted">Or click an empty spot on the trace.</span>{/if}
        {#if addError}<span class="error" role="alert">{addError}</span>{/if}
      </form>
    {:else}
      <p class="muted">
        A lap cannot be added by time here: the race's start on {timer} is not known and the seat has
        no crossing to count from.
      </p>
    {/if}
  {/if}
</li>

<style>
  .seat {
    list-style: none;
    background: var(--gf-surface);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    padding: var(--gf-space-3);
    display: grid;
    gap: var(--gf-space-3);
    min-width: 0;
  }
  .head {
    display: flex;
    align-items: baseline;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
  }
  .who {
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-bold);
  }
  .where {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .head-action {
    margin-left: auto;
  }
  .num {
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
    font-size: var(--gf-font-size-sm);
  }
  .muted {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    font-family: var(--gf-font-family);
  }
  .laps {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--gf-font-size-sm);
  }
  .laps th,
  .laps td {
    text-align: left;
    padding: var(--gf-space-1) var(--gf-space-2);
    border-bottom: 1px solid var(--gf-border-subtle);
    vertical-align: middle;
  }
  .laps thead th {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
    font-weight: var(--gf-font-weight-semibold);
  }
  .laps tr.selected,
  .uncounted li.selected {
    background: var(--gf-accent-soft, var(--gf-surface-alt));
    outline: 1px solid var(--gf-accent);
  }
  .actions {
    text-align: right;
    white-space: nowrap;
  }
  .laps td.actions {
    text-align: right;
  }
  .reasons {
    display: inline-flex;
    gap: var(--gf-space-1);
    flex-wrap: wrap;
    justify-content: flex-end;
    white-space: normal;
  }
  .pick {
    background: none;
    border: 0;
    padding: 0;
    color: inherit;
    font: inherit;
    font-weight: var(--gf-font-weight-semibold);
    cursor: pointer;
    text-decoration: underline dotted;
    text-underline-offset: 3px;
  }
  .pick:focus-visible {
    outline: 2px solid var(--gf-accent);
    outline-offset: 2px;
  }
  .uncounted h4 {
    margin: 0 0 var(--gf-space-1);
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .uncounted ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--gf-space-1);
    font-size: var(--gf-font-size-sm);
  }
  .uncounted li {
    display: flex;
    align-items: baseline;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
    padding: 0 var(--gf-space-2);
  }
  .uncounted .pick {
    font-family: var(--gf-font-mono);
  }
  .why {
    color: var(--gf-text-secondary);
    flex: 1;
  }
  .add {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
    font-size: var(--gf-font-size-sm);
  }
  .add-label {
    font-weight: var(--gf-font-weight-semibold);
  }
  .add-input {
    width: 8rem;
  }
  .error {
    color: var(--gf-danger);
  }
  .visually-hidden {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
</style>
