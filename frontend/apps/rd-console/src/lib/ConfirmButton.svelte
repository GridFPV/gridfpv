<script lang="ts">
  /**
   * A button that confirms before firing its action (clients.html §5: reversible /
   * confirm-on-destructive). On first click it flips into a two-step "Confirm? /
   * Cancel" state; the action only runs on the explicit confirm. Non-destructive
   * callers pass `confirm={false}` for a plain one-click button.
   */
  import type { Snippet } from 'svelte';

  let {
    onconfirm,
    confirm = true,
    disabled = false,
    variant = 'default',
    title = undefined,
    children
  }: {
    onconfirm: () => void;
    confirm?: boolean;
    disabled?: boolean;
    variant?: 'default' | 'primary' | 'danger';
    title?: string | undefined;
    children: Snippet;
  } = $props();

  let armed = $state(false);

  function click() {
    if (!confirm) {
      onconfirm();
      return;
    }
    if (armed) {
      armed = false;
      onconfirm();
    } else {
      armed = true;
    }
  }

  function cancel() {
    armed = false;
  }
</script>

<span class="confirm-wrap">
  {#if armed}
    <button
      type="button"
      class="btn confirm"
      class:danger={variant === 'danger'}
      {disabled}
      onclick={click}
    >
      Confirm
    </button>
    <button type="button" class="btn cancel" onclick={cancel}>Cancel</button>
  {:else}
    <button
      type="button"
      class="btn"
      class:primary={variant === 'primary'}
      class:danger={variant === 'danger'}
      {disabled}
      {title}
      onclick={click}
    >
      {@render children()}
    </button>
  {/if}
</span>

<style>
  .confirm-wrap {
    display: inline-flex;
    gap: var(--gf-space-2);
  }
  .btn {
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-medium);
    padding: var(--gf-space-2) var(--gf-space-4);
    border-radius: var(--gf-radius-sm);
    border: 1px solid var(--gf-color-border);
    background: var(--gf-color-surface);
    color: var(--gf-color-text);
    cursor: pointer;
  }
  .btn:hover:not(:disabled) {
    border-color: var(--gf-color-accent);
  }
  .btn:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  .btn.primary {
    background: var(--gf-color-accent);
    color: var(--gf-color-accent-contrast);
    border-color: var(--gf-color-accent);
  }
  .btn.danger,
  .btn.confirm.danger {
    border-color: var(--gf-color-danger);
    color: var(--gf-color-danger);
  }
  .btn.confirm {
    background: var(--gf-color-accent);
    color: var(--gf-color-accent-contrast);
    border-color: var(--gf-color-accent);
  }
  .btn.cancel {
    background: transparent;
  }
</style>
