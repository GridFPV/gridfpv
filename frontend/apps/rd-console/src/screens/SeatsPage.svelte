<script lang="ts">
  /**
   * SeatsPage — the Seats page: the channel each seat listens on, the thresholds that make
   * a crossing, and the profiles those are saved in.
   *
   * Seats belong to a timer, so the page shows one timer at a time: the one named in the
   * URL, or the first one that is connected. With more than one timer a box at the top
   * picks between them. Everything under it is {@link TimerSeats}.
   *
   * The timer list (names and status) is read here, again every {@link TIMER_POLL_MS}, and
   * handed down. Timers are added and removed on the Timers page, one tab over.
   */
  import { Banner, Card, Select, StatusPill } from '@gridfpv/components';
  import type { AdapterId, TimerView } from '@gridfpv/types';
  import Page from '../Page.svelte';
  import PageTabs from '../PageTabs.svelte';
  import { formatHash } from '../lib/route.js';
  import type { Session } from '../lib/session.svelte.js';
  import { TIMING_TABS } from '../lib/tabs.js';
  import { TIMER_POLL_MS } from '../lib/timers.js';
  import TimerSeats from './TimerSeats.svelte';

  let {
    session,
    timer: named,
    onpick
  }: {
    session: Session;
    /** The timer to show, when the URL names one. A wire handle: it is never shown. */
    timer?: AdapterId;
    /** Show another timer's seats. */
    onpick: (timer: AdapterId) => void;
  } = $props();

  let timers = $state.raw<TimerView[] | undefined>(undefined);
  let listError = $state<string | undefined>(undefined);

  async function loadTimers() {
    try {
      timers = await session.listTimers();
      listError = undefined;
    } catch (e) {
      listError = e instanceof Error ? e.message : String(e);
    }
  }

  $effect(() => {
    void loadTimers();
    const poll = setInterval(() => void loadTimers(), TIMER_POLL_MS);
    return () => clearInterval(poll);
  });

  /** Timers whose seats Grid can set. A RotorHazard timer's are set on its own pages for now. */
  const tunable = $derived((timers ?? []).filter((view) => view.timer.kind === 'Oft'));
  /** The timer on the page: the one named, else the first connected one, else the first. */
  const shown = $derived(
    named !== undefined
      ? (timers ?? []).find((view) => view.timer.id === named)
      : (tunable.find((view) => view.status.connected) ?? tunable[0])
  );
  const timersHref = formatHash({ kind: 'page', page: 'timers' });
</script>

<Page
  title="Seats"
  help="seats"
  lede="The channel each seat listens on and the levels that make a crossing. Changes are a draft until they are applied, and the timer's answer is read back."
>
  <PageTabs tabs={TIMING_TABS} current="seats" label="Seats and timers" />

  {#if listError}
    <Banner tone="danger" title="The timers could not be read.">{listError}</Banner>
  {/if}

  {#if timers === undefined}
    {#if !listError}<p class="muted" role="status">Loading…</p>{/if}
  {:else if named !== undefined && !shown}
    <Banner tone="warn" title="This timer is not in the list any more.">
      It may have been removed. <a href={timersHref}>See the timers.</a>
    </Banner>
  {:else if !shown}
    <Card elevation="sm">
      <p class="muted" data-testid="no-timer">
        {timers.length === 0
          ? 'No timer is added yet, so there are no seats to set.'
          : 'None of the timers has seats that are set from here.'}
        <a href={timersHref}>Add a timer on the Timers page.</a>
      </p>
    </Card>
  {:else}
    <div class="which">
      {#if tunable.length > 1}
        <label class="pick">
          <span class="eyebrow">Timer</span>
          <Select
            aria-label="Timer"
            value={shown.timer.id}
            onchange={(e: Event) => onpick((e.currentTarget as HTMLSelectElement).value)}
          >
            {#each tunable as view (view.timer.id)}
              <option value={view.timer.id}>{view.timer.name}</option>
            {/each}
          </Select>
        </label>
      {:else}
        <span class="timer-name" data-testid="timer-name">{shown.timer.name}</span>
      {/if}
      {#if shown.status.connected}
        <StatusPill status="Connected" label="Connected" size="sm" />
      {:else}
        <StatusPill status="Disconnected" label="Not connected" size="sm" />
      {/if}
      <span class="address">{shown.timer.address}</span>
    </div>
    <!-- Keyed on the timer, so another timer starts a fresh draft and a fresh live feed. -->
    {#key shown.timer.id}
      <TimerSeats {session} timer={shown.timer.id} {timers} />
    {/key}
  {/if}
</Page>

<style>
  .muted {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .muted a {
    color: var(--gf-accent);
  }
  .which {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
  }
  .pick {
    display: grid;
    gap: var(--gf-space-1);
    min-width: 14rem;
  }
  .eyebrow {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .timer-name {
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-semibold);
  }
  .address {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    font-variant-numeric: tabular-nums;
    overflow-wrap: anywhere;
  }
</style>
