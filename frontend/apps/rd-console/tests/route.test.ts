/**
 * Unit tests for the hash routing helper (`lib/route.ts`): the pure route ↔ hash parse and
 * format, and the reconciliation against whether an event is open. The browser e2e proves
 * the wiring end to end; this pins the parsing the wiring depends on.
 */
import { describe, expect, it } from 'vitest';
import {
  parseHash,
  formatHash,
  reconcileRoute,
  resolveInitialRoute,
  DEFAULT_ROUTE,
  OVERLAYS,
  inEvent,
  isOverlay,
  type AppPage,
  type EventSection,
  type Route
} from '../src/lib/route.js';

const EVENT_PAGE: Route = { kind: 'event', section: 'event' };
const EVENTS_PAGE: Route = { kind: 'page', page: 'events' };
/** Percent-encoding that cannot be decoded. */
const BROKEN = '%E0%A4%A';

describe('parseHash', () => {
  it("opens the event's own page from empty, bare-hash and slash forms", () => {
    expect(DEFAULT_ROUTE).toEqual(EVENT_PAGE);
    for (const h of ['', '#', '#/', '/']) {
      expect(parseHash(h)).toEqual(EVENT_PAGE);
    }
  });

  it('parses each page that needs no open event', () => {
    const pages: AppPage[] = ['events', 'pilots', 'classes', 'timers', 'formats'];
    for (const page of pages) {
      expect(parseHash(`#/${page}`)).toEqual({ kind: 'page', page });
    }
    expect(parseHash('#/Timers/')).toEqual({ kind: 'page', page: 'timers' });
  });

  it('parses each page of the open event', () => {
    expect(parseHash('#/event')).toEqual(EVENT_PAGE);
    const sections: EventSection[] = ['phases', 'results', 'overlays'];
    for (const section of sections) {
      expect(parseHash(`#/event/${section}`)).toEqual({ kind: 'event', section });
    }
    // The event's own page has one spelling when formatted, but the long one still opens it.
    expect(parseHash('#/event/event')).toEqual(EVENT_PAGE);
  });

  it('is tolerant of a missing leading slash and a trailing slash and casing', () => {
    expect(parseHash('pilots')).toEqual({ kind: 'page', page: 'pilots' });
    expect(parseHash('#/pilots/')).toEqual({ kind: 'page', page: 'pilots' });
    expect(parseHash('#/EVENT/')).toEqual(EVENT_PAGE);
    expect(parseHash('#/EVENT/Phases/')).toEqual({ kind: 'event', section: 'phases' });
    expect(parseHash('event/results')).toEqual({ kind: 'event', section: 'results' });
  });

  it('falls back to the event page for an unknown hash', () => {
    expect(parseHash('#/nope')).toEqual(DEFAULT_ROUTE);
    expect(parseHash('#/foo/bar')).toEqual(DEFAULT_ROUTE);
    expect(parseHash('#/event/other')).toEqual(DEFAULT_ROUTE);
    expect(parseHash('#/event/phases/extra')).toEqual(DEFAULT_ROUTE);
    expect(parseHash('#/pilots/extra')).toEqual(DEFAULT_ROUTE);
  });

  it('has no hub page and no per-timer page under #/timers any more', () => {
    expect(parseHash('#/home')).toEqual(DEFAULT_ROUTE);
    expect(parseHash('#/timers/gate-X1y2')).toEqual(DEFAULT_ROUTE);
  });

  // The `#/event` page and the `#/events` page differ by ONE letter; neither may swallow the
  // other.
  it('does not confuse the event page with the Events page', () => {
    expect(parseHash('#/event')).toEqual(EVENT_PAGE);
    expect(parseHash('#/events')).toEqual(EVENTS_PAGE);
  });
});

describe('formatHash', () => {
  it('formats the pages that need no open event', () => {
    expect(formatHash({ kind: 'page', page: 'events' })).toBe('#/events');
    expect(formatHash({ kind: 'page', page: 'pilots' })).toBe('#/pilots');
    expect(formatHash({ kind: 'page', page: 'classes' })).toBe('#/classes');
    expect(formatHash({ kind: 'page', page: 'timers' })).toBe('#/timers');
    expect(formatHash({ kind: 'page', page: 'formats' })).toBe('#/formats');
  });

  it("formats the event's own page as #/event and the others beneath it", () => {
    expect(formatHash(EVENT_PAGE)).toBe('#/event');
    expect(formatHash({ kind: 'event', section: 'phases' })).toBe('#/event/phases');
    expect(formatHash({ kind: 'event', section: 'results' })).toBe('#/event/results');
    expect(formatHash({ kind: 'event', section: 'overlays' })).toBe('#/event/overlays');
  });
});

describe('round-trip (format → parse)', () => {
  const routes: Route[] = [
    { kind: 'page', page: 'events' },
    { kind: 'page', page: 'pilots' },
    { kind: 'page', page: 'classes' },
    { kind: 'page', page: 'timers' },
    { kind: 'page', page: 'formats' },
    { kind: 'event', section: 'event' },
    { kind: 'event', section: 'phases' },
    { kind: 'event', section: 'results' },
    { kind: 'event', section: 'overlays' },
    { kind: 'run' },
    { kind: 'run', heat: 'quali-ab12' },
    { kind: 'run', heat: 'finals/winners-1/heat-2' },
    { kind: 'run', heat: 'Heat 1 #2 ?&%' },
    { kind: 'marshal' },
    { kind: 'marshal', race: 'finals/winners-1/heat-2#3' },
    { kind: 'seats' },
    { kind: 'seats', timer: 'gate-X1y2' },
    { kind: 'seats', timer: 'far gate/9 #?&%' },
    { kind: 'capture', timer: 'gate-X1y2', seat: '0' },
    { kind: 'capture', timer: 'far gate/9 #?&%', seat: 'Seat/3 #?&%' },
    { kind: 'help' },
    { kind: 'help', slug: 'start-here' },
    { kind: 'overlay' },
    { kind: 'overlay', overlay: 'race' },
    { kind: 'overlay', overlay: 'race', theme: 'light' }
  ];
  for (const route of routes) {
    it(`round-trips ${JSON.stringify(route)}`, () => {
      expect(parseHash(formatHash(route))).toEqual(route);
    });
  }
});

describe('the Run page route', () => {
  it('parses the page with no heat named', () => {
    expect(parseHash('#/event/run')).toEqual({ kind: 'run' });
    expect(parseHash('#/event/run/')).toEqual({ kind: 'run' });
    expect(parseHash('#/EVENT/Run')).toEqual({ kind: 'run' });
  });

  it("parses a heat's Run page, decoding the heat id and keeping its case", () => {
    expect(parseHash('#/event/run/quali-A1b2')).toEqual({ kind: 'run', heat: 'quali-A1b2' });
    // A generated heat id holds slashes; they travel as one encoded segment.
    expect(parseHash('#/event/run/finals%2Fwinners-1%2Fheat-2')).toEqual({
      kind: 'run',
      heat: 'finals/winners-1/heat-2'
    });
    expect(parseHash('#/EVENT/RUN/h1/')).toEqual({ kind: 'run', heat: 'h1' });
  });

  it('opens the page with no heat when the heat segment is broken, and the event page for a longer path', () => {
    expect(parseHash(`#/event/run/${BROKEN}`)).toEqual({ kind: 'run' });
    expect(parseHash('#/event/run/a/b')).toEqual(EVENT_PAGE);
  });

  it('formats with the heat id as one encoded segment, and without one when none is named', () => {
    expect(formatHash({ kind: 'run' })).toBe('#/event/run');
    expect(formatHash({ kind: 'run', heat: 'finals/winners-1/heat-2' })).toBe(
      '#/event/run/finals%2Fwinners-1%2Fheat-2'
    );
  });

  it('needs an open event, with or without a heat', () => {
    const run: Route = { kind: 'run', heat: 'finals/final' };
    expect(reconcileRoute(run, true)).toEqual(run);
    expect(reconcileRoute(run, false)).toEqual(EVENTS_PAGE);
    expect(reconcileRoute({ kind: 'run' }, true)).toEqual({ kind: 'run' });
    expect(reconcileRoute({ kind: 'run' }, false)).toEqual(EVENTS_PAGE);
  });

  it('a Run hash survives a reload while an event is active', () => {
    expect(resolveInitialRoute('#/event/run/finals%2Ffinal', true)).toEqual({
      kind: 'run',
      heat: 'finals/final'
    });
    expect(resolveInitialRoute('#/event/run', true)).toEqual({ kind: 'run' });
  });
});

describe('the Marshal page route', () => {
  it('parses the page with and without a race, keeping the race id as it is', () => {
    expect(parseHash('#/event/marshal')).toEqual({ kind: 'marshal' });
    expect(parseHash('#/EVENT/Marshal/')).toEqual({ kind: 'marshal' });
    expect(parseHash('#/event/marshal/quali-A1b2%231')).toEqual({
      kind: 'marshal',
      race: 'quali-A1b2#1'
    });
  });

  it('opens the page with no race when the race segment is broken, and the event page for a longer path', () => {
    expect(parseHash(`#/event/marshal/${BROKEN}`)).toEqual({ kind: 'marshal' });
    expect(parseHash('#/event/marshal/a/b')).toEqual(EVENT_PAGE);
  });

  it('formats with the race id encoded, so a "#" or "/" in it survives', () => {
    expect(formatHash({ kind: 'marshal' })).toBe('#/event/marshal');
    expect(formatHash({ kind: 'marshal', race: 'finals/final#2' })).toBe(
      '#/event/marshal/finals%2Ffinal%232'
    );
  });

  it('needs an open event', () => {
    expect(reconcileRoute({ kind: 'marshal', race: 'h#1' }, false)).toEqual(EVENTS_PAGE);
    expect(reconcileRoute({ kind: 'marshal' }, true)).toEqual({ kind: 'marshal' });
  });
});

describe('the Seats page route', () => {
  const seats: Route = { kind: 'seats', timer: 'gate-X1y2' };

  it('parses the page for the first timer when none is named', () => {
    expect(parseHash('#/seats')).toEqual({ kind: 'seats' });
    expect(parseHash('#/Seats/')).toEqual({ kind: 'seats' });
    expect(parseHash('seats')).toEqual({ kind: 'seats' });
  });

  it('parses #/seats/‹timer›, decoding the id and keeping its case', () => {
    expect(parseHash('#/seats/gate-X1y2')).toEqual(seats);
    expect(parseHash('#/SEATS/gate-X1y2/')).toEqual(seats);
    expect(parseHash('#/seats/far%20gate%2F9')).toEqual({ kind: 'seats', timer: 'far gate/9' });
  });

  it('formats with the timer id as one encoded segment', () => {
    expect(formatHash({ kind: 'seats' })).toBe('#/seats');
    expect(formatHash(seats)).toBe('#/seats/gate-X1y2');
    expect(formatHash({ kind: 'seats', timer: 'far gate/9' })).toBe('#/seats/far%20gate%2F9');
  });

  it('falls a broken timer segment back to the first timer, and a longer path to the event page', () => {
    expect(parseHash(`#/seats/${BROKEN}`)).toEqual({ kind: 'seats' });
    expect(parseHash('#/seats/a/b/c')).toEqual(DEFAULT_ROUTE);
  });

  it('needs no open event: it survives a reload with or without one', () => {
    for (const route of [seats, { kind: 'seats' }] satisfies Route[]) {
      expect(inEvent(route)).toBe(false);
      expect(reconcileRoute(route, false)).toEqual(route);
      expect(reconcileRoute(route, true)).toEqual(route);
    }
    expect(resolveInitialRoute('#/seats/gate-X1y2', false)).toEqual(seats);
    expect(resolveInitialRoute('#/seats', false)).toEqual({ kind: 'seats' });
  });
});

describe("a seat's Tune page route", () => {
  const capture: Route = { kind: 'capture', timer: 'gate-X1y2', seat: '3' };

  it('parses #/seats/‹timer›/‹seat›, decoding both ids and keeping their case', () => {
    expect(parseHash('#/seats/gate-X1y2/3')).toEqual(capture);
    expect(parseHash('#/SEATS/gate-X1y2/3/')).toEqual(capture);
    expect(parseHash('#/seats/far%20gate%2F9/Seat%2FA')).toEqual({
      kind: 'capture',
      timer: 'far gate/9',
      seat: 'Seat/A'
    });
  });

  it('formats with each id as its own encoded segment', () => {
    expect(formatHash(capture)).toBe('#/seats/gate-X1y2/3');
    expect(formatHash({ kind: 'capture', timer: 'far gate/9', seat: 'Seat/A #1' })).toBe(
      '#/seats/far%20gate%2F9/Seat%2FA%20%231'
    );
  });

  it('falls a broken segment back one level', () => {
    // A broken seat still knows its timer: the Seats page for that timer.
    expect(parseHash(`#/seats/gate-X1y2/${BROKEN}`)).toEqual({
      kind: 'seats',
      timer: 'gate-X1y2'
    });
    // A broken timer names nothing, so its seat means nothing either.
    expect(parseHash(`#/seats/${BROKEN}/3`)).toEqual({ kind: 'seats' });
  });

  it('needs no open event: it survives a reload with or without one', () => {
    expect(inEvent(capture)).toBe(false);
    expect(reconcileRoute(capture, false)).toEqual(capture);
    expect(reconcileRoute(capture, true)).toEqual(capture);
    expect(resolveInitialRoute('#/seats/gate-X1y2/3', false)).toEqual(capture);
  });
});

describe('the help routes', () => {
  it('parses the index and one page', () => {
    expect(parseHash('#/help')).toEqual({ kind: 'help' });
    expect(parseHash('#/Help/')).toEqual({ kind: 'help' });
    expect(parseHash('#/help/start-here')).toEqual({ kind: 'help', slug: 'start-here' });
  });

  it('reads a page name in any letter case as its lower-case name', () => {
    expect(parseHash('#/HELP/Start-Here/')).toEqual({ kind: 'help', slug: 'start-here' });
  });

  it('opens the index for a broken page name, and the event page for a longer path', () => {
    expect(parseHash(`#/help/${BROKEN}`)).toEqual({ kind: 'help' });
    expect(parseHash('#/help/run/extra')).toEqual(DEFAULT_ROUTE);
  });

  it('formats the index and one page', () => {
    expect(formatHash({ kind: 'help' })).toBe('#/help');
    expect(formatHash({ kind: 'help', slug: 'start-here' })).toBe('#/help/start-here');
  });

  it('needs no open event', () => {
    const page: Route = { kind: 'help', slug: 'run' };
    expect(inEvent(page)).toBe(false);
    expect(reconcileRoute(page, false)).toEqual(page);
    expect(resolveInitialRoute('#/help', false)).toEqual({ kind: 'help' });
  });
});

describe('inEvent', () => {
  it('is true for the event pages, the Run page and the Marshal page, and nothing else', () => {
    const inside: Route[] = [
      { kind: 'event', section: 'event' },
      { kind: 'event', section: 'phases' },
      { kind: 'event', section: 'results' },
      { kind: 'event', section: 'overlays' },
      { kind: 'run' },
      { kind: 'run', heat: 'h1' },
      { kind: 'marshal' },
      { kind: 'marshal', race: 'h1#1' }
    ];
    const outside: Route[] = [
      { kind: 'page', page: 'events' },
      { kind: 'page', page: 'timers' },
      { kind: 'page', page: 'formats' },
      { kind: 'seats' },
      { kind: 'capture', timer: 't1', seat: '0' },
      { kind: 'help' },
      { kind: 'overlay', overlay: 'race' }
    ];
    for (const route of inside) expect(inEvent(route)).toBe(true);
    for (const route of outside) expect(inEvent(route)).toBe(false);
  });
});

describe('reconcileRoute', () => {
  it('keeps a page route regardless of the active event', () => {
    for (const page of ['events', 'pilots', 'classes', 'timers', 'formats'] satisfies AppPage[]) {
      const r: Route = { kind: 'page', page };
      expect(reconcileRoute(r, true)).toEqual(r);
      expect(reconcileRoute(r, false)).toEqual(r);
    }
  });

  it('keeps every event page when an event is active', () => {
    for (const section of ['event', 'phases', 'results', 'overlays'] satisfies EventSection[]) {
      const r: Route = { kind: 'event', section };
      expect(reconcileRoute(r, true)).toEqual(r);
    }
  });

  it('falls every event page back to the Events page when no event is active', () => {
    for (const section of ['event', 'phases', 'results', 'overlays'] satisfies EventSection[]) {
      expect(reconcileRoute({ kind: 'event', section }, false)).toEqual(EVENTS_PAGE);
    }
  });
});

describe('resolveInitialRoute (the hash is authoritative)', () => {
  it("an empty hash opens the event's page when an event is active", () => {
    expect(resolveInitialRoute('', true)).toEqual(EVENT_PAGE);
    expect(resolveInitialRoute('#/', true)).toEqual(EVENT_PAGE);
  });

  it('an empty hash with no active event lands on the Events page', () => {
    expect(resolveInitialRoute('', false)).toEqual(EVENTS_PAGE);
    expect(resolveInitialRoute('#/', false)).toEqual(EVENTS_PAGE);
  });

  it('an unknown hash goes where an empty one does', () => {
    expect(resolveInitialRoute('#/nope', true)).toEqual(EVENT_PAGE);
    expect(resolveInitialRoute('#/nope', false)).toEqual(EVENTS_PAGE);
  });

  it('an event hash restores its page when an event is active', () => {
    expect(resolveInitialRoute('#/event', true)).toEqual(EVENT_PAGE);
    expect(resolveInitialRoute('#/event/phases', true)).toEqual({
      kind: 'event',
      section: 'phases'
    });
    expect(resolveInitialRoute('#/event/marshal/q-1%231', true)).toEqual({
      kind: 'marshal',
      race: 'q-1#1'
    });
  });

  it('an event hash with no active event reconciles to the Events page', () => {
    for (const hash of [
      '#/event',
      '#/event/phases',
      '#/event/results',
      '#/event/overlays',
      '#/event/run',
      '#/event/run/h1',
      '#/event/marshal',
      '#/event/marshal/h1%231'
    ]) {
      expect(resolveInitialRoute(hash, false)).toEqual(EVENTS_PAGE);
    }
  });

  it('an explicit page hash is honoured with or without an active event', () => {
    expect(resolveInitialRoute('#/pilots', true)).toEqual({ kind: 'page', page: 'pilots' });
    expect(resolveInitialRoute('#/formats', false)).toEqual({ kind: 'page', page: 'formats' });
  });
});

describe('overlay routes', () => {
  const race: Route = { kind: 'overlay', overlay: 'race' };
  const light: Route = { kind: 'overlay', overlay: 'race', theme: 'light' };
  const nothing: Route = { kind: 'overlay' };

  it('has the race overlay', () => {
    expect(OVERLAYS).toEqual(['race']);
  });

  it('parses the race overlay, with light text by default', () => {
    expect(parseHash('#/overlay/race')).toEqual(race);
    expect(parseHash('#/Overlay/RACE/')).toEqual(race);
    expect(parseHash('overlay/race')).toEqual(race);
  });

  it('parses the light panel, and takes any other third part as the default', () => {
    expect(parseHash('#/overlay/race/light')).toEqual(light);
    expect(parseHash('#/overlay/race/LIGHT/')).toEqual(light);
    expect(parseHash('#/overlay/race/dark')).toEqual(race);
    expect(parseHash('#/overlay/race/lihgt')).toEqual(race);
  });

  it('an unknown overlay name, or none, is an overlay that draws nothing, never a console page', () => {
    expect(parseHash('#/overlay/nope')).toEqual(nothing);
    expect(parseHash('#/overlay/nope/light')).toEqual(nothing);
    expect(parseHash('#/overlay')).toEqual(nothing);
    expect(parseHash('#/overlay/')).toEqual(nothing);
    expect(parseHash('#/overlay/nope')).not.toEqual(DEFAULT_ROUTE);
  });

  it('formats and round-trips', () => {
    expect(formatHash(race)).toBe('#/overlay/race');
    expect(formatHash(light)).toBe('#/overlay/race/light');
    expect(formatHash(nothing)).toBe('#/overlay');
    for (const route of [race, light, nothing]) {
      expect(parseHash(formatHash(route))).toEqual(route);
    }
  });

  it('is bare, and no other route is', () => {
    expect(isOverlay(race)).toBe(true);
    expect(isOverlay(light)).toBe(true);
    expect(isOverlay(nothing)).toBe(true);
    for (const route of [
      DEFAULT_ROUTE,
      { kind: 'page', page: 'events' },
      { kind: 'event', section: 'overlays' },
      { kind: 'run', heat: 'h1' },
      { kind: 'marshal' },
      { kind: 'seats', timer: 't1' },
      { kind: 'capture', timer: 't1', seat: '0' },
      { kind: 'help' }
    ] satisfies Route[]) {
      expect(isOverlay(route)).toBe(false);
    }
  });

  it('needs no open event and is never sent elsewhere', () => {
    expect(inEvent(race)).toBe(false);
    for (const route of [race, light, nothing]) {
      expect(reconcileRoute(route, false)).toEqual(route);
      expect(reconcileRoute(route, true)).toEqual(route);
    }
    expect(resolveInitialRoute('#/overlay/race/light', false)).toEqual(light);
    expect(resolveInitialRoute('#/overlay/nope', true)).toEqual(nothing);
  });
});
