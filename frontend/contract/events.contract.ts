/**
 * Seam 9 (issue #72): events are first-class containers — the events lifecycle API and the
 * event-rooted surface.
 *
 * guards:
 *  - `GET /events` lists the events, with the built-in **Practice** event always present and
 *    listed first (in-memory, non-persistent).
 *  - `POST /events` is RD-gated (no/bad token → 401), auto-generates a unique `id` from the
 *    display `name` (the id is never user-supplied), and returns the new event's `EventMeta`.
 *  - the new event is immediately reachable: a snapshot under `/events/{id}/snapshot/...` is a
 *    200, and a control command under `/events/{id}/control` acks — against THAT event's own
 *    log, independent of Practice (a heat scheduled in one is not visible in the other).
 *  - an unknown event id → a typed `ProtocolError` 404 (`UnknownScope`), the same shape an
 *    unknown heat/pilot gets.
 *
 * Everything drives the real Director over the real wire — no mocks.
 */
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import type {
  Class,
  Command,
  EventMeta,
  FormatSchema,
  Pilot,
  RoundDef,
  Timer
} from '@gridfpv/types';

import { listHeats } from '../packages/protocol-client/dist/index.js';
import { type Director } from '../test-harness/director.ts';
import { eventRoot, startContractDirector } from './harness.ts';

const TOKEN = 'rd-events-contract';

let director: Director;

beforeAll(async () => {
  // Configure a data dir so created events are genuinely persistent (a SQLite file per
  // event) — the local realization of the per-event log (#72).
  const dataDir = mkdtempSync(join(tmpdir(), 'gridfpv-events-data-'));
  director = await startContractDirector({
    token: TOKEN,
    simLaps: 1,
    simLapMs: 40,
    env: { GRIDFPV_DATA_DIR: dataDir }
  });
});

afterAll(async () => {
  await director?.stop();
});

/** `GET /events` → the parsed `EventMeta[]` (asserting a 200). */
async function listEvents(): Promise<EventMeta[]> {
  const res = await fetch(`${director.baseUrl}/events`);
  expect(res.status).toBe(200);
  return (await res.json()) as EventMeta[];
}

/** `POST /events` with an optional bearer token → the raw status + parsed body. */
async function createEvent(
  name: string,
  token?: string
): Promise<{ status: number; body: unknown }> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' };
  if (token !== undefined) headers.Authorization = `Bearer ${token}`;
  const res = await fetch(`${director.baseUrl}/events`, {
    method: 'POST',
    headers,
    body: JSON.stringify({ name })
  });
  let body: unknown;
  try {
    body = await res.json();
  } catch {
    body = undefined;
  }
  return { status: res.status, body };
}

/** `DELETE /events/{id}` with an optional bearer token → the raw status + parsed body. */
async function deleteEvent(id: string, token?: string): Promise<{ status: number; body: unknown }> {
  const headers: Record<string, string> = {};
  if (token !== undefined) headers.Authorization = `Bearer ${token}`;
  const res = await fetch(`${director.baseUrl}/events/${encodeURIComponent(id)}`, {
    method: 'DELETE',
    headers
  });
  let body: unknown;
  try {
    body = await res.json();
  } catch {
    body = undefined;
  }
  return { status: res.status, body };
}

describe('seam 9: events lifecycle API', () => {
  it('GET /events lists the built-in Practice event first (in-memory, non-persistent)', async () => {
    const events = await listEvents();
    expect(events.length).toBeGreaterThanOrEqual(1);
    const first = events[0];
    expect(first.id).toBe('practice');
    expect(first.name).toBe('Practice');
    expect(first.persistent).toBe(false);
    // created_at is a plain JSON number (the i64 → number contract), never a bigint/string.
    expect(typeof first.created_at).toBe('number');
  });

  it('POST /events requires the RD token — no/bad token → 401', async () => {
    const anon = await createEvent('No Auth');
    expect(anon.status).toBe(401);
    const bad = await createEvent('Bad Auth', 'not-a-real-token');
    expect(bad.status).toBe(401);
  });

  it('POST /events auto-generates a unique id from the name and returns its EventMeta', async () => {
    const a = (await createEvent('Spring Cup 2026!', TOKEN)).body as EventMeta;
    const b = (await createEvent('Spring Cup 2026!', TOKEN)).body as EventMeta;
    // The id is server-generated (a name slug + suffix), never the verbatim name; two events
    // with the same name get distinct ids.
    expect(a.id).toMatch(/^spring-cup-2026-/);
    expect(b.id).toMatch(/^spring-cup-2026-/);
    expect(a.id).not.toBe(b.id);
    expect(a.name).toBe('Spring Cup 2026!');
    expect(a.persistent).toBe(true);

    // The new event now appears in the listing (after Practice).
    const ids = (await listEvents()).map((e) => e.id);
    expect(ids[0]).toBe('practice');
    expect(ids).toContain(a.id);
  });

  it('a created event is reachable and independent of Practice', async () => {
    const created = (await createEvent('Race Night', TOKEN)).body as EventMeta;

    // Schedule a heat in the created event over ITS OWN control path (`/events/{id}/control`).
    const command: Command = { ScheduleHeat: { heat: 'cn-1', lineup: ['A', 'B'] } };
    const scheduled = await fetch(`${eventRoot(director.baseUrl, created.id)}/control`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${TOKEN}` },
      body: JSON.stringify(command)
    });
    expect(scheduled.status).toBe(200);
    expect(((await scheduled.json()) as { ok: boolean }).ok).toBe(true);

    // The heat resolves a 200 under the created event…
    const inCreated = await fetch(`${eventRoot(director.baseUrl, created.id)}/snapshot/heat/cn-1`);
    expect(inCreated.status).toBe(200);

    // …but is NOT visible in Practice (per-event logs are independent).
    const inPractice = await fetch(`${eventRoot(director.baseUrl)}/snapshot/heat/cn-1`);
    expect(inPractice.status).toBe(404);
  });

  it('an unknown event id → 404 ProtocolError(UnknownScope)', async () => {
    const res = await fetch(`${eventRoot(director.baseUrl, 'no-such-event')}/snapshot/event/x`);
    expect(res.status).toBe(404);
    const body = (await res.json()) as { code?: string };
    expect(body.code).toBe('UnknownScope');
  });

  it('DELETE /events/{id} permanently removes a created event and ALL its data', async () => {
    // Create an event, give it a fact (a scheduled heat), then delete it.
    const created = (await createEvent('Doomed Event', TOKEN)).body as EventMeta;
    const scheduled = await fetch(`${eventRoot(director.baseUrl, created.id)}/control`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${TOKEN}` },
      body: JSON.stringify({ ScheduleHeat: { heat: 'd-1', lineup: ['A'] } } as Command)
    });
    expect(scheduled.status).toBe(200);

    // The delete is RD-gated and a 200 with no error body.
    const del = await deleteEvent(created.id, TOKEN);
    expect(del.status).toBe(200);

    // It is gone from the listing, and every per-event surface 404s (the data is gone too).
    const ids = (await listEvents()).map((e) => e.id);
    expect(ids).not.toContain(created.id);
    const snap = await fetch(`${eventRoot(director.baseUrl, created.id)}/snapshot/event/x`);
    expect(snap.status).toBe(404);
    const heat = await fetch(`${eventRoot(director.baseUrl, created.id)}/snapshot/heat/d-1`);
    expect(heat.status).toBe(404);
  });

  it('DELETE /events/{id} is RD-gated, rejects Practice (400), and unknown ids (404)', async () => {
    // Create a fresh event to attempt an unauthenticated delete against (it must survive).
    const created = (await createEvent('Gated Delete', TOKEN)).body as EventMeta;
    expect((await deleteEvent(created.id)).status).toBe(401);
    expect((await deleteEvent(created.id, 'not-a-real-token')).status).toBe(401);
    // Still present after the rejected deletes.
    expect((await listEvents()).map((e) => e.id)).toContain(created.id);

    // The built-in Practice cannot be deleted → a typed BadRequest (400).
    const practice = await deleteEvent('practice', TOKEN);
    expect(practice.status).toBe(400);
    expect((practice.body as { code?: string }).code).toBe('BadRequest');
    expect((await listEvents())[0].id).toBe('practice');

    // An unknown id → a typed 404 (UnknownScope).
    const unknown = await deleteEvent('no-such-event', TOKEN);
    expect(unknown.status).toBe(404);
    expect((unknown.body as { code?: string }).code).toBe('UnknownScope');
  });
});

/**
 * Issue #90: the **active event is Director (server-side) state** — a reload/reconnect resumes
 * into the selected event instead of dropping to the picker.
 *
 * guards:
 *  - `GET /active-event` is an **open read** → `{ event: EventMeta | null }`; a fresh Director
 *    has no active event (`null`).
 *  - `PUT /active-event` is **RD-gated** (no/bad token → 401), validates the id names a known
 *    event (unknown → 404 `UnknownScope`), and on success persists it: a subsequent open
 *    `GET /active-event` resolves the same event — the resume semantics every client reads.
 */
describe('seam 9b: the Director active event (#90)', () => {
  /** `GET /active-event` → the parsed `{ event }` body (asserting a 200, open read). */
  async function getActive(): Promise<{ event: EventMeta | null }> {
    const res = await fetch(`${director.baseUrl}/active-event`);
    expect(res.status).toBe(200);
    return (await res.json()) as { event: EventMeta | null };
  }

  /** `PUT /active-event` with `{ id }` and an optional bearer token → raw status + parsed body. */
  async function putActive(id: string, token?: string): Promise<{ status: number; body: unknown }> {
    const headers: Record<string, string> = { 'Content-Type': 'application/json' };
    if (token !== undefined) headers.Authorization = `Bearer ${token}`;
    const res = await fetch(`${director.baseUrl}/active-event`, {
      method: 'PUT',
      headers,
      body: JSON.stringify({ id })
    });
    let body: unknown;
    try {
      body = await res.json();
    } catch {
      body = undefined;
    }
    return { status: res.status, body };
  }

  it('PUT /active-event is RD-gated — no/bad token → 401', async () => {
    expect((await putActive('practice')).status).toBe(401);
    expect((await putActive('practice', 'not-a-real-token')).status).toBe(401);
  });

  it('PUT /active-event rejects an unknown event with 404 UnknownScope', async () => {
    const { status, body } = await putActive('no-such-event', TOKEN);
    expect(status).toBe(404);
    expect((body as { code?: string }).code).toBe('UnknownScope');
  });

  it('setting the active event makes an open GET resume into it', async () => {
    // Create a fresh persistent event, set it active (RD-gated), then read it back openly.
    const created = (await createEvent('Resume Me', TOKEN)).body as EventMeta;
    const set = await putActive(created.id, TOKEN);
    expect(set.status).toBe(200);
    expect((set.body as EventMeta).id).toBe(created.id);

    // The open read now resolves the same event — the resume semantics every client follows.
    const active = await getActive();
    expect(active.event?.id).toBe(created.id);

    // And switching it to Practice re-points the Director (last write wins).
    expect((await putActive('practice', TOKEN)).status).toBe(200);
    expect((await getActive()).event?.id).toBe('practice');
  });
});

/**
 * Issue #73: timers are **application-level configuration** — a persisted registry the RD
 * configures once, and each event selects which timers to use.
 *
 * guards:
 *  - `GET /timers` is an **open read** → `Timer[]` with the built-in **Mock** first.
 *  - `POST /timers` is **RD-gated** (no/bad token → 401), auto-generates an id, returns the
 *    new `Timer`, which then appears in the listing.
 *  - `PUT /events/{id}/timers` is **RD-gated**, validates each id names a known timer (unknown →
 *    404 `UnknownScope`), and on success records the selection on the event's `EventMeta.timers`.
 */
describe('seam 10: application-level timers + per-event selection (#73)', () => {
  /** `GET /timers` → the parsed `Timer[]` (asserting a 200, open read). */
  async function listTimers(): Promise<Timer[]> {
    const res = await fetch(`${director.baseUrl}/timers`);
    expect(res.status).toBe(200);
    return (await res.json()) as Timer[];
  }

  /** `POST /timers` with an optional bearer token → raw status + parsed body. */
  async function createTimer(
    body: unknown,
    token?: string
  ): Promise<{ status: number; body: unknown }> {
    const headers: Record<string, string> = { 'Content-Type': 'application/json' };
    if (token !== undefined) headers.Authorization = `Bearer ${token}`;
    const res = await fetch(`${director.baseUrl}/timers`, {
      method: 'POST',
      headers,
      body: JSON.stringify(body)
    });
    let parsed: unknown;
    try {
      parsed = await res.json();
    } catch {
      parsed = undefined;
    }
    return { status: res.status, body: parsed };
  }

  /** `PUT /events/{id}/timers` with `{ ids }` + optional token → raw status + parsed body. */
  async function setEventTimers(
    eventId: string,
    ids: string[],
    token?: string
  ): Promise<{ status: number; body: unknown }> {
    const headers: Record<string, string> = { 'Content-Type': 'application/json' };
    if (token !== undefined) headers.Authorization = `Bearer ${token}`;
    const res = await fetch(`${eventRoot(director.baseUrl, eventId)}/timers`, {
      method: 'PUT',
      headers,
      body: JSON.stringify({ ids })
    });
    let parsed: unknown;
    try {
      parsed = await res.json();
    } catch {
      parsed = undefined;
    }
    return { status: res.status, body: parsed };
  }

  it('GET /timers lists the built-in Mock first (open read)', async () => {
    const timers = await listTimers();
    expect(timers.length).toBeGreaterThanOrEqual(1);
    expect(timers[0].id).toBe('mock');
    expect(timers[0].name).toBe('Mock');
    expect('Mock' in timers[0].kind).toBe(true);
  });

  it('POST /timers requires the RD token — no/bad token → 401', async () => {
    const body = { name: 'No Auth', kind: { Mock: { laps: 1, lap_ms: 50 } } };
    expect((await createTimer(body)).status).toBe(401);
    expect((await createTimer(body, 'not-a-real-token')).status).toBe(401);
  });

  it('POST /timers creates a timer and it appears in the listing', async () => {
    const created = (
      await createTimer(
        { name: 'Field RH', kind: { Rotorhazard: { url: 'http://rh.local:5000' } } },
        TOKEN
      )
    ).body as Timer;
    expect(created.id).toMatch(/^field-rh-/);
    expect(created.status).toBe('Configured');

    const ids = (await listTimers()).map((t) => t.id);
    expect(ids[0]).toBe('mock');
    expect(ids).toContain(created.id);
  });

  it('PUT /events/{id}/timers validates ids and records the selection', async () => {
    // Create a real timer, then select it for a fresh event.
    const timer = (
      await createTimer({ name: 'Extra Sim', kind: { Mock: { laps: 1, lap_ms: 50 } } }, TOKEN)
    ).body as Timer;
    const event = (await createEvent('Timers Event', TOKEN)).body as EventMeta;

    // Selecting a known timer succeeds and the event meta reflects it.
    const ok = await setEventTimers(event.id, [timer.id], TOKEN);
    expect(ok.status).toBe(200);
    expect((ok.body as EventMeta).timers).toEqual([timer.id]);

    // Selecting an UNKNOWN timer → 404 UnknownScope.
    const bad = await setEventTimers(event.id, ['no-such-timer'], TOKEN);
    expect(bad.status).toBe(404);
    expect((bad.body as { code?: string }).code).toBe('UnknownScope');

    // RD-gated: no token → 401.
    expect((await setEventTimers(event.id, [timer.id])).status).toBe(401);
  });
});

/**
 * Issue #112: **primary/alternate timer roles + failover** — redundant timers at one gate, one
 * designated **primary**, the rest **alternates**. Only the role designation API is exercised on
 * the wire here (the single-active-source feed + failover is proven by the Rust bridge/live tests).
 *
 * guards:
 *  - a new event's `EventMeta.primary_timer` is absent (additive `#[serde(default)]`); the first
 *    selected timer is the effective primary by default.
 *  - `PUT /events/{id}/timers` accepts an optional `primary` (it must be one of `ids`), recorded on
 *    `EventMeta.primary_timer`.
 *  - `PUT /events/{id}/primary-timer` is **RD-gated**, designates a selected timer as primary,
 *    rejects a primary not in the selection (400), and `null` clears the override.
 */
describe('seam 10b: primary/alternate timer roles (#112)', () => {
  /** `PUT /events/{id}/timers` with `{ ids, primary? }` + optional token → raw status + parsed body. */
  async function setEventTimersWithPrimary(
    eventId: string,
    ids: string[],
    primary: string | undefined,
    token?: string
  ): Promise<{ status: number; body: unknown }> {
    const headers: Record<string, string> = { 'Content-Type': 'application/json' };
    if (token !== undefined) headers.Authorization = `Bearer ${token}`;
    const res = await fetch(`${eventRoot(director.baseUrl, eventId)}/timers`, {
      method: 'PUT',
      headers,
      body: JSON.stringify({ ids, ...(primary !== undefined ? { primary } : {}) })
    });
    let parsed: unknown;
    try {
      parsed = await res.json();
    } catch {
      parsed = undefined;
    }
    return { status: res.status, body: parsed };
  }

  /** `PUT /events/{id}/primary-timer` with `{ id }` (or `{}`) + optional token → status + body. */
  async function setPrimaryTimer(
    eventId: string,
    id: string | null | undefined,
    token?: string
  ): Promise<{ status: number; body: unknown }> {
    const headers: Record<string, string> = { 'Content-Type': 'application/json' };
    if (token !== undefined) headers.Authorization = `Bearer ${token}`;
    const res = await fetch(`${eventRoot(director.baseUrl, eventId)}/primary-timer`, {
      method: 'PUT',
      headers,
      body: JSON.stringify(id === undefined ? {} : { id })
    });
    let parsed: unknown;
    try {
      parsed = await res.json();
    } catch {
      parsed = undefined;
    }
    return { status: res.status, body: parsed };
  }

  /** Create two Mock timers + an event selecting both, returning their ids. */
  async function eventWithTwoTimers(): Promise<{ event: string; a: string; b: string }> {
    const create = async (name: string) => {
      const res = await fetch(`${director.baseUrl}/timers`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${TOKEN}` },
        body: JSON.stringify({ name, kind: { Mock: { laps: 1, lap_ms: 50 } } })
      });
      return ((await res.json()) as Timer).id;
    };
    const a = await create('Primary Sim');
    const b = await create('Alternate Sim');
    const event = (await createEvent('Redundant Timers', TOKEN)).body as EventMeta;
    return { event: event.id, a, b };
  }

  it('a new event has no explicit primary (additive default)', async () => {
    const event = (await createEvent('No Primary Yet', TOKEN)).body as EventMeta;
    expect(event.primary_timer).toBeUndefined();
  });

  it('PUT /events/{id}/timers records an optional primary among the selection', async () => {
    const { event, a, b } = await eventWithTwoTimers();
    const ok = await setEventTimersWithPrimary(event, [a, b], b, TOKEN);
    expect(ok.status).toBe(200);
    const meta = ok.body as EventMeta;
    expect(meta.timers).toEqual([a, b]);
    expect(meta.primary_timer).toBe(b);

    // A primary NOT in the selection → 400 BadRequest.
    const bad = await setEventTimersWithPrimary(event, [a, b], 'mock', TOKEN);
    expect(bad.status).toBe(400);
  });

  it('PUT /events/{id}/primary-timer designates, rejects out-of-selection, and clears', async () => {
    const { event, a, b } = await eventWithTwoTimers();
    await setEventTimersWithPrimary(event, [a, b], undefined, TOKEN);

    // Designate the alternate as primary.
    const ok = await setPrimaryTimer(event, b, TOKEN);
    expect(ok.status).toBe(200);
    expect((ok.body as EventMeta).primary_timer).toBe(b);

    // A primary not in the selection → 400 BadRequest.
    const bad = await setPrimaryTimer(event, 'mock', TOKEN);
    expect(bad.status).toBe(400);

    // Clearing the override (`null`) → effective primary falls back to the first selected.
    const cleared = await setPrimaryTimer(event, null, TOKEN);
    expect(cleared.status).toBe(200);
    expect((cleared.body as EventMeta).primary_timer).toBeUndefined();

    // RD-gated: no token → 401.
    expect((await setPrimaryTimer(event, a)).status).toBe(401);
  });
});

/**
 * Issue #74: pilots are **application-level configuration** — a persisted directory the RD
 * maintains once, and each event builds a **roster** of which directory pilots race it. Channels
 * are a separate concern (#117) and are not exercised here.
 *
 * guards:
 *  - `GET /pilots` is an **open read** → `Pilot[]` (empty on a fresh Director — no built-in pilot).
 *  - `POST /pilots` is **RD-gated** (no/bad token → 401), requires a `callsign`, auto-generates an
 *    id, carries optional metadata incl. the cloud-pull `vtx_types`/`multigp_id` hooks, and the new
 *    pilot then appears in the listing.
 *  - `PUT /events/{id}/roster` is **RD-gated**, validates each id names a directory pilot (unknown →
 *    404 `UnknownScope`), and records the roster on the event's `EventMeta.roster` (empty default).
 *  - `POST`/`DELETE /events/{id}/roster/{pilotId}` add/remove a single pilot (idempotent).
 */
describe('seam 11: application-level pilots + per-event roster (#74)', () => {
  /** `GET /pilots` → the parsed `Pilot[]` (asserting a 200, open read). */
  async function listPilots(): Promise<Pilot[]> {
    const res = await fetch(`${director.baseUrl}/pilots`);
    expect(res.status).toBe(200);
    return (await res.json()) as Pilot[];
  }

  /** `POST /pilots` with an optional bearer token → raw status + parsed body. */
  async function createPilot(
    body: unknown,
    token?: string
  ): Promise<{ status: number; body: unknown }> {
    const headers: Record<string, string> = { 'Content-Type': 'application/json' };
    if (token !== undefined) headers.Authorization = `Bearer ${token}`;
    const res = await fetch(`${director.baseUrl}/pilots`, {
      method: 'POST',
      headers,
      body: JSON.stringify(body)
    });
    let parsed: unknown;
    try {
      parsed = await res.json();
    } catch {
      parsed = undefined;
    }
    return { status: res.status, body: parsed };
  }

  /** `PUT /events/{id}/roster` with `{ pilot_ids }` + optional token → raw status + parsed body. */
  async function setRoster(
    eventId: string,
    pilotIds: string[],
    token?: string
  ): Promise<{ status: number; body: unknown }> {
    const headers: Record<string, string> = { 'Content-Type': 'application/json' };
    if (token !== undefined) headers.Authorization = `Bearer ${token}`;
    const res = await fetch(`${eventRoot(director.baseUrl, eventId)}/roster`, {
      method: 'PUT',
      headers,
      body: JSON.stringify({ pilot_ids: pilotIds })
    });
    let parsed: unknown;
    try {
      parsed = await res.json();
    } catch {
      parsed = undefined;
    }
    return { status: res.status, body: parsed };
  }

  it('GET /pilots is an open read (empty on a fresh Director — no built-in pilot)', async () => {
    const pilots = await listPilots();
    expect(Array.isArray(pilots)).toBe(true);
  });

  it('POST /pilots requires the RD token — no/bad token → 401', async () => {
    const body = { callsign: 'No Auth' };
    expect((await createPilot(body)).status).toBe(401);
    expect((await createPilot(body, 'not-a-real-token')).status).toBe(401);
  });

  it('POST /pilots creates a pilot (with metadata) and it appears in the listing', async () => {
    const created = (
      await createPilot(
        {
          callsign: 'Acro Ace',
          name: 'Ada Ace',
          phonetic: 'AK-ro AYS',
          team: 'Team Zoom',
          color: '#1188ff',
          country: 'gb', // normalized uppercase by the server
          vtx_types: ['Analog', 'HDZero'],
          multigp_id: 'mgp-42'
        },
        TOKEN
      )
    ).body as Pilot;
    expect(created.id).toMatch(/^acro-ace-/);
    expect(created.callsign).toBe('Acro Ace');
    expect(created.name).toBe('Ada Ace');
    expect(created.phonetic).toBe('AK-ro AYS');
    expect(created.team).toBe('Team Zoom');
    expect(created.color).toBe('#1188FF'); // hex normalized uppercase
    expect(created.country).toBe('GB'); // ISO alpha-2, uppercased
    expect(created.vtx_types).toEqual(['Analog', 'HDZero']);
    expect(created.multigp_id).toBe('mgp-42');

    const ids = (await listPilots()).map((p) => p.id);
    expect(ids).toContain(created.id);
  });

  it('POST /pilots rejects a missing/blank callsign with 400', async () => {
    expect((await createPilot({ callsign: '   ' }, TOKEN)).status).toBe(400);
  });

  it('POST /pilots validates color (hex) / country (2-letter) → 400', async () => {
    // Bad hex color.
    expect((await createPilot({ callsign: 'BadColor', color: 'red' }, TOKEN)).status).toBe(400);
    // Bad country (not a 2-letter code).
    expect((await createPilot({ callsign: 'BadCountry', country: 'USA' }, TOKEN)).status).toBe(400);
  });

  it('PUT /pilots/{id} edits the new fields (set / clear / leave-unchanged)', async () => {
    const created = (
      await createPilot({ callsign: 'Editable', color: '#abcdef', country: 'de' }, TOKEN)
    ).body as Pilot;

    const put = async (id: string, body: unknown, token?: string) => {
      const headers: Record<string, string> = { 'Content-Type': 'application/json' };
      if (token !== undefined) headers.Authorization = `Bearer ${token}`;
      const res = await fetch(`${director.baseUrl}/pilots/${id}`, {
        method: 'PUT',
        headers,
        body: JSON.stringify(body)
      });
      return { status: res.status, body: (await res.json().catch(() => undefined)) as unknown };
    };

    // Set team/phonetic, change country (normalized), leave color untouched (an absent field is
    // left unchanged).
    const updated = (
      await put(
        created.id,
        {
          team: 'Solo',
          phonetic: 'ED it uh bull',
          country: 'us' // set → normalized uppercase
        },
        TOKEN
      )
    ).body as Pilot;
    expect(updated.team).toBe('Solo');
    expect(updated.phonetic).toBe('ED it uh bull');
    expect(updated.country).toBe('US');
    expect(updated.color).toBe('#ABCDEF'); // unchanged (absent in the body)

    // A bad-hex color on update → 400 (and leaves the pilot untouched).
    expect((await put(created.id, { color: 'nope' }, TOKEN)).status).toBe(400);
    expect(((await put(created.id, {}, TOKEN)).body as Pilot).color).toBe('#ABCDEF');

    // Three-state `OptionalEdit` over the wire, proven on `color`/`country` — the fields whose
    // `#hex` / 2-letter validation rejects an empty string, so a wire `null` is the *only* way to
    // clear them. (Before the fix, a wire `null` deserialized the same as an absent field and these
    // could never be cleared.)
    //   1. present value → set (here a fresh, valid color/country).
    const setBoth = (await put(created.id, { color: '#001122', country: 'gb' }, TOKEN))
      .body as Pilot;
    expect(setBoth.color).toBe('#001122');
    expect(setBoth.country).toBe('GB');
    //   2. present `null` → clear (the case that was broken). `team` absent → left untouched.
    const clearedBoth = (await put(created.id, { color: null, country: null }, TOKEN))
      .body as Pilot;
    expect(clearedBoth.color).toBeUndefined(); // cleared (omitted from the wire when unset)
    expect(clearedBoth.country).toBeUndefined(); // cleared
    expect(clearedBoth.team).toBe('Solo'); // absent in this body → unchanged
    //   3. absent → leave unchanged (a no-op body leaves the now-cleared fields cleared).
    const leftAlone = (await put(created.id, {}, TOKEN)).body as Pilot;
    expect(leftAlone.color).toBeUndefined();
    expect(leftAlone.country).toBeUndefined();

    // RD-gated.
    expect((await put(created.id, { team: 'X' })).status).toBe(401);
  });

  it('PUT /events/{id}/roster validates ids and records the roster (empty default)', async () => {
    // A new event has an empty roster by default.
    const event = (await createEvent('Roster Event', TOKEN)).body as EventMeta;
    expect(event.roster).toEqual([]);

    // Create a directory pilot and roster it.
    const pilot = (await createPilot({ callsign: 'Roster Me' }, TOKEN)).body as Pilot;
    const ok = await setRoster(event.id, [pilot.id], TOKEN);
    expect(ok.status).toBe(200);
    expect((ok.body as EventMeta).roster).toEqual([pilot.id]);

    // An UNKNOWN pilot id → 404 UnknownScope.
    const bad = await setRoster(event.id, ['no-such-pilot'], TOKEN);
    expect(bad.status).toBe(404);
    expect((bad.body as { code?: string }).code).toBe('UnknownScope');

    // RD-gated: no token → 401.
    expect((await setRoster(event.id, [pilot.id])).status).toBe(401);
  });

  it('POST/DELETE /events/{id}/roster/{pilotId} add and remove one pilot (idempotent)', async () => {
    const event = (await createEvent('Add Remove Event', TOKEN)).body as EventMeta;
    const a = (await createPilot({ callsign: 'Pilot A' }, TOKEN)).body as Pilot;
    const b = (await createPilot({ callsign: 'Pilot B' }, TOKEN)).body as Pilot;

    const addOne = async (pilotId: string, token?: string) => {
      const headers: Record<string, string> = {};
      if (token !== undefined) headers.Authorization = `Bearer ${token}`;
      const res = await fetch(`${eventRoot(director.baseUrl, event.id)}/roster/${pilotId}`, {
        method: 'POST',
        headers
      });
      return { status: res.status, body: (await res.json().catch(() => undefined)) as unknown };
    };
    const removeOne = async (pilotId: string) => {
      const res = await fetch(`${eventRoot(director.baseUrl, event.id)}/roster/${pilotId}`, {
        method: 'DELETE',
        headers: { Authorization: `Bearer ${TOKEN}` }
      });
      return { status: res.status, body: (await res.json().catch(() => undefined)) as unknown };
    };

    // RD-gated.
    expect((await addOne(a.id)).status).toBe(401);

    // Add two; adding a is idempotent.
    expect(((await addOne(a.id, TOKEN)).body as EventMeta).roster).toEqual([a.id]);
    expect(((await addOne(b.id, TOKEN)).body as EventMeta).roster).toEqual([a.id, b.id]);
    expect(((await addOne(a.id, TOKEN)).body as EventMeta).roster).toEqual([a.id, b.id]);

    // Unknown pilot add → 404.
    expect((await addOne('no-such-pilot', TOKEN)).status).toBe(404);

    // Remove one; removing an absent one is a no-op.
    expect(((await removeOne(a.id)).body as EventMeta).roster).toEqual([b.id]);
    expect(((await removeOne(a.id)).body as EventMeta).roster).toEqual([b.id]);
  });
});

/**
 * Issue #84 (registry slice): classes are **application-level configuration** — a persisted
 * directory the RD maintains once, and each event selects which directory classes run at it. The
 * directory mirrors the pilot directory (a `ClassSource` provenance + optional reference/description;
 * the `ClassId`/`OptionalEdit` types are reused). Rounds / the phase engine are NOT in this slice.
 *
 * guards:
 *  - `GET /classes` is an **open read** → `Class[]` carrying the 9 locked, fixed-id **built-in**
 *    classes (present on every Director, flagged `builtin`, carrying their real org as `source`).
 *  - `POST /classes` is **RD-gated** (no/bad token → 401), requires a `name`, auto-generates an id,
 *    carries the `source`/`reference`/`description` metadata, and the new class appears in the listing.
 *  - `PUT/DELETE /classes/{id}` on a **built-in** id is rejected (read-only); user/Custom classes are
 *    full CRUD (set / clear via wire-`null` — the `OptionalEdit` three-state).
 *  - `PUT /events/{id}/classes` is **RD-gated**, validates each id names a directory class (unknown →
 *    404 `UnknownScope`), and records the selection on the event's `EventMeta.classes` (empty default);
 *    a built-in id is selectable like any class.
 */
describe('seam 12: application-level classes + per-event selection (#84)', () => {
  /** `GET /classes` → the parsed `Class[]` (asserting a 200, open read). */
  async function listClasses(): Promise<Class[]> {
    const res = await fetch(`${director.baseUrl}/classes`);
    expect(res.status).toBe(200);
    return (await res.json()) as Class[];
  }

  /** `POST /classes` with an optional bearer token → raw status + parsed body. */
  async function createClass(
    body: unknown,
    token?: string
  ): Promise<{ status: number; body: unknown }> {
    const headers: Record<string, string> = { 'Content-Type': 'application/json' };
    if (token !== undefined) headers.Authorization = `Bearer ${token}`;
    const res = await fetch(`${director.baseUrl}/classes`, {
      method: 'POST',
      headers,
      body: JSON.stringify(body)
    });
    let parsed: unknown;
    try {
      parsed = await res.json();
    } catch {
      parsed = undefined;
    }
    return { status: res.status, body: parsed };
  }

  /** `PUT /events/{id}/classes` with `{ ids }` + optional token → raw status + parsed body. */
  async function setEventClasses(
    eventId: string,
    ids: string[],
    token?: string
  ): Promise<{ status: number; body: unknown }> {
    const headers: Record<string, string> = { 'Content-Type': 'application/json' };
    if (token !== undefined) headers.Authorization = `Bearer ${token}`;
    const res = await fetch(`${eventRoot(director.baseUrl, eventId)}/classes`, {
      method: 'PUT',
      headers,
      body: JSON.stringify({ ids })
    });
    let parsed: unknown;
    try {
      parsed = await res.json();
    } catch {
      parsed = undefined;
    }
    return { status: res.status, body: parsed };
  }

  it('GET /classes is an open read carrying the 9 fixed-id built-ins (org-sourced, flagged builtin)', async () => {
    const classes = await listClasses();
    expect(Array.isArray(classes)).toBe(true);

    // The 9 canonical built-ins are present with their fixed ids — identical on every Director.
    const byId = new Map(classes.map((c) => [c.id, c]));
    const fixedIds = [
      'mgp-open',
      'mgp-pro-spec',
      'mgp-whoop',
      'mgp-micro',
      'five33-tiny-trainer',
      'freedom-spec',
      'street-league',
      'udl-igniter',
      'udl-shrieker'
    ];
    for (const id of fixedIds) {
      const cls = byId.get(id);
      expect(cls, `built-in ${id} present`).toBeDefined();
      expect(cls?.builtin).toBe(true);
    }
    // They carry their real org as the source (a badge), not Custom.
    expect(byId.get('mgp-open')?.source).toBe('MultiGP');
    expect(byId.get('five33-tiny-trainer')?.source).toBe('Five33');
    expect(byId.get('freedom-spec')?.source).toBe('FreedomSpec');
    expect(byId.get('street-league')?.source).toBe('StreetLeague');
    expect(byId.get('udl-igniter')?.source).toBe('UDL');
  });

  it('PUT/DELETE on a built-in class is rejected — built-ins are read-only', async () => {
    const put = await fetch(`${director.baseUrl}/classes/mgp-open`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${TOKEN}` },
      body: JSON.stringify({ name: 'Hacked' })
    });
    expect(put.status).toBe(400);

    const del = await fetch(`${director.baseUrl}/classes/mgp-open`, {
      method: 'DELETE',
      headers: { Authorization: `Bearer ${TOKEN}` }
    });
    expect(del.status).toBe(400);

    // The built-in is untouched.
    const stillThere = (await listClasses()).find((c) => c.id === 'mgp-open');
    expect(stillThere?.name).toBe('Open Class');
    expect(stillThere?.builtin).toBe(true);
  });

  it('POST /classes requires the RD token — no/bad token → 401', async () => {
    const body = { name: 'No Auth' };
    expect((await createClass(body)).status).toBe(401);
    expect((await createClass(body, 'not-a-real-token')).status).toBe(401);
  });

  it('POST /classes rejects a missing/blank name with 400', async () => {
    expect((await createClass({ name: '   ' }, TOKEN)).status).toBe(400);
  });

  it('POST /classes creates a class (with metadata) and it appears in the listing', async () => {
    const created = (
      await createClass(
        { name: 'Open', source: 'MultiGP', reference: 'mgp-open', description: 'The open class' },
        TOKEN
      )
    ).body as Class;
    expect(created.id).toMatch(/^open-/);
    expect(created.name).toBe('Open');
    expect(created.source).toBe('MultiGP');
    expect(created.reference).toBe('mgp-open');
    expect(created.description).toBe('The open class');

    // The source defaults to "Custom" when omitted.
    const custom = (await createClass({ name: 'Spec' }, TOKEN)).body as Class;
    expect(custom.source).toBe('Custom');

    const ids = (await listClasses()).map((c) => c.id);
    expect(ids).toContain(created.id);
  });

  it('PUT /classes/{id} edits (set, and clear via wire-null — the OptionalEdit three-state)', async () => {
    const created = (
      await createClass({ name: 'Editable', reference: 'ref-1', description: 'desc' }, TOKEN)
    ).body as Class;

    const put = async (id: string, body: unknown, token?: string) => {
      const headers: Record<string, string> = { 'Content-Type': 'application/json' };
      if (token !== undefined) headers.Authorization = `Bearer ${token}`;
      const res = await fetch(`${director.baseUrl}/classes/${id}`, {
        method: 'PUT',
        headers,
        body: JSON.stringify(body)
      });
      return { status: res.status, body: (await res.json().catch(() => undefined)) as unknown };
    };

    // Set name + source, leave reference/description untouched (absent → unchanged).
    const updated = (await put(created.id, { name: 'Renamed', source: 'Other' }, TOKEN))
      .body as Class;
    expect(updated.name).toBe('Renamed');
    expect(updated.source).toBe('Other');
    expect(updated.reference).toBe('ref-1'); // unchanged
    expect(updated.description).toBe('desc'); // unchanged

    // Wire `null` clears reference/description (the OptionalEdit clear case).
    const cleared = (await put(created.id, { reference: null, description: null }, TOKEN))
      .body as Class;
    expect(cleared.reference).toBeUndefined(); // cleared (omitted from the wire when unset)
    expect(cleared.description).toBeUndefined(); // cleared

    // Absent → leave unchanged (a no-op body leaves the now-cleared fields cleared).
    const leftAlone = (await put(created.id, {}, TOKEN)).body as Class;
    expect(leftAlone.reference).toBeUndefined();

    // RD-gated, and an unknown id → 404.
    expect((await put(created.id, { name: 'X' })).status).toBe(401);
    expect((await put('no-such-class', { name: 'X' }, TOKEN)).status).toBe(404);
  });

  it('PUT /events/{id}/classes validates ids and records the selection (empty default)', async () => {
    // A new event has an empty class selection by default.
    const event = (await createEvent('Classes Event', TOKEN)).body as EventMeta;
    expect(event.classes).toEqual([]);

    // Create a directory class and select it.
    const cls = (await createClass({ name: 'Selectable' }, TOKEN)).body as Class;
    const ok = await setEventClasses(event.id, [cls.id], TOKEN);
    expect(ok.status).toBe(200);
    expect((ok.body as EventMeta).classes).toEqual([cls.id]);

    // An UNKNOWN class id → 404 UnknownScope.
    const bad = await setEventClasses(event.id, ['no-such-class'], TOKEN);
    expect(bad.status).toBe(404);
    expect((bad.body as { code?: string }).code).toBe('UnknownScope');

    // RD-gated: no token → 401.
    expect((await setEventClasses(event.id, [cls.id])).status).toBe(401);
  });

  it('PUT /classes/{id}/hidden toggles visibility (built-ins too), RD-gated, 404 on unknown', async () => {
    const setHidden = async (id: string, hidden: boolean, token?: string) => {
      const headers: Record<string, string> = { 'Content-Type': 'application/json' };
      if (token !== undefined) headers.Authorization = `Bearer ${token}`;
      const res = await fetch(`${director.baseUrl}/classes/${id}/hidden`, {
        method: 'PUT',
        headers,
        body: JSON.stringify({ hidden })
      });
      return { status: res.status, body: (await res.json().catch(() => undefined)) as unknown };
    };

    // A built-in CAN be hidden — visibility is a preference, not a read-only edit.
    const hid = await setHidden('mgp-open', true, TOKEN);
    expect(hid.status).toBe(200);
    expect((hid.body as Class).hidden).toBe(true);
    expect((hid.body as Class).builtin).toBe(true);
    // GET /classes reflects the hidden flag.
    expect((await listClasses()).find((c) => c.id === 'mgp-open')?.hidden).toBe(true);

    // Un-hide it again.
    const shown = await setHidden('mgp-open', false, TOKEN);
    expect(shown.status).toBe(200);
    expect((shown.body as Class).hidden).toBeUndefined(); // omitted from the wire when false

    // RD-gated (no token → 401) and an unknown id → 404.
    expect((await setHidden('mgp-open', true)).status).toBe(401);
    expect((await setHidden('no-such-class', true, TOKEN)).status).toBe(404);
  });
});

/**
 * Race redesign Slice 1a: **per-class membership** — given the event's present pilots (roster) and
 * its selected classes, *which roster pilots race which class*. The membership is recorded on the
 * event's `EventMeta.classes_membership` (additive, omitted from the wire when empty).
 *
 * guards:
 *  - a new event's `EventMeta.classes_membership` is absent (additive `#[serde(default)]`,
 *    omit-when-empty).
 *  - `PUT /events/{id}/classes/{classId}/membership` is **RD-gated** (no token → 401), validates the
 *    class names a known directory class and each pilot id names a directory pilot (unknown → 404
 *    `UnknownScope`), and replaces that class's pilot list wholesale.
 *  - an empty `pilot_ids` clears the class's membership entry.
 */
describe('race Slice 1a: per-class membership', () => {
  /**
   * `PUT /events/{id}/classes/{classId}/membership` with `{ pilots }` + optional token. Each element
   * may be a bare pilot-id string (the legacy/channel-less shape, accepted by the server's serde
   * shim) or a full `{ pilot, channel? }` slot (race redesign Slice 7a).
   */
  async function setMembership(
    eventId: string,
    classId: string,
    pilots: Array<string | { pilot: string; channel?: number }>,
    token?: string
  ): Promise<{ status: number; body: unknown }> {
    const headers: Record<string, string> = { 'Content-Type': 'application/json' };
    if (token !== undefined) headers.Authorization = `Bearer ${token}`;
    const res = await fetch(
      `${eventRoot(director.baseUrl, eventId)}/classes/${classId}/membership`,
      { method: 'PUT', headers, body: JSON.stringify({ pilots }) }
    );
    let parsed: unknown;
    try {
      parsed = await res.json();
    } catch {
      parsed = undefined;
    }
    return { status: res.status, body: parsed };
  }

  /** Create a pilot, returning its id. */
  async function makePilot(callsign: string): Promise<string> {
    const res = await fetch(`${director.baseUrl}/pilots`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${TOKEN}` },
      body: JSON.stringify({ callsign })
    });
    return ((await res.json()) as Pilot).id;
  }

  it('a new event has no class membership (additive, omit-when-empty)', async () => {
    const event = (await createEvent('Membership Default', TOKEN)).body as EventMeta;
    expect(event.classes_membership).toBeUndefined();
  });

  it('PUT membership validates the class + pilots and replaces a class list wholesale', async () => {
    const event = (await createEvent('Membership Event', TOKEN)).body as EventMeta;
    const a = await makePilot('Member A');
    const b = await makePilot('Member B');

    // Set the Open built-in class's membership (bare ids → channel-less slots, via the serde shim).
    const ok = await setMembership(event.id, 'mgp-open', [a, b], TOKEN);
    expect(ok.status).toBe(200);
    const meta = ok.body as EventMeta;
    const entry = (meta.classes_membership ?? []).find((m) => m.class === 'mgp-open');
    // The wire shape is now member slots: `{ pilot, channel? }` (race redesign Slice 7a).
    expect(entry?.pilots.map((s) => s.pilot)).toEqual([a, b]);
    expect(entry?.pilots.every((s) => s.channel === undefined)).toBe(true);

    // Replacing that class's list is wholesale.
    const replaced = (await setMembership(event.id, 'mgp-open', [a], TOKEN)).body as EventMeta;
    const replacedEntry = (replaced.classes_membership ?? []).find((m) => m.class === 'mgp-open');
    expect(replacedEntry?.pilots.map((s) => s.pilot)).toEqual([a]);

    // An empty list clears the class's entry.
    const cleared = (await setMembership(event.id, 'mgp-open', [], TOKEN)).body as EventMeta;
    expect((cleared.classes_membership ?? []).some((m) => m.class === 'mgp-open')).toBe(false);

    // An UNKNOWN class id → 404 UnknownScope.
    const badClass = await setMembership(event.id, 'no-such-class', [a], TOKEN);
    expect(badClass.status).toBe(404);
    expect((badClass.body as { code?: string }).code).toBe('UnknownScope');

    // An UNKNOWN pilot id → 404 UnknownScope.
    const badPilot = await setMembership(event.id, 'mgp-open', ['no-such-pilot'], TOKEN);
    expect(badPilot.status).toBe(404);
    expect((badPilot.body as { code?: string }).code).toBe('UnknownScope');

    // RD-gated: no token → 401.
    expect((await setMembership(event.id, 'mgp-open', [a])).status).toBe(401);
  });

  it('per-pilot channels (Slice 7a) are set when valid and rejected when not in the primary timer pool', async () => {
    const event = (await createEvent('Channel Membership', TOKEN)).body as EventMeta;
    const a = await makePilot('Chan A');
    const b = await makePilot('Chan B');

    // The event's default primary is the built-in Mock, whose available channels include Raceband
    // R1/R2 (5658 / 5695). Assigning those is accepted and round-trips on the slot's `channel`.
    const ok = await setMembership(
      event.id,
      'mgp-open',
      [
        { pilot: a, channel: 5658 },
        { pilot: b, channel: 5695 }
      ],
      TOKEN
    );
    expect(ok.status).toBe(200);
    const entry = ((ok.body as EventMeta).classes_membership ?? []).find(
      (m) => m.class === 'mgp-open'
    );
    expect(entry?.pilots).toEqual([
      { pilot: a, channel: 5658 },
      { pilot: b, channel: 5695 }
    ]);

    // A channel NOT in the primary timer's available pool → 400 BadRequest (node_count does not
    // cap the channel set; only the pool membership is validated).
    const bad = await setMembership(event.id, 'mgp-open', [{ pilot: a, channel: 1234 }], TOKEN);
    expect(bad.status).toBe(400);
    expect((bad.body as { code?: string }).code).toBe('BadRequest');
  });
});

/**
 * Race redesign Slice 2a: **rounds** — event-level, class-tagged, *dynamic* format-instances. A
 * `RoundDef` scopes a `FormatRegistry` format (+ its config + win condition) to one or more eligible
 * classes, with a `SeedingRule` (default `FromRoster`; `FromRanking` is the #84 carry seam). Rounds
 * are recorded on `EventMeta.rounds` (additive, omitted from the wire when empty). The Rounds UI is
 * Slice 2b — these guard the backend wire only.
 *
 * guards:
 *  - a new event's `EventMeta.rounds` is absent (additive `#[serde(default)]`, omit-when-empty).
 *  - `POST /events/{id}/rounds` is **RD-gated** (no token → 401), auto-generates the round id, and
 *    returns the created `RoundDef`. Each class must be selected by the event, the format must be a
 *    known registry name, and a `FromRanking` source must exist (bad → 400 `BadRequest`).
 *  - `PUT /events/{id}/rounds/{roundId}` replaces the round wholesale; `DELETE` removes it; an
 *    unknown round id → 404 `UnknownScope`.
 */
describe('race Slice 2a: rounds', () => {
  /** `POST /events/{id}/rounds` with a body + optional token → raw status + parsed body. */
  async function addRound(
    eventId: string,
    body: unknown,
    token?: string
  ): Promise<{ status: number; body: unknown }> {
    const headers: Record<string, string> = { 'Content-Type': 'application/json' };
    if (token !== undefined) headers.Authorization = `Bearer ${token}`;
    const res = await fetch(`${eventRoot(director.baseUrl, eventId)}/rounds`, {
      method: 'POST',
      headers,
      body: JSON.stringify(body)
    });
    let parsed: unknown;
    try {
      parsed = await res.json();
    } catch {
      parsed = undefined;
    }
    return { status: res.status, body: parsed };
  }

  /** `PUT`/`DELETE /events/{id}/rounds/{roundId}` → raw status + parsed body. */
  async function mutateRound(
    eventId: string,
    roundId: string,
    method: 'PUT' | 'DELETE',
    body?: unknown,
    token: string = TOKEN
  ): Promise<{ status: number; body: unknown }> {
    const headers: Record<string, string> = {
      'Content-Type': 'application/json',
      Authorization: `Bearer ${token}`
    };
    const res = await fetch(
      `${eventRoot(director.baseUrl, eventId)}/rounds/${encodeURIComponent(roundId)}`,
      { method, headers, body: body === undefined ? undefined : JSON.stringify(body) }
    );
    let parsed: unknown;
    try {
      parsed = await res.json();
    } catch {
      parsed = undefined;
    }
    return { status: res.status, body: parsed };
  }

  /** Select the built-in `mgp-open` class on an event so a round may run for it. */
  async function selectOpen(eventId: string): Promise<void> {
    await fetch(`${eventRoot(director.baseUrl, eventId)}/classes`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${TOKEN}` },
      body: JSON.stringify({ ids: ['mgp-open'] })
    });
  }

  it('a new event has no rounds (additive, omit-when-empty)', async () => {
    const event = (await createEvent('Rounds Default', TOKEN)).body as EventMeta;
    expect(event.rounds).toBeUndefined();
  });

  it('POST /rounds is RD-gated — no token → 401', async () => {
    const event = (await createEvent('Rounds Auth', TOKEN)).body as EventMeta;
    await selectOpen(event.id);
    const res = await addRound(event.id, {
      label: 'Q1',
      classes: ['mgp-open'],
      format: 'timed_qual',
      win_condition: 'BestLap'
    });
    expect(res.status).toBe(401);
  });

  it('POST /rounds generates an id and records the round; PUT/DELETE edit it', async () => {
    const event = (await createEvent('Rounds CRUD', TOKEN)).body as EventMeta;
    await selectOpen(event.id);

    const created = await addRound(
      event.id,
      {
        label: 'Qualifying R1',
        classes: ['mgp-open'],
        format: 'timed_qual',
        params: {},
        win_condition: 'BestLap',
        time_limit_secs: 60
      },
      TOKEN
    );
    expect(created.status).toBe(200);
    const round = created.body as RoundDef;
    expect(round.id).toMatch(/^qualifying-r1-/);
    expect(round.label).toBe('Qualifying R1');
    expect(round.format).toBe('timed_qual');
    expect(round.seeding).toBe('FromRoster'); // default
    // Race redesign Slice 7a: channel_mode defaults by format — timed_qual → 'Static'.
    expect(round.channel_mode).toBe('Static');

    // A bracket format defaults to 'PerHeat'; an explicit channel_mode overrides the default.
    const bracket = (
      await addRound(
        event.id,
        {
          label: 'Bracket',
          classes: ['mgp-open'],
          format: 'single_elim',
          win_condition: 'BestLap',
          time_limit_secs: 60
        },
        TOKEN
      )
    ).body as RoundDef;
    expect(bracket.channel_mode).toBe('PerHeat');
    const forced = (
      await addRound(
        event.id,
        {
          label: 'Forced',
          classes: ['mgp-open'],
          format: 'timed_qual',
          channel_mode: 'PerHeat',
          win_condition: 'BestLap',
          time_limit_secs: 60
        },
        TOKEN
      )
    ).body as RoundDef;
    expect(forced.channel_mode).toBe('PerHeat');

    // The round is recorded on the event meta (re-create returns the current snapshot via list).
    const list = (await fetch(`${director.baseUrl}/events`).then((r) => r.json())) as EventMeta[];
    const meta = list.find((e) => e.id === event.id)!;
    expect((meta.rounds ?? []).map((r) => r.id)).toContain(round.id);

    // PUT replaces wholesale (id preserved).
    const updated = await mutateRound(event.id, round.id, 'PUT', {
      label: 'Open Qualifying',
      classes: ['mgp-open'],
      format: 'single_elim',
      params: { advance: '2' },
      win_condition: { FirstToLaps: { n: 5 } }
    });
    expect(updated.status).toBe(200);
    const updatedRound = updated.body as RoundDef;
    expect(updatedRound.id).toBe(round.id);
    expect(updatedRound.format).toBe('single_elim');

    // DELETE removes it; a second DELETE is a 404 UnknownScope.
    expect((await mutateRound(event.id, round.id, 'DELETE')).status).toBe(200);
    const gone = await mutateRound(event.id, round.id, 'DELETE');
    expect(gone.status).toBe(404);
    expect((gone.body as { code?: string }).code).toBe('UnknownScope');
  });

  it('POST /rounds accepts an open_practice round seeded AllChannels (open-practice format)', async () => {
    // Open practice (open-practice format, Slice 1): a round is `format: "open_practice"` +
    // `seeding: AllChannels { channels }` (node indices), with no eligible classes — it is keyed on
    // active *channels*, not pilots. The round round-trips through the meta with its seeding intact.
    const event = (await createEvent('Open Practice Round', TOKEN)).body as EventMeta;
    // No class selection needed — an open-practice round has an empty classes list.
    const created = await addRound(
      event.id,
      {
        label: 'Open Practice',
        classes: [],
        format: 'open_practice',
        params: {},
        win_condition: 'BestLap',
        seeding: { AllChannels: { channels: [0, 1, 2] } }
      },
      TOKEN
    );
    expect(created.status).toBe(200);
    const round = created.body as RoundDef;
    expect(round.format).toBe('open_practice');
    expect(round.classes).toEqual([]);
    expect(round.seeding).toEqual({ AllChannels: { channels: [0, 1, 2] } });

    // It round-trips through the event meta (the seeding + format survive the persist).
    const list = (await fetch(`${director.baseUrl}/events`).then((r) => r.json())) as EventMeta[];
    const meta = list.find((e) => e.id === event.id)!;
    const stored = (meta.rounds ?? []).find((r) => r.id === round.id)!;
    expect(stored.seeding).toEqual({ AllChannels: { channels: [0, 1, 2] } });
  });

  it('POST /rounds open_practice with NO win condition + a time limit auto-creates one heat', async () => {
    // Open-practice refinement: the request may omit `win_condition` (open practice does no scoring)
    // and carry a `time_limit_secs`; creating the round auto-creates its single channel heat (no
    // manual FillRound). Re-filling is idempotent — there is still exactly one heat for the round.
    const event = (await createEvent('Open Practice Auto', TOKEN)).body as EventMeta;
    const created = await addRound(
      event.id,
      {
        label: 'Open Practice',
        classes: [],
        format: 'open_practice',
        // No `win_condition` field at all — additive on the wire.
        seeding: { AllChannels: { channels: [0, 1] } },
        time_limit_secs: 3600
      },
      TOKEN
    );
    expect(created.status).toBe(200);
    const round = created.body as RoundDef;
    // The inert default win condition is stored; the time limit round-trips.
    expect(round.win_condition).toBe('BestLap');
    expect(round.time_limit_secs).toBe(3600);

    // The single channel heat was auto-created on round creation (no FillRound was sent).
    const heats = await listHeats(director.baseUrl, event.id);
    const forRound = heats.filter((h) => h.round === round.id);
    expect(forRound).toHaveLength(1);
    expect(forRound[0].lineup).toEqual(['node-0', 'node-1']);

    // Idempotent: re-running the round's FillRound over the control path adds no second heat.
    await fetch(`${eventRoot(director.baseUrl, event.id)}/control`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${TOKEN}` },
      body: JSON.stringify({ FillRound: { round: round.id } })
    });
    const after = (await listHeats(director.baseUrl, event.id)).filter((h) => h.round === round.id);
    expect(after).toHaveLength(1);
  });

  it('POST /rounds validates format, class selection, and seeding source → 400', async () => {
    const event = (await createEvent('Rounds Validation', TOKEN)).body as EventMeta;
    await selectOpen(event.id);

    // Unknown format → 400 BadRequest.
    const badFormat = await addRound(
      event.id,
      { label: 'X', classes: ['mgp-open'], format: 'no-such-format', win_condition: 'BestLap' },
      TOKEN
    );
    expect(badFormat.status).toBe(400);
    expect((badFormat.body as { code?: string }).code).toBe('BadRequest');

    // A directory class the event does NOT select (mgp-7 is a built-in, but only mgp-open is
    // selected here) → 400.
    const badClass = await addRound(
      event.id,
      { label: 'X', classes: ['mgp-7'], format: 'timed_qual', win_condition: 'BestLap' },
      TOKEN
    );
    expect(badClass.status).toBe(400);

    // FromRanking with a dangling source round → 400.
    const dangling = await addRound(
      event.id,
      {
        label: 'Bracket',
        classes: ['mgp-open'],
        format: 'single_elim',
        win_condition: 'BestLap',
        seeding: { FromRanking: { source_rounds: ['does-not-exist'], top_n: 4 } }
      },
      TOKEN
    );
    expect(dangling.status).toBe(400);

    // An unknown event id → 404 UnknownScope.
    const badEvent = await addRound(
      'no-such-event',
      { label: 'X', classes: ['mgp-open'], format: 'timed_qual', win_condition: 'BestLap' },
      TOKEN
    );
    expect(badEvent.status).toBe(404);
    expect((badEvent.body as { code?: string }).code).toBe('UnknownScope');
  });

  it('GET /formats is an open read of the standard formats + their param schemas (the round dropdown / params editor source)', async () => {
    // The Rounds UI sources its format dropdown AND per-format params editor here rather than
    // hard-coding the list — the single source of truth is the engine's `FormatRegistry`. An open
    // read (no token). Race redesign Slice 7a: each entry is `{ name, params: [...] }`.
    const res = await fetch(`${director.baseUrl}/formats`);
    expect(res.status).toBe(200);
    const schemas = (await res.json()) as FormatSchema[];
    // The **offered** production formats, in sorted name order, including the casual `open_practice`
    // (open-practice format) with no param knobs. ZippyQ is shelved (#218) — still registered (so
    // persisted `zippyq` rounds validate, see the `POST /rounds` accepts-registered-format guard) but
    // omitted from this offered set so a new round can't select it.
    expect(schemas.map((s) => s.name)).toEqual([
      'double_elim',
      'multi_main',
      'open_practice',
      'round_robin',
      'single_elim',
      'timed_qual'
    ]);
    expect(schemas.map((s) => s.name)).not.toContain('zippyq');
    // open_practice declares no params (its active channels are the field, via AllChannels seeding).
    expect(schemas.find((s) => s.name === 'open_practice')!.params).toEqual([]);
    // timed_qual declares `rounds` (number, default 3) relabeled "Heats per pilot". It declares NO
    // `metric` param: the qualifying metric is derived from the round's win condition (the qualifying
    // metric IS the win condition — Rounds form redesign), not a separate stored knob.
    const tq = schemas.find((s) => s.name === 'timed_qual')!;
    const rounds = tq.params.find((p) => p.key === 'rounds')!;
    expect(rounds.kind).toBe('number');
    expect(rounds.default).toBe('3');
    expect(rounds.label).toBe('Heats per pilot');
    expect(tq.params.find((p) => p.key === 'metric')).toBeUndefined();
    // round_robin likewise declares no `metric` param (its `rounds` is also "Heats per pilot").
    const rr = schemas.find((s) => s.name === 'round_robin')!;
    expect(rr.params.find((p) => p.key === 'metric')).toBeUndefined();
    expect(rr.params.find((p) => p.key === 'rounds')?.label).toBe('Heats per pilot');
    // double_elim declares a bool `bracket_reset`.
    const de = schemas.find((s) => s.name === 'double_elim')!;
    expect(de.params.find((p) => p.key === 'bracket_reset')?.kind).toBe('bool');
  });
});

/**
 * Race redesign Slice 3a: the **round-driven engine** — `Command::FillRound`. Building the round's
 * format generator from the eligible classes' membership (the field) and the round's completed heats
 * read off the log, it schedules the **next** heat tagged with the round (and the class when
 * single-class). A round whose generator has no more heats is **complete** — a successful ack, not an
 * error. The Heats UI is Slice 3b; this guards the backend wire only.
 *
 * guards:
 *  - `FillRound` over `POST /events/{id}/control` (RD-gated) acks `{ ok: true }` and the
 *    round-scheduled heat becomes snapshot-able, tagged with the round + the single class, lineup =
 *    the class membership.
 *  - `FillRound` on a round with no membership (empty field) is a well-formed `{ ok: false }`
 *    (BadRequest), NOT an HTTP error; on an unknown round it is `{ ok: false, UnknownScope }`.
 */
describe('race Slice 3a: FillRound (round-driven engine)', () => {
  /** `POST /events/{id}/control` a command with an optional token → raw status + parsed body. */
  async function control(
    eventId: string,
    command: Command,
    token?: string
  ): Promise<{ status: number; body: unknown }> {
    const headers: Record<string, string> = { 'Content-Type': 'application/json' };
    if (token !== undefined) headers.Authorization = `Bearer ${token}`;
    const res = await fetch(`${eventRoot(director.baseUrl, eventId)}/control`, {
      method: 'POST',
      headers,
      body: JSON.stringify(command)
    });
    let body: unknown;
    try {
      body = await res.json();
    } catch {
      body = undefined;
    }
    return { status: res.status, body };
  }

  /** Create a pilot, returning its id. */
  async function makePilot(callsign: string): Promise<string> {
    const res = await fetch(`${director.baseUrl}/pilots`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${TOKEN}` },
      body: JSON.stringify({ callsign })
    });
    return ((await res.json()) as Pilot).id;
  }

  /** Select `mgp-open`, set its membership to `pilotIds`, add a 1-round timed_qual; return ids. */
  async function setupQualRound(
    eventName: string,
    pilotIds: string[]
  ): Promise<{ eventId: string; roundId: string }> {
    const event = (await createEvent(eventName, TOKEN)).body as EventMeta;
    await fetch(`${eventRoot(director.baseUrl, event.id)}/classes`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${TOKEN}` },
      body: JSON.stringify({ ids: ['mgp-open'] })
    });
    await fetch(`${eventRoot(director.baseUrl, event.id)}/classes/mgp-open/membership`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${TOKEN}` },
      // Bare pilot-id elements (channel-less slots, via the serde shim) — this seam exercises the
      // per-heat path (whole-field heat + first-fit channels), so the round is forced PerHeat.
      body: JSON.stringify({ pilots: pilotIds })
    });
    const created = await fetch(`${eventRoot(director.baseUrl, event.id)}/rounds`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${TOKEN}` },
      body: JSON.stringify({
        label: 'Qualifying',
        classes: ['mgp-open'],
        format: 'timed_qual',
        params: { rounds: '1' },
        win_condition: 'BestLap',
        // Best Lap only ranks, so a scored round needs a race time to end (server validation).
        time_limit_secs: 60,
        // timed_qual defaults to Static (Slice 7a); these FillRound seams assert the per-heat
        // whole-field path, so force PerHeat (the static path is covered by the app-level e2e).
        channel_mode: 'PerHeat'
      })
    });
    const round = (await created.json()) as RoundDef;
    return { eventId: event.id, roundId: round.id };
  }

  it('FillRound acks ok and the round-scheduled heat is snapshot-able (tagged round + class)', async () => {
    const a = await makePilot('Fill A');
    const b = await makePilot('Fill B');
    const { eventId, roundId } = await setupQualRound('FillRound Event', [a, b]);

    // FillRound draws the field from the class membership and schedules the first heat.
    const ack = await control(eventId, { FillRound: { round: roundId } }, TOKEN);
    expect(ack.status).toBe(200);
    expect(ack.body).toEqual({ ok: true });

    // The class snapshot now resolves (the round-tagged heat folded into the class scope).
    const classSnap = await fetch(
      `${eventRoot(director.baseUrl, eventId)}/snapshot/class/${eventId}/mgp-open`
    );
    expect(classSnap.status).toBe(200);
  });

  it('FillRound on an empty-field round → CommandAck{ok:false} (BadRequest), HTTP 200', async () => {
    // A round whose class has no membership has no field to schedule.
    const { eventId, roundId } = await setupQualRound('FillRound Empty', []);
    const ack = await control(eventId, { FillRound: { round: roundId } }, TOKEN);
    expect(ack.status).toBe(200); // the failure rides in the ack body, not the HTTP status
    const body = ack.body as { ok: boolean; error?: { code: string } };
    expect(body.ok).toBe(false);
    expect(body.error?.code).toBe('BadRequest');
  });

  it('FillRound on an unknown round → CommandAck{ok:false, UnknownScope}', async () => {
    const event = (await createEvent('FillRound Unknown', TOKEN)).body as EventMeta;
    const ack = await control(event.id, { FillRound: { round: 'no-such-round' } }, TOKEN);
    const body = ack.body as { ok: boolean; error?: { code: string } };
    expect(body.ok).toBe(false);
    expect(body.error?.code).toBe('UnknownScope');
  });

  it('FillRound is RD-gated — no token → 401', async () => {
    const { eventId, roundId } = await setupQualRound('FillRound Auth', []);
    const res = await control(eventId, { FillRound: { round: roundId } });
    expect(res.status).toBe(401);
  });
});
