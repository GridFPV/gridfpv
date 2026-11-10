<script lang="ts">
  /**
   * PhaseList — every phase of the event as a tree: a phase that holds phases, with its
   * heats indented beneath it. Each row shows the phase's name and where it stands. A heat
   * also shows its lineup, and a button that opens its Run view.
   *
   * The rulings on a heat or a phase are made here too, each one a person's decision sent to
   * the Director as a fact:
   *
   * - **Lock** makes a phase's results official; **Unlock** is offered on the phase that
   *   carries the lock. A locked phase shows its ranking but takes no ruling.
   * - **Reopen heat** undoes the director calling a heat done.
   * - Under "Ranking": **Set position** for a pilot in a heat (and **Clear position**; a
   *   position set by hand carries a marker), and **Disqualify** / **Reinstate** a pilot in
   *   any phase.
   * - A pilot who reached a phase's false start limit is shown in a notice with a Disqualify
   *   button. Grid only proposes it.
   *
   * A refusal is shown in the Director's own words.
   */
  import { Badge, Banner, Button } from '@gridfpv/components';
  import type {
    Command,
    EventView,
    PhaseId,
    PhaseView,
    PilotId,
    ProtocolError
  } from '@gridfpv/types';
  import ErrorBanner from '../lib/ErrorBanner.svelte';
  import { toProtocolError, type EventLive } from '../lib/eventLive.svelte.js';
  import type { Names } from '../lib/names.js';
  import { hasRanking, openProposals, positionChoices, proposalSentence } from '../lib/rulings.js';
  import DisqualifyDialog from './DisqualifyDialog.svelte';
  import SetPositionDialog from './SetPositionDialog.svelte';
  import {
    isHeat,
    PHASE_STATUS_LABELS,
    phaseStatus,
    phaseTree,
    racesOfHeat,
    type PhaseStatus
  } from '../lib/phases.js';

  let {
    view,
    names,
    live,
    onrun
  }: {
    view: EventView;
    names: Names;
    /** The live event, to send rulings through. */
    live: Pick<EventLive, 'send'>;
    /** Open the Run view for a heat. */
    onrun: (heat: PhaseView['def']['id']) => void;
  } = $props();

  const rows = $derived(phaseTree(view.phases));

  const proposals = $derived(openProposals(view));

  let busy = $state(false);
  let error = $state<ProtocolError | undefined>(undefined);
  /** The pilot and phase the disqualify dialog is open for. */
  let disqualifying = $state<{ phase: PhaseId; pilot: PilotId } | undefined>(undefined);
  /** The pilot and heat the set-position dialog is open for. */
  let positioning = $state<{ heat: PhaseView; pilot: PilotId; current: number } | undefined>(
    undefined
  );

  /** Send one ruling. A refusal lands in the banner in the Director's words. */
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

  // The dialogs show a failure themselves, so these are not caught here.
  async function disqualify(reason: string | undefined, keepPlaceEmpty: boolean) {
    if (!disqualifying) return;
    const done = await live.send({
      Record: {
        Disqualified: {
          phase: disqualifying.phase,
          pilot: disqualifying.pilot,
          ...(reason === undefined ? {} : { reason }),
          keep_place_empty: keepPlaceEmpty
        }
      }
    });
    if (done) disqualifying = undefined;
  }

  async function setPosition(position: number, note: string | undefined) {
    if (!positioning) return;
    const done = await live.send({
      Record: {
        PositionSet: {
          heat: positioning.heat.def.id,
          pilot: positioning.pilot,
          position,
          ...(note === undefined ? {} : { note })
        }
      }
    });
    if (done) positioning = undefined;
  }

  const TONES: Record<PhaseStatus, 'neutral' | 'accent' | 'success' | 'warn' | 'info'> = {
    waiting: 'neutral',
    ready: 'accent',
    racing: 'warn',
    complete: 'success',
    locked: 'info'
  };
</script>

<ErrorBanner {error} ondismiss={() => (error = undefined)} />

{#each proposals as proposal (`${proposal.phase}/${proposal.pilot}`)}
  <Banner tone="warn" title={proposalSentence(names, proposal)}>
    Nothing happens unless you decide it.
    {#snippet actions()}
      <Button
        size="sm"
        variant="danger"
        aria-label="Disqualify {names.pilot(proposal.pilot)} from {names.phase(proposal.phase)}"
        onclick={() => (disqualifying = { phase: proposal.phase, pilot: proposal.pilot })}
        >Disqualify</Button
      >
    {/snippet}
  </Banner>
{/each}

<ul class="phases" aria-label="Phases">
  {#each rows as { phase, depth } (phase.def.id)}
    {@const status = phaseStatus(phase, view)}
    {@const heat = isHeat(phase.def)}
    <li class="phase" class:heat style="--depth: {depth}">
      <div class="head">
        <span class="label">{phase.def.label}</span>
        <Badge tone={TONES[status]} dot={status === 'racing'}>{PHASE_STATUS_LABELS[status]}</Badge>
        <span class="action">
          {#if phase.locked_here}
            <Button
              size="sm"
              variant="ghost"
              disabled={busy}
              aria-label="Unlock {phase.def.label}"
              onclick={() => act({ Record: { PhaseUnlocked: { phase: phase.def.id } } })}
              >Unlock</Button
            >
          {:else if !phase.locked}
            {#if heat && phase.director_done}
              <Button
                size="sm"
                variant="ghost"
                disabled={busy}
                aria-label="Reopen {phase.def.label}"
                onclick={() => act({ Record: { HeatReopened: { heat: phase.def.id } } })}
                >Reopen heat</Button
              >
            {/if}
            <Button
              size="sm"
              variant="ghost"
              disabled={busy}
              title="Make the results official. Rulings are refused until it is unlocked."
              aria-label="Lock {phase.def.label}"
              onclick={() => act({ Record: { PhaseLocked: { phase: phase.def.id } } })}>Lock</Button
            >
          {/if}
          {#if heat}
            {#if status === 'ready' || status === 'racing'}
              <Button
                size="sm"
                variant="primary"
                aria-label="Run {phase.def.label}"
                onclick={() => onrun(phase.def.id)}>Run</Button
              >
            {:else if racesOfHeat(view, phase.def.id).length > 0 || status === 'complete'}
              <Button
                size="sm"
                variant="ghost"
                aria-label="View {phase.def.label}"
                onclick={() => onrun(phase.def.id)}>View</Button
              >
            {/if}
          {/if}
        </span>
      </div>
      {#if heat && phase.lineup.length > 0}
        <ol class="lineup" aria-label="Lineup of {phase.def.label}">
          {#each phase.lineup as seat, index (index)}
            <li class:pending={typeof seat.who === 'string' || 'Waiting' in seat.who}>
              <span class="seat">{names.seat(seat.adapter, seat.seat, index)}</span>
              <span class="who">{names.who(seat.who)}</span>
            </li>
          {/each}
        </ol>
      {/if}
      {#if hasRanking(phase)}
        <details class="rulings">
          <summary aria-label="Ranking of {phase.def.label}">Ranking</summary>
          {#if phase.locked}
            <p class="locked-note">
              Locked. The results are official and take no ruling until
              {phase.locked_here ? 'it is' : 'the phase above is'} unlocked.
            </p>
          {/if}
          <ol class="ranking" aria-label="Ranking of {phase.def.label}">
            {#each phase.ranking as placed (placed.pilot)}
              {@const callsign = names.pilot(placed.pilot)}
              <li>
                <span class="place">{placed.position}</span>
                <span class="who">{callsign}</span>
                {#if placed.position_set}
                  <span title="The director set this position by hand.">
                    <Badge tone="violet">Set by hand</Badge>
                  </span>
                {/if}
                {#if placed.disqualified}<Badge tone="danger">Disqualified</Badge>{/if}
                {#if !placed.flew}<Badge tone="neutral">Did not fly</Badge>{/if}
                {#if !phase.locked}
                  <span class="rank-actions">
                    {#if heat}
                      <Button
                        size="sm"
                        variant="ghost"
                        disabled={busy}
                        aria-label="Set the position of {callsign} in {phase.def.label}"
                        onclick={() =>
                          (positioning = {
                            heat: phase,
                            pilot: placed.pilot,
                            current: placed.position
                          })}>Set position</Button
                      >
                      {#if placed.position_set}
                        <Button
                          size="sm"
                          variant="ghost"
                          disabled={busy}
                          aria-label="Clear the position of {callsign} in {phase.def.label}"
                          onclick={() =>
                            act({
                              Record: {
                                PositionCleared: { heat: phase.def.id, pilot: placed.pilot }
                              }
                            })}>Clear position</Button
                        >
                      {/if}
                    {/if}
                    {#if placed.disqualified}
                      <Button
                        size="sm"
                        variant="ghost"
                        disabled={busy}
                        aria-label="Reinstate {callsign} in {phase.def.label}"
                        onclick={() =>
                          act({
                            Record: { Reinstated: { phase: phase.def.id, pilot: placed.pilot } }
                          })}>Reinstate</Button
                      >
                    {:else}
                      <Button
                        size="sm"
                        variant="ghost"
                        disabled={busy}
                        aria-label="Disqualify {callsign} from {phase.def.label}"
                        onclick={() =>
                          (disqualifying = { phase: phase.def.id, pilot: placed.pilot })}
                        >Disqualify</Button
                      >
                    {/if}
                  </span>
                {/if}
              </li>
            {/each}
          </ol>
        </details>
      {/if}
    </li>
  {/each}
</ul>

{#if disqualifying}
  <DisqualifyDialog
    callsign={names.pilot(disqualifying.pilot)}
    phaseLabel={names.phase(disqualifying.phase)}
    ondisqualify={disqualify}
    onclose={() => (disqualifying = undefined)}
  />
{/if}
{#if positioning}
  <SetPositionDialog
    callsign={names.pilot(positioning.pilot)}
    heatLabel={positioning.heat.def.label}
    positions={positionChoices(positioning.heat)}
    current={positioning.current}
    onset={setPosition}
    onclose={() => (positioning = undefined)}
  />
{/if}

<style>
  .phases {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .phase {
    margin-left: calc(var(--depth) * var(--gf-space-6));
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-border-subtle);
    border-left: 3px solid var(--gf-border-strong);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .phase.heat {
    border-left-color: var(--gf-accent);
    background: var(--gf-surface);
  }
  .head {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
  }
  .label {
    font-weight: var(--gf-font-weight-semibold);
    overflow-wrap: anywhere;
  }
  .action {
    margin-left: auto;
    display: flex;
    gap: var(--gf-space-1);
    align-items: center;
  }
  .rulings summary {
    cursor: pointer;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-secondary);
  }
  .locked-note {
    margin: var(--gf-space-2) 0 0;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .ranking {
    list-style: none;
    margin: var(--gf-space-2) 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
    font-size: var(--gf-font-size-sm);
  }
  .ranking li {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
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
  .rank-actions {
    margin-left: auto;
    display: flex;
    gap: var(--gf-space-1);
    flex-wrap: wrap;
  }
  .lineup {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-1) var(--gf-space-4);
    font-size: var(--gf-font-size-sm);
  }
  .lineup li {
    display: flex;
    gap: var(--gf-space-2);
    align-items: baseline;
  }
  .seat {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .pending .who {
    color: var(--gf-text-muted);
    font-style: italic;
  }
  @media (max-width: 36rem) {
    .phase {
      margin-left: calc(var(--depth) * var(--gf-space-3));
    }
  }
</style>
