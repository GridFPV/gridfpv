<script lang="ts">
  /**
   * HomeHub — the RD console's app-level landing (#118).
   *
   * The two-level IA's root: big, field-readable cards — **Events** (the prominent, full-width
   * primary action on top) above a smaller row of **Pilots**, **Classes**, **Timers** — each
   * navigating to its own page. The picker is no longer the landing (it's the Events page),
   * and app-level timer management is no longer a modal (it's the Timers page). Each card shows a
   * quick at-a-glance summary read open (no token) on mount: Pilots count (`GET /pilots`), Events
   * count (`GET /events`), and timer count + how many are connected (`GET /timers`). A failed read
   * just shows a dash — the hub never blocks on a summary.
   *
   * Big text + dark surfaces by design: this hub is glanced at on a laptop outdoors in the sun, so
   * the cards are large, high-contrast tap targets ("readable in sunlight?" is the gate).
   */
  import { Card } from '@gridfpv/components';
  import type { Timer } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import { isTimerConnected } from '../lib/timers.js';

  let {
    session,
    onpilots,
    onclasses,
    onevents,
    ontimers
  }: {
    session: Session;
    onpilots: () => void;
    onclasses: () => void;
    onevents: () => void;
    ontimers: () => void;
  } = $props();

  // Each summary loads independently and best-effort; `undefined` while loading, `null` on a
  // failed/unreachable read (rendered as a dash). The hub renders immediately regardless.
  let pilotCount = $state<number | undefined | null>(undefined);
  let classCount = $state<number | undefined | null>(undefined);
  let eventCount = $state<number | undefined | null>(undefined);
  let timerCount = $state<number | undefined | null>(undefined);
  let connectedTimers = $state<number | undefined | null>(undefined);

  // A timer is "connected" when it is usable now: a Mock reports Ready, a live RotorHazard reports
  // Connected once its socket is up (a Configured-but-not-dialed-in RH does NOT count). Shared
  // predicate (`isTimerConnected`) so the count can't drift from the timer screens.
  function countConnected(timers: Timer[]): number {
    return timers.filter(isTimerConnected).length;
  }

  $effect(() => {
    void session
      .listPilots()
      .then((pilots) => (pilotCount = pilots.length))
      .catch(() => (pilotCount = null));
    void session
      .listClasses()
      .then((classes) => (classCount = classes.length))
      .catch(() => (classCount = null));
    void session
      .listEvents()
      .then((events) => (eventCount = events.length))
      .catch(() => (eventCount = null));
    void session
      .listTimers()
      .then((timers) => {
        timerCount = timers.length;
        connectedTimers = countConnected(timers);
      })
      .catch(() => {
        timerCount = null;
        connectedTimers = null;
      });
  });

  function fmt(n: number | undefined | null): string {
    if (n === undefined) return '…';
    if (n === null) return '—';
    return String(n);
  }
</script>

<div class="hub">
  <div class="hub-inner">
    <header class="head">
      <svg class="brand-mark" viewBox="20 20 60 60" role="img" aria-label="GridFPV">
        <path
          d="M71 33 H40 Q31 33 31 42 V58 Q31 67 40 67 H60 Q69 67 69 58 V51 H53"
          fill="none"
          stroke="var(--gf-brand-500)"
          stroke-width="10"
          stroke-linecap="round"
          stroke-linejoin="round"
        />
      </svg>
      <div class="brand-text">
        <span class="name">Grid<span class="brand-fpv">FPV</span></span>
        <span class="kicker">Race Director Console</span>
      </div>
    </header>

    <!-- Events is the primary action: a single full-width, prominent card on top. The three config
         pages (Pilots, Classes, Timers) sit beneath it as a smaller row. To flip Events to the
         bottom later, move the `.events-row` element after `.config-row` (or set `order` on them). -->
    <div class="cards">
      <div class="events-row">
        <button type="button" class="hub-card events feature" onclick={onevents}>
          <Card elevation="md">
            <div class="card-body">
              <svg class="card-icon" viewBox="0 0 24 24" aria-hidden="true">
                <path
                  d="M16 6 L25 11 L25 21 L16 26 L7 21 L7 11 Z"
                  transform="scale(0.85) translate(2 2)"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.8"
                  stroke-linejoin="round"
                />
              </svg>
              <h2 class="card-title">Events</h2>
              <p class="card-summary">
                <span class="count">{fmt(eventCount)}</span>
                <span class="unit">{eventCount === 1 ? 'event' : 'events'}</span>
              </p>
              <span class="card-go" aria-hidden="true">→</span>
            </div>
          </Card>
        </button>
      </div>

      <div class="config-row">
        <button type="button" class="hub-card pilots" onclick={onpilots}>
          <Card elevation="md">
            <div class="card-body">
              <svg class="card-icon" viewBox="0 0 24 24" aria-hidden="true">
                <path
                  d="M16 11a4 4 0 1 0-8 0M3 21v-1a6 6 0 0 1 6-6h6a6 6 0 0 1 6 6v1"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.8"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                />
                <circle cx="12" cy="7" r="3" fill="none" stroke="currentColor" stroke-width="1.8" />
              </svg>
              <h2 class="card-title">Pilots</h2>
              <p class="card-summary">
                <span class="count">{fmt(pilotCount)}</span>
                <span class="unit">{pilotCount === 1 ? 'pilot' : 'pilots'}</span>
              </p>
              <span class="card-go" aria-hidden="true">→</span>
            </div>
          </Card>
        </button>

        <button type="button" class="hub-card classes" onclick={onclasses}>
          <Card elevation="md">
            <div class="card-body">
              <svg class="card-icon" viewBox="0 0 24 24" aria-hidden="true">
                <path
                  d="M4 6h16M4 12h10M4 18h7"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.8"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                />
                <path
                  d="M18 14l2 2 3-4"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.8"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                />
              </svg>
              <h2 class="card-title">Classes</h2>
              <p class="card-summary">
                <span class="count">{fmt(classCount)}</span>
                <span class="unit">{classCount === 1 ? 'class' : 'classes'}</span>
              </p>
              <span class="card-go" aria-hidden="true">→</span>
            </div>
          </Card>
        </button>

        <button type="button" class="hub-card timers" onclick={ontimers}>
          <Card elevation="md">
            <div class="card-body">
              <svg class="card-icon" viewBox="0 0 24 24" aria-hidden="true">
                <path
                  d="M12 8v4l2.5 2.5M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.8"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                />
              </svg>
              <h2 class="card-title">Timers</h2>
              <p class="card-summary">
                <span class="count">{fmt(timerCount)}</span>
                <span class="unit">{timerCount === 1 ? 'timer' : 'timers'}</span>
                {#if timerCount !== undefined && timerCount !== null && timerCount > 0}
                  <span class="connected">· {fmt(connectedTimers)} connected</span>
                {/if}
              </p>
              <span class="card-go" aria-hidden="true">→</span>
            </div>
          </Card>
        </button>
      </div>
    </div>
  </div>
</div>

<style>
  .hub {
    display: grid;
    place-items: start center;
    min-height: 100vh;
    padding: var(--gf-space-8) var(--gf-space-6);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    overflow: auto;
  }
  .hub-inner {
    width: min(60rem, 100%);
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-8);
    padding-top: var(--gf-space-8);
  }
  .head {
    display: flex;
    align-items: center;
    gap: var(--gf-space-4);
  }
  .brand-mark {
    display: block;
    flex-shrink: 0;
    width: 48px;
    height: 48px;
  }
  .brand-text {
    display: flex;
    flex-direction: column;
    line-height: 1.15;
  }
  .brand-text .name {
    font-weight: var(--gf-font-weight-bold);
    font-size: var(--gf-font-size-2xl);
    letter-spacing: var(--gf-tracking-tight);
  }
  .brand-text .name .brand-fpv {
    color: var(--gf-brand-500);
  }
  .brand-text .kicker {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
  }

  /* Events on top (full width), the three config cards beneath. `.cards` stacks the two rows; flip
     the order here (e.g. `flex-direction: column-reverse`) to move Events to the bottom later. */
  .cards {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
  }
  .events-row {
    display: grid;
    grid-template-columns: 1fr;
  }
  .config-row {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: var(--gf-space-5);
  }
  @media (max-width: 44rem) {
    /* Narrow widths: the three config cards wrap into a flexible grid. */
    .config-row {
      grid-template-columns: repeat(auto-fit, minmax(13rem, 1fr));
    }
  }
  .hub-card {
    display: block;
    border: none;
    background: transparent;
    padding: 0;
    cursor: pointer;
    font-family: inherit;
    color: inherit;
    text-align: left;
    border-radius: var(--gf-radius-lg);
    transition: transform var(--gf-motion-fast) var(--gf-ease-out);
  }
  .hub-card :global(.gf-card) {
    height: 100%;
    transition:
      border-color var(--gf-motion-fast) var(--gf-ease-out),
      background var(--gf-motion-fast) var(--gf-ease-out);
  }
  .hub-card:hover {
    transform: translateY(-2px);
  }
  .hub-card:hover :global(.gf-card) {
    border-color: var(--gf-accent);
  }
  .hub-card:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
  .card-body {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
    padding: var(--gf-space-3);
    min-height: 11rem;
  }
  .card-icon {
    width: 2.5rem;
    height: 2.5rem;
    color: var(--gf-accent);
  }
  .card-title {
    margin: 0;
    font-size: var(--gf-font-size-xl);
    font-weight: var(--gf-font-weight-bold);
    letter-spacing: var(--gf-tracking-tight);
  }
  .card-summary {
    margin: auto 0 0;
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: var(--gf-space-2);
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .card-summary .count {
    font-size: var(--gf-font-size-2xl);
    font-weight: var(--gf-font-weight-bold);
    color: var(--gf-text);
    letter-spacing: var(--gf-tracking-tight);
  }
  .card-summary .unit {
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    font-size: var(--gf-font-size-xs);
  }
  .card-summary .connected {
    color: var(--gf-text-faint);
    font-size: var(--gf-font-size-xs);
  }
  .card-go {
    position: absolute;
    top: var(--gf-space-3);
    right: var(--gf-space-3);
    color: var(--gf-text-faint);
    font-size: var(--gf-font-size-xl);
    transition:
      color var(--gf-motion-fast) var(--gf-ease-out),
      transform var(--gf-motion-fast) var(--gf-ease-out);
  }
  .hub-card:hover .card-go {
    color: var(--gf-accent);
    transform: translateX(2px);
  }

  /* The prominent, full-width Events card: a taller body, a larger icon + title, and an accent
     hairline so it reads as the primary action. */
  .hub-card.feature .card-body {
    min-height: 13rem;
    padding: var(--gf-space-5);
    gap: var(--gf-space-4);
  }
  .hub-card.feature :global(.gf-card) {
    border-color: color-mix(in srgb, var(--gf-accent) 35%, var(--gf-border));
    background: linear-gradient(135deg, var(--gf-accent-soft) 0%, transparent 60%);
  }
  .hub-card.feature .card-icon {
    width: 3.25rem;
    height: 3.25rem;
  }
  .hub-card.feature .card-title {
    font-size: var(--gf-font-size-3xl);
  }
  .hub-card.feature .card-summary .count {
    font-size: var(--gf-font-size-3xl);
  }
  .hub-card.feature .card-go {
    font-size: var(--gf-font-size-2xl);
  }
</style>
