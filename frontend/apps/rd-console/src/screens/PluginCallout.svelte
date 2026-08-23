<script lang="ts">
  // The GridFPV-plugin chip + guided-install prompt for a RotorHazard timer row (D16, Slice 1).
  //
  // Shows a small status chip reflecting the timer's plugin presence. When the plugin is missing
  // or incompatible the chip is a button that opens the install guide: download the bundle, unzip
  // it, drop the gridfpv folder into RotorHazard's plugins/ dir, restart RH. A healthy plugin
  // shows a quiet ✓ chip. Renders nothing for non-RotorHazard timers or before the timer has been
  // probed.
  //
  // #386 closes the last step INSIDE GridFPV: RotorHazard imports plugins once at startup, so the
  // folder the RD just dropped in is inert until RH re-executes — and RH exposes that restart,
  // unauthenticated, on the socket the Director is already holding. The guide's **Restart timer**
  // action fires it, so the RD never has to leave for RotorHazard's own web UI. It is confirmed
  // before firing (it restarts the RD's timing hardware) and the Director REFUSES it outright while
  // a race is in progress on the timer — the refusal names the heat. Only `restart_server` is
  // wired; its `shutdown_pi` / `reboot_pi` neighbours stay out of reach.
  import { Badge, Button, Dialog, toast } from '@gridfpv/components';
  import type { Timer } from '@gridfpv/types';
  import ConfirmButton from '../lib/ConfirmButton.svelte';
  import type { Session } from '../lib/session.svelte.js';
  import { pluginBundleUrl, pluginView } from '../lib/pluginPresence.js';

  let {
    timer,
    baseUrl,
    session
  }: {
    timer: Timer;
    baseUrl: string;
    /**
     * The console session, for the RD-gated restart (#386). Optional so the callout still renders
     * (chip + guide + download) wherever a session isn't handy; without it the restart action is
     * simply not offered rather than firing an ungated request.
     */
    session?: Session;
  } = $props();

  const view = $derived(pluginView(timer));
  let open = $state(false);

  /** The name the bundle is served (and saved) under — `Content-Disposition` matches. */
  const BUNDLE_FILE = 'gridfpv-plugin.zip';

  /** In-flight guard + the last outcome, rendered **inside** the dialog (see `status`). */
  let downloading = $state(false);
  let status = $state<{ tone: 'ok' | 'bad' | 'info'; message: string } | null>(null);

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

  // ── Restart the timer (#386) ────────────────────────────────────────────────────────────────
  //
  // `awaitingReconnect` is what keeps the expected drop from reading as a fault. RotorHazard
  // re-executes, so the socket legitimately goes down and the timer passes through
  // `Disconnected` / `Error` / `Connecting` for a few seconds before the Director's reconnect
  // re-probes the plugin. While this flag is set, those states are narrated as a restart in
  // progress; when the probe comes back `Present` this whole `needsInstall` branch (dialog
  // included) is replaced by the quiet ✓ chip, which is the success signal.
  let restarting = $state(false);
  let awaitingReconnect = $state(false);

  /** Whether the timer's connection is currently down — expected, mid-restart. */
  const reconnecting = $derived(
    timer.status === 'Disconnected' || timer.status === 'Error' || timer.status === 'Connecting'
  );

  // Clear the restart narration once the timer is actually back. Without this, `awaitingReconnect`
  // stayed set forever: if the restart did NOT fix the plugin — a wrongly-nested folder (step 2
  // warns about exactly that) or a `live_pass` self-check failure — this branch keeps rendering and
  // every later blip is narrated "waiting for it to come back… this is expected", telling the RD a
  // real, persistent fault is normal. Reaching `Connected` again is the signal the restart round
  // trip is over, whatever its outcome; if the plugin is now present the whole branch disappears
  // anyway, and if it is not, the RD sees the install guide rather than a reassuring message.
  $effect(() => {
    if (awaitingReconnect && timer.status === 'Connected') awaitingReconnect = false;
  });

  /**
   * Whether **Restart timer** can fire: we need a session to make the RD-gated call, and the
   * Director only accepts a restart on a live connection (there is no socket to emit on otherwise).
   * The race-phase refusal is the Director's — it owns the event log — and comes back as a named
   * error rather than being second-guessed here.
   */
  const canRestart = $derived(!!session && timer.status === 'Connected');

  async function restart() {
    if (!session || restarting) return;
    restarting = true;
    status = null;
    try {
      const updated = await session.restartTimer(timer.id);
      if (!updated) {
        status = { tone: 'bad', message: 'A control token is required to restart a timer.' };
        return;
      }
      awaitingReconnect = true;
      // The timer is NAMED, never its URL or id (repo display rule).
      const message =
        `Restarting “${timer.name}”. It drops off for a few seconds and reconnects on its own — ` +
        'the plugin badge turns green once it is back with the plugin loaded.';
      status = { tone: 'info', message };
      toast.info(message, 'Timer restarting');
    } catch (err) {
      // The Director's refusals (a heat in progress, a timer that is not connected) arrive already
      // phrased for the RD and naming the heat — surface them verbatim.
      const reason = err instanceof Error ? err.message : String(err);
      status = { tone: 'bad', message: reason };
      toast.error(reason, 'Restart refused');
    } finally {
      restarting = false;
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
            Copy the <code>gridfpv</code> folder into RotorHazard's <code>plugins/</code> directory
            (usually <code>~/rh-data/plugins/</code> — see below), so you end up with
            <code>plugins/gridfpv/</code> holding <code>__init__.py</code> and
            <code>manifest.json</code> <em>directly</em> inside it — no extra folder in between.
          </li>
          <li>
            Restart RotorHazard with <strong>Restart timer</strong> below — no need to open
            RotorHazard's own web interface. It drops off for a few seconds, reconnects by itself,
            and the badge turns green. (Restarting is refused while a race is staged, armed or
            running.)
          </li>
        </ol>
        <!--
          Where `plugins/` lives (#385). RotorHazard resolves its data dir through a six-step
          cascade, so no single path is right for everyone — name the two common ones, say how to
          find a custom one, and (the actual field sticking point) say the folder may not exist.
        -->
        <details class="where">
          <summary>Where is RotorHazard's <code>plugins/</code> folder?</summary>
          <p>
            It lives in RotorHazard's <strong>data directory</strong>, which depends on how RH was
            installed:
          </p>
          <ul>
            <li>
              <strong>Usually <code>~/rh-data/plugins/</code></strong> — on a Raspberry Pi that is
              <code>/home/pi/rh-data/plugins/</code>. This is the typical default install, not a
              guarantee.
            </li>
            <li>
              <strong>Older, in-place installs:</strong>
              <code>&lt;RotorHazard&gt;/src/server/plugins/</code>.
            </li>
            <li>
              <strong>Custom or vendor timers</strong> (NuclearHazard and friends) sit somewhere
              else again. Whatever the layout, it is the <code>plugins/</code> folder beside
              RotorHazard's
              <code>config.json</code> and <code>database.db</code> — and RH logs
              <code>Data path: …</code> in its startup log.
            </li>
          </ul>
          <p>
            <strong>The folder often doesn't exist yet.</strong> RotorHazard only looks for it, and
            a fresh install with no user plugins has none — if there's no <code>plugins/</code> in the
            data directory, create it yourself.
          </p>
          <p>
            RotorHazard's own guide:
            <a
              href="https://github.com/RotorHazard/RotorHazard/blob/v4.3.0/doc/Plugins.md"
              target="_blank"
              rel="noreferrer">doc/Plugins.md</a
            >.
          </p>
        </details>
        {#if status}
          <p
            class="status"
            class:bad={status.tone === 'bad'}
            class:info={status.tone === 'info'}
            role="status"
          >
            {status.message}
          </p>
        {/if}
        <!-- The expected drop, narrated as progress rather than a fault: RotorHazard is re-executing,
             so `Disconnected` / `Error` / `Connecting` here is the restart working, not a failure. -->
        {#if awaitingReconnect && reconnecting}
          <p class="status info" role="status">
            <!-- Named, never its URL (CLAUDE.md). -->
            Waiting for <strong>{timer.name}</strong> to come back up… this is expected right after a
            restart.
          </p>
        {/if}
      </div>
      {#snippet footer()}
        <Button variant="ghost" onclick={() => (open = false)}>Close</Button>
        {#if session}
          <!-- Confirmed before firing: this restarts the RD's timing hardware. The Director
               additionally REFUSES it while a race is in progress — the gate is heat phase, not
               this dialog. -->
          <ConfirmButton
            variant="danger"
            disabled={!canRestart || restarting}
            title={canRestart
              ? `Restart RotorHazard on “${timer.name}” so it loads the plugin`
              : `“${timer.name}” must be connected before it can be restarted`}
            onconfirm={restart}
          >
            Restart timer
          </ConfirmButton>
        {/if}
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
  .install-guide .where {
    margin-top: 0.9rem;
    font-size: 0.92em;
  }
  .install-guide .where summary {
    cursor: pointer;
    font-weight: 600;
  }
  .install-guide .where p,
  .install-guide .where ul {
    margin: 0.5rem 0 0;
  }
  .install-guide .where ul {
    padding-left: 1.25rem;
    line-height: 1.6;
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
  .install-guide .status.info {
    border-color: var(--gf-info, var(--gf-accent));
    background: var(--gf-info-soft, transparent);
    color: var(--gf-info, var(--gf-accent));
  }
  .install-guide .status.bad {
    border-color: var(--gf-danger);
    background: var(--gf-danger-soft);
    color: var(--gf-danger);
  }
</style>
