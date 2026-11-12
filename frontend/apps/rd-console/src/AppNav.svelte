<script lang="ts">
  /**
   * AppNav — the one row every console page sits under: the brand, the pages, and the open
   * event.
   *
   * The pages are plain links (`#/…`), so middle-click, copy-link and the back button all
   * work. Pages that belong to an event (Event, Phases, Run, Marshal, Results, Overlays)
   * are dimmed while no event is open; following one lands on the Events page, where an
   * event is opened or added.
   *
   * On the right is the open event's name. It leads to the Events page, which is where
   * another event is opened.
   */
  import { Badge } from '@gridfpv/components';
  import Brand from './Brand.svelte';
  import { formatHash, type Route } from './lib/route.js';

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

  /** The pages, in the order a race day uses them. */
  type Key =
    | 'event'
    | 'seats'
    | 'pilots'
    | 'phases'
    | 'formats'
    | 'run'
    | 'marshal'
    | 'results'
    | 'overlays'
    | 'help';
  const PAGES: readonly { key: Key; label: string; to: Route; needsEvent: boolean }[] = [
    { key: 'event', label: 'Event', to: { kind: 'event', section: 'event' }, needsEvent: true },
    { key: 'seats', label: 'Seats', to: { kind: 'seats' }, needsEvent: false },
    { key: 'pilots', label: 'Pilots', to: { kind: 'page', page: 'pilots' }, needsEvent: false },
    { key: 'phases', label: 'Phases', to: { kind: 'event', section: 'phases' }, needsEvent: true },
    { key: 'formats', label: 'Formats', to: { kind: 'page', page: 'formats' }, needsEvent: false },
    { key: 'run', label: 'Run', to: { kind: 'run' }, needsEvent: true },
    { key: 'marshal', label: 'Marshal', to: { kind: 'marshal' }, needsEvent: true },
    {
      key: 'results',
      label: 'Results',
      to: { kind: 'event', section: 'results' },
      needsEvent: true
    },
    {
      key: 'overlays',
      label: 'Overlays',
      to: { kind: 'event', section: 'overlays' },
      needsEvent: true
    },
    { key: 'help', label: 'Help', to: { kind: 'help' }, needsEvent: false }
  ];

  /** Which nav entry the showing page belongs under. The Events page belongs under none. */
  const current = $derived.by((): Key | undefined => {
    switch (route.kind) {
      case 'event':
        return route.section;
      case 'run':
        return 'run';
      case 'marshal':
        return 'marshal';
      case 'seats':
      case 'capture':
        return 'seats';
      case 'help':
        return 'help';
      case 'page':
        // Classes sit beside Pilots, and Timers behind Seats.
        if (route.page === 'pilots' || route.page === 'classes') return 'pilots';
        if (route.page === 'timers') return 'seats';
        if (route.page === 'formats') return 'formats';
        return undefined;
      default:
        return undefined;
    }
  });

  const eventsHref = formatHash({ kind: 'page', page: 'events' });
  const homeHref = $derived(
    eventName ? formatHash({ kind: 'event', section: 'event' }) : eventsHref
  );
</script>

<header class="nav">
  <Brand onclick={() => (location.hash = homeHref)} />
  <nav aria-label="Pages">
    {#each PAGES as page (page.key)}
      <a
        href={formatHash(page.to)}
        class:active={current === page.key}
        class:dim={page.needsEvent && !eventName}
        aria-current={current === page.key ? 'page' : undefined}
        aria-label={page.key === 'marshal' && toMarshal > 0
          ? `Marshal, ${toMarshal} ${toMarshal === 1 ? 'race needs' : 'races need'} a look`
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
    class="event"
    class:active={route.kind === 'page' && route.page === 'events'}
    href={eventsHref}
    title="Open, add or delete an event"
  >
    <span class="event-label">Event</span>
    <span class="event-name">{eventName ?? 'None open'}</span>
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
  nav {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-1);
    margin-right: auto;
  }
  nav a {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    padding: var(--gf-space-1) var(--gf-space-3);
    border-radius: var(--gf-radius-md);
    text-decoration: none;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-md);
  }
  nav a:hover {
    color: var(--gf-text);
  }
  nav a.active {
    color: var(--gf-text);
    background: var(--gf-surface);
  }
  nav a.dim:not(.active) {
    color: var(--gf-text-faint);
  }
  nav a:focus-visible,
  .event:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
  .event {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    max-width: 16rem;
    padding: var(--gf-space-1) var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    text-decoration: none;
    color: var(--gf-text);
    line-height: 1.2;
  }
  .event:hover,
  .event.active {
    background: var(--gf-surface);
  }
  .event-label {
    font-size: var(--gf-font-size-2xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .event-name {
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
  }
</style>
