<script lang="ts">
  /**
   * Tabs — an accessible tablist. Takes an `items` list ({ id, label, badge? })
   * and a bound `value` (the active id); emits `onchange` on selection. Arrow-key
   * navigation + roving `tabindex` per the WAI-ARIA tabs pattern. The caller owns
   * the panels — this is just the selector strip.
   */
  type Item = { id: string; label: string; badge?: string | number };

  let {
    items,
    value = $bindable(),
    onchange = undefined,
    ariaLabel = 'Tabs'
  }: {
    items: Item[];
    value: string;
    onchange?: (id: string) => void;
    ariaLabel?: string;
  } = $props();

  function select(id: string) {
    value = id;
    onchange?.(id);
  }

  function onKeydown(e: KeyboardEvent) {
    const idx = items.findIndex((i) => i.id === value);
    if (idx < 0) return;
    let next = idx;
    if (e.key === 'ArrowRight' || e.key === 'ArrowDown') next = (idx + 1) % items.length;
    else if (e.key === 'ArrowLeft' || e.key === 'ArrowUp')
      next = (idx - 1 + items.length) % items.length;
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = items.length - 1;
    else return;
    e.preventDefault();
    select(items[next].id);
  }
</script>

<div class="gf-tabs" role="tablist" aria-label={ariaLabel}>
  {#each items as item (item.id)}
    {@const active = item.id === value}
    <button
      type="button"
      role="tab"
      class="gf-tab"
      class:active
      aria-selected={active}
      tabindex={active ? 0 : -1}
      onkeydown={onKeydown}
      onclick={() => select(item.id)}
    >
      <span>{item.label}</span>
      {#if item.badge !== undefined}<span class="gf-tab-badge">{item.badge}</span>{/if}
    </button>
  {/each}
</div>

<style>
  .gf-tabs {
    display: inline-flex;
    gap: var(--gf-space-1);
    padding: var(--gf-space-1);
    background: var(--gf-surface-alt);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-md);
  }
  .gf-tab {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    padding: var(--gf-space-2) var(--gf-space-4);
    border: none;
    border-radius: var(--gf-radius-sm);
    background: transparent;
    color: var(--gf-text-muted);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-medium);
    cursor: pointer;
    transition:
      background var(--gf-motion-fast) var(--gf-ease-out),
      color var(--gf-motion-fast) var(--gf-ease-out);
  }
  .gf-tab:hover {
    color: var(--gf-text);
  }
  .gf-tab.active {
    background: var(--gf-elevated);
    color: var(--gf-text);
    box-shadow: var(--gf-shadow-xs);
  }
  .gf-tab:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
  .gf-tab-badge {
    min-width: 1.4em;
    padding: 0 0.35em;
    border-radius: var(--gf-radius-pill);
    background: var(--gf-accent-soft);
    color: var(--gf-accent);
    font-size: var(--gf-font-size-2xs);
    font-weight: var(--gf-font-weight-bold);
    text-align: center;
  }
</style>
