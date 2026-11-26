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
   *
   * **Print** makes a sheet to cut up and hand out: one card for each pilot, with their
   * callsign, their QR code and the code as letters, four or six to a page. It is printed
   * by the browser, which can also save it as a PDF. Nothing is sent anywhere.
   */
  import { tick } from 'svelte';
  import { Badge, Banner, Button, Card, Dialog, Select } from '@gridfpv/components';
  import type { EventId, PilotAccessView, PilotCodeView, PilotId } from '@gridfpv/types';
  import type { Names } from '../lib/names.js';
  import { PER_PAGE_CHOICES, sheetPages, spellCode, type PerPage } from '../lib/pilotAccess.js';
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

  // ── The printed sheet ──────────────────────────────────────────────────────
  /** How many cards go on a page. */
  let perPage = $state<PerPage>(6);
  /** Whether the sheet is in the page, ready for the printer. It is not there otherwise. */
  let printing = $state(false);
  let sheet = $state<HTMLElement | undefined>(undefined);
  const pages = $derived(sheetPages(rows, perPage));

  /**
   * Print the sheet: read the codes, put the sheet in the page, wait until every QR
   * picture has loaded (a printer would show an empty box otherwise), and hand over to
   * the browser's own print box.
   */
  const print = () =>
    act(async () => {
      await load();
      if (loadError) throw new Error(loadError);
      if (!access?.address) {
        throw new Error(
          'Switch on Public, on the Network page, first: a QR code needs an address.'
        );
      }
      if (rows.length === 0) throw new Error('No pilot is in this event yet.');
      printing = true;
      await tick();
      const pictures = [...(sheet?.querySelectorAll('img') ?? [])];
      await Promise.all(
        pictures.map(
          (img) =>
            new Promise<void>((done) => {
              if (img.complete) done();
              else {
                img.addEventListener('load', () => done(), { once: true });
                img.addEventListener('error', () => done(), { once: true });
              }
            })
        )
      );
      try {
        window.print();
      } finally {
        printing = false;
      }
    });

  const signOut = (pilot: PilotId) =>
    act(async () => {
      if (await session.endPilotSession(event, pilot)) await load();
    });
</script>

<Card title="Sign-in codes" elevation="sm">
  {#snippet actions()}
    <span class="print">
      <Select
        size="sm"
        aria-label="Cards on a page"
        value={String(perPage)}
        onchange={(e: Event) =>
          (perPage = Number((e.currentTarget as HTMLSelectElement).value) as PerPage)}
      >
        {#each PER_PAGE_CHOICES as choice (choice)}
          <option value={String(choice)}>{choice} to a page</option>
        {/each}
      </Select>
      <Button size="sm" variant="ghost" disabled={busy} onclick={print}>Print</Button>
    </span>
    <Button size="sm" variant="secondary" onclick={() => (shown = !shown)}>
      {shown ? 'Hide codes' : 'Show codes'}
    </Button>
  {/snippet}

  {#if error && !shown}
    <Banner tone="danger">{error}</Banner>
  {/if}

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

<!-- The sheet for the printer. It is in the page only while printing, and the print styles
     below hide everything else. Black on white: it is for paper. -->
{#if printing && access?.address}
  <div class="sheet" bind:this={sheet} data-testid="code-sheet" data-per-page={perPage}>
    {#each pages as cards, index (index)}
      <section class="sheet-page" aria-label="Page {index + 1}">
        {#each cards as row (row.pilot)}
          <article class="sheet-card" aria-label={row.callsign}>
            <h2>{row.callsign}</h2>
            <img
              src={session.pilotQrUrl(event, row.pilot)}
              alt="QR code that signs {row.callsign} in"
              width="240"
              height="240"
            />
            <p class="sheet-code">{spellCode(row.code)}</p>
            <p class="sheet-how">Scan it, or open {access.address} and type the code.</p>
          </article>
        {/each}
      </section>
    {/each}
  </div>
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
  .print {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-1);
  }

  /* The sheet is off the screen: it is for the printer only. */
  .sheet {
    position: absolute;
    left: -200vw;
    top: 0;
    width: 190mm;
    color: #000;
    background: #fff;
  }
  .sheet-page {
    display: grid;
    gap: 6mm;
    padding: 4mm;
    break-after: page;
  }
  .sheet-page:last-child {
    break-after: auto;
  }
  .sheet[data-per-page='4'] .sheet-page {
    grid-template-columns: repeat(2, 1fr);
  }
  .sheet[data-per-page='6'] .sheet-page {
    grid-template-columns: repeat(3, 1fr);
  }
  .sheet-card {
    display: grid;
    justify-items: center;
    gap: 2mm;
    padding: 4mm;
    border: 0.3mm dashed #888;
    border-radius: 2mm;
    text-align: center;
    break-inside: avoid;
  }
  .sheet-card h2 {
    margin: 0;
    font-size: 16pt;
    overflow-wrap: anywhere;
  }
  .sheet-card img {
    width: 100%;
    max-width: 62mm;
    height: auto;
    image-rendering: pixelated;
  }
  .sheet[data-per-page='6'] .sheet-card h2 {
    font-size: 12pt;
  }
  .sheet-card p {
    margin: 0;
    max-width: none;
  }
  .sheet-code {
    font-family: var(--gf-font-mono);
    font-size: 14pt;
    font-weight: 700;
    letter-spacing: 0.08em;
  }
  .sheet-how {
    font-size: 8pt;
    color: #333;
  }
  @media print {
    /* Only the sheet goes to the printer, on plain white. */
    :global(html),
    :global(body) {
      background: #fff !important;
    }
    :global(body *) {
      visibility: hidden;
    }
    .sheet,
    .sheet :global(*) {
      visibility: visible;
    }
    .sheet {
      left: 0;
      width: 100%;
    }
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
