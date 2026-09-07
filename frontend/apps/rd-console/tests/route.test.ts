/**
 * Unit tests for the hash routing helper (#118): the pure view ↔ hash parse/format + the
 * reconciliation rules. The browser e2e (`e2e/routing.spec.ts`) proves the wiring end-to-end; this
 * pins the parsing logic the wiring depends on.
 */
import { describe, expect, it } from 'vitest';
import {
  parseHash,
  formatHash,
  reconcileRoute,
  resolveInitialRoute,
  DEFAULT_ROUTE,
  WORKSPACE_TABS,
  type Route
} from '../src/lib/route.js';

describe('parseHash', () => {
  it('parses the hub from empty / bare-hash / slash forms', () => {
    for (const h of ['', '#', '#/', '/']) {
      expect(parseHash(h)).toEqual({ kind: 'page', page: 'home' });
    }
  });

  it('parses each top-level page', () => {
    expect(parseHash('#/pilots')).toEqual({ kind: 'page', page: 'pilots' });
    expect(parseHash('#/classes')).toEqual({ kind: 'page', page: 'classes' });
    expect(parseHash('#/events')).toEqual({ kind: 'page', page: 'events' });
    expect(parseHash('#/timers')).toEqual({ kind: 'page', page: 'timers' });
  });

  it('parses each workspace tab', () => {
    for (const tab of WORKSPACE_TABS) {
      expect(parseHash(`#/event/${tab}`)).toEqual({ kind: 'workspace', tab });
    }
  });

  it('is tolerant of a missing leading slash and a trailing slash and casing', () => {
    expect(parseHash('pilots')).toEqual({ kind: 'page', page: 'pilots' });
    expect(parseHash('#/pilots/')).toEqual({ kind: 'page', page: 'pilots' });
    expect(parseHash('#/EVENT/Classes-Roster')).toEqual({
      kind: 'workspace',
      tab: 'classes-roster'
    });
  });

  it('falls back to the hub for an unknown hash', () => {
    expect(parseHash('#/nope')).toEqual(DEFAULT_ROUTE);
    expect(parseHash('#/foo/bar')).toEqual(DEFAULT_ROUTE);
  });

  // The tune route (#355) is the scheme's first PARAMETERISED route — it carries a timer id, so
  // the RD can open the tuning view on a phone at the gate instead of walking back to the laptop.
  it('parses the parameterised tune route', () => {
    expect(parseHash('#/timers/rh-1/tune')).toEqual({ kind: 'tune', timer: 'rh-1' });
    expect(parseHash('#/timers/mock/tune/')).toEqual({ kind: 'tune', timer: 'mock' });
  });

  it('keeps the timer id verbatim while still lower-casing the keyword segments', () => {
    // Only `timers`/`tune` are keywords; the id is a wire handle and must round-trip EXACTLY, or a
    // deep link resolves to a timer that isn't in the registry.
    expect(parseHash('#/TIMERS/RH-Alpha/TUNE')).toEqual({ kind: 'tune', timer: 'RH-Alpha' });
  });

  it('decodes an escaped timer id', () => {
    expect(parseHash('#/timers/rh%2Fone/tune')).toEqual({ kind: 'tune', timer: 'rh/one' });
  });

  it('degrades a malformed tune hash to the Timers page, not the hub', () => {
    // The RD asked for something timer-shaped — land them on timers rather than the hub.
    expect(parseHash('#/timers/rh-1/nope')).toEqual({ kind: 'page', page: 'timers' });
    expect(parseHash('#/timers//tune')).toEqual({ kind: 'page', page: 'timers' });
  });

  it('still parses the bare Timers page', () => {
    expect(parseHash('#/timers')).toEqual({ kind: 'page', page: 'timers' });
  });

  // The EVENT-SCOPED tune route (#411): the same page, but the URL says which event's tune is
  // being edited — which is what lets the page name its scope and lets "back" return to the event.
  it('parses the event-scoped tune route, carrying both ids', () => {
    expect(parseHash('#/events/e1/timers/rh-1/tune')).toEqual({
      kind: 'tune',
      timer: 'rh-1',
      event: 'e1'
    });
    expect(parseHash('#/events/e1/timers/rh-1/tune/')).toEqual({
      kind: 'tune',
      timer: 'rh-1',
      event: 'e1'
    });
  });

  it('keeps BOTH ids verbatim while still lower-casing the keyword segments', () => {
    // `events`/`timers`/`tune` are keywords; the event id and the timer id are wire handles and
    // must round-trip EXACTLY, or the route resolves to entities that aren't there.
    expect(parseHash('#/EVENTS/Saturday-Race/TIMERS/RH-Alpha/TUNE')).toEqual({
      kind: 'tune',
      timer: 'RH-Alpha',
      event: 'Saturday-Race'
    });
  });

  it('decodes escaped ids on both sides of the event-scoped route', () => {
    expect(parseHash('#/events/sat%2Frace/timers/rh%20one/tune')).toEqual({
      kind: 'tune',
      timer: 'rh one',
      event: 'sat/race'
    });
  });

  it('degrades a malformed event-scoped hash to the Events page, not the hub', () => {
    // The RD asked for something event-shaped — land them on events rather than the hub.
    expect(parseHash('#/events/e1/timers/rh-1')).toEqual({ kind: 'page', page: 'events' });
    expect(parseHash('#/events/e1/timers/rh-1/nope')).toEqual({ kind: 'page', page: 'events' });
    expect(parseHash('#/events//timers/rh-1/tune')).toEqual({ kind: 'page', page: 'events' });
    expect(parseHash('#/events/e1/timers//tune')).toEqual({ kind: 'page', page: 'events' });
  });

  it('still parses the bare Events page', () => {
    expect(parseHash('#/events')).toEqual({ kind: 'page', page: 'events' });
  });

  // The `#/event/<tab>` workspace route and the `#/events/...` scoped route differ by ONE letter;
  // neither may swallow the other.
  it('does not confuse the workspace route with the event-scoped tune route', () => {
    expect(parseHash('#/event/timers')).toEqual({ kind: 'workspace', tab: 'timers' });
    expect(parseHash('#/events/e1/timers/rh-1/tune')).toEqual({
      kind: 'tune',
      timer: 'rh-1',
      event: 'e1'
    });
  });

  it('defaults a tab-less / unknown-tab workspace hash to the live tab', () => {
    expect(parseHash('#/event')).toEqual({ kind: 'workspace', tab: 'live' });
    expect(parseHash('#/event/nope')).toEqual({ kind: 'workspace', tab: 'live' });
  });
});

describe('formatHash', () => {
  it('formats the hub canonically as #/', () => {
    expect(formatHash({ kind: 'page', page: 'home' })).toBe('#/');
  });

  it('formats pages and workspace tabs', () => {
    expect(formatHash({ kind: 'page', page: 'pilots' })).toBe('#/pilots');
    expect(formatHash({ kind: 'workspace', tab: 'classes-roster' })).toBe('#/event/classes-roster');
  });

  it('formats the tune route under the Timers page, escaping the id', () => {
    expect(formatHash({ kind: 'tune', timer: 'rh-1' })).toBe('#/timers/rh-1/tune');
    expect(formatHash({ kind: 'tune', timer: 'rh/one' })).toBe('#/timers/rh%2Fone/tune');
  });

  it('formats the event-scoped tune route under its event, escaping both ids', () => {
    expect(formatHash({ kind: 'tune', timer: 'rh-1', event: 'e1' })).toBe(
      '#/events/e1/timers/rh-1/tune'
    );
    expect(formatHash({ kind: 'tune', timer: 'rh one', event: 'sat/race' })).toBe(
      '#/events/sat%2Frace/timers/rh%20one/tune'
    );
  });
});

describe('round-trip (format → parse)', () => {
  const routes: Route[] = [
    { kind: 'page', page: 'home' },
    { kind: 'page', page: 'pilots' },
    { kind: 'page', page: 'events' },
    { kind: 'page', page: 'timers' },
    ...WORKSPACE_TABS.map((tab) => ({ kind: 'workspace', tab }) as Route),
    { kind: 'tune', timer: 'rh-1' },
    { kind: 'tune', timer: 'mock' },
    // An id with characters the hash would otherwise eat, to prove the escape round-trips.
    { kind: 'tune', timer: 'rh/one two' },
    // …and the same for the event-scoped form, where BOTH ids have to survive the trip.
    { kind: 'tune', timer: 'rh-1', event: 'e1' },
    { kind: 'tune', timer: 'RH-Alpha', event: 'Saturday-Race' },
    { kind: 'tune', timer: 'rh/one two', event: 'sat/race night' }
  ];
  for (const route of routes) {
    it(`round-trips ${JSON.stringify(route)}`, () => {
      expect(parseHash(formatHash(route))).toEqual(route);
    });
  }
});

describe('reconcileRoute', () => {
  it('keeps a page route regardless of the active event', () => {
    const r: Route = { kind: 'page', page: 'pilots' };
    expect(reconcileRoute(r, true)).toEqual(r);
    expect(reconcileRoute(r, false)).toEqual(r);
  });

  it('keeps a workspace route when an event is active', () => {
    const r: Route = { kind: 'workspace', tab: 'results' };
    expect(reconcileRoute(r, true)).toEqual(r);
  });

  it('falls a workspace route back to the Events page when no event is active', () => {
    expect(reconcileRoute({ kind: 'workspace', tab: 'results' }, false)).toEqual({
      kind: 'page',
      page: 'events'
    });
  });

  // The tune route is app-level: a timer is tuned before an event exists, and the RD may open it
  // from a phone with no event entered. So the active event must not affect it either way.
  it('keeps a tune route regardless of the active event', () => {
    const r: Route = { kind: 'tune', timer: 'rh-1' };
    expect(reconcileRoute(r, true)).toEqual(r);
    expect(reconcileRoute(r, false)).toEqual(r);
  });

  it('keeps a tune route whose timer is in the registry', () => {
    const r: Route = { kind: 'tune', timer: 'rh-1' };
    expect(reconcileRoute(r, false, (id) => id === 'rh-1')).toEqual(r);
  });

  it('falls a tune route back to the Timers page when the timer is gone', () => {
    // A bookmarked link to a removed timer, or a hand-edited id: never render a tune view over
    // nothing — land on the surface that can explain the absence and offer another timer.
    expect(reconcileRoute({ kind: 'tune', timer: 'ghost' }, false, () => false)).toEqual({
      kind: 'page',
      page: 'timers'
    });
  });

  it('keeps an event-scoped tune route whose event is the one in play', () => {
    const r: Route = { kind: 'tune', timer: 'rh-1', event: 'e1' };
    expect(
      reconcileRoute(
        r,
        true,
        () => true,
        (id) => id === 'e1'
      )
    ).toEqual(r);
  });

  it('drops the SCOPE (not the tune) when the event is not the one in play', () => {
    // A stale bookmark or a link from another event: the timer is real and tuning it still works,
    // so degrade to the timer's own baseline route rather than bouncing the RD out of tuning.
    expect(
      reconcileRoute(
        { kind: 'tune', timer: 'rh-1', event: 'gone' },
        false,
        () => true,
        () => false
      )
    ).toEqual({ kind: 'tune', timer: 'rh-1' });
  });

  it('falls back to Timers when the TIMER is gone, event-scoped or not', () => {
    // A missing timer outranks the scope: there is nothing to tune either way.
    expect(
      reconcileRoute(
        { kind: 'tune', timer: 'ghost', event: 'e1' },
        true,
        () => false,
        () => true
      )
    ).toEqual({ kind: 'page', page: 'timers' });
  });

  it('does NOT bounce an event-scoped tune route while the event is still unknown', () => {
    // Same rule as the registry: `eventKnown` absent means "not resolved yet". Bouncing here would
    // flash a deep link — dropping its scope — on its way in.
    const r: Route = { kind: 'tune', timer: 'rh-1', event: 'e1' };
    expect(reconcileRoute(r, false)).toEqual(r);
    expect(reconcileRoute(r, true, () => true)).toEqual(r);
  });

  it('does NOT bounce a tune route while the registry is still unknown', () => {
    // `timerKnown` absent means "not loaded yet". Bouncing here would flash every deep link
    // through the Timers page on its way in.
    const r: Route = { kind: 'tune', timer: 'rh-1' };
    expect(reconcileRoute(r, false)).toEqual(r);
  });
});

describe('resolveInitialRoute (hash is authoritative; #118)', () => {
  // Supersedes the #90 reload-resume: a bare/hub load never auto-enters the active event. Being
  // outside an event survives a reload — staying on the hub keeps you on the hub.
  it('empty/hub hash + active event stays on the hub (does NOT resume into the workspace)', () => {
    expect(resolveInitialRoute('', true)).toEqual({ kind: 'page', page: 'home' });
    expect(resolveInitialRoute('#/', true)).toEqual({ kind: 'page', page: 'home' });
  });

  it('empty hash + no active event lands on the hub', () => {
    expect(resolveInitialRoute('', false)).toEqual({ kind: 'page', page: 'home' });
  });

  it('a workspace tab hash restores that tab when an event is active', () => {
    expect(resolveInitialRoute('#/event/classes-roster', true)).toEqual({
      kind: 'workspace',
      tab: 'classes-roster'
    });
  });

  it('a workspace hash with no active event reconciles to the Events page', () => {
    expect(resolveInitialRoute('#/event/classes-roster', false)).toEqual({
      kind: 'page',
      page: 'events'
    });
  });

  it('an explicit page hash is honoured even with an active event', () => {
    expect(resolveInitialRoute('#/pilots', true)).toEqual({ kind: 'page', page: 'pilots' });
  });

  // A reload (or a link opened on the phone at the gate) keeps the SCOPE it was opened with.
  it('an event-scoped tune hash keeps its scope when that event is the one in play', () => {
    expect(
      resolveInitialRoute('#/events/e1/timers/rh-1/tune', true, undefined, () => true)
    ).toEqual({ kind: 'tune', timer: 'rh-1', event: 'e1' });
  });

  it('an event-scoped tune hash with no event in play still tunes, on the timer scope', () => {
    // #414 removes the built-in Practice event, so "no event at all" is a real state — and an
    // untuned timer is tuned before any event exists. Tuning must survive that.
    expect(
      resolveInitialRoute('#/events/e1/timers/rh-1/tune', false, undefined, () => false)
    ).toEqual({ kind: 'tune', timer: 'rh-1' });
  });

  it('a timer-scoped tune hash is unaffected by the active event either way', () => {
    for (const active of [true, false]) {
      expect(resolveInitialRoute('#/timers/rh-1/tune', active, undefined, () => false)).toEqual({
        kind: 'tune',
        timer: 'rh-1'
      });
    }
  });
});
