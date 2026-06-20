<script lang="ts">
  /**
   * RD login (#51, restyled by #71) — bearer-token sign-in. The RD enters the
   * Director base URL and a bearer token (protocol.html §9.4); the token is held
   * in memory / sessionStorage by the `Session` (never persisted to disk) and
   * feeds both the read and control seams. Keyboard-friendly: a plain form that
   * submits on Enter. Styled as a centered "auth card" on the dark canvas.
   */
  import { untrack } from 'svelte';
  import { Button, Input } from '@gridfpv/components';
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
  <div class="auth-card">
    <div class="brand">
      <span class="logo" aria-hidden="true">
        <svg viewBox="0 0 32 32" width="34" height="34">
          <rect x="2" y="2" width="28" height="28" rx="8" fill="var(--gf-accent-soft)" />
          <path
            d="M16 6 L25 11 L25 21 L16 26 L7 21 L7 11 Z"
            fill="none"
            stroke="var(--gf-accent)"
            stroke-width="2"
            stroke-linejoin="round"
          />
          <circle cx="16" cy="16" r="3" fill="var(--gf-accent)" />
        </svg>
      </span>
      <div class="brand-text">
        <span class="name">GridFPV</span>
        <span class="kicker">Race Director Console</span>
      </div>
    </div>

    <form onsubmit={submit} aria-label="Sign in">
      <h1>Sign in</h1>
      <p class="muted">Connect to your Director with its address and your control token.</p>

      <label class="field">
        <span class="field-label">Director address</span>
        <Input
          type="url"
          bind:value={baseUrl}
          placeholder="http://localhost:8080"
          autocomplete="off"
        />
      </label>
      <label class="field">
        <span class="field-label">Control token</span>
        <Input type="password" bind:value={token} placeholder="bearer token" autocomplete="off" />
      </label>

      <Button
        type="submit"
        variant="primary"
        size="lg"
        block
        disabled={!baseUrl.trim() || !token.trim()}
      >
        Sign in
      </Button>
    </form>
  </div>
</div>

<style>
  .login {
    display: grid;
    place-items: center;
    min-height: 100vh;
    padding: var(--gf-space-6);
  }
  .auth-card {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-6);
    width: min(25rem, 92vw);
    padding: var(--gf-space-8);
    background: var(--gf-elevated);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-xl);
    box-shadow: var(--gf-shadow-lg), var(--gf-shadow-inset);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
  }
  .brand-text {
    display: flex;
    flex-direction: column;
    line-height: 1.15;
  }
  .brand-text .name {
    font-weight: var(--gf-font-weight-bold);
    font-size: var(--gf-font-size-lg);
    letter-spacing: var(--gf-tracking-tight);
  }
  .brand-text .kicker {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-muted);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
  }
  form {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  h1 {
    font-size: var(--gf-font-size-xl);
    margin: 0;
    letter-spacing: var(--gf-tracking-tight);
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    margin: calc(-1 * var(--gf-space-2)) 0 var(--gf-space-2);
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .field-label {
    font-size: var(--gf-font-size-xs);
    font-weight: var(--gf-font-weight-semibold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
</style>
