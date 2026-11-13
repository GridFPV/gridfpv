<script lang="ts">
  /**
   * LevelsFields — how one set of enter and exit levels is set: following the noise floor (two
   * distances in dB) or fixed (an enter and an exit level in dB).
   *
   * The Tune page uses it twice: once for the timer's levels, and once for each seat that has
   * its own. It edits the draft it is bound to and sends nothing.
   *
   * Going to fixed levels fills empty fields from the levels in use right now (`inUse`), so the
   * director starts from what is on the trace (`withMode` in `lib/tune.ts`).
   */
  import type { Snippet } from 'svelte';
  import { Field, Input } from '@gridfpv/components';
  import { withMode, type EnterExit, type LevelsDraft, type ThresholdMode } from '../lib/tune.js';

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
        label="Enter this far above the floor (dB)"
        hint="A crossing starts when the signal rises this far above the noise floor."
      >
        <Input
          type="number"
          inputmode="decimal"
          step="0.5"
          aria-label={named('Enter this far above the floor, in dB')}
          bind:value={levels.aboveFloorDb}
        />
      </Field>
      <Field
        label="Exit this far below enter (dB)"
        hint="The crossing ends when the signal falls this far below the enter level."
      >
        <Input
          type="number"
          inputmode="decimal"
          step="0.5"
          aria-label={named('Exit this far below enter, in dB')}
          bind:value={levels.belowEnterDb}
        />
      </Field>
    {:else}
      <Field label="Enter level (dB)" hint="A crossing starts at this signal level.">
        <Input
          type="number"
          inputmode="decimal"
          step="0.5"
          aria-label={named('Enter level, in dB')}
          bind:value={levels.enterDb}
        />
      </Field>
      <Field
        label="Exit level (dB)"
        hint="The crossing ends when the signal falls below this level."
      >
        <Input
          type="number"
          inputmode="decimal"
          step="0.5"
          aria-label={named('Exit level, in dB')}
          bind:value={levels.exitDb}
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
