/**
 * Hash-based URL routing for the RD console (#118).
 *
 * The console's view (which app-level page, or which in-event workspace tab) used to live only in
 * in-memory state, so a browser **refresh reset it** — losing where you were. This module makes
 * `location.hash` the source of truth for navigation so refresh, bookmarks, and back/forward all
 * work, while staying a tiny parse/format helper (no router dependency).
 *
 * **Why hash, not path:** `/pilots`, `/events`, `/timers` are already **Director API routes** — a
 * path router would collide with the server. The `#` fragment stays entirely client-side and can
 * never reach the Director, so it is collision-proof.
 *
 * The hash scheme:
 *   - `#/` (or empty)        → the home hub
 *   - `#/pilots`             → the Pilots page
 *   - `#/classes`            → the Classes page
 *   - `#/events`             → the Events page (the picker)
 *   - `#/timers`             → the Timers page
 *   - `#/timers/<id>/tune`   → the **Tune** page for one timer (#355) — the only *parameterised*
 *                              route, and the reason this scheme grew a third `Route` kind
 *   - `#/event/<tab>`        → the in-event workspace on `<tab>`
 *                              (tab ∈ classes-roster | rounds | live | marshaling | results | audit | timers)
 *
 * **Why tuning is a page with an id in its URL (#355):** tuning a gate is a two-location activity —
 * set a threshold, walk to the gate, wave a quad through, walk back, read the graph. The console is
 * served over the LAN, so a *linkable* tune view lets the RD open it on a phone at the gate instead
 * of walking back for every adjustment. A modal has no URL, so that case is simply unavailable.
 *
 * The active event itself is **app-wide server state** (the Director's active event, #90), so the
 * hash only restores *which tab* of the workspace — not *which* event. The workspace always shows
 * the Director's active event; an `#/event/<tab>` hash with no active event reconciles to the
 * Events page rather than rendering a broken workspace (see {@link reconcileRoute}).
 */

import type { TimerId } from '@gridfpv/types';

/** The app-level pages shown when no event is selected. */
export type AppPage = 'home' | 'events' | 'timers' | 'pilots' | 'classes';

/** The in-event workspace sidebar tabs. */
export type WorkspaceTab =
  | 'classes-roster'
  | 'rounds'
  | 'live'
  | 'marshaling'
  | 'results'
  | 'audit'
  | 'timers';

/**
 * A parsed route. Either an app-level page (no event), or the in-event workspace on a tab. This is
 * the pure view-intent the hash encodes; reconciliation against the live active event happens in
 * {@link reconcileRoute}.
 */
export type Route =
  | { kind: 'page'; page: AppPage }
  | { kind: 'workspace'; tab: WorkspaceTab }
  /**
   * The **Tune** page for one timer (#355), `#/timers/<id>/tune`. The first parameterised route:
   * it carries the {@link TimerId} it is tuning, so the view survives a reload and can be opened
   * on a second device. App-level (no event required) — a timer is tuned before an event exists.
   */
  | { kind: 'tune'; timer: TimerId };

export const WORKSPACE_TABS: readonly WorkspaceTab[] = [
  'classes-roster',
  'rounds',
  'live',
  'marshaling',
  'results',
  'audit',
  'timers'
];

const APP_PAGES: readonly AppPage[] = ['home', 'events', 'timers', 'pilots', 'classes'];

/** The route shown when the hash is empty or unrecognised: the home hub. */
export const DEFAULT_ROUTE: Route = { kind: 'page', page: 'home' };

/**
 * Parse a `location.hash` string into a {@link Route}. Tolerant of a leading `#`, a leading `#/`,
 * a missing slash, and a trailing slash. Unknown/empty hashes fall back to {@link DEFAULT_ROUTE}
 * (the hub), so a stale or hand-edited URL never wedges the shell.
 */
export function parseHash(hash: string): Route {
  // Strip a leading '#', then a leading '/', then any trailing '/'.
  const s = hash.replace(/^#/, '').replace(/^\//, '').replace(/\/$/, '');
  if (s === '') return DEFAULT_ROUTE;

  // Only the *keyword* segments are lower-cased (tolerance for a hand-typed URL). A parameter
  // segment — the tune route's timer id — keeps its case verbatim, so the route round-trips the id
  // the registry actually holds rather than a mangled one.
  const segments = s.split('/');
  const head = segments[0].toLowerCase();

  if (head === 'event') {
    const tab = segments[1]?.toLowerCase();
    if (tab && (WORKSPACE_TABS as readonly string[]).includes(tab)) {
      return { kind: 'workspace', tab: tab as WorkspaceTab };
    }
    // `#/event` with no/unknown tab → the workspace's default tab (live).
    return { kind: 'workspace', tab: 'live' };
  }

  // `#/timers/<id>/tune` — the parameterised tune route (#355). It nests under the Timers page
  // deliberately: that is where it is entered from, and it makes the Home › Timers › Tune trail
  // honest. A malformed variant (`#/timers/<id>`, or an empty id) degrades to the Timers page
  // rather than the hub — the RD asked for something timer-shaped, so land them on timers.
  if (head === 'timers' && segments.length >= 3) {
    if (segments[2].toLowerCase() !== 'tune') return { kind: 'page', page: 'timers' };
    let timer = segments[1];
    try {
      timer = decodeURIComponent(timer);
    } catch {
      // A hand-mangled `%` escape — take the segment verbatim rather than throwing out of a parse.
    }
    if (timer === '') return { kind: 'page', page: 'timers' };
    return { kind: 'tune', timer };
  }

  if ((APP_PAGES as readonly string[]).includes(head)) {
    return { kind: 'page', page: head as AppPage };
  }

  return DEFAULT_ROUTE;
}

/**
 * Format a {@link Route} back into a hash string (always with the leading `#/`). The hub formats to
 * `#/` (canonical), the pages to `#/<page>`, and the workspace to `#/event/<tab>`. Round-trips with
 * {@link parseHash} for every well-formed route.
 */
export function formatHash(route: Route): string {
  if (route.kind === 'workspace') return `#/event/${route.tab}`;
  if (route.kind === 'tune') return `#/timers/${encodeURIComponent(route.timer)}/tune`;
  if (route.page === 'home') return '#/';
  return `#/${route.page}`;
}

/**
 * Reconcile a parsed {@link Route} against whether an event is currently active (the Director's
 * active event, server state) and, for the parameterised tune route, whether its timer still
 * exists. This is the **graceful reconciliation** the IA needs:
 *
 *   - A `workspace` route with **no active event** → fall back to the Events page (`#/events`),
 *     since there is no event to show — never render a broken workspace.
 *   - A `tune` route whose timer is **not in the registry** → fall back to the Timers page. Only
 *     applied once the registry is known (see `timerKnown`).
 *   - A `page` route with an **active event** is fine as-is: the user explicitly navigated to a
 *     top-level page (e.g. via the breadcrumb) even though an event is active, so honour it. This
 *     includes the hub: an empty/hub hash stays on the hub even when an event is active — the hash
 *     is authoritative, so being outside an event survives a reload.
 */
export function reconcileRoute(
  route: Route,
  hasActiveEvent: boolean,
  timerKnown?: (timer: TimerId) => boolean
): Route {
  if (route.kind === 'workspace' && !hasActiveEvent) {
    return { kind: 'page', page: 'events' };
  }
  // A tune route names a timer that may be gone (removed since the link was made / bookmarked, or a
  // hand-edited id). Fall back to the Timers page — the surface that can explain the absence and
  // offer another timer — never a tune view over nothing. `timerKnown` is deliberately OPTIONAL and
  // its absence means "not known yet": before the registry has loaded we must NOT bounce, or every
  // deep link would flash through Timers on its way in.
  if (route.kind === 'tune' && timerKnown && !timerKnown(route.timer)) {
    return { kind: 'page', page: 'timers' };
  }
  return route;
}

/**
 * Resolve the route to show on **initial load**. The **hash is authoritative**: where you were
 * survives a reload, and the shell never auto-enters an event from a bare/hub load.
 *
 *   - Empty/hub hash → the hub, **regardless of whether an event is active**. (This intentionally
 *     supersedes the #90 "reload resumes into the active event" behaviour: being outside an event
 *     stays outside; the user only enters an event by navigating in or loading a workspace URL.)
 *   - An explicit page hash (`#/pilots` etc.) → that page (honoured even if an event is active).
 *   - A workspace hash → the workspace on that tab if an event is active, else the Events page
 *     (via {@link reconcileRoute}).
 *
 * This is just the parse composed with {@link reconcileRoute}; there is no longer a special hub →
 * workspace resume branch.
 */
export function resolveInitialRoute(
  hash: string,
  hasActiveEvent: boolean,
  timerKnown?: (timer: TimerId) => boolean
): Route {
  return reconcileRoute(parseHash(hash), hasActiveEvent, timerKnown);
}
