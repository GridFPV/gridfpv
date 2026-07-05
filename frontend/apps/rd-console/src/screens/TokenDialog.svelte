<script lang="ts">
  /**
   * TokenDialog — the lazy RD-token prompt (#72, Slice 1b).
   *
   * Reading and browsing events needs no token; the RD token is requested **only when a
   * privileged action needs it** (create event, or the first control command). The shell
   * registers {@link Session.setTokenProvider} with a function that opens this dialog and
   * resolves with the entered token (or `undefined` if cancelled), so the ask is an
   * occasional, in-context prompt — not a login wall.
   *
   * This is forward-compatible with the coming loopback-trust auth slice: when the
   * Director trusts a localhost caller, the token provider simply returns immediately and
   * this dialog never opens. (Loopback-trust is NOT built here.)
   */
  import { Button, Dialog, Field, Input } from '@gridfpv/components';

  let { open = $bindable(false) }: { open?: boolean } = $props();

  let token = $state('');
  // The pending resolve from the active `request()` call.
  let resolver: ((value: string | undefined) => void) | undefined;

  /** Open the dialog and resolve with the entered token, or `undefined` if cancelled. */
  export function request(): Promise<string | undefined> {
    token = '';
    open = true;
    return new Promise((resolve) => {
      resolver = resolve;
    });
  }

  function settle(value: string | undefined) {
    open = false;
    resolver?.(value);
    resolver = undefined;
  }

  function submit(e: Event) {
    e.preventDefault();
    const t = token.trim();
    if (!t) return;
    settle(t);
  }

  function onclose() {
    // Native close (Esc / backdrop) counts as a cancel if still pending.
    if (resolver) settle(undefined);
  }
</script>

<Dialog bind:open title="Control token" {onclose}>
  <form class="token-form" onsubmit={submit} aria-label="Control token">
    <p class="lede">
      This action writes to the event, so it needs your <strong>RD control token</strong>. It's kept
      for this session only and never saved to disk.
    </p>
    <Field label="Control token">
      <Input
        type="password"
        bind:value={token}
        placeholder="bearer token"
        aria-label="Control token"
        autocomplete="off"
      />
    </Field>
  </form>
  {#snippet footer()}
    <Button variant="ghost" onclick={() => settle(undefined)}>Cancel</Button>
    <Button variant="primary" onclick={submit} disabled={!token.trim()}>Use token</Button>
  {/snippet}
</Dialog>

<style>
  .token-form {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .lede {
    margin: 0;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .lede strong {
    color: var(--gf-text);
    font-weight: var(--gf-font-weight-semibold);
  }
</style>
