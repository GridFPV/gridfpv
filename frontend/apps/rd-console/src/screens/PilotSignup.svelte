<script lang="ts">
  /**
   * PilotSignup — the sign-up sheet on a phone (docs/primitives.md §11.5).
   *
   * Everyone sees the heats and who is on each seat. A pilot who is signed in can also
   * take a free seat, give theirs up, or take the next free spot on a seat. A seat they
   * cannot take right now says why instead of offering a button.
   *
   * The page only offers what the sheet says will work. If the Director refuses anyway
   * (another pilot got the seat first), its reason is shown and nothing changes.
   */
  import { Badge, Button } from '@gridfpv/components';
  import type {
    PhaseId,
    PublicPilot,
    SeatChannel,
    SignupHeatView,
    SignupSeat
  } from '@gridfpv/types';
  import type { Names } from '../lib/names.js';
  import {
    hasNextFree,
    heatStateWords,
    runsOf,
    runsWords,
    seatAction,
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
    channels = [],
    actions = undefined
  }: {
    /** The phases with sign-up, each with its sheet. */
    phases: readonly SignupPhase[];
    names: Names;
    /** The pilot this phone is signed in as. Left out for someone who is only looking. */
    pilot?: PublicPilot | undefined;
    channels?: readonly SeatChannel[];
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

<div class="signup">
  {#if error}
    <p class="error" role="alert" data-testid="signup-error">{error}</p>
  {/if}
  {#if phases.length === 0}
    <p class="muted">There is nothing to sign up for right now.</p>
  {/if}

  {#each phases as phase (phase.id)}
    {@const signup = phase.signup}
    {@const seats = seatHeadings(signup.rules, names, channels)}
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

      {#if me && actions}
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

      <ol class="heats">
        {#each signup.heats as heat (heat.heat)}
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
    </section>
  {/each}
</div>

<style>
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
