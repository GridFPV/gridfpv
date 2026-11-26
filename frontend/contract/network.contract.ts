/**
 * Other devices on the network: the contract a phone, a tablet and the console rely on
 * (docs/primitives.md §11.10), pinned against the real Director over real sockets.
 *
 * - the Grid computer is the director, and reads and writes as before;
 * - there are two doors, each off until `PUT /network/{door}` switches it on, each on its
 *   own port;
 * - a device at the **Public** door is a visitor: it gets `/public/hello` and the public
 *   pages, and is refused everything else with one typed error, whatever it sends;
 * - a device at the **Remote RD** door has the whole Director with the access token, and
 *   nothing but the sign-in without it. A browser sign-in carries a cookie from then on.
 */
import { createServer } from 'node:net';
import { networkInterfaces } from 'node:os';

import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import type { DoorView, NetworkView, ProtocolError, PublicHello } from '@gridfpv/types';

import { type Director } from '../test-harness/director.ts';
import { startContractDirector } from './harness.ts';

let director: Director;
/** The ports the two doors are put on. */
let port: number;
let remotePort: number;
/** The access token of the Remote RD door, read on the Grid computer. */
let access = '';
/** This computer's address on its network, when it has one. */
const lan = Object.values(networkInterfaces())
  .flat()
  .find(
    (entry) =>
      entry && entry.family === 'IPv4' && !entry.internal && !entry.address.startsWith('169.254.')
  )?.address;
const onNetwork = lan !== undefined;
/** The Public door, and the Remote RD door, as another device reaches them. */
const there = (): string => `http://${lan}:${port}`;
const remote = (): string => `http://${lan}:${remotePort}`;
const bearer = (): Record<string, string> => ({ Authorization: `Bearer ${access}` });

function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const srv = createServer();
    srv.on('error', reject);
    srv.listen(0, '0.0.0.0', () => {
      const address = srv.address();
      const found = typeof address === 'object' && address ? address.port : 0;
      srv.close(() => resolve(found));
    });
  });
}

const auth = (): Record<string, string> => ({ Authorization: `Bearer ${director.token}` });

async function network(): Promise<NetworkView> {
  return (await (await fetch(`${director.baseUrl}/network`)).json()) as NetworkView;
}

type DoorName = 'public' | 'remote';

async function setDoor(door: DoorName, body: unknown, headers: Record<string, string> = auth()) {
  return fetch(`${director.baseUrl}/network/${door}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json', ...headers },
    body: JSON.stringify(body)
  });
}

async function until(door: DoorName, state: DoorView['state']): Promise<DoorView> {
  for (let n = 0; n < 200; n += 1) {
    const view = (await network())[door];
    if (view.state === state) return view;
    await new Promise((r) => setTimeout(r, 50));
  }
  throw new Error(`${door} never reached ${state}: ${JSON.stringify(await network())}`);
}

beforeAll(async () => {
  director = await startContractDirector();
  port = await freePort();
  remotePort = await freePort();
});

afterAll(async () => {
  await director?.stop();
});

describe('GET /public/hello', () => {
  it('tells the Grid computer it is local, with exactly the documented fields', async () => {
    const res = await fetch(`${director.baseUrl}/public/hello`);
    expect(res.status).toBe(200);
    const hello = (await res.json()) as PublicHello;
    // No event is open on a fresh Director, so `event` is left out, not null.
    expect(Object.keys(hello).sort()).toEqual([
      'caller',
      'director_sign_in',
      'kiosk',
      'server',
      'version'
    ]);
    // A Director is never a remote server.
    expect(hello.server).toBe(false);
    expect(hello.caller).toBe('Local');
    expect(hello.director_sign_in).toBe(true);
    expect(typeof hello.version).toBe('string');
    expect(hello.version.length).toBeGreaterThan(0);
  });

  it('names the open event once one is open', async () => {
    const made = await fetch(`${director.baseUrl}/events`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', ...auth() },
      body: JSON.stringify({ name: 'Contract Night' })
    });
    expect(made.ok).toBe(true);
    const { id } = (await made.json()) as { id: string };
    const opened = await fetch(`${director.baseUrl}/active-event`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json', ...auth() },
      body: JSON.stringify({ id })
    });
    expect(opened.ok).toBe(true);
    const hello = (await (await fetch(`${director.baseUrl}/public/hello`)).json()) as PublicHello;
    expect(hello.event).toBe('Contract Night');
  });

  it('a request passed on by a proxy is a visitor, even from this computer', async () => {
    const res = await fetch(`${director.baseUrl}/public/hello`, {
      headers: { 'X-Forwarded-For': '127.0.0.1' }
    });
    expect(((await res.json()) as PublicHello).caller).toBe('Visitor');
    const refused = await fetch(`${director.baseUrl}/events`, {
      headers: { 'X-Forwarded-For': '127.0.0.1' }
    });
    expect(refused.status).toBe(401);
  });
});

const DOOR_FIELDS = ['addresses', 'enabled', 'port', 'port_picked', 'state'];

describe('GET /network and PUT /network/{door}', () => {
  it('starts with both doors off and no token, with exactly the documented fields', async () => {
    const view = await network();
    expect(Object.keys(view).sort()).toEqual(['public', 'remote']);
    expect(Object.keys(view.public).sort()).toEqual(DOOR_FIELDS);
    expect(Object.keys(view.remote).sort()).toEqual(DOOR_FIELDS);
    expect(view.public).toEqual({
      enabled: false,
      port: 80,
      port_picked: false,
      state: 'Off',
      addresses: []
    });
    expect(view.remote).toEqual({
      enabled: false,
      port: 8082,
      port_picked: false,
      state: 'Off',
      addresses: []
    });
  });

  it('needs the director to change a door or the token', async () => {
    for (const door of ['public', 'remote'] as const) {
      const res = await setDoor(door, { enabled: true, port }, {});
      expect(res.status, door).toBe(401);
      expect(((await res.json()) as ProtocolError).code).toBe('Unauthorized');
      expect((await network())[door].enabled).toBe(false);
    }
    const token = await fetch(`${director.baseUrl}/network/token`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: '{}'
    });
    expect(token.status).toBe(401);
  });

  it("refuses the Director's own port, and a door that is not one, with a typed error", async () => {
    const own = Number(new URL(director.baseUrl).port);
    for (const door of ['public', 'remote'] as const) {
      const res = await setDoor(door, { enabled: true, port: own });
      expect(res.status, door).toBe(400);
      expect(((await res.json()) as ProtocolError).code).toBe('BadRequest');
    }
    const third = await fetch(`${director.baseUrl}/network/private`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json', ...auth() },
      body: JSON.stringify({ enabled: true })
    });
    expect(third.status).toBe(404);
    const view = await network();
    expect(view.public.enabled).toBe(false);
    expect(view.remote.enabled).toBe(false);
  });

  it('switches Public on, on the port picked, and leaves Remote RD off', async () => {
    const res = await setDoor('public', { enabled: true, port });
    expect(res.status).toBe(200);
    const view = await until('public', 'On');
    expect(view.port).toBe(port);
    expect(view.port_picked).toBe(true);
    expect(view.enabled).toBe(true);
    if (onNetwork) expect(view.addresses).toContain(`http://${lan}:${port}/`);
    for (const address of view.addresses) expect(address).toMatch(/^http:\/\/[\d.]+:\d+\/$/);
    const whole = await network();
    expect(whole.remote.state).toBe('Off');
    expect('token' in whole).toBe(false);
  });

  it('refuses to put Remote RD on the port Public has', async () => {
    const res = await setDoor('remote', { enabled: true, port });
    expect(res.status).toBe(400);
    expect(((await res.json()) as ProtocolError).message).toContain('Public');
  });

  it('switches Remote RD on, which makes the access token', async () => {
    const res = await setDoor('remote', { enabled: true, port: remotePort });
    expect(res.status).toBe(200);
    const told = (await res.json()) as NetworkView;
    expect(told.token).toMatch(/^[A-Z2-9]{4}-[A-Z2-9]{4}-[A-Z2-9]{4}$/);
    access = told.token ?? '';
    const view = await until('remote', 'On');
    expect(view.port).toBe(remotePort);
    // Read again on the Grid computer, it is the same token.
    expect((await network()).token).toBe(access);
  });
});

describe.skipIf(!onNetwork)('a device at the Public door', () => {
  it('is a visitor, and is told which door it is at', async () => {
    const hello = (await (await fetch(`${there()}/public/hello`)).json()) as PublicHello;
    expect(hello.caller).toBe('Visitor');
    expect(hello.door).toBe('Public');
    expect(hello.director_sign_in).toBe(false);
    expect(hello.event).toBe('Contract Night');
  });

  it('is refused every read and write of the console, with one typed error, whatever it holds', async () => {
    const tries: [string, string, unknown?][] = [
      ['GET', '/events'],
      ['GET', '/active-event'],
      ['GET', '/pilots'],
      ['GET', '/classes'],
      ['GET', '/timers'],
      ['GET', '/formats'],
      ['GET', '/network'],
      ['GET', '/video/settings'],
      ['GET', '/video/status'],
      ['GET', '/overlay/look'],
      ['GET', '/data'],
      ['GET', '/diagnostics'],
      ['POST', '/events', { name: 'Sneaky' }],
      ['POST', '/pilots', { callsign: 'Sneaky' }],
      ['PUT', '/network/public', { enabled: false }],
      ['PUT', '/network/remote', { enabled: false }],
      ['PUT', '/network/token', {}],
      ['PUT', '/active-event', { id: 'x' }],
      ['POST', '/data/export'],
      ['DELETE', '/events/x'],
      // The director's sign-in does not answer at this door.
      ['POST', '/public/session']
    ];
    // With nothing, with the access token, and with the token the Director was started with.
    for (const held of [{}, bearer(), auth()]) {
      for (const [method, path, body] of tries) {
        const res = await fetch(`${there()}${path}`, {
          method,
          headers: { ...(body ? { 'Content-Type': 'application/json' } : {}), ...held },
          body: body ? JSON.stringify(body) : undefined
        });
        expect(res.status, `${method} ${path}`).toBe(401);
        expect(res.headers.get('set-cookie'), `${method} ${path}`).toBeNull();
        const error = (await res.json()) as ProtocolError;
        expect(Object.keys(error).sort(), `${method} ${path}`).toEqual(['code', 'message']);
        expect(error.code).toBe('Unauthorized');
        expect(error.message).toContain('not open to the public');
      }
    }
    // Nothing was changed.
    const events = (await (await fetch(`${director.baseUrl}/events`)).json()) as { name: string }[];
    expect(events.map((event) => event.name)).toEqual(['Contract Night']);
    const view = await network();
    expect(view.public.enabled).toBe(true);
    expect(view.remote.enabled).toBe(true);
    expect(view.token).toBe(access);
  });

  it('still gets the three open reads and the public pages', async () => {
    expect(await (await fetch(`${there()}/health`)).text()).toBe('ok');
    expect((await fetch(`${there()}/about`)).status).toBe(200);
    expect((await fetch(`${there()}/time`)).status).toBe(200);
    expect((await fetch(`${there()}/public/event`)).status).toBe(200);
  });

  it('is still a visitor with the access token in hand', async () => {
    const hello = (await (
      await fetch(`${there()}/public/hello`, { headers: bearer() })
    ).json()) as PublicHello;
    expect(hello.caller).toBe('Visitor');
  });
});

describe.skipIf(!onNetwork)('a device at the Remote RD door', () => {
  it('gets the sign-in and nothing about the race until it shows the token', async () => {
    const hello = (await (await fetch(`${remote()}/public/hello`)).json()) as PublicHello;
    expect(hello.caller).toBe('Visitor');
    expect(hello.door).toBe('Remote');
    expect(hello.director_sign_in).toBe(true);
    expect('event' in hello).toBe(false);
    for (const path of ['/events', '/network', '/public/event', '/public/event/state']) {
      const res = await fetch(`${remote()}${path}`);
      expect(res.status, path).toBe(401);
      expect(((await res.json()) as ProtocolError).message).toContain("director's access token");
    }
    expect(await (await fetch(`${remote()}/health`)).text()).toBe('ok');
  });

  it('has the whole Director with the access token, and is never told the token', async () => {
    const read = await fetch(`${remote()}/events`, { headers: bearer() });
    expect(read.status).toBe(200);
    const hello = (await (
      await fetch(`${remote()}/public/hello`, { headers: bearer() })
    ).json()) as PublicHello;
    expect(hello.caller).toBe('Director');
    expect(hello.event).toBe('Contract Night');
    const told = await (await fetch(`${remote()}/network`, { headers: bearer() })).text();
    expect(told).not.toContain(access);
    expect('token' in (JSON.parse(told) as NetworkView)).toBe(false);
  });

  it('cannot change the access token', async () => {
    const res = await fetch(`${remote()}/network/token`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json', ...bearer() },
      body: JSON.stringify({ token: 'mine-now-thanks' })
    });
    expect(res.status).toBe(403);
    expect(((await res.json()) as ProtocolError).code).toBe('Forbidden');
    expect((await network()).token).toBe(access);
  });

  it('signs in for a cookie, uses it, and signs out', async () => {
    const wrong = await fetch(`${remote()}/public/session`, {
      method: 'POST',
      headers: { Authorization: 'Bearer not-the-token' }
    });
    expect(wrong.status).toBe(401);
    expect(wrong.headers.get('set-cookie')).toBeNull();

    const signed = await fetch(`${remote()}/public/session`, { method: 'POST', headers: bearer() });
    expect(signed.status).toBe(204);
    const set = signed.headers.get('set-cookie') ?? '';
    expect(set).toContain('HttpOnly');
    expect(set).toContain('SameSite=Strict');
    const cookie = set.split(';')[0];
    expect(cookie.startsWith('gridfpv_director=')).toBe(true);
    expect(cookie).not.toContain(access);

    const read = await fetch(`${remote()}/events`, { headers: { Cookie: cookie } });
    expect(read.status).toBe(200);
    const made = await fetch(`${remote()}/pilots`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Cookie: cookie },
      body: JSON.stringify({ callsign: 'Tablet' })
    });
    expect(made.ok).toBe(true);
    // The same cookie counts for nothing at the Public door.
    expect((await fetch(`${there()}/events`, { headers: { Cookie: cookie } })).status).toBe(401);

    const out = await fetch(`${remote()}/public/session`, {
      method: 'DELETE',
      headers: { Cookie: cookie }
    });
    expect(out.status).toBe(204);
    expect((await fetch(`${remote()}/events`, { headers: { Cookie: cookie } })).status).toBe(401);
  });

  it('a change asked for by another website is refused, cookie or not', async () => {
    const signed = await fetch(`${remote()}/public/session`, { method: 'POST', headers: bearer() });
    const cookie = (signed.headers.get('set-cookie') ?? '').split(';')[0];
    const forged = await fetch(`${remote()}/pilots`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        Cookie: cookie,
        Origin: 'http://evil.example'
      },
      body: JSON.stringify({ callsign: 'Forged' })
    });
    expect(forged.status).toBe(403);
    expect(((await forged.json()) as ProtocolError).code).toBe('Forbidden');
    const pilots = (await (await fetch(`${director.baseUrl}/pilots`)).json()) as {
      callsign: string;
    }[];
    expect(pilots.map((pilot) => pilot.callsign)).not.toContain('Forged');
  });

  it('is signed out when the token is changed on the Grid computer', async () => {
    const signed = await fetch(`${remote()}/public/session`, { method: 'POST', headers: bearer() });
    const cookie = (signed.headers.get('set-cookie') ?? '').split(';')[0];
    const old = bearer();
    const changed = await fetch(`${director.baseUrl}/network/token`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json', ...auth() },
      body: JSON.stringify({ token: 'sunday-race-7' })
    });
    expect(changed.status).toBe(200);
    expect(((await changed.json()) as NetworkView).token).toBe('sunday-race-7');
    access = 'sunday-race-7';
    // The door starts again, so nothing opened with the old token stays open.
    const status = async (headers: Record<string, string>): Promise<number> => {
      for (let n = 0; n < 100; n += 1) {
        try {
          return (await fetch(`${remote()}/events`, { headers })).status;
        } catch {
          await new Promise((r) => setTimeout(r, 50));
        }
      }
      throw new Error('the Remote RD door did not come back');
    };
    expect(await status({ Cookie: cookie })).toBe(401);
    expect(await status(old)).toBe(401);
    expect(await status(bearer())).toBe(200);
  });
});

describe('switching the doors off again', () => {
  it('closes each listener, and keeps its port and the token', async () => {
    for (const door of ['remote', 'public'] as const) {
      const res = await setDoor(door, { enabled: false });
      expect(res.status).toBe(200);
      const view = await until(door, 'Off');
      expect(view.addresses).toEqual([]);
      expect(view.port).toBe(door === 'public' ? port : remotePort);
    }
    if (onNetwork) {
      await expect(fetch(`${there()}/public/hello`)).rejects.toThrow();
      await expect(fetch(`${remote()}/public/hello`)).rejects.toThrow();
      expect((await network()).token).toBe(access);
    }
    // The Grid computer is untouched.
    expect((await fetch(`${director.baseUrl}/health`)).status).toBe(200);
  });
});

describe('a pilot signs in with a code (docs/primitives.md §11.3)', () => {
  let event: string;
  let ace: string;
  let code: string;

  beforeAll(async () => {
    const post = async <T>(path: string, body: unknown): Promise<T> => {
      const res = await fetch(`${director.baseUrl}${path}`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', ...auth() },
        body: JSON.stringify(body)
      });
      if (!res.ok) throw new Error(`${path}: ${res.status} ${await res.text()}`);
      return (await res.json()) as T;
    };
    ace = (await post<{ id: string }>('/pilots', { callsign: 'Ace' })).id;
    const open = (await post<{ id: string }>('/classes', { name: 'Open' })).id;
    event = (await post<{ id: string }>('/events', { name: 'Sign-in Night' })).id;
    await fetch(`${director.baseUrl}/active-event`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json', ...auth() },
      body: JSON.stringify({ id: event })
    });
    await post(`/events/${event}/commands`, {
      Record: { RosterSet: { class: open, pilots: [ace] } }
    });
  });

  it('GET /events/{id}/access lists each pilot with a six-character code, and is not kept', async () => {
    const res = await fetch(`${director.baseUrl}/events/${event}/access`);
    expect(res.status).toBe(200);
    expect(res.headers.get('cache-control')).toBe('no-store');
    const view = (await res.json()) as {
      pilots: { pilot: string; code: string; signed_in: boolean }[];
    };
    expect(view.pilots).toHaveLength(1);
    expect(Object.keys(view.pilots[0]).sort()).toEqual(['code', 'pilot', 'signed_in']);
    expect(view.pilots[0].pilot).toBe(ace);
    expect(view.pilots[0].code).toMatch(/^[ABCDEFGHJKLMNPQRSTUVWXYZ23456789]{6}$/);
    expect(view.pilots[0].signed_in).toBe(false);
    code = view.pilots[0].code;
  });

  it('POST /public/pilot/session answers the pilot and sets a cookie no script can read', async () => {
    const wrong = await fetch(`${director.baseUrl}/public/pilot/session`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ code: code === 'AAAAAA' ? 'BBBBBB' : 'AAAAAA' })
    });
    expect(wrong.status).toBe(401);
    expect(((await wrong.json()) as ProtocolError).code).toBe('Unauthorized');
    expect(wrong.headers.get('set-cookie')).toBeNull();

    const res = await fetch(`${director.baseUrl}/public/pilot/session`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ code: code.toLowerCase() })
    });
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ id: ace, callsign: 'Ace' });
    const set = res.headers.get('set-cookie') ?? '';
    expect(set).toContain('HttpOnly');
    expect(set).toContain('SameSite=Strict');
    const cookie = set.split(';')[0];
    expect(cookie.startsWith('gridfpv_pilot=')).toBe(true);

    const hello = (await (
      await fetch(`${director.baseUrl}/public/hello`, { headers: { Cookie: cookie } })
    ).json()) as PublicHello;
    expect(hello.pilot).toEqual({ id: ace, callsign: 'Ace' });
    // Without the cookie there is no pilot field at all.
    const plain = (await (await fetch(`${director.baseUrl}/public/hello`)).json()) as PublicHello;
    expect('pilot' in plain).toBe(false);

    const out = await fetch(`${director.baseUrl}/public/pilot/session`, {
      method: 'DELETE',
      headers: { Cookie: cookie }
    });
    expect(out.status).toBe(204);
    const after = (await (
      await fetch(`${director.baseUrl}/public/hello`, { headers: { Cookie: cookie } })
    ).json()) as PublicHello;
    expect('pilot' in after).toBe(false);
  });

  it('the QR picture is refused with the reason while other devices cannot reach Grid', async () => {
    const res = await fetch(`${director.baseUrl}/events/${event}/access/${ace}/qr`);
    expect(res.status).toBe(409);
    const error = (await res.json()) as ProtocolError;
    expect(error.code).toBe('Refused');
    expect(error.message).toContain('Network page');
  });
});
