<script lang="ts">
  /**
   * VisitorPage — what someone on another device sees when they open Grid
   * (docs/primitives.md §11).
   *
   * A visitor is not the director: the console is not for them, and the Director would
   * refuse everything it asks. So they get this page instead. For now it says which event
   * is open; the pages for spectators and pilots are built on top of it.
   *
   * A director who is using another device signs in here with the access token. The
   * Director sets a cookie, the page loads again, and this time it is the console.
   */
  import { Button, Card, Input } from '@gridfpv/components';
  import type { PublicHello } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';

  let {
    session,
    hello,
    reload = () => location.reload()
  }: {
    session: Pick<Session, 'signInDirector'>;
    hello: PublicHello;
    /** Load the page again after signing in. A test passes its own. */
    reload?: () => void;
  } = $props();

  let asking = $state(false);
  let token = $state('');
  let busy = $state(false);
  let error = $state<string | undefined>(undefined);

  async function signIn(e: Event) {
    e.preventDefault();
    if (busy || token.trim() === '') return;
    busy = true;
    error = undefined;
    try {
      await session.signInDirector(token.trim());
      token = '';
      reload();
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }
</script>

<main class="visitor">
  <h1>Grid<span class="accent">FPV</span></h1>

  <Card elevation="sm">
    {#if hello.event}
      <p class="event" data-testid="visitor-event">{hello.event}</p>
      <p>You are connected. The race will show here.</p>
    {:else}
      <p class="event" data-testid="visitor-event">No event is open</p>
      <p>You are connected. When the director opens an event, it will show here.</p>
    {/if}
    <p class="muted">You can look. The director runs the race from the Grid computer.</p>
  </Card>

  {#if hello.director_sign_in}
    {#if asking}
      <Card title="Director sign-in" elevation="sm">
        <form onsubmit={signIn} aria-label="Director sign-in">
          <label for="director-token">Access token</label>
          <Input
            id="director-token"
            type="password"
            autocomplete="off"
            bind:value={token}
            disabled={busy}
          />
          {#if error}<p class="error" role="alert">{error}</p>{/if}
          <div class="row">
            <Button variant="primary" type="submit" disabled={busy || token.trim() === ''}>
              Sign in
            </Button>
            <Button variant="ghost" onclick={() => ((asking = false), (error = undefined))}>
              Cancel
            </Button>
          </div>
        </form>
      </Card>
    {:else}
      <p class="director">
        <Button variant="ghost" size="sm" onclick={() => (asking = true)}>I am the director</Button>
      </p>
    {/if}
  {/if}
</main>

<style>
  .visitor {
    max-width: 32rem;
    margin: 0 auto;
    padding: var(--gf-space-5) var(--gf-space-4);
    display: grid;
    gap: var(--gf-space-4);
    font-family: var(--gf-font-family);
  }
  h1 {
    margin: 0;
    font-size: var(--gf-font-size-xl);
  }
  .accent {
    color: var(--gf-accent);
  }
  p {
    margin: 0 0 var(--gf-space-2);
  }
  .event {
    font-size: var(--gf-font-size-xl);
    font-weight: var(--gf-font-weight-bold);
    overflow-wrap: anywhere;
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .director {
    text-align: center;
  }
  form {
    display: grid;
    gap: var(--gf-space-2);
  }
  label {
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
  }
  .row {
    display: flex;
    gap: var(--gf-space-2);
  }
  .error {
    color: var(--gf-danger);
  }
</style>
