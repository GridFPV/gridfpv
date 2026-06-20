<script lang="ts">
  /**
   * Setup wizard (#52) — a stepped form: event → class(es) → track → format/win-condition
   * → review. Produces a typed `EventConfig` held in app state. Where the contract
   * supports it the wizard drives the wire (`ScheduleHeat`); the rest is a local hold
   * until the server grows configuration commands (see `lib/setup.ts` for the gap).
   *
   * Progressive disclosure (clients.html §5): one step at a time, plain language, every
   * step revisitable (back/next), nothing destructive.
   */
  import {
    defaultClass,
    FORMAT_LABELS,
    defaultWinCondition,
    isConfigComplete,
    validateConfig,
    type EventConfig,
    type RaceFormat
  } from '../lib/setup.js';
  import type { WinCondition } from '@gridfpv/types';

  let {
    config = $bindable(),
    oncommit = undefined
  }: { config: EventConfig; oncommit?: (config: EventConfig) => void } = $props();

  const steps = ['Event', 'Classes', 'Track', 'Format', 'Review'] as const;
  let step = $state(0);

  const problems = $derived(validateConfig(config));
  const complete = $derived(isConfigComplete(config));

  function next() {
    if (step < steps.length - 1) step += 1;
  }
  function back() {
    if (step > 0) step -= 1;
  }

  function addClass() {
    const n = config.classes.length + 1;
    config.classes = [...config.classes, defaultClass(`class-${n}`, `Class ${n}`, 'timed-qual')];
  }
  function removeClass(i: number) {
    config.classes = config.classes.filter((_, j) => j !== i);
  }
  function setFormat(i: number, format: RaceFormat) {
    config.classes = config.classes.map((c, j) =>
      j === i ? { ...c, format, winCondition: defaultWinCondition(format) } : c
    );
  }

  // Win-condition is a discriminated union; expose the numeric knob for the chosen kind.
  function winConditionNumber(wc: WinCondition): number {
    if (typeof wc === 'object' && 'Timed' in wc)
      return Math.round(wc.Timed.window_micros / 1_000_000);
    if (typeof wc === 'object' && 'FirstToLaps' in wc) return wc.FirstToLaps.n;
    if (typeof wc === 'object' && 'BestConsecutive' in wc) return wc.BestConsecutive.n;
    return 0;
  }
  function winConditionLabel(wc: WinCondition): string {
    if (wc === 'BestLap') return 'Best single lap';
    if ('Timed' in wc) return 'Timed window (seconds)';
    if ('FirstToLaps' in wc) return 'First to N laps';
    return 'Best N consecutive laps';
  }
  function setWinConditionNumber(i: number, value: number) {
    config.classes = config.classes.map((c, j) => {
      if (j !== i) return c;
      const wc = c.winCondition;
      let next: WinCondition = wc;
      if (typeof wc === 'object' && 'Timed' in wc)
        next = { Timed: { window_micros: Math.round(value * 1_000_000) } };
      else if (typeof wc === 'object' && 'FirstToLaps' in wc)
        next = { FirstToLaps: { n: Math.max(1, Math.round(value)) } };
      else if (typeof wc === 'object' && 'BestConsecutive' in wc)
        next = { BestConsecutive: { n: Math.max(1, Math.round(value)) } };
      return { ...c, winCondition: next };
    });
  }

  function finish() {
    if (complete && oncommit) oncommit($state.snapshot(config) as EventConfig);
  }
</script>

<section class="wizard" aria-label="Setup wizard">
  <ol class="steps">
    {#each steps as s, i (s)}
      <li class:active={i === step} class:done={i < step}>
        <button type="button" onclick={() => (step = i)}>{i + 1}. {s}</button>
      </li>
    {/each}
  </ol>

  <div class="panel">
    {#if step === 0}
      <h3>Event</h3>
      <label
        >Name <input type="text" bind:value={config.eventName} placeholder="Spring Cup" /></label
      >
      <label>Id <input type="text" bind:value={config.eventId} placeholder="spring-cup" /></label>
    {:else if step === 1}
      <h3>Classes</h3>
      {#if config.classes.length === 0}
        <p class="muted">No classes yet.</p>
      {/if}
      {#each config.classes as cls, i (i)}
        <div class="class-row">
          <input type="text" bind:value={config.classes[i].name} aria-label="Class name" />
          <input type="text" bind:value={config.classes[i].id} aria-label="Class id" />
          <button
            type="button"
            class="remove"
            onclick={() => removeClass(i)}
            aria-label={`Remove ${cls.name}`}>Remove</button
          >
        </div>
      {/each}
      <button type="button" onclick={addClass}>Add class</button>
    {:else if step === 2}
      <h3>Track</h3>
      <label
        >Track / venue <input
          type="text"
          bind:value={config.track}
          placeholder="Main field"
        /></label
      >
    {:else if step === 3}
      <h3>Format &amp; win condition</h3>
      {#if config.classes.length === 0}
        <p class="muted">Add a class first.</p>
      {/if}
      {#each config.classes as cls, i (i)}
        <fieldset>
          <legend>{cls.name}</legend>
          <label
            >Format
            <select
              value={cls.format}
              onchange={(e) =>
                setFormat(i, (e.currentTarget as HTMLSelectElement).value as RaceFormat)}
            >
              {#each Object.entries(FORMAT_LABELS) as [val, label] (val)}
                <option value={val}>{label}</option>
              {/each}
            </select>
          </label>
          {#if cls.winCondition !== 'BestLap'}
            <label
              >{winConditionLabel(cls.winCondition)}
              <input
                type="number"
                step="1"
                value={winConditionNumber(cls.winCondition)}
                onchange={(e) =>
                  setWinConditionNumber(i, Number((e.currentTarget as HTMLInputElement).value))}
              />
            </label>
          {:else}
            <span class="muted">Best single lap.</span>
          {/if}
        </fieldset>
      {/each}
    {:else}
      <h3>Review</h3>
      <dl class="review">
        <dt>Event</dt>
        <dd>{config.eventName} <code>({config.eventId})</code></dd>
        <dt>Track</dt>
        <dd>{config.track}</dd>
        <dt>Classes</dt>
        <dd>
          <ul>
            {#each config.classes as cls (cls.id)}
              <li>{cls.name} — {FORMAT_LABELS[cls.format]}</li>
            {/each}
          </ul>
        </dd>
      </dl>
      {#if problems.length > 0}
        <ul class="problems">
          {#each problems as p (p)}<li>{p}</li>{/each}
        </ul>
      {/if}
      <button type="button" class="finish" onclick={finish} disabled={!complete}>
        Save configuration
      </button>
    {/if}
  </div>

  <div class="nav">
    <button type="button" onclick={back} disabled={step === 0}>Back</button>
    <button type="button" onclick={next} disabled={step === steps.length - 1}>Next</button>
  </div>
</section>

<style>
  .wizard {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
    max-width: 48rem;
  }
  .steps {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-2);
    list-style: none;
    margin: 0;
    padding: 0;
    counter-reset: step;
  }
  .steps li button {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    background: var(--gf-surface-alt);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-pill);
    padding: var(--gf-space-2) var(--gf-space-4);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
    cursor: pointer;
    transition:
      color var(--gf-motion-fast) var(--gf-ease-out),
      background var(--gf-motion-fast) var(--gf-ease-out),
      border-color var(--gf-motion-fast) var(--gf-ease-out);
  }
  .steps li button:hover {
    color: var(--gf-text);
  }
  .steps li.active button {
    border-color: var(--gf-accent);
    background: var(--gf-accent-soft);
    color: var(--gf-accent);
    font-weight: var(--gf-font-weight-semibold);
  }
  .steps li.done button {
    color: var(--gf-text-secondary);
    border-color: var(--gf-border);
  }
  .panel {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
    padding: var(--gf-space-6);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    background: var(--gf-elevated);
    box-shadow: var(--gf-shadow-xs);
  }
  h3 {
    margin: 0;
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
  }
  label {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
    font-size: var(--gf-font-size-xs);
    font-weight: var(--gf-font-weight-semibold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
    max-width: 24rem;
  }
  input,
  select {
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-normal);
    text-transform: none;
    letter-spacing: normal;
    height: 2.25rem;
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
  .class-row {
    display: flex;
    gap: var(--gf-space-2);
    align-items: center;
  }
  fieldset {
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    padding: var(--gf-space-4);
    display: flex;
    gap: var(--gf-space-4);
    align-items: end;
    flex-wrap: wrap;
  }
  legend {
    font-weight: var(--gf-font-weight-semibold);
    font-size: var(--gf-font-size-sm);
    padding: 0 var(--gf-space-2);
  }
  .review dt {
    font-weight: var(--gf-font-weight-semibold);
    font-size: var(--gf-font-size-2xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .review dd {
    margin: var(--gf-space-1) 0 var(--gf-space-3);
    font-size: var(--gf-font-size-sm);
  }
  .review code {
    color: var(--gf-text-muted);
    font-family: var(--gf-font-mono);
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .problems {
    color: var(--gf-danger);
    font-size: var(--gf-font-size-sm);
  }
  button {
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
    height: 2.25rem;
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
  .finish {
    border-color: var(--gf-accent);
    background: var(--gf-accent);
    color: var(--gf-accent-contrast);
    align-self: flex-start;
  }
  .finish:hover:not(:disabled) {
    background: var(--gf-accent-hover);
    border-color: var(--gf-accent-hover);
  }
  .nav {
    display: flex;
    gap: var(--gf-space-3);
  }
</style>
