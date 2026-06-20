<script lang="ts">
  /**
   * ToastHost — renders the global `toasts` queue in a fixed, aria-live region
   * (bottom-right). Each toast auto-dismisses after its `duration`. Mount one per
   * surface; push toasts via the `toast` helpers in `toast.svelte.ts`.
   */
  import { toasts, type Toast } from './toast.svelte.js';

  // Schedule auto-dismiss for any toast with a positive duration. The effect
  // re-runs as the queue changes; timers are cleared on teardown.
  $effect(() => {
    const timers = toasts.items
      .filter((t) => t.duration > 0)
      .map((t: Toast) => setTimeout(() => toasts.dismiss(t.id), t.duration));
    return () => timers.forEach(clearTimeout);
  });
</script>

<div class="gf-toast-host" role="region" aria-label="Notifications" aria-live="polite">
  {#each toasts.items as t (t.id)}
    <div class="gf-toast" data-tone={t.tone} role={t.tone === 'danger' ? 'alert' : 'status'}>
      <span class="gf-toast-bar" aria-hidden="true"></span>
      <div class="gf-toast-body">
        {#if t.title}<strong class="gf-toast-title">{t.title}</strong>{/if}
        <span class="gf-toast-msg">{t.message}</span>
      </div>
      <button
        type="button"
        class="gf-toast-close"
        onclick={() => toasts.dismiss(t.id)}
        aria-label="Dismiss">×</button
      >
    </div>
  {/each}
</div>

<style>
  .gf-toast-host {
    position: fixed;
    bottom: var(--gf-space-5);
    right: var(--gf-space-5);
    z-index: 1000;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
    max-width: min(24rem, 92vw);
    pointer-events: none;
  }
  .gf-toast {
    --_c: var(--gf-info);
    pointer-events: auto;
    position: relative;
    display: flex;
    align-items: flex-start;
    gap: var(--gf-space-3);
    padding: var(--gf-space-3) var(--gf-space-4) var(--gf-space-3) var(--gf-space-5);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    background: var(--gf-overlay);
    color: var(--gf-text);
    box-shadow: var(--gf-shadow-md);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    overflow: hidden;
    animation: gf-toast-in var(--gf-motion-base) var(--gf-ease-spring);
  }
  .gf-toast[data-tone='success'] {
    --_c: var(--gf-success);
  }
  .gf-toast[data-tone='warn'] {
    --_c: var(--gf-warn);
  }
  .gf-toast[data-tone='danger'] {
    --_c: var(--gf-danger);
  }
  .gf-toast-bar {
    position: absolute;
    left: 0;
    top: 0;
    bottom: 0;
    width: 3px;
    background: var(--_c);
  }
  .gf-toast-body {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
  }
  .gf-toast-title {
    font-weight: var(--gf-font-weight-semibold);
    color: var(--_c);
  }
  .gf-toast-msg {
    color: var(--gf-text-secondary);
    line-height: var(--gf-line-snug);
  }
  .gf-toast-close {
    border: none;
    background: transparent;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-lg);
    line-height: 1;
    cursor: pointer;
  }
  .gf-toast-close:hover {
    color: var(--gf-text);
  }
  @keyframes gf-toast-in {
    from {
      opacity: 0;
      transform: translateX(12px);
    }
    to {
      opacity: 1;
      transform: none;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .gf-toast {
      animation: none;
    }
  }
</style>
