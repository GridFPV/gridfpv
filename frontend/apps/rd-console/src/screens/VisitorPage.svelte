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
   * Opened as `#/kiosk` the page is a **kiosk**: a shared tablet at the field (§11.4). A
   * pilot types their code to act, and is signed out again when they press Done or walk
   * away. Nobody stays signed in on a kiosk, and it never signs a pilot's own phone out.
   *
   * A director who is using another device signs in here with the access token. The
   * Director sets a cookie, the page loads again, and this time it is the console.
   */
  import { untrack } from 'svelte';
  import { Button, Card, Input } from '@gridfpv/components';
  import type { PublicEventName, PublicHello, PublicPilot, SeatChannel } from '@gridfpv/types';
  import { isKioskHash, kioskLeft, kioskWarning, typedCode } from '../lib/kiosk.js';
  import { codeInHash, codeProblem, tidyCode } from '../lib/pilotAccess.js';
  import { EventLive } from '../lib/eventLive.svelte.js';
  import type { Session } from '../lib/session.svelte.js';
  import type { PublicSource } from '../lib/spectator.js';
  import SpectatorView from './SpectatorView.svelte';

  /** How often to ask which event is open, in milliseconds. */
  const EVENT_POLL_MS = 5000;
  /** How often to ask which channel each seat is on. They change rarely. */
  const CHANNEL_POLL_MS = 20_000;

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
  /** Whether this page is a kiosk. It follows the address. */
  let kiosk = $state(isKioskHash(location.hash));
  $effect(() => {
    const follow = () => (kiosk = isKioskHash(location.hash));
    window.addEventListener('hashchange', follow);
    return () => window.removeEventListener('hashchange', follow);
  });

  /** Who this browser is signed in as. Starts from what the Director said at load. */
  let ownPilot = $state<PublicPilot | undefined>(untrack(() => hello.pilot));
  /** The pilot at the kiosk, with the pass that lets the page act for them. */
  let atKiosk = $state<{ pilot: PublicPilot; pass: string } | undefined>(undefined);
  // A kiosk goes by who typed their code, never by who the tablet's browser is signed in as.
  const pilot = $derived(kiosk ? atKiosk?.pilot : ownPilot);
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
      ownPilot = await session.signInPilot(tidyCode(typed));
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
      ownPilot = undefined;
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

  // Which channel each seat is on. Asked for while an event is open, and again when
  // another one is. A failure keeps what was known: channels are a help, not the race.
  let channels = $state.raw<SeatChannel[]>([]);
  $effect(() => {
    if (openId === undefined) {
      channels = [];
      return;
    }
    let stopped = false;
    const ask = async () => {
      try {
        const found = await source.getChannels();
        if (!stopped) channels = found;
      } catch {
        // Keep the last answer.
      }
    };
    void ask();
    const timer = setInterval(ask, CHANNEL_POLL_MS);
    return () => {
      stopped = true;
      clearInterval(timer);
    };
  });

  // ── The kiosk ──────────────────────────────────────────────────────────────
  let kioskCode = $state('');
  let kioskBusy = $state(false);
  let kioskError = $state<string | undefined>(undefined);
  let kioskTried = $state(false);
  /** Said once after the page signed a pilot out by itself. */
  let kioskNote = $state<string | undefined>(undefined);
  const kioskTrouble = $derived(kioskTried ? codeProblem(typedCode(kioskCode)) : undefined);
  /** When the screen was last touched, and the time now, for the countdown. */
  let lastTouch = $state(Date.now());
  let now = $state(Date.now());
  const left = $derived(kioskLeft(lastTouch, now));
  const warning = $derived(atKiosk ? kioskWarning(left) : undefined);
  let codeBox = $state<HTMLElement | undefined>(undefined);

  /** Put the cursor in the code box, so a handheld scanner types into it. */
  function focusCode() {
    queueMicrotask(() => codeBox?.querySelector('input')?.focus());
  }

  async function kioskSignIn(e: Event) {
    e.preventDefault();
    if (kioskBusy) return;
    kioskTried = true;
    kioskNote = undefined;
    const code = typedCode(kioskCode);
    if (codeProblem(code) !== undefined) return;
    kioskBusy = true;
    kioskError = undefined;
    try {
      const found = await source.kioskIn(code);
      atKiosk = { pilot: found.pilot, pass: found.pass };
      lastTouch = now = Date.now();
    } catch (err) {
      kioskError = err instanceof Error ? err.message : String(err);
    } finally {
      // Whatever happened, the code does not stay on a shared screen. The box is empty
      // again, and an empty box is not a slip to point out.
      kioskCode = '';
      kioskTried = false;
      kioskBusy = false;
    }
  }

  /** Sign the kiosk's pilot out. `note` is why, when the pilot did not ask for it. */
  function kioskDone(note?: string) {
    const was = atKiosk;
    atKiosk = undefined;
    kioskNote = note;
    kioskError = undefined;
    kioskTried = false;
    kioskCode = '';
    focusCode();
    // The page has already forgotten the pass. Telling Grid is tidying up: if it does not
    // arrive, the pass runs out by itself.
    if (was) void source.kioskDone(was.pass).catch(() => {});
  }

  /**
   * Do something for the kiosk's pilot. If Grid no longer knows the pass (it ran out, or
   * the director signed the pilot out), the page signs them out too and says so.
   */
  async function asKiosk(what: (pass: string) => Promise<unknown>) {
    const was = atKiosk;
    if (!was) throw new Error('Type your code first.');
    lastTouch = now = Date.now();
    try {
      await what(was.pass);
    } catch (err) {
      if ((err as { status?: number }).status === 401) {
        if (atKiosk?.pass === was.pass) kioskDone('You were signed out. Type your code again.');
        return;
      }
      throw err;
    }
    await live?.reload();
  }

  // While a pilot is at the kiosk: any touch or key keeps them signed in, and the clock
  // signs them out when nobody has touched the screen for long enough.
  $effect(() => {
    if (!atKiosk) return;
    const touched = () => (lastTouch = now = Date.now());
    const events = ['pointerdown', 'keydown', 'wheel', 'touchstart'] as const;
    for (const name of events) window.addEventListener(name, touched, { passive: true });
    const timer = setInterval(() => {
      now = Date.now();
      if (kioskLeft(lastTouch, now) <= 0)
        kioskDone('You were signed out. Nobody touched the screen.');
    }, 500);
    return () => {
      for (const name of events) window.removeEventListener(name, touched);
      clearInterval(timer);
    };
  });
  // Leaving kiosk mode, or the event closing, signs the pilot out.
  $effect(() => {
    if ((!kiosk || openId === undefined) && untrack(() => atKiosk)) kioskDone();
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

<main class="visitor" class:kiosk>
  <h1>Grid<span class="accent">FPV</span></h1>

  {#if offline}
    <p class="offline" role="status" data-testid="visitor-offline">
      Not connected to Grid right now. Trying again.
    </p>
  {/if}
  {#if eventName}
    <p class="event" data-testid="visitor-event">{eventName}</p>
    {#if kiosk}
      <section class="kiosk-bar" aria-label="Kiosk sign-in">
        {#if atKiosk}
          <p class="kiosk-who" data-testid="kiosk-pilot">
            <span>Hi <strong>{atKiosk.pilot.callsign}</strong></span>
            <Button variant="primary" onclick={() => kioskDone()}>Done</Button>
          </p>
          {#if warning}
            <p class="kiosk-warning" role="status" data-testid="kiosk-warning">{warning}</p>
          {/if}
        {:else}
          <form onsubmit={kioskSignIn} aria-label="Kiosk code">
            <label for="kiosk-code">Pilots: type your code to sign up</label>
            <div class="kiosk-row" bind:this={codeBox}>
              <Input
                id="kiosk-code"
                autocomplete="off"
                autocapitalize="characters"
                spellcheck="false"
                placeholder="ABC 234"
                bind:value={kioskCode}
                disabled={kioskBusy}
              />
              <Button variant="primary" type="submit" disabled={kioskBusy}>Go</Button>
            </div>
            {#if kioskTrouble}
              <p class="error" role="alert">{kioskTrouble}</p>
            {:else if kioskError}
              <p class="error" role="alert">{kioskError}</p>
            {:else if kioskNote}
              <p class="muted" role="status" data-testid="kiosk-note">{kioskNote}</p>
            {/if}
          </form>
        {/if}
      </section>
    {/if}
    {#if live}
      <SpectatorView
        {live}
        {pilot}
        {channels}
        landOn={kiosk ? 'signup' : 'me'}
        signupHow={kiosk ? 'Type your code above to take a seat.' : undefined}
        buzz={kiosk ? () => false : undefined}
        signup={kiosk
          ? atKiosk && {
              take: (heat, seat) => asKiosk((pass) => source.takeSpot(heat, seat, pass)),
              next: (phase, seat) => asKiosk((pass) => source.takeNextSpot(phase, seat, pass)),
              leave: (heat) => asKiosk((pass) => source.leaveSpot(heat, pass))
            }
          : pilot && {
              take: (heat, seat) => source.takeSpot(heat, seat).then(() => live?.reload()),
              next: (phase, seat) => source.takeNextSpot(phase, seat).then(() => live?.reload()),
              leave: (heat) => source.leaveSpot(heat).then(() => live?.reload())
            }}
      />
    {/if}
  {:else}
    <Card elevation="sm">
      <p class="event" data-testid="visitor-event">No event is open</p>
      <p>You are connected. When the director opens an event, it will show here.</p>
    </Card>
  {/if}
  {#if kiosk}
    <!-- Nothing here: on a kiosk the sign-in is at the top, and nobody stays signed in. -->
  {:else if pilot}
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
  {#if !pilot && !kiosk}
    <p class="muted">You can look. The director runs the race from the Grid computer.</p>
  {/if}

  {#if hello.director_sign_in && !kiosk}
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
  /* The kiosk is read from arm's length, by someone standing. */
  .kiosk {
    font-size: 1.15rem;
  }
  .kiosk-bar {
    border: 2px solid var(--gf-accent);
    border-radius: var(--gf-radius-md);
    padding: var(--gf-space-3) var(--gf-space-4);
    margin-bottom: var(--gf-space-3);
  }
  .kiosk-bar label {
    display: block;
    font-weight: var(--gf-font-weight-semibold);
    margin-bottom: var(--gf-space-2);
  }
  .kiosk-row {
    display: flex;
    gap: var(--gf-space-2);
    align-items: center;
    max-width: 24rem;
  }
  .kiosk-who {
    margin: 0;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-3);
    font-size: var(--gf-font-size-xl);
    overflow-wrap: anywhere;
  }
  .kiosk-warning {
    margin: var(--gf-space-2) 0 0;
    color: var(--gf-warn, var(--gf-danger));
    font-weight: var(--gf-font-weight-semibold);
  }
  .kiosk-bar .error,
  .kiosk-bar .muted {
    margin: var(--gf-space-2) 0 0;
  }
</style>
