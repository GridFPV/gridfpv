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
    expect(parseHash('#/EVENT/Roster')).toEqual({ kind: 'workspace', tab: 'roster' });
  });

  it('falls back to the hub for an unknown hash', () => {
    expect(parseHash('#/nope')).toEqual(DEFAULT_ROUTE);
    expect(parseHash('#/foo/bar')).toEqual(DEFAULT_ROUTE);
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
    expect(formatHash({ kind: 'workspace', tab: 'roster' })).toBe('#/event/roster');
  });
});

describe('round-trip (format → parse)', () => {
  const routes: Route[] = [
    { kind: 'page', page: 'home' },
    { kind: 'page', page: 'pilots' },
    { kind: 'page', page: 'events' },
    { kind: 'page', page: 'timers' },
    ...WORKSPACE_TABS.map((tab) => ({ kind: 'workspace', tab }) as Route)
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
    expect(resolveInitialRoute('#/event/roster', true)).toEqual({
      kind: 'workspace',
      tab: 'roster'
    });
  });

  it('a workspace hash with no active event reconciles to the Events page', () => {
    expect(resolveInitialRoute('#/event/roster', false)).toEqual({
      kind: 'page',
      page: 'events'
    });
  });

  it('an explicit page hash is honoured even with an active event', () => {
    expect(resolveInitialRoute('#/pilots', true)).toEqual({ kind: 'page', page: 'pilots' });
  });
});
