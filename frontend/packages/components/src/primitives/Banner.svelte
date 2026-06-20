<script lang="ts">
  /**
   * Banner — an inline, contextual message strip (info/success/warn/danger).
   * Used for form-level errors, command failures, and standing notices. Optional
   * `title`, an optional dismiss button, and an `actions` snippet. `role` is set
   * to `alert` for warn/danger so failures are announced.
   */
  import type { Snippet } from 'svelte';

  let {
    tone = 'info',
    title = undefined,
    ondismiss = undefined,
    actions = undefined,
    children
  }: {
    tone?: 'info' | 'success' | 'warn' | 'danger';
    title?: string;
    ondismiss?: () => void;
    actions?: Snippet;
    children: Snippet;
  } = $props();

  const role = $derived(tone === 'danger' || tone === 'warn' ? 'alert' : 'status');
</script>

<div class="gf-banner" data-tone={tone} {role}>
  <span class="gf-banner-icon" aria-hidden="true"></span>
  <div class="gf-banner-body">
    {#if title}<strong class="gf-banner-title">{title}</strong>{/if}
    <span class="gf-banner-msg">{@render children()}</span>
  </div>
  {#if actions}<div class="gf-banner-actions">{@render actions()}</div>{/if}
  {#if ondismiss}
    <button type="button" class="gf-banner-dismiss" onclick={ondismiss} aria-label="Dismiss"
      >×</button
    >
  {/if}
</div>

<style>
  .gf-banner {
    --_c: var(--gf-info);
    --_soft: var(--gf-info-soft);
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    padding: var(--gf-space-3) var(--gf-space-4);
    border: 1px solid color-mix(in srgb, var(--_c) 40%, var(--gf-border));
    border-left-width: 3px;
    border-left-color: var(--_c);
    border-radius: var(--gf-radius-md);
    background: var(--_soft);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
  }
  .gf-banner[data-tone='success'] {
    --_c: var(--gf-success);
    --_soft: var(--gf-success-soft);
  }
  .gf-banner[data-tone='warn'] {
    --_c: var(--gf-warn);
    --_soft: var(--gf-warn-soft);
  }
  .gf-banner[data-tone='danger'] {
    --_c: var(--gf-danger);
    --_soft: var(--gf-danger-soft);
  }
  .gf-banner-icon {
    flex-shrink: 0;
    width: 0.55rem;
    height: 0.55rem;
    border-radius: 50%;
    background: var(--_c);
    box-shadow: 0 0 0 4px color-mix(in srgb, var(--_c) 22%, transparent);
  }
  .gf-banner-body {
    flex: 1;
    display: flex;
    flex-wrap: wrap;
    gap: 0 var(--gf-space-2);
    align-items: baseline;
  }
  .gf-banner-title {
    font-weight: var(--gf-font-weight-semibold);
  }
  .gf-banner-msg {
    color: var(--gf-text-secondary);
  }
  .gf-banner-actions {
    display: flex;
    gap: var(--gf-space-2);
    flex-shrink: 0;
  }
  .gf-banner-dismiss {
    flex-shrink: 0;
    border: none;
    background: transparent;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-lg);
    line-height: 1;
    cursor: pointer;
    padding: 0 var(--gf-space-1);
    border-radius: var(--gf-radius-xs);
    transition: color var(--gf-motion-fast) var(--gf-ease-out);
  }
  .gf-banner-dismiss:hover {
    color: var(--gf-text);
  }
  .gf-banner-dismiss:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
</style>
