<script lang="ts">
  /**
   * RunSeatCard — one seat of the heat on the Run view: who is on it, their place, how many
   * laps, the last and best lap, and every lap in order. In a heat ranked on the best N laps
   * in a row it also shows that run: this race's, the heat's best so far, and which laps
   * make it up.
   *
   * While a race is being flown, or has stopped and is not saved yet, a seat with a pilot
   * has an **Out** button: the pilot has crashed or will not finish. The race then stops
   * waiting for them. Their laps still count. **Back in** takes it back.
   *
   * An **open seat** (no pilot named on it) can have a pilot checked in while the race is
   * flown: pick them in "Check in". The laps on the seat are then theirs from their next
   * pass, and the card shows that pilot and those laps. **Drop** frees the seat.
   *
   * A bench seat has no timer behind it, so it also carries a "Crossing" button marked
   * "Bench" that logs a crossing by hand. A seat on a real timer never shows it.
   */
  import { Badge, Button, formatMicros } from '@gridfpv/components';
  import type { LapRun } from '../lib/overlay.js';
  import type { PilotId } from '@gridfpv/types';
  import { cardLaps, runLapsLabel, type SeatRow } from '../lib/run.js';

  let {
    row,
    racing,
    runLength,
    run,
    heatBestMicros,
    tuneHref,
    oncrossing,
    onout,
    pilots = [],
    oncheckin
  }: {
    row: SeatRow;
    /** Whether a race is running, so a crossing would count. */
    racing: boolean;
    /** The N of a heat ranked on its best N laps in a row. Left out for any other heat. */
    runLength?: number;
    /** This seat's best run of N laps in the race on screen, if it has one yet. */
    run?: LapRun;
    /** The pilot's best run of N laps in the heat so far, in microseconds. */
    heatBestMicros?: number;
    /** Where the seat's Tune page is. Left out for a seat with nothing to tune. */
    tuneHref?: string;
    /** Log a crossing on this seat by hand (the bench tool). */
    oncrossing: () => void;
    /**
     * Mark the pilot out of the race (`true`), or back in (`false`). Left out when there
     * is no race a pilot could be out of, and the button is then not shown.
     */
    onout?: (out: boolean) => void;
    /** The pilots who can be checked in to an open seat, by callsign. */
    pilots?: readonly { id: PilotId; callsign: string }[];
    /**
     * Check a pilot in to this open seat, or (`undefined`) drop whoever has it. Left out
     * when there is no race a seat could be taken in.
     */
    oncheckin?: (pilot: PilotId | undefined) => void;
  } = $props();

  // The latest laps only, so the card is one size however long the race runs. Which lap
  // was the best is said beside the best time. Every lap is in the table under the cards.
  const listed = $derived(cardLaps(row.laps, row.bestLapMicros, run));
</script>

<li class="seat" class:out={row.out} aria-label="{row.seatName}, {row.who}">
  <div class="head">
    <span class="seat-name">{row.seatName}</span>
    <span class="who">{row.who}</span>
    {#if row.falseStart}<Badge tone="danger">False start</Badge>{/if}
    {#if row.out}
      <Badge tone="warn">Out</Badge>
    {:else if row.onCourse && racing}
      <Badge tone="success" dot>On course</Badge>
    {/if}
    {#if oncheckin && row.open}
      <span class="checkin">
        <select
          aria-label="Check a pilot in to {row.seatName}"
          value=""
          onchange={(e) => {
            const picked = e.currentTarget.value;
            e.currentTarget.value = '';
            if (picked) oncheckin(picked);
          }}
        >
          <option value="">{row.pilot === undefined ? 'Check in…' : 'Change pilot…'}</option>
          {#each pilots.filter((pilot) => pilot.id !== row.pilot) as pilot (pilot.id)}
            <option value={pilot.id}>{pilot.callsign}</option>
          {/each}
        </select>
        {#if row.pilot !== undefined}
          <Button
            size="sm"
            variant="secondary"
            title="Free this seat. The laps this pilot flew stay theirs."
            aria-label="Drop {row.who} from {row.seatName}"
            onclick={() => oncheckin(undefined)}>Drop</Button
          >
        {/if}
      </span>
    {/if}
    {#if onout && row.pilot !== undefined && !row.open}
      <Button
        size="sm"
        variant="secondary"
        title={row.out
          ? 'Put this pilot back in the race'
          : 'This pilot has crashed or will not finish. The race stops waiting for them.'}
        aria-label={row.out ? `Put ${row.who} back in` : `Mark ${row.who} out`}
        onclick={() => onout(!row.out)}>{row.out ? 'Back in' : 'Out'}</Button
      >
    {/if}
    {#if tuneHref}<a class="tune" href={tuneHref} aria-label="Tune {row.seatName}">tune</a>{/if}
    {#if row.bench}
      <span class="bench">
        <Badge tone="violet" variant="outline">Bench</Badge>
        <Button
          size="sm"
          variant="ghost"
          disabled={!racing}
          title="Bench tool: log a gate crossing on this seat by hand"
          aria-label="Crossing for {row.seatName}"
          onclick={oncrossing}>Crossing</Button
        >
      </span>
    {/if}
  </div>
  <dl class="stats">
    <div class="stat big">
      <dt>Place</dt>
      <dd data-stat="place">{row.place === undefined ? '—' : `P${row.place}`}</dd>
    </div>
    <div class="stat big">
      <dt>Laps</dt>
      <dd data-stat="laps">{row.laps.length}</dd>
    </div>
    <div class="stat">
      <dt>Last lap</dt>
      <dd data-stat="last">{formatMicros(row.lastLapMicros)}</dd>
    </div>
    <div class="stat">
      <dt>Best lap</dt>
      <dd data-stat="best">
        {formatMicros(row.bestLapMicros)}{#if listed.bestNumber !== undefined}
          <span class="which">{' '}(lap {listed.bestNumber})</span>{/if}
      </dd>
    </div>
    {#if runLength !== undefined}
      <div class="stat">
        <dt>Best {runLength}</dt>
        <dd data-stat="run">
          {formatMicros(run?.totalMicros)}{#if run}
            <span class="which">{' '}({runLapsLabel(run)})</span>{/if}
        </dd>
      </div>
      <div class="stat">
        <dt>Heat best {runLength}</dt>
        <dd data-stat="heat-run">{formatMicros(heatBestMicros)}</dd>
      </div>
    {/if}
  </dl>
  {#if row.laps.length > 0}
    <ol class="laps" aria-label="Laps for {row.seatName}">
      {#each listed.laps as shown (shown.lap.crossing)}
        <li class:best={shown.best} class:in-run={shown.inRun} data-lap={shown.number}>
          <span class="n">{shown.number}</span>{formatMicros(shown.lap.micros)}
        </li>
      {/each}
    </ol>
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
    align-content: start;
  }
  /* A pilot who is out is no longer the one to watch. */
  .seat.out {
    border-style: dashed;
  }
  .seat.out .who,
  .seat.out .stats {
    opacity: 0.6;
  }
  .head {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
  }
  .seat-name {
    font-size: var(--gf-font-size-xs);
    font-weight: var(--gf-font-weight-semibold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
    padding: 0.1em 0.5em;
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-xs);
  }
  .who {
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-bold);
    overflow-wrap: anywhere;
  }
  .checkin {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-1);
  }
  .checkin select {
    font: inherit;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text);
    background: var(--gf-surface);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    padding: 0.2em 0.4em;
    max-width: 9rem;
  }
  .bench {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-1);
  }
  .stats {
    margin: 0;
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(4.5rem, 1fr));
    gap: var(--gf-space-2);
  }
  .stat {
    display: flex;
    flex-direction: column-reverse;
    gap: 2px;
  }
  .stat dt {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .stat dd {
    margin: 0;
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
    font-size: var(--gf-font-size-md);
  }
  .stat.big dd {
    font-size: var(--gf-font-size-xl);
    font-weight: var(--gf-font-weight-bold);
  }
  .laps {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-1);
  }
  .laps li {
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
    font-size: var(--gf-font-size-sm);
    padding: 0.1em 0.5em;
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
  }
  /* The laps of the best run of N are filled, so the run can be found at a glance. */
  .laps li.in-run {
    background: color-mix(in srgb, var(--gf-accent) 18%, var(--gf-surface-sunken));
    border-color: var(--gf-accent);
  }
  .tune {
    margin-left: auto;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .laps li.best {
    border-color: var(--gf-success);
    color: var(--gf-success);
  }
  /* Which lap a best time was: small and quiet beside the time. */
  .which {
    font-size: var(--gf-font-size-sm);
    font-weight: normal;
    color: var(--gf-text-muted);
    white-space: nowrap;
  }
  .laps .n {
    margin-right: 0.5em;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-xs);
  }
</style>
