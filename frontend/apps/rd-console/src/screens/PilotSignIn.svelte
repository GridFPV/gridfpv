<script lang="ts">
  /**
   * PilotSignIn — the director's side of a pilot signing in (docs/primitives.md §11.3).
   *
   * Every pilot in the event has a short code. The director shows a pilot theirs: as
   * letters to type, or as a QR code to scan with the phone's camera. Signing in makes the
   * pilot known by name on that one phone, for this event.
   *
   * From here the director can also give a pilot a new code (when the old one got out),
   * or sign their phone out.
   *
   * The codes are secrets between the director and each pilot, so the card is closed
   * until the director opens it: a screen left in view shows none.
   */
  import { Badge, Banner, Button, Card, Dialog } from '@gridfpv/components';
  import type { EventId, PilotAccessView, PilotCodeView, PilotId } from '@gridfpv/types';
  import type { Names } from '../lib/names.js';
  import { spellCode } from '../lib/pilotAccess.js';
  import { formatHash } from '../lib/route.js';
  import type { Session } from '../lib/session.svelte.js';

  /** How often to look again for who has signed in, in milliseconds. */
  const POLL_MS = 5000;

  let {
    session,
    event,
    names,
    cursor
  }: {
    session: Pick<Session, 'getPilotAccess' | 'resetPilotCode' | 'endPilotSession' | 'pilotQrUrl'>;
    event: EventId;
    names: Names;
    /** The event's log cursor: when it moves, the pilots in the event may have changed. */
    cursor: number | undefined;
  } = $props();

  let shown = $state(false);
  let access = $state<PilotAccessView | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);
  /** The pilot whose QR code is up. */
  let showing = $state<PilotId | undefined>(undefined);
  /** The pilot about to be given a new code, while the page asks. */
  let resetting = $state<PilotId | undefined>(undefined);
  /** Bumped to draw a QR picture again after a new code. */
  let drawn = $state(0);

  const say = (err: unknown): string => (err instanceof Error ? err.message : String(err));

  async function load() {
    try {
      access = await session.getPilotAccess(event);
      loadError = undefined;
    } catch (err) {
      loadError = say(err);
    }
  }

  // Only while the card is open: read the codes, read again when the event changes, and
  // look every few seconds for who has signed in.
  $effect(() => {
    if (!shown) return;
    void cursor;
    void event;
    void load();
    const timer = setInterval(load, POLL_MS);
    return () => clearInterval(timer);
  });

  const rows = $derived(
    (access?.pilots ?? [])
      .map((entry) => ({ ...entry, callsign: names.pilot(entry.pilot) }))
      .sort((a, b) => a.callsign.localeCompare(b.callsign))
  );
  const showingRow = $derived(rows.find((row) => row.pilot === showing));
  const resettingRow = $derived(rows.find((row) => row.pilot === resetting));

  function take(next: PilotCodeView) {
    if (!access) return;
    access = {
      ...access,
      pilots: access.pilots.map((entry) => (entry.pilot === next.pilot ? next : entry))
    };
  }

  async function act(what: () => Promise<void>) {
    if (busy) return;
    busy = true;
    error = undefined;
    try {
      await what();
    } catch (err) {
      error = say(err);
    } finally {
      busy = false;
    }
  }

  const newCode = () =>
    act(async () => {
      const pilot = resetting;
      resetting = undefined;
      if (!pilot) return;
      const next = await session.resetPilotCode(event, pilot);
      if (!next) return;
      take(next);
      drawn += 1;
    });

  const signOut = (pilot: PilotId) =>
    act(async () => {
      if (await session.endPilotSession(event, pilot)) await load();
    });
</script>

<Card title="Pilot sign-in" elevation="sm">
  {#snippet actions()}
    <Button size="sm" variant="secondary" onclick={() => (shown = !shown)}>
      {shown ? 'Hide codes' : 'Show codes'}
    </Button>
  {/snippet}

  <p class="muted">
    A pilot signs in on their own phone with a code. Show them theirs, or let them scan it. It works
    for this event, on one phone at a time.
  </p>

  {#if shown}
    {#if loadError}
      <Banner tone="danger" title="The codes could not be read.">{loadError}</Banner>
    {/if}
    {#if error}
      <Banner tone="danger">{error}</Banner>
    {/if}
    {#if access}
      {#if !access.address}
        <Banner tone="warn">
          Phones cannot reach Grid yet. Turn that on, on the
          <a href={formatHash({ kind: 'page', page: 'network' })}>Network page</a>, to sign pilots
          in.
        </Banner>
      {/if}
      {#if rows.length === 0}
        <p class="muted" data-testid="no-pilots">
          No pilot is in this event yet. Add pilots to a class above.
        </p>
      {:else}
        <table aria-label="Pilot sign-in codes">
          <thead>
            <tr>
              <th scope="col">Pilot</th>
              <th scope="col">Code</th>
              <th scope="col"><span class="visually-hidden">Signed in</span></th>
              <th scope="col"><span class="visually-hidden">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            {#each rows as row (row.pilot)}
              <tr>
                <th scope="row">{row.callsign}</th>
                <td class="code" data-testid="pilot-code">{spellCode(row.code)}</td>
                <td>
                  {#if row.signed_in}<Badge tone="success">Signed in</Badge>{/if}
                </td>
                <td class="actions">
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={!access.address}
                    title={access.address
                      ? undefined
                      : 'Turn on other devices on the Network page first'}
                    aria-label="Show the QR code for {row.callsign}"
                    onclick={() => (showing = row.pilot)}>QR code</Button
                  >
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={busy}
                    aria-label="Give {row.callsign} a new code"
                    onclick={() => (resetting = row.pilot)}>New code</Button
                  >
                  {#if row.signed_in}
                    <Button
                      size="sm"
                      variant="ghost"
                      disabled={busy}
                      aria-label="Sign {row.callsign} out"
                      onclick={() => signOut(row.pilot)}>Sign out</Button
                    >
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    {:else if !loadError}
      <p class="muted" role="status">Loading…</p>
    {/if}
  {/if}
</Card>

{#if showingRow && access?.address}
  <Dialog open={true} title="Sign in: {showingRow.callsign}" onclose={() => (showing = undefined)}>
    <div class="qr">
      {#key `${showingRow.pilot}\u0000${showingRow.code}\u0000${drawn}`}
        <img
          src={session.pilotQrUrl(event, showingRow.pilot)}
          alt="QR code that signs {showingRow.callsign} in"
          width="240"
          height="240"
        />
      {/key}
      <p>Scan this with the phone's camera.</p>
      <p class="muted">
        Or open <strong>{access.address}</strong> on the phone, press
        <strong>I am a pilot</strong>, and type:
      </p>
      <p class="big-code" data-testid="qr-code-text">{spellCode(showingRow.code)}</p>
    </div>
    {#snippet footer()}
      <Button variant="primary" onclick={() => (showing = undefined)}>Done</Button>
    {/snippet}
  </Dialog>
{/if}

{#if resettingRow}
  <Dialog
    open={true}
    title="Give {resettingRow.callsign} a new code?"
    onclose={() => (resetting = undefined)}
  >
    <p>
      The code {resettingRow.callsign} has now stops working.
      {#if resettingRow.signed_in}Their phone is signed out.{/if}
      They sign in again with the new one.
    </p>
    {#snippet footer()}
      <Button variant="ghost" onclick={() => (resetting = undefined)}>Cancel</Button>
      <Button variant="primary" onclick={newCode}>New code</Button>
    {/snippet}
  </Dialog>
{/if}

<style>
  p {
    margin: 0 0 var(--gf-space-3);
    max-width: 65ch;
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--gf-font-size-sm);
  }
  th,
  td {
    text-align: left;
    padding: var(--gf-space-1) var(--gf-space-2);
    border-bottom: 1px solid var(--gf-border-subtle);
    vertical-align: middle;
  }
  thead th {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
    font-weight: var(--gf-font-weight-semibold);
  }
  .code,
  .big-code {
    font-family: var(--gf-font-mono);
    letter-spacing: 0.08em;
  }
  .actions {
    text-align: right;
    white-space: nowrap;
  }
  .qr {
    display: grid;
    justify-items: center;
    gap: var(--gf-space-2);
    text-align: center;
  }
  .qr img {
    background: #fff;
    border-radius: var(--gf-radius-sm);
    /* A QR code is squares: never smooth them. */
    image-rendering: pixelated;
  }
  .qr p {
    margin: 0;
  }
  .big-code {
    font-size: var(--gf-font-size-xl);
    font-weight: var(--gf-font-weight-bold);
  }
  .visually-hidden {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
</style>
