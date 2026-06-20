<script lang="ts">
  /**
   * A button that confirms before firing its action (clients.html §5: reversible /
   * confirm-on-destructive). On first click it flips into a two-step "Confirm? /
   * Cancel" state; the action only runs on the explicit confirm. Non-destructive
   * callers pass `confirm={false}` for a plain one-click button.
   *
   * Restyled by #71 to delegate to the shared `Button` primitive so every action
   * in the console matches the design system. The caller's `variant` is mapped to
   * the primitive's variants (`default` → secondary).
   */
  import type { Snippet } from 'svelte';
  import { Button } from '@gridfpv/components';

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

  const btnVariant = $derived(
    variant === 'primary' ? 'primary' : variant === 'danger' ? 'danger' : 'secondary'
  );

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
    <Button variant="danger" {disabled} onclick={click}>Confirm</Button>
    <Button variant="ghost" onclick={cancel}>Cancel</Button>
  {:else}
    <Button variant={btnVariant} {disabled} {title} onclick={click}>
      {@render children()}
    </Button>
  {/if}
</span>

<style>
  .confirm-wrap {
    display: inline-flex;
    gap: var(--gf-space-2);
  }
</style>
