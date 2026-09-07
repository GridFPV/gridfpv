<script lang="ts">
  /**
   * Card — a raised surface for grouping content. Optional `title`/`subtitle`
   * header and an `actions` snippet (rendered top-right). `elevation` picks the
   * shadow ramp; `pad={false}` lets a child (e.g. a full-bleed table) own its
   * own padding.
   *
   * `help` is the tooltip form of `subtitle` (#466): the same explanatory text, behind a `?`
   * beside the title instead of a permanent paragraph under it. Prefer it for anything the
   * operator reads once — what a section is, how a feature behaves. Keep `subtitle` for the
   * short line that has to be visible every time.
   */
  import type { Snippet } from 'svelte';
  import InfoTip from './InfoTip.svelte';

  let {
    title = undefined,
    subtitle = undefined,
    help = undefined,
    elevation = 'sm',
    pad = true,
    actions = undefined,
    children
  }: {
    title?: string;
    subtitle?: string;
    /** Explanatory text for this section, shown on demand rather than permanently. */
    help?: string;
    elevation?: 'flat' | 'xs' | 'sm' | 'md';
    pad?: boolean;
    actions?: Snippet;
    children: Snippet;
  } = $props();
</script>

<section class="gf-card" data-elevation={elevation}>
  {#if title || actions}
    <header class="gf-card-head">
      <div class="titles">
        <!-- The InfoTip sits BESIDE the heading, never inside it: a `?` button nested in an <h3>
             becomes part of the heading's accessible name ("Rounds, More information"), which
             breaks both screen-reader navigation and every by-role query. -->
        {#if title}
          <div class="gf-card-titleline">
            <h3 class="gf-card-title">{title}</h3>
            {#if help}<InfoTip text={help} label={`About ${title}`} />{/if}
          </div>
        {/if}
        {#if subtitle}<p class="gf-card-subtitle">{subtitle}</p>{/if}
      </div>
      {#if actions}<div class="gf-card-actions">{@render actions()}</div>{/if}
    </header>
  {/if}
  <div class="gf-card-body" class:pad>
    {@render children()}
  </div>
</section>

<style>
  .gf-card {
    background: var(--gf-elevated);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    box-shadow: var(--gf-shadow-sm), var(--gf-shadow-inset);
    overflow: hidden;
  }
  .gf-card[data-elevation='flat'] {
    box-shadow: none;
    background: var(--gf-surface-alt);
  }
  .gf-card[data-elevation='xs'] {
    box-shadow: var(--gf-shadow-xs);
  }
  .gf-card[data-elevation='md'] {
    box-shadow: var(--gf-shadow-md), var(--gf-shadow-inset);
  }
  .gf-card-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--gf-space-3);
    padding: var(--gf-space-4) var(--gf-space-5);
    border-bottom: 1px solid var(--gf-border-subtle);
  }
  .gf-card-titleline {
    display: flex;
    align-items: center;
  }
  .gf-card-title {
    margin: 0;
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
  }
  .gf-card-subtitle {
    margin: var(--gf-space-1) 0 0;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .gf-card-actions {
    display: flex;
    gap: var(--gf-space-2);
    flex-shrink: 0;
  }
  .gf-card-body.pad {
    padding: var(--gf-space-5);
  }
</style>
