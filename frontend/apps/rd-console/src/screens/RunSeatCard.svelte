<script lang="ts">
  /**
   * RunSeatCard — one seat of the heat on the Run view: who is on it, their place, how many
   * laps, the last and best lap, and every lap in order.
   *
   * While crossings are simulated (`SIMULATE_CROSSINGS` in `lib/seats.ts`) it also carries a
   * "Crossing" button marked "Bench". That button is a bench tool, not a race-day control.
   */
  import { Badge, Button, formatMicros } from '@gridfpv/components';
  import type { SeatRow } from '../lib/run.js';
  import { SIMULATE_CROSSINGS } from '../lib/seats.js';

  let {
    row,
    racing,
    oncrossing
  }: {
    row: SeatRow;
    /** Whether a race is running, so a crossing would count. */
    racing: boolean;
    /** Log a crossing on this seat by hand (the bench tool). */
    oncrossing: () => void;
  } = $props();
</script>

<li class="seat" aria-label="{row.seatName}, {row.who}">
  <div class="head">
    <span class="seat-name">{row.seatName}</span>
    <span class="who">{row.who}</span>
    {#if row.falseStart}<Badge tone="danger">False start</Badge>{/if}
    {#if row.onCourse && racing}<Badge tone="success" dot>On course</Badge>{/if}
    {#if SIMULATE_CROSSINGS}
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
      <dd data-stat="best">{formatMicros(row.bestLapMicros)}</dd>
    </div>
  </dl>
  {#if row.laps.length > 0}
    <ol class="laps" aria-label="Laps for {row.seatName}">
      {#each row.laps as lap, index (lap.crossing)}
        <li class:best={lap.micros === row.bestLapMicros}>
          <span class="n">{index + 1}</span>{formatMicros(lap.micros)}
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
  .bench {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-1);
  }
  .stats {
    margin: 0;
    display: grid;
    grid-template-columns: repeat(4, 1fr);
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
  .laps li.best {
    border-color: var(--gf-success);
    color: var(--gf-success);
  }
  .laps .n {
    margin-right: 0.5em;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-xs);
  }
</style>
