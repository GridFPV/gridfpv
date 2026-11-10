/**
 * Hash-based URL routing for the RD console (#118).
 *
 * `location.hash` is the source of truth for navigation, so refresh, bookmarks, and back/forward
 * all work, while this stays a tiny parse/format helper (no router dependency).
 *
 * **Why hash, not path:** `/pilots`, `/events`, `/classes` are **Director API routes** — a path
 * router would collide with the server. The `#` fragment stays entirely client-side and can
 * never reach the Director, so it is collision-proof.
 *
 * The hash scheme:
 *   - `#/` (or empty)  → the home hub
 *   - `#/pilots`       → the Pilots page
 *   - `#/classes`      → the Classes page
 *   - `#/events`       → the Events page
 *   - `#/event`        → the open event's page
 *   - `#/event/run/‹heat›` → the Run view for one heat of the open event. `‹heat›` is the
 *     heat's id, percent-encoded (a heat id can hold slashes).
 *
 * The open event itself is **app-wide server state** (the Director's active event, #90), so the
 * hash does not name *which* event. The event page always shows the Director's active event; an
 * `#/event` hash with no active event reconciles to the Events page rather than rendering an
 * empty page (see {@link reconcileRoute}).
 */
import type { PhaseId } from '@gridfpv/types';

/** The app-level pages shown outside an event. */
export type AppPage = 'home' | 'events' | 'pilots' | 'classes';

/**
 * A parsed route. An app-level page, the open event's page, or the Run view for one heat of
 * the open event. This is the pure view-intent the hash encodes; reconciliation against the
 * live active event happens in {@link reconcileRoute}.
 */
export type Route =
  | { kind: 'page'; page: AppPage }
  | { kind: 'event' }
  | { kind: 'run'; heat: PhaseId };

/** Decode one percent-encoded hash segment, or `undefined` if it is not valid encoding. */
function decodeSegment(segment: string): string | undefined {
  try {
    return decodeURIComponent(segment);
  } catch {
    return undefined;
  }
}

const APP_PAGES: readonly AppPage[] = ['home', 'events', 'pilots', 'classes'];

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
  if (head === 'event') {
    // `event/run/‹heat›`: the heat id keeps its case and is one encoded segment. A run hash
    // with no heat, or a broken one, falls back to the event page.
    if (segments[1]?.toLowerCase() === 'run' && segments.length === 3) {
      const heat = decodeSegment(segments[2]);
      if (heat) return { kind: 'run', heat };
    }
    return { kind: 'event' };
  }
  if ((APP_PAGES as readonly string[]).includes(head)) {
    return { kind: 'page', page: head as AppPage };
  }
  return DEFAULT_ROUTE;
}

/**
 * Format a {@link Route} back into a hash string (always with the leading `#/`). The hub formats
 * to `#/` (canonical), the pages to `#/<page>`, the event page to `#/event` and a heat's Run view
 * to `#/event/run/‹encoded heat id›`. Round-trips with {@link parseHash} for every route.
 */
export function formatHash(route: Route): string {
  if (route.kind === 'event') return '#/event';
  if (route.kind === 'run') return `#/event/run/${encodeURIComponent(route.heat)}`;
  if (route.page === 'home') return '#/';
  return `#/${route.page}`;
}

/**
 * Reconcile a parsed {@link Route} against whether an event is currently active (the Director's
 * active event, server state):
 *
 *   - An `event` or `run` route with **no active event** → fall back to the Events page
 *     (`#/events`), since there is no event to show.
 *   - A `page` route with an **active event** is fine as-is: the user explicitly navigated to a
 *     top-level page even though an event is active, so honour it. This includes the hub: an
 *     empty/hub hash stays on the hub even when an event is active — the hash is authoritative,
 *     so being outside an event survives a reload.
 */
export function reconcileRoute(route: Route, hasActiveEvent: boolean): Route {
  if (route.kind !== 'page' && !hasActiveEvent) {
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
