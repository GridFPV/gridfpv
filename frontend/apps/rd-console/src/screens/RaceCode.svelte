<script lang="ts">
  /**
   * RaceCode — the event's race code, for the director (docs/primitives.md §13.13).
   *
   * A pilot can take an open seat from a server's page, on their own phone data, with no
   * Wi-Fi from the director. Taking a seat gives them the laps flown on it, so it is only
   * for someone who is at the race. This one code is the proof: the director shows it at
   * the race, and a pilot types it, or scans the QR code that holds it, once.
   *
   * The code is a secret of the race, so the card is closed until the director opens it.
   * A new code ends the old one at once.
   */
  import { Banner, Button, Card, Dialog } from '@gridfpv/components';
  import type { EventId, RaceCodeView } from '@gridfpv/types';
  import { spellCode } from '../lib/pilotAccess.js';
  import type { Session } from '../lib/session.svelte.js';

  let {
    session,
    event
  }: {
    session: Pick<Session, 'getRaceCode' | 'newRaceCode' | 'raceCodeQrUrl'>;
    event: EventId;
  } = $props();

  let shown = $state(false);
  let view = $state<RaceCodeView | undefined>(undefined);
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);
  /** Whether the page is asking before it makes a new code. */
  let renewing = $state(false);
  /** Whether the QR code is up, big, for pilots to scan. */
  let scanning = $state(false);

  const say = (err: unknown): string => (err instanceof Error ? err.message : String(err));

  async function load() {
    try {
      view = await session.getRaceCode(event);
      error = undefined;
    } catch (err) {
      error = say(err);
    }
  }

  // Only while the card is open. Read again when the event changes.
  $effect(() => {
    void event;
    view = undefined;
    if (shown) void load();
  });

  async function renew() {
    if (busy) return;
    busy = true;
    error = undefined;
    try {
      const next = await session.newRaceCode(event);
      if (next) view = next;
      renewing = false;
    } catch (err) {
      error = say(err);
      renewing = false;
    } finally {
      busy = false;
    }
  }
</script>

<Card elevation="sm">
  <div class="block" data-testid="race-code">
    <div class="head">
      <h2>Race code</h2>
      <Button size="sm" aria-expanded={shown} onclick={() => (shown = !shown)}
        >{shown ? 'Hide' : 'Show'}</Button
      >
    </div>
    <p class="muted">
      One code for the whole race. A pilot who takes a seat from the server's page types it once, to
      show they are here. Show it only at the race.
    </p>
    {#if error}<Banner tone="danger">{error}</Banner>{/if}
    {#if shown && view}
      <p class="big-code" data-testid="race-code-text">{spellCode(view.code)}</p>
      <div class="row">
        {#if view.link}
          <Button size="sm" variant="primary" onclick={() => (scanning = true)}
            >Show the QR code</Button
          >
        {/if}
        <Button size="sm" disabled={busy} onclick={() => (renewing = true)}>New code…</Button>
      </div>
      {#if !view.link}
        <p class="muted" data-testid="race-code-no-link">
          Link a server on the Server page, under Setup, to get a QR code pilots can scan.
        </p>
      {/if}
    {/if}
  </div>
</Card>

{#if scanning && view?.link}
  <Dialog open={true} title="At the race? Scan this" onclose={() => (scanning = false)}>
    <div class="qr">
      {#key view.code}
        <img
          src={session.raceCodeQrUrl(event)}
          alt="QR code that opens this event on the server with the race code"
          width="280"
          height="280"
        />
      {/key}
      <p>Scan this with your phone's camera, then sign in.</p>
      <p class="muted">Or open the event on the server and type:</p>
      <p class="big-code">{spellCode(view.code)}</p>
    </div>
    {#snippet footer()}
      <Button variant="primary" onclick={() => (scanning = false)}>Done</Button>
    {/snippet}
  </Dialog>
{/if}

{#if renewing}
  <Dialog open={true} title="Make a new race code?" onclose={() => (renewing = false)}>
    <p>The old code stops working straight away.</p>
    <p>Pilots who already have a seat keep it. To take another seat they need the new code.</p>
    {#snippet footer()}
      <Button variant="ghost" onclick={() => (renewing = false)}>Cancel</Button>
      <Button variant="danger" disabled={busy} onclick={renew}>New code</Button>
    {/snippet}
  </Dialog>
{/if}

<style>
  .block {
    display: grid;
    gap: var(--gf-space-3);
  }
  .head,
  .row {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
  }
  .head {
    justify-content: space-between;
  }
  h2 {
    margin: 0;
    font-size: var(--gf-font-size-lg);
  }
  p {
    margin: 0;
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    max-width: 65ch;
  }
  .big-code {
    font-family: var(--gf-font-mono);
    font-size: var(--gf-font-size-3xl, 2rem);
    font-weight: var(--gf-font-weight-bold);
    letter-spacing: 0.12em;
  }
  .qr {
    display: grid;
    justify-items: center;
    gap: var(--gf-space-3);
    text-align: center;
  }
  .qr img {
    background: #fff;
    padding: var(--gf-space-2);
    border-radius: var(--gf-radius-sm);
  }
</style>
