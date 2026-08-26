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
 *  - seam 8 (#422) → resume: an envelope states the log offset it was folded through, and a
 *    resume — from that offset or from a stale one — never hands the client an older fold.
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import { connect } from '../packages/protocol-client/dist/index.js';
import {
  openSocket,
  rdControl,
  startDirectorWithEvent,
  waitForFrame,
  wsBase,
  type ContractDirector
} from './harness.ts';

const TOKEN = 'rd-stream-contract';
const HEAT = 'q-1';

let director: ContractDirector;

beforeAll(async () => {
  director = await startDirectorWithEvent({ token: TOKEN, simLaps: 2, simLapMs: 40 });
  const ack = await rdControl(director, TOKEN, {
    ScheduleHeat: { heat: HEAT, lineup: ['A', 'B'] }
  });
  expect(ack.ok).toBe(true);
});

afterAll(async () => {
  await director?.stop();
});

/**
 * The snapshot cursor for a scope path — the `from:` a stream resumes at. `path` is the
 * within-event snapshot path (e.g. `/snapshot/heat/q-1`); it is rooted under the Practice
 * event (#72) here.
 */
async function snapshotCursor(path: string): Promise<number> {
  const res = await fetch(`${director.eventRoot}${path}`);
  const snap = (await res.json()) as { cursor: number };
  return snap.cursor;
}

describe('seam 2: stream frames are externally-tagged StreamMessage', () => {
  it('a control append produces a `{ Change: ChangeEnvelope }` frame, not a bare envelope', async () => {
    const cursor = await snapshotCursor(`/snapshot/heat/${HEAT}`);
    const { ws, frames } = await openSocket(`${wsBase(director.eventRoot)}/stream`);
    ws.send(JSON.stringify({ scope: { Heat: { heat: HEAT } }, from: cursor }));

    // A heat-state change after the subscribe re-folds the scope and pushes one envelope.
    await rdControl(director, TOKEN, { Stage: { heat: HEAT } });
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
      await rdControl(director, TOKEN, {
        Register: { adapter: 'sim', competitor: `x${i}`, pilot: `p${i}` }
      });
    }
    const { ws, frames, closed } = await openSocket(`${wsBase(director.eventRoot)}/stream`);
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

    const { ws, frames } = await openSocket(`${wsBase(director.eventRoot)}/stream`);
    ws.send(JSON.stringify({ scope: { Heat: { heat: HEAT } }, from: cursor }));
    await rdControl(director, TOKEN, { Start: { heat: HEAT } });
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
    const client = connect({
      baseUrl: director.baseUrl,
      eventId: director.event,
      scope: { Heat: { heat: HEAT } }
    });
    try {
      await waitForState(client, (s) => s.body !== undefined);
      // SkipCountdown forces Armed → Running (the override standing in for the runtime auto-start).
      await rdControl(director, TOKEN, { SkipCountdown: { heat: HEAT } });
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
    const { ws, frames, closed } = await openSocket(`${wsBase(director.eventRoot)}/stream`);
    ws.send(JSON.stringify({ scope: { Heat: { heat: HEAT } }, contract_version: 999 }));
    await waitForFrame(frames, (f) => f.length > 0);
    const frame = frames[0] as { code?: string };
    expect(frame.code).toBe('VersionMismatch');
    await closed; // the server closes after the refresh signal
  });

  it('an absent contract_version subscribes and streams normally', async () => {
    const cursor = await snapshotCursor(`/snapshot/heat/${HEAT}`);
    const { ws, frames } = await openSocket(`${wsBase(director.eventRoot)}/stream`);
    // No contract_version field at all — treated as this build's version, streams fine.
    ws.send(JSON.stringify({ scope: { Heat: { heat: HEAT } }, from: cursor }));
    await rdControl(director, TOKEN, { ForceEnd: { heat: HEAT } });
    await waitForFrame(frames, (f) =>
      f.some((x) => (x as { Change?: unknown }).Change !== undefined)
    );
    expect((frames[0] as { code?: string }).code).not.toBe('VersionMismatch');
    ws.close();
  });
});

describe('seam 8 (#422): the resume cursor is stated by the server, and a resume never goes backwards', () => {
  const RESUME_HEAT = 'q-422';

  /** Every `Change` envelope a raw frame array has collected. */
  const envelopesOf = (frames: unknown[]): Array<{ sequence: number; cursor: number }> =>
    frames
      .map((f) => (f as { Change?: { sequence: number; cursor: number } }).Change)
      .filter((e): e is { sequence: number; cursor: number } => e !== undefined);

  /** The heat phase a `Change` envelope's fresh-value body reports. */
  const phaseOfEnvelope = (frame: unknown): string | undefined =>
    (
      frame as {
        Change?: { change?: { FreshValue?: { LiveRaceState?: { phase: string } } } };
      }
    ).Change?.change?.FreshValue?.LiveRaceState?.phase;

  /**
   * Wait until `frames` stops growing for `quietMs` — the log is quiescent and the stream has
   * delivered everything it is going to. The Director's simulator keeps appending passes while a
   * heat runs, so "the tail" is only a fixed number once the run has settled.
   */
  const settle = async (frames: unknown[], quietMs = 400): Promise<void> => {
    let seen = -1;
    while (seen !== frames.length) {
      seen = frames.length;
      await new Promise((r) => setTimeout(r, quietMs));
    }
  };

  it('an envelope states the log offset it was folded through — not the sequence, not a +1 count', async () => {
    const ack = await rdControl(director, TOKEN, {
      ScheduleHeat: { heat: RESUME_HEAT, lineup: ['R', 'S'] }
    });
    expect(ack.ok).toBe(true);
    const base = await snapshotCursor(`/snapshot/heat/${RESUME_HEAT}`);

    const { ws, frames } = await openSocket(`${wsBase(director.eventRoot)}/stream`);
    ws.send(JSON.stringify({ scope: { Heat: { heat: RESUME_HEAT } }, from: base }));

    // Appends that move NO projection for this scope — the drift-wideners. They emit nothing,
    // so a client counting applied envelopes can never see them.
    for (let i = 0; i < 5; i++) {
      await rdControl(director, TOKEN, {
        Register: { adapter: 'sim', competitor: `r${i}`, pilot: `rp${i}` }
      });
    }
    // …then one that does.
    await rdControl(director, TOKEN, { Stage: { heat: RESUME_HEAT } });
    await waitForFrame(frames, (f) => f.some((x) => phaseOfEnvelope(x) === 'Staged'));
    ws.close();

    const seen = envelopesOf(frames);
    expect(seen[0].sequence).toBe(1); // the ordering axis restarts, as ever
    const last = seen.at(-1)!;
    // The drift, stated on the wire: the log advanced by MORE offsets than this stream emitted
    // envelopes, because the appends in between moved no projection. `cursor - base` is the true
    // advance; `seen.length` is everything a +1-per-applied-envelope tracker could ever count.
    expect(last.cursor - base).toBeGreaterThan(seen.length);
    expect(last.cursor).not.toBe(last.sequence);
  });

  it('a resume from the stated cursor replays nothing; a resume from a stale one gets ONE settled fold', async () => {
    const stale = await snapshotCursor(`/snapshot/heat/${RESUME_HEAT}`);

    // Run the heat from a live subscription, let the simulator's passes land, and keep the cursor
    // the last envelope states — this client's exact position.
    const live = await openSocket(`${wsBase(director.eventRoot)}/stream`);
    live.ws.send(JSON.stringify({ scope: { Heat: { heat: RESUME_HEAT } }, from: stale }));
    await rdControl(director, TOKEN, { Start: { heat: RESUME_HEAT } });
    await rdControl(director, TOKEN, { SkipCountdown: { heat: RESUME_HEAT } });
    await waitForFrame(live.frames, (f) => f.some((x) => phaseOfEnvelope(x) === 'Running'));
    await settle(live.frames);
    const stated = envelopesOf(live.frames).at(-1)!.cursor;
    live.ws.close();

    // 1. Resuming from the STATED cursor: the client is exactly where the server left it, so the
    //    server has nothing to replay. The next real change is the first thing it sends.
    const exact = await openSocket(`${wsBase(director.eventRoot)}/stream`);
    exact.ws.send(JSON.stringify({ scope: { Heat: { heat: RESUME_HEAT } }, from: stated }));
    await new Promise((r) => setTimeout(r, 400));
    expect(envelopesOf(exact.frames)).toHaveLength(0);
    await rdControl(director, TOKEN, { ForceEnd: { heat: RESUME_HEAT } });
    await waitForFrame(exact.frames, (f) => envelopesOf(f).length > 0);
    expect(phaseOfEnvelope(exact.frames[0])).toBe('Unofficial');
    await settle(exact.frames);
    exact.ws.close();

    // 2. Resuming from the STALE cursor — the drifted position the old client would have
    //    presented. It is in-window, so the server replays rather than refusing; that replay must
    //    be ONE settled fold at the tail, not an envelope per offset ending there. The staircase
    //    is what made a live lap count step backwards on screen after a socket blip.
    const drifted = await openSocket(`${wsBase(director.eventRoot)}/stream`);
    drifted.ws.send(JSON.stringify({ scope: { Heat: { heat: RESUME_HEAT } }, from: stale }));
    await waitForFrame(drifted.frames, (f) => envelopesOf(f).length > 0);
    await settle(drifted.frames);
    const replayed = envelopesOf(drifted.frames);
    expect(replayed).toHaveLength(1);
    // …and that one fold is the CURRENT state, never an intermediate phase the heat has left.
    expect(phaseOfEnvelope(drifted.frames[0])).toBe('Unofficial');
    drifted.ws.close();
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
