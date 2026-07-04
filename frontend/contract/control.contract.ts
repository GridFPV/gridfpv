/**
 * Seam 5 + seam 6: the control write path (headers + command shape + the resulting change
 * reaching a `/stream` subscriber) and auth.
 *
 * guards:
 *  - seam 5 → command shape + headers: each `Command` variant acks `{ ok: true }` and appends;
 *    a missing `Content-Type: application/json` is rejected; an illegal transition is a
 *    well-formed `{ ok: false, error: ProtocolError(BadRequest) }` (NOT an HTTP error); and the
 *    consequence of a command reaches a `/stream` subscriber on the read path.
 *  - seam 6 → auth: control with no token / an unknown (revoked-equivalent) token is `401`; a
 *    valid RD token is accepted; reads (`/snapshot`, `/stream`) are open with no token; and an
 *    RD can mint a read-only **join token** over the wire (`POST /auth/join-token`, #63) that
 *    authenticates a READ but is rejected on CONTROL.
 *
 * NOTE (#63, formerly a recorded gap): the server now exposes `POST /auth/join-token` — an
 * RD-gated endpoint that mints a fresh read-only join token (`issue_join_token`) as a
 * `JoinTokenResponse`. So the "a read-only join-token is rejected on control" arm of seam 6 is
 * exercised over the real wire here (mint with the RD token, then assert the minted token is
 * accepted on a read and rejected — 401 — on control). The Rust unit test
 * `auth::tests::join_token_is_read_only_and_rejected_on_control` still covers the store-level
 * invariant.
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import type { Command, JoinTokenResponse } from '@gridfpv/types';

import { type Director } from '../test-harness/director.ts';
import {
  eventRoot,
  openSocket,
  postControl,
  rdControl,
  startContractDirector,
  tryOpenControlWs,
  waitForFrame,
  wsBase
} from './harness.ts';

const TOKEN = 'rd-control-contract';

let director: Director;

beforeAll(async () => {
  director = await startContractDirector({ token: TOKEN, simLaps: 1, simLapMs: 40 });
});

afterAll(async () => {
  await director?.stop();
});

describe('seam 5: control command shape + headers', () => {
  it('ScheduleHeat → CommandAck{ok:true} and the heat becomes snapshot-able', async () => {
    const ack = await rdControl(director.baseUrl, TOKEN, {
      ScheduleHeat: { heat: 'h-shape', lineup: ['A', 'B'] }
    });
    expect(ack).toEqual({ ok: true });
    const res = await fetch(`${eventRoot(director.baseUrl)}/snapshot/heat/h-shape`);
    expect(res.status).toBe(200); // it now resolves — the append took effect
  });

  it('the heat-loop transitions each ack ok and append in order', async () => {
    await rdControl(director.baseUrl, TOKEN, { ScheduleHeat: { heat: 'h-loop', lineup: ['A'] } });
    // Heat-lifecycle Slice 2 collapsed the manual middle steps: `Start` arms the heat, and the
    // `Armed → Running` / `Running → Unofficial` transitions are runtime-clock-driven. This contract
    // drives the FSM deterministically through the **overrides** (`SkipCountdown`/`ForceEnd`) that
    // record the same transitions, so it doesn't depend on the start-delay / win-condition timers.
    const loop: Command[] = [
      { Stage: { heat: 'h-loop' } },
      { Start: { heat: 'h-loop' } },
      { SkipCountdown: { heat: 'h-loop' } },
      { ForceEnd: { heat: 'h-loop' } },
      { Finalize: { heat: 'h-loop' } },
      { Revert: { heat: 'h-loop' } },
      { Finalize: { heat: 'h-loop' } }
    ];
    for (const command of loop) {
      const ack = await rdControl(director.baseUrl, TOKEN, command);
      expect(ack.ok, `command ${JSON.stringify(command)} should ack ok`).toBe(true);
    }
  });

  it('the runtime-clock overrides (SkipCountdown/ForceEnd) ack ok in their state', async () => {
    // Heat-lifecycle Slice 2: SkipCountdown forces Armed → Running, ForceEnd forces Running →
    // Unofficial. Drive Stage → Start to reach Armed, then exercise both overrides.
    await rdControl(director.baseUrl, TOKEN, { ScheduleHeat: { heat: 'h-ovr', lineup: ['A'] } });
    for (const command of [
      { Stage: { heat: 'h-ovr' } },
      { Start: { heat: 'h-ovr' } },
      { SkipCountdown: { heat: 'h-ovr' } },
      { ForceEnd: { heat: 'h-ovr' } }
    ] as Command[]) {
      const ack = await rdControl(director.baseUrl, TOKEN, command);
      expect(ack.ok, `override ${JSON.stringify(command)} should ack ok`).toBe(true);
    }
    // An override out of its state is rejected (ForceEnd is only legal while Running).
    const illegal = await rdControl(director.baseUrl, TOKEN, { ForceEnd: { heat: 'h-ovr' } });
    expect(illegal.ok).toBe(false);
    expect(illegal.error?.code).toBe('BadRequest');
  });

  it('Register + the marshaling adjudications ack ok', async () => {
    await rdControl(director.baseUrl, TOKEN, {
      ScheduleHeat: { heat: 'h-marshal', lineup: ['A'] }
    });
    const register = await rdControl(director.baseUrl, TOKEN, {
      Register: { adapter: 'sim', competitor: 'A', pilot: 'acroace' }
    });
    expect(register.ok).toBe(true);
    const penalty = await rdControl(director.baseUrl, TOKEN, {
      ApplyPenalty: {
        heat: 'h-marshal',
        competitor: 'A',
        penalty: { TimeAdded: { micros: 2_000_000 } }
      }
    });
    expect(penalty.ok).toBe(true);
    const voidHeat = await rdControl(director.baseUrl, TOKEN, { VoidHeat: { heat: 'h-marshal' } });
    expect(voidHeat.ok).toBe(true);
  });

  it('the Slice-2 primitives (SplitLap/ReverseRuling) are wired and validate their target', async () => {
    // SplitLap targets the pass that *ends* the over-long lap; ReverseRuling targets a prior
    // penalty. No real pass/penalty is appended here, so both must be *rejected* with a typed
    // BadRequest (the target validation) — proving the commands route through the handler and
    // are not silent no-ops. (The happy-path fold is covered exhaustively by the Rust tests.)
    const split = await rdControl(director.baseUrl, TOKEN, {
      SplitLap: { target: 999_999, at: 5_000_000 }
    });
    expect(split.ok).toBe(false);
    expect(split.error?.code).toBe('BadRequest');

    const reverse = await rdControl(director.baseUrl, TOKEN, {
      ReverseRuling: { target: 999_999 }
    });
    expect(reverse.ok).toBe(false);
    expect(reverse.error?.code).toBe('BadRequest');
  });

  it('a missing Content-Type → a JSON ProtocolError(BadRequest), not a bare-text 4xx', async () => {
    const { status, body } = await postControl(
      director.baseUrl,
      { ScheduleHeat: { heat: 'h-noct', lineup: ['PILOT-A'] } },
      { token: TOKEN, contentType: false }
    );
    // The control endpoint now answers the uniform `ProtocolError` JSON shape every other API
    // surface uses (the papercut fix) — a typed `BadRequest` (HTTP 400) with a message — instead
    // of axum's bare-text rejection a client can't parse.
    expect(status).toBe(400);
    const err = body as { code?: string; message?: string };
    expect(err.code).toBe('BadRequest');
    expect(typeof err.message).toBe('string');
    expect(err.message && err.message.length).toBeGreaterThan(0);
  });

  it('a malformed JSON body (with the right Content-Type) → a JSON ProtocolError(BadRequest)', async () => {
    // Even with `Content-Type: application/json`, an unparseable body is the same uniform typed
    // error — not a bare-text 4xx. Posted raw so the body really is invalid JSON.
    const res = await fetch(`${eventRoot(director.baseUrl)}/control`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${TOKEN}` },
      body: '{ not valid json'
    });
    expect(res.status).toBe(400);
    const err = (await res.json()) as { code?: string };
    expect(err.code).toBe('BadRequest');
  });

  it('an illegal transition → CommandAck{ok:false, error: ProtocolError(BadRequest)}, HTTP 200', async () => {
    await rdControl(director.baseUrl, TOKEN, {
      ScheduleHeat: { heat: 'h-illegal', lineup: ['A'] }
    });
    // Start (Staged → Armed) is illegal straight from Scheduled — the heat must Stage first.
    const { status, body } = await postControl(
      director.baseUrl,
      { Start: { heat: 'h-illegal' } },
      { token: TOKEN }
    );
    expect(status).toBe(200); // the failure rides in the ack body, not the HTTP status
    const ack = body as { ok: boolean; error?: { code: string } };
    expect(ack.ok).toBe(false);
    expect(ack.error?.code).toBe('BadRequest');
  });

  it('a command on an unknown heat → CommandAck{ok:false, error: UnknownScope}', async () => {
    const { body } = await postControl(
      director.baseUrl,
      { Stage: { heat: 'never-scheduled' } },
      { token: TOKEN }
    );
    const ack = body as { ok: boolean; error?: { code: string } };
    expect(ack.ok).toBe(false);
    expect(ack.error?.code).toBe('UnknownScope');
  });

  it("a command's resulting change reaches a /stream subscriber on the read path", async () => {
    await rdControl(director.baseUrl, TOKEN, {
      ScheduleHeat: { heat: 'h-reaches', lineup: ['A'] }
    });
    const snap = (await (
      await fetch(`${eventRoot(director.baseUrl)}/snapshot/heat/h-reaches`)
    ).json()) as {
      cursor: number;
    };
    const { ws, frames } = await openSocket(`${wsBase(eventRoot(director.baseUrl))}/stream`);
    ws.send(JSON.stringify({ scope: { Heat: { heat: 'h-reaches' } }, from: snap.cursor }));
    // Drive a change over the CONTROL path; it must surface on the READ stream.
    await rdControl(director.baseUrl, TOKEN, { Stage: { heat: 'h-reaches' } });
    await waitForFrame(frames, (f) =>
      f.some((x) => (x as { Change?: unknown }).Change !== undefined)
    );
    const env = (
      frames.find((x) => (x as { Change?: unknown }).Change) as {
        Change: { change: { FreshValue: { LiveRaceState: { phase: string } } } };
      }
    ).Change;
    expect(env.change.FreshValue.LiveRaceState.phase).toBe('Staged');
    ws.close();
  });

  it('the bidirectional control WS (with the auth header) acks commands', async () => {
    const { ws, frames } = await openSocket(`${wsBase(eventRoot(director.baseUrl))}/control`, {
      Authorization: `Bearer ${TOKEN}`
    });
    ws.send(JSON.stringify({ ScheduleHeat: { heat: 'h-ws', lineup: ['A'] } }));
    await waitForFrame(frames, (f) => f.length > 0);
    expect(frames[0]).toEqual({ ok: true });
    ws.close();
  });
});

describe('seam 6: auth gates control, reads stay open', () => {
  it('control with NO token → 401 ProtocolError(Unauthorized)', async () => {
    const { status, body } = await postControl(director.baseUrl, {
      ScheduleHeat: { heat: 'h-noauth', lineup: ['PILOT-A'] }
    });
    expect(status).toBe(401);
    expect((body as { code?: string }).code).toBe('Unauthorized');
  });

  it('control with an UNKNOWN/revoked token → 401', async () => {
    const { status } = await postControl(
      director.baseUrl,
      { ScheduleHeat: { heat: 'h-badtok', lineup: ['PILOT-A'] } },
      { token: 'not-a-real-token' }
    );
    expect(status).toBe(401);
  });

  it('control with the valid RD token → accepted', async () => {
    const ack = await rdControl(director.baseUrl, TOKEN, {
      ScheduleHeat: { heat: 'h-goodtok', lineup: ['PILOT-A'] }
    });
    expect(ack.ok).toBe(true);
  });

  it('the control WS upgrade is rejected without the auth header', async () => {
    const withAuth = await tryOpenControlWs(`${wsBase(eventRoot(director.baseUrl))}/control`, {
      Authorization: `Bearer ${TOKEN}`
    });
    const withoutAuth = await tryOpenControlWs(`${wsBase(eventRoot(director.baseUrl))}/control`);
    expect(withAuth).toBe(true);
    expect(withoutAuth).toBe(false);
  });

  it('reads are OPEN — /snapshot and /stream need no token', async () => {
    const snap = await fetch(`${eventRoot(director.baseUrl)}/snapshot/event/any`);
    expect(snap.status).toBe(200); // no Authorization header sent
    const { ws, frames } = await openSocket(`${wsBase(eventRoot(director.baseUrl))}/stream`);
    ws.send(JSON.stringify({ scope: { Event: { event: 'any' } }, from: 0 }));
    // It subscribes without auth and does not get an Unauthorized error frame.
    await new Promise((r) => setTimeout(r, 300));
    expect((frames[0] as { code?: string } | undefined)?.code).not.toBe('Unauthorized');
    ws.close();
  });

  it('minting a join token requires the RD token — no/bad token → 401 (#63)', async () => {
    // No Authorization → 401.
    const anon = await fetch(`${eventRoot(director.baseUrl)}/auth/join-token`, { method: 'POST' });
    expect(anon.status).toBe(401);
    // An unknown token → 401.
    const bad = await fetch(`${eventRoot(director.baseUrl)}/auth/join-token`, {
      method: 'POST',
      headers: { Authorization: 'Bearer not-a-real-token' }
    });
    expect(bad.status).toBe(401);
  });

  it('an RD mints a read-only join token that reads but is REJECTED on control (#63)', async () => {
    // The RD trades its RD token for a fresh read-only join token over the wire.
    const res = await fetch(`${eventRoot(director.baseUrl)}/auth/join-token`, {
      method: 'POST',
      headers: { Authorization: `Bearer ${TOKEN}` }
    });
    expect(res.status).toBe(200);
    const { token: join } = (await res.json()) as JoinTokenResponse;
    expect(typeof join).toBe('string');
    expect(join.length).toBeGreaterThan(0);
    expect(join).not.toBe(TOKEN); // a distinct, freshly-minted credential

    // The minted join token authenticates a READ (reads accept a valid token of either role).
    const read = await fetch(`${eventRoot(director.baseUrl)}/snapshot/event/any`, {
      headers: { Authorization: `Bearer ${join}` }
    });
    expect(read.status).toBe(200);

    // …but it is REJECTED on CONTROL — a spectator can watch, never run the race.
    const { status, body } = await postControl(
      director.baseUrl,
      { ScheduleHeat: { heat: 'h-join-rejected', lineup: ['PILOT-A'] } },
      { token: join }
    );
    expect(status).toBe(401);
    expect((body as { code?: string }).code).toBe('Unauthorized');
  });
});
