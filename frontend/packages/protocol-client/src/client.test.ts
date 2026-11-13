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
