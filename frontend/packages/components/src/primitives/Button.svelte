<script lang="ts">
  /**
   * Button — the console's primary action primitive.
   *
   * Variants (primary/secondary/ghost/danger) and sizes (sm/md/lg) all style off
   * the design tokens, so a theme re-skins every button at once. Accessible by
   * default: a real `<button>` with a visible focus ring, `:disabled` handling,
   * and an optional `loading` state that disables + shows a spinner without
   * collapsing the label width.
   */
  import type { Snippet } from 'svelte';

  let {
    variant = 'secondary',
    size = 'md',
    type = 'button',
    disabled = false,
    loading = false,
    block = false,
    title = undefined,
    onclick = undefined,
    children,
    // This is a normal Svelte component (not a custom element); the rest-prop
    // attribute forwarding is intentional.
    // eslint-disable-next-line svelte/valid-compile
    ...rest
  }: {
    variant?: 'primary' | 'secondary' | 'ghost' | 'danger';
    size?: 'sm' | 'md' | 'lg';
    type?: 'button' | 'submit' | 'reset';
    disabled?: boolean;
    loading?: boolean;
    block?: boolean;
    title?: string | undefined;
    onclick?: ((e: MouseEvent) => void) | undefined;
    children: Snippet;
    [key: string]: unknown;
  } = $props();
</script>

<button
  {type}
  {title}
  class="gf-btn"
  class:block
  data-variant={variant}
  data-size={size}
  data-loading={loading || undefined}
  disabled={disabled || loading}
  {onclick}
  {...rest}
>
  {#if loading}<span class="spinner" aria-hidden="true"></span>{/if}
  <span class="label">{@render children()}</span>
</button>

<style>
  .gf-btn {
    --_bg: var(--gf-elevated);
    --_fg: var(--gf-text);
    --_bd: var(--gf-border);
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: var(--gf-space-2);
    padding: 0 var(--gf-space-4);
    height: 2.25rem;
    border: 1px solid var(--_bd);
    border-radius: var(--gf-radius-sm);
    background: var(--_bg);
    color: var(--_fg);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
    line-height: 1;
    letter-spacing: var(--gf-tracking-tight);
    cursor: pointer;
    white-space: nowrap;
    user-select: none;
    transition:
      background var(--gf-motion-fast) var(--gf-ease-out),
      border-color var(--gf-motion-fast) var(--gf-ease-out),
      color var(--gf-motion-fast) var(--gf-ease-out),
      transform var(--gf-motion-fast) var(--gf-ease-out),
      box-shadow var(--gf-motion-fast) var(--gf-ease-out);
  }
  .gf-btn.block {
    display: flex;
    width: 100%;
  }
  .gf-btn[data-size='sm'] {
    height: 1.85rem;
    padding: 0 var(--gf-space-3);
    font-size: var(--gf-font-size-xs);
  }
  .gf-btn[data-size='lg'] {
    height: 2.75rem;
    padding: 0 var(--gf-space-5);
    font-size: var(--gf-font-size-md);
  }

  .gf-btn:hover:not(:disabled) {
    background: var(--gf-elevated-hover);
    border-color: var(--gf-border-strong);
  }
  .gf-btn:active:not(:disabled) {
    transform: translateY(1px);
  }
  .gf-btn:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
  .gf-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  /* Primary — solid brand accent, the one strong call to action per area. */
  .gf-btn[data-variant='primary'] {
    --_bg: var(--gf-accent);
    --_fg: var(--gf-accent-contrast);
    --_bd: var(--gf-accent);
    box-shadow: var(--gf-shadow-xs);
  }
  .gf-btn[data-variant='primary']:hover:not(:disabled) {
    background: var(--gf-accent-hover);
    border-color: var(--gf-accent-hover);
  }
  .gf-btn[data-variant='primary']:active:not(:disabled) {
    background: var(--gf-accent-active);
  }

  /* Ghost — no chrome until hovered; for low-emphasis / inline actions. */
  .gf-btn[data-variant='ghost'] {
    --_bg: transparent;
    --_bd: transparent;
    --_fg: var(--gf-text-secondary);
  }
  .gf-btn[data-variant='ghost']:hover:not(:disabled) {
    background: var(--gf-accent-soft);
    border-color: transparent;
    color: var(--gf-text);
  }

  /* Danger — destructive actions; restrained until hover/active. */
  .gf-btn[data-variant='danger'] {
    --_bg: transparent;
    --_fg: var(--gf-danger);
    --_bd: color-mix(in srgb, var(--gf-danger) 50%, var(--gf-border));
  }
  .gf-btn[data-variant='danger']:hover:not(:disabled) {
    background: var(--gf-danger-soft);
    border-color: var(--gf-danger);
    color: var(--gf-danger);
  }

  .spinner {
    width: 0.85em;
    height: 0.85em;
    border: 2px solid currentColor;
    border-right-color: transparent;
    border-radius: 50%;
    animation: gf-spin 0.6s linear infinite;
  }
  .gf-btn[data-loading] .label {
    opacity: 0.85;
  }
  @keyframes gf-spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .spinner {
      animation-duration: 1.2s;
    }
  }
</style>
