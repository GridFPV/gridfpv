/**
 * The Director's HTTP surface, driven over the real wire: the events lifecycle, the active
 * event, the app-level pilot and class directories, the join token, and the build stamp.
 *
 * guards:
 *  - `GET /events` on a **fresh** Director is an empty list — there is no built-in event
 *    (#414), so first run is genuinely "create an event", not "pick the seeded one".
 *  - `POST /events` is RD-gated (no/bad token → 401), auto-generates a unique `id` from the
 *    display `name` (the id is never user-supplied), and returns the new event's `EventMeta`.
 *  - `DELETE /events/{id}` is RD-gated and permanent; an unknown id → a typed `ProtocolError`
 *    404 (`UnknownScope`).
 *  - a path under an API prefix that matches no route is the same typed 404, never the console.
 *
 * Every body is also checked against its generated binding (`wire-shape.ts`), so an expectation
 * here is never a second hand-written copy of a wire shape.
 *
 * Everything drives the real Director over the real wire — no mocks.
 */
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import type { Class, EventMeta, JoinTokenResponse, Pilot } from '@gridfpv/types';

import {
  createPilot as clientCreatePilot,
  isRequestFailure,
  listClasses as clientListClasses,
  listEvents as clientListEvents
} from '../packages/protocol-client/dist/index.js';
import { type Director } from '../test-harness/director.ts';
import { eventRoot, startContractDirector } from './harness.ts';
import { wireShapeProblems } from './wire-shape.ts';

const TOKEN = 'rd-events-contract';

let director: Director;

beforeAll(async () => {
  // Configure a data dir so created events are genuinely persistent (a SQLite file per
  // event) — the local realization of the per-event log (#72).
  const dataDir = mkdtempSync(join(tmpdir(), 'gridfpv-events-data-'));
  director = await startContractDirector({
    token: TOKEN,
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
  it('GET /events on a fresh Director is empty — there is no built-in event (#414)', async () => {
    // This must run first: the suite creates events below. A brand-new Director holds nothing,
    // which is the first-run state the console's "create your first event" screen handles.
    expect(await listEvents()).toEqual([]);
  });

  it('a created event is persistent and carries a numeric created_at', async () => {
    const created = (await createEvent('Shape Check', TOKEN)).body as EventMeta;
    expect(created.persistent).toBe(true);
    // created_at is a plain JSON number (the i64 → number contract), never a bigint/string.
    expect(typeof created.created_at).toBe('number');
    expect(wireShapeProblems(created, 'EventMeta')).toEqual([]);
    const listed = (await listEvents()).find((e) => e.id === created.id);
    expect(listed?.name).toBe('Shape Check');
  });

  it('POST /events stores the optional details and omits the unset ones', async () => {
    const res = await fetch(`${director.baseUrl}/events`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${TOKEN}` },
      body: JSON.stringify({ name: 'Detailed', date: '2026-06-20', location: 'Main field' })
    });
    expect(res.status).toBe(200);
    const created = (await res.json()) as EventMeta;
    expect(wireShapeProblems(created, 'EventMeta')).toEqual([]);
    expect(created.date).toBe('2026-06-20');
    expect(created.location).toBe('Main field');
    expect('organizer' in created).toBe(false);
    expect('description' in created).toBe(false);
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

    // Both new events now appear in the listing.
    const ids = (await listEvents()).map((e) => e.id);
    expect(ids).toContain(a.id);
    expect(ids).toContain(b.id);
  });

  it('a path under /events that matches no route → 404 ProtocolError(UnknownScope)', async () => {
    const res = await fetch(`${eventRoot(director.baseUrl, 'no-such-event')}/snapshot/event/x`);
    expect(res.status).toBe(404);
    const body = (await res.json()) as { code?: string };
    expect(body.code).toBe('UnknownScope');
    expect(wireShapeProblems(body, 'ProtocolError')).toEqual([]);
  });

  it('DELETE /events/{id} permanently removes a created event', async () => {
    const created = (await createEvent('Doomed Event', TOKEN)).body as EventMeta;

    // The delete is RD-gated and a 200 with no error body.
    const del = await deleteEvent(created.id, TOKEN);
    expect(del.status).toBe(200);

    // It is gone from the listing, and a second delete finds nothing.
    const ids = (await listEvents()).map((e) => e.id);
    expect(ids).not.toContain(created.id);
    expect((await deleteEvent(created.id, TOKEN)).status).toBe(404);
  });

  it('DELETE /events/{id} is RD-gated and 404s unknown ids — no event is reserved', async () => {
    // Create a fresh event to attempt an unauthenticated delete against (it must survive).
    const created = (await createEvent('Gated Delete', TOKEN)).body as EventMeta;
    expect((await deleteEvent(created.id)).status).toBe(401);
    expect((await deleteEvent(created.id, 'not-a-real-token')).status).toBe(401);
    // Still present after the rejected deletes.
    expect((await listEvents()).map((e) => e.id)).toContain(created.id);

    // Nothing is undeletable any more (#414): the old built-in `practice` id is simply an
    // unknown event, so it 404s like any other rather than 400-ing as reserved.
    const practice = await deleteEvent('practice', TOKEN);
    expect(practice.status).toBe(404);
    expect((practice.body as { code?: string }).code).toBe('UnknownScope');

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
    const created = (await createEvent('Gate Check', TOKEN)).body as EventMeta;
    expect((await putActive(created.id)).status).toBe(401);
    expect((await putActive(created.id, 'not-a-real-token')).status).toBe(401);
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

    // And switching it to a second event re-points the Director (last write wins).
    const second = (await createEvent('Resume Me Too', TOKEN)).body as EventMeta;
    expect((await putActive(second.id, TOKEN)).status).toBe(200);
    expect((await getActive()).event?.id).toBe(second.id);
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

/**
 * Issue #74: pilots are **application-level** — a persisted directory the RD maintains once.
 *
 * guards:
 *  - `GET /pilots` is an **open read** → `Pilot[]` (empty on a fresh Director — no built-in pilot).
 *  - `POST /pilots` is **RD-gated** (no/bad token → 401), requires a `callsign`, auto-generates an
 *    id, carries optional metadata incl. the `vtx_types`/`multigp_id` hooks, and the new pilot
 *    then appears in the listing.
 *  - `PUT /pilots/{id}` edits with the three-state `OptionalEdit`; `DELETE /pilots/{id}` removes.
 */
describe('seam 11: the application-level pilot directory (#74)', () => {
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

    expect(wireShapeProblems(created, 'Pilot')).toEqual([]);

    const pilots = await listPilots();
    expect(pilots.map((p) => p.id)).toContain(created.id);
    for (const pilot of pilots) expect(wireShapeProblems(pilot, 'Pilot')).toEqual([]);
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

  it('DELETE /pilots/{id} removes the pilot — RD-gated, 404 on an unknown id', async () => {
    const created = (await createPilot({ callsign: 'Leaving' }, TOKEN)).body as Pilot;
    const del = (id: string, token?: string) =>
      fetch(`${director.baseUrl}/pilots/${id}`, {
        method: 'DELETE',
        headers: token === undefined ? {} : { Authorization: `Bearer ${token}` }
      });

    expect((await del(created.id)).status).toBe(401);
    expect((await del(created.id, TOKEN)).status).toBe(200);
    expect((await listPilots()).map((p) => p.id)).not.toContain(created.id);

    const unknown = await del(created.id, TOKEN);
    expect(unknown.status).toBe(404);
    expect(((await unknown.json()) as { code?: string }).code).toBe('UnknownScope');
  });
});

/**
 * Issue #84 (registry slice): classes are **application-level configuration** — a persisted
 * directory the RD maintains once, that outlives any one event. The
 * directory mirrors the pilot directory (a `ClassSource` provenance + optional reference/description;
 * the `ClassId`/`OptionalEdit` types are reused).
 *
 * guards:
 *  - `GET /classes` is an **open read** → `Class[]` carrying the 9 locked, fixed-id **built-in**
 *    classes (present on every Director, flagged `builtin`, carrying their real org as `source`).
 *  - `POST /classes` is **RD-gated** (no/bad token → 401), requires a `name`, auto-generates an id,
 *    carries the `source`/`reference`/`description` metadata, and the new class appears in the listing.
 *  - `PUT/DELETE /classes/{id}` on a **built-in** id is rejected (read-only); user/Custom classes are
 *    full CRUD (set / clear via wire-`null` — the `OptionalEdit` three-state).
 *  - `PUT /classes/{id}/hidden` hides or un-hides any class, built-ins included.
 */
describe('seam 12: the application-level class directory (#84)', () => {
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
    for (const cls of classes) expect(wireShapeProblems(cls, 'Class')).toEqual([]);
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
 * Join tokens (#63): an RD trades its token for a **read-only** one to hand to a spectator.
 *
 * guards:
 *  - `POST /events/{id}/auth/join-token` is **RD-gated** (no/bad token → 401) and 404s an
 *    unknown event.
 *  - the minted token is a distinct credential, and it is **rejected on every write**.
 */
describe('seam 6: the read-only join token (#63)', () => {
  const mint = (eventId: string, token?: string) =>
    fetch(`${eventRoot(director.baseUrl, eventId)}/auth/join-token`, {
      method: 'POST',
      headers: token === undefined ? {} : { Authorization: `Bearer ${token}` }
    });

  it('minting a join token requires the RD token — no/bad token → 401', async () => {
    const event = (await createEvent('Join Gate', TOKEN)).body as EventMeta;
    expect((await mint(event.id)).status).toBe(401);
    expect((await mint(event.id, 'not-a-real-token')).status).toBe(401);
  });

  it('minting against an unknown event → 404 UnknownScope', async () => {
    const res = await mint('no-such-event', TOKEN);
    expect(res.status).toBe(404);
    expect(((await res.json()) as { code?: string }).code).toBe('UnknownScope');
  });

  it('an RD mints a read-only join token that is REJECTED on a write', async () => {
    const event = (await createEvent('Join Me', TOKEN)).body as EventMeta;
    const res = await mint(event.id, TOKEN);
    expect(res.status).toBe(200);
    const body = (await res.json()) as JoinTokenResponse;
    expect(wireShapeProblems(body, 'JoinTokenResponse')).toEqual([]);
    expect(body.token.length).toBeGreaterThan(0);
    expect(body.token).not.toBe(TOKEN); // a distinct, freshly-minted credential

    // A spectator can watch, never write: the join token is refused where the RD token works.
    const asSpectator = await createEvent('Spectator Event', body.token);
    expect(asSpectator.status).toBe(401);
    expect((asSpectator.body as { code?: string }).code).toBe('Unauthorized');
  });
});

/**
 * The real `@gridfpv/protocol-client` build against the real Director: the client's own calls
 * read the same wire the raw `fetch` tests above pin, and a refusal reaches the caller as a
 * `RequestFailure` carrying the Director's sentence and the HTTP status.
 */
describe('the protocol-client against the real Director', () => {
  it('reads the event list and the class directory', async () => {
    const events = await clientListEvents(director.baseUrl);
    expect(events.length).toBeGreaterThan(0);
    for (const event of events) expect(wireShapeProblems(event, 'EventMeta')).toEqual([]);

    const classes = await clientListClasses(director.baseUrl);
    expect(classes.map((c) => c.id)).toContain('mgp-open');
  });

  it('creates a pilot with the RD token', async () => {
    const pilot = await clientCreatePilot(
      director.baseUrl,
      { callsign: 'Client Made', vtx_types: [] },
      TOKEN
    );
    expect(pilot.callsign).toBe('Client Made');
    expect(wireShapeProblems(pilot, 'Pilot')).toEqual([]);
  });

  it('a refused write rejects with the status and the typed code', async () => {
    const refused = await clientCreatePilot(director.baseUrl, {
      callsign: 'No Token',
      vtx_types: []
    }).then(
      () => undefined,
      (e: unknown) => e
    );
    expect(isRequestFailure(refused)).toBe(true);
    if (!isRequestFailure(refused)) return;
    expect(refused.status).toBe(401);
    expect(refused.code).toBe('Unauthorized');
    expect(refused.message.length).toBeGreaterThan(0);
  });
});

describe('seam: /about (the build stamp)', () => {
  it('serves the product version + the wire contract version', async () => {
    const res = await fetch(`${director.baseUrl}/about`);
    expect(res.ok).toBe(true);
    const about = await res.json();
    expect(about.name).toBe('GridFPV');
    // x.y.z with an optional -prerelease tail: a release's standard naming
    // (0.4.0-alpha.1), or a non-release build's commit stamp (0.4.0-dev-<short hash>,
    // -dirty appended for an uncommitted tree) — #513.
    expect(about.version).toMatch(/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/);
    expect(typeof about.contract_version).toBe('number');
  });
});
