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
  import type { NetworkRequest, NetworkView, PublicHello } from '@gridfpv/types';
  import Page from '../Page.svelte';
  import { portProblem, reachable, refreshEvery, stateLabel, stateTone } from '../lib/network.js';
  import type { Session } from '../lib/session.svelte.js';

  let {
    session
  }: {
    session: Pick<Session, 'getNetwork' | 'putNetwork' | 'hello' | 'signOutDirector'>;
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
