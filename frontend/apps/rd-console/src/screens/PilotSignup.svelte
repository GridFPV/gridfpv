<script lang="ts">
  /**
   * PilotSignup — the sign-up sheet on a phone (docs/primitives.md §11.5).
   *
   * It lists the heats a pilot can still get into or out of: the ones that are open. A
   * heat that is closed, on or flown is not on it. A full heat is: someone may drop out.
   *
   * Everyone given the sheet sees the heats and who is on each seat. A pilot who is signed in can also
   * take a free seat, give theirs up, or take the next free spot on a seat. A seat they
   * cannot take right now says why instead of offering a button.
   *
   * The page only offers what the sheet says will work. If the Director refuses anyway
   * (another pilot got the seat first), its reason is shown and nothing changes.
   */
  import { Badge, Button } from '@gridfpv/components';
  import type { PhaseId, PublicPilot, SignupHeatView, SignupSeat } from '@gridfpv/types';
  import type { Names } from '../lib/names.js';
  import {
    hasNextFree,
    heatStateWords,
    runsOf,
    runsWords,
    seatAction,
    openHeats,
    seatHeadings,
    type SignupPhase
  } from '../lib/signup.js';

  /** What a pilot can ask for. Each rejects with the Director's reason when refused. */
  export interface SignupActions {
    take(heat: PhaseId, seat: SignupSeat): Promise<unknown>;
    next(phase: PhaseId, seat: SignupSeat): Promise<unknown>;
    leave(heat: PhaseId): Promise<unknown>;
  }

  let {
    phases,
    names,
    pilot = undefined,
    actions = undefined,
    layout = 'list'
  }: {
    /**
     * `list` is for a phone held upright: one heat under another. `grid` is for a tablet
     * on its side at a kiosk: heats down, seats across, with large buttons.
     */
    layout?: 'list' | 'grid';
    /** The phases with sign-up, each with its sheet. */
    phases: readonly SignupPhase[];
    names: Names;
    /** The pilot this phone is signed in as. Left out for someone who is only looking. */
    pilot?: PublicPilot | undefined;
    /** Left out when nothing can be changed from here. */
    actions?: SignupActions | undefined;
  } = $props();

  let busy = $state(false);
  let error = $state<string | undefined>(undefined);

  async function act(what: () => Promise<unknown>) {
    if (busy) return;
    busy = true;
    error = undefined;
    try {
      await what();
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  const me = $derived(pilot && actions ? pilot.id : undefined);
  const heatName = (heat: SignupHeatView) => names.phase(heat.heat);
</script>

<div class="signup" data-layout={layout}>
  {#if error}
    <p class="error" role="alert" data-testid="signup-error">{error}</p>
  {/if}
  {#if phases.length === 0}
    <p class="muted">There is nothing to sign up for right now.</p>
  {/if}

  {#each phases as phase (phase.id)}
    {@const signup = phase.signup}
    {@const seats = seatHeadings(signup.rules, names)}
    {@const heats = openHeats(signup)}
    <section aria-label="Sign-up for {phase.name}">
      <header>
        <h2>{phase.name}</h2>
        <Badge tone={signup.rules.open ? 'success' : 'neutral'}>
          {signup.rules.open ? 'Open' : 'Closed'}
        </Badge>
      </header>
      {#if me}
        <p class="runs" data-testid="my-runs">{runsWords(runsOf(signup, me))}</p>
      {:else if !pilot}
        <p class="muted">Sign in as a pilot to take a seat.</p>
      {/if}

      {#if me && actions && layout === 'list'}
        <div class="next" role="group" aria-label="Next free spot in {phase.name}">
          <span class="muted">Next free:</span>
          {#each seats as seat, index (seat.name)}
            <Button
              size="sm"
              variant="secondary"
              disabled={busy || !hasNextFree(signup, index, me)}
              aria-label="Next free spot on {seat.name}"
              onclick={() => act(() => actions.next(phase.id, seat.seat))}
            >
              {seat.name}
            </Button>
          {/each}
        </div>
      {/if}

      {#if heats.length === 0}
        <p class="muted" data-testid="no-open-heats">No heats are open right now.</p>
      {:else if layout === 'grid'}
        <table aria-label="Heats of {phase.name}">
          <thead>
            <tr>
              <th scope="col">Heat</th>
              {#each seats as seat, index (seat.name)}
                <th scope="col">
                  <span class="seat-name">{seat.name}</span>
                  {#if seat.channel}<span class="channel">{seat.channel}</span>{/if}
                  {#if me && actions}
                    <Button
                      variant="secondary"
                      disabled={busy || !hasNextFree(signup, index, me)}
                      aria-label="Next free spot on {seat.name}"
                      onclick={() => act(() => actions.next(phase.id, seat.seat))}>Next free</Button
                    >
                  {/if}
                </th>
              {/each}
            </tr>
          </thead>
          <tbody>
            {#each heats as heat (heat.heat)}
              <tr aria-label={heatName(heat)} data-state={heat.state}>
                <th scope="row">
                  <span class="heat-no">{heat.number}</span>
                  {#if heatStateWords(heat)}
                    <Badge tone={heat.state === 'Racing' ? 'success' : 'neutral'}>
                      {heatStateWords(heat)}
                    </Badge>
                  {/if}
                </th>
                {#each heat.seats as on, index (index)}
                  {@const seat = seats[index]}
                  {@const action = me ? seatAction(signup, heat, index, me) : undefined}
                  <td class:mine={me !== undefined && on === me}>
                    {#if action?.kind === 'take' && actions}
                      <Button
                        variant="primary"
                        disabled={busy}
                        aria-label="Take {seat.name} in {heatName(heat)}"
                        onclick={() => act(() => actions.take(heat.heat, seat.seat))}>Take</Button
                      >
                    {:else if on !== null && on !== undefined}
                      <span class="who">{names.pilot(on)}</span>
                      {#if action?.kind === 'leave' && actions}
                        <Button
                          variant="secondary"
                          disabled={busy}
                          aria-label="Leave {heatName(heat)}"
                          onclick={() => act(() => actions.leave(heat.heat))}>Leave</Button
                        >
                      {/if}
                    {:else}
                      <span class="free">Free</span>
                      {#if action?.kind === 'blocked'}
                        <span class="muted why">{action.why}</span>
                      {/if}
                    {/if}
                  </td>
                {/each}
              </tr>
            {/each}
          </tbody>
        </table>
      {:else}
        <ol class="heats">
          {#each heats as heat (heat.heat)}
            <li aria-label={heatName(heat)} data-state={heat.state}>
              <div class="heat-head">
                <strong>Heat {heat.number}</strong>
                {#if heatStateWords(heat)}
                  <Badge tone={heat.state === 'Racing' ? 'success' : 'neutral'}>
                    {heatStateWords(heat)}
                  </Badge>
                {/if}
              </div>
              <ul>
                {#each heat.seats as on, index (index)}
                  {@const seat = seats[index]}
                  {@const action = me ? seatAction(signup, heat, index, me) : undefined}
                  <li class:mine={me !== undefined && on === me}>
                    <span class="seat">
                      {seat.name}
                      {#if seat.channel}<span class="channel">{seat.channel}</span>{/if}
                    </span>
                    <span class="who">
                      {#if on !== null && on !== undefined}
                        {names.pilot(on)}
                      {:else}
                        <span class="free">Free</span>
                      {/if}
                    </span>
                    <span class="do">
                      {#if action?.kind === 'take' && actions}
                        <Button
                          size="sm"
                          variant="primary"
                          disabled={busy}
                          aria-label="Take {seat.name} in {heatName(heat)}"
                          onclick={() => act(() => actions.take(heat.heat, seat.seat))}>Take</Button
                        >
                      {:else if action?.kind === 'leave' && actions}
                        <Button
                          size="sm"
                          variant="secondary"
                          disabled={busy}
                          aria-label="Leave {heatName(heat)}"
                          onclick={() => act(() => actions.leave(heat.heat))}>Leave</Button
                        >
                      {:else if action?.kind === 'mine'}
                        <span class="muted">You</span>
                      {:else if action?.kind === 'blocked'}
                        <span class="muted why">{action.why}</span>
                      {/if}
                    </span>
                  </li>
                {/each}
              </ul>
            </li>
          {/each}
        </ol>
      {/if}
    </section>
  {/each}
</div>

<style>
  /* The kiosk grid: read standing up, pressed with a thumb. */
  table {
    width: 100%;
    border-collapse: collapse;
    table-layout: fixed;
    margin-top: var(--gf-space-3);
  }
  th,
  td {
    border: 1px solid var(--gf-border-subtle);
    padding: var(--gf-space-2) var(--gf-space-3);
    text-align: center;
    vertical-align: middle;
    height: 4.25rem;
  }
  thead th {
    height: auto;
    font-weight: var(--gf-font-weight-semibold);
  }
  thead th :global(button) {
    display: block;
    margin: var(--gf-space-1) auto 0;
  }
  thead th:first-child,
  tbody th {
    width: 7rem;
  }
  .seat-name {
    display: block;
  }
  .heat-no {
    font-size: var(--gf-font-size-xl);
    font-weight: var(--gf-font-weight-bold);
    margin-right: var(--gf-space-1);
  }
  td :global(button) {
    min-width: 6rem;
    min-height: 2.75rem;
    font-size: var(--gf-font-size-lg);
  }
  td .who {
    display: block;
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-semibold);
  }
  td.mine {
    outline: 2px solid var(--gf-accent);
    outline-offset: -2px;
  }
  td.mine .who {
    color: var(--gf-accent);
  }
  td .why {
    display: block;
  }
  tr[data-state='Flown'] {
    opacity: 0.55;
  }
  [data-layout='grid'] h2 {
    font-size: var(--gf-font-size-xl);
  }
  [data-layout='grid'] .runs {
    font-size: var(--gf-font-size-lg);
  }
  .signup {
    display: grid;
    gap: var(--gf-space-4);
  }
  header {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
  }
  h2 {
    margin: 0;
    font-size: var(--gf-font-size-lg);
    overflow-wrap: anywhere;
  }
  .muted {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .runs {
    margin: var(--gf-space-1) 0 0;
    font-size: var(--gf-font-size-sm);
  }
  .error {
    margin: 0;
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-danger);
    border-radius: var(--gf-radius-sm);
    color: var(--gf-danger);
    font-size: var(--gf-font-size-sm);
  }
  .next {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
    margin: var(--gf-space-2) 0;
  }
  .heats {
    list-style: none;
    margin: var(--gf-space-2) 0 0;
    padding: 0;
    display: grid;
    gap: var(--gf-space-2);
  }
  .heats > li {
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-sm);
    padding: var(--gf-space-2);
  }
  .heats > li[data-state='Flown'] {
    opacity: 0.6;
  }
  .heat-head {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    margin-bottom: var(--gf-space-1);
  }
  .heats ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--gf-space-1);
    font-size: var(--gf-font-size-sm);
  }
  .heats ul li {
    display: grid;
    grid-template-columns: minmax(5rem, auto) 1fr auto;
    align-items: center;
    gap: var(--gf-space-2);
    min-height: 2rem;
  }
  .seat {
    color: var(--gf-text-muted);
  }
  .channel {
    display: block;
    font-family: var(--gf-font-mono);
    font-size: var(--gf-font-size-xs);
  }
  .who {
    overflow-wrap: anywhere;
  }
  .free {
    color: var(--gf-text-muted);
  }
  li.mine .who {
    color: var(--gf-accent);
    font-weight: var(--gf-font-weight-semibold);
  }
  .why {
    font-size: var(--gf-font-size-xs);
  }
</style>
