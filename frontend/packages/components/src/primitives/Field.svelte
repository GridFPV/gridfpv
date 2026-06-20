<script lang="ts">
  /**
   * Field — a labelled form-control wrapper: label + optional hint + optional
   * error message, wrapping any control (Input/Select/Textarea or a raw control)
   * passed as children. Wiring the label to the control stays the caller's job
   * (the control owns its own id/aria); Field provides the consistent layout,
   * spacing, and the error/hint slots so every form in the console matches.
   */
  import type { Snippet } from 'svelte';

  let {
    label,
    hint = undefined,
    error = undefined,
    required = false,
    children
  }: {
    label?: string;
    hint?: string;
    error?: string;
    required?: boolean;
    children: Snippet;
  } = $props();
</script>

<div class="gf-field" class:has-error={!!error}>
  {#if label}
    <span class="gf-field-label">
      {label}{#if required}<span class="req" aria-hidden="true">*</span>{/if}
    </span>
  {/if}
  {@render children()}
  {#if error}
    <span class="gf-field-error" role="alert">{error}</span>
  {:else if hint}
    <span class="gf-field-hint">{hint}</span>
  {/if}
</div>

<style>
  .gf-field {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
    font-family: var(--gf-font-family);
  }
  .gf-field-label {
    font-size: var(--gf-font-size-xs);
    font-weight: var(--gf-font-weight-semibold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .req {
    color: var(--gf-danger);
    margin-left: 0.15em;
  }
  .gf-field-hint {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-faint);
  }
  .gf-field-error {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-danger);
    font-weight: var(--gf-font-weight-medium);
  }
</style>
