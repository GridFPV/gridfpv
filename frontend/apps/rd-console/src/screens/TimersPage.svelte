<script lang="ts">
  /**
   * TimersPage — the app-level **Timers** page (Home › Timers).
   *
   * The list of timers the Director knows. Each row shows the timer's name and address,
   * whether the Director is connected to it, what is wrong when it is not (the Director's
   * own sentence), how many seats it reports and how well its clock is known. A timer is
   * added in a dialog and removed with a two-step button. **Tune** opens the timer's Tune page.
   *
   * Under the list, **Found on the network** shows the timers that advertise themselves and
   * are not added yet. Each has an Add button that adds it in one click, with the name the
   * timer gives itself and the address it was found at.
   *
   * The Director holds the connection to each timer and there is no change feed for them, so
   * this page reads both lists again every {@link TIMER_POLL_MS} while it is open.
   */
  import {
    Banner,
    Button,
    Card,
    Dialog,
    Field,
    Input,
    Select,
    StatusPill,
    toast
  } from '@gridfpv/components';
  import type {
    AdapterId,
    CreateTimerRequest,
    DiscoveredTimer,
    TimerKind,
    TimerView
  } from '@gridfpv/types';
  import Brand from '../Brand.svelte';
  import Breadcrumbs from '../Breadcrumbs.svelte';
  import ConfirmButton from '../lib/ConfirmButton.svelte';
  import type { Session } from '../lib/session.svelte.js';
  import {
    clockSentence,
    notAddedYet,
    seatCount,
    seatCountSentence,
    TIMER_KINDS,
    TIMER_POLL_MS
  } from '../lib/timers.js';

  let {
    session,
    onhome,
    ontune
  }: {
    session: Session;
    onhome: () => void;
    /** Open a timer's Tune page. */
    ontune?: (timer: AdapterId) => void;
  } = $props();

  /** The list, or `undefined` before the first read lands. */
  let timers = $state.raw<TimerView[] | undefined>(undefined);
  /** Why the last read failed. Cleared by the next good one. */
  let loadError = $state<string | undefined>(undefined);

  async function loadTimers() {
    try {
      timers = await session.listTimers();
      loadError = undefined;
    } catch (e) {
      loadError = e instanceof Error ? e.message : String(e);
    }
  }

  /** The timers found on the network, or `undefined` before the first read lands. */
  let found = $state.raw<DiscoveredTimer[] | undefined>(undefined);
  /** Why the last read of them failed. Cleared by the next good one. */
  let foundError = $state<string | undefined>(undefined);
  /** The found timers a person can still add. */
  const toAdd = $derived(found ? notAddedYet(found) : undefined);

  async function loadFound() {
    try {
      found = await session.listDiscoveredTimers();
      foundError = undefined;
    } catch (e) {
      foundError = e instanceof Error ? e.message : String(e);
    }
  }

  /** Read both lists. Each stands alone: one failing does not hide the other. */
  async function load() {
    await Promise.all([loadTimers(), loadFound()]);
  }

  // Read now, then every couple of seconds until the page closes.
  $effect(() => {
    void load();
    const poll = setInterval(() => void load(), TIMER_POLL_MS);
    return () => clearInterval(poll);
  });

  // ── Add a timer ────────────────────────────────────────────────────────────
  // The dialog is mounted only while open, so a closed copy never sits in the page.
  let addOpen = $state(false);
  let name = $state('');
  let address = $state('');
  let kind = $state<TimerKind>('Oft');
  const kindInfo = $derived(TIMER_KINDS.find((choice) => choice.kind === kind) ?? TIMER_KINDS[0]);
  let saving = $state(false);
  let formError = $state<string | undefined>(undefined);

  function openAdd() {
    name = '';
    address = '';
    formError = undefined;
    addOpen = true;
  }

  async function submit(e?: Event) {
    e?.preventDefault();
    if (saving) return;
    formError = undefined;
    // The Director checks the name and the address and answers in a sentence; that sentence
    // is shown as it is, so the rule lives in one place.
    const request: CreateTimerRequest = { name: name.trim(), kind, address: address.trim() };
    saving = true;
    try {
      const created = await session.createTimer(request);
      if (!created) return;
      addOpen = false;
      toast.success(`Added “${created.name}”.`);
      await load();
    } catch (err) {
      formError = err instanceof Error ? err.message : String(err);
    } finally {
      saving = false;
    }
  }

  // ── Add a timer found on the network ───────────────────────────────────────
  /** The address of the found timer being added right now. */
  let addingFound = $state<string | undefined>(undefined);
  /** Why the last one-click add failed, in the Director's words. */
  let addFoundError = $state<string | undefined>(undefined);

  async function addFound(timer: DiscoveredTimer) {
    if (addingFound) return;
    addingFound = timer.address;
    addFoundError = undefined;
    try {
      const created = await session.createTimer({
        name: timer.name,
        kind: 'Oft',
        address: timer.address
      });
      if (!created) return;
      toast.success(`Added “${created.name}”.`);
      await load();
    } catch (err) {
      addFoundError = err instanceof Error ? err.message : String(err);
    } finally {
      addingFound = undefined;
    }
  }

  // ── Remove a timer ─────────────────────────────────────────────────────────
  let removing = $state<string | undefined>(undefined);

  async function remove(view: TimerView) {
    if (removing) return;
    removing = view.timer.id;
    try {
      if (!(await session.deleteTimer(view.timer.id))) return;
      toast.success(`Removed “${view.timer.name}”.`);
      await load();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : String(err));
    } finally {
      removing = undefined;
    }
  }
</script>

<div class="page">
  <div class="page-inner">
    <div class="brand-row"><Brand onclick={onhome} /></div>
    <Breadcrumbs crumbs={[{ label: 'Home', onclick: onhome }, { label: 'Timers' }]} />

    <header class="page-head">
      <h1 class="page-title">Timers</h1>
      <Button variant="primary" onclick={openAdd}>+ Add timer</Button>
    </header>

    {#if loadError}
      <Banner tone="danger" title="The timers could not be read.">{loadError}</Banner>
    {/if}

    <Card elevation="sm">
      <div class="wrap">
        {#if timers === undefined}
          {#if !loadError}<p class="muted" role="status">Loading…</p>{/if}
        {:else if timers.length === 0}
          <p class="muted">No timer is added yet. Heats can still be run on the bench seats.</p>
        {:else}
          <ul class="timers" aria-label="Timers">
            {#each timers as view (view.timer.id)}
              {@const status = view.status}
              <li class="timer" aria-label={view.timer.name}>
                <div class="main">
                  <div class="headline">
                    <span class="name">{view.timer.name}</span>
                    {#if status.connected}
                      <StatusPill status="Connected" label="Connected" size="sm" />
                    {:else}
                      <StatusPill status="Disconnected" label="Not connected" size="sm" />
                    {/if}
                  </div>
                  <span class="address">{view.timer.address}</span>
                  {#if !status.connected && status.problem}
                    <p class="problem" data-testid="timer-problem">{status.problem}</p>
                  {/if}
                  <p class="facts">
                    <span data-fact="seats">{seatCountSentence(status)}</span>
                    <span aria-hidden="true">·</span>
                    <span data-fact="clock">{clockSentence(status.clock_error_micros)}</span>
                  </p>
                </div>
                <div class="actions">
                  {#if ontune && view.timer.kind === 'Oft'}
                    <Button
                      variant="secondary"
                      title="Tune {view.timer.name}"
                      onclick={() => ontune(view.timer.id)}>Tune</Button
                    >
                  {/if}
                  <ConfirmButton
                    variant="danger"
                    disabled={removing === view.timer.id}
                    title="Remove {view.timer.name}"
                    onconfirm={() => remove(view)}>Remove</ConfirmButton
                  >
                </div>
              </li>
            {/each}
          </ul>
        {/if}
      </div>
    </Card>

    <Card elevation="sm" title="Found on the network">
      <div class="wrap found">
        {#if foundError}
          <Banner tone="danger" title="The network could not be searched for timers.">
            {foundError}
          </Banner>
        {/if}
        {#if addFoundError}
          <div data-testid="found-problem">
            <Banner
              tone="danger"
              title="The timer was not added."
              ondismiss={() => (addFoundError = undefined)}>{addFoundError}</Banner
            >
          </div>
        {/if}
        {#if toAdd === undefined}
          {#if !foundError}<p class="muted" role="status">Looking for timers…</p>{/if}
        {:else if toAdd.length === 0}
          <p class="muted" data-testid="found-none">
            Timers that announce themselves on the network show up here, and a timer can always be
            added by its address.
          </p>
        {:else}
          <ul class="timers" aria-label="Found on the network">
            {#each toAdd as timer (timer.address)}
              <li class="timer" aria-label={timer.name}>
                <div class="main">
                  <span class="name">{timer.name}</span>
                  <span class="address">{timer.address}</span>
                  {#if timer.seats !== undefined}
                    <p class="facts"><span data-fact="seats">{seatCount(timer.seats)}</span></p>
                  {/if}
                </div>
                <div class="actions">
                  <Button
                    variant="secondary"
                    title="Add {timer.name}"
                    disabled={addingFound !== undefined}
                    onclick={() => void addFound(timer)}
                  >
                    {addingFound === timer.address ? 'Adding…' : 'Add'}
                  </Button>
                </div>
              </li>
            {/each}
          </ul>
        {/if}
      </div>
    </Card>
  </div>
</div>

{#if addOpen}
  <Dialog open={true} title="Add timer" onclose={() => (addOpen = false)}>
    <form class="form" aria-label="Add timer" onsubmit={submit}>
      <Field label="Name" required hint="What people will call it, like Gate.">
        <Input bind:value={name} placeholder="e.g. Gate" aria-label="Name" autocomplete="off" />
      </Field>
      <Field label="Kind of timer" hint={kindInfo.hint}>
        <Select bind:value={kind} aria-label="Kind of timer">
          {#each TIMER_KINDS as choice (choice.kind)}
            <option value={choice.kind}>{choice.label}</option>
          {/each}
        </Select>
      </Field>
      <Field
        label="Address"
        required
        hint="The timer's address and port. The port is {kindInfo.port} unless it was changed."
      >
        <Input
          bind:value={address}
          placeholder="192.168.1.50:{kindInfo.port}"
          aria-label="Address"
          autocomplete="off"
          spellcheck="false"
        />
      </Field>
      {#if formError}
        <Banner tone="danger">{formError}</Banner>
      {/if}
    </form>
    {#snippet footer()}
      <Button variant="ghost" onclick={() => (addOpen = false)} disabled={saving}>Cancel</Button>
      <Button variant="primary" onclick={() => submit()} disabled={saving}>
        {saving ? 'Adding…' : 'Add timer'}
      </Button>
    {/snippet}
  </Dialog>
{/if}

<style>
  .brand-row {
    margin-bottom: var(--gf-space-4);
  }
  .page {
    min-height: 100vh;
    padding: var(--gf-space-6) var(--gf-space-8) var(--gf-space-8);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    overflow: auto;
  }
  .page-inner {
    width: min(52rem, 100%);
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
  }
  .page-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--gf-space-4);
  }
  .page-title {
    margin: 0;
    font-size: var(--gf-font-size-2xl);
    letter-spacing: var(--gf-tracking-tight);
  }
  .wrap {
    padding: var(--gf-space-2);
  }
  .found {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .muted {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .timers {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .timer {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--gf-space-4);
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
  }
  .main {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
    min-width: 0;
  }
  .headline {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
  }
  .name {
    font-weight: var(--gf-font-weight-semibold);
    font-size: var(--gf-font-size-md);
    overflow-wrap: anywhere;
  }
  .address {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    font-variant-numeric: tabular-nums;
    overflow-wrap: anywhere;
  }
  .problem {
    margin: 0;
    color: var(--gf-danger);
    font-size: var(--gf-font-size-sm);
    overflow-wrap: anywhere;
  }
  .facts {
    margin: 0;
    display: flex;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .actions {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-shrink: 0;
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
</style>
