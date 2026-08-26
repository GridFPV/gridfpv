/**
 * Tune-telemetry contract (#355 S2a, built as the guard for #410):
 * `GET /timers/{id}/signal` + `POST /timers/{id}/signal/stop`.
 *
 * ── Why this file exists ────────────────────────────────────────────────────
 * The Tune page shipped against a **hand-declared** `TimerSignal` / `TimerSignalNode`,
 * written while the backend slice was unmerged. Every field name differed from the real
 * one — `current_rssi` vs `rssi`, `enter_at_level` vs `enter_at`, `crossing_flag` vs
 * `crossing`, a per-node `from`/`period_micros` vs the shared top-level `sample_micros` —
 * and all five gates passed anyway, because `tsc` cannot tell a fabricated interface from
 * a real one, the unit fixtures were shaped like the fabrication, and **this endpoint had
 * no contract test at all**. The page would have rendered every readout as `undefined`
 * against a live Director, discovered on the timer, in the field.
 *
 * So: the endpoint is exercised here against the real Director, and the expected body is
 * **derived from the generated ts-rs bindings** (`./wire-shape.ts`) rather than written
 * out by hand — a hand-written expectation would be the same failure mode one level down.
 *
 * ── What is asserted ────────────────────────────────────────────────────────
 *  - the body is exactly `bindings/TimerSignal.ts` — every declared field present with its
 *    declared type, and nothing on the wire the binding does not declare;
 *  - the **lease**: the `GET` *is* the subscription, every `GET` renews it to a full
 *    `SIGNAL_LEASE`, and a first poll before any data flows is a legitimate
 *    `streaming: false` with no samples — not an error, and not an empty 404;
 *  - `POST …/signal/stop` ends it (204), is idempotent, is harmless on a timer that never
 *    streamed, and does not stop a later `GET` from opening a fresh subscription;
 *  - the refusals: RD-gated (401), a Mock is a 400 that names the timer by its **friendly
 *    name** (repo display rule), an unknown id is a 404 `UnknownScope`;
 *  - the real `@gridfpv/protocol-client` `timerSignal` / `stopTimerSignal` drive all of it.
 *
 * ── Recorded gap ────────────────────────────────────────────────────────────
 * `TimerSignal.nodes` is fed **only** by a live RotorHazard socket, and this suite is
 * deliberately Docker-free — so against a Director with no timer plugged in the array is
 * legitimately empty, and the per-node `NodeSignal` field names are checked here only when
 * a node is present (with data they are exercised by the Rust route test
 * `app::tests::reading_a_timers_signal_starts_and_renews_the_lease`, which pushes readings
 * straight into the registry, and by the live matrix). {@link assertSignalShape} recurses into
 * every node it is given, so this coverage turns itself on the moment a node-bearing
 * fixture reaches the suite.
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import { stopTimerSignal, timerSignal } from '../packages/protocol-client/dist/index.js';
import { type Director } from '../test-harness/director.ts';
import { startContractDirector } from './harness.ts';
import { wireShapeProblems } from './wire-shape.ts';

import type { Timer, TimerSignal } from '@gridfpv/types';

/** The built-in Mock timer's reserved id — the timer that has no detector to tune. */
const MOCK_TIMER_ID = 'mock';

/** `SIGNAL_LEASE` on the Director (`crates/server/src/timers.rs`), in ms. */
const SIGNAL_LEASE_MS = 5_000;

let director: Director;
let token: string;
/** A RotorHazard timer that is never dialed — signal is readable, nothing feeds it. */
let rhTimerId: string;

const sleep = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms));

/** `POST /timers` a RotorHazard timer with the RD token, returning the created `Timer`. */
async function createRhTimer(name: string): Promise<Timer> {
  const res = await fetch(`${director.baseUrl}/timers`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${token}` },
    body: JSON.stringify({ name, kind: { Rotorhazard: { url: 'http://rh.invalid:5000' } } })
  });
  expect(res.status).toBe(200);
  return (await res.json()) as Timer;
}

/** `GET /timers/{id}/signal` with explicit header control → raw status + parsed body. */
async function getSignal(id: string, bearer?: string): Promise<{ status: number; body: unknown }> {
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (bearer !== undefined) headers.Authorization = `Bearer ${bearer}`;
  const res = await fetch(`${director.baseUrl}/timers/${encodeURIComponent(id)}/signal`, {
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

/** `POST /timers/{id}/signal/stop` with explicit header control → raw status + text body. */
async function postStop(id: string, bearer?: string): Promise<{ status: number; text: string }> {
  const headers: Record<string, string> = { Accept: 'application/json' };
  if (bearer !== undefined) headers.Authorization = `Bearer ${bearer}`;
  const res = await fetch(`${director.baseUrl}/timers/${encodeURIComponent(id)}/signal/stop`, {
    method: 'POST',
    headers
  });
  return { status: res.status, text: await res.text() };
}

/**
 * The whole shape assertion, generated from `bindings/TimerSignal.ts` — never hand-written.
 *
 * Recurses through `nodes` into `bindings/NodeSignal.ts`, so the per-node field names are
 * asserted for free whenever the array is non-empty.
 */
function assertSignalShape(body: unknown): TimerSignal {
  expect(wireShapeProblems(body, 'TimerSignal')).toEqual([]);
  return body as TimerSignal;
}

beforeAll(async () => {
  director = await startContractDirector({});
  token = director.token;
  rhTimerId = (await createRhTimer('Signal RH')).id;
});

afterAll(async () => {
  await director?.stop();
});

describe('GET /timers/{id}/signal serves the generated TimerSignal (#355 S2a, #410)', () => {
  it('the body is exactly the shape bindings/TimerSignal.ts declares', async () => {
    const { status, body } = await getSignal(rhTimerId, token);
    expect(status).toBe(200);

    // The one assertion this suite exists for: the wire matches the GENERATED type, field
    // for field. It is derived from bindings/TimerSignal.ts (which `cargo xtask ci` keeps
    // identical to the Rust struct), so a rename in Rust moves the expectation with it —
    // and a body shaped like a hand-declared guess fails here with every wrong name listed.
    const signal = assertSignalShape(body);

    // The identity + cadence fields the graph is drawn from.
    expect(signal.timer).toBe(rhTimerId);
    expect(signal.period_micros).toBeGreaterThan(0);
    expect(Number.isInteger(signal.period_micros)).toBe(true);
  });

  it('the first poll is a legitimate streaming:false with no samples yet', async () => {
    // Nothing is feeding this timer (it was never dialed), which is exactly the state the
    // Tune page opens in: a snapshot arrives, and it is empty. "No signal" is information,
    // not an error — and the page tells it apart from "no link" by `streaming`.
    const fresh = await createRhTimer('First Poll RH');
    const { status, body } = await getSignal(fresh.id, token);
    expect(status).toBe(200);

    const signal = assertSignalShape(body);
    expect(signal.streaming).toBe(false);
    expect(signal.sample_micros).toEqual([]);
    expect(signal.nodes).toEqual([]);
  });

  it('the shared sample axis is one axis for every node, not one per node', async () => {
    // The #410 fabrication put a `from` + `period_micros` on each node; the real wire has a
    // single top-level `sample_micros` axis that `nodes[*].samples[i]` indexes into.
    const signal = assertSignalShape((await getSignal(rhTimerId, token)).body);
    for (const node of signal.nodes) expect(node.samples.length).toBe(signal.sample_micros.length);
  });
});

describe('the signal subscription is a lease the GET renews (#355 S2a)', () => {
  it('every GET comes back holding a full lease, so polling never runs it down', async () => {
    const first = assertSignalShape((await getSignal(rhTimerId, token)).body);
    expect(first.lease_ms_remaining).toBeGreaterThan(0);
    expect(first.lease_ms_remaining).toBeLessThanOrEqual(SIGNAL_LEASE_MS);

    // Wait a good fraction of the lease, then poll again. A lease that merely *started* on
    // the first call would be visibly shorter here; a renewed one is full again. This is
    // the contract the Tune page's poll cadence depends on.
    await sleep(1_200);
    const second = assertSignalShape((await getSignal(rhTimerId, token)).body);
    expect(second.lease_ms_remaining).toBeGreaterThan(SIGNAL_LEASE_MS - 1_000);
    expect(second.lease_ms_remaining).toBeLessThanOrEqual(SIGNAL_LEASE_MS);
  });

  it('POST …/signal/stop ends it now — 204, idempotent, and harmless when never streaming', async () => {
    // The lease alone guarantees the stream stops; `stop` is for promptness on view close.
    expect((await postStop(rhTimerId, token)).status).toBe(204);
    expect((await postStop(rhTimerId, token)).status).toBe(204);

    // A timer nobody ever polled has no subscription to end — still a clean 204, no body.
    const never = await createRhTimer('Never Polled RH');
    const stopped = await postStop(never.id, token);
    expect(stopped.status).toBe(204);
    expect(stopped.text).toBe('');
  });

  it('a GET after stop opens a fresh subscription rather than staying closed', async () => {
    await postStop(rhTimerId, token);
    const { status, body } = await getSignal(rhTimerId, token);
    expect(status).toBe(200);

    const signal = assertSignalShape(body);
    expect(signal.lease_ms_remaining).toBeGreaterThan(SIGNAL_LEASE_MS - 1_000);
    // A restarted subscription starts empty: its ring belonged to a window that has ended.
    expect(signal.streaming).toBe(false);
    expect(signal.sample_micros).toEqual([]);
  });
});

describe('the signal route refuses what it must (#355 S2a)', () => {
  it('is RD-gated — no token and a wrong token are both 401', async () => {
    expect((await getSignal(rhTimerId)).status).toBe(401);
    expect((await getSignal(rhTimerId, 'not-a-real-token')).status).toBe(401);
    expect((await postStop(rhTimerId)).status).toBe(401);
    expect((await postStop(rhTimerId, 'not-a-real-token')).status).toBe(401);
  });

  it('an unknown timer is a 404 UnknownScope on both halves', async () => {
    const missing = await getSignal('no-such-timer', token);
    expect(missing.status).toBe(404);
    expect((missing.body as { code?: string }).code).toBe('UnknownScope');
    expect((await postStop('no-such-timer', token)).status).toBe(404);
  });

  it('the Mock is a 400 that names the timer by its friendly name', async () => {
    const { status, body } = await getSignal(MOCK_TIMER_ID, token);
    expect(status).toBe(400);

    const error = body as { code?: string; message?: string };
    expect(error.code).toBe('BadRequest');
    // Repo display rule: the refusal reaches the RD naming "Mock", not the raw id.
    expect(error.message).toContain('Mock');
    expect(error.message).not.toContain('mock');
  });
});

describe('the real protocol-client drives the signal seam (#355 S2a)', () => {
  it('timerSignal returns the generated TimerSignal and renews the lease', async () => {
    const signal = await timerSignal(director.baseUrl, rhTimerId, { token });
    assertSignalShape(signal);
    expect(signal.timer).toBe(rhTimerId);
    expect(signal.lease_ms_remaining).toBeGreaterThan(0);
  });

  it('stopTimerSignal ends the stream through the client', async () => {
    await expect(stopTimerSignal(director.baseUrl, rhTimerId, token)).resolves.toBeUndefined();
  });

  it("surfaces the Director's own refusal for a Mock, and the 401 without a token", async () => {
    // The client re-throws the Director's message, which is already phrased for the RD and
    // names the timer by its friendly name — not a line carrying the raw id.
    await expect(timerSignal(director.baseUrl, MOCK_TIMER_ID, { token })).rejects.toThrow(/Mock/);
    // Tokenless: the Director's 401 reaches the caller as a thrown error, never as an empty
    // `TimerSignal` the page would then render as a dead timer.
    await expect(timerSignal(director.baseUrl, rhTimerId)).rejects.toThrow();
  });
});
