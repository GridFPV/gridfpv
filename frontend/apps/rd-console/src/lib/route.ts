/**
 * Hash-based URL routing for the RD console (#118).
 *
 * `location.hash` is the source of truth for navigation, so refresh, bookmarks, and back/forward
 * all work, while this stays a tiny parse/format helper (no router dependency).
 *
 * **Why hash, not path:** `/pilots`, `/events`, `/classes`, `/timers` are **Director API routes** — a path
 * router would collide with the server. The `#` fragment stays entirely client-side and can
 * never reach the Director, so it is collision-proof.
 *
 * The hash scheme:
 *   - `#/` (or empty)  → the home hub
 *   - `#/pilots`       → the Pilots page
 *   - `#/classes`      → the Classes page
 *   - `#/timers`       → the Timers page
 *   - `#/timers/‹timer›` → the Tune page for one timer. `‹timer›` is the timer's id,
 *     percent-encoded. A timer is app-level, so this page needs no open event.
 *   - `#/events`       → the Events page
 *   - `#/event`        → the open event's page
 *   - `#/event/run/‹heat›` → the Run view for one heat of the open event. `‹heat›` is the
 *     heat's id, percent-encoded (a heat id can hold slashes).
 *   - `#/event/marshal` → the Marshal page of the open event, and `#/event/marshal/‹race›`
 *     → the same page with one race open. `‹race›` is the race's id, percent-encoded.
 *   - `#/overlay/race` → the race overlay, a page for an OBS browser source: light text, for
 *     dark video. `#/overlay/race/light` is the same page with dark text on a light panel,
 *     for bright video. An overlay is drawn bare (no shell) on a see-through page, and
 *     follows the Director's active event by itself. `#/overlay/‹anything else›` draws
 *     nothing at all, so a mistyped source never puts the console in the video.
 *
 * The open event itself is **app-wide server state** (the Director's active event, #90), so the
 * hash does not name *which* event. The event page always shows the Director's active event; an
 * `#/event` hash with no active event reconciles to the Events page rather than rendering an
 * empty page (see {@link reconcileRoute}).
 */
import type { AdapterId, PhaseId, RaceId } from '@gridfpv/types';

/** The app-level pages shown outside an event. */
export type AppPage = 'home' | 'events' | 'pilots' | 'classes' | 'timers';

/**
 * A parsed route. An app-level page, the Tune page for one timer, the open event's page, the
 * Run view for one heat of the open event, or its Marshal page (with or without a race open). This is the pure view-intent the hash encodes; reconciliation against the
 * live active event happens in {@link reconcileRoute}.
 */
export type Route =
  | { kind: 'page'; page: AppPage }
  | { kind: 'tune'; timer: AdapterId }
  | { kind: 'event' }
  | { kind: 'run'; heat: PhaseId }
  | { kind: 'marshal'; race?: RaceId }
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
function decodeSegment(segment: string): string | undefined {
  try {
    return decodeURIComponent(segment);
  } catch {
    return undefined;
  }
}

const APP_PAGES: readonly AppPage[] = ['home', 'events', 'pilots', 'classes', 'timers'];

/** The route shown when the hash is empty or unrecognised: the home hub. */
export const DEFAULT_ROUTE: Route = { kind: 'page', page: 'home' };

/**
 * Parse a `location.hash` string into a {@link Route}. Tolerant of a leading `#`, a leading `#/`,
 * a missing slash, a trailing slash, and letter case. Unknown/empty hashes fall back to
 * {@link DEFAULT_ROUTE} (the hub), so a stale or hand-edited URL never wedges the shell.
 */
export function parseHash(hash: string): Route {
  // Strip a leading '#', then a leading '/', then any trailing '/'.
  const s = hash.replace(/^#/, '').replace(/^\//, '').replace(/\/$/, '');
  if (s === '') return DEFAULT_ROUTE;

  const segments = s.split('/');
  const head = segments[0].toLowerCase();
  if (head === 'overlay') {
    // `overlay/‹name›` and `overlay/‹name›/light`. An unknown name, or no name, is an overlay
    // that draws nothing: a browser source must never fall back to the hub. Any third part
    // other than `light` is the default contrast, so a mistyped one still draws the overlay.
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
    // `event/run/‹heat›`: the heat id keeps its case and is one encoded segment. A run hash
    // with no heat, or a broken one, falls back to the event page.
    if (segments[1]?.toLowerCase() === 'run' && segments.length === 3) {
      const heat = decodeSegment(segments[2]);
      if (heat) return { kind: 'run', heat };
    }
    // `event/marshal` and `event/marshal/‹race›`. A broken race segment opens the page with
    // no race chosen.
    if (segments[1]?.toLowerCase() === 'marshal' && segments.length <= 3) {
      const race = segments.length === 3 ? decodeSegment(segments[2]) : undefined;
      return race ? { kind: 'marshal', race } : { kind: 'marshal' };
    }
    return { kind: 'event' };
  }
  if (head === 'timers' && segments.length === 2) {
    // `timers/‹timer›`: the timer id keeps its case and is one encoded segment. A broken one
    // falls back to the Timers page.
    const timer = decodeSegment(segments[1]);
    if (timer) return { kind: 'tune', timer };
    return { kind: 'page', page: 'timers' };
  }
  if ((APP_PAGES as readonly string[]).includes(head)) {
    return { kind: 'page', page: head as AppPage };
  }
  return DEFAULT_ROUTE;
}

/**
 * Format a {@link Route} back into a hash string (always with the leading `#/`). The hub formats
 * to `#/` (canonical), the pages to `#/<page>`, a timer's Tune page to `#/timers/‹encoded timer
 * id›`, the event page to `#/event` and a heat's Run view to `#/event/run/‹encoded heat id›`. Round-trips with {@link parseHash} for every route.
 * An overlay formats to `#/overlay/‹name›`, with `/light` for the light panel; the overlay
 * with no name formats to `#/overlay`.
 */
export function formatHash(route: Route): string {
  if (route.kind === 'overlay') {
    if (route.overlay === undefined) return '#/overlay';
    return route.theme === 'light'
      ? `#/overlay/${route.overlay}/light`
      : `#/overlay/${route.overlay}`;
  }
  if (route.kind === 'event') return '#/event';
  if (route.kind === 'run') return `#/event/run/${encodeURIComponent(route.heat)}`;
  if (route.kind === 'marshal') {
    return route.race === undefined
      ? '#/event/marshal'
      : `#/event/marshal/${encodeURIComponent(route.race)}`;
  }
  if (route.kind === 'tune') return `#/timers/${encodeURIComponent(route.timer)}`;
  if (route.page === 'home') return '#/';
  return `#/${route.page}`;
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
 *   - An `event`, `run` or `marshal` route with **no active event** → fall back to the Events page
 *     (`#/events`), since there is no event to show.
 *   - A `tune` route never needs an event: a timer is app-level, like the pages.
 *   - An `overlay` route is never changed: with no event it draws its idle mark, which is
 *     what a browser source left in a scene should do.
 *   - A `page` route with an **active event** is fine as-is: the user explicitly navigated to a
 *     top-level page even though an event is active, so honour it. This includes the hub: an
 *     empty/hub hash stays on the hub even when an event is active — the hash is authoritative,
 *     so being outside an event survives a reload.
 */
export function reconcileRoute(route: Route, hasActiveEvent: boolean): Route {
  if (inEvent(route) && !hasActiveEvent) {
    return { kind: 'page', page: 'events' };
  }
  return route;
}

/**
 * Resolve the route to show on **initial load**. The **hash is authoritative**: where you were
 * survives a reload, and the shell never opens an event from a bare/hub load.
 *
 *   - Empty/hub hash → the hub, **regardless of whether an event is active**.
 *   - An explicit page hash (`#/pilots` etc.) → that page (honoured even if an event is active).
 *   - An `#/event` hash → the event page if an event is active, else the Events page
 *     (via {@link reconcileRoute}).
 *
 * This is just the parse composed with {@link reconcileRoute}.
 */
export function resolveInitialRoute(hash: string, hasActiveEvent: boolean): Route {
  return reconcileRoute(parseHash(hash), hasActiveEvent);
}
