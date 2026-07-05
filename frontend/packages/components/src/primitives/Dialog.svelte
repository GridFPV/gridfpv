<script lang="ts">
  /**
   * Dialog — a modal built on the native `<dialog>` element, so focus trapping,
   * `Esc`-to-close, and the top-layer backdrop come from the platform. Controlled
   * via a bound `open`; emits `onclose`. Header (`title`) + body (children) +
   * optional `footer` snippet. Clicking the backdrop closes it.
   */
  import type { Snippet } from 'svelte';

  let {
    open = $bindable(false),
    title = undefined,
    onclose = undefined,
    dismissable = true,
    footer = undefined,
    children
  }: {
    open?: boolean;
    title?: string;
    onclose?: () => void;
    dismissable?: boolean;
    footer?: Snippet;
    children: Snippet;
  } = $props();

  let dialog = $state<HTMLDialogElement>();

  $effect(() => {
    const el = dialog;
    if (!el) return;
    if (open && !el.open) el.showModal();
    else if (!open && el.open) el.close();
  });

  function close() {
    open = false;
    onclose?.();
  }

  /**
   * Backdrop click-to-close, with a **buffer** so a near-miss while reaching for the form doesn't
   * dismiss it. A click whose target is the dialog element itself (not its inner panel) is the
   * backdrop region — but we only close when it lands *well outside* the panel: clicks within
   * {@link BACKDROP_BUFFER_PX} of the panel edge are treated as intended-for-the-dialog and ignored.
   */
  const BACKDROP_BUFFER_PX = 32;
  function onBackdrop(e: MouseEvent) {
    if (!dismissable) return;
    if (e.target !== dialog) return; // clicked inside the panel / its content
    const panel = dialog?.querySelector('.gf-dialog-panel');
    if (panel) {
      const r = panel.getBoundingClientRect();
      const b = BACKDROP_BUFFER_PX;
      const nearPanel =
        e.clientX >= r.left - b &&
        e.clientX <= r.right + b &&
        e.clientY >= r.top - b &&
        e.clientY <= r.bottom + b;
      if (nearPanel) return; // within the buffer — a near-miss, don't close
    }
    close();
  }
</script>

<dialog
  bind:this={dialog}
  class="gf-dialog"
  onclose={close}
  oncancel={(e) => {
    if (!dismissable) e.preventDefault();
  }}
  onclick={onBackdrop}
>
  <div class="gf-dialog-panel" role="document">
    {#if title || dismissable}
      <header class="gf-dialog-head">
        {#if title}<h2 class="gf-dialog-title">{title}</h2>{/if}
        {#if dismissable}
          <button type="button" class="gf-dialog-close" onclick={close} aria-label="Close">×</button
          >
        {/if}
      </header>
    {/if}
    <div class="gf-dialog-body">{@render children()}</div>
    {#if footer}<footer class="gf-dialog-foot">{@render footer()}</footer>{/if}
  </div>
</dialog>

<style>
  .gf-dialog {
    padding: 0;
    border: none;
    background: transparent;
    max-width: min(34rem, 92vw);
    width: 100%;
    color: var(--gf-text);
  }
  .gf-dialog::backdrop {
    background: rgba(4, 7, 11, 0.62);
    backdrop-filter: blur(3px);
  }
  .gf-dialog[open] {
    animation: gf-dialog-in var(--gf-motion-base) var(--gf-ease-out);
  }
  .gf-dialog-panel {
    background: var(--gf-overlay);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    box-shadow: var(--gf-shadow-lg);
    font-family: var(--gf-font-family);
    overflow: hidden;
  }
  .gf-dialog-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-3);
    padding: var(--gf-space-4) var(--gf-space-5);
    border-bottom: 1px solid var(--gf-border-subtle);
  }
  .gf-dialog-title {
    margin: 0;
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
  }
  .gf-dialog-close {
    border: none;
    background: transparent;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-xl);
    line-height: 1;
    cursor: pointer;
    border-radius: var(--gf-radius-xs);
  }
  .gf-dialog-close:hover {
    color: var(--gf-text);
  }
  .gf-dialog-close:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
  .gf-dialog-body {
    padding: var(--gf-space-5);
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-secondary);
    line-height: var(--gf-line-normal);
  }
  .gf-dialog-foot {
    display: flex;
    justify-content: flex-end;
    gap: var(--gf-space-2);
    padding: var(--gf-space-4) var(--gf-space-5);
    border-top: 1px solid var(--gf-border-subtle);
    background: var(--gf-surface-alt);
  }
  @keyframes gf-dialog-in {
    from {
      opacity: 0;
      transform: translateY(8px) scale(0.98);
    }
    to {
      opacity: 1;
      transform: none;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .gf-dialog[open] {
      animation: none;
    }
  }
</style>
