<script lang="ts">
  /**
   * DoorCard — one of the two doors other devices come in by (docs/primitives.md §11.10):
   * its switch, the address to open, and its port. The Network page shows two.
   *
   * The card says little when all is well: the switch and the address. A pill appears only
   * while the listener is starting or not working.
   */
  import type { Snippet } from 'svelte';
  import { Badge, Banner, Button, Card, Input, toast } from '@gridfpv/components';
  import type { DoorRequest, DoorView } from '@gridfpv/types';
  import { hostOf, portProblem, reachable, stateLabel, stateTone } from '../lib/network.js';

  let {
    name,
    what,
    door,
    busy = false,
    onchange,
    children = undefined
  }: {
    /** What the door is called: "Public", "Remote RD". */
    name: string;
    /** Who it is for, in a few words. */
    what: string;
    door: DoorView;
    busy?: boolean;
    /** Send a change. Resolves `true` when the Director took it. */
    onchange: (request: DoorRequest) => Promise<boolean>;
    /** More about this door, under the address: the access token. */
    children?: Snippet;
  } = $props();

  const slug = $derived(name.toLowerCase().replace(/\s+/g, '-'));

  /** The port as typed. Follows the Director's until the director edits it. */
  let portText = $state('');
  let portEdited = $state(false);
  $effect(() => {
    if (!portEdited) portText = String(door.port);
  });
  const portTrouble = $derived(portEdited ? portProblem(portText) : undefined);
  const portChanged = $derived(portEdited && portText.trim() !== String(door.port));

  /**
   * The switch was flipped. The box shows what the Director says, not what was clicked: a
   * change that is refused, or still on its way, puts it back.
   */
  async function flip(e: Event & { currentTarget: HTMLInputElement }) {
    const box = e.currentTarget;
    await onchange({ enabled: box.checked });
    box.checked = door.enabled;
  }

  async function savePort(e: Event) {
    e.preventDefault();
    if (portProblem(portText)) return;
    if (await onchange({ enabled: door.enabled, port: Number(portText.trim()) })) {
      portEdited = false;
    }
  }
  async function usualPort() {
    if (await onchange({ enabled: door.enabled, port: 0 })) portEdited = false;
  }

  async function copy(address: string) {
    try {
      await navigator.clipboard.writeText(address);
      toast.success('Copied.');
    } catch {
      toast.error('Could not copy. Select the address and copy it by hand.');
    }
  }
</script>

<Card elevation="sm">
  <div class="door" data-testid="door-{slug}">
    <header>
      <div class="title">
        <h2>{name}</h2>
        <p class="what">{what}</p>
      </div>
      {#if door.state === 'Starting' || door.state === 'Failed'}
        <Badge tone={stateTone(door.state)}>
          <span data-testid="door-state">{stateLabel(door.state)}</span>
        </Badge>
      {/if}
      <label class="switch">
        <input
          type="checkbox"
          role="switch"
          aria-label="Enable {name}"
          checked={door.enabled}
          disabled={busy}
          onchange={flip}
        />
        <span class="track" aria-hidden="true"><span class="thumb"></span></span>
      </label>
    </header>

    {#if door.problem}
      <Banner tone={door.state === 'Failed' ? 'danger' : 'warn'}>
        <span data-testid="door-problem">{door.problem}</span>
      </Banner>
    {/if}

    {#if reachable(door)}
      <ul class="addresses" aria-label="{name} addresses">
        {#each door.addresses as address (address)}
          <li>
            <a href={address} target="_blank" rel="noreferrer">{hostOf(address)}</a>
            <Button
              size="sm"
              variant="ghost"
              aria-label="Copy {hostOf(address)}"
              onclick={() => copy(address)}>Copy</Button
            >
          </li>
        {/each}
      </ul>
      {#if door.addresses.length > 1}
        <p class="muted">Use the first address. If it does not open, try the next.</p>
      {/if}
    {/if}
    {#if door.note && door.enabled}
      <p class="muted" data-testid="door-note">{door.note}</p>
    {/if}

    {@render children?.()}

    <form class="port" onsubmit={savePort} aria-label="{name} port">
      <label for="port-{slug}">Port</label>
      <span class="port-input">
        <Input
          id="port-{slug}"
          size="sm"
          inputmode="numeric"
          autocomplete="off"
          bind:value={portText}
          oninput={() => (portEdited = true)}
          disabled={busy}
        />
      </span>
      {#if portChanged}
        <Button size="sm" type="submit" disabled={busy || portTrouble !== undefined}
          >Change port</Button
        >
      {:else if door.port_picked}
        <Button size="sm" variant="ghost" disabled={busy} onclick={usualPort}
          >Use the usual port</Button
        >
      {/if}
      {#if portTrouble}<span class="error" role="alert">{portTrouble}</span>{/if}
    </form>
  </div>
</Card>

<style>
  .door {
    display: grid;
    gap: var(--gf-space-3);
  }
  header {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
  }
  .title {
    flex: 1;
    min-width: 0;
  }
  h2 {
    margin: 0;
    font-size: var(--gf-font-size-lg);
  }
  p {
    margin: 0;
  }
  .what,
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }

  /* The switch: a real checkbox, drawn as a track and a thumb. */
  .switch {
    position: relative;
    display: inline-flex;
    cursor: pointer;
  }
  .switch input {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    margin: 0;
    opacity: 0;
    cursor: pointer;
    /* Above the drawn track and its thumb, so a click always lands on the box itself. */
    z-index: 1;
  }
  .switch input:disabled {
    cursor: default;
  }
  .track {
    width: 2.75rem;
    height: 1.5rem;
    border-radius: var(--gf-radius-pill);
    background: var(--gf-surface-sunken);
    border: 1px solid var(--gf-border-strong);
    transition: background var(--gf-motion-fast) var(--gf-ease-out);
    display: flex;
    align-items: center;
    padding: 0 2px;
    pointer-events: none;
  }
  .thumb {
    width: 1.125rem;
    height: 1.125rem;
    border-radius: 50%;
    background: var(--gf-text-muted);
    transition:
      transform var(--gf-motion-fast) var(--gf-ease-out),
      background var(--gf-motion-fast) var(--gf-ease-out);
  }
  .switch input:checked + .track {
    background: var(--gf-accent);
    border-color: var(--gf-accent);
  }
  .switch input:checked + .track .thumb {
    transform: translateX(1.2rem);
    background: var(--gf-accent-contrast);
  }
  .switch input:focus-visible + .track {
    outline: var(--gf-ring-width) solid var(--gf-ring-color);
    outline-offset: var(--gf-ring-offset);
  }
  .switch input:disabled + .track {
    opacity: 0.6;
  }

  .addresses {
    list-style: none;
    margin: 0;
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
    font-size: var(--gf-font-size-xl);
    font-weight: var(--gf-font-weight-semibold);
  }
  .port {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .port-input {
    width: 6rem;
  }
  .error {
    color: var(--gf-danger);
  }
</style>
