<script lang="ts">
  /**
   * Badge — a small status/label chip. `tone` picks a semantic color; `variant`
   * chooses a soft (tinted) or solid fill; `dot` prepends a status dot. Used for
   * counts, tags, and inline state markers across the console.
   */
  import type { Snippet } from 'svelte';

  let {
    tone = 'neutral',
    variant = 'soft',
    dot = false,
    children
  }: {
    tone?: 'neutral' | 'accent' | 'success' | 'warn' | 'danger' | 'info';
    variant?: 'soft' | 'solid' | 'outline';
    dot?: boolean;
    children: Snippet;
  } = $props();
</script>

<span class="gf-badge" data-tone={tone} data-variant={variant}>
  {#if dot}<span class="gf-badge-dot" aria-hidden="true"></span>{/if}
  {@render children()}
</span>

<style>
  .gf-badge {
    --_c: var(--gf-text-muted);
    --_soft: var(--gf-surface-alt);
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    padding: 0.15em 0.6em;
    border-radius: var(--gf-radius-pill);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-xs);
    font-weight: var(--gf-font-weight-semibold);
    line-height: 1.5;
    white-space: nowrap;
  }
  .gf-badge[data-tone='accent'] {
    --_c: var(--gf-accent);
    --_soft: var(--gf-accent-soft);
  }
  .gf-badge[data-tone='success'] {
    --_c: var(--gf-success);
    --_soft: var(--gf-success-soft);
  }
  .gf-badge[data-tone='warn'] {
    --_c: var(--gf-warn);
    --_soft: var(--gf-warn-soft);
  }
  .gf-badge[data-tone='danger'] {
    --_c: var(--gf-danger);
    --_soft: var(--gf-danger-soft);
  }
  .gf-badge[data-tone='info'] {
    --_c: var(--gf-info);
    --_soft: var(--gf-info-soft);
  }

  .gf-badge[data-variant='soft'] {
    background: var(--_soft);
    color: var(--_c);
  }
  .gf-badge[data-variant='outline'] {
    background: transparent;
    color: var(--_c);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--_c) 50%, transparent);
  }
  .gf-badge[data-variant='solid'] {
    background: var(--_c);
    color: var(--gf-surface-sunken);
  }
  .gf-badge[data-tone='neutral'][data-variant='solid'] {
    color: var(--gf-text);
    background: var(--gf-border-strong);
  }
  .gf-badge-dot {
    width: 0.45em;
    height: 0.45em;
    border-radius: 50%;
    background: var(--_c);
  }
  .gf-badge[data-variant='solid'] .gf-badge-dot {
    background: currentColor;
  }
</style>
