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
 *    valid RD token is accepted; reads (`/snapshot`, `/stream`) are open with no token.
 *
 * NOTE (recorded gap, not a failure): the server exposes **no wire endpoint to mint a
 * read-only join token** — `issue_join_token` exists in `crates/server/src/auth.rs` but is not
 * reachable over HTTP, and only the single `GRIDFPV_RD_TOKEN` is pinned. So the "a read-only
 * join-token is rejected on control" arm of seam 6 cannot be exercised over the wire here; it
 * is covered by the Rust unit test `auth::tests::join_token_is_read_only_and_rejected_on_control`.
 * We assert the reachable equivalent: an unknown token (which a revoked token becomes) is 401.
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import type { Command } from '@gridfpv/types';

import { type Director } from '../test-harness/director.ts';
import {
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
    const res = await fetch(`${director.baseUrl}/snapshot/heat/h-shape`);
    expect(res.status).toBe(200); // it now resolves — the append took effect
  });

  it('the heat-loop transitions each ack ok and append in order', async () => {
    await rdControl(director.baseUrl, TOKEN, { ScheduleHeat: { heat: 'h-loop', lineup: ['A'] } });
    const loop: Command[] = [
      { Stage: { heat: 'h-loop' } },
      { Arm: { heat: 'h-loop' } },
      { Start: { heat: 'h-loop' } },
      { Finish: { heat: 'h-loop' } },
      { Score: { heat: 'h-loop' } }
    ];
    for (const command of loop) {
      const ack = await rdControl(director.baseUrl, TOKEN, command);
      expect(ack.ok, `command ${JSON.stringify(command)} should ack ok`).toBe(true);
    }
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

  it('a missing Content-Type is rejected (the Json extractor refuses it)', async () => {
    const { status } = await postControl(
      director.baseUrl,
      { ScheduleHeat: { heat: 'h-noct', lineup: [] } },
      { token: TOKEN, contentType: false }
    );
    // axum's `Json` extractor requires `application/json`; without it the request is rejected
    // (HTTP 4xx — 415 Unsupported Media Type in practice), NOT silently accepted.
    expect(status).toBeGreaterThanOrEqual(400);
    expect(status).toBeLessThan(500);
  });

  it('an illegal transition → CommandAck{ok:false, error: ProtocolError(BadRequest)}, HTTP 200', async () => {
    await rdControl(director.baseUrl, TOKEN, {
      ScheduleHeat: { heat: 'h-illegal', lineup: ['A'] }
    });
    // Start before Arm is illegal in the heat FSM.
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
    const snap = (await (await fetch(`${director.baseUrl}/snapshot/heat/h-reaches`)).json()) as {
      cursor: number;
    };
    const { ws, frames } = await openSocket(`${wsBase(director.baseUrl)}/stream`);
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
    const { ws, frames } = await openSocket(`${wsBase(director.baseUrl)}/control`, {
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
      ScheduleHeat: { heat: 'h-noauth', lineup: [] }
    });
    expect(status).toBe(401);
    expect((body as { code?: string }).code).toBe('Unauthorized');
  });

  it('control with an UNKNOWN/revoked token → 401', async () => {
    const { status } = await postControl(
      director.baseUrl,
      { ScheduleHeat: { heat: 'h-badtok', lineup: [] } },
      { token: 'not-a-real-token' }
    );
    expect(status).toBe(401);
  });

  it('control with the valid RD token → accepted', async () => {
    const ack = await rdControl(director.baseUrl, TOKEN, {
      ScheduleHeat: { heat: 'h-goodtok', lineup: [] }
    });
    expect(ack.ok).toBe(true);
  });

  it('the control WS upgrade is rejected without the auth header', async () => {
    const withAuth = await tryOpenControlWs(`${wsBase(director.baseUrl)}/control`, {
      Authorization: `Bearer ${TOKEN}`
    });
    const withoutAuth = await tryOpenControlWs(`${wsBase(director.baseUrl)}/control`);
    expect(withAuth).toBe(true);
    expect(withoutAuth).toBe(false);
  });

  it('reads are OPEN — /snapshot and /stream need no token', async () => {
    const snap = await fetch(`${director.baseUrl}/snapshot/event/any`);
    expect(snap.status).toBe(200); // no Authorization header sent
    const { ws, frames } = await openSocket(`${wsBase(director.baseUrl)}/stream`);
    ws.send(JSON.stringify({ scope: { Event: { event: 'any' } }, from: 0 }));
    // It subscribes without auth and does not get an Unauthorized error frame.
    await new Promise((r) => setTimeout(r, 300));
    expect((frames[0] as { code?: string } | undefined)?.code).not.toBe('Unauthorized');
    ws.close();
  });
});
