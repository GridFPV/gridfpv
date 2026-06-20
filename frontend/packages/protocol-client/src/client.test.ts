import { describe, expect, it } from 'vitest';
import type {
  ChangeEnvelope,
  HeatPhase,
  ProjectionBody,
  ProtocolError,
  Scope
} from '@gridfpv/types';
import { connect } from './client.js';
import type { FetchLike, WebSocketLike } from './client.js';

// ── Test fixtures ────────────────────────────────────────────────────────────

const SCOPE: Scope = { Heat: { heat: 'heat-1' } };

const liveState = (phase: HeatPhase): ProjectionBody => ({
  LiveRaceState: { current_heat: 'heat-1', phase }
});

// A fetch mock that serves the given snapshots in order (sticking on the last)
// and records every requested URL. Each response's `json()` yields a Snapshot
// whose cursor is a JSON number — exactly how serde renders the u64 `Cursor` on the
// wire, and exactly the `number` the client now works with.
function mockFetch(snapshots: Array<{ cursor: number; body: ProjectionBody }>): {
  fetch: FetchLike;
  calls: string[];
} {
  const calls: string[] = [];
  let i = 0;
  const fetch: FetchLike = async (input) => {
    calls.push(String(input));
    const snap = snapshots[Math.min(i, snapshots.length - 1)];
    i += 1;
    return {
      ok: true,
      status: 200,
      json: async (): Promise<unknown> => ({ cursor: snap.cursor, body: snap.body })
    } as unknown as Response;
  };
  return { fetch, calls };
}

// A scriptable mock WebSocket. Tests drive it: open it, then push frames.
class MockWebSocket implements WebSocketLike {
  onopen: ((this: WebSocketLike, ev: unknown) => unknown) | null = null;
  onclose: ((this: WebSocketLike, ev: unknown) => unknown) | null = null;
  onerror: ((this: WebSocketLike, ev: unknown) => unknown) | null = null;
  onmessage: ((this: WebSocketLike, ev: { data: unknown }) => unknown) | null = null;

  readonly url: string;
  readonly sent: string[] = [];
  closed = false;

  constructor(url: string) {
    this.url = url;
  }

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.closed = true;
  }

  // ── Test drivers ──
  open(): void {
    this.onopen?.call(this, {});
  }
  emit(frame: unknown): void {
    // Mirror the wire: serde renders the u64 Cursor as a JSON number, which is just
    // a plain `number` here — a straight JSON.stringify matches the wire.
    const data = JSON.stringify(frame);
    this.onmessage?.call(this, { data });
  }
  drop(): void {
    this.onclose?.call(this, {});
  }
}

// Collect every MockWebSocket the client opens, so tests can drive the latest one.
function mockWsFactory(): { factory: (url: string) => WebSocketLike; sockets: MockWebSocket[] } {
  const sockets: MockWebSocket[] = [];
  const factory = (url: string): WebSocketLike => {
    const ws = new MockWebSocket(url);
    sockets.push(ws);
    return ws;
  };
  return { factory, sockets };
}

// A controllable timer: tests fire the queued callback on demand (deterministic
// reconnect, no real waiting).
function manualTimer(): {
  setTimer: (cb: () => void, ms: number) => unknown;
  clearTimer: (h: unknown) => void;
  fire: () => void;
  pending: () => boolean;
} {
  let queued: (() => void) | null = null;
  return {
    setTimer: (cb) => {
      queued = cb;
      return 1;
    },
    clearTimer: () => {
      queued = null;
    },
    fire: () => {
      const cb = queued;
      queued = null;
      cb?.();
    },
    pending: () => queued !== null
  };
}

const envelope = (sequence: number, phase: HeatPhase): ChangeEnvelope => ({
  sequence,
  projection: 'LiveRaceState',
  change: { FreshValue: liveState(phase) }
});

/**
 * Wrap an envelope as the `StreamMessage` the server actually sends on the wire:
 * `{ Change: ChangeEnvelope }` (externally tagged). The client must unwrap it — a
 * raw, unwrapped envelope was the shape these mocks used before, which masked the
 * client ignoring every real (wrapped) frame.
 */
const change = (sequence: number, phase: HeatPhase) => ({ Change: envelope(sequence, phase) });

// Let queued microtasks (the async snapshot fetch) settle.
const flush = async (): Promise<void> => {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
};

const phaseOf = (body: ProjectionBody | undefined): HeatPhase | undefined =>
  body && 'LiveRaceState' in body ? body.LiveRaceState.phase : undefined;

// ── Tests ────────────────────────────────────────────────────────────────────

describe('ProtocolClient', () => {
  it('fetches the scoped snapshot, then subscribes from its cursor', async () => {
    const { fetch, calls } = mockFetch([{ cursor: 10, body: liveState('Staged') }]);
    const { factory, sockets } = mockWsFactory();

    const client = connect({
      baseUrl: 'http://director.local:8080',
      scope: SCOPE,
      fetch,
      webSocketFactory: factory
    });
    await flush();

    // Snapshot was fetched (path-scoped, matching the server's routes) and applied.
    expect(calls).toHaveLength(1);
    expect(calls[0]).toContain('/snapshot/heat/heat-1');
    expect(client.getState().cursor).toBe(10);
    expect(phaseOf(client.getState().body)).toBe('Staged');

    // A socket was opened; on open it sends a SubscribeRequest resuming from 10.
    expect(sockets).toHaveLength(1);
    sockets[0].open();
    expect(sockets[0].sent).toHaveLength(1);
    const req = JSON.parse(sockets[0].sent[0]);
    expect(req.scope).toEqual(SCOPE);
    expect(req.from).toBe(10); // cursor serialized as a JSON number (serde u64 default)
    expect(client.getState().status).toBe('live');

    client.close();
  });

  it('applies an ordered change stream regardless of the snapshot cursor value', async () => {
    // Regression (the freeze): the snapshot cursor is a log OFFSET (here 5), while the
    // stream's `sequence` is its own axis starting at 1. The client must order the
    // stream by `sequence` — conflating it with the cursor (`seq <= 5` → "duplicate")
    // dropped the early stream and froze the live view against any non-empty log,
    // which is every real Director snapshot. Earlier tests used cursor 0, masking it.
    const { fetch } = mockFetch([{ cursor: 5, body: liveState('Scheduled') }]);
    const { factory, sockets } = mockWsFactory();
    const client = connect({ baseUrl: 'http://d', scope: SCOPE, fetch, webSocketFactory: factory });
    await flush();
    sockets[0].open();

    sockets[0].emit(change(1, 'Staged'));
    sockets[0].emit(change(2, 'Armed'));
    sockets[0].emit(change(3, 'Running'));

    // Every envelope applied → body converged. The resume cursor stays the snapshot
    // offset (the stream sequence is not a log offset).
    expect(phaseOf(client.getState().body)).toBe('Running');
    expect(client.getState().cursor).toBe(5);

    client.close();
  });

  it('is idempotent: re-delivered envelopes at/below the cursor are no-ops', async () => {
    const { fetch } = mockFetch([{ cursor: 0, body: liveState('Scheduled') }]);
    const { factory, sockets } = mockWsFactory();
    const client = connect({ baseUrl: 'http://d', scope: SCOPE, fetch, webSocketFactory: factory });
    await flush();
    sockets[0].open();

    sockets[0].emit(change(1, 'Staged'));
    sockets[0].emit(change(2, 'Armed'));
    // Re-deliver 1 and 2 (at-least-once): they're at/below the last applied sequence
    // (2), so they're no-ops and must not regress the state back to 'Scheduled'.
    sockets[0].emit(change(1, 'Scheduled'));
    sockets[0].emit(change(2, 'Scheduled'));

    expect(phaseOf(client.getState().body)).toBe('Armed');

    client.close();
  });

  it('re-snapshots on a sequence gap, then resumes from the fresh cursor', async () => {
    const { fetch, calls } = mockFetch([
      { cursor: 0, body: liveState('Scheduled') }, // initial snapshot
      { cursor: 5, body: liveState('Running') } // re-snapshot after the gap
    ]);
    const { factory, sockets } = mockWsFactory();
    const client = connect({ baseUrl: 'http://d', scope: SCOPE, fetch, webSocketFactory: factory });
    await flush();
    sockets[0].open();

    sockets[0].emit(change(1, 'Staged'));
    // Gap: jump to 4 (missed 2 and 3). The client must re-snapshot.
    sockets[0].emit(change(4, 'Armed'));
    await flush();

    // A second snapshot fetch happened and the old socket was torn down.
    expect(calls).toHaveLength(2);
    expect(sockets[0].closed).toBe(true);
    expect(client.getState().cursor).toBe(5);
    expect(phaseOf(client.getState().body)).toBe('Running');

    // A fresh socket re-subscribes from the new cursor (5).
    expect(sockets).toHaveLength(2);
    sockets[1].open();
    const req = JSON.parse(sockets[1].sent[0]);
    expect(req.from).toBe(5);

    // The fresh subscription restarts the per-stream sequence, so its first envelope
    // is accepted and the body converges. The resume cursor stays the re-snapshot
    // offset (5).
    sockets[1].emit(change(6, 'Finished'));
    expect(phaseOf(client.getState().body)).toBe('Finished');
    expect(client.getState().cursor).toBe(5);

    client.close();
  });

  it('re-snapshots when the server reports a stale cursor', async () => {
    const { fetch, calls } = mockFetch([
      { cursor: 100, body: liveState('Staged') },
      { cursor: 200, body: liveState('Running') }
    ]);
    const { factory, sockets } = mockWsFactory();
    const client = connect({ baseUrl: 'http://d', scope: SCOPE, fetch, webSocketFactory: factory });
    await flush();
    sockets[0].open();

    const staleErr: ProtocolError = { code: 'StaleCursor', message: 'cursor too old to replay' };
    sockets[0].emit({ ReSnapshotRequired: staleErr });
    await flush();

    expect(calls).toHaveLength(2);
    expect(client.getState().cursor).toBe(200);
    expect(sockets).toHaveLength(2);

    client.close();
  });

  it('reconnects on socket drop and resumes from the last-applied cursor', async () => {
    const { fetch, calls } = mockFetch([{ cursor: 0, body: liveState('Scheduled') }]);
    const { factory, sockets } = mockWsFactory();
    const timer = manualTimer();
    const client = connect({
      baseUrl: 'http://d',
      scope: SCOPE,
      fetch,
      webSocketFactory: factory,
      setTimer: timer.setTimer,
      clearTimer: timer.clearTimer,
      reconnectDelayMs: 5
    });
    await flush();
    sockets[0].open();
    sockets[0].emit(change(1, 'Staged'));
    sockets[0].emit(change(2, 'Armed'));

    // Socket drops.
    sockets[0].drop();
    expect(client.getState().status).toBe('reconnecting');
    expect(timer.pending()).toBe(true);

    // Reconnect fires: it resumes by re-subscribing (no re-snapshot needed).
    timer.fire();
    await flush();
    expect(calls).toHaveLength(1); // no extra snapshot — resumed by cursor
    expect(sockets).toHaveLength(2);

    sockets[1].open();
    const req = JSON.parse(sockets[1].sent[0]);
    // Resume from the snapshot's log offset (0) — NOT the stream sequence (which is a
    // different axis). The server replays from there and fresh-value envelopes
    // re-converge. (A future enhancement: carry the log offset on envelopes so the
    // resume point can advance; until then resume re-replays from the snapshot.)
    expect(req.from).toBe(0);
    expect(client.getState().status).toBe('live');

    // The resumed subscription restarts the sequence; its first envelope converges.
    sockets[1].emit(change(1, 'Running'));
    expect(phaseOf(client.getState().body)).toBe('Running');

    client.close();
  });

  it('notifies onState listeners and stops after close', async () => {
    const { fetch } = mockFetch([{ cursor: 0, body: liveState('Scheduled') }]);
    const { factory, sockets } = mockWsFactory();
    const client = connect({ baseUrl: 'http://d', scope: SCOPE, fetch, webSocketFactory: factory });

    const seen: (HeatPhase | undefined)[] = [];
    const unsub = client.onState((s) => seen.push(phaseOf(s.body)));
    await flush();
    sockets[0].open();
    sockets[0].emit(change(1, 'Staged'));

    expect(seen).toContain('Scheduled');
    expect(seen).toContain('Staged');

    unsub();
    const before = seen.length;
    sockets[0].emit(change(2, 'Armed'));
    expect(seen.length).toBe(before); // unsubscribed: no more notifications

    client.close();
    expect(client.getState().status).toBe('closed');
  });

  // ── Event-rooted surface (issue #72) ─────────────────────────────────────────

  it('roots the snapshot + stream URLs under the default Practice event', async () => {
    const { fetch, calls } = mockFetch([{ cursor: 3, body: liveState('Scheduled') }]);
    const { factory, sockets } = mockWsFactory();

    const client = connect({
      baseUrl: 'http://director.local:8080',
      scope: SCOPE,
      fetch,
      webSocketFactory: factory
    });
    await flush();

    // No eventId given → both the snapshot and the WS are rooted under `/events/practice`.
    expect(calls[0]).toBe('http://director.local:8080/events/practice/snapshot/heat/heat-1');
    expect(sockets[0].url).toBe('ws://director.local:8080/events/practice/stream');

    client.close();
  });

  it('roots the URLs under an explicit eventId when given', async () => {
    const { fetch, calls } = mockFetch([{ cursor: 0, body: liveState('Scheduled') }]);
    const { factory, sockets } = mockWsFactory();

    const client = connect({
      baseUrl: 'http://director.local:8080',
      eventId: 'spring-cup-2026-ab12',
      scope: SCOPE,
      fetch,
      webSocketFactory: factory
    });
    await flush();

    expect(calls[0]).toBe(
      'http://director.local:8080/events/spring-cup-2026-ab12/snapshot/heat/heat-1'
    );
    expect(sockets[0].url).toBe('ws://director.local:8080/events/spring-cup-2026-ab12/stream');

    client.close();
  });
});

describe('events lifecycle helpers (#72)', () => {
  it('listEvents GETs /events and returns the EventMeta list', async () => {
    const calls: string[] = [];
    const fetch: FetchLike = async (input) => {
      calls.push(String(input));
      return {
        ok: true,
        status: 200,
        json: async (): Promise<unknown> => [
          { id: 'practice', name: 'Practice', created_at: 1, persistent: false }
        ]
      } as unknown as Response;
    };
    const { listEvents } = await import('./client.js');
    const events = await listEvents('http://director.local:8080/', { fetch });
    expect(calls[0]).toBe('http://director.local:8080/events');
    expect(events[0].id).toBe('practice');
  });

  it('createEvent POSTs the name to /events with the RD token and returns the new EventMeta', async () => {
    const seen: { url: string; init?: RequestInit }[] = [];
    const fetch: FetchLike = async (input, init) => {
      seen.push({ url: String(input), init });
      return {
        ok: true,
        status: 200,
        json: async (): Promise<unknown> => ({
          id: 'race-night-xy99',
          name: 'Race Night',
          created_at: 2,
          persistent: true
        })
      } as unknown as Response;
    };
    const { createEvent } = await import('./client.js');
    const meta = await createEvent('http://director.local:8080', 'Race Night', 'rd-tok', { fetch });
    expect(seen[0].url).toBe('http://director.local:8080/events');
    expect(seen[0].init?.method).toBe('POST');
    const headers = seen[0].init?.headers as Record<string, string>;
    expect(headers.Authorization).toBe('Bearer rd-tok');
    expect(JSON.parse(seen[0].init?.body as string)).toEqual({ name: 'Race Night' });
    expect(meta.id).toBe('race-night-xy99');
    expect(meta.persistent).toBe(true);
  });
});
