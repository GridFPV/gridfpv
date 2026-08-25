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
    { kind: 'tune', timer: 'rh/one two' }
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
});
