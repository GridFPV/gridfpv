<script lang="ts">
  /**
   * Collapsible — an accessible, token-styled disclosure section for the data-heavy
   * event stages. A header row (a `title` + an optional `summary` snippet + a chevron)
   * toggles a content region; the whole header is a `<button>` carrying `aria-expanded`
   * and pointing at the region it controls (`aria-controls`), and the region is
   * `aria-hidden` + inert when collapsed.
   *
   * Open state is **controlled or self-managed**: bind `open` to own it from the
   * outside (e.g. to persist it in `localStorage`), or leave it unbound and the
   * component tracks it internally. Defaults to open.
   *
   * An optional `actions` snippet renders interactive controls in the header
   * *beside* the toggle (not inside the button — so a click on a control doesn't
   * also toggle the section). Keyboard: the header button toggles on Enter/Space
   * (native `<button>` behaviour) and via an explicit handler so screen-reader
   * virtual cursors behave too. Field-readable: large header text on a dark surface,
   * consistent with the Card/Tabs primitives.
   */
  import type { Snippet } from 'svelte';

  let {
    title,
    open = $bindable(true),
    id = undefined,
    summary = undefined,
    actions = undefined,
    children
  }: {
    /** The section heading shown in the toggle. */
    title: string;
    /** Open state. Bindable: controlled when bound, self-managed otherwise. Defaults open. */
    open?: boolean;
    /** A stable id stem for the region (so `aria-controls` resolves). Auto-generated if omitted. */
    id?: string;
    /** A count/summary snippet rendered next to the title (e.g. a placed-count badge). */
    summary?: Snippet;
    /** Interactive controls rendered in the header beside the toggle (clicks don't toggle). */
    actions?: Snippet;
    children?: Snippet;
  } = $props();

  // A stable id for the region so the toggle's `aria-controls` resolves even when the
  // caller doesn't pass one. `$props.id()` is per-instance and stable across renders.
  const autoId = $props.id();
  const regionId = $derived(id ? `${id}-region` : `gf-collapsible-${autoId}`);

  function toggle() {
    open = !open;
  }
</script>

<section class="gf-collapsible" data-open={open}>
  <div class="gf-collapsible-head">
    <button
      type="button"
      class="gf-collapsible-toggle"
      aria-expanded={open}
      aria-controls={regionId}
      onclick={toggle}
    >
      <svg
        class="gf-collapsible-chevron"
        viewBox="0 0 16 16"
        width="16"
        height="16"
        aria-hidden="true"
      >
        <path
          d="M6 4l4 4-4 4"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
          stroke-linejoin="round"
        />
      </svg>
      <span class="gf-collapsible-title">{title}</span>
      {#if summary}<span class="gf-collapsible-summary">{@render summary()}</span>{/if}
    </button>
    {#if actions}<div class="gf-collapsible-actions">{@render actions()}</div>{/if}
  </div>

  <div id={regionId} class="gf-collapsible-region" role="region" aria-hidden={!open} hidden={!open}>
    <div class="gf-collapsible-body">
      {@render children?.()}
    </div>
  </div>
</section>

<style>
  .gf-collapsible {
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    background: var(--gf-surface);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    overflow: hidden;
  }
  .gf-collapsible-head {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    padding-right: var(--gf-space-4);
  }
  .gf-collapsible-toggle {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    padding: var(--gf-space-4) var(--gf-space-4);
    border: none;
    background: transparent;
    color: var(--gf-text);
    font-family: inherit;
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
    text-align: left;
    cursor: pointer;
    transition: color var(--gf-motion-fast) var(--gf-ease-out);
  }
  .gf-collapsible-toggle:hover {
    color: var(--gf-text);
  }
  .gf-collapsible-toggle:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
    border-radius: var(--gf-radius-md);
  }
  .gf-collapsible-chevron {
    flex-shrink: 0;
    color: var(--gf-text-muted);
    transform: rotate(0deg);
    transition: transform var(--gf-motion-base) var(--gf-ease-out);
  }
  .gf-collapsible[data-open='true'] .gf-collapsible-chevron {
    transform: rotate(90deg);
  }
  .gf-collapsible-title {
    flex-shrink: 0;
  }
  .gf-collapsible-summary {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    min-width: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-medium);
  }
  .gf-collapsible-actions {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-shrink: 0;
  }
  .gf-collapsible-region[hidden] {
    display: none;
  }
  .gf-collapsible-body {
    padding: 0 var(--gf-space-5) var(--gf-space-5);
    border-top: 1px solid var(--gf-border-subtle);
    padding-top: var(--gf-space-4);
  }
</style>
