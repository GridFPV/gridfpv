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
   * - **Move** gives a crossing a new time. Drag it on the trace, or pick the lap and use
   *   "Move here". Either way it is a preview first, with the lap times it would give, and
   *   goes to the Director only when the marshal says "Move it".
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
    movePreview,
    parseOffset,
    REMOVAL_REASONS,
    rowWords,
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
    act,
    playhead = undefined,
    onseek = undefined
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
    /** Where the race's video is, in microseconds into the race. Left out with no video open. */
    playhead?: number | undefined;
    /** Move the race's video to a moment of the race, in microseconds from its start. */
    onseek?: ((raceMicros: number) => void) | undefined;
  } = $props();

  const who = $derived(
    seat.pilot === undefined
      ? names.seatWho('Open', seat.adapter, seat.seat, index)
      : names.pilot(seat.pilot)
  );
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

  // The video and this seat's times only line up when the times are counted from the race's
  // start. Counted from the seat's first crossing, there is nothing to line them up with.
  const aligned = $derived(base?.words === 'into the race' ? base.at : undefined);
  const cursor = $derived(
    aligned !== undefined && playhead !== undefined ? aligned + playhead : undefined
  );
  const seekTo = $derived(
    aligned !== undefined && onseek ? (at: number) => onseek(at - aligned) : undefined
  );
  // With the video open, "add a lap" is always where the video is: play it, step it or
  // click the trace, and the time in the box follows. So what the marshal sees on the
  // screen is the moment a lap would be added at. Typing in the box works the other way
  // round: the video goes to the time typed. While the box is being typed in, it is left
  // alone.
  let typing = $state(false);
  $effect(() => {
    if (!editable || typing || cursor === undefined || aligned === undefined) return;
    if (cursor < aligned) {
      // Before the start there is no lap to add.
      pickedAt = undefined;
      addText = '';
      return;
    }
    pickedAt = cursor;
    addText = ((cursor - aligned) / 1_000_000).toFixed(3);
    addError = undefined;
  });
  function typed() {
    pickedAt = undefined;
    if (!base || !seekTo) return;
    const at = parseOffset(addText, base.at);
    if (at !== undefined) seekTo(at);
  }

  /** Pick a lap in the list, and show its crossing in the video. */
  function pickRow(crossing: LogRef) {
    pick(crossing);
    const mark = marks.find((entry) => entry.crossing === crossing);
    if (mark) seekTo?.(mark.at);
  }

  function pickOnTrace(at: number) {
    if (!base) return;
    pickedAt = at;
    addText = ((at - base.at) / 1_000_000).toFixed(6);
    addError = undefined;
  }

  /** The race every ruling here names. */
  const ref = $derived({ race: race.race });

  /** A crossing the marshal is moving and where to. A preview until "Move it". */
  let move = $state<{ crossing: LogRef; at: number } | undefined>(undefined);
  const preview = $derived(move ? movePreview(seat, move.crossing, move.at) : undefined);
  /** Moves smaller than this are not offered: a millisecond is as fine as a lap time is shown. */
  const SMALLEST_MOVE = 1000;

  /** A crossing was dragged on the trace: preview it there, and show that moment in the video. */
  function dragged(crossing: LogRef, at: number) {
    move = { crossing, at };
    selected = crossing;
    seekTo?.(at);
  }
  /** Whether "Move here" has somewhere to move a row to: the video's moment or a picked spot. */
  const canMoveTo = (row: (typeof rows)[number]): boolean =>
    addAt !== undefined && Math.abs(addAt - row.at) >= SMALLEST_MOVE;

  async function confirmMove() {
    if (!move) return;
    const done = await act({ Record: { LapAdjusted: { ...ref, ...move } } });
    if (done) move = undefined;
  }

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
    // With the video open the box stays on the video's moment. Without it, it is cleared.
    if (done && cursor === undefined) {
      addText = '';
      pickedAt = undefined;
    }
  }

  const rowName = rowWords;
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
    {#if seat.out}<Badge tone="warn">Out</Badge>{/if}
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
      {cursor}
      onseek={seekTo}
      moving={preview ? move : undefined}
      onmove={editable ? dragged : undefined}
    />
    {#if move && preview && base}
      <div class="move" role="group" aria-label="Move {preview.name} of {who}">
        <span>
          Move {preview.name} from {formatOffset(preview.from, base.at)} s to
          <strong data-testid="move-to">{formatOffset(move.at, base.at)} s</strong>?
        </span>
        {#if preview.laps.length > 0}
          <span class="muted" data-testid="move-laps">
            {#each preview.laps as lap, n (lap.name)}{n > 0 ? ', ' : ''}{lap.name} becomes {formatMicros(
                lap.micros
              )}{/each}.
          </span>
        {:else}
          <span class="muted" data-testid="move-laps">
            This moves it past another crossing, so the laps are worked out again.
          </span>
        {/if}
        <Button size="sm" disabled={busy} onclick={confirmMove}>Move it</Button>
        <Button size="sm" variant="ghost" onclick={() => (move = undefined)}>Cancel</Button>
      </div>
    {/if}
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
                onclick={() => pickRow(row.crossing)}
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
                  {#if selected === row.crossing && canMoveTo(row) && addAt !== undefined}
                    {@const to = addAt}
                    <Button
                      size="sm"
                      variant="ghost"
                      disabled={busy}
                      title={cursor !== undefined
                        ? 'Move this crossing to where the video is'
                        : 'Move this crossing to the time in "Add a lap"'}
                      aria-label="Move {rowName(row)} of {who} here"
                      onclick={() => (move = { crossing: row.crossing, at: to })}>Move here</Button
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
              onclick={() => pickRow(crossing.crossing)}
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
            {:else if editable}
              <!-- Left out by a lap filter. A marshal who saw the lap can rule that it counts. -->
              <Button
                size="sm"
                variant="ghost"
                disabled={busy}
                title="Rule that this crossing was a real lap. No lap rule leaves it out after that."
                aria-label="Count the crossing of {who}{base
                  ? ` at ${formatOffset(crossing.at, base.at)} s`
                  : ''}"
                onclick={() =>
                  act({ Record: { LapCounted: { ...ref, crossing: crossing.crossing } } })}
                >Count it</Button
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
            oninput={typed}
            onfocus={() => (typing = true)}
            onblur={() => (typing = false)}
            disabled={busy}
          />
        </span>
        <Button size="sm" type="submit" disabled={busy || addText.trim() === ''}>
          {pickedAt === undefined ? 'Add lap' : 'Add a lap here'}
        </Button>
        {#if hasTrace}
          <span class="muted">
            Or click an empty spot on the trace. Drag a crossing on the trace to move it.
          </span>
        {/if}
        <span class="muted" data-testid="add-hint">
          Missed the hole shot? Add it here. A crossing before the first one becomes the hole shot.
        </span>
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
  .move {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
    font-size: var(--gf-font-size-sm);
    padding: var(--gf-space-2);
    border: 1px dashed var(--gf-accent);
    border-radius: var(--gf-radius-sm);
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
