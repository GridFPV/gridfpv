<script lang="ts">
  /**
   * Marshaling (#55, Slice 3) — the timer-agnostic lap-level review surface.
   *
   * The RD reviews each competitor's laps (from the heat's `LapList` projection, marshaling already
   * folded in) and corrects them by **selecting a lap** and acting on it — never by hand-typing a log
   * offset. Each lap carries the global `start_ref`/`end_ref` offsets of its bounding passes, so a
   * selected lap maps to the exact `LogRef` a correction command targets (void / split / edit-time on
   * the lap's end pass; insert-after between this lap and the next). Per-competitor rulings (DQ, time
   * penalty, reverse a ruling) act on the competitor.
   *
   * Every correction *appends* a marshaling event; the projection re-folds and the corrected lap list,
   * the audit trail, AND the standings (the live stream) all refresh by the same append→re-fold→re-read
   * path — nothing reconciled locally (architecture.html §3). The **audit panel** renders that history,
   * reverse-chronological, derived purely from the event type — the "defensible results" theme made
   * visible. Mutating controls are **role-gated**: a read-only-pilot session sees the laps + audit but
   * every action is hidden (the Director enforces the boundary; this mirrors it).
   */
  import type {
    AuditEntry,
    AuditKind,
    CompetitorRef,
    HeatId,
    Lap,
    LapList,
    LogRef,
    SignalTraceView
  } from '@gridfpv/types';
  import { formatMicros } from '@gridfpv/components';
  import {
    adjustLapCommand,
    applyPenaltyCommand,
    DISQUALIFY,
    insertLapCommand,
    reverseRulingCommand,
    secondsToSourceTime,
    splitLapCommand,
    timeAddedPenalty,
    voidDetectionCommand,
    voidHeatCommand
  } from '../lib/marshaling.js';
  import type { Session } from '../lib/session.svelte.js';
  import ConfirmButton from '../lib/ConfirmButton.svelte';
  import ErrorBanner from '../lib/ErrorBanner.svelte';
  import RssiGraph from '../lib/RssiGraph.svelte';

  let { session, adapter = 'rh-1' }: { session: Session; adapter?: string } = $props();

  const heat = $derived<HeatId | undefined>(session.liveState?.current_heat);
  const laps = $derived<LapList | undefined>(session.lapList);
  const audit = $derived<AuditEntry[] | undefined>(session.marshalingAudit);
  const canControl = $derived(session.canControl);

  // The captured RSSI trace for this heat (`?projection=signal`, Slice 1), pulled alongside the
  // lap list + audit by `refreshMarshaling`. A heat that captured signal (a RotorHazard heat) has
  // one or more competitor traces; a **sim heat** has none — `hasTrace` is then false and the
  // signal-as-evidence graph is skipped, leaving today's lap-only layout (marshaling.html §3.2).
  const signalTrace = $derived<SignalTraceView | undefined>(session.signalTrace);
  const hasTrace = $derived((signalTrace?.competitors.length ?? 0) > 0);

  // Drive the marshaling reads off the live stream: whenever the current heat (or the stream's
  // cursor — a new appended event, e.g. a correction we or another client made) changes, re-pull
  // the heat's lap list + audit so corrections flow in live.
  let lastKey = $state<string | undefined>(undefined);
  $effect(() => {
    const h = heat;
    const cursor = session.protocolState?.cursor;
    if (!h) return;
    const key = `${h}:${cursor ?? ''}`;
    if (key === lastKey) return;
    lastKey = key;
    void session.refreshMarshaling(h);
  });

  // ── Lap selection ──
  // A selection identifies a competitor + a lap by its end_ref (the lap's stable identity / the
  // natural correction target). `null` when nothing is selected.
  let selected = $state<{ competitor: CompetitorRef; lap: Lap } | null>(null);

  function selectLap(competitor: CompetitorRef, lap: Lap): void {
    if (selected && selected.competitor === competitor && selected.lap.end_ref === lap.end_ref) {
      selected = null; // toggle off
    } else {
      selected = { competitor, lap };
    }
  }

  function isSelected(competitor: CompetitorRef, lap: Lap): boolean {
    return (
      selected !== null &&
      selected.competitor === competitor &&
      selected.lap.end_ref === lap.end_ref
    );
  }

  // Clear a stale selection if the lap it pointed at no longer exists after a re-fold.
  $effect(() => {
    if (!selected || !laps) return;
    const sel = selected;
    const cl = laps.competitors.find((c) => c.competitor.competitor === sel.competitor);
    const stillThere = cl?.laps.some((l) => l.end_ref === sel.lap.end_ref);
    if (!stillThere) selected = null;
  });

  // ── Inline corrections on the SELECTED lap ──
  // Edit-time / split / insert-after take a time input (seconds, source clock).
  let editSeconds = $state(0);

  async function afterCorrection(): Promise<void> {
    // The send already recorded any error; on success the stream cursor advances and the
    // $effect re-pulls. Re-pull immediately too so the panel updates without waiting for the
    // next stream tick (idempotent — a fresh snapshot).
    if (heat) await session.refreshMarshaling(heat);
  }

  async function doVoidSelected(): Promise<void> {
    if (!selected) return;
    const target: LogRef = selected.lap.end_ref;
    const ack = await session.send(voidDetectionCommand(target));
    if (ack.ok) {
      selected = null;
      await afterCorrection();
    }
  }

  async function doSplitSelected(): Promise<void> {
    if (!selected) return;
    const ack = await session.send(
      splitLapCommand(selected.lap.end_ref, secondsToSourceTime(editSeconds))
    );
    if (ack.ok) await afterCorrection();
  }

  async function doEditTimeSelected(): Promise<void> {
    if (!selected) return;
    const ack = await session.send(
      adjustLapCommand(selected.lap.end_ref, secondsToSourceTime(editSeconds))
    );
    if (ack.ok) await afterCorrection();
  }

  async function doInsertAfterSelected(): Promise<void> {
    if (!selected) return;
    const ack = await session.send(
      insertLapCommand(adapter, selected.competitor, secondsToSourceTime(editSeconds))
    );
    if (ack.ok) await afterCorrection();
  }

  // ── Per-competitor rulings ──
  let penaltyTarget = $state<CompetitorRef | ''>('');
  let penaltyKind = $state<'dq' | 'time'>('dq');
  let penaltySeconds = $state(2);
  async function doPenalty(): Promise<void> {
    if (!heat || !penaltyTarget) return;
    const penalty = penaltyKind === 'dq' ? DISQUALIFY : timeAddedPenalty(penaltySeconds);
    const ack = await session.send(applyPenaltyCommand(heat, penaltyTarget, penalty));
    if (ack.ok) await afterCorrection();
  }

  // Reverse a prior reversible ruling (a penalty) selected from the audit trail.
  const reversiblePenalties = $derived((audit ?? []).filter((e) => e.kind === 'PenaltyApplied'));
  let reverseTargetRef = $state<number | ''>('');
  async function doReverse(): Promise<void> {
    if (reverseTargetRef === '') return;
    const ack = await session.send(reverseRulingCommand(reverseTargetRef as LogRef));
    if (ack.ok) {
      reverseTargetRef = '';
      await afterCorrection();
    }
  }

  async function doVoidHeat(): Promise<void> {
    if (!heat) return;
    const ack = await session.send(voidHeatCommand(heat));
    if (ack.ok) await afterCorrection();
  }

  // Competitors that can be acted on: those in the lap list, else the live lineup.
  const competitors = $derived<CompetitorRef[]>(
    laps && laps.competitors.length > 0
      ? laps.competitors.map((c) => c.competitor.competitor)
      : (session.liveState?.active_pilots ?? [])
  );

  // ── Audit rendering helpers ──
  function auditLabel(kind: AuditKind): string {
    switch (kind) {
      case 'Voided':
        return 'Voided';
      case 'Inserted':
        return 'Inserted';
      case 'Adjusted':
        return 'Re-timed';
      case 'Split':
        return 'Split';
      case 'PenaltyApplied':
        return 'Penalty';
      case 'RulingReversed':
        return 'Reversed';
      case 'HeatVoided':
        return 'Heat voided';
      case 'Pass':
        return 'Detection';
      default:
        return kind;
    }
  }

  function auditTime(at: number | null): string {
    if (at == null) return '';
    // `at` is microseconds since the Unix epoch (server recorded_at).
    return new Date(at / 1000).toLocaleTimeString();
  }
</script>

<section class="marshaling" aria-label="Marshaling">
  <header>
    <h2>
      Marshaling{#if heat}<span class="heat"> — {heat}</span>{/if}
    </h2>
    <p class="muted">
      {#if canControl}
        Select a lap to correct it. Every change is a recorded, reversible fact.
      {:else}
        Read-only — you can review the laps and audit trail, but not change them.
      {/if}
    </p>
  </header>

  {#if session.lastCommandError}
    <ErrorBanner error={session.lastCommandError} ondismiss={() => session.clearCommandError()} />
  {/if}

  <div class="layout">
    <div class="main">
      {#if hasTrace && signalTrace}
        <!-- Signal-as-evidence (Slice 4): the RSSI graph for heats that captured a trace. A marker
             click selects that lap in the action surface below; the lap-list selection highlights
             the same marker (two-way — `selectLap` is the one shared selection). Display only — no
             re-detection (marshaling.html §3.2/§5). Sim heats (no trace) skip this entirely. -->
        <RssiGraph trace={signalTrace} {laps} {selected} onselect={selectLap} />
      {/if}

      {#if laps && laps.competitors.length > 0}
        <div class="laps">
          {#each laps.competitors as cl (cl.competitor.competitor)}
            <div class="comp">
              <h4>{cl.competitor.competitor}</h4>
              {#if cl.laps.length === 0}
                <p class="empty">No laps yet.</p>
              {:else}
                <ol>
                  {#each cl.laps as lap (lap.end_ref)}
                    <li>
                      <button
                        type="button"
                        class="lap"
                        class:selected={isSelected(cl.competitor.competitor, lap)}
                        aria-pressed={isSelected(cl.competitor.competitor, lap)}
                        onclick={() => selectLap(cl.competitor.competitor, lap)}
                      >
                        <span class="lap-num">Lap {lap.number}</span>
                        <span class="lap-time">{formatMicros(lap.duration_micros)}</span>
                      </button>
                    </li>
                  {/each}
                </ol>
              {/if}
            </div>
          {/each}
        </div>
      {:else}
        <p class="empty">No lap list for this heat yet.</p>
      {/if}

      {#if canControl}
        <!-- Inline corrections on the selected lap -->
        <fieldset class="selection-actions" disabled={!selected}>
          <legend>
            {#if selected}
              Selected: {selected.competitor} · Lap {selected.lap.number}
            {:else}
              Select a lap to correct
            {/if}
          </legend>
          <div class="row">
            <button type="button" onclick={doVoidSelected} disabled={!selected}
              >Remove (void)</button
            >
            <label class="time"
              >Time (s)
              <input
                type="number"
                step="0.001"
                bind:value={editSeconds}
                aria-label="Correction time"
              />
            </label>
            <button type="button" onclick={doSplitSelected} disabled={!selected}>Split</button>
            <button type="button" onclick={doEditTimeSelected} disabled={!selected}
              >Edit time</button
            >
            <button type="button" onclick={doInsertAfterSelected} disabled={!selected}
              >Insert after</button
            >
          </div>
        </fieldset>

        <!-- Per-competitor rulings -->
        <fieldset>
          <legend>Competitor ruling</legend>
          <div class="row">
            <label
              >Competitor
              <select bind:value={penaltyTarget} aria-label="Ruling competitor">
                <option value="" disabled>—</option>
                {#each competitors as c (c)}<option value={c}>{c}</option>{/each}
              </select>
            </label>
            <label
              >Kind
              <select bind:value={penaltyKind} aria-label="Penalty kind">
                <option value="dq">Disqualify</option>
                <option value="time">Time added</option>
              </select>
            </label>
            {#if penaltyKind === 'time'}
              <label>Seconds <input type="number" step="0.1" bind:value={penaltySeconds} /></label>
            {/if}
            <button type="button" onclick={doPenalty} disabled={!penaltyTarget || !heat}
              >Apply</button
            >
          </div>
          <div class="row">
            <label
              >Reverse a ruling
              <select bind:value={reverseTargetRef} aria-label="Reverse ruling">
                <option value="" disabled>—</option>
                {#each reversiblePenalties as p (p.at_ref)}
                  <option value={p.at_ref}>{p.summary}</option>
                {/each}
              </select>
            </label>
            <button
              type="button"
              onclick={doReverse}
              disabled={reverseTargetRef === '' || reversiblePenalties.length === 0}
              >Reverse ruling</button
            >
          </div>
        </fieldset>

        <fieldset class="danger-zone">
          <legend>Void the heat</legend>
          <p class="muted">Throws out the whole heat — it will not count.</p>
          <ConfirmButton onconfirm={doVoidHeat} variant="danger" disabled={!heat}>
            Void heat
          </ConfirmButton>
        </fieldset>
      {/if}
    </div>

    <!-- Audit panel: reverse-chronological "what changed, when, what kind" -->
    <aside class="audit" aria-label="Audit trail">
      <h3>Audit trail</h3>
      {#if audit && audit.length > 0}
        <ol class="audit-list">
          {#each audit as entry (entry.at_ref)}
            <li class="audit-entry kind-{entry.kind}">
              <span class="audit-kind">{auditLabel(entry.kind)}</span>
              <span class="audit-summary">{entry.summary}</span>
              {#if entry.at != null}
                <span class="audit-at">{auditTime(entry.at)}</span>
              {/if}
            </li>
          {/each}
        </ol>
      {:else}
        <p class="empty">No corrections yet — the raw timer output stands.</p>
      {/if}
    </aside>
  </div>
</section>

<style>
  .marshaling {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
  }
  h2 {
    font-size: var(--gf-font-size-xl);
    margin: 0;
    letter-spacing: var(--gf-tracking-tight);
  }
  .heat {
    color: var(--gf-text-muted);
    font-weight: var(--gf-font-weight-normal);
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    margin: var(--gf-space-1) 0 0;
  }
  .layout {
    display: grid;
    grid-template-columns: 2fr 1fr;
    gap: var(--gf-space-5);
    align-items: start;
  }
  .main {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .laps {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-4);
  }
  .comp {
    flex: 1 1 14rem;
    padding: var(--gf-space-4);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    background: var(--gf-elevated);
    box-shadow: var(--gf-shadow-xs);
  }
  .comp h4 {
    margin: 0 0 var(--gf-space-2);
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
  }
  .comp ol {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
  }
  .lap {
    width: 100%;
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: var(--gf-space-3);
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid transparent;
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    color: var(--gf-text-secondary);
    font-family: var(--gf-font-mono);
    font-size: var(--gf-font-size-sm);
    cursor: pointer;
    text-align: left;
  }
  .lap:hover {
    border-color: var(--gf-border-strong);
  }
  .lap.selected {
    border-color: var(--gf-accent);
    background: var(--gf-accent-soft);
    color: var(--gf-text);
  }
  .lap-num {
    font-weight: var(--gf-font-weight-semibold);
  }
  .empty {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  fieldset {
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    padding: var(--gf-space-4) var(--gf-space-5);
    background: var(--gf-elevated);
  }
  fieldset:disabled {
    opacity: 0.55;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: end;
    gap: var(--gf-space-3);
  }
  .row + .row {
    margin-top: var(--gf-space-3);
  }
  legend {
    font-weight: var(--gf-font-weight-semibold);
    font-size: var(--gf-font-size-sm);
    padding: 0 var(--gf-space-2);
  }
  label {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
    font-size: var(--gf-font-size-2xs);
    font-weight: var(--gf-font-weight-semibold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  input,
  select {
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-normal);
    text-transform: none;
    letter-spacing: normal;
    height: 2.1rem;
    padding: 0 var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    color: var(--gf-text);
  }
  input:focus,
  select:focus {
    outline: none;
    border-color: var(--gf-accent);
    box-shadow: 0 0 0 3px var(--gf-accent-soft);
  }
  button {
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
    height: 2.1rem;
    padding: 0 var(--gf-space-4);
    border-radius: var(--gf-radius-sm);
    border: 1px solid var(--gf-border);
    background: var(--gf-elevated);
    color: var(--gf-text);
    cursor: pointer;
    transition:
      background var(--gf-motion-fast) var(--gf-ease-out),
      border-color var(--gf-motion-fast) var(--gf-ease-out);
  }
  button:hover:not(:disabled) {
    background: var(--gf-elevated-hover);
    border-color: var(--gf-border-strong);
  }
  button:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
  button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .danger-zone {
    border-color: color-mix(in srgb, var(--gf-danger) 45%, var(--gf-border));
    background: var(--gf-danger-soft);
  }
  .audit {
    padding: var(--gf-space-4);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    background: var(--gf-elevated);
    box-shadow: var(--gf-shadow-xs);
    position: sticky;
    top: var(--gf-space-4);
  }
  .audit h3 {
    margin: 0 0 var(--gf-space-3);
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
  }
  .audit-list {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .audit-entry {
    display: grid;
    grid-template-columns: auto 1fr;
    grid-template-areas: 'kind summary' 'at at';
    gap: var(--gf-space-1) var(--gf-space-2);
    padding: var(--gf-space-2) var(--gf-space-3);
    border-left: 3px solid var(--gf-border-strong);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    font-size: var(--gf-font-size-sm);
  }
  .audit-entry.kind-Voided,
  .audit-entry.kind-HeatVoided {
    border-left-color: var(--gf-danger);
  }
  .audit-entry.kind-RulingReversed {
    border-left-color: var(--gf-accent);
  }
  .audit-kind {
    grid-area: kind;
    font-weight: var(--gf-font-weight-semibold);
    font-size: var(--gf-font-size-2xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .audit-summary {
    grid-area: summary;
    color: var(--gf-text);
  }
  .audit-at {
    grid-area: at;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-2xs);
    font-family: var(--gf-font-mono);
  }
  @media (max-width: 70rem) {
    .layout {
      grid-template-columns: 1fr;
    }
    .audit {
      position: static;
    }
  }
</style>
