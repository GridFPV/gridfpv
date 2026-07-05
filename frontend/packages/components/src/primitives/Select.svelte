<script lang="ts">
  /**
   * Select — a token-styled native `<select>` with a custom chevron. Two-way
   * `value` binding; options are passed as children (`<option>`s) so callers keep
   * full control. Native for accessibility + keyboard behaviour out of the box.
   */
  import type { Snippet } from 'svelte';

  let {
    value = $bindable(''),
    size = 'md',
    children,
    // Normal component (not a custom element); attribute forwarding is intentional.
    // eslint-disable-next-line svelte/valid-compile
    ...rest
  }: {
    value?: string | number;
    size?: 'sm' | 'md';
    children: Snippet;
    [key: string]: unknown;
  } = $props();
</script>

<div class="gf-select" data-size={size}>
  <select bind:value {...rest}>
    {@render children()}
  </select>
  <span class="chev" aria-hidden="true"></span>
</div>

<style>
  .gf-select {
    position: relative;
    display: inline-flex;
    width: 100%;
  }
  select {
    appearance: none;
    width: 100%;
    height: 2.25rem;
    padding: 0 var(--gf-space-8) 0 var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    cursor: pointer;
    transition:
      border-color var(--gf-motion-fast) var(--gf-ease-out),
      box-shadow var(--gf-motion-fast) var(--gf-ease-out);
  }
  .gf-select[data-size='sm'] select {
    height: 1.85rem;
    font-size: var(--gf-font-size-xs);
  }
  select:hover:not(:disabled) {
    border-color: var(--gf-border-strong);
  }
  select:focus {
    outline: none;
    border-color: var(--gf-accent);
    box-shadow: 0 0 0 3px var(--gf-accent-soft);
  }
  select:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }
  .chev {
    position: absolute;
    right: var(--gf-space-3);
    top: 50%;
    width: 0.5rem;
    height: 0.5rem;
    border-right: 2px solid var(--gf-text-muted);
    border-bottom: 2px solid var(--gf-text-muted);
    transform: translateY(-65%) rotate(45deg);
    pointer-events: none;
  }
</style>
