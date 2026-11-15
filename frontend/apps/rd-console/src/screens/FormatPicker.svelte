<script lang="ts">
  /**
   * FormatPicker — choose one of the saved formats, and read what it does.
   *
   * Used wherever a heat, a bracket or a phase is given a format. `value` is the saved
   * format's handle, or `''` for the first entry when `none` is given: that entry is "use
   * the phase's format" under a phase that has one, or "no format of its own".
   */
  import { Field, Select } from '@gridfpv/components';
  import type { SavedFormat } from '@gridfpv/types';
  import { formatSummary, isRanked } from '../lib/formats.js';
  import { formatHash } from '../lib/route.js';

  let {
    formats,
    value = $bindable(''),
    none,
    noneSummary,
    rankedOnly = false,
    disabled = false
  }: {
    formats: readonly SavedFormat[];
    value?: string;
    /** What picking no saved format is called. Left out, one must be picked. */
    none?: string;
    /** What that choice means, shown under the box while it is picked. */
    noneSummary?: string;
    /** Offer only formats that rank a heat (a bracket needs to know who moves on). */
    rankedOnly?: boolean;
    disabled?: boolean;
  } = $props();

  const offered = $derived(rankedOnly ? formats.filter((f) => isRanked(f.format)) : formats);
  const picked = $derived(offered.find((saved) => saved.id === value));
  const formatsHref = formatHash({ kind: 'page', page: 'formats' });
</script>

<Field label="Format">
  <Select bind:value aria-label="Format" {disabled}>
    {#if none !== undefined}<option value="">{none}</option>{/if}
    {#each offered as saved (saved.id)}
      <option value={saved.id}>{saved.format.name}</option>
    {/each}
  </Select>
  <p class="reads" data-testid="format-reads">
    {#if picked}
      {formatSummary(picked.format)}
    {:else if value === '' && noneSummary}
      {noneSummary}
    {:else if offered.length === 0}
      No saved format fits.
    {/if}
    <a href={formatsHref}>Formats</a>
  </p>
</Field>

<style>
  .reads {
    margin: var(--gf-space-1) 0 0;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .reads a {
    margin-left: var(--gf-space-2);
    color: var(--gf-accent);
  }
</style>
