/**
 * Seam 1 + seam 4: snapshot routes are **path-scoped**, and the numbers on the wire are
 * JSON `number`s (never bigint/string).
 *
 * guards: the path-vs-`?scope=` bug (a client that addressed `/snapshot?scope=…` got the SPA
 *   shell / a generic 404 — never a `Snapshot` — silently; v0.4 bug #1) and the i64/bigint
 *   class (a `cursor`/`last_lap_micros`/`duration_micros` that arrived as a bigint or string
 *   broke comparison + rendering; v0.4 bug class).
 *
 * Asserts the real wire: each correct path returns `200` + the right externally-tagged
 * `ProjectionBody` variant + a numeric `cursor`; a wrong/unknown **API** route returns a
 * typed `ProtocolError` 404 (the smart fallback #64), never the SPA shell or a silent 200
 * Snapshot, while a genuine non-API client route still falls through to the SPA; and every
 * integer field is `typeof === 'number'`.
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import { type Director } from '../test-harness/director.ts';
import { rdControl, startContractDirector } from './harness.ts';

const TOKEN = 'rd-snapshot-contract';
const HEAT = 'q-1';
const LINEUP = ['A', 'B'];

let director: Director;

beforeAll(async () => {
  director = await startContractDirector({ token: TOKEN, simLaps: 2, simLapMs: 40 });
  // A non-empty log so the snapshot cursor is > 0 and the scopes resolve (a heat must be
  // scheduled before /snapshot/heat/{id} returns anything but UnknownScope).
  const ack = await rdControl(director.baseUrl, TOKEN, {
    ScheduleHeat: { heat: HEAT, lineup: LINEUP }
  });
  expect(ack.ok).toBe(true);
});

afterAll(async () => {
  await director?.stop();
});

/** Fetch a snapshot route and return the raw status + parsed JSON (or `undefined`). */
async function getSnapshot(path: string): Promise<{ status: number; json: unknown; text: string }> {
  const res = await fetch(`${director.baseUrl}${path}`);
  const text = await res.text();
  let json: unknown;
  try {
    json = JSON.parse(text);
  } catch {
    json = undefined;
  }
  return { status: res.status, json, text };
}

/** Whether a parsed body is a wire `Snapshot` (`{ cursor: number, body: ProjectionBody }`). */
function isSnapshot(v: unknown): v is { cursor: number; body: Record<string, unknown> } {
  return (
    typeof v === 'object' &&
    v !== null &&
    'cursor' in v &&
    'body' in v &&
    typeof (v as { body: unknown }).body === 'object'
  );
}

describe('seam 1: snapshot routes are path-scoped', () => {
  it('GET /snapshot/event/{id} → 200 LiveRaceState + numeric cursor', async () => {
    const { status, json } = await getSnapshot('/events/practice/snapshot/event/spring-cup');
    expect(status).toBe(200);
    expect(isSnapshot(json)).toBe(true);
    const snap = json as { cursor: number; body: Record<string, unknown> };
    expect(Object.keys(snap.body)).toEqual(['LiveRaceState']);
    expect(typeof snap.cursor).toBe('number');
  });

  it('GET /snapshot/heat/{id} → 200 LiveRaceState (default projection)', async () => {
    const { status, json } = await getSnapshot(`/events/practice/snapshot/heat/${HEAT}`);
    expect(status).toBe(200);
    expect(Object.keys((json as { body: object }).body)).toEqual(['LiveRaceState']);
  });

  it('GET /snapshot/heat/{id}?projection=laps → 200 LapList', async () => {
    const { status, json } = await getSnapshot(
      `/events/practice/snapshot/heat/${HEAT}?projection=laps`
    );
    expect(status).toBe(200);
    expect(Object.keys((json as { body: object }).body)).toEqual(['LapList']);
  });

  it('GET /snapshot/heat/{id}?projection=result → 200 HeatResult', async () => {
    const { status, json } = await getSnapshot(
      `/events/practice/snapshot/heat/${HEAT}?projection=result`
    );
    expect(status).toBe(200);
    expect(Object.keys((json as { body: object }).body)).toEqual(['HeatResult']);
  });

  it('GET /snapshot/heat/{id}?projection=audit → 200 MarshalingAudit (#55)', async () => {
    const { status, json } = await getSnapshot(
      `/events/practice/snapshot/heat/${HEAT}?projection=audit`
    );
    expect(status).toBe(200);
    const body = (json as { body: object }).body;
    expect(Object.keys(body)).toEqual(['MarshalingAudit']);
    // The body is an array of audit entries (empty when the heat has no rulings).
    expect(Array.isArray((body as { MarshalingAudit: unknown }).MarshalingAudit)).toBe(true);
  });

  it('GET /snapshot/class/{event}/{class} → 200 LiveRaceState', async () => {
    const { status, json } = await getSnapshot('/events/practice/snapshot/class/spring-cup/open');
    expect(status).toBe(200);
    expect(Object.keys((json as { body: object }).body)).toEqual(['LiveRaceState']);
  });

  it('GET /snapshot/pilot/{event}/{pilot} → 200 LapList for a competitor with laps', async () => {
    // The sim emits passes for A/B once the heat runs; drive it so the pilot scope resolves.
    // Start arms the heat; SkipCountdown forces it Running (the override for the runtime auto-start).
    await rdControl(director.baseUrl, TOKEN, { Stage: { heat: HEAT } });
    await rdControl(director.baseUrl, TOKEN, { Start: { heat: HEAT } });
    await rdControl(director.baseUrl, TOKEN, { SkipCountdown: { heat: HEAT } });
    // Poll the pilot snapshot until A has a lap (sim paces in real time).
    const deadline = Date.now() + 8_000;
    let snap: { status: number; json: unknown } | undefined;
    while (Date.now() < deadline) {
      snap = await getSnapshot('/events/practice/snapshot/pilot/spring-cup/A');
      if (snap.status === 200) break;
      await new Promise((r) => setTimeout(r, 50));
    }
    expect(snap?.status).toBe(200);
    expect(Object.keys((snap?.json as { body: object }).body)).toEqual(['LapList']);
  });

  it('projection=result places carry the win-condition metric (best-lap fallback, round-less heat)', async () => {
    // The contract heat was scheduled with no round, so the result scores under the best-lap
    // fallback (#45 — the result now scores under the heat's ROUND win condition, falling back to
    // best lap for a round-less / ad-hoc heat). The previous `it` ran the heat, so it now has laps:
    // each placement carries a `BestLapMicros` metric — the win-condition-derived metric on the
    // wire, NOT a bare placement. Poll until the scored places appear.
    const deadline = Date.now() + 8_000;
    let places: Array<{ metric: Record<string, unknown> }> = [];
    while (Date.now() < deadline) {
      const { json } = await getSnapshot(
        `/events/practice/snapshot/heat/${HEAT}?projection=result`
      );
      places = (
        json as { body: { HeatResult: { places: Array<{ metric: Record<string, unknown> }> } } }
      ).body.HeatResult.places;
      if (places.length > 0) break;
      await new Promise((r) => setTimeout(r, 50));
    }
    expect(places.length).toBeGreaterThan(0);
    for (const place of places) {
      expect(Object.keys(place.metric)).toEqual(['BestLapMicros']);
    }
  });

  it('an unknown SCOPE id → 404 ProtocolError(UnknownScope), not a silent Snapshot', async () => {
    // A real path with a missing id: the contract is a typed 404, not a 200 fallback.
    const { status, json } = await getSnapshot('/events/practice/snapshot/heat/does-not-exist');
    expect(status).toBe(404);
    expect((json as { code?: string }).code).toBe('UnknownScope');
  });

  it('a WRONG API route → typed 404 ProtocolError, NOT the SPA shell (#64)', async () => {
    // THE path-vs-scope bug, now fixed by the smart fallback (#64): a client that addressed
    // the snapshot by a `?scope=` query (path `/snapshot`) — or any path under a known API
    // tree the protocol router does not concretely own — gets a typed `ProtocolError` 404
    // JSON, NOT the SPA `index.html`. So a mistyped API call is an honest, parseable error a
    // client can branch on, never an HTML 200 it would choke on. It is still NOT a `Snapshot`,
    // so a client must address by PATH, never `?scope=`.
    const wrong = await getSnapshot('/snapshot?scope=event:spring-cup');
    expect(wrong.status).toBe(404);
    expect((wrong.json as { code?: string }).code).toBe('UnknownScope');
    expect(isSnapshot(wrong.json)).toBe(false);

    // A bad nested API path is likewise a typed 404, not the SPA.
    const garbage = await getSnapshot('/snapshot/zzz/nope/extra');
    expect(garbage.status).toBe(404);
    expect((garbage.json as { code?: string }).code).toBe('UnknownScope');
  });

  it('a NON-API client route still falls through to the SPA shell, not a typed 404 (#64)', async () => {
    // The smart fallback only typed-404s *API-tree* paths; a genuine client-side route (a
    // deep link the SPA router owns) still resolves to the SPA shell. The contract Director
    // points `GRIDFPV_ASSETS` at an empty dir, so the unbuilt-console fallback is a 404 text
    // body — but, crucially, it is NOT a `ProtocolError` JSON (no `code`), proving the path
    // took the SPA branch, not the API-404 branch.
    const spa = await getSnapshot('/heats/q-1/live');
    expect((spa.json as { code?: string } | undefined)?.code).toBeUndefined();
    expect(isSnapshot(spa.json)).toBe(false);
    expect(spa.text).not.toContain('"cursor"');
  });
});

describe('seam 4: wire integers are JSON numbers, never bigint/string', () => {
  it('snapshot cursor is a number > 0 on a non-empty log', async () => {
    const { json } = await getSnapshot('/events/practice/snapshot/event/spring-cup');
    const snap = json as { cursor: number };
    expect(typeof snap.cursor).toBe('number');
    expect(Number.isInteger(snap.cursor)).toBe(true);
    expect(snap.cursor).toBeGreaterThan(0);
  });

  it('PilotProgress.laps_completed + last_lap_micros are numbers', async () => {
    // The heat is running from the test above; poll until a pilot has a completed lap, then
    // assert the live-state integer fields are plain numbers (the i64/u32 → number contract).
    const deadline = Date.now() + 8_000;
    let progressed: { laps_completed: unknown; last_lap_micros?: unknown } | undefined;
    while (Date.now() < deadline) {
      const { json } = await getSnapshot(`/events/practice/snapshot/heat/${HEAT}`);
      const ls = (
        json as { body: { LiveRaceState: { progress?: Array<Record<string, unknown>> } } }
      ).body.LiveRaceState;
      const withLap = (ls.progress ?? []).find((p) => typeof p.last_lap_micros === 'number');
      if (withLap) {
        progressed = withLap as { laps_completed: unknown; last_lap_micros?: unknown };
        break;
      }
      await new Promise((r) => setTimeout(r, 50));
    }
    expect(progressed).toBeDefined();
    expect(typeof progressed?.laps_completed).toBe('number');
    expect(typeof progressed?.last_lap_micros).toBe('number');
  });

  it('Lap.duration_micros (laps projection) is a number', async () => {
    const deadline = Date.now() + 8_000;
    let dur: unknown;
    while (Date.now() < deadline) {
      const { json } = await getSnapshot(`/events/practice/snapshot/heat/${HEAT}?projection=laps`);
      const list = (
        json as {
          body: { LapList: { competitors: Array<{ laps: Array<{ duration_micros: unknown }> }> } };
        }
      ).body.LapList;
      const firstLap = list.competitors.flatMap((c) => c.laps)[0];
      if (firstLap) {
        dur = firstLap.duration_micros;
        break;
      }
      await new Promise((r) => setTimeout(r, 50));
    }
    expect(typeof dur).toBe('number');
  });
});
