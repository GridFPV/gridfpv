<script lang="ts">
  /**
   * LevelsFields — how one set of enter and exit levels is set: following the noise floor (two
   * distances) or fixed (an enter and an exit level). The numbers are in the unit the draft
   * carries (`levels.unit`): dB on most timers, millivolts on some.
   *
   * The Tune page uses it twice: once for the timer's levels, and once for each seat that has
   * its own. It edits the draft it is bound to and sends nothing.
   *
   * Going to fixed levels fills empty fields from the levels in use right now (`inUse`), so the
   * director starts from what is on the trace (`withMode` in `lib/tune.ts`).
   */
  import type { Snippet } from 'svelte';
  import { Field, Input } from '@gridfpv/components';
  import {
    gapLabel,
    unitLabel,
    unitScale,
    withMode,
    type EnterExit,
    type LevelsDraft,
    type ThresholdMode
  } from '../lib/tune.js';

  let {
    levels = $bindable(),
    inUse = undefined,
    group,
    legend,
    who = undefined,
    children = undefined
  }: {
    /** The levels in the draft. */
    levels: LevelsDraft;
    /** The enter and exit levels in use right now, when the timer has reported them. */
    inUse?: EnterExit | undefined;
    /** A name for the pair of choices that no other pair on the page has. */
    group: string;
    /** What the pair of choices is called. */
    legend: string;
    /** The seat these levels belong to, by the name people see. Left out for the timer's own. */
    who?: string | undefined;
    /** More fields, laid out after the level fields. */
    children?: Snippet | undefined;
  } = $props();

  /** A field's name for a screen reader: the seat first, when the levels are one seat's. */
  const named = (words: string): string =>
    who ? `${who} ${words.charAt(0).toLowerCase()}${words.slice(1)}` : words;

  // The unit after a label ("Enter level (dB)") and in a screen reader's name (", in dB").
  // Plain counts have no name, so nothing is added.
  const levelUnit = $derived(unitLabel(levels.unit));
  const gapUnit = $derived(gapLabel(levels.unit));
  const after = (unit: string): string => (unit ? ` (${unit})` : '');
  const spoken = (unit: string): string => (unit ? `, in ${unit}` : '');
  /** One click of a field's arrows: half a dB, or one millivolt or count. */
  const step = $derived(unitScale(levels.unit) === 1 ? '1' : '0.5');

  function setMode(mode: ThresholdMode) {
    levels = withMode(levels, mode, inUse);
  }
</script>

<div class="levels">
  <fieldset class="mode">
    <legend>{legend}</legend>
    <label class="radio">
      <input
        type="radio"
        name={group}
        checked={levels.mode === 'Auto'}
        onchange={() => setMode('Auto')}
      />
      <span>Follow the noise floor</span>
    </label>
    <label class="radio">
      <input
        type="radio"
        name={group}
        checked={levels.mode === 'Manual'}
        onchange={() => setMode('Manual')}
      />
      <span>Fixed levels</span>
    </label>
  </fieldset>

  <div class="fields">
    {#if levels.mode === 'Auto'}
      <Field
        label="Enter this far above the floor{after(gapUnit)}"
        hint="A crossing starts when the signal rises this far above the noise floor."
      >
        <Input
          type="number"
          inputmode="decimal"
          {step}
          aria-label={named(`Enter this far above the floor${spoken(gapUnit)}`)}
          bind:value={levels.aboveFloor}
        />
      </Field>
      <Field
        label="Exit this far below enter{after(gapUnit)}"
        hint="The crossing ends when the signal falls this far below the enter level."
      >
        <Input
          type="number"
          inputmode="decimal"
          {step}
          aria-label={named(`Exit this far below enter${spoken(gapUnit)}`)}
          bind:value={levels.belowEnter}
        />
      </Field>
    {:else}
      <Field label="Enter level{after(levelUnit)}" hint="A crossing starts at this signal level.">
        <Input
          type="number"
          inputmode="decimal"
          {step}
          aria-label={named(`Enter level${spoken(levelUnit)}`)}
          bind:value={levels.enter}
        />
      </Field>
      <Field
        label="Exit level{after(levelUnit)}"
        hint="The crossing ends when the signal falls below this level."
      >
        <Input
          type="number"
          inputmode="decimal"
          {step}
          aria-label={named(`Exit level${spoken(levelUnit)}`)}
          bind:value={levels.exit}
        />
      </Field>
    {/if}
    {@render children?.()}
  </div>
</div>

<style>
  .levels {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .mode {
    margin: 0;
    padding: 0;
    border: 0;
    display: flex;
    align-items: center;
    gap: var(--gf-space-5);
    flex-wrap: wrap;
  }
  .mode legend {
    padding: 0;
    margin-bottom: var(--gf-space-2);
    font-weight: var(--gf-font-weight-semibold);
  }
  .radio {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    cursor: pointer;
  }
  .radio input {
    width: 1.15rem;
    height: 1.15rem;
    accent-color: var(--gf-accent);
  }
  .fields {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(15rem, 1fr));
    gap: var(--gf-space-4);
  }
</style>
