/**
 * Seam 2 + seam 3 + seam 7: the `/stream` WebSocket frames are externally-tagged
 * `StreamMessage`s, the per-stream `sequence` axis is distinct from the snapshot `cursor`
 * axis (and the real client applies early frames rather than dropping them as duplicates),
 * and an out-of-band `contract_version` gets the `VersionMismatch` refresh signal.
 *
 * guards:
 *  - seam 2 → the StreamMessage-unwrap bug: a frame is `{ "Change": ChangeEnvelope }` /
 *    `{ "ReSnapshotRequired": … }`, NOT a bare envelope. A client that read `frame.sequence`
 *    instead of `frame.Change.sequence` saw `undefined` and stalled (v0.4 bug).
 *  - seam 3 → the cursor/sequence conflation (the freeze): a snapshot of a non-empty log has
 *    `cursor > 0`, while the stream `sequence` restarts at 1; conflating the two axes made the
 *    client treat the first envelopes as `<= cursor` "duplicates" and freeze. The real client
 *    must converge.
 *  - seam 7 → version negotiation: a subscribe carrying an unsupported `contract_version` is
 *    answered with a `VersionMismatch` error frame + close; an absent version streams normally.
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import { connect } from '../packages/protocol-client/dist/index.js';
import { type Director } from '../test-harness/director.ts';
import { openSocket, rdControl, startContractDirector, waitForFrame, wsBase } from './harness.ts';

const TOKEN = 'rd-stream-contract';
const HEAT = 'q-1';

let director: Director;

beforeAll(async () => {
  director = await startContractDirector({ token: TOKEN, simLaps: 2, simLapMs: 40 });
  const ack = await rdControl(director.baseUrl, TOKEN, {
    ScheduleHeat: { heat: HEAT, lineup: ['A', 'B'] }
  });
  expect(ack.ok).toBe(true);
});

afterAll(async () => {
  await director?.stop();
});

/** The snapshot cursor for a scope path — the `from:` a stream resumes at. */
async function snapshotCursor(path: string): Promise<number> {
  const res = await fetch(`${director.baseUrl}${path}`);
  const snap = (await res.json()) as { cursor: number };
  return snap.cursor;
}

describe('seam 2: stream frames are externally-tagged StreamMessage', () => {
  it('a control append produces a `{ Change: ChangeEnvelope }` frame, not a bare envelope', async () => {
    const cursor = await snapshotCursor(`/snapshot/heat/${HEAT}`);
    const { ws, frames } = await openSocket(`${wsBase(director.baseUrl)}/stream`);
    ws.send(JSON.stringify({ scope: { Heat: { heat: HEAT } }, from: cursor }));

    // A heat-state change after the subscribe re-folds the scope and pushes one envelope.
    await rdControl(director.baseUrl, TOKEN, { Stage: { heat: HEAT } });
    await waitForFrame(frames, (f) => f.length > 0);

    const frame = frames[0] as Record<string, unknown>;
    // The contract: a tagged StreamMessage. The envelope is UNDER `Change`, not at top level.
    expect('Change' in frame).toBe(true);
    expect('sequence' in frame).toBe(false); // would be true if the server sent a bare envelope
    const env = frame.Change as Record<string, unknown>;
    expect(env).toHaveProperty('sequence');
    expect(env).toHaveProperty('projection');
    expect(env).toHaveProperty('change');
    ws.close();
  });

  it('a stale resume cursor yields `{ ReSnapshotRequired: ProtocolError(StaleCursor) }`', async () => {
    // Push the log tail far past the retained window (256), then resume from offset 1: that
    // offset is below the window, so the server sends the terminal ReSnapshotRequired signal.
    for (let i = 0; i < 300; i++) {
      await rdControl(director.baseUrl, TOKEN, {
        Register: { adapter: 'sim', competitor: `x${i}`, pilot: `p${i}` }
      });
    }
    const { ws, frames, closed } = await openSocket(`${wsBase(director.baseUrl)}/stream`);
    ws.send(JSON.stringify({ scope: { Heat: { heat: HEAT } }, from: 1 }));
    await waitForFrame(frames, (f) => f.length > 0);

    const frame = frames[0] as { ReSnapshotRequired?: { code: string } };
    expect(frame.ReSnapshotRequired).toBeDefined();
    expect(frame.ReSnapshotRequired?.code).toBe('StaleCursor');
    await closed; // the server closes after the signal
  });
});

describe('seam 3: sequence and cursor are distinct axes; the client converges', () => {
  it('a non-empty-log snapshot has cursor > 0 while the stream sequence starts at 1', async () => {
    const cursor = await snapshotCursor(`/snapshot/heat/${HEAT}`);
    expect(cursor).toBeGreaterThan(0); // the cursor axis is well past 1

    const { ws, frames } = await openSocket(`${wsBase(director.baseUrl)}/stream`);
    ws.send(JSON.stringify({ scope: { Heat: { heat: HEAT } }, from: cursor }));
    await rdControl(director.baseUrl, TOKEN, { Arm: { heat: HEAT } });
    await waitForFrame(frames, (f) => f.length > 0);

    const seq = (frames[0] as { Change: { sequence: number } }).Change.sequence;
    // The sequence axis restarts at 1 for THIS subscription — independent of the cursor (>0).
    expect(seq).toBe(1);
    expect(seq).not.toBe(cursor);
    ws.close();
  });

  it('the real protocol-client applies early frames (does NOT drop them as duplicates) and converges', async () => {
    // Subscribe with the real client to a non-empty log (cursor > 0). If it conflated cursor
    // and sequence it would treat sequence 1,2,3 (<= cursor) as duplicates and freeze. It must
    // instead converge to the running heat with climbing laps.
    const client = connect({ baseUrl: director.baseUrl, scope: { Heat: { heat: HEAT } } });
    try {
      await waitForState(client, (s) => s.body !== undefined);
      await rdControl(director.baseUrl, TOKEN, { Start: { heat: HEAT } });
      await waitForState(
        client,
        (s) => {
          const b = s.body as
            | { LiveRaceState?: { phase: string; progress?: Array<{ laps_completed: number }> } }
            | undefined;
          const ls = b?.LiveRaceState;
          return ls?.phase === 'Running' && (ls.progress ?? []).some((p) => p.laps_completed >= 1);
        },
        8_000
      );
      const ls = (client.getState().body as { LiveRaceState: { phase: string } }).LiveRaceState;
      expect(ls.phase).toBe('Running'); // converged — not frozen on stale "duplicates"
    } finally {
      client.close();
    }
  });
});

describe('seam 7: contract-version negotiation', () => {
  it('an out-of-band contract_version → VersionMismatch refresh signal + close', async () => {
    const { ws, frames, closed } = await openSocket(`${wsBase(director.baseUrl)}/stream`);
    ws.send(JSON.stringify({ scope: { Heat: { heat: HEAT } }, contract_version: 999 }));
    await waitForFrame(frames, (f) => f.length > 0);
    const frame = frames[0] as { code?: string };
    expect(frame.code).toBe('VersionMismatch');
    await closed; // the server closes after the refresh signal
  });

  it('an absent contract_version subscribes and streams normally', async () => {
    const cursor = await snapshotCursor(`/snapshot/heat/${HEAT}`);
    const { ws, frames } = await openSocket(`${wsBase(director.baseUrl)}/stream`);
    // No contract_version field at all — treated as this build's version, streams fine.
    ws.send(JSON.stringify({ scope: { Heat: { heat: HEAT } }, from: cursor }));
    await rdControl(director.baseUrl, TOKEN, { Finish: { heat: HEAT } });
    await waitForFrame(frames, (f) =>
      f.some((x) => (x as { Change?: unknown }).Change !== undefined)
    );
    expect((frames[0] as { code?: string }).code).not.toBe('VersionMismatch');
    ws.close();
  });
});

/** Resolve once the client's exposed state satisfies `pred` (or reject on timeout). */
function waitForState(
  client: ReturnType<typeof connect>,
  pred: (state: ReturnType<typeof client.getState>) => boolean,
  timeoutMs = 5_000
): Promise<void> {
  return new Promise((res, rej) => {
    let unsub: () => void = () => {};
    const to = setTimeout(() => {
      unsub();
      rej(new Error(`client state never satisfied predicate within ${timeoutMs}ms`));
    }, timeoutMs);
    unsub = client.onState((s) => {
      if (pred(s)) {
        clearTimeout(to);
        unsub();
        res();
      }
    });
  });
}
