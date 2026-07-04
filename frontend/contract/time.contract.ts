/**
 * `GET /time` contract: the **clock-skew keystone**.
 *
 * The RD console runs on a separate device from the Director, so every UI clock derives from
 * `session.serverNowMs()` — an offset measured against `GET /time` (`Date.now() + offset`),
 * never the local `Date.now()` alone. That whole scheme hangs off this one tiny response
 * shape: `{ now_micros }`, the server wall clock in **microseconds** since the Unix epoch as
 * a plain JSON `number` (the i64 → number wire rule, seam 4). If the field were renamed,
 * nested, stringified, or switched to milliseconds, every countdown/race clock in the console
 * would silently skew — so the shape is pinned here against the real Director.
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import { type Director } from '../test-harness/director.ts';
import { startContractDirector } from './harness.ts';

let director: Director;

beforeAll(async () => {
  director = await startContractDirector();
});

afterAll(async () => {
  await director?.stop();
});

/** `GET /time` → the raw status + parsed JSON body. */
async function getTime(): Promise<{ status: number; json: unknown }> {
  const res = await fetch(`${director.baseUrl}/time`);
  let json: unknown;
  try {
    json = await res.json();
  } catch {
    json = undefined;
  }
  return { status: res.status, json };
}

describe('GET /time serves the server wall clock as { now_micros }', () => {
  it('answers 200 with exactly { now_micros: number } — an open read, no token', async () => {
    const { status, json } = await getTime();
    expect(status).toBe(200);

    // Exactly the one field, a plain JSON number (never bigint/string — seam 4),
    // and a safe integer (cursors/times are bounded well below 2^53).
    expect(Object.keys(json as object)).toEqual(['now_micros']);
    const now = (json as { now_micros: unknown }).now_micros;
    expect(typeof now).toBe('number');
    expect(Number.isSafeInteger(now)).toBe(true);

    // MICROseconds since the epoch, i.e. "now": within a minute of this process's own
    // clock (both run on this machine). A milliseconds or seconds value would be three
    // or six orders of magnitude off and fail this bound.
    expect(Math.abs((now as number) - Date.now() * 1000)).toBeLessThan(60_000_000);
  });

  it('is a live clock: a later read serves a later instant', async () => {
    const first = ((await getTime()).json as { now_micros: number }).now_micros;
    await new Promise((r) => setTimeout(r, 10));
    const second = ((await getTime()).json as { now_micros: number }).now_micros;
    expect(second).toBeGreaterThan(first);
  });
});
