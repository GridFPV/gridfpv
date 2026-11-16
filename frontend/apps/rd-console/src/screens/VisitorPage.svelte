<script lang="ts">
  /**
   * VisitorPage — what someone on another device sees when they open Grid
   * (docs/primitives.md §11).
   *
   * A visitor is not the director: the console is not for them, and the Director would
   * refuse everything it asks. So they get this page instead: the open event, to watch
   * (`SpectatorView`).
   *
   * The page asks every few seconds which event is open. When the director opens another
   * one, or closes it, the page follows.
   *
   * A **pilot** signs in here with the code the director gives them, or by scanning its
   * QR code, which opens this page with the code in the address. Signed in, the page knows
   * them by name.
   *
   * A director who is using another device signs in here with the access token. The
   * Director sets a cookie, the page loads again, and this time it is the console.
   */
  import { untrack } from 'svelte';
  import { Button, Card, Input } from '@gridfpv/components';
  import type { PublicEventName, PublicHello, PublicPilot } from '@gridfpv/types';
  import { codeInHash, codeProblem, tidyCode } from '../lib/pilotAccess.js';
  import { EventLive } from '../lib/eventLive.svelte.js';
  import type { Session } from '../lib/session.svelte.js';
  import type { PublicSource } from '../lib/spectator.js';
  import SpectatorView from './SpectatorView.svelte';

  /** How often to ask which event is open, in milliseconds. */
  const EVENT_POLL_MS = 5000;

  let {
    session,
    source,
    hello,
    reload = () => location.reload()
  }: {
    session: Pick<Session, 'signInDirector' | 'signInPilot' | 'signOutPilot'>;
    /** Where the open event is read from: the Director's public routes. */
    source: PublicSource;
    hello: PublicHello;
    /** Load the page again after signing in. A test passes its own. */
    reload?: () => void;
  } = $props();

  /** The event that is open. Starts from what the Director said when the page loaded. */
  let open = $state<PublicEventName | undefined>(undefined);
  /** Whether the Director has answered yet: until then, go by `hello`. */
  let asked = $state(false);
  const eventName = $derived(asked ? open?.name : hello.event);
  /**
   * Whether the last try to reach Grid failed. The page keeps showing what it has, and
   * says that it is not live: a result on a phone that has dropped off the Wi-Fi must not
   * pass for the race as it stands.
   */
  let offline = $state(false);

  $effect(() => {
    let stopped = false;
    const ask = async () => {
      try {
        const found = (await source.getEvent()).event;
        if (stopped) return;
        // Keep the same object while the event is the same, so the live store is not rebuilt.
        if (found?.id !== open?.id || found?.name !== open?.name) open = found;
        asked = true;
        offline = false;
      } catch {
        // Not reachable right now: keep what is shown, say so, and ask again.
        if (!stopped) offline = true;
      }
    };
    void ask();
    const timer = setInterval(ask, EVENT_POLL_MS);
    return () => {
      stopped = true;
      clearInterval(timer);
    };
  });

  // One live store per open event. It lives until another event is opened or the page goes.
  let live = $state.raw<EventLive | undefined>(undefined);
  const openId = $derived(open?.id);
  $effect(() => {
    const id = openId;
    if (id === undefined) {
      live = undefined;
      return;
    }
    const store = new EventLive(source, id, { pollTimers: false });
    live = store;
    store.start();
    return () => store.stop();
  });

  // ── The pilot ──────────────────────────────────────────────────────────────
  /** Who this browser is signed in as. Starts from what the Director said at load. */
  let pilot = $state<PublicPilot | undefined>(untrack(() => hello.pilot));
  let pilotAsking = $state(false);
  let code = $state('');
  let pilotBusy = $state(false);
  let pilotError = $state<string | undefined>(undefined);
  /** Shown only once the pilot has tried to send a code that cannot be one. */
  let codeTried = $state(false);
  const codeTrouble = $derived(codeTried ? codeProblem(code) : undefined);

  async function sendCode(typed: string) {
    if (pilotBusy) return;
    pilotBusy = true;
    pilotError = undefined;
    try {
      pilot = await session.signInPilot(tidyCode(typed));
      code = '';
      codeTried = false;
      pilotAsking = false;
    } catch (err) {
      pilotError = err instanceof Error ? err.message : String(err);
      pilotAsking = true;
    } finally {
      pilotBusy = false;
    }
  }

  function signInPilot(e: Event) {
    e.preventDefault();
    codeTried = true;
    if (codeProblem(code) !== undefined) return;
    void sendCode(code);
  }

  async function signOutPilot() {
    if (pilotBusy) return;
    pilotBusy = true;
    try {
      await session.signOutPilot();
      pilot = undefined;
    } catch (err) {
      pilotError = err instanceof Error ? err.message : String(err);
    } finally {
      pilotBusy = false;
    }
  }

  // A pilot who scanned their QR code arrives with the code in the address. Sign them in
  // with it, and take it out of the address so it is not left in the phone's history or
  // sent on if they share the page.
  $effect(() => {
    const scanned = codeInHash(location.hash);
    if (scanned === undefined) return;
    history.replaceState(null, '', location.pathname + location.search);
    void sendCode(scanned);
  });

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

  {#if offline}
    <p class="offline" role="status" data-testid="visitor-offline">
      Not connected to Grid right now. Trying again.
    </p>
  {/if}
  {#if eventName}
    <p class="event" data-testid="visitor-event">{eventName}</p>
    {#if live}
      <SpectatorView {live} />
    {/if}
  {:else}
    <Card elevation="sm">
      <p class="event" data-testid="visitor-event">No event is open</p>
      <p>You are connected. When the director opens an event, it will show here.</p>
    </Card>
  {/if}
  {#if pilot}
    <p class="who" data-testid="pilot-signed-in">
      Signed in as <strong>{pilot.callsign}</strong>
      <Button size="sm" variant="ghost" disabled={pilotBusy} onclick={signOutPilot}>Sign out</Button
      >
    </p>
  {:else if pilotAsking}
    <Card title="Pilot sign-in" elevation="sm">
      <form onsubmit={signInPilot} aria-label="Pilot sign-in">
        <label for="pilot-code">Your code</label>
        <Input
          id="pilot-code"
          autocomplete="off"
          autocapitalize="characters"
          spellcheck="false"
          placeholder="ABC 234"
          bind:value={code}
          disabled={pilotBusy}
        />
        {#if codeTrouble}
          <p class="error" role="alert">{codeTrouble}</p>
        {:else if pilotError}
          <p class="error" role="alert">{pilotError}</p>
        {/if}
        <p class="muted">The director has your code. It works on one phone at a time.</p>
        <div class="row">
          <Button variant="primary" type="submit" disabled={pilotBusy}>Sign in</Button>
          <Button
            variant="ghost"
            onclick={() => ((pilotAsking = false), (pilotError = undefined), (codeTried = false))}
          >
            Cancel
          </Button>
        </div>
      </form>
    </Card>
  {:else if eventName}
    <p class="director">
      <Button variant="secondary" size="sm" onclick={() => (pilotAsking = true)}>
        I am a pilot
      </Button>
    </p>
  {/if}
  <p class="muted">You can look. The director runs the race from the Grid computer.</p>

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
    max-width: 40rem;
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
    margin: 0;
    font-size: var(--gf-font-size-xl);
    font-weight: var(--gf-font-weight-bold);
    overflow-wrap: anywhere;
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .offline {
    margin: 0;
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-warn);
    border-radius: var(--gf-radius-sm);
    color: var(--gf-warn);
    font-size: var(--gf-font-size-sm);
  }
  .director {
    text-align: center;
    margin: 0;
  }
  .who {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
    margin: 0;
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
