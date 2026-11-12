/**
 * Hash-based URL routing for the RD console.
 *
 * `location.hash` is the source of truth for navigation, so refresh, bookmarks, and back/forward
 * all work, while this stays a tiny parse/format helper (no router dependency).
 *
 * **Why hash, not path:** `/pilots`, `/events`, `/classes`, `/timers`, `/formats` are **Director
 * API routes** — a path router would collide with the server. The `#` fragment stays entirely
 * client-side and can never reach the Director, so it is collision-proof.
 *
 * The console is one row of pages (see `AppNav.svelte`). Some belong to the open event and
 * some to the whole Director:
 *
 *   - `#/` (or empty)   → the open event's page
 *   - `#/events`        → the Events page: open, add or delete an event
 *   - `#/event`         → the open event's page (details, lap rules, classes)
 *   - `#/event/phases`  → the Phases page: the competition as a tree, where heats are added
 *   - `#/event/run`     → the Run page, on the heat the Director is racing or would race next,
 *     and `#/event/run/‹heat›` → the Run page for one heat. `‹heat›` is the heat's id,
 *     percent-encoded (a heat id can hold slashes).
 *   - `#/event/marshal` → the Marshal page, and `#/event/marshal/‹race›` → the same page with
 *     one race open. `‹race›` is the race's id, percent-encoded.
 *   - `#/event/results` → the Results page
 *   - `#/event/overlays` → the Overlays page: the URLs that go into OBS
 *   - `#/seats`         → the Seats page for the first timer, and `#/seats/‹timer›` → for one
 *     timer. `#/seats/‹timer›/‹seat›` → that seat's Tune page (record, then set levels).
 *   - `#/timers`        → the Timers page: add, find and remove timers
 *   - `#/pilots`, `#/classes`, `#/formats` → the lists that outlive events
 *   - `#/help`          → the help index, and `#/help/‹page›` → one help page
 *   - `#/overlay/race`  → the race overlay, a page for an OBS browser source: light text, for
 *     dark video. `#/overlay/race/light` is the same page with dark text on a light panel,
 *     for bright video. An overlay is drawn bare (no shell) on a see-through page, and
 *     follows the Director's active event by itself. `#/overlay/‹anything else›` draws
 *     nothing at all, so a mistyped source never puts the console in the video.
 *
 * The open event itself is **app-wide server state** (the Director's active event), so the
 * hash does not name *which* event. An event route with no active event reconciles to the
 * Events page rather than rendering an empty page (see {@link reconcileRoute}).
 */
import type { AdapterId, PhaseId, RaceId, SeatRef } from '@gridfpv/types';

/** The pages that need no open event. */
export type AppPage = 'events' | 'pilots' | 'classes' | 'timers' | 'formats';

/** The pages of the open event that take no further argument. */
export type EventSection = 'event' | 'phases' | 'results' | 'overlays';

/**
 * A parsed route. This is the pure view-intent the hash encodes; reconciliation against the
 * live active event happens in {@link reconcileRoute}.
 */
export type Route =
  | { kind: 'page'; page: AppPage }
  /** The Seats page, for one timer or (with none named) the first one. */
  | { kind: 'seats'; timer?: AdapterId }
  /** One seat's Tune page: record its signal, then set its levels on the recording. */
  | { kind: 'capture'; timer: AdapterId; seat: SeatRef }
  | { kind: 'event'; section: EventSection }
  /** The Run page. With no heat named it opens on the heat being raced, or the next one. */
  | { kind: 'run'; heat?: PhaseId }
  | { kind: 'marshal'; race?: RaceId }
  | { kind: 'help'; slug?: string }
  /**
   * An overlay page for an OBS browser source. `overlay` is absent for a name the console
   * does not have: that page draws nothing. `theme` is only ever present as `'light'`
   * (dark text on a light panel); absent means the default, light text on a dark panel.
   */
  | { kind: 'overlay'; overlay?: OverlayName; theme?: 'light' };

/** The overlay pages the console has. */
export type OverlayName = 'race';
export const OVERLAYS: readonly OverlayName[] = ['race'];
/**
 * An overlay's contrast, named for its panel: `'dark'` is light text on a dark panel (for
 * dark video), `'light'` is dark text on a light panel (for bright video).
 */
export type OverlayTheme = 'dark' | 'light';

/** Decode one percent-encoded hash segment, or `undefined` if it is not valid encoding. */
function decodeSegment(segment: string | undefined): string | undefined {
  if (!segment) return undefined;
  try {
    return decodeURIComponent(segment);
  } catch {
    return undefined;
  }
}

const APP_PAGES: readonly AppPage[] = ['events', 'pilots', 'classes', 'timers', 'formats'];
const EVENT_SECTIONS: readonly EventSection[] = ['event', 'phases', 'results', 'overlays'];

/** The route shown when the hash is empty or unrecognised: the open event's page. */
export const DEFAULT_ROUTE: Route = { kind: 'event', section: 'event' };

/**
 * Parse a `location.hash` string into a {@link Route}. Tolerant of a leading `#`, a leading `#/`,
 * a missing slash, a trailing slash, and letter case. Unknown/empty hashes fall back to
 * {@link DEFAULT_ROUTE}, so a stale or hand-edited URL never wedges the shell.
 */
export function parseHash(hash: string): Route {
  // Strip a leading '#', then a leading '/', then any trailing '/'.
  const s = hash.replace(/^#/, '').replace(/^\//, '').replace(/\/$/, '');
  if (s === '') return DEFAULT_ROUTE;

  const segments = s.split('/');
  const head = segments[0].toLowerCase();
  if (head === 'overlay') {
    // `overlay/‹name›` and `overlay/‹name›/light`. An unknown name, or no name, is an overlay
    // that draws nothing: a browser source must never fall back to a console page. Any third
    // part other than `light` is the default contrast, so a mistyped one still draws the overlay.
    const name = segments[1]?.toLowerCase();
    if (name === undefined || !(OVERLAYS as readonly string[]).includes(name)) {
      return { kind: 'overlay' };
    }
    const overlay = name as OverlayName;
    return segments[2]?.toLowerCase() === 'light'
      ? { kind: 'overlay', overlay, theme: 'light' }
      : { kind: 'overlay', overlay };
  }
  if (head === 'event') {
    const section = segments[1]?.toLowerCase();
    // `event/run` and `event/run/‹heat›`: the heat id keeps its case and is one encoded
    // segment. A broken one opens the page with no heat named.
    if (section === 'run' && segments.length <= 3) {
      const heat = decodeSegment(segments[2]);
      return heat ? { kind: 'run', heat } : { kind: 'run' };
    }
    // `event/marshal` and `event/marshal/‹race›`. A broken race segment opens the page with
    // no race chosen.
    if (section === 'marshal' && segments.length <= 3) {
      const race = decodeSegment(segments[2]);
      return race ? { kind: 'marshal', race } : { kind: 'marshal' };
    }
    if (segments.length === 2 && (EVENT_SECTIONS as readonly string[]).includes(section ?? '')) {
      return { kind: 'event', section: section as EventSection };
    }
    return DEFAULT_ROUTE;
  }
  if (head === 'seats' && segments.length <= 3) {
    // `seats`, `seats/‹timer›` and `seats/‹timer›/‹seat›`. Ids keep their case. A broken
    // segment falls back one level.
    const timer = decodeSegment(segments[1]);
    const seat = decodeSegment(segments[2]);
    if (timer && seat) return { kind: 'capture', timer, seat };
    return timer ? { kind: 'seats', timer } : { kind: 'seats' };
  }
  if (head === 'help' && segments.length <= 2) {
    const slug = decodeSegment(segments[1])?.toLowerCase();
    return slug ? { kind: 'help', slug } : { kind: 'help' };
  }
  if (segments.length === 1 && (APP_PAGES as readonly string[]).includes(head)) {
    return { kind: 'page', page: head as AppPage };
  }
  return DEFAULT_ROUTE;
}

/**
 * Format a {@link Route} back into a hash string (always with the leading `#/`). Round-trips
 * with {@link parseHash} for every route.
 */
export function formatHash(route: Route): string {
  const part = encodeURIComponent;
  switch (route.kind) {
    case 'overlay':
      if (route.overlay === undefined) return '#/overlay';
      return route.theme === 'light'
        ? `#/overlay/${route.overlay}/light`
        : `#/overlay/${route.overlay}`;
    case 'event':
      return route.section === 'event' ? '#/event' : `#/event/${route.section}`;
    case 'run':
      return route.heat === undefined ? '#/event/run' : `#/event/run/${part(route.heat)}`;
    case 'marshal':
      return route.race === undefined ? '#/event/marshal' : `#/event/marshal/${part(route.race)}`;
    case 'seats':
      return route.timer === undefined ? '#/seats' : `#/seats/${part(route.timer)}`;
    case 'capture':
      return `#/seats/${part(route.timer)}/${part(route.seat)}`;
    case 'help':
      return route.slug === undefined ? '#/help' : `#/help/${part(route.slug)}`;
    case 'page':
      return `#/${route.page}`;
  }
}

/** Whether a route shows part of the open event, so it needs one. */
export function inEvent(route: Route): boolean {
  return route.kind === 'event' || route.kind === 'run' || route.kind === 'marshal';
}

/**
 * Whether a route is an overlay: drawn bare on a see-through page, with no shell, no token
 * prompt, no toasts and no sound.
 */
export function isOverlay(route: Route): boolean {
  return route.kind === 'overlay';
}

/**
 * Reconcile a parsed {@link Route} against whether an event is currently active (the Director's
 * active event, server state):
 *
 *   - An event route with **no active event** → the Events page (`#/events`), since there is no
 *     event to show. That is also where a fresh Director lands: its first act is to add one.
 *   - Every other route never needs an event and is left alone. An `overlay` with no event
 *     draws its idle mark, which is what a browser source left in a scene should do.
 */
export function reconcileRoute(route: Route, hasActiveEvent: boolean): Route {
  if (inEvent(route) && !hasActiveEvent) {
    return { kind: 'page', page: 'events' };
  }
  return route;
}

/**
 * Resolve the route to show on **initial load**: the parse composed with
 * {@link reconcileRoute}. The hash is authoritative, so where you were survives a reload.
 */
export function resolveInitialRoute(hash: string, hasActiveEvent: boolean): Route {
  return reconcileRoute(parseHash(hash), hasActiveEvent);
}
