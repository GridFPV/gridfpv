<script lang="ts">
  /**
   * Marshaling (#55) — review and correct a heat's laps.
   *
   * The RD reviews the per-competitor laps (from a `LapList` projection, falling back
   * to the live progress) and applies adjudications: void a detection, insert a lap,
   * adjust a lap's time, void the whole heat, or apply a penalty. Each emits the
   * matching marshaling `Command`; the server appends the event and the projection
   * re-folds, pushing a fresh value back on the read stream — so the corrected result
   * arrives by the same path the RD is already watching, nothing reconciled locally
   * (architecture.html §3). Destructive actions (void heat) confirm first.
   */
  import type { AdapterId, CompetitorRef, HeatId, LapList } from '@gridfpv/types';
  import { formatMicros } from '@gridfpv/components';
  import {
    adjustLapCommand,
    applyPenaltyCommand,
    DISQUALIFY,
    insertLapCommand,
    secondsToSourceTime,
    timeAddedPenalty,
    voidDetectionCommand,
    voidHeatCommand
  } from '../lib/marshaling.js';
  import type { Session } from '../lib/session.svelte.js';
  import ConfirmButton from '../lib/ConfirmButton.svelte';
  import ErrorBanner from '../lib/ErrorBanner.svelte';

  let {
    session,
    laps = undefined,
    adapter = 'rh-1'
  }: { session: Session; laps?: LapList; adapter?: AdapterId } = $props();

  const heat = $derived<HeatId | undefined>(session.liveState?.current_heat);
  // Competitors to act on: those in the lap list, else the live lineup.
  const competitors = $derived<CompetitorRef[]>(
    laps
      ? laps.competitors.map((c) => c.competitor.competitor)
      : (session.liveState?.active_pilots ?? [])
  );

  // ── Insert-lap form ──
  let insertRef = $state('');
  let insertAt = $state(0);
  async function doInsert() {
    if (!insertRef) return;
    await session.send(insertLapCommand(adapter, insertRef, secondsToSourceTime(insertAt)));
  }

  // ── Void / adjust a logged detection by its log offset ──
  let target = $state(0);
  let adjustAt = $state(0);
  async function doVoidDetection() {
    await session.send(voidDetectionCommand(Math.trunc(target)));
  }
  async function doAdjust() {
    await session.send(adjustLapCommand(Math.trunc(target), secondsToSourceTime(adjustAt)));
  }

  // ── Penalty form ──
  let penaltyRef = $state('');
  let penaltyKind = $state<'dq' | 'time'>('time');
  let penaltySeconds = $state(2);
  async function doPenalty() {
    if (!heat || !penaltyRef) return;
    const penalty = penaltyKind === 'dq' ? DISQUALIFY : timeAddedPenalty(penaltySeconds);
    await session.send(applyPenaltyCommand(heat, penaltyRef, penalty));
  }

  // ── Void the whole heat ──
  async function doVoidHeat() {
    if (!heat) return;
    await session.send(voidHeatCommand(heat));
  }
</script>

<section class="marshaling" aria-label="Marshaling">
  <header>
    <h2>
      Marshaling{#if heat}<span class="heat"> — {heat}</span>{/if}
    </h2>
    <p class="muted">Review and correct the laps. Corrections re-fold the result live.</p>
  </header>

  {#if session.lastCommandError}
    <ErrorBanner error={session.lastCommandError} ondismiss={() => session.clearCommandError()} />
  {/if}

  {#if laps}
    <div class="laps">
      {#each laps.competitors as cl (cl.competitor.competitor)}
        <div class="comp">
          <h4>{cl.competitor.competitor}</h4>
          <ol>
            {#each cl.laps as lap (lap.number)}
              <li>Lap {lap.number}: {formatMicros(lap.duration_micros)}</li>
            {/each}
          </ol>
        </div>
      {/each}
    </div>
  {:else}
    <p class="empty">No lap list loaded; act on the live lineup below.</p>
  {/if}

  <div class="actions">
    <fieldset>
      <legend>Insert a missed lap</legend>
      <label
        >Competitor
        <select bind:value={insertRef} aria-label="Insert competitor">
          <option value="" disabled>—</option>
          {#each competitors as c (c)}<option value={c}>{c}</option>{/each}
        </select>
      </label>
      <label>At (s) <input type="number" step="0.001" bind:value={insertAt} /></label>
      <button type="button" onclick={doInsert} disabled={!insertRef}>Insert lap</button>
    </fieldset>

    <fieldset>
      <legend>Void / adjust a detection</legend>
      <label>Log offset <input type="number" step="1" bind:value={target} /></label>
      <button type="button" onclick={doVoidDetection}>Void detection</button>
      <label>New time (s) <input type="number" step="0.001" bind:value={adjustAt} /></label>
      <button type="button" onclick={doAdjust}>Adjust lap</button>
    </fieldset>

    <fieldset>
      <legend>Apply a penalty</legend>
      <label
        >Competitor
        <select bind:value={penaltyRef} aria-label="Penalty competitor">
          <option value="" disabled>—</option>
          {#each competitors as c (c)}<option value={c}>{c}</option>{/each}
        </select>
      </label>
      <label
        >Kind
        <select bind:value={penaltyKind} aria-label="Penalty kind">
          <option value="time">Time added</option>
          <option value="dq">Disqualify</option>
        </select>
      </label>
      {#if penaltyKind === 'time'}
        <label>Seconds <input type="number" step="0.1" bind:value={penaltySeconds} /></label>
      {/if}
      <button type="button" onclick={doPenalty} disabled={!penaltyRef || !heat}
        >Apply penalty</button
      >
    </fieldset>

    <fieldset class="danger-zone">
      <legend>Void the heat</legend>
      <p class="muted">Throws out the whole heat — it will not count.</p>
      <ConfirmButton onconfirm={doVoidHeat} variant="danger" disabled={!heat}>
        Void heat
      </ConfirmButton>
    </fieldset>
  </div>
</section>

<style>
  .marshaling {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  h2 {
    font-size: var(--gf-font-size-lg);
    margin: 0;
  }
  .heat {
    color: var(--gf-color-text-muted);
    font-weight: var(--gf-font-weight-normal);
  }
  .muted {
    color: var(--gf-color-text-muted);
    font-size: var(--gf-font-size-sm);
    margin: var(--gf-space-1) 0 0;
  }
  .laps {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-6);
  }
  .comp h4 {
    margin: 0 0 var(--gf-space-2);
    font-size: var(--gf-font-size-md);
  }
  .comp ol {
    margin: 0;
    padding-left: var(--gf-space-6);
    font-size: var(--gf-font-size-sm);
  }
  .actions {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--gf-space-4);
  }
  fieldset {
    border: 1px solid var(--gf-color-border);
    border-radius: var(--gf-radius-md);
    padding: var(--gf-space-3) var(--gf-space-4);
    display: flex;
    flex-wrap: wrap;
    align-items: end;
    gap: var(--gf-space-3);
  }
  legend {
    font-weight: var(--gf-font-weight-bold);
    font-size: var(--gf-font-size-sm);
  }
  label {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
    font-size: var(--gf-font-size-xs);
    color: var(--gf-color-text-muted);
  }
  input,
  select {
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    padding: var(--gf-space-1) var(--gf-space-2);
    border: 1px solid var(--gf-color-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-color-surface);
    color: var(--gf-color-text);
  }
  button {
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    padding: var(--gf-space-2) var(--gf-space-4);
    border-radius: var(--gf-radius-sm);
    border: 1px solid var(--gf-color-border);
    background: var(--gf-color-surface);
    color: var(--gf-color-text);
    cursor: pointer;
  }
  button:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  .danger-zone {
    border-color: var(--gf-color-danger);
  }
  @media (max-width: 60rem) {
    .actions {
      grid-template-columns: 1fr;
    }
  }
</style>
