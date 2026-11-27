import { describe, expect, it } from 'vitest';
import type {
  CreateEventRequest,
  CreateTimerRequest,
  DiscoveredTimer,
  EventMeta,
  ProtocolError,
  Timer,
  TimerView
} from '@gridfpv/types';
import type { FetchLike } from './client.js';

describe('events lifecycle helpers (#72)', () => {
  it('listEvents GETs /events and returns the EventMeta list', async () => {
    const calls: string[] = [];
    const fetch: FetchLike = async (input) => {
      calls.push(String(input));
      return {
        ok: true,
        status: 200,
        json: async (): Promise<unknown> => [
          { id: 'spring-cup-2026-ab12', name: 'Spring Cup', created_at: 1, persistent: true }
        ]
      } as unknown as Response;
    };
    const { listEvents } = await import('./client.js');
    const events = await listEvents('http://director.local:8080/', { fetch });
    expect(calls[0]).toBe('http://director.local:8080/events');
    expect(events[0].id).toBe('spring-cup-2026-ab12');
  });

  it('listEvents accepts an EMPTY list — a fresh Director has no events (#414)', async () => {
    const fetch: FetchLike = async () =>
      ({ ok: true, status: 200, json: async (): Promise<unknown> => [] }) as unknown as Response;
    const { listEvents } = await import('./client.js');
    expect(await listEvents('http://director.local:8080/', { fetch })).toEqual([]);
  });

  it('createEvent POSTs the name to /events with the RD token and returns the new EventMeta', async () => {
    const seen: { url: string; init?: RequestInit }[] = [];
    const fetch: FetchLike = async (input, init) => {
      seen.push({ url: String(input), init });
      return {
        ok: true,
        status: 200,
        json: async (): Promise<unknown> => ({
          id: 'race-night-xy99',
          name: 'Race Night',
          created_at: 2,
          persistent: true
        })
      } as unknown as Response;
    };
    const { createEvent } = await import('./client.js');
    const meta = await createEvent('http://director.local:8080', 'Race Night', 'rd-tok', { fetch });
    expect(seen[0].url).toBe('http://director.local:8080/events');
    expect(seen[0].init?.method).toBe('POST');
    const headers = seen[0].init?.headers as Record<string, string>;
    expect(headers.Authorization).toBe('Bearer rd-tok');
    expect(JSON.parse(seen[0].init?.body as string)).toEqual({ name: 'Race Night' });
    expect(meta.id).toBe('race-night-xy99');
    expect(meta.persistent).toBe(true);
  });

  it('createEvent omits Authorization when no token is given (full-trust open Director) and forwards optional fields', async () => {
    const seen: { url: string; init?: RequestInit }[] = [];
    const fetch: FetchLike = async (input, init) => {
      seen.push({ url: String(input), init });
      return {
        ok: true,
        status: 200,
        json: async (): Promise<unknown> => ({
          id: 'spring-cup-ab12',
          name: 'Spring Cup',
          created_at: 3,
          persistent: true,
          date: '2026-06-20',
          location: 'Main field'
        })
      } as unknown as Response;
    };
    const { createEvent } = await import('./client.js');
    const meta = await createEvent('http://director.local:8080', 'Spring Cup', undefined, {
      fetch,
      fields: { date: '2026-06-20', location: 'Main field' }
    });
    const headers = seen[0].init?.headers as Record<string, string>;
    expect(headers.Authorization).toBeUndefined();
    expect(JSON.parse(seen[0].init?.body as string)).toEqual({
      name: 'Spring Cup',
      date: '2026-06-20',
      location: 'Main field'
    });
    expect(meta.date).toBe('2026-06-20');
    expect(meta.location).toBe('Main field');
  });

  it('updateEvent PUTs the name and details to /events/{id} with the id encoded and the token, and returns the event as it now is', async () => {
    const seen: { url: string; init?: RequestInit }[] = [];
    const now: EventMeta = {
      id: 'club night/1',
      name: 'Club Night',
      created_at: 4,
      persistent: true,
      location: 'Back field'
    };
    const fetch: FetchLike = async (input, init) => {
      seen.push({ url: String(input), init });
      return {
        ok: true,
        status: 200,
        json: async (): Promise<unknown> => now
      } as unknown as Response;
    };
    const { updateEvent } = await import('./client.js');
    const request: CreateEventRequest = { name: 'Club Night', location: 'Back field' };
    expect(
      await updateEvent('http://director.local:8080/', 'club night/1', request, 'rd-tok', { fetch })
    ).toEqual(now);
    expect(seen).toHaveLength(1);
    expect(seen[0].url).toBe('http://director.local:8080/events/club%20night%2F1');
    expect(seen[0].init?.method).toBe('PUT');
    const headers = seen[0].init?.headers as Record<string, string>;
    expect(headers.Authorization).toBe('Bearer rd-tok');
    expect(headers['Content-Type']).toBe('application/json');
    expect(JSON.parse(seen[0].init?.body as string)).toEqual(request);
  });

  it('updateEvent omits Authorization when no token is given', async () => {
    const seen: { url: string; init?: RequestInit }[] = [];
    const fetch: FetchLike = async (input, init) => {
      seen.push({ url: String(input), init });
      return {
        ok: true,
        status: 200,
        json: async (): Promise<unknown> => ({
          id: 'e1',
          name: 'A',
          created_at: 1,
          persistent: true
        })
      } as unknown as Response;
    };
    const { updateEvent } = await import('./client.js');
    await updateEvent('http://director.local:8080', 'e1', { name: 'A' }, undefined, { fetch });
    const headers = seen[0].init?.headers as Record<string, string>;
    expect(headers.Authorization).toBeUndefined();
  });

  it("updateEvent rejects an empty name with the Director's sentence and the 400", async () => {
    const SAID = 'An event needs a name.';
    const fetch: FetchLike = async () =>
      ({
        ok: false,
        status: 400,
        json: async (): Promise<unknown> => ({ code: 'BadRequest', message: SAID })
      }) as unknown as Response;
    const { updateEvent, isRequestFailure } = await import('./client.js');
    const err: unknown = await updateEvent(
      'http://director.local:8080',
      'evt-9f3a7c',
      { name: '' },
      'rd-tok',
      { fetch }
    ).then(
      () => undefined,
      (e: unknown) => e
    );
    expect(isRequestFailure(err)).toBe(true);
    if (!isRequestFailure(err)) return;
    expect(err.message).toBe(SAID);
    expect(err.status).toBe(400);
    expect(err.code).toBe('BadRequest');
  });
});

// ── The timer directory ──────────────────────────────────────────────────────────────────────

describe('the timer directory', () => {
  const BASE = 'http://director.local:8080';
  const GATE: Timer = { id: 'gate-x1y2', name: 'Gate', kind: 'Oft', address: '192.168.1.50:7270' };

  /** A fetch that records every request and answers 200 with `body`. */
  function recording(body: unknown): {
    fetch: FetchLike;
    seen: { url: string; init?: RequestInit }[];
  } {
    const seen: { url: string; init?: RequestInit }[] = [];
    const fetch: FetchLike = async (input, init) => {
      seen.push({ url: String(input), init });
      return {
        ok: true,
        status: 200,
        json: async (): Promise<unknown> => body
      } as unknown as Response;
    };
    return { fetch, seen };
  }
  const headersOf = (init?: RequestInit): Record<string, string> =>
    init?.headers as Record<string, string>;

  it('listTimers GETs /timers with no token and returns each timer with its status', async () => {
    const views: TimerView[] = [
      { timer: GATE, status: { connected: true, seats: ['0', '1'], clock_error_micros: 200 } },
      {
        timer: { ...GATE, id: 'far-9zz0', name: 'Far' },
        status: { connected: false, problem: 'Nothing answered at that address.', seats: [] }
      }
    ];
    const { fetch, seen } = recording(views);
    const { listTimers } = await import('./client.js');
    expect(await listTimers(`${BASE}/`, { fetch })).toEqual(views);
    expect(seen[0].url).toBe(`${BASE}/timers`);
    expect(seen[0].init?.method).toBeUndefined();
    expect(headersOf(seen[0].init).Authorization).toBeUndefined();
  });

  it('listDiscoveredTimers GETs /timers/discovered with no token and returns what was found', async () => {
    const found: DiscoveredTimer[] = [
      { name: 'Gate', address: '192.168.1.50:7270', seats: 4, added: true },
      { name: 'Far', address: '192.168.1.51:7270', added: false }
    ];
    const { fetch, seen } = recording(found);
    const { listDiscoveredTimers } = await import('./client.js');
    expect(await listDiscoveredTimers(`${BASE}/`, { fetch })).toEqual(found);
    expect(seen[0].url).toBe(`${BASE}/timers/discovered`);
    expect(seen[0].init?.method).toBeUndefined();
    expect(headersOf(seen[0].init).Authorization).toBeUndefined();
  });

  it('createTimer POSTs the name, kind and address with the token and returns the timer', async () => {
    const { fetch, seen } = recording(GATE);
    const { createTimer } = await import('./client.js');
    const request: CreateTimerRequest = { name: 'Gate', kind: 'Oft', address: '192.168.1.50:7270' };
    expect(await createTimer(BASE, request, 'rd-tok', { fetch })).toEqual(GATE);
    expect(seen[0].url).toBe(`${BASE}/timers`);
    expect(seen[0].init?.method).toBe('POST');
    expect(headersOf(seen[0].init).Authorization).toBe('Bearer rd-tok');
    expect(headersOf(seen[0].init)['Content-Type']).toBe('application/json');
    expect(JSON.parse(seen[0].init?.body as string)).toEqual(request);
  });

  it("createTimer rejects a bad address with the Director's sentence and the 400", async () => {
    const SAID = 'A timer needs an address, like 192.168.1.50:7270.';
    const fetch: FetchLike = async () =>
      ({
        ok: false,
        status: 400,
        json: async (): Promise<unknown> => ({ code: 'BadRequest', message: SAID })
      }) as unknown as Response;
    const { createTimer, isRequestFailure } = await import('./client.js');
    const err = await createTimer(BASE, { name: 'Gate', kind: 'Oft', address: 'nope' }, undefined, {
      fetch
    }).then(
      () => undefined,
      (e: unknown) => e
    );
    expect(isRequestFailure(err)).toBe(true);
    if (!isRequestFailure(err)) return;
    expect(err.message).toBe(SAID);
    expect(err.status).toBe(400);
  });

  it('renameTimer PUTs the new name to /timers/{id} and gives back the timer', async () => {
    const seen: { url: string; init?: RequestInit }[] = [];
    const renamed = { id: 'gate-x1y2', name: 'Start gate', kind: 'Oft', address: '10.0.0.5:7270' };
    const fetch: FetchLike = async (input, init) => {
      seen.push({ url: String(input), init });
      return { ok: true, status: 200, json: async () => renamed } as unknown as Response;
    };
    const { renameTimer } = await import('./client.js');
    await expect(
      renameTimer(BASE, 'gate/x1 y2', 'Start gate', 'rd-tok', { fetch })
    ).resolves.toEqual(renamed);
    expect(seen[0].url).toBe(`${BASE}/timers/gate%2Fx1%20y2`);
    expect(seen[0].init?.method).toBe('PUT');
    expect(JSON.parse(String(seen[0].init?.body))).toEqual({ name: 'Start gate' });
    expect(headersOf(seen[0].init).Authorization).toBe('Bearer rd-tok');
  });

  it('deleteTimer DELETEs /timers/{id} with the id encoded, and reads no body', async () => {
    const seen: { url: string; init?: RequestInit }[] = [];
    const fetch: FetchLike = async (input, init) => {
      seen.push({ url: String(input), init });
      return {
        ok: true,
        status: 200,
        json: async (): Promise<unknown> => {
          throw new SyntaxError('Unexpected end of JSON input');
        }
      } as unknown as Response;
    };
    const { deleteTimer } = await import('./client.js');
    await expect(deleteTimer(BASE, 'gate/x1 y2', 'rd-tok', { fetch })).resolves.toBeUndefined();
    expect(seen[0].url).toBe(`${BASE}/timers/gate%2Fx1%20y2`);
    expect(seen[0].init?.method).toBe('DELETE');
    expect(headersOf(seen[0].init).Authorization).toBe('Bearer rd-tok');
  });
});

// ── Request failures: the Director's words, never a route line (#433) ────────────────────────
//
// A refused write used to reach the RD as `DELETE /classes/{classId} failed: HTTP 400` — a raw id
// on screen and the server's explanation discarded, while the Director had written a sentence
// precisely so it could be shown.

describe("request failures carry the Director's words (#433)", () => {
  const BASE = 'http://director.local:8080';
  const TOKEN = 'rd-tok';
  const EVT = 'evt-9f3a7c';
  const PILOT = 'pilot-3c4d';
  const CLASS = 'class-88ee';
  const TIMER = 'gate-x1y2';
  const FORMAT = 'format-5a6b';
  const PROFILE = 'profile-7c8d';
  /** Every raw handle the sweep hands the client — none may come back out in a message. */
  const RAW_IDS = [EVT, PILOT, CLASS, TIMER, FORMAT, PROFILE];

  /**
   * A stand-in request body. The failing fetch never reads one, and `never` satisfies every
   * request-param type, so the sweep below needs no fixture per wire shape.
   */
  const REQ = {} as unknown as never;

  /** A fetch that fails every request — with a typed `ProtocolError` body, or with none at all. */
  const failing =
    (status: number, body?: ProtocolError): FetchLike =>
    async () =>
      ({
        ok: false,
        status,
        json: async (): Promise<unknown> => {
          // A bodyless failure is what a bare 500 (or an HTML error page) actually looks like.
          if (!body) throw new SyntaxError('Unexpected end of JSON input');
          return body;
        }
      }) as unknown as Response;

  /** Run a call expected to reject and hand back the error it threw. */
  async function caught(run: () => Promise<unknown>): Promise<Error & { status?: number }> {
    try {
      await run();
    } catch (e) {
      return e as Error & { status?: number };
    }
    throw new Error('expected the request to reject');
  }

  it("throws the Director's refusal verbatim — the sentence #433 was discarding", async () => {
    const REFUSAL = 'Open is a built-in class — it cannot be edited or deleted';
    const { deleteClass, isRequestFailure } = await import('./client.js');
    const err = await caught(() =>
      deleteClass(BASE, CLASS, TOKEN, {
        fetch: failing(400, { code: 'BadRequest', message: REFUSAL })
      })
    );
    // Verbatim: not prefixed, not wrapped, not appended to.
    expect(err.message).toBe(REFUSAL);
    expect(isRequestFailure(err)).toBe(true);
    expect(err.status).toBe(400);
    expect((err as { code?: string }).code).toBe('BadRequest');
  });

  it('falls back only when there is no body, and then says what was attempted', async () => {
    const { deleteClass } = await import('./client.js');
    const err = await caught(() => deleteClass(BASE, CLASS, TOKEN, { fetch: failing(500) }));
    // Honest, actionable, and status-bearing — a poor message is fine, a silent one is not.
    expect(err.message).toBe('The Director could not delete the class (HTTP 500).');
    expect(err.status).toBe(500);
    expect((err as { code?: string }).code).toBeUndefined();
  });

  it('never puts a raw id — or a route line — in a surfaced message', async () => {
    const c = await import('./client.js');
    const probes: [string, (fetch: FetchLike) => Promise<unknown>][] = [
      ['listEvents', (fetch) => c.listEvents(BASE, { fetch })],
      ['createEvent', (fetch) => c.createEvent(BASE, 'Friday', TOKEN, { fetch })],
      ['deleteEvent', (fetch) => c.deleteEvent(BASE, EVT, TOKEN, { fetch })],
      ['getActiveEvent', (fetch) => c.getActiveEvent(BASE, { fetch })],
      ['setActiveEvent', (fetch) => c.setActiveEvent(BASE, EVT, TOKEN, { fetch })],
      ['listPilots', (fetch) => c.listPilots(BASE, { fetch })],
      ['createPilot', (fetch) => c.createPilot(BASE, REQ, TOKEN, { fetch })],
      ['updatePilot', (fetch) => c.updatePilot(BASE, PILOT, REQ, TOKEN, { fetch })],
      ['deletePilot', (fetch) => c.deletePilot(BASE, PILOT, TOKEN, { fetch })],
      ['listClasses', (fetch) => c.listClasses(BASE, { fetch })],
      ['createClass', (fetch) => c.createClass(BASE, REQ, TOKEN, { fetch })],
      ['updateClass', (fetch) => c.updateClass(BASE, CLASS, REQ, TOKEN, { fetch })],
      ['deleteClass', (fetch) => c.deleteClass(BASE, CLASS, TOKEN, { fetch })],
      ['setClassHidden', (fetch) => c.setClassHidden(BASE, CLASS, true, TOKEN, { fetch })],
      ['listTimers', (fetch) => c.listTimers(BASE, { fetch })],
      ['listDiscoveredTimers', (fetch) => c.listDiscoveredTimers(BASE, { fetch })],
      ['createTimer', (fetch) => c.createTimer(BASE, REQ, TOKEN, { fetch })],
      ['deleteTimer', (fetch) => c.deleteTimer(BASE, TIMER, TOKEN, { fetch })],
      ['renameTimer', (fetch) => c.renameTimer(BASE, TIMER, 'Gate', TOKEN, { fetch })],
      ['getTimerConfig', (fetch) => c.getTimerConfig(BASE, TIMER, { fetch })],
      ['putTimerConfig', (fetch) => c.putTimerConfig(BASE, TIMER, REQ, TOKEN, { fetch })],
      ['updateEvent', (fetch) => c.updateEvent(BASE, EVT, REQ, TOKEN, { fetch })],
      ['listFormats', (fetch) => c.listFormats(BASE, { fetch })],
      ['createFormat', (fetch) => c.createFormat(BASE, REQ, TOKEN, { fetch })],
      ['updateFormat', (fetch) => c.updateFormat(BASE, FORMAT, REQ, TOKEN, { fetch })],
      ['deleteFormat', (fetch) => c.deleteFormat(BASE, FORMAT, TOKEN, { fetch })],
      ['listProfiles', (fetch) => c.listProfiles(BASE, TIMER, { fetch })],
      ['createProfile', (fetch) => c.createProfile(BASE, TIMER, REQ, TOKEN, { fetch })],
      ['updateProfile', (fetch) => c.updateProfile(BASE, TIMER, PROFILE, REQ, TOKEN, { fetch })],
      ['deleteProfile', (fetch) => c.deleteProfile(BASE, TIMER, PROFILE, TOKEN, { fetch })],
      ['applyProfile', (fetch) => c.applyProfile(BASE, TIMER, PROFILE, TOKEN, { fetch })],
      ['getEventState', (fetch) => c.getEventState(BASE, EVT, { fetch })],
      ['sendCommand', (fetch) => c.sendCommand(BASE, EVT, REQ, TOKEN, { fetch })],
      ['createBracket', (fetch) => c.createBracket(BASE, EVT, REQ, TOKEN, { fetch })],
      ['createMains', (fetch) => c.createMains(BASE, EVT, REQ, TOKEN, { fetch })],
      ['createRoundRobin', (fetch) => c.createRoundRobin(BASE, EVT, REQ, TOKEN, { fetch })],
      ['postPass', (fetch) => c.postPass(BASE, EVT, REQ, TOKEN, { fetch })]
    ];

    for (const [name, run] of probes) {
      const err = await caught(() => run(failing(400)));
      const surfaced = `${name}: ${err.message}`;
      for (const id of RAW_IDS) expect(surfaced).not.toContain(id);
      // No method/URL either — a route line is what carried the ids in the first place.
      expect(surfaced).not.toMatch(/\//);
      expect(surfaced).not.toMatch(/\b(GET|PUT|POST|DELETE)\b/);
      // Still honest about the status, and still branchable on it.
      expect(surfaced).toContain('HTTP 400');
      expect(err.status).toBe(400);
    }
  });

  it("prefers the Director's sentence over the fallback on every call site", async () => {
    const c = await import('./client.js');
    const SAID = 'the callsign is required';
    const fetch = failing(400, { code: 'BadRequest', message: SAID });
    const messages = await Promise.all(
      [
        () => c.createPilot(BASE, REQ, TOKEN, { fetch }),
        () => c.updatePilot(BASE, PILOT, REQ, TOKEN, { fetch }),
        () => c.createClass(BASE, REQ, TOKEN, { fetch }),
        () => c.setClassHidden(BASE, CLASS, true, TOKEN, { fetch }),
        () => c.setActiveEvent(BASE, EVT, TOKEN, { fetch })
      ].map(async (run) => (await caught(run)).message)
    );
    expect(messages).toEqual([SAID, SAID, SAID, SAID, SAID]);
  });

  it('carries the status structurally so auth detection never reads the words', async () => {
    const { deleteEvent, isRequestFailure } = await import('./client.js');
    // A 401 whose message says nothing about 401 — the console's `isAuthFailure` must still fire.
    const err = await caught(() =>
      deleteEvent(BASE, EVT, undefined, {
        fetch: failing(401, { code: 'Unauthorized', message: 'Control on this Director is gated.' })
      })
    );
    expect(err.message).toBe('Control on this Director is gated.');
    expect(err.message).not.toContain('401');
    expect(isRequestFailure(err) && err.status === 401).toBe(true);
    expect((err as { code?: string }).code).toBe('Unauthorized');
  });
});

describe('other devices on the network (docs/primitives.md §11)', () => {
  const answer = (status: number, body: unknown): Response =>
    ({
      ok: status >= 200 && status < 300,
      status,
      json: async (): Promise<unknown> => body,
      text: async (): Promise<string> => JSON.stringify(body)
    }) as unknown as Response;
  const recorder = (status: number, body: unknown) => {
    const seen: { url: string; init?: RequestInit }[] = [];
    const fetch: FetchLike = async (input, init) => {
      seen.push({ url: String(input), init });
      return answer(status, body);
    };
    return { seen, fetch };
  };
  const door = {
    enabled: true,
    port: 8081,
    port_picked: false,
    state: 'On',
    addresses: ['http://192.168.1.20:8081/']
  };
  const view = { public: door, remote: { ...door, enabled: false, state: 'Off', addresses: [] } };

  it('getNetwork GETs /network', async () => {
    const { seen, fetch } = recorder(200, view);
    const { getNetwork } = await import('./client.js');
    expect(await getNetwork('http://d.local:8080/', { fetch })).toEqual(view);
    expect(seen[0].url).toBe('http://d.local:8080/network');
    expect(seen[0].init?.method ?? 'GET').toBe('GET');
  });

  it('putDoor PUTs to the door named, with the token, and leaves the port out when it is not given', async () => {
    const { seen, fetch } = recorder(200, view);
    const { putDoor } = await import('./client.js');
    await putDoor('http://d.local:8080', 'Public', { enabled: true }, 'rd-tok', { fetch });
    await putDoor('http://d.local:8080', 'Remote', { enabled: false, port: 9000 }, undefined, {
      fetch
    });
    expect(seen[0].url).toBe('http://d.local:8080/network/public');
    expect(seen[0].init?.method).toBe('PUT');
    expect((seen[0].init?.headers as Record<string, string>).Authorization).toBe('Bearer rd-tok');
    expect(JSON.parse(seen[0].init?.body as string)).toEqual({ enabled: true });
    expect(seen[1].url).toBe('http://d.local:8080/network/remote');
    expect((seen[1].init?.headers as Record<string, string>).Authorization).toBeUndefined();
    expect(JSON.parse(seen[1].init?.body as string)).toEqual({ enabled: false, port: 9000 });
  });

  it("putDoor rejects with the Director's own words", async () => {
    const { fetch } = recorder(400, {
      code: 'BadRequest',
      message: 'Port 8080 is the one Grid itself uses on this computer. Pick another.'
    });
    const { putDoor } = await import('./client.js');
    await expect(
      putDoor('http://d.local:8080', 'Public', { enabled: true, port: 8080 }, undefined, { fetch })
    ).rejects.toThrow('Port 8080 is the one Grid itself uses on this computer. Pick another.');
  });

  it('the server link is read, set and an event replaced at the right addresses', async () => {
    const link = {
      clubs: [{ id: 'k1', name: '', url: 'https://grid.example.org', trust: 'System' }],
      state: 'NotSet',
      events: []
    };
    const { seen, fetch } = recorder(200, link);
    const { getServerLink, saveLinkClub, removeLinkClub, setEventLinkClub, replaceOnServer } =
      await import('./client.js');
    expect(await getServerLink('http://d.local:8080/', { fetch })).toEqual(link);
    expect(seen[0].url).toBe('http://d.local:8080/server-link');
    expect(seen[0].init?.method ?? 'GET').toBe('GET');

    // A new club is posted. A change is put to the club's own address.
    const settings = {
      name: 'Recon',
      url: 'https://grid.example.org',
      token: 'gfs_abc',
      trust: 'System' as const
    };
    expect(
      await saveLinkClub('http://d.local:8080', undefined, settings, 'rd-tok', { fetch })
    ).toEqual(link);
    expect(seen[1].url).toBe('http://d.local:8080/server-link/clubs');
    expect(seen[1].init?.method).toBe('POST');
    expect((seen[1].init?.headers as Record<string, string>).Authorization).toBe('Bearer rd-tok');
    expect(JSON.parse(seen[1].init?.body as string)).toEqual(settings);

    await saveLinkClub('http://d.local:8080', 'k 1/x', settings, undefined, { fetch });
    expect(seen[2].url).toBe('http://d.local:8080/server-link/clubs/k%201%2Fx');
    expect(seen[2].init?.method).toBe('PUT');

    await removeLinkClub('http://d.local:8080', 'k 1/x', undefined, { fetch });
    expect(seen[3].url).toBe('http://d.local:8080/server-link/clubs/k%201%2Fx');
    expect(seen[3].init?.method).toBe('DELETE');

    // An event goes to a club, or to none.
    await setEventLinkClub('http://d.local:8080', 'friday night/1', 'k1', undefined, { fetch });
    expect(seen[4].url).toBe('http://d.local:8080/server-link/events/friday%20night%2F1/club');
    expect(seen[4].init?.method).toBe('PUT');
    expect(JSON.parse(seen[4].init?.body as string)).toEqual({ club: 'k1' });
    await setEventLinkClub('http://d.local:8080', 'friday', null, undefined, { fetch });
    expect(JSON.parse(seen[5].init?.body as string)).toEqual({ club: null });

    await replaceOnServer('http://d.local:8080', 'friday night/1', undefined, { fetch });
    expect(seen[6].url).toBe('http://d.local:8080/server-link/events/friday%20night%2F1/replace');
    expect(seen[6].init?.method).toBe('POST');
  });

  it('a remote server is asked for its clubs and their events, and one event has its own base', async () => {
    const { listServerClubs, listServerEvents, serverEventBase } = await import('./client.js');
    const clubs = recorder(200, [{ id: 'club', name: 'Recon FPV' }]);
    expect(await listServerClubs('https://grid.example.org/', { fetch: clubs.fetch })).toEqual([
      { id: 'club', name: 'Recon FPV' }
    ]);
    expect(clubs.seen[0].url).toBe('https://grid.example.org/api/clubs');

    const events = recorder(200, [{ id: 'friday', name: 'Friday', created_at: 1 }]);
    await listServerEvents('https://grid.example.org', 'north club', { fetch: events.fetch });
    expect(events.seen[0].url).toBe('https://grid.example.org/api/clubs/north%20club/events');

    expect(serverEventBase('https://grid.example.org/', 'club', 'friday-night-1')).toBe(
      'https://grid.example.org/api/clubs/club/events/friday-night-1'
    );
    expect(serverEventBase('', 'a b', 'c/d')).toBe('/api/clubs/a%20b/events/c%2Fd');
  });

  it("saveLinkClub rejects with the Director's own words", async () => {
    const { fetch } = recorder(400, {
      code: 'BadRequest',
      message:
        'The address must start with https://. Only a server on this same computer may use http://'
    });
    const { saveLinkClub } = await import('./client.js');
    await expect(
      saveLinkClub(
        'http://d.local:8080',
        undefined,
        { name: '', url: 'http://x.example', token: 'gfs_abc', trust: 'System' },
        undefined,
        { fetch }
      )
    ).rejects.toThrow('The address must start with https://');
  });

  it('putAccessToken PUTs the token typed, or nothing for a new one', async () => {
    const { seen, fetch } = recorder(200, { ...view, token: 'K7QM-2XWD-9HRA' });
    const { putAccessToken } = await import('./client.js');
    const made = await putAccessToken('http://d.local:8080', {}, undefined, { fetch });
    await putAccessToken('http://d.local:8080', { token: 'sunday-race-7' }, undefined, { fetch });
    expect(made.token).toBe('K7QM-2XWD-9HRA');
    expect(seen[0].url).toBe('http://d.local:8080/network/token');
    expect(seen[0].init?.method).toBe('PUT');
    expect(JSON.parse(seen[0].init?.body as string)).toEqual({});
    expect(JSON.parse(seen[1].init?.body as string)).toEqual({ token: 'sunday-race-7' });
  });

  it('getPublicHello GETs /public/hello, with a token only when one is held', async () => {
    const hello = { version: '0.5.1', caller: 'Visitor', director_sign_in: true };
    const { seen, fetch } = recorder(200, hello);
    const { getPublicHello } = await import('./client.js');
    expect(await getPublicHello('http://d.local:8080', { fetch })).toEqual(hello);
    await getPublicHello('http://d.local:8080', { fetch, token: 'rd-tok' });
    expect(seen[0].url).toBe('http://d.local:8080/public/hello');
    expect((seen[0].init?.headers as Record<string, string> | undefined)?.Authorization).toBe(
      undefined
    );
    expect((seen[1].init?.headers as Record<string, string>).Authorization).toBe('Bearer rd-tok');
  });

  it('signInDirector POSTs the token as a bearer header and sends no body', async () => {
    const { seen, fetch } = recorder(204, undefined);
    const { signInDirector } = await import('./client.js');
    await signInDirector('http://d.local:8080', 'rd-tok', { fetch });
    expect(seen[0].url).toBe('http://d.local:8080/public/session');
    expect(seen[0].init?.method).toBe('POST');
    expect((seen[0].init?.headers as Record<string, string>).Authorization).toBe('Bearer rd-tok');
    expect(seen[0].init?.body).toBeUndefined();
  });

  it("signInDirector rejects with the Director's words for a wrong token and for too many tries", async () => {
    const { signInDirector } = await import('./client.js');
    const wrong = recorder(401, {
      code: 'Unauthorized',
      message: "That is not the director's access token."
    });
    await expect(
      signInDirector('http://d.local:8080', 'nope', { fetch: wrong.fetch })
    ).rejects.toThrow("That is not the director's access token.");
    const shut = recorder(429, {
      code: 'TooManyRequests',
      message: 'Too many wrong access tokens from this device. Wait a minute and try again.'
    });
    await expect(
      signInDirector('http://d.local:8080', 'nope', { fetch: shut.fetch })
    ).rejects.toThrow('Wait a minute and try again.');
  });

  it('signOutDirector DELETEs /public/session with no token', async () => {
    const { seen, fetch } = recorder(204, undefined);
    const { signOutDirector } = await import('./client.js');
    await signOutDirector('http://d.local:8080', { fetch });
    expect(seen[0].url).toBe('http://d.local:8080/public/session');
    expect(seen[0].init?.method).toBe('DELETE');
    expect((seen[0].init?.headers as Record<string, string> | undefined)?.Authorization).toBe(
      undefined
    );
  });
});

describe('the open event, for a visitor (docs/primitives.md §11.2)', () => {
  const recorder = (body: unknown) => {
    const seen: { url: string; init?: RequestInit }[] = [];
    const fetch: FetchLike = async (input, init) => {
      seen.push({ url: String(input), init });
      return {
        ok: true,
        status: 200,
        json: async (): Promise<unknown> => body
      } as unknown as Response;
    };
    return { seen, fetch };
  };

  it('each read goes to its public route, as a plain GET with no token', async () => {
    const client = await import('./client.js');
    const { seen, fetch } = recorder([]);
    await client.getPublicEvent('http://d.local:8081/', { fetch });
    await client.getPublicEventState('http://d.local:8081', { fetch });
    await client.listPublicPilots('http://d.local:8081', { fetch });
    await client.listPublicClasses('http://d.local:8081', { fetch });
    await client.listPublicTimers('http://d.local:8081', { fetch });
    expect(seen.map((call) => call.url)).toEqual([
      'http://d.local:8081/public/event',
      'http://d.local:8081/public/event/state',
      'http://d.local:8081/public/event/pilots',
      'http://d.local:8081/public/event/classes',
      'http://d.local:8081/public/event/timers'
    ]);
    for (const call of seen) {
      expect(call.init?.method ?? 'GET').toBe('GET');
      expect(
        (call.init?.headers as Record<string, string> | undefined)?.Authorization
      ).toBeUndefined();
      expect(call.init?.body).toBeUndefined();
    }
  });

  it('getPublicEvent gives an empty answer when no event is open', async () => {
    const { getPublicEvent } = await import('./client.js');
    expect(await getPublicEvent('http://d.local:8081', { fetch: recorder({}).fetch })).toEqual({});
  });

  it("a read with no event open rejects with the Director's words", async () => {
    const { getPublicEventState } = await import('./client.js');
    const fetch: FetchLike = async () =>
      ({
        ok: false,
        status: 404,
        json: async (): Promise<unknown> => ({
          code: 'UnknownScope',
          message: 'No event is open.'
        }),
        text: async (): Promise<string> =>
          JSON.stringify({ code: 'UnknownScope', message: 'No event is open.' })
      }) as unknown as Response;
    await expect(getPublicEventState('http://d.local:8081', { fetch })).rejects.toThrow(
      'No event is open.'
    );
  });

  it('subscribePublicChanges listens on the public feed and passes on each cursor', async () => {
    const { subscribePublicChanges } = await import('./client.js');
    const opened: string[] = [];
    let closed = 0;
    const listeners: Record<string, (event: { data: string }) => void> = {};
    class FakeSource {
      constructor(url: string) {
        opened.push(url);
      }
      addEventListener(name: string, fn: (event: { data: string }) => void) {
        listeners[name] = fn;
      }
      close() {
        closed += 1;
      }
    }
    const cursors: number[] = [];
    let errors = 0;
    const stop = subscribePublicChanges('http://d.local:8081/', (cursor) => cursors.push(cursor), {
      EventSource: FakeSource,
      onError: () => (errors += 1)
    });
    expect(opened).toEqual(['http://d.local:8081/public/event/changes']);
    listeners.cursor({ data: '12' });
    listeners.cursor({ data: 'not a number' });
    listeners.cursor({ data: '13' });
    expect(cursors).toEqual([12, 13]);
    listeners.error({ data: '' });
    expect(errors).toBe(1);
    stop();
    expect(closed).toBe(1);
  });

  it('subscribePublicChanges says so in a browser that cannot listen', async () => {
    const { subscribePublicChanges } = await import('./client.js');
    const had = (globalThis as { EventSource?: unknown }).EventSource;
    delete (globalThis as { EventSource?: unknown }).EventSource;
    try {
      expect(() => subscribePublicChanges('http://d.local:8081', () => {})).toThrow(
        'This browser cannot listen for live changes.'
      );
    } finally {
      if (had) (globalThis as { EventSource?: unknown }).EventSource = had;
    }
  });
});

describe("a pilot's sign-in (docs/primitives.md §11.3)", () => {
  const recorder = (status: number, body: unknown) => {
    const seen: { url: string; init?: RequestInit }[] = [];
    const fetch: FetchLike = async (input, init) => {
      seen.push({ url: String(input), init });
      return {
        ok: status >= 200 && status < 300,
        status,
        json: async (): Promise<unknown> => body,
        text: async (): Promise<string> => JSON.stringify(body)
      } as unknown as Response;
    };
    return { seen, fetch };
  };
  const auth = (init?: RequestInit) =>
    (init?.headers as Record<string, string> | undefined)?.Authorization;

  it("getPilotAccess GETs the event's codes, escaping the event id", async () => {
    const view = { pilots: [{ pilot: 'p-ace', code: 'ABC234', signed_in: false }] };
    const { seen, fetch } = recorder(200, view);
    const { getPilotAccess } = await import('./client.js');
    expect(await getPilotAccess('http://d.local:8080/', 'night race/1', { fetch })).toEqual(view);
    expect(seen[0].url).toBe('http://d.local:8080/events/night%20race%2F1/access');
    expect(seen[0].init?.method ?? 'GET').toBe('GET');
  });

  it('resetPilotCode POSTs with the token and escapes both ids', async () => {
    const next = { pilot: 'p ace', code: 'NEW456', signed_in: false };
    const { seen, fetch } = recorder(200, next);
    const { resetPilotCode } = await import('./client.js');
    expect(
      await resetPilotCode('http://d.local:8080', 'evt/1', 'p ace', 'rd-tok', { fetch })
    ).toEqual(next);
    expect(seen[0].url).toBe('http://d.local:8080/events/evt%2F1/access/p%20ace/reset');
    expect(seen[0].init?.method).toBe('POST');
    expect(auth(seen[0].init)).toBe('Bearer rd-tok');
    expect(seen[0].init?.body).toBeUndefined();
  });

  it("endPilotSession DELETEs the pilot's sign-in", async () => {
    const { seen, fetch } = recorder(204, undefined);
    const { endPilotSession } = await import('./client.js');
    await endPilotSession('http://d.local:8080', 'evt-1', 'p-ace', undefined, { fetch });
    expect(seen[0].url).toBe('http://d.local:8080/events/evt-1/access/p-ace/session');
    expect(seen[0].init?.method).toBe('DELETE');
    expect(auth(seen[0].init)).toBeUndefined();
  });

  it('pilotQrUrl is the picture address, with both ids escaped', async () => {
    const { pilotQrUrl } = await import('./client.js');
    expect(pilotQrUrl('http://d.local:8080/', 'evt/1', 'p ace')).toBe(
      'http://d.local:8080/events/evt%2F1/access/p%20ace/qr'
    );
  });

  it('signInPilot POSTs the code as JSON, with no token, and gives back the pilot', async () => {
    const { seen, fetch } = recorder(200, { id: 'p-ace', callsign: 'Ace' });
    const { signInPilot } = await import('./client.js');
    expect(await signInPilot('http://d.local:8081', 'ABC234', { fetch })).toEqual({
      id: 'p-ace',
      callsign: 'Ace'
    });
    expect(seen[0].url).toBe('http://d.local:8081/public/pilot/session');
    expect(seen[0].init?.method).toBe('POST');
    expect(JSON.parse(seen[0].init?.body as string)).toEqual({ code: 'ABC234' });
    expect(auth(seen[0].init)).toBeUndefined();
  });

  it("signInPilot rejects with the Director's words", async () => {
    const { signInPilot } = await import('./client.js');
    for (const [status, code, message] of [
      [401, 'Unauthorized', 'That code is not right. Check it with the director.'],
      [
        429,
        'TooManyRequests',
        'Too many wrong codes from this device. Wait a minute and try again.'
      ],
      [404, 'UnknownScope', 'No event is open.']
    ] as const) {
      const { fetch } = recorder(status, { code, message });
      await expect(signInPilot('http://d.local:8081', 'AAA222', { fetch })).rejects.toThrow(
        message
      );
    }
  });

  it('signOutPilot DELETEs /public/pilot/session', async () => {
    const { seen, fetch } = recorder(204, undefined);
    const { signOutPilot } = await import('./client.js');
    await signOutPilot('http://d.local:8081', { fetch });
    expect(seen[0].url).toBe('http://d.local:8081/public/pilot/session');
    expect(seen[0].init?.method).toBe('DELETE');
  });
});

describe('seat channels, for a visitor', () => {
  it('listPublicChannels GETs /public/event/channels with no token', async () => {
    const channels = [{ adapter: 'gate-x1', seat: '0', freq_mhz: 5658 }];
    const seen: { url: string; init?: RequestInit }[] = [];
    const fetch: FetchLike = async (input, init) => {
      seen.push({ url: String(input), init });
      return {
        ok: true,
        status: 200,
        json: async (): Promise<unknown> => channels
      } as unknown as Response;
    };
    const { listPublicChannels } = await import('./client.js');
    expect(await listPublicChannels('http://d.local:8081/', { fetch })).toEqual(channels);
    expect(seen[0].url).toBe('http://d.local:8081/public/event/channels');
    expect(seen[0].init?.method ?? 'GET').toBe('GET');
    expect(
      (seen[0].init?.headers as Record<string, string> | undefined)?.Authorization
    ).toBeUndefined();
  });
});

describe("a pilot's own sign-up (docs/primitives.md §11.5)", () => {
  const recorder = (status: number, body: unknown) => {
    const seen: { url: string; init?: RequestInit }[] = [];
    const fetch: FetchLike = async (input, init) => {
      seen.push({ url: String(input), init });
      return {
        ok: status >= 200 && status < 300,
        status,
        json: async (): Promise<unknown> => body,
        text: async (): Promise<string> => JSON.stringify(body)
      } as unknown as Response;
    };
    return { seen, fetch };
  };
  const seat = { adapter: 'gate-x1', seat: '0' };

  it('takeSpot, takeNextSpot and leaveSpot POST to their routes with no token and no pilot', async () => {
    const client = await import('./client.js');
    const { seen, fetch } = recorder(200, { cursor: 12 });
    expect(await client.takeSpot('http://d.local:8081/', 'quali/heat-1', seat, { fetch })).toEqual({
      cursor: 12
    });
    await client.takeNextSpot('http://d.local:8081', 'quali', seat, { fetch });
    await client.leaveSpot('http://d.local:8081', 'quali/heat-1', { fetch });
    expect(seen.map((call) => [call.init?.method, call.url])).toEqual([
      ['POST', 'http://d.local:8081/public/pilot/signup/take'],
      ['POST', 'http://d.local:8081/public/pilot/signup/next'],
      ['POST', 'http://d.local:8081/public/pilot/signup/leave']
    ]);
    expect(seen.map((call) => JSON.parse(call.init?.body as string))).toEqual([
      { heat: 'quali/heat-1', seat },
      { phase: 'quali', seat },
      { heat: 'quali/heat-1' }
    ]);
    for (const call of seen) {
      expect(
        (call.init?.headers as Record<string, string> | undefined)?.Authorization
      ).toBeUndefined();
    }
  });

  it("each rejects with the Director's reason when it refuses", async () => {
    const client = await import('./client.js');
    const refused = recorder(409, {
      code: 'Refused',
      message: 'that seat in Qualifying Heat 1 is already taken'
    });
    await expect(
      client.takeSpot('http://d.local:8081', 'quali/heat-1', seat, { fetch: refused.fetch })
    ).rejects.toThrow('that seat in Qualifying Heat 1 is already taken');
    const signedOut = recorder(401, {
      code: 'Unauthorized',
      message: 'Sign in with your code first.'
    });
    await expect(
      client.leaveSpot('http://d.local:8081', 'quali/heat-1', { fetch: signedOut.fetch })
    ).rejects.toThrow('Sign in with your code first.');
  });
});

describe('the kiosk (docs/primitives.md §11.4)', () => {
  const recorder = (status: number, body: unknown) => {
    const seen: { url: string; init?: RequestInit }[] = [];
    const fetch: FetchLike = async (input, init) => {
      seen.push({ url: String(input), init });
      return {
        ok: status >= 200 && status < 300,
        status,
        json: async (): Promise<unknown> => body,
        text: async (): Promise<string> => JSON.stringify(body)
      } as unknown as Response;
    };
    return { seen, fetch };
  };
  const header = (init?: RequestInit) =>
    (init?.headers as Record<string, string> | undefined)?.['X-GridFPV-Kiosk'];
  const seat = { adapter: 'gate-x1', seat: '0' };

  it('kioskIn POSTs the code and answers the pilot and the pass', async () => {
    const client = await import('./client.js');
    const pass = { pilot: { id: 'p-1', callsign: 'Ace' }, pass: 'pass-1', idle_seconds: 120 };
    const { seen, fetch } = recorder(200, pass);
    expect(await client.kioskIn('http://d.local:8081/', 'ABC234', { fetch })).toEqual(pass);
    expect(seen[0].url).toBe('http://d.local:8081/public/pilot/kiosk');
    expect(seen[0].init?.method).toBe('POST');
    expect(JSON.parse(seen[0].init?.body as string)).toEqual({ code: 'ABC234' });
    expect(header(seen[0].init)).toBeUndefined();
  });

  it('kioskIn rejects with the reason and the status when the code is wrong or held up', async () => {
    const client = await import('./client.js');
    for (const [status, code, message] of [
      [401, 'Unauthorized', 'That code is not right. Check it with the director.'],
      [
        429,
        'TooManyRequests',
        'Too many wrong codes from this device. Wait a minute and try again.'
      ]
    ] as const) {
      const { fetch } = recorder(status, { code, message });
      await expect(
        client.kioskIn('http://d.local:8081', 'ZZZZZZ', { fetch })
      ).rejects.toMatchObject({
        message,
        status
      });
    }
  });

  it('kioskDone DELETEs with the pass in its header and reads no body', async () => {
    const client = await import('./client.js');
    const { seen, fetch } = recorder(204, undefined);
    await expect(
      client.kioskDone('http://d.local:8081', 'pass-1', { fetch })
    ).resolves.toBeUndefined();
    expect(seen[0].url).toBe('http://d.local:8081/public/pilot/kiosk');
    expect(seen[0].init?.method).toBe('DELETE');
    expect(header(seen[0].init)).toBe('pass-1');
    expect(seen[0].init?.body).toBeUndefined();
  });

  it('the sign-up calls carry a pass only when given one, and never in the body', async () => {
    const client = await import('./client.js');
    const { seen, fetch } = recorder(200, { cursor: 3 });
    await client.takeSpot('http://d.local:8081', 'quali/heat-1', seat, { fetch, kiosk: 'pass-1' });
    await client.takeNextSpot('http://d.local:8081', 'quali', seat, { fetch, kiosk: 'pass-1' });
    await client.leaveSpot('http://d.local:8081', 'quali/heat-1', { fetch, kiosk: 'pass-1' });
    await client.takeSpot('http://d.local:8081', 'quali/heat-1', seat, { fetch });
    await client.leaveSpot('http://d.local:8081', 'quali/heat-1', { fetch, kiosk: '' });
    expect(seen.map((call) => header(call.init))).toEqual([
      'pass-1',
      'pass-1',
      'pass-1',
      undefined,
      undefined
    ]);
    for (const call of seen) {
      expect(call.url).not.toContain('pass-1');
      expect(call.init?.body as string).not.toContain('pass-1');
    }
  });

  it('a refused request at a kiosk carries its status, so the page can tell a lost pass from a rule', async () => {
    const client = await import('./client.js');
    const lost = recorder(401, { code: 'Unauthorized', message: 'Sign in with your code first.' });
    await expect(
      client.takeSpot('http://d.local:8081', 'quali/heat-1', seat, {
        fetch: lost.fetch,
        kiosk: 'old'
      })
    ).rejects.toMatchObject({ status: 401 });
    const rule = recorder(409, { code: 'Refused', message: 'no runs left' });
    await expect(
      client.takeSpot('http://d.local:8081', 'quali/heat-1', seat, {
        fetch: rule.fetch,
        kiosk: 'p'
      })
    ).rejects.toMatchObject({ status: 409, message: 'no runs left' });
  });
});

describe('setting kiosks up (docs/primitives.md §11.4)', () => {
  const recorder = (status: number, body: unknown) => {
    const seen: { url: string; init?: RequestInit }[] = [];
    const fetch: FetchLike = async (input, init) => {
      seen.push({ url: String(input), init });
      return {
        ok: status >= 200 && status < 300,
        status,
        json: async (): Promise<unknown> => body,
        text: async (): Promise<string> => JSON.stringify(body)
      } as unknown as Response;
    };
    return { seen, fetch };
  };
  const auth = (init?: RequestInit) =>
    (init?.headers as Record<string, string> | undefined)?.Authorization;
  const VIEW = { codes: ['ABCD2345'], kiosks: [{ number: 2, set_up_at: 1794800000 }] };

  it("the director's four calls go to their routes, with the token when one is held", async () => {
    const client = await import('./client.js');
    const { seen, fetch } = recorder(200, VIEW);
    expect(await client.getKiosks('http://d.local:8080/', { token: 't', fetch })).toEqual(VIEW);
    expect(await client.newKioskCode('http://d.local:8080', 't', { fetch })).toEqual(VIEW);
    expect(await client.cancelKioskCode('http://d.local:8080', 'ABCD2345', 't', { fetch })).toEqual(
      VIEW
    );
    expect(await client.removeKiosk('http://d.local:8080', 2, 't', { fetch })).toEqual(VIEW);
    expect(seen.map((call) => [call.init?.method ?? 'GET', call.url])).toEqual([
      ['GET', 'http://d.local:8080/kiosk'],
      ['POST', 'http://d.local:8080/kiosk/codes'],
      ['DELETE', 'http://d.local:8080/kiosk/codes/ABCD2345'],
      ['DELETE', 'http://d.local:8080/kiosk/devices/2']
    ]);
    for (const call of seen) expect(auth(call.init)).toBe('Bearer t');
    // With no token held, none is sent.
    const open = recorder(200, VIEW);
    await client.newKioskCode('http://d.local:8080', undefined, { fetch: open.fetch });
    expect(auth(open.seen[0].init)).toBeUndefined();
  });

  it('renameKiosk PUTs the name to the kiosk, and an empty name is still sent', async () => {
    const client = await import('./client.js');
    const { seen, fetch } = recorder(200, VIEW);
    expect(
      await client.renameKiosk('http://d.local:8080/', 2, 'By the gate', 't', { fetch })
    ).toEqual(VIEW);
    await client.renameKiosk('http://d.local:8080', 2, '', undefined, { fetch });
    expect(seen.map((call) => [call.init?.method, call.url, call.init?.body])).toEqual([
      ['PUT', 'http://d.local:8080/kiosk/devices/2', '{"name":"By the gate"}'],
      ['PUT', 'http://d.local:8080/kiosk/devices/2', '{"name":""}']
    ]);
    expect(auth(seen[0].init)).toBe('Bearer t');
    expect(auth(seen[1].init)).toBeUndefined();
    const gone = recorder(404, {
      code: 'UnknownScope',
      message: 'That kiosk is not on the list any more.'
    });
    await expect(
      client.renameKiosk('http://d.local:8080', 9, 'x', undefined, { fetch: gone.fetch })
    ).rejects.toMatchObject({ status: 404, message: 'That kiosk is not on the list any more.' });
  });

  it('a code with odd characters in it cannot change the route', async () => {
    const client = await import('./client.js');
    const { seen, fetch } = recorder(200, VIEW);
    await client.cancelKioskCode('http://d.local:8080', '../devices/1?x=#', undefined, { fetch });
    expect(seen[0].url).toBe('http://d.local:8080/kiosk/codes/..%2Fdevices%2F1%3Fx%3D%23');
  });

  it('kioskSetUp POSTs the code with no token and reads no body', async () => {
    const client = await import('./client.js');
    const { seen, fetch } = recorder(204, undefined);
    await expect(
      client.kioskSetUp('http://d.local:8081/', 'ABCD2345', { fetch })
    ).resolves.toBeUndefined();
    expect(seen[0].url).toBe('http://d.local:8081/public/pilot/kiosk/device');
    expect(seen[0].init?.method).toBe('POST');
    expect(JSON.parse(seen[0].init?.body as string)).toEqual({ code: 'ABCD2345' });
    expect(auth(seen[0].init)).toBeUndefined();
  });

  it("each rejects with the Director's reason and the status", async () => {
    const client = await import('./client.js');
    const used = recorder(401, {
      code: 'Unauthorized',
      message: 'That kiosk code is not right, or it has been used.'
    });
    await expect(
      client.kioskSetUp('http://d.local:8081', 'ZZZZZZZZ', { fetch: used.fetch })
    ).rejects.toMatchObject({
      status: 401,
      message: 'That kiosk code is not right, or it has been used.'
    });
    const full = recorder(409, {
      code: 'Refused',
      message: 'There are already a lot of kiosk codes waiting.'
    });
    await expect(
      client.newKioskCode('http://d.local:8080', undefined, { fetch: full.fetch })
    ).rejects.toMatchObject({ status: 409 });
    const notKiosk = recorder(403, {
      code: 'Forbidden',
      message: 'This device is not set up as a kiosk.'
    });
    await expect(
      client.kioskIn('http://d.local:8081', 'ABC234', { fetch: notKiosk.fetch })
    ).rejects.toMatchObject({ status: 403 });
  });
});
