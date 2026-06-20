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
    gap: var(--gf-space-4);
  }
  .steps {
    display: flex;
    gap: var(--gf-space-2);
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .steps li button {
    background: var(--gf-color-surface);
    border: 1px solid var(--gf-color-border);
    border-radius: var(--gf-radius-sm);
    padding: var(--gf-space-1) var(--gf-space-3);
    font-size: var(--gf-font-size-sm);
    color: var(--gf-color-text);
    cursor: pointer;
  }
  .steps li.active button {
    border-color: var(--gf-color-accent);
    color: var(--gf-color-accent);
    font-weight: var(--gf-font-weight-bold);
  }
  .steps li.done button {
    color: var(--gf-color-text-muted);
  }
  .panel {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
    padding: var(--gf-space-4);
    border: 1px solid var(--gf-color-border);
    border-radius: var(--gf-radius-md);
    background: var(--gf-color-surface);
  }
  h3 {
    margin: 0;
    font-size: var(--gf-font-size-md);
  }
  label {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
    font-size: var(--gf-font-size-sm);
    max-width: 24rem;
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
  .class-row {
    display: flex;
    gap: var(--gf-space-2);
    align-items: center;
  }
  fieldset {
    border: 1px solid var(--gf-color-border);
    border-radius: var(--gf-radius-sm);
    display: flex;
    gap: var(--gf-space-4);
    align-items: end;
    flex-wrap: wrap;
  }
  legend {
    font-weight: var(--gf-font-weight-bold);
    font-size: var(--gf-font-size-sm);
  }
  .review dt {
    font-weight: var(--gf-font-weight-bold);
    font-size: var(--gf-font-size-sm);
  }
  .review dd {
    margin: 0 0 var(--gf-space-2);
    font-size: var(--gf-font-size-sm);
  }
  .muted {
    color: var(--gf-color-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .problems {
    color: var(--gf-color-danger);
    font-size: var(--gf-font-size-sm);
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
  .finish {
    border-color: var(--gf-color-accent);
    background: var(--gf-color-accent);
    color: var(--gf-color-accent-contrast);
    align-self: flex-start;
  }
  .nav {
    display: flex;
    gap: var(--gf-space-3);
  }
</style>
