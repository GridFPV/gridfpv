<script lang="ts">
  /**
   * RD login (#51) — bearer-token sign-in. The RD enters the Director base URL and a
   * bearer token (protocol.html §9.4); the token is held in memory / sessionStorage by
   * the `Session` (never persisted to disk) and feeds both the read and control seams.
   * Keyboard-friendly: a plain form that submits on Enter.
   */
  import { untrack } from 'svelte';
  import type { Session } from '../lib/session.svelte.js';

  let { session }: { session: Session } = $props();

  // The console is served BY the Director, so the Director is this page's own origin —
  // default to it (prevents the "localhost from the wrong machine" Failed-to-fetch trap).
  let baseUrl = $state(
    globalThis.location?.origin || untrack(() => session.baseUrl) || 'http://localhost:8080'
  );
  let token = $state('');

  function submit(e: Event) {
    e.preventDefault();
    if (!baseUrl.trim() || !token.trim()) return;
    session.login(baseUrl.trim(), token.trim());
  }
</script>

<div class="login">
  <form onsubmit={submit} aria-label="Sign in">
    <h1>GridFPV — RD console</h1>
    <p class="muted">Sign in with the Director address and your control token.</p>
    <label>
      Director address
      <input
        type="url"
        bind:value={baseUrl}
        placeholder="http://localhost:8080"
        autocomplete="off"
      />
    </label>
    <label>
      Control token
      <input type="password" bind:value={token} placeholder="bearer token" autocomplete="off" />
    </label>
    <button type="submit" disabled={!baseUrl.trim() || !token.trim()}>Sign in</button>
  </form>
</div>

<style>
  .login {
    display: grid;
    place-items: center;
    min-height: 100vh;
    background: var(--gf-color-surface-alt);
  }
  form {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
    padding: var(--gf-space-8);
    width: min(26rem, 90vw);
    background: var(--gf-color-surface);
    border: 1px solid var(--gf-color-border);
    border-radius: var(--gf-radius-md);
  }
  h1 {
    font-size: var(--gf-font-size-lg);
    margin: 0;
  }
  .muted {
    color: var(--gf-color-text-muted);
    font-size: var(--gf-font-size-sm);
    margin: 0;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
    font-size: var(--gf-font-size-sm);
  }
  input {
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-md);
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-color-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-color-surface);
    color: var(--gf-color-text);
  }
  button {
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-md);
    padding: var(--gf-space-3) var(--gf-space-4);
    border-radius: var(--gf-radius-sm);
    border: 1px solid var(--gf-color-accent);
    background: var(--gf-color-accent);
    color: var(--gf-color-accent-contrast);
    cursor: pointer;
  }
  button:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
</style>
