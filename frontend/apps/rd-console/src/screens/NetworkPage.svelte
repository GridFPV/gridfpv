<script lang="ts">
  /**
   * NetworkPage — how other devices reach Grid (docs/primitives.md §11.10).
   *
   * Grid starts out answering only the computer it runs on. This page has two switches,
   * each with its own port:
   *
   * - **Public**: the spectator, pilot and kiosk pages.
   * - **Remote RD**: the whole console, for a director on another device. It asks for the
   *   access token, which is shown here on the Grid computer and nowhere else.
   *
   * What a caller at each door may do is decided in the Director (`access.rs`).
   */
  import { Banner, Button, Card, Dialog, Input, toast } from '@gridfpv/components';
  import type { Door, DoorRequest, KioskView, NetworkView, PublicHello } from '@gridfpv/types';
  import Page from '../Page.svelte';
  import { kioskLink, setUpWords, spellKioskCode } from '../lib/kiosk.js';
  import { hostOf, reachable, refreshEvery, tokenProblem } from '../lib/network.js';
  import type { Session } from '../lib/session.svelte.js';
  import DoorCard from './DoorCard.svelte';

  let {
    session
  }: {
    session: Pick<
      Session,
      | 'getNetwork'
      | 'putDoor'
      | 'putAccessToken'
      | 'hello'
      | 'signOutDirector'
      | 'getKiosks'
      | 'newKioskCode'
      | 'cancelKioskCode'
      | 'removeKiosk'
      | 'renameKiosk'
    >;
  } = $props();

  let view = $state<NetworkView | undefined>(undefined);
  let hello = $state<PublicHello | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);

  const say = (err: unknown): string => (err instanceof Error ? err.message : String(err));

  async function load() {
    try {
      view = await session.getNetwork();
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

  /** The kiosk being named, and the name as typed. */
  let naming = $state<{ number: number; name: string } | undefined>(undefined);

  async function saveName(e: Event) {
    e.preventDefault();
    const target = naming;
    if (!target) return;
    const before = kiosks;
    await kioskChange(() => session.renameKiosk(target.number, target.name.trim()));
    // Close it once the change is in. On a refusal it stays open, with the reason above.
    if (kiosks !== before && kioskError === undefined) naming = undefined;
  }

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

  // Read it now, and keep reading: a listener starts a moment after its switch, and this
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
    }, refreshEvery(view));
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

  /** Send a change, and take the Director's answer. `true` when it was taken. */
  async function change(what: () => Promise<NetworkView | undefined>): Promise<boolean> {
    if (busy) return false;
    busy = true;
    error = undefined;
    try {
      const next = await what();
      if (!next) return false;
      view = next;
      // The wait before the next read depends on the state, which has just changed.
      schedule();
      return true;
    } catch (err) {
      error = say(err);
      return false;
    } finally {
      busy = false;
    }
  }
  const setDoor = (door: Door) => (request: DoorRequest) =>
    change(() => session.putDoor(door, request));

  // ── The access token ───────────────────────────────────────────────────────
  /** The token dialog: open, and what is typed in it. */
  let changing = $state<{ typed: string } | undefined>(undefined);
  const tokenTrouble = $derived(changing ? tokenProblem(changing.typed) : undefined);

  async function saveToken(e: Event) {
    e.preventDefault();
    const typed = changing?.typed.trim();
    if (typed === undefined || tokenTrouble) return;
    if (await change(() => session.putAccessToken(typed === '' ? undefined : typed))) {
      changing = undefined;
      toast.success('The access token is changed.');
    }
  }

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      toast.success('Copied.');
    } catch {
      toast.error('Could not copy. Select it and copy it by hand.');
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

<Page title="Network" help="network" lede="Let other devices on your network open Grid.">
  {#if loadError}
    <Banner tone="danger" title="Grid could not read its network setting.">{loadError}</Banner>
  {/if}
  {#if error}
    <Banner tone="danger">{error}</Banner>
  {/if}

  {#if view}
    <div class="doors">
      <DoorCard
        name="Public"
        what="Spectators, pilots and kiosks"
        door={view.public}
        {busy}
        onchange={setDoor('Public')}
      />
      <DoorCard
        name="Remote RD"
        what="Run Grid from another device"
        door={view.remote}
        {busy}
        onchange={setDoor('Remote')}
      >
        {#if view.token}
          <div class="token">
            <span class="token-label" id="token-label">Access token</span>
            <code aria-labelledby="token-label" data-testid="access-token">{view.token}</code>
            <Button
              size="sm"
              variant="ghost"
              aria-label="Copy the access token"
              onclick={() => view?.token && copy(view.token)}>Copy</Button
            >
            <Button
              size="sm"
              variant="ghost"
              disabled={busy}
              onclick={() => (changing = { typed: '' })}>Change…</Button
            >
          </div>
        {/if}
        {#if hello?.caller === 'Director'}
          <p class="signed-in">
            This device is signed in with the access token.
            <Button size="sm" onclick={signOut}>Sign out on this device</Button>
          </p>
        {/if}
      </DoorCard>
    </div>

    <Card title="Kiosks" elevation="sm">
      {#if kioskError}<Banner tone="danger">{kioskError}</Banner>{/if}
      {#if reachable(view.public)}
        <ol class="steps">
          <li>
            On the tablet, open
            <a
              href={kioskLink(view.public.addresses[0])}
              target="_blank"
              rel="noreferrer"
              data-testid="kiosk-link">{hostOf(kioskLink(view.public.addresses[0]))}</a
            >
          </li>
          <li>Make a kiosk code here and type it on the tablet. A code works once.</li>
        </ol>
      {:else}
        <p class="muted">Enable Public first. A kiosk comes in that way.</p>
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
              <span>
                Kiosk {kiosk.number}{#if kiosk.name}{': '}<strong data-testid="kiosk-name"
                    >{kiosk.name}</strong
                  >{/if}
              </span>
              <span class="muted">set up {setUpWords(kiosk.set_up_at)}</span>
              <Button
                size="sm"
                variant="ghost"
                disabled={kioskBusy}
                aria-label="Name kiosk {kiosk.number}"
                onclick={() => (naming = { number: kiosk.number, name: kiosk.name ?? '' })}
                >{kiosk.name ? 'Rename' : 'Name it'}</Button
              >
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
  {/if}
</Page>

{#if changing}
  {@const draft = changing}
  <Dialog open={true} title="Change the access token" onclose={() => (changing = undefined)}>
    <form id="access-token" onsubmit={saveToken} aria-label="Access token">
      <label for="access-token-box">New token</label>
      <Input
        id="access-token-box"
        autocomplete="off"
        spellcheck="false"
        maxlength={64}
        placeholder="Leave empty and Grid makes one"
        bind:value={draft.typed}
        disabled={busy}
      />
      {#if tokenTrouble}<p class="error" role="alert">{tokenTrouble}</p>{/if}
      <p class="muted">Every device signed in with the old token is signed out.</p>
      {#if error}<Banner tone="danger">{error}</Banner>{/if}
    </form>
    {#snippet footer()}
      <Button variant="ghost" onclick={() => (changing = undefined)}>Cancel</Button>
      <Button
        variant="primary"
        type="submit"
        form="access-token"
        disabled={busy || tokenTrouble !== undefined}>Change</Button
      >
    {/snippet}
  </Dialog>
{/if}

{#if naming}
  {@const target = naming}
  <Dialog open={true} title="Name kiosk {target.number}" onclose={() => (naming = undefined)}>
    <form id="kiosk-name" onsubmit={saveName} aria-label="Kiosk name">
      <label for="kiosk-name-box">Name</label>
      <Input
        id="kiosk-name-box"
        autocomplete="off"
        maxlength={40}
        placeholder="By the gate"
        bind:value={target.name}
        disabled={kioskBusy}
      />
      <p class="muted">
        Say where it is, so you know which tablet is which. The tablet shows its name while it waits
        for a pilot. Leave it empty for no name.
      </p>
      {#if kioskError}<Banner tone="danger">{kioskError}</Banner>{/if}
    </form>
    {#snippet footer()}
      <Button variant="ghost" onclick={() => (naming = undefined)}>Cancel</Button>
      <Button variant="primary" type="submit" form="kiosk-name" disabled={kioskBusy}>Save</Button>
    {/snippet}
  </Dialog>
{/if}

<style>
  p {
    margin: 0 0 var(--gf-space-3);
    max-width: 65ch;
  }
  h3 {
    margin: var(--gf-space-3) 0 var(--gf-space-2);
    font-size: var(--gf-font-size-sm);
  }
  .doors {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(20rem, 1fr));
    gap: var(--gf-space-4);
    align-items: start;
    margin-bottom: var(--gf-space-4);
  }
  .token {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
  }
  .token-label {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .token code {
    font-family: var(--gf-font-mono);
    font-size: var(--gf-font-size-lg);
    letter-spacing: 0.06em;
    padding: var(--gf-space-1) var(--gf-space-2);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    user-select: all;
  }
  .signed-in {
    margin: 0;
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
    font-size: var(--gf-font-size-sm);
  }
  .row {
    display: flex;
    gap: var(--gf-space-3);
    align-items: center;
    margin-bottom: var(--gf-space-3);
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
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .error {
    color: var(--gf-danger);
  }
</style>
