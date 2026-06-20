<script lang="ts">
  /**
   * Surfaces a shared `ProtocolError` from a failed `CommandAck` (protocol.html §9.8)
   * in plain language for the RD (clients.html §5), with a dismiss. The error code is
   * shown small for support; the message is the human detail.
   */
  import type { ProtocolError } from '@gridfpv/types';

  let { error, ondismiss }: { error: ProtocolError | undefined; ondismiss?: () => void } = $props();
</script>

{#if error}
  <div class="error-banner" role="alert">
    <span class="msg"><strong>That didn’t work.</strong> {error.message}</span>
    <span class="code">{error.code}</span>
    {#if ondismiss}
      <button type="button" class="dismiss" onclick={ondismiss} aria-label="Dismiss">×</button>
    {/if}
  </div>
{/if}

<style>
  .error-banner {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    padding: var(--gf-space-2) var(--gf-space-4);
    border: 1px solid var(--gf-color-danger);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-color-surface-alt);
    color: var(--gf-color-text);
    font-size: var(--gf-font-size-sm);
  }
  .msg {
    flex: 1;
  }
  .code {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-color-text-muted);
    font-family: monospace;
  }
  .dismiss {
    border: none;
    background: transparent;
    color: var(--gf-color-text-muted);
    font-size: var(--gf-font-size-lg);
    line-height: 1;
    cursor: pointer;
  }
</style>
