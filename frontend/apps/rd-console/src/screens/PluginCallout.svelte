<script lang="ts">
  // The GridFPV-plugin chip + guided-install prompt for a RotorHazard timer row (D16, Slice 1).
  //
  // Shows a small status chip reflecting the timer's plugin presence. When the plugin is missing
  // or incompatible the chip is a button that opens the install guide: download the bundle, unzip
  // it, drop the gridfpv folder into RotorHazard's plugins/ dir, restart RH. A healthy plugin
  // shows a quiet ✓ chip. Renders nothing for non-RotorHazard timers or before the timer has been
  // probed.
  import { Badge, Button, Dialog, toast } from '@gridfpv/components';
  import type { Timer } from '@gridfpv/types';
  import { pluginBundleUrl, pluginView } from '../lib/pluginPresence.js';

  let { timer, baseUrl }: { timer: Timer; baseUrl: string } = $props();

  const view = $derived(pluginView(timer));
  let open = $state(false);

  /** The name the bundle is served (and saved) under — `Content-Disposition` matches. */
  const BUNDLE_FILE = 'gridfpv-plugin.zip';

  /** In-flight guard + the last outcome, rendered **inside** the dialog (see `status`). */
  let downloading = $state(false);
  let status = $state<{ tone: 'ok' | 'bad'; message: string } | null>(null);

  /**
   * Fetch the bundle, then hand it to the browser as a download (#384).
   *
   * The old transient-anchor-on-the-URL form was silent: it could not tell success from a 500 or a
   * dropped connection, so a click gave the RD nothing at all. Fetching first means a failure is a
   * real error we can name. The outcome is reported **twice on purpose**: the dialog is a native
   * `showModal()`, which renders in the browser's top layer *above* any toast, so the inline
   * `status` line is what the RD actually sees while the guide is open; the toast carries the same
   * message on to whoever closes the dialog first.
   *
   * Desktop note: inside the Tauri shell there is no download shelf, so the wording points at the
   * browser/Downloads folder rather than promising a path. A native save dialog
   * (`tauri-plugin-dialog` + `fs`) is the better desktop fix and is tracked separately.
   */
  async function download() {
    if (downloading) return;
    downloading = true;
    status = null;
    try {
      const res = await fetch(pluginBundleUrl(baseUrl));
      if (!res.ok) throw new Error(`the Director returned ${res.status}`);
      const blob = await res.blob();
      const href = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = href;
      a.download = BUNDLE_FILE;
      document.body.appendChild(a);
      a.click();
      a.remove();
      // Keep the object URL alive past the click (some webviews read it asynchronously).
      setTimeout(() => URL.revokeObjectURL(href), 30_000);
      const message = `${BUNDLE_FILE} downloaded — check your browser’s downloads (usually your Downloads folder), then unzip it.`;
      status = { tone: 'ok', message };
      toast.success(message, 'Plugin downloaded');
    } catch (err) {
      const reason = err instanceof Error ? err.message : String(err);
      const message = `Couldn’t download ${BUNDLE_FILE}: ${reason}.`;
      status = { tone: 'bad', message };
      toast.error(message, 'Download failed');
    } finally {
      downloading = false;
    }
  }
</script>

{#if view}
  {#if view.needsInstall}
    <button
      type="button"
      class="plugin-chip"
      onclick={() => {
        status = null;
        open = true;
      }}
      title="GridFPV plugin — click for install steps"
    >
      <Badge tone={view.tone}>⚠ {view.label}</Badge>
    </button>

    <Dialog bind:open title={view.title}>
      <div class="install-guide">
        <!-- The timer is named by its friendly name (never its URL), per CLAUDE.md. -->
        <p class="lead"><strong>{timer.name}</strong> needs the GridFPV plugin:</p>
        {#if view.detail}<p class="detail">{view.detail}</p>{/if}
        <ol class="steps">
          <li>
            Download <code>{BUNDLE_FILE}</code> with the button below.
          </li>
          <li>
            <strong>Unzip it.</strong> Inside is a single <code>gridfpv</code> folder — that folder is
            what you copy, not the zip and not the wrapper folder your unzipper may put around it.
          </li>
          <li>
            Copy the <code>gridfpv</code> folder into RotorHazard's <code>plugins/</code> directory,
            so you end up with <code>plugins/gridfpv/</code> holding <code>__init__.py</code> and
            <code>manifest.json</code> <em>directly</em> inside it — no extra folder in between.
          </li>
          <li>Restart RotorHazard — the timer reconnects and the badge turns green.</li>
        </ol>
        {#if status}
          <p class="status" class:bad={status.tone === 'bad'} role="status">{status.message}</p>
        {/if}
      </div>
      {#snippet footer()}
        <Button variant="ghost" onclick={() => (open = false)}>Close</Button>
        <Button variant="primary" loading={downloading} onclick={download}>Download plugin</Button>
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
  .install-guide .status {
    margin: 0.75rem 0 0;
    padding: 0.5rem 0.75rem;
    border-radius: var(--gf-radius-sm, 4px);
    border: 1px solid var(--gf-success);
    background: var(--gf-success-soft);
    color: var(--gf-success);
    font-size: 0.9em;
  }
  .install-guide .status.bad {
    border-color: var(--gf-danger);
    background: var(--gf-danger-soft);
    color: var(--gf-danger);
  }
</style>
