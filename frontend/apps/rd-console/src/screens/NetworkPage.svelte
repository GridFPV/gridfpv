<script lang="ts">
  /**
   * NetworkPage — whether other devices can open Grid (docs/primitives.md §11).
   *
   * Grid starts out answering only the computer it runs on. This page has the one switch
   * that lets phones, tablets and other computers on the same network open it too, and
   * shows the address to type on them.
   *
   * It also says, plainly, what those devices can do: look. Nothing on this page gives a
   * visitor more than that. The rule itself is in the Director (`access.rs`).
   *
   * A Director that was started on the network already (`GRIDFPV_ADDR`) is always on. The
   * switch is hidden then, because it would do nothing.
   */
  import { Badge, Banner, Button, Card, Input, toast } from '@gridfpv/components';
  import type { KioskView, NetworkRequest, NetworkView, PublicHello } from '@gridfpv/types';
  import Page from '../Page.svelte';
  import { kioskLink, setUpWords, spellKioskCode } from '../lib/kiosk.js';
  import { portProblem, reachable, refreshEvery, stateLabel, stateTone } from '../lib/network.js';
  import type { Session } from '../lib/session.svelte.js';

  let {
    session
  }: {
    session: Pick<
      Session,
      | 'getNetwork'
      | 'putNetwork'
      | 'hello'
      | 'signOutDirector'
      | 'getKiosks'
      | 'newKioskCode'
      | 'cancelKioskCode'
      | 'removeKiosk'
    >;
  } = $props();

  let view = $state<NetworkView | undefined>(undefined);
  let hello = $state<PublicHello | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);
  /** The port as typed. Follows the Director's until the director edits it. */
  let portText = $state('');
  let portEdited = $state(false);

  const say = (err: unknown): string => (err instanceof Error ? err.message : String(err));

  function take(next: NetworkView) {
    view = next;
    if (!portEdited) portText = String(next.port);
  }

  async function load() {
    try {
      take(await session.getNetwork());
      loadError = undefined;
    } catch (err) {
      loadError = say(err);
    }
    // A kiosk code that a tablet has just used moves from one list to the other.
    try {
      kiosks = await session.getKiosks();
    } catch {
      // Keep the last list. The network setting above says if Grid cannot be reached.
    }
  }

  // ── Kiosks ─────────────────────────────────────────────────────────────────
  let kiosks = $state<KioskView | undefined>(undefined);
  let kioskBusy = $state(false);
  let kioskError = $state<string | undefined>(undefined);

  async function kioskChange(what: () => Promise<KioskView | undefined>) {
    if (kioskBusy) return;
    kioskBusy = true;
    kioskError = undefined;
    try {
      const next = await what();
      if (next) kiosks = next;
    } catch (err) {
      kioskError = say(err);
    } finally {
      kioskBusy = false;
    }
  }

  // Read it now, and keep reading: the listener starts a moment after the switch, and this
  // computer's address changes when it joins another network.
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  /** Read the setting again after the wait that fits its state, and keep doing so. */
  function schedule() {
    clearTimeout(timer);
    if (stopped) return;
    timer = setTimeout(async () => {
      await load();
      schedule();
    }, refreshEvery(view?.state));
  }
  $effect(() => {
    stopped = false;
    void load().then(schedule);
    void session
      .hello()
      .then((found) => (hello = found))
      .catch(() => {});
    return () => {
      stopped = true;
      clearTimeout(timer);
    };
  });

  async function send(request: NetworkRequest, done?: string) {
    if (busy) return;
    busy = true;
    error = undefined;
    try {
      const next = await session.putNetwork(request);
      if (next) {
        portEdited = false;
        take(next);
        // The wait before the next read depends on the state, which has just changed.
        schedule();
        if (done) toast.success(done);
      }
    } catch (err) {
      error = say(err);
    } finally {
      busy = false;
    }
  }

  const always = $derived(view?.state === 'Always');
  const portTrouble = $derived(portEdited ? portProblem(portText) : undefined);
  const portChanged = $derived(
    portEdited && view !== undefined && portText.trim() !== String(view.port)
  );

  function turn(on: boolean) {
    void send({ enabled: on }, on ? undefined : 'Other devices can no longer open Grid.');
  }
  function savePort(e?: Event) {
    e?.preventDefault();
    if (!view || portProblem(portText)) return;
    void send({ enabled: view.enabled, port: Number(portText.trim()) });
  }

  async function copy(address: string) {
    try {
      await navigator.clipboard.writeText(address);
      toast.success('Copied.');
    } catch {
      toast.error('Could not copy. Select the address and copy it by hand.');
    }
  }

  async function signOut() {
    try {
      await session.signOutDirector();
    } finally {
      location.reload();
    }
  }
</script>

<Page
  title="Network"
  help="network"
  lede="Let phones and other computers on your network open Grid to look."
>
  {#if loadError}
    <Banner tone="danger" title="Grid could not read its network setting.">{loadError}</Banner>
  {/if}
  {#if error}
    <Banner tone="danger">{error}</Banner>
  {/if}

  {#if view}
    <Card title="Other devices" elevation="sm">
      <p class="state">
        <Badge tone={stateTone(view.state)}>
          <span data-testid="network-state">{stateLabel(view.state)}</span>
        </Badge>
        {#if view.state === 'Off'}
          Only this computer can open Grid.
        {:else if view.state === 'Starting'}
          Starting…
        {:else if reachable(view)}
          Other devices on this network can open Grid.
        {/if}
      </p>

      {#if view.problem}
        <Banner tone={view.state === 'Failed' ? 'danger' : 'warn'}>
          <span data-testid="network-problem">{view.problem}</span>
        </Banner>
      {/if}

      {#if always}
        <p>
          Grid was started on the network, so this is always on. To turn it off, start Grid without
          a network address.
        </p>
      {:else}
        <div class="row">
          {#if view.enabled}
            <Button disabled={busy} onclick={() => turn(false)}>Turn off</Button>
          {:else}
            <Button variant="primary" disabled={busy} onclick={() => turn(true)}>
              Let other devices open Grid
            </Button>
          {/if}
        </div>
      {/if}

      {#if reachable(view)}
        <h3>Open this on the other device</h3>
        <ul class="addresses" aria-label="Addresses for other devices">
          {#each view.addresses as address (address)}
            <li>
              <a href={address} target="_blank" rel="noreferrer">{address}</a>
              <Button size="sm" variant="ghost" onclick={() => copy(address)}>Copy</Button>
            </li>
          {/each}
        </ul>
        <p class="muted">
          The device must be on the same Wi-Fi or network as this computer.
          {#if view.addresses.length > 1}
            The first address is the one that usually works. This computer is on more than one
            network, so if it does not open, try the next one.
          {/if}
        </p>
      {/if}

      {#if !always}
        <form class="port" onsubmit={savePort} aria-label="Port for other devices">
          <label for="network-port">Port</label>
          <span class="port-input">
            <Input
              id="network-port"
              size="sm"
              inputmode="numeric"
              autocomplete="off"
              bind:value={portText}
              oninput={() => (portEdited = true)}
              disabled={busy}
            />
          </span>
          <Button
            size="sm"
            type="submit"
            disabled={busy || !portChanged || portTrouble !== undefined}>Change port</Button
          >
          {#if portTrouble}<span class="error" role="alert">{portTrouble}</span>{/if}
        </form>
        <p class="muted">
          Leave the port alone unless Grid says it is in use. The first time you turn this on,
          Windows may ask whether Grid may use the network. Say yes for private networks.
        </p>
      {/if}
    </Card>

    <Card title="Kiosks" elevation="sm">
      <p>
        A kiosk is a tablet at your stand where a pilot with no phone types their code to sign up.
        Only a tablet you set up here can be one.
      </p>
      {#if kioskError}<Banner tone="danger">{kioskError}</Banner>{/if}
      {#if reachable(view) && view.addresses.length > 0}
        <ol class="steps">
          <li>
            On the tablet, open
            <a
              href={kioskLink(view.addresses[0])}
              target="_blank"
              rel="noreferrer"
              data-testid="kiosk-link">{kioskLink(view.addresses[0])}</a
            >
          </li>
          <li>Make a kiosk code here and type it on the tablet. A code works once.</li>
        </ol>
      {:else}
        <p class="muted">Let other devices open Grid first. A kiosk is another device.</p>
      {/if}
      <div class="row">
        <Button
          variant="primary"
          disabled={kioskBusy}
          onclick={() => kioskChange(() => session.newKioskCode())}>New kiosk code</Button
        >
      </div>
      {#if kiosks && kiosks.codes.length > 0}
        <h3>Codes waiting to be used</h3>
        <ul class="kiosk-list" aria-label="Kiosk codes waiting to be used">
          {#each kiosks.codes as code (code)}
            <li>
              <span class="kiosk-code" data-testid="kiosk-code">{spellKioskCode(code)}</span>
              <Button
                size="sm"
                variant="ghost"
                disabled={kioskBusy}
                aria-label="Cancel kiosk code {spellKioskCode(code)}"
                onclick={() => kioskChange(() => session.cancelKioskCode(code))}>Cancel</Button
              >
            </li>
          {/each}
        </ul>
      {/if}
      {#if kiosks && kiosks.kiosks.length > 0}
        <h3>Kiosks set up</h3>
        <ul class="kiosk-list" aria-label="Kiosks set up">
          {#each kiosks.kiosks as kiosk (kiosk.number)}
            <li>
              <span>Kiosk {kiosk.number}</span>
              <span class="muted">set up {setUpWords(kiosk.set_up_at)}</span>
              <Button
                size="sm"
                variant="ghost"
                disabled={kioskBusy}
                title="That tablet stops being a kiosk"
                aria-label="Remove kiosk {kiosk.number}"
                onclick={() => kioskChange(() => session.removeKiosk(kiosk.number))}>Remove</Button
              >
            </li>
          {/each}
        </ul>
      {/if}
    </Card>

    <Card title="What other devices can do" elevation="sm">
      <p>
        They can look. They cannot start or stop a race, change a lap, or change any setting. Only
        this computer can do those things.
      </p>
      <p>
        Anyone on the same network can open the address above, so turn this on only on a network you
        are happy to share the race with.
      </p>
      {#if hello?.caller === 'Director'}
        <p>
          This device is signed in as the director with the access token.
          <Button size="sm" onclick={signOut}>Sign out on this device</Button>
        </p>
      {/if}
    </Card>
  {/if}
</Page>

<style>
  p {
    margin: 0 0 var(--gf-space-3);
    max-width: 65ch;
  }
  h3 {
    margin: var(--gf-space-3) 0 var(--gf-space-2);
    font-size: var(--gf-font-size-sm);
  }
  .state {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
  }
  .row {
    display: flex;
    gap: var(--gf-space-3);
    align-items: center;
    margin-bottom: var(--gf-space-3);
  }
  .addresses {
    list-style: none;
    margin: 0 0 var(--gf-space-2);
    padding: 0;
    display: grid;
    gap: var(--gf-space-1);
  }
  .addresses li {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
  }
  .addresses a {
    font-family: var(--gf-font-mono);
    font-size: var(--gf-font-size-sm);
    overflow-wrap: anywhere;
  }
  /* The first address is the one to use. The rest are there in case. */
  .addresses li:first-child a {
    font-size: var(--gf-font-size-lg);
  }
  .steps {
    margin: 0 0 var(--gf-space-3);
    padding-left: var(--gf-space-5);
    display: grid;
    gap: var(--gf-space-1);
  }
  .steps a {
    font-family: var(--gf-font-mono);
    overflow-wrap: anywhere;
  }
  .kiosk-list {
    list-style: none;
    margin: 0 0 var(--gf-space-2);
    padding: 0;
    display: grid;
    gap: var(--gf-space-1);
  }
  .kiosk-list li {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
  }
  .kiosk-code {
    font-family: var(--gf-font-mono);
    font-size: var(--gf-font-size-xl);
    letter-spacing: 0.08em;
  }
  .port {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
    margin: var(--gf-space-3) 0 var(--gf-space-2);
    font-size: var(--gf-font-size-sm);
  }
  .port label {
    font-weight: var(--gf-font-weight-semibold);
  }
  .port-input {
    width: 7rem;
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .error {
    color: var(--gf-danger);
  }
</style>
