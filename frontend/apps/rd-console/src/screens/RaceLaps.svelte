<script lang="ts">
  /**
   * RaceLaps — one race's lap times, big enough to read at a glance.
   *
   * A column for each pilot in finishing order: their place, every lap in order, their
   * best lap marked, and the total. Opened from a pilot's own races and from the results.
   * It only shows: nothing here changes anything.
   */
  import { Badge, Dialog, formatMicros } from '@gridfpv/components';
  import type { PilotId, RaceView } from '@gridfpv/types';
  import { ordinal, type Names } from '../lib/names.js';
  import { raceDetail } from '../lib/results.js';

  let {
    name,
    race,
    names,
    runLength = undefined,
    pilot = undefined,
    onclose
  }: {
    /** "Qualifying Heat 1 · race 2". */
    name: string;
    race: RaceView;
    names: Names;
    /** The N of a heat ranked on best N laps in a row. */
    runLength?: number | undefined;
    /** The pilot looking, so their column can be marked. */
    pilot?: PilotId | undefined;
    onclose: () => void;
  } = $props();

  const rows = $derived(raceDetail(race, names, runLength));
  const mine = $derived(
    pilot === undefined
      ? undefined
      : (() => {
          const index = race.seats.findIndex((seat) => seat.pilot === pilot);
          const seat = race.seats[index];
          return seat ? `${seat.adapter}\u0000${seat.seat}` : undefined;
        })()
  );
  const time = (micros: number | undefined): string =>
    micros === undefined ? '—' : formatMicros(micros);
</script>

<Dialog open={true} title="Lap times: {name}" {onclose}>
  <div class="pilots">
    {#each rows as row (row.key)}
      <section class:me={row.key === mine} aria-label="Laps of {row.name}">
        <header>
          <strong>{row.name}</strong>
          {#if row.place !== undefined}<Badge tone="accent">{ordinal(row.place)}</Badge>{/if}
          {#if row.falseStart}<Badge tone="danger">False start</Badge>{/if}
        </header>
        {#if row.lapList.length === 0}
          <p class="muted">No laps.</p>
        {:else}
          <ol>
            {#each row.lapList as lap, index (index)}
              <li class:best={lap.best} class:run={lap.inBestRun}>
                <span class="label">{lap.label === 'HS' ? 'Start' : lap.label.slice(1)}</span>
                <span class="num">{time(lap.micros)}</span>
              </li>
            {/each}
          </ol>
          <dl>
            <dt>Best lap</dt>
            <dd class="num">{time(row.bestLapMicros)}</dd>
            {#if runLength !== undefined}
              <dt>Best {runLength} in a row</dt>
              <dd class="num">{time(row.bestRun?.totalMicros)}</dd>
            {/if}
            <dt>Total</dt>
            <dd class="num">{time(row.totalMicros)}</dd>
          </dl>
        {/if}
      </section>
    {/each}
  </div>
</Dialog>

<style>
  .pilots {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(9.5rem, 1fr));
    gap: var(--gf-space-3);
  }
  section {
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-sm);
    padding: var(--gf-space-2) var(--gf-space-3);
    min-width: 0;
  }
  section.me {
    border-color: var(--gf-accent);
  }
  header {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
    margin-bottom: var(--gf-space-2);
    font-size: var(--gf-font-size-lg);
    overflow-wrap: anywhere;
  }
  ol {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
  }
  li {
    display: flex;
    justify-content: space-between;
    gap: var(--gf-space-3);
    font-size: var(--gf-font-size-lg);
    padding: 2px var(--gf-space-1);
    border-radius: var(--gf-radius-sm);
  }
  li.run {
    background: var(--gf-surface-raised, transparent);
  }
  li.best .num {
    color: var(--gf-accent);
    font-weight: var(--gf-font-weight-bold);
  }
  .label {
    color: var(--gf-text-muted);
  }
  .num {
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  dl {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 2px var(--gf-space-3);
    margin: var(--gf-space-2) 0 0;
    padding-top: var(--gf-space-2);
    border-top: 1px solid var(--gf-border-subtle);
    font-size: var(--gf-font-size-sm);
  }
  dt {
    color: var(--gf-text-muted);
    white-space: nowrap;
  }
  dd {
    margin: 0;
    text-align: right;
  }
  .muted {
    margin: 0;
    color: var(--gf-text-muted);
  }
</style>
