/**
 * Other devices on the network: the contract a phone, a tablet and the console rely on
 * (docs/primitives.md §11), pinned against the real Director over real sockets.
 *
 * - the Grid computer is the director, and reads and writes as before;
 * - a device that comes in by the computer's network address is a visitor: it gets
 *   `/public/hello` and is refused everything else with one typed error;
 * - the director's access token, shown from another device, opens the whole Director, and a
 *   browser sign-in carries it in a cookie from then on;
 * - the listener for other devices is off until `PUT /network` switches it on.
 */
import { createServer } from 'node:net';
import { networkInterfaces } from 'node:os';

import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import type { NetworkView, ProtocolError, PublicHello } from '@gridfpv/types';

import { type Director } from '../test-harness/director.ts';
import { startContractDirector } from './harness.ts';

let director: Director;
let port: number;
/** This computer's address on its network, when it has one. */
const lan = Object.values(networkInterfaces())
  .flat()
  .find(
    (entry) =>
      entry && entry.family === 'IPv4' && !entry.internal && !entry.address.startsWith('169.254.')
  )?.address;
const onNetwork = lan !== undefined;
const there = (): string => `http://${lan}:${port}`;

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

async function setNetwork(body: unknown, headers: Record<string, string> = auth()) {
  return fetch(`${director.baseUrl}/network`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json', ...headers },
    body: JSON.stringify(body)
  });
}

async function until(state: NetworkView['state']): Promise<NetworkView> {
  for (let n = 0; n < 200; n += 1) {
    const view = await network();
    if (view.state === state) return view;
    await new Promise((r) => setTimeout(r, 50));
  }
  throw new Error(`the listener never reached ${state}: ${JSON.stringify(await network())}`);
}

beforeAll(async () => {
  director = await startContractDirector();
  port = await freePort();
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
    expect(Object.keys(hello).sort()).toEqual(['caller', 'director_sign_in', 'kiosk', 'version']);
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

describe('GET and PUT /network', () => {
  it('starts off, with exactly the documented fields', async () => {
    const view = await network();
    expect(Object.keys(view).sort()).toEqual(['addresses', 'enabled', 'port', 'state']);
    expect(view).toEqual({ enabled: false, port: 8081, state: 'Off', addresses: [] });
  });

  it('needs the director to change it', async () => {
    const res = await setNetwork({ enabled: true, port }, {});
    expect(res.status).toBe(401);
    expect(((await res.json()) as ProtocolError).code).toBe('Unauthorized');
    expect((await network()).enabled).toBe(false);
  });

  it('refuses a port that cannot be used, with a typed error', async () => {
    for (const bad of [0, 80, 1023]) {
      const res = await setNetwork({ enabled: true, port: bad });
      expect(res.status, String(bad)).toBe(400);
      expect(((await res.json()) as ProtocolError).code).toBe('BadRequest');
    }
    const own = Number(new URL(director.baseUrl).port);
    expect((await setNetwork({ enabled: true, port: own })).status).toBe(400);
    expect((await network()).enabled).toBe(false);
  });

  it('switches on, and then other devices can connect', async () => {
    const res = await setNetwork({ enabled: true, port });
    expect(res.status).toBe(200);
    const view = await until('On');
    expect(view.port).toBe(port);
    expect(view.enabled).toBe(true);
    if (onNetwork) expect(view.addresses).toContain(`http://${lan}:${port}/`);
    for (const address of view.addresses) expect(address).toMatch(/^http:\/\/[\d.]+:\d+\/$/);
  });
});

describe.skipIf(!onNetwork)('a device that comes in by the network address', () => {
  it('is a visitor', async () => {
    const hello = (await (await fetch(`${there()}/public/hello`)).json()) as PublicHello;
    expect(hello.caller).toBe('Visitor');
    expect(hello.event).toBe('Contract Night');
  });

  it('is refused every read and write of the console, with one typed error', async () => {
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
      ['PUT', '/network', { enabled: false }],
      ['PUT', '/active-event', { id: 'x' }],
      ['POST', '/data/export'],
      ['DELETE', '/events/x']
    ];
    for (const [method, path, body] of tries) {
      const res = await fetch(`${there()}${path}`, {
        method,
        headers: body ? { 'Content-Type': 'application/json' } : {},
        body: body ? JSON.stringify(body) : undefined
      });
      expect(res.status, `${method} ${path}`).toBe(401);
      const error = (await res.json()) as ProtocolError;
      expect(Object.keys(error).sort(), `${method} ${path}`).toEqual(['code', 'message']);
      expect(error.code).toBe('Unauthorized');
      expect(error.message).toContain("director's access token");
    }
    // Nothing was changed.
    const events = (await (await fetch(`${director.baseUrl}/events`)).json()) as { name: string }[];
    expect(events.map((event) => event.name)).toEqual(['Contract Night']);
    expect((await network()).enabled).toBe(true);
  });

  it('still gets the three open reads', async () => {
    expect(await (await fetch(`${there()}/health`)).text()).toBe('ok');
    expect((await fetch(`${there()}/about`)).status).toBe(200);
    expect((await fetch(`${there()}/time`)).status).toBe(200);
  });

  it('has the whole Director with the access token', async () => {
    const read = await fetch(`${there()}/events`, { headers: auth() });
    expect(read.status).toBe(200);
    const hello = (await (
      await fetch(`${there()}/public/hello`, { headers: auth() })
    ).json()) as PublicHello;
    expect(hello.caller).toBe('Director');
  });

  it('signs in for a cookie, uses it, and signs out', async () => {
    const wrong = await fetch(`${there()}/public/session`, {
      method: 'POST',
      headers: { Authorization: 'Bearer not-the-token' }
    });
    expect(wrong.status).toBe(401);
    expect(wrong.headers.get('set-cookie')).toBeNull();

    const signed = await fetch(`${there()}/public/session`, { method: 'POST', headers: auth() });
    expect(signed.status).toBe(204);
    const set = signed.headers.get('set-cookie') ?? '';
    expect(set).toContain('HttpOnly');
    expect(set).toContain('SameSite=Strict');
    const cookie = set.split(';')[0];
    expect(cookie.startsWith('gridfpv_director=')).toBe(true);
    expect(cookie).not.toContain(director.token);

    const read = await fetch(`${there()}/events`, { headers: { Cookie: cookie } });
    expect(read.status).toBe(200);
    const made = await fetch(`${there()}/pilots`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Cookie: cookie },
      body: JSON.stringify({ callsign: 'Tablet' })
    });
    expect(made.ok).toBe(true);

    const out = await fetch(`${there()}/public/session`, {
      method: 'DELETE',
      headers: { Cookie: cookie }
    });
    expect(out.status).toBe(204);
    expect((await fetch(`${there()}/events`, { headers: { Cookie: cookie } })).status).toBe(401);
  });

  it('a change asked for by another website is refused, cookie or not', async () => {
    const signed = await fetch(`${there()}/public/session`, { method: 'POST', headers: auth() });
    const cookie = (signed.headers.get('set-cookie') ?? '').split(';')[0];
    const forged = await fetch(`${there()}/pilots`, {
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
});

describe('switching it off again', () => {
  it('closes the listener for other devices', async () => {
    const res = await setNetwork({ enabled: false });
    expect(res.status).toBe(200);
    const view = await until('Off');
    expect(view.addresses).toEqual([]);
    // The port is still remembered.
    expect(view.port).toBe(port);
    if (onNetwork) {
      await expect(fetch(`${there()}/public/hello`)).rejects.toThrow();
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
    const open = (await post<{ id: string }>('/classes', { name: 'Open', source: 'Custom' })).id;
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
