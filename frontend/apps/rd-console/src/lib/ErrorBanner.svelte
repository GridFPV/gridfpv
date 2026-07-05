<script lang="ts">
  /**
   * Surfaces a shared `ProtocolError` from a failed `CommandAck` (protocol.html §9.8)
   * in plain language for the RD (clients.html §5), with a dismiss. The error code is
   * shown small for support; the message is the human detail.
   *
   * Restyled by #71 to render through the shared `Banner` primitive (danger tone).
   */
  import type { ProtocolError } from '@gridfpv/types';
  import { Banner } from '@gridfpv/components';

  let { error, ondismiss }: { error: ProtocolError | undefined; ondismiss?: () => void } = $props();
</script>

{#if error}
  <Banner tone="danger" title="That didn’t work." {ondismiss}>
    {error.message}
    <span class="code">{error.code}</span>
  </Banner>
{/if}

<style>
  .code {
    margin-left: var(--gf-space-2);
    padding: 0.05em 0.4em;
    border-radius: var(--gf-radius-xs);
    background: color-mix(in srgb, var(--gf-danger) 14%, transparent);
    font-size: var(--gf-font-size-xs);
    color: var(--gf-danger);
    font-family: var(--gf-font-mono);
  }
</style>
