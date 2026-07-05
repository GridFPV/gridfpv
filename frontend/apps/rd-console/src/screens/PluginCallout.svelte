<script lang="ts">
  // The GridFPV-plugin chip + guided-install prompt for a RotorHazard timer row (D16, Slice 1).
  //
  // Shows a small status chip reflecting the timer's plugin presence. When the plugin is missing
  // or incompatible the chip is a button that opens a one-step install guide: download the bundle,
  // drop it into RotorHazard's plugins/ dir, restart RH. A healthy plugin shows a quiet ✓ chip.
  // Renders nothing for non-RotorHazard timers or before the timer has been probed.
  import { Badge, Button, Dialog } from '@gridfpv/components';
  import type { Timer } from '@gridfpv/types';
  import { pluginBundleUrl, pluginView } from '../lib/pluginPresence.js';

  let { timer, baseUrl }: { timer: Timer; baseUrl: string } = $props();

  const view = $derived(pluginView(timer));
  let open = $state(false);

  // Trigger the bundle download via a transient anchor (the endpoint serves it as an attachment).
  function download() {
    const a = document.createElement('a');
    a.href = pluginBundleUrl(baseUrl);
    a.download = 'gridfpv-plugin.zip';
    document.body.appendChild(a);
    a.click();
    a.remove();
  }
</script>

{#if view}
  {#if view.needsInstall}
    <button
      type="button"
      class="plugin-chip"
      onclick={() => (open = true)}
      title="GridFPV plugin — click for install steps"
    >
      <Badge tone={view.tone}>⚠ {view.label}</Badge>
    </button>

    <Dialog bind:open title={view.title}>
      <div class="install-guide">
        <!-- The timer is named by its friendly name (never its URL), per CLAUDE.md. -->
        <p class="lead">
          <strong>{timer.name}</strong> needs the GridFPV plugin. Install it in one step:
        </p>
        {#if view.detail}<p class="detail">{view.detail}</p>{/if}
        <ol class="steps">
          <li>Download the GridFPV plugin folder.</li>
          <li>
            Drop the <code>gridfpv</code> folder into RotorHazard's <code>plugins/</code> directory.
          </li>
          <li>Restart RotorHazard — the timer reconnects and the badge turns green.</li>
        </ol>
      </div>
      {#snippet footer()}
        <Button variant="ghost" onclick={() => (open = false)}>Close</Button>
        <Button variant="primary" onclick={download}>Download plugin</Button>
      {/snippet}
    </Dialog>
  {:else}
    <span class="plugin-ok" title={view.detail ?? view.title}>
      <Badge tone={view.tone}>{view.label}</Badge>
    </span>
  {/if}
{/if}

<style>
  .plugin-chip {
    background: none;
    border: none;
    padding: 0;
    margin: 0;
    cursor: pointer;
    font: inherit;
  }
  .plugin-ok {
    display: inline-flex;
  }
  .install-guide .lead {
    margin: 0 0 0.5rem;
  }
  .install-guide .detail {
    margin: 0 0 0.75rem;
    opacity: 0.85;
  }
  .install-guide .steps {
    margin: 0;
    padding-left: 1.25rem;
    line-height: 1.7;
  }
  .install-guide code {
    font-family: var(--font-mono, monospace);
  }
</style>
