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
  });

  it('parses the open event page', () => {
    expect(parseHash('#/event')).toEqual({ kind: 'event' });
  });

  it('is tolerant of a missing leading slash and a trailing slash and casing', () => {
    expect(parseHash('pilots')).toEqual({ kind: 'page', page: 'pilots' });
    expect(parseHash('#/pilots/')).toEqual({ kind: 'page', page: 'pilots' });
    expect(parseHash('#/EVENT/')).toEqual({ kind: 'event' });
  });

  it('falls back to the hub for an unknown hash', () => {
    expect(parseHash('#/nope')).toEqual(DEFAULT_ROUTE);
    expect(parseHash('#/foo/bar')).toEqual(DEFAULT_ROUTE);
  });

  it("parses a heat's Run view, decoding the heat id and keeping its case", () => {
    expect(parseHash('#/event/run/quali-A1b2')).toEqual({ kind: 'run', heat: 'quali-A1b2' });
    // A generated heat id holds slashes; they travel as one encoded segment.
    expect(parseHash('#/event/run/finals%2Fwinners-1%2Fheat-2')).toEqual({
      kind: 'run',
      heat: 'finals/winners-1/heat-2'
    });
    expect(parseHash('#/EVENT/RUN/h1/')).toEqual({ kind: 'run', heat: 'h1' });
  });

  it('falls a Run hash with no heat, a broken one, or extra parts back to the event page', () => {
    expect(parseHash('#/event/run')).toEqual({ kind: 'event' });
    expect(parseHash('#/event/run/')).toEqual({ kind: 'event' });
    expect(parseHash('#/event/run/%E0%A4%A')).toEqual({ kind: 'event' });
    expect(parseHash('#/event/run/a/b')).toEqual({ kind: 'event' });
    expect(parseHash('#/event/other')).toEqual({ kind: 'event' });
  });

  // The `#/event` page and the `#/events` page differ by ONE letter; neither may swallow the
  // other.
  it('does not confuse the event page with the Events page', () => {
    expect(parseHash('#/event')).toEqual({ kind: 'event' });
    expect(parseHash('#/events')).toEqual({ kind: 'page', page: 'events' });
  });
});

describe('formatHash', () => {
  it('formats the hub canonically as #/', () => {
    expect(formatHash({ kind: 'page', page: 'home' })).toBe('#/');
  });

  it('formats pages and the event page', () => {
    expect(formatHash({ kind: 'page', page: 'pilots' })).toBe('#/pilots');
    expect(formatHash({ kind: 'event' })).toBe('#/event');
  });

  it("formats a heat's Run view with the heat id as one encoded segment", () => {
    expect(formatHash({ kind: 'run', heat: 'finals/winners-1/heat-2' })).toBe(
      '#/event/run/finals%2Fwinners-1%2Fheat-2'
    );
  });
});

describe('round-trip (format → parse)', () => {
  const routes: Route[] = [
    { kind: 'page', page: 'home' },
    { kind: 'page', page: 'pilots' },
    { kind: 'page', page: 'classes' },
    { kind: 'page', page: 'events' },
    { kind: 'event' },
    { kind: 'run', heat: 'quali-ab12' },
    { kind: 'run', heat: 'finals/winners-1/heat-2' },
    { kind: 'run', heat: 'Heat 1 #2 ?&%' }
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

  it('keeps the event route when an event is active', () => {
    const r: Route = { kind: 'event' };
    expect(reconcileRoute(r, true)).toEqual(r);
  });

  it('falls the event route back to the Events page when no event is active', () => {
    expect(reconcileRoute({ kind: 'event' }, false)).toEqual({ kind: 'page', page: 'events' });
  });
});

describe('reconcileRoute for a Run view', () => {
  const run: Route = { kind: 'run', heat: 'finals/final' };

  it('keeps the Run view when an event is active', () => {
    expect(reconcileRoute(run, true)).toEqual(run);
  });

  it('falls back to the Events page when no event is active', () => {
    expect(reconcileRoute(run, false)).toEqual({ kind: 'page', page: 'events' });
  });

  it('a Run hash survives a reload while an event is active', () => {
    expect(resolveInitialRoute('#/event/run/finals%2Ffinal', true)).toEqual(run);
  });
});

describe('resolveInitialRoute (hash is authoritative; #118)', () => {
  // A bare/hub load never opens the active event. Being outside an event survives a reload —
  // staying on the hub keeps you on the hub.
  it('empty/hub hash + active event stays on the hub (does NOT open the event)', () => {
    expect(resolveInitialRoute('', true)).toEqual({ kind: 'page', page: 'home' });
    expect(resolveInitialRoute('#/', true)).toEqual({ kind: 'page', page: 'home' });
  });

  it('empty hash + no active event lands on the hub', () => {
    expect(resolveInitialRoute('', false)).toEqual({ kind: 'page', page: 'home' });
  });

  it('an event hash restores the event page when an event is active', () => {
    expect(resolveInitialRoute('#/event', true)).toEqual({ kind: 'event' });
  });

  it('an event hash with no active event reconciles to the Events page', () => {
    expect(resolveInitialRoute('#/event', false)).toEqual({ kind: 'page', page: 'events' });
  });

  it('an explicit page hash is honoured even with an active event', () => {
    expect(resolveInitialRoute('#/pilots', true)).toEqual({ kind: 'page', page: 'pilots' });
  });
});
