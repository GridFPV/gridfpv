<script lang="ts">
  /**
   * AppNav — what every console page sits under: the brand, the switch between the two
   * sides of the console, and the row of pages of the side that is showing.
   *
   * - **Event** is the open event: Details, Phases, Run, Marshal, Results, Overlays. These are
   *   the pages used all race day.
   *
   * The switch reads just "Setup" and "Event"; the rest is in its tooltips. The open event's
   * name is at the top right, and leads to the Events page.
   * - **Setup** is what is kept for every event: Events (open, add or delete one), Timers,
   *   Seats, Pilots, Classes, Formats.
   *
   * The side follows the page that is showing, so a link or the back button lands on the
   * right one. Each side remembers the page it was last on: switching to Setup and back
   * returns to the same page of the event. Help is on both rows and leaves the side as it
   * was.
   *
   * Everything here is a plain link (`#/…`), so middle-click, copy-link and the back button
   * all work. With no event open the Event side leads to the Events page, where one is
   * opened or added.
   */
  import { Badge } from '@gridfpv/components';
  import Brand from './Brand.svelte';
  import { formatHash, sideOf, type Route, type Side } from './lib/route.js';

  let {
    route,
    eventName,
    toMarshal = 0
  }: {
    /** The page showing now. */
    route: Route;
    /** The open event's name, if one is open. */
    eventName?: string;
    /** How many races need a look or have an open protest. */
    toMarshal?: number;
  } = $props();

  interface Entry {
    key: string;
    label: string;
    to: Route;
  }
  /** The open event's pages, in the order a race day uses them. */
  const EVENT_PAGES: readonly Entry[] = [
    { key: 'event', label: 'Details', to: { kind: 'event', section: 'event' } },
    { key: 'phases', label: 'Phases', to: { kind: 'event', section: 'phases' } },
    { key: 'run', label: 'Run', to: { kind: 'run' } },
    { key: 'marshal', label: 'Marshal', to: { kind: 'marshal' } },
    { key: 'results', label: 'Results', to: { kind: 'event', section: 'results' } },
    { key: 'overlays', label: 'Overlays', to: { kind: 'event', section: 'overlays' } }
  ];
  /** What is kept for every event. */
  const SETUP_PAGES: readonly Entry[] = [
    { key: 'events', label: 'Events', to: { kind: 'page', page: 'events' } },
    { key: 'timers', label: 'Timers', to: { kind: 'page', page: 'timers' } },
    { key: 'seats', label: 'Seats', to: { kind: 'seats' } },
    { key: 'pilots', label: 'Pilots', to: { kind: 'page', page: 'pilots' } },
    { key: 'classes', label: 'Classes', to: { kind: 'page', page: 'classes' } },
    { key: 'formats', label: 'Formats', to: { kind: 'page', page: 'formats' } }
  ];
  const HELP: Entry = { key: 'help', label: 'Help', to: { kind: 'help' } };

  /** Which entry the showing page belongs under. */
  const current = $derived.by((): string | undefined => {
    switch (route.kind) {
      case 'event':
        return route.section;
      case 'run':
      case 'marshal':
      case 'help':
        return route.kind;
      case 'seats':
      case 'capture':
        return 'seats';
      case 'page':
        return route.page;
      default:
        return undefined;
    }
  });

  // The side showing, and the page each side was last on. A page that is on neither side
  // (help) leaves them as they were. Until a page of either side has been shown (the
  // console was loaded on a help page), the side is the event's if one is open.
  let shown = $state<Side | undefined>(undefined);
  let lastOf = $state<Record<Side, Route | undefined>>({ setup: undefined, event: undefined });
  $effect.pre(() => {
    const now = sideOf(route);
    if (!now) return;
    shown = now;
    lastOf[now] = route;
  });
  const side = $derived(shown ?? sideOf(route) ?? (eventName ? 'event' : 'setup'));

  const eventsHref = formatHash({ kind: 'page', page: 'events' });
  /** Where each side of the switch goes: the page that side was last on. */
  const setupHref = $derived(lastOf.setup ? formatHash(lastOf.setup) : eventsHref);
  const eventHref = $derived(
    !eventName ? eventsHref : formatHash(lastOf.event ?? { kind: 'event', section: 'event' })
  );
  const pages = $derived([...(side === 'event' ? EVENT_PAGES : SETUP_PAGES), HELP]);
  const marshalWords = $derived(
    `${toMarshal} ${toMarshal === 1 ? 'race needs' : 'races need'} a look`
  );
</script>

<header class="nav">
  <Brand onclick={() => (location.hash = eventHref)} />
  <nav class="sides" aria-label="Setup or event">
    <a
      href={setupHref}
      class:active={side === 'setup'}
      aria-current={side === 'setup' ? 'true' : undefined}
      title="What is kept for every event: events, timers, seats, pilots, classes, formats"
    >
      <span class="side-name">Setup</span>
    </a>
    <a
      href={eventHref}
      class:active={side === 'event'}
      class:dim={!eventName}
      aria-current={side === 'event' ? 'true' : undefined}
      aria-label={eventName
        ? `Event: ${eventName}${toMarshal > 0 && side !== 'event' ? `, ${marshalWords}` : ''}`
        : 'Event: none open'}
      title={eventName ? 'The open event' : 'No event is open. Open or add one.'}
    >
      <span class="side-name">
        Event
        {#if toMarshal > 0 && side !== 'event'}
          <span data-testid="marshal-count"
            ><Badge tone="warn" variant="solid">{toMarshal}</Badge></span
          >
        {/if}
      </span>
    </a>
  </nav>
  <nav class="pages" aria-label="Pages">
    {#each pages as page (page.key)}
      <a
        href={formatHash(page.to)}
        class:active={current === page.key}
        aria-current={current === page.key ? 'page' : undefined}
        aria-label={page.key === 'marshal' && toMarshal > 0
          ? `Marshal, ${marshalWords}`
          : undefined}
      >
        {page.label}
        {#if page.key === 'marshal' && toMarshal > 0}
          <span data-testid="marshal-count"
            ><Badge tone="warn" variant="solid">{toMarshal}</Badge></span
          >
        {/if}
      </a>
    {/each}
  </nav>
  <a
    class="open-event"
    class:none={!eventName}
    href={eventsHref}
    data-testid="open-event"
    title="The open event. Open, add or delete an event."
  >
    {eventName ?? 'No event open'}
  </a>
</header>

<style>
  .nav {
    display: flex;
    align-items: center;
    gap: var(--gf-space-5);
    flex-wrap: wrap;
    width: min(76rem, 100%);
    margin: 0 auto;
    padding: var(--gf-space-4) var(--gf-space-6) 0;
    font-family: var(--gf-font-family);
  }
  /* The switch: two halves of one control. */
  .sides {
    display: flex;
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    overflow: hidden;
  }
  .sides a {
    display: flex;
    align-items: center;
    padding: var(--gf-space-2) var(--gf-space-4);
    text-decoration: none;
    color: var(--gf-text-muted);
    line-height: 1.25;
  }
  .sides a + a {
    border-left: 1px solid var(--gf-border);
  }
  .sides a:hover {
    color: var(--gf-text);
  }
  .sides a.active {
    color: var(--gf-text);
    background: var(--gf-surface);
    box-shadow: inset 0 -2px 0 var(--gf-accent);
  }
  .sides a.dim:not(.active) {
    color: var(--gf-text-faint);
  }
  .side-name {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
  }
  .open-event {
    margin-left: auto;
    max-width: 18rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    padding: var(--gf-space-1) var(--gf-space-2);
    border-radius: var(--gf-radius-md);
    text-decoration: none;
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
    color: var(--gf-text);
  }
  .open-event:hover {
    background: var(--gf-surface);
  }
  .open-event.none {
    font-weight: var(--gf-font-weight-regular);
    color: var(--gf-text-muted);
  }
  .pages {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-1);
  }
  .pages a {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    padding: var(--gf-space-1) var(--gf-space-3);
    border-radius: var(--gf-radius-md);
    text-decoration: none;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-md);
  }
  .pages a:hover {
    color: var(--gf-text);
  }
  .pages a.active {
    color: var(--gf-text);
    background: var(--gf-surface);
  }
  a:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
</style>
