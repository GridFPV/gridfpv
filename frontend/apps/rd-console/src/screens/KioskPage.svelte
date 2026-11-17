<script lang="ts">
  /**
   * KioskPage — a tablet at the race director's stand (docs/primitives.md §11.4).
   *
   * Three screens, one at a time:
   *
   * 1. **Not a kiosk yet.** The director types a kiosk code from the console's Network
   *    page. The code is used up and this tablet is a kiosk from then on.
   * 2. **Waiting for a pilot.** Nothing but the event's name and a box for a pilot's code.
   * 3. **A pilot is here.** The sign-up sheet, large, for a tablet on its side. **Done**
   *    signs them out. So does walking away: after a while with no touch the page counts
   *    down and signs them out by itself.
   *
   * Nobody stays signed in on a kiosk, and it never signs a pilot's own phone out.
   */
  import { Button, Input } from '@gridfpv/components';
  import type { PublicPilot } from '@gridfpv/types';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import { kioskCodeProblem, kioskLeft, kioskWarning, typedCode } from '../lib/kiosk.js';
  import { codeProblem, tidyCode } from '../lib/pilotAccess.js';
  import { signupPhases } from '../lib/signup.js';
  import type { PublicSource } from '../lib/spectator.js';
  import PilotSignup from './PilotSignup.svelte';

  let {
    source,
    isKiosk,
    eventName,
    live,
    kioskName = undefined,
    offline = false
  }: {
    source: Pick<
      PublicSource,
      'kioskSetUp' | 'kioskIn' | 'kioskDone' | 'takeSpot' | 'takeNextSpot' | 'leaveSpot'
    >;
    /** Whether the Director said, when the page loaded, that this device is a kiosk. */
    isKiosk: boolean;
    /** The open event's name. Left out when none is open. */
    eventName: string | undefined;
    /** The open event, live. Left out when none is open. */
    live: Pick<EventLive, 'view' | 'names' | 'reload'> | undefined;
    /** What the director calls this kiosk, when the Director has said. */
    kioskName?: string | undefined;
    /** Whether Grid cannot be reached right now. */
    offline?: boolean;
  } = $props();

  // ── 1. Setting the tablet up ───────────────────────────────────────────────
  // svelte-ignore state_referenced_locally
  let ready = $state(isKiosk);
  let setupCode = $state('');
  let setupBusy = $state(false);
  let setupError = $state<string | undefined>(undefined);
  let setupTried = $state(false);
  const setupTrouble = $derived(setupTried ? kioskCodeProblem(setupCode) : undefined);

  async function setUp(e: Event) {
    e.preventDefault();
    if (setupBusy) return;
    setupTried = true;
    setupError = undefined;
    if (kioskCodeProblem(setupCode) !== undefined) return;
    setupBusy = true;
    try {
      await source.kioskSetUp(tidyCode(setupCode));
      ready = true;
      focusCode();
    } catch (err) {
      setupError = err instanceof Error ? err.message : String(err);
    } finally {
      setupCode = '';
      setupTried = false;
      setupBusy = false;
    }
  }

  // ── 2 and 3. A pilot ───────────────────────────────────────────────────────
  /** The pilot at the kiosk, with the pass that lets the page act for them. */
  let here = $state<{ pilot: PublicPilot; pass: string } | undefined>(undefined);
  let code = $state('');
  let busy = $state(false);
  let error = $state<string | undefined>(undefined);
  let tried = $state(false);
  /** Said once after the page signed a pilot out by itself. */
  let note = $state<string | undefined>(undefined);
  const trouble = $derived(tried ? codeProblem(typedCode(code)) : undefined);
  /** When the screen was last touched, and the time now, for the countdown. */
  let lastTouch = $state(Date.now());
  let now = $state(Date.now());
  const warning = $derived(here ? kioskWarning(kioskLeft(lastTouch, now)) : undefined);
  let codeBox = $state<HTMLElement | undefined>(undefined);

  /** Put the cursor in the code box, so a handheld scanner types into it. */
  function focusCode() {
    queueMicrotask(() => codeBox?.querySelector('input')?.focus());
  }

  async function signIn(e: Event) {
    e.preventDefault();
    if (busy) return;
    tried = true;
    note = undefined;
    const typed = typedCode(code);
    if (codeProblem(typed) !== undefined) return;
    busy = true;
    error = undefined;
    try {
      const found = await source.kioskIn(typed);
      here = { pilot: found.pilot, pass: found.pass };
      lastTouch = now = Date.now();
    } catch (err) {
      // The director removed this kiosk: go back to setting it up.
      if ((err as { status?: number }).status === 403) ready = false;
      else error = err instanceof Error ? err.message : String(err);
    } finally {
      // Whatever happened, the code does not stay on a shared screen. The box is empty
      // again, and an empty box is not a slip to point out.
      code = '';
      tried = false;
      busy = false;
    }
  }

  /** Sign the pilot out. `why` is said when the pilot did not ask for it. */
  function done(why?: string) {
    const was = here;
    here = undefined;
    note = why;
    error = undefined;
    tried = false;
    code = '';
    focusCode();
    // The page has already forgotten the pass. Telling Grid is tidying up: if it does not
    // arrive, the pass runs out by itself.
    if (was) void source.kioskDone(was.pass).catch(() => {});
  }

  /**
   * Do something for the pilot. If Grid no longer knows the pass (it ran out, or the
   * director signed the pilot out), the page signs them out too and says so.
   */
  async function act(what: (pass: string) => Promise<unknown>) {
    const was = here;
    if (!was) throw new Error('Type your code first.');
    lastTouch = now = Date.now();
    try {
      await what(was.pass);
    } catch (err) {
      if ((err as { status?: number }).status === 401) {
        if (here?.pass === was.pass) done('You were signed out. Type your code again.');
        return;
      }
      throw err;
    }
    await live?.reload();
  }

  // While a pilot is here: any touch or key keeps them signed in, and the clock signs
  // them out when nobody has touched the screen for long enough.
  $effect(() => {
    if (!here) return;
    const touched = () => (lastTouch = now = Date.now());
    const events = ['pointerdown', 'keydown', 'wheel', 'touchstart'] as const;
    for (const name of events) window.addEventListener(name, touched, { passive: true });
    const timer = setInterval(() => {
      now = Date.now();
      if (kioskLeft(lastTouch, now) <= 0) {
        done('You were signed out. Nobody touched the screen.');
      }
    }, 500);
    return () => {
      for (const name of events) window.removeEventListener(name, touched);
      clearInterval(timer);
    };
  });
  // The event closing signs the pilot out.
  $effect(() => {
    if (eventName === undefined && here) done();
  });

  const sheets = $derived(signupPhases(live?.view));
</script>

<main class="kiosk" data-screen={!ready ? 'setup' : here ? 'pilot' : 'wait'}>
  <header class="top">
    <h1>Grid<span class="accent">FPV</span></h1>
    {#if eventName}<p class="event" data-testid="visitor-event">{eventName}</p>{/if}
    {#if ready && kioskName && !here}
      <p class="kiosk-name" data-testid="kiosk-name">{kioskName}</p>
    {/if}
    {#if here}
      <p class="who" data-testid="kiosk-pilot">
        <span>Hi <strong>{here.pilot.callsign}</strong></span>
        <Button variant="primary" onclick={() => done()}>Done</Button>
      </p>
    {/if}
  </header>

  {#if offline}
    <p class="notice" role="status" data-testid="visitor-offline">
      Not connected to Grid right now. Trying again.
    </p>
  {/if}

  {#if !ready}
    <section class="card" aria-label="Set up this kiosk">
      <h2>Set up this kiosk</h2>
      <p class="muted">
        For the race director. Make a kiosk code on the Network page of the Grid computer and type
        it here. A code works once.
      </p>
      <form onsubmit={setUp} aria-label="Set up kiosk">
        <label for="kiosk-setup">Kiosk code</label>
        <div class="row">
          <Input
            id="kiosk-setup"
            autocomplete="off"
            autocapitalize="characters"
            spellcheck="false"
            placeholder="ABCD 2345"
            bind:value={setupCode}
            disabled={setupBusy}
          />
          <Button variant="primary" type="submit" disabled={setupBusy}>Set up</Button>
        </div>
        {#if setupTrouble}
          <p class="error" role="alert">{setupTrouble}</p>
        {:else if setupError}
          <p class="error" role="alert">{setupError}</p>
        {/if}
      </form>
    </section>
  {:else if eventName === undefined}
    <section class="card">
      <h2 data-testid="visitor-event">No event is open</h2>
      <p class="muted">When the director opens an event, pilots can sign up here.</p>
    </section>
  {:else if !here}
    <section class="card" aria-label="Pilot sign-in">
      <form onsubmit={signIn} aria-label="Pilot code">
        <label for="kiosk-code">Type your code</label>
        <div class="row" bind:this={codeBox}>
          <Input
            id="kiosk-code"
            autocomplete="off"
            autocapitalize="characters"
            spellcheck="false"
            placeholder="ABC 234"
            bind:value={code}
            disabled={busy}
          />
          <Button variant="primary" type="submit" disabled={busy}>Go</Button>
        </div>
        {#if trouble}
          <p class="error" role="alert">{trouble}</p>
        {:else if error}
          <p class="error" role="alert">{error}</p>
        {:else if note}
          <p class="muted" role="status" data-testid="kiosk-note">{note}</p>
        {/if}
      </form>
    </section>
  {:else}
    {#if warning}
      <p class="warning" role="status" data-testid="kiosk-warning">{warning}</p>
    {/if}
    {#if !live?.view}
      <p class="muted" role="status">Loading…</p>
    {:else}
      <PilotSignup
        phases={sheets}
        names={live.names}
        pilot={here.pilot}
        layout="grid"
        actions={{
          take: (heat, seat) => act((pass) => source.takeSpot(heat, seat, pass)),
          next: (phase, seat) => act((pass) => source.takeNextSpot(phase, seat, pass)),
          leave: (heat) => act((pass) => source.leaveSpot(heat, pass))
        }}
      />
    {/if}
  {/if}
</main>

<style>
  /* Read from arm's length by someone standing, on a tablet on its side. */
  .kiosk {
    max-width: 76rem;
    margin: 0 auto;
    padding: var(--gf-space-4) var(--gf-space-5);
    display: grid;
    gap: var(--gf-space-4);
    font-family: var(--gf-font-family);
    font-size: 1.2rem;
  }
  .top {
    display: flex;
    align-items: center;
    gap: var(--gf-space-4);
    flex-wrap: wrap;
  }
  h1 {
    margin: 0;
    font-size: var(--gf-font-size-xl);
  }
  .accent {
    color: var(--gf-accent);
  }
  .event {
    margin: 0;
    font-size: var(--gf-font-size-lg);
    color: var(--gf-text-muted);
    overflow-wrap: anywhere;
  }
  .kiosk-name {
    margin: 0 0 0 auto;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    overflow-wrap: anywhere;
  }
  .who {
    margin: 0 0 0 auto;
    display: flex;
    align-items: center;
    gap: var(--gf-space-4);
    font-size: var(--gf-font-size-xl);
    overflow-wrap: anywhere;
  }
  .who :global(button) {
    min-width: 7rem;
    min-height: 3rem;
    font-size: var(--gf-font-size-lg);
  }
  /* Waiting for a code, the box is the whole page. */
  [data-screen='wait'],
  [data-screen='setup'] {
    min-height: 100vh;
    align-content: start;
  }
  .card {
    justify-self: center;
    width: min(100%, 32rem);
    margin-top: 12vh;
    border: 2px solid var(--gf-accent);
    border-radius: var(--gf-radius-md);
    padding: var(--gf-space-5);
  }
  h2 {
    margin: 0 0 var(--gf-space-2);
    font-size: var(--gf-font-size-xl);
  }
  form {
    display: grid;
    gap: var(--gf-space-3);
  }
  label {
    font-size: var(--gf-font-size-xl);
    font-weight: var(--gf-font-weight-semibold);
  }
  .row {
    display: flex;
    gap: var(--gf-space-2);
    align-items: stretch;
  }
  .row :global(input) {
    font-size: var(--gf-font-size-xl);
    letter-spacing: 0.1em;
    min-height: 3.5rem;
  }
  .row :global(button) {
    min-width: 6rem;
    min-height: 3.5rem;
    font-size: var(--gf-font-size-lg);
  }
  p {
    margin: 0;
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-md, 1rem);
  }
  .error {
    color: var(--gf-danger);
  }
  .notice {
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-warn);
    border-radius: var(--gf-radius-sm);
    color: var(--gf-warn);
  }
  .warning {
    padding: var(--gf-space-2) var(--gf-space-3);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-warn-soft);
    color: var(--gf-warn);
    font-weight: var(--gf-font-weight-semibold);
    font-size: var(--gf-font-size-lg);
  }
</style>
