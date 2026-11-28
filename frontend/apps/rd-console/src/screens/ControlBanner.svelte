<script lang="ts">
  /**
   * ControlBanner — says that another program has control of a timer, so Grid cannot change
   * it, and where a person puts that right. Shows nothing when Grid may change the timer.
   * The words are {@link controlTrouble}'s.
   */
  import { Banner } from '@gridfpv/components';
  import type { TimerView } from '@gridfpv/types';
  import { controlTrouble } from '../lib/timers.js';

  let { view }: { view: TimerView | undefined } = $props();

  const trouble = $derived(view ? controlTrouble(view.timer, view.status) : undefined);
</script>

{#if trouble}
  <div data-testid="control-trouble">
    <Banner tone="warn" title={trouble.trouble}>
      {trouble.detail}
      {#if trouble.page}
        <a href={trouble.page} target="_blank" rel="noreferrer">Open the timer's page</a>
      {/if}
    </Banner>
  </div>
{/if}

<style>
  a {
    color: var(--gf-accent);
    white-space: nowrap;
  }
</style>
