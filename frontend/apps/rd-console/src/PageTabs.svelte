<script lang="ts">
  /**
   * PageTabs — a short row of links between pages that sit under one nav entry (Pilots and
   * Classes; Seats and Timers). Plain links, so the back button and copy-link work.
   */
  import { formatHash, type Route } from './lib/route.js';

  let {
    tabs,
    current,
    label
  }: {
    tabs: readonly { key: string; label: string; to: Route }[];
    /** The key of the page showing now. */
    current: string;
    /** What the row is, for a screen reader. */
    label: string;
  } = $props();
</script>

<nav class="tabs" aria-label={label}>
  {#each tabs as tab (tab.key)}
    <a
      href={formatHash(tab.to)}
      class:active={tab.key === current}
      aria-current={tab.key === current ? 'page' : undefined}>{tab.label}</a
    >
  {/each}
</nav>

<style>
  .tabs {
    display: flex;
    gap: var(--gf-space-1);
    border-bottom: 1px solid var(--gf-border);
  }
  a {
    padding: var(--gf-space-2) var(--gf-space-4);
    margin-bottom: -1px;
    border-bottom: 2px solid transparent;
    text-decoration: none;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-md);
  }
  a:hover {
    color: var(--gf-text);
  }
  a.active {
    color: var(--gf-text);
    border-bottom-color: var(--gf-accent);
  }
  a:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
</style>
