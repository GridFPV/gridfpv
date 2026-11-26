<script lang="ts">
  /**
   * OpenSeats — check-in on a phone or a kiosk (docs/primitives.md §11.9).
   *
   * While a practice race with open seats is being flown, this lists those seats and who
   * is on each. A signed-in pilot can take one, take one over from another pilot, or drop
   * the one they have. Their laps start at their first pass through the gate after they
   * take the seat.
   *
   * If the Director refuses (the race just stopped, say), its reason is shown and nothing
   * changes.
   */
  import { Badge, Button } from '@gridfpv/components';
  import type { PublicPilot, RaceView } from '@gridfpv/types';
  import { openSeats, type CheckinActions, type OpenSeat } from '../lib/checkin.js';
  import type { Names } from '../lib/names.js';

  let {
    race,
    names,
    pilot,
    actions = undefined,
    channelOf = undefined
  }: {
    /** The race being flown that has open seats. */
    race: RaceView;
    names: Names;
    /** The pilot this device is signed in as. */
    pilot: PublicPilot;
    /** Left out when nothing can be changed from here. */
    actions?: CheckinActions | undefined;
    /** The video channel a seat is on, when the timer says. */
    channelOf?: ((seat: OpenSeat) => string | undefined) | undefined;
  } = $props();

  const seats = $derived(openSeats(race, names));
  const mine = $derived(seats.find((seat) => seat.holder === pilot.id));

  let busy = $state(false);
  let error = $state<string | undefined>(undefined);

  async function act(what: () => Promise<unknown>) {
    if (busy) return;
    busy = true;
    error = undefined;
    try {
      await what();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }
</script>

<section class="open-seats" aria-label="Fly now">
  <p class="how">
    {#if mine}
      You have {mine.seatName}. Your laps count from your first pass through the gate.
    {:else}
      Take a seat to fly now. Your laps count from your first pass through the gate.
    {/if}
  </p>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  <ul aria-label="Open seats">
    {#each seats as seat (`${seat.adapter}/${seat.seat}`)}
      {@const channel = channelOf?.(seat)}
      <li class:mine={seat.holder === pilot.id}>
        <span class="seat">
          <strong>{seat.seatName}</strong>
          {#if channel}<span class="channel">{channel}</span>{/if}
        </span>
        <span class="who">
          {#if seat.holder === pilot.id}
            <Badge tone="accent">You</Badge>
          {:else if seat.holderName}
            {seat.holderName}
          {:else}
            <span class="free">Free</span>
          {/if}
        </span>
        {#if actions}
          {#if seat.holder === pilot.id}
            <Button
              size="sm"
              variant="secondary"
              disabled={busy}
              aria-label="Drop {seat.seatName}"
              onclick={() => act(() => actions.drop(race.race))}>Drop</Button
            >
          {:else}
            <Button
              size="sm"
              variant={seat.holder === undefined ? 'primary' : 'secondary'}
              disabled={busy}
              aria-label={seat.holderName
                ? `Take ${seat.seatName} over from ${seat.holderName}`
                : `Take ${seat.seatName}`}
              onclick={() => act(() => actions.take(race.race, seat))}
              >{seat.holder === undefined ? 'Take' : 'Take over'}</Button
            >
          {/if}
        {/if}
      </li>
    {/each}
  </ul>
</section>

<style>
  .how {
    margin: 0 0 var(--gf-space-2);
    color: var(--gf-text-secondary);
    font-size: var(--gf-font-size-sm);
  }
  .error {
    margin: 0 0 var(--gf-space-2);
    color: var(--gf-danger);
    font-size: var(--gf-font-size-sm);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--gf-space-2);
  }
  li {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-md);
  }
  li.mine {
    border-color: var(--gf-accent);
  }
  .seat {
    display: flex;
    flex-direction: column;
  }
  .channel {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .who {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .free {
    color: var(--gf-text-muted);
  }
</style>
