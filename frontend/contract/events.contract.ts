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

import type { Command, EventMeta, Timer } from '@gridfpv/types';

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
