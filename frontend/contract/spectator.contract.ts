/**
 * The open event as a visitor may read it (docs/primitives.md §11.2), pinned against the
 * real Director.
 *
 * The spectator's page reads the console's own shapes from `/public/event…`. These tests
 * pin that the shapes really are the same ones, that the director's things are not in
 * them, and what is answered when no event is open.
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import type {
  Class,
  EventView,
  KioskPassView,
  Pilot,
  ProtocolError,
  PublicEvent,
  PublicHello,
  TimerView
} from '@gridfpv/types';

import { type Director } from '../test-harness/director.ts';
import { startContractDirector } from './harness.ts';

let director: Director;
let event: string;

const auth = (): Record<string, string> => ({
  Authorization: `Bearer ${director.token}`,
  'Content-Type': 'application/json'
});
/** A visitor: this computer, but as a proxy would pass a phone's request on. */
const visitor = { 'X-Forwarded-For': '192.168.1.77' };

async function direct<T = unknown>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(`${director.baseUrl}${path}`, {
    method,
    headers: auth(),
    body: body === undefined ? undefined : JSON.stringify(body)
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`${method} ${path} answered ${res.status}: ${text}`);
  return (text ? JSON.parse(text) : undefined) as T;
}

async function visit(path: string): Promise<Response> {
  return fetch(`${director.baseUrl}${path}`, { headers: visitor });
}

beforeAll(async () => {
  director = await startContractDirector();
});

afterAll(async () => {
  await director?.stop();
});

describe('with no event open', () => {
  it('GET /public/event is an empty object', async () => {
    const res = await visit('/public/event');
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({});
  });

  it('every other read is a typed 404 that says so', async () => {
    for (const path of ['state', 'changes', 'pilots', 'classes', 'timers', 'channels']) {
      const res = await visit(`/public/event/${path}`);
      expect(res.status, path).toBe(404);
      expect(await res.json()).toEqual({ code: 'UnknownScope', message: 'No event is open.' });
    }
  });
});

describe('with an event open', () => {
  beforeAll(async () => {
    const ace = (
      await direct<Pilot>('POST', '/pilots', {
        callsign: 'Ace',
        name: 'Private Name',
        color: '#FF3344'
      })
    ).id;
    await direct('POST', '/pilots', { callsign: 'Not In The Event' });
    const open = (await direct<Class>('POST', '/classes', { name: 'Open', source: 'Custom' })).id;
    event = (await direct<{ id: string }>('POST', '/events', { name: 'Contract Night' })).id;
    await direct('PUT', '/active-event', { id: event });
    const commands = `/events/${event}/commands`;
    await direct('POST', commands, { Record: { RosterSet: { class: open, pilots: [ace] } } });
    await direct('POST', commands, {
      Record: {
        PhaseDefined: {
          def: {
            id: 'heat-1',
            label: 'Heat 1',
            kind: {
              Heat: { lineup: [{ adapter: 'bench', seat: 'seat-1', source: { Pilot: ace } }] }
            },
            format: {
              name: 'Open',
              race: { end: 'Open', staging_micros: 0, first_lap: 'HoleShot' },
              completion: 'DirectorDecides'
            }
          }
        }
      }
    });
    await direct('POST', commands, { StartRace: { heat: 'heat-1' } });
    await new Promise((r) => setTimeout(r, 100));
    await direct('POST', commands, { StopRace: { race: 'heat-1#1' } });
    await direct('POST', commands, {
      SaveRace: { race: 'heat-1#1', needs_marshal: true, note: 'private note' }
    });
  });

  it('GET /public/event names it, with exactly an id and a name', async () => {
    const found = (await (await visit('/public/event')).json()) as PublicEvent;
    expect(Object.keys(found)).toEqual(['event']);
    expect(Object.keys(found.event ?? {}).sort()).toEqual(['id', 'name']);
    expect(found.event).toEqual({ id: event, name: 'Contract Night' });
  });

  it("GET /public/event/state is the console's snapshot shape, minus the director's things", async () => {
    const theirs = (await (await visit('/public/event/state')).json()) as EventView;
    const ours = await direct<EventView>('GET', `/events/${event}/state`);
    // The same top-level fields, so one page draws both.
    expect(Object.keys(theirs).sort()).toEqual(Object.keys(ours).sort());
    expect(theirs.cursor).toBe(ours.cursor);
    expect(theirs.phases).toEqual(ours.phases);
    expect(theirs.rosters).toEqual(ours.rosters);
    expect(theirs.queue).toEqual(ours.queue);
    // The director's snapshot has the note and the to-do flag; the visitor's does not.
    expect(ours.races[0].note).toBe('private note');
    expect(ours.races[0].needs_marshal).toBe(true);
    expect(theirs.races[0].note).toBeUndefined();
    expect(theirs.races[0].needs_marshal).toBe(false);
    expect(theirs.races[0].video).toBeUndefined();
    expect(theirs.protests).toEqual([]);
    expect(JSON.stringify(theirs)).not.toContain('private note');
    // Everything else about the race is the same.
    const strip = (race: EventView['races'][number]) => {
      const { note: _note, needs_marshal: _flag, video: _video, ...rest } = race;
      return {
        ...rest,
        seats: rest.seats.map(({ uncounted: _uncounted, ...seat }) => seat)
      };
    };
    expect(theirs.races.map(strip)).toEqual(ours.races.map(strip));
  });

  it('GET /public/event/pilots is the pilots of the event: a handle, a callsign and a colour', async () => {
    const pilots = (await (await visit('/public/event/pilots')).json()) as Pilot[];
    expect(pilots).toHaveLength(1);
    expect(Object.keys(pilots[0]).sort()).toEqual(['callsign', 'color', 'id', 'vtx_types']);
    expect(pilots[0].callsign).toBe('Ace');
    expect(pilots[0].color).toBe('#FF3344');
    expect(JSON.stringify(pilots)).not.toContain('Private Name');
    expect(JSON.stringify(pilots)).not.toContain('Not In The Event');
  });

  it('GET /public/event/classes is the classes of the event, by name', async () => {
    const classes = (await (await visit('/public/event/classes')).json()) as Class[];
    expect(classes.map((klass) => klass.name)).toEqual(['Open']);
  });

  it('GET /public/event/timers is empty for an event flown on the bench', async () => {
    const timers = (await (await visit('/public/event/timers')).json()) as TimerView[];
    expect(timers).toEqual([]);
  });

  it('GET /public/event/channels is an empty list for an event flown on the bench', async () => {
    const res = await visit('/public/event/channels');
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual([]);
  });

  it('GET /public/event/changes is a server-sent feed of the log cursor', async () => {
    const res = await visit('/public/event/changes');
    expect(res.status).toBe(200);
    expect(res.headers.get('content-type')).toContain('text/event-stream');
    const reader = res.body!.getReader();
    const { value } = await reader.read();
    await reader.cancel();
    const text = new TextDecoder().decode(value);
    expect(text).toMatch(/^event: cursor\ndata: \d+\n/);
  });

  it('a visitor still cannot read the same event by the console route', async () => {
    const res = await visit(`/events/${event}/state`);
    expect(res.status).toBe(401);
    expect(((await res.json()) as ProtocolError).code).toBe('Unauthorized');
  });
});

describe('pilot sign-up over the wire (docs/primitives.md §11.5)', () => {
  it('a Signup command turns it on, and both snapshots carry the same sheet', async () => {
    const group = {
      Record: {
        PhaseDefined: {
          def: {
            id: 'quali',
            label: 'Qualifying',
            kind: { Group: { combine: 'Stack' } },
            format: {
              name: 'One race each',
              race: { end: 'Open', staging_micros: 0, first_lap: 'HoleShot' },
              completion: { SavedRaces: 1 }
            }
          }
        }
      }
    };
    await direct('POST', `/events/${event}/commands`, group);
    const rules = {
      seats: [
        { adapter: 'bench', seat: 'seat-1' },
        { adapter: 'bench', seat: 'seat-2' }
      ],
      open_heats: 2,
      run_limit: 3,
      rest_heats: 0,
      open: true
    };
    await direct('POST', `/events/${event}/commands`, {
      Signup: { Set: { phase: 'quali', rules } }
    });
    const ours = await direct<EventView>('GET', `/events/${event}/state`);
    const phase = ours.phases.find((entry) => entry.def.id === 'quali');
    const signup = phase?.signup;
    expect(signup?.rules).toEqual(rules);
    expect(Object.keys(signup ?? {}).sort()).toEqual(['heats', 'pilots', 'rules']);
    expect(signup?.heats).toEqual([
      { heat: 'quali/heat-1', number: 1, seats: [null, null], state: 'Open', full: false },
      { heat: 'quali/heat-2', number: 2, seats: [null, null], state: 'Open', full: false }
    ]);
    for (const pilot of signup?.pilots ?? []) {
      expect(Object.keys(pilot).sort()).toEqual(['allowed', 'extra', 'held', 'pilot', 'used']);
    }
    const theirs = (await (await visit('/public/event/state')).json()) as EventView;
    expect(theirs.phases.find((entry) => entry.def.id === 'quali')?.signup).toEqual(signup);
  });

  it("the pilot's routes need a sign-in, and say so", async () => {
    for (const [route, body] of [
      ['take', { heat: 'quali/heat-1', seat: { adapter: 'bench', seat: 'seat-1' } }],
      ['next', { phase: 'quali', seat: { adapter: 'bench', seat: 'seat-1' } }],
      ['leave', { heat: 'quali/heat-1' }]
    ] as const) {
      const res = await fetch(`${director.baseUrl}/public/pilot/signup/${route}`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body)
      });
      expect(res.status, route).toBe(401);
      expect(await res.json()).toEqual({
        code: 'Unauthorized',
        message: 'Sign in with your code first.'
      });
    }
  });

  it('a refused sign-up command is a typed 409 with the reason', async () => {
    const res = await fetch(`${director.baseUrl}/events/${event}/commands`, {
      method: 'POST',
      headers: auth(),
      body: JSON.stringify({
        Signup: { Set: { phase: 'quali', rules: { seats: [], open_heats: 1 } } }
      })
    });
    expect(res.status).toBe(409);
    expect(await res.json()).toEqual({
      code: 'Refused',
      message: 'a sign-up phase needs at least one seat'
    });
  });
});

describe('the kiosk over the wire (docs/primitives.md §11.4)', () => {
  const kiosk = (code: unknown) =>
    fetch(`${director.baseUrl}/public/pilot/kiosk`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ code })
    });

  it('a wrong code is a typed 401, and nothing but the reason comes back', async () => {
    const res = await kiosk('ZZZZZZ');
    expect(res.status).toBe(401);
    expect(res.headers.get('set-cookie')).toBeNull();
    expect(await res.json()).toEqual({
      code: 'Unauthorized',
      message: 'That code is not right. Check it with the director.'
    });
  });

  it('a right code answers the pilot, a pass and how long it lasts, and sets no cookie', async () => {
    const access = await direct<{ pilots: { pilot: string; code: string }[] }>(
      'GET',
      `/events/${event}/access`
    );
    const entry = access.pilots[0];
    expect(entry, 'the event has a pilot with a code').toBeDefined();
    const res = await kiosk(entry.code);
    expect(res.status).toBe(200);
    expect(res.headers.get('set-cookie')).toBeNull();
    expect(res.headers.get('cache-control')).toBe('no-store');
    const body = (await res.json()) as KioskPassView;
    expect(Object.keys(body).sort()).toEqual(['idle_seconds', 'pass', 'pilot']);
    expect(Object.keys(body.pilot).sort()).toEqual(['callsign', 'id']);
    expect(body.pilot.id).toBe(entry.pilot);
    expect(body.idle_seconds).toBe(120);
    expect(typeof body.pass).toBe('string');

    // With the pass in its header the Director knows the pilot; without it, nobody.
    const hello = async (headers: Record<string, string>) =>
      ((await (await fetch(`${director.baseUrl}/public/hello`, { headers })).json()) as PublicHello)
        .pilot;
    expect(await hello({ 'X-GridFPV-Kiosk': body.pass })).toEqual(body.pilot);
    expect(await hello({})).toBeUndefined();
    expect(await hello({ 'X-GridFPV-Kiosk': `${body.pass}x` })).toBeUndefined();

    // Done ends it: 204, no body, and the pass is no use after.
    const done = await fetch(`${director.baseUrl}/public/pilot/kiosk`, {
      method: 'DELETE',
      headers: { 'X-GridFPV-Kiosk': body.pass }
    });
    expect(done.status).toBe(204);
    expect(await done.text()).toBe('');
    expect(await hello({ 'X-GridFPV-Kiosk': body.pass })).toBeUndefined();
  });
});
