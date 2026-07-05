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
    <!-- The *active* confirm is deliberately loud: a solid red (danger) fill with near-black text,
         inverted from the restrained default danger style, so a destructive confirm (Revert /
         Restart / Abort / Discard) is unmistakable on a sunlit field. We tag it with a forwarded
         `data-confirm-danger` attribute (NOT a `class` — a spread `class` would clobber the Button
         primitive's own `gf-btn` class) and restyle that below. -->
    <Button variant="danger" data-confirm-danger {disabled} onclick={click}>Confirm</Button>
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

  /* Solid red fill + dark letters for the *armed* confirm — a high-prominence destructive affordance
     (inverted from the restrained default danger style), so Revert/Restart confirms are unmistakable
     on a sunlit field. We target the Button primitive via the forwarded `data-confirm-danger`
     attribute (the primitive keeps its own `gf-btn` class — a spread `class` would have replaced it).

     The primitive's own `.gf-btn[data-variant='danger'].<scope>` rule lives in a separate component
     stylesheet whose source order we don't control, so we win deterministically with `!important` on
     the load-bearing declarations. `--gf-danger` is the existing danger token; near-black ink
     (`--gf-neutral-0`) holds strong contrast on it in both dark and light themes. */
  :global(.gf-btn[data-confirm-danger]) {
    background: var(--gf-danger) !important;
    border-color: var(--gf-danger) !important;
    color: var(--gf-neutral-0) !important;
    font-weight: var(--gf-font-weight-bold, 700);
  }
  :global(.gf-btn[data-confirm-danger]:hover:not(:disabled)) {
    /* A touch brighter on hover so the solid fill still reads as interactive. */
    filter: brightness(1.08);
  }
</style>
