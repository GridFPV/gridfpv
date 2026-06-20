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
 * `ProjectionBody` variant + a numeric `cursor`; a wrong/unknown route does NOT return a
 * parseable `Snapshot` (it is the SPA fallback, not a silent 200 Snapshot); and every
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
    const { status, json } = await getSnapshot('/snapshot/event/spring-cup');
    expect(status).toBe(200);
    expect(isSnapshot(json)).toBe(true);
    const snap = json as { cursor: number; body: Record<string, unknown> };
    expect(Object.keys(snap.body)).toEqual(['LiveRaceState']);
    expect(typeof snap.cursor).toBe('number');
  });

  it('GET /snapshot/heat/{id} → 200 LiveRaceState (default projection)', async () => {
    const { status, json } = await getSnapshot(`/snapshot/heat/${HEAT}`);
    expect(status).toBe(200);
    expect(Object.keys((json as { body: object }).body)).toEqual(['LiveRaceState']);
  });

  it('GET /snapshot/heat/{id}?projection=laps → 200 LapList', async () => {
    const { status, json } = await getSnapshot(`/snapshot/heat/${HEAT}?projection=laps`);
    expect(status).toBe(200);
    expect(Object.keys((json as { body: object }).body)).toEqual(['LapList']);
  });

  it('GET /snapshot/heat/{id}?projection=result → 200 HeatResult', async () => {
    const { status, json } = await getSnapshot(`/snapshot/heat/${HEAT}?projection=result`);
    expect(status).toBe(200);
    expect(Object.keys((json as { body: object }).body)).toEqual(['HeatResult']);
  });

  it('GET /snapshot/class/{event}/{class} → 200 LiveRaceState', async () => {
    const { status, json } = await getSnapshot('/snapshot/class/spring-cup/open');
    expect(status).toBe(200);
    expect(Object.keys((json as { body: object }).body)).toEqual(['LiveRaceState']);
  });

  it('GET /snapshot/pilot/{event}/{pilot} → 200 LapList for a competitor with laps', async () => {
    // The sim emits passes for A/B once the heat runs; drive it so the pilot scope resolves.
    await rdControl(director.baseUrl, TOKEN, { Stage: { heat: HEAT } });
    await rdControl(director.baseUrl, TOKEN, { Arm: { heat: HEAT } });
    await rdControl(director.baseUrl, TOKEN, { Start: { heat: HEAT } });
    // Poll the pilot snapshot until A has a lap (sim paces in real time).
    const deadline = Date.now() + 8_000;
    let snap: { status: number; json: unknown } | undefined;
    while (Date.now() < deadline) {
      snap = await getSnapshot('/snapshot/pilot/spring-cup/A');
      if (snap.status === 200) break;
      await new Promise((r) => setTimeout(r, 50));
    }
    expect(snap?.status).toBe(200);
    expect(Object.keys((snap?.json as { body: object }).body)).toEqual(['LapList']);
  });

  it('an unknown SCOPE id → 404 ProtocolError(UnknownScope), not a silent Snapshot', async () => {
    // A real path with a missing id: the contract is a typed 404, not a 200 fallback.
    const { status, json } = await getSnapshot('/snapshot/heat/does-not-exist');
    expect(status).toBe(404);
    expect((json as { code?: string }).code).toBe('UnknownScope');
  });

  it('a WRONG route (`?scope=` style) is NOT a Snapshot — it is the SPA fallback', async () => {
    // THE path-vs-scope bug: a client that addressed the snapshot by a `?scope=` query (or
    // any path the protocol router does not own) falls through to the SPA `fallback_service`.
    // With no built console that is a 404 text body; with one it is `index.html` (200). Either
    // way it is NOT a parseable `Snapshot`, so a client must address by PATH, never `?scope=`.
    const wrong = await getSnapshot('/snapshot?scope=event:spring-cup');
    expect(isSnapshot(wrong.json)).toBe(false);
    expect(wrong.text).not.toContain('"cursor"');
  });
});

describe('seam 4: wire integers are JSON numbers, never bigint/string', () => {
  it('snapshot cursor is a number > 0 on a non-empty log', async () => {
    const { json } = await getSnapshot('/snapshot/event/spring-cup');
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
      const { json } = await getSnapshot(`/snapshot/heat/${HEAT}`);
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
      const { json } = await getSnapshot(`/snapshot/heat/${HEAT}?projection=laps`);
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
