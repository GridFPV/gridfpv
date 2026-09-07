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

/** The event every test connection is rooted under — explicit since #414 removed the default. */
const EVENT = 'test-event-ab12';

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

/**
 * The `cursor` a fixture envelope carries when a test does not care about the resume
 * axis. Deliberately far from any `sequence` these tests use: the two are different
 * axes (#422 / seam 3), and a fixture that let them coincide would hide a client
 * conflating them.
 */
const someOffset = (sequence: number): number => 1000 + sequence;

const envelope = (
  sequence: number,
  phase: HeatPhase,
  cursor = someOffset(sequence)
): ChangeEnvelope => ({
  sequence,
  cursor,
  projection: 'LiveRaceState',
  change: { FreshValue: liveState(phase) }
});

/**
 * Wrap an envelope as the `StreamMessage` the server actually sends on the wire:
 * `{ Change: ChangeEnvelope }` (externally tagged). The client must unwrap it — a
 * raw, unwrapped envelope was the shape these mocks used before, which masked the
 * client ignoring every real (wrapped) frame.
 *
 * `cursor` is the log offset the server folded that body through (#422). It is what the
 * client must store as its resume position — never a count of envelopes it applied.
 */
const change = (sequence: number, phase: HeatPhase, cursor = someOffset(sequence)) => ({
  Change: envelope(sequence, phase, cursor)
});

/**
 * A `Delta` change envelope, wrapped as the wire `StreamMessage`. The per-projection
 * delta encodings are deferred (#43), so the client cannot fold one into `body` —
 * an in-order delta must fail safe (re-snapshot), never freeze the view silently.
 */
const deltaChange = (sequence: number, cursor = someOffset(sequence)) => ({
  Change: {
    sequence,
    cursor,
    projection: 'LiveRaceState',
    change: { Delta: { appended: 'lap' } }
  }
});

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
      eventId: EVENT,
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
    const client = connect({
      baseUrl: 'http://d',
      eventId: EVENT,
      scope: SCOPE,
      fetch,
      webSocketFactory: factory
    });
    await flush();
    sockets[0].open();

    // The three envelopes were folded through log offsets 6, 7 and 8 — the server states
    // each one; the stream sequence (1, 2, 3) is the other axis entirely.
    sockets[0].emit(change(1, 'Staged', 6));
    sockets[0].emit(change(2, 'Armed', 7));
    sockets[0].emit(change(3, 'Running', 8));

    // Every envelope applied → body converged. The resume cursor is the last envelope's
    // OWN offset (8), so a reconnect resumes exactly there rather than replaying from the
    // snapshot offset. It is still NOT the stream sequence (a different axis).
    expect(phaseOf(client.getState().body)).toBe('Running');
    expect(client.getState().cursor).toBe(8);

    client.close();
  });

  it('is idempotent: re-delivered envelopes at/below the cursor are no-ops', async () => {
    const { fetch } = mockFetch([{ cursor: 0, body: liveState('Scheduled') }]);
    const { factory, sockets } = mockWsFactory();
    const client = connect({
      baseUrl: 'http://d',
      eventId: EVENT,
      scope: SCOPE,
      fetch,
      webSocketFactory: factory
    });
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
    const client = connect({
      baseUrl: 'http://d',
      eventId: EVENT,
      scope: SCOPE,
      fetch,
      webSocketFactory: factory
    });
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
    // is accepted and the body converges. The resume cursor moves to that envelope's own
    // offset (6), past the re-snapshot's 5.
    sockets[1].emit(change(6, 'Unofficial', 6));
    expect(phaseOf(client.getState().body)).toBe('Unofficial');
    expect(client.getState().cursor).toBe(6);

    client.close();
  });

  it('re-snapshots when the server reports a stale cursor', async () => {
    const { fetch, calls } = mockFetch([
      { cursor: 100, body: liveState('Staged') },
      { cursor: 200, body: liveState('Running') }
    ]);
    const { factory, sockets } = mockWsFactory();
    const client = connect({
      baseUrl: 'http://d',
      eventId: EVENT,
      scope: SCOPE,
      fetch,
      webSocketFactory: factory
    });
    await flush();
    sockets[0].open();

    // An applied envelope moves the resume cursor off the snapshot offset (100 → 101)…
    sockets[0].emit(change(1, 'Armed', 101));
    expect(client.getState().cursor).toBe(101);

    const staleErr: ProtocolError = { code: 'StaleCursor', message: 'cursor too old to replay' };
    sockets[0].emit({ ReSnapshotRequired: staleErr });
    await flush();

    // …and the stale-cursor fallback re-seeds it wholesale from the fresh snapshot (200):
    // re-snapshot remains the authority, whatever the advanced cursor said.
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
      eventId: EVENT,
      scope: SCOPE,
      fetch,
      webSocketFactory: factory,
      setTimer: timer.setTimer,
      clearTimer: timer.clearTimer,
      reconnectDelayMs: 5
    });
    await flush();
    sockets[0].open();
    // Two envelopes, folded through log offsets 1 and 5 — offsets 2, 3 and 4 were appends
    // that moved no projection (a signal chunk, a marshaling no-op) and so emitted nothing.
    sockets[0].emit(change(1, 'Staged', 1));
    sockets[0].emit(change(2, 'Armed', 5));

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
    // Resume from the LAST-APPLIED position, as the SERVER stated it: the second envelope
    // was folded through offset 5, so that is the `from`. Counting applied envelopes would
    // have said 2 — three offsets short — and the server would have replayed 3, 4 and 5,
    // pushing an older fold through onState before climbing back (#422). The re-subscribe
    // also cannot age out of the retained window (StaleCursor) while envelopes keep applying.
    expect(req.from).toBe(5);
    expect(client.getState().status).toBe('live');

    // The resumed subscription restarts the sequence; its first envelope converges.
    sockets[1].emit(change(1, 'Running'));
    expect(phaseOf(client.getState().body)).toBe('Running');

    client.close();
  });

  it('#422: the resume cursor is the offset the server echoed, never a count of applied envelopes', async () => {
    // The bug, at its source. The wire echoed no offset, so this client advanced `cursor`
    // by one per APPLIED envelope and documented it as "conservative (at-or-behind the true
    // offset)". Every log append that moved no projection — a SignalHistory chunk, a
    // CompetitorSeen, a marshaling no-op — emitted nothing, so it was never counted, and the
    // gap between the cursor and the true tail grew without bound. A reconnect then resumed
    // from `tail - drift`, which is INSIDE the server's retained window, so the stream
    // replayed instead of asking for a re-snapshot: `body` was overwritten with an older
    // fold (fewer laps) and climbed back through every intermediate one. On the Race
    // Director's board a pilot lost laps and regained them, mid-race, with no operator
    // action — indistinguishable from a marshal voiding a pass.
    const { fetch } = mockFetch([{ cursor: 40, body: liveState('Scheduled') }]);
    const { factory, sockets } = mockWsFactory();
    const client = connect({
      baseUrl: 'http://d',
      eventId: EVENT,
      scope: SCOPE,
      fetch,
      webSocketFactory: factory
    });
    await flush();
    sockets[0].open();

    // Three envelopes across a log that advanced by 30 offsets: the ones in between moved
    // no projection. A +1 tracker would say 43; the truth is 70.
    sockets[0].emit(change(1, 'Staged', 47));
    expect(client.getState().cursor).toBe(47);
    sockets[0].emit(change(2, 'Armed', 61));
    sockets[0].emit(change(3, 'Running', 70));

    expect(client.getState().cursor).toBe(70);
    expect(client.getState().cursor).not.toBe(43); // what counting envelopes would have said

    // A duplicate redelivery must not move the cursor at all — it is not applied.
    sockets[0].emit(change(2, 'Scheduled', 61));
    expect(client.getState().cursor).toBe(70);
    expect(phaseOf(client.getState().body)).toBe('Running');

    client.close();
  });

  it('#422: falls back to the old lower-bound advance against a server that echoes no offset', async () => {
    // A Director too old to carry `ChangeEnvelope.cursor` still has to resume somewhere.
    // Losing the resume position outright would re-present the original snapshot offset on
    // every blip (or age out of the retained window); the pre-#422 `+1` advance is the
    // graceful degradation. Its stream still replays on reconnect — that is the bug this
    // field fixes — but nothing here may re-derive an offset the server did not state.
    const { fetch } = mockFetch([{ cursor: 10, body: liveState('Scheduled') }]);
    const { factory, sockets } = mockWsFactory();
    const client = connect({
      baseUrl: 'http://d',
      eventId: EVENT,
      scope: SCOPE,
      fetch,
      webSocketFactory: factory
    });
    await flush();
    sockets[0].open();

    const legacy = (sequence: number, phase: HeatPhase) => ({
      Change: { sequence, projection: 'LiveRaceState', change: { FreshValue: liveState(phase) } }
    });
    sockets[0].emit(legacy(1, 'Staged'));
    sockets[0].emit(legacy(2, 'Armed'));

    expect(phaseOf(client.getState().body)).toBe('Armed');
    expect(client.getState().cursor).toBe(12); // 10 + the two applied envelopes

    client.close();
  });

  it('fails safe on an unhandled Delta envelope: re-snapshot, never a silent freeze', async () => {
    // The per-projection delta encodings are deferred (#43): the client cannot fold a
    // `Delta` into `body`. Advancing the sequence without the mutation would freeze
    // the view while `status` reads 'live' — so an in-order delta must take the same
    // re-snapshot path a StaleCursor does.
    const { fetch, calls } = mockFetch([
      { cursor: 3, body: liveState('Scheduled') }, // initial snapshot
      { cursor: 9, body: liveState('Running') } // re-snapshot forced by the delta
    ]);
    const { factory, sockets } = mockWsFactory();
    const client = connect({
      baseUrl: 'http://d',
      eventId: EVENT,
      scope: SCOPE,
      fetch,
      webSocketFactory: factory
    });
    await flush();
    sockets[0].open();

    sockets[0].emit(deltaChange(1));
    await flush();

    // The delta triggered a re-snapshot: a second fetch, the old socket torn down,
    // and the state is the FRESH snapshot's (current), not a frozen 'Scheduled'.
    expect(calls).toHaveLength(2);
    expect(sockets[0].closed).toBe(true);
    expect(phaseOf(client.getState().body)).toBe('Running');
    expect(client.getState().cursor).toBe(9);

    // A fresh socket re-subscribes from the re-snapshot cursor.
    expect(sockets).toHaveLength(2);
    sockets[1].open();
    expect(JSON.parse(sockets[1].sent[0]).from).toBe(9);
    expect(client.getState().status).toBe('live');

    client.close();
  });

  it('a re-delivered Delta at/below the applied sequence is a duplicate no-op (no re-snapshot)', async () => {
    const { fetch, calls } = mockFetch([{ cursor: 0, body: liveState('Scheduled') }]);
    const { factory, sockets } = mockWsFactory();
    const client = connect({
      baseUrl: 'http://d',
      eventId: EVENT,
      scope: SCOPE,
      fetch,
      webSocketFactory: factory
    });
    await flush();
    sockets[0].open();

    sockets[0].emit(change(1, 'Staged', 1));
    sockets[0].emit(change(2, 'Armed', 2));
    // At-least-once redelivery of an already-applied sequence as a Delta: it is deduped
    // by sequence BEFORE the unsupported-delta check, so no re-snapshot fires.
    sockets[0].emit(deltaChange(2));
    await flush();

    expect(calls).toHaveLength(1); // no extra snapshot fetch
    expect(sockets).toHaveLength(1); // the socket stayed up
    expect(phaseOf(client.getState().body)).toBe('Armed');
    expect(client.getState().cursor).toBe(2); // the last APPLIED envelope's own offset

    client.close();
  });

  it('notifies onState listeners and stops after close', async () => {
    const { fetch } = mockFetch([{ cursor: 0, body: liveState('Scheduled') }]);
    const { factory, sockets } = mockWsFactory();
    const client = connect({
      baseUrl: 'http://d',
      eventId: EVENT,
      scope: SCOPE,
      fetch,
      webSocketFactory: factory
    });

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

  it('roots the snapshot + stream URLs under the named event', async () => {
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
          { id: 'spring-cup-2026-ab12', name: 'Spring Cup', created_at: 1, persistent: true }
        ]
      } as unknown as Response;
    };
    const { listEvents } = await import('./client.js');
    const events = await listEvents('http://director.local:8080/', { fetch });
    expect(calls[0]).toBe('http://director.local:8080/events');
    expect(events[0].id).toBe('spring-cup-2026-ab12');
  });

  it('listEvents accepts an EMPTY list — a fresh Director has no events (#414)', async () => {
    const fetch: FetchLike = async () =>
      ({ ok: true, status: 200, json: async (): Promise<unknown> => [] }) as unknown as Response;
    const { listEvents } = await import('./client.js');
    expect(await listEvents('http://director.local:8080/', { fetch })).toEqual([]);
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

  it('createEvent omits Authorization when no token is given (full-trust open Director) and forwards optional fields', async () => {
    const seen: { url: string; init?: RequestInit }[] = [];
    const fetch: FetchLike = async (input, init) => {
      seen.push({ url: String(input), init });
      return {
        ok: true,
        status: 200,
        json: async (): Promise<unknown> => ({
          id: 'spring-cup-ab12',
          name: 'Spring Cup',
          created_at: 3,
          persistent: true,
          date: '2026-06-20',
          location: 'Main field'
        })
      } as unknown as Response;
    };
    const { createEvent } = await import('./client.js');
    const meta = await createEvent('http://director.local:8080', 'Spring Cup', undefined, {
      fetch,
      fields: { date: '2026-06-20', location: 'Main field' }
    });
    const headers = seen[0].init?.headers as Record<string, string>;
    expect(headers.Authorization).toBeUndefined();
    expect(JSON.parse(seen[0].init?.body as string)).toEqual({
      name: 'Spring Cup',
      date: '2026-06-20',
      location: 'Main field'
    });
    expect(meta.date).toBe('2026-06-20');
    expect(meta.location).toBe('Main field');
  });

  // ── #73: application-level timers + per-event selection ───────────────────────

  it('listTimers GETs /timers and returns the Timer list', async () => {
    const calls: string[] = [];
    const fetch: FetchLike = async (input) => {
      calls.push(String(input));
      return {
        ok: true,
        status: 200,
        json: async (): Promise<unknown> => [
          {
            id: 'mock',
            name: 'Mock',
            kind: { Mock: { laps: 5, lap_ms: 2500 } },
            status: 'Ready'
          }
        ]
      } as unknown as Response;
    };
    const { listTimers } = await import('./client.js');
    const timers = await listTimers('http://director.local:8080/', { fetch });
    expect(calls[0]).toBe('http://director.local:8080/timers');
    expect(timers[0].id).toBe('mock');
  });

  it('createTimer POSTs the request to /timers with the RD token and returns the new Timer', async () => {
    const seen: { url: string; init?: RequestInit }[] = [];
    const fetch: FetchLike = async (input, init) => {
      seen.push({ url: String(input), init });
      return {
        ok: true,
        status: 200,
        json: async (): Promise<unknown> => ({
          id: 'field-rh-ab12',
          name: 'Field RH',
          kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
          status: 'Configured'
        })
      } as unknown as Response;
    };
    const { createTimer } = await import('./client.js');
    const timer = await createTimer(
      'http://director.local:8080',
      { name: 'Field RH', kind: { Rotorhazard: { url: 'http://rh.local:5000' } } },
      'rd-tok',
      { fetch }
    );
    expect(seen[0].url).toBe('http://director.local:8080/timers');
    expect(seen[0].init?.method).toBe('POST');
    expect((seen[0].init?.headers as Record<string, string>).Authorization).toBe('Bearer rd-tok');
    expect(timer.id).toBe('field-rh-ab12');
  });

  it('updateTimer PUTs to /timers/{id} and deleteTimer DELETEs it', async () => {
    const seen: { url: string; method?: string }[] = [];
    const fetch: FetchLike = async (input, init) => {
      seen.push({ url: String(input), method: init?.method });
      return {
        ok: true,
        status: 200,
        json: async (): Promise<unknown> => ({
          id: 'mock',
          name: 'Mock',
          kind: { Mock: { laps: 9, lap_ms: 100 } },
          status: 'Ready'
        })
      } as unknown as Response;
    };
    const { updateTimer, deleteTimer } = await import('./client.js');
    const updated = await updateTimer(
      'http://director.local:8080',
      'mock',
      { kind: { Mock: { laps: 9, lap_ms: 100 } } },
      'rd-tok',
      { fetch }
    );
    expect(seen[0].url).toBe('http://director.local:8080/timers/mock');
    expect(seen[0].method).toBe('PUT');
    expect(updated.kind).toEqual({ Mock: { laps: 9, lap_ms: 100 } });

    await deleteTimer('http://director.local:8080', 'race-rh-xy99', 'rd-tok', { fetch });
    expect(seen[1].url).toBe('http://director.local:8080/timers/race-rh-xy99');
    expect(seen[1].method).toBe('DELETE');
  });

  it('setEventTimers PUTs the ids to /events/{id}/timers and returns the updated EventMeta', async () => {
    const seen: { url: string; body?: unknown }[] = [];
    const fetch: FetchLike = async (input, init) => {
      seen.push({ url: String(input), body: JSON.parse(String(init?.body)) });
      return {
        ok: true,
        status: 200,
        json: async (): Promise<unknown> => ({
          id: 'evt-a',
          name: 'Friday Series',
          created_at: 1,
          persistent: true,
          timers: ['mock', 'field-rh-ab12']
        })
      } as unknown as Response;
    };
    const { setEventTimers } = await import('./client.js');
    const meta = await setEventTimers(
      'http://director.local:8080',
      'evt-a',
      ['mock', 'field-rh-ab12'],
      'rd-tok',
      { fetch }
    );
    expect(seen[0].url).toBe('http://director.local:8080/events/evt-a/timers');
    expect(seen[0].body).toEqual({ ids: ['mock', 'field-rh-ab12'] });
    expect(meta.timers).toEqual(['mock', 'field-rh-ab12']);
  });

  it('setPrimaryTimer PUTs { id } to /events/{id}/primary-timer and returns the updated EventMeta', async () => {
    const seen: { url: string; method?: string; body?: unknown }[] = [];
    const fetch: FetchLike = async (input, init) => {
      seen.push({
        url: String(input),
        method: init?.method,
        body: JSON.parse(String(init?.body))
      });
      return {
        ok: true,
        status: 200,
        json: async (): Promise<unknown> => ({
          id: 'evt-a',
          name: 'Friday Series',
          created_at: 1,
          persistent: true,
          timers: ['mock', 'field-rh-ab12'],
          primary_timer: 'field-rh-ab12'
        })
      } as unknown as Response;
    };
    const { setPrimaryTimer } = await import('./client.js');
    const meta = await setPrimaryTimer(
      'http://director.local:8080',
      'evt-a',
      'field-rh-ab12',
      'rd-tok',
      { fetch }
    );
    expect(seen[0].url).toBe('http://director.local:8080/events/evt-a/primary-timer');
    expect(seen[0].method).toBe('PUT');
    expect(seen[0].body).toEqual({ id: 'field-rh-ab12' });
    expect(meta.primary_timer).toBe('field-rh-ab12');
  });

  it('setPrimaryTimer sends { id: null } to clear the override', async () => {
    const seen: { body?: unknown }[] = [];
    const fetch: FetchLike = async (_input, init) => {
      seen.push({ body: JSON.parse(String(init?.body)) });
      return {
        ok: true,
        status: 200,
        json: async (): Promise<unknown> => ({
          id: 'evt-a',
          name: 'Friday Series',
          created_at: 1,
          persistent: true,
          timers: ['mock', 'field-rh-ab12']
        })
      } as unknown as Response;
    };
    const { setPrimaryTimer } = await import('./client.js');
    await setPrimaryTimer('http://director.local:8080', 'evt-a', null, 'rd-tok', { fetch });
    expect(seen[0].body).toEqual({ id: null });
  });
});

// ── Request failures: the Director's words, never a route line (#433) ────────────────────────
//
// Refusing to delete a round used to reach the RD as
// `DELETE /events/{eventId}/rounds/{roundId} failed: HTTP 400` — two raw ids on screen and the
// server's explanation discarded, while the Director had written a sentence naming the heat by its
// friendly name precisely so it could be shown.

describe("request failures carry the Director's words (#433)", () => {
  const BASE = 'http://director.local:8080';
  const TOKEN = 'rd-tok';
  const EVT = 'evt-9f3a7c';
  const ROUND = 'round-4b21';
  const TIMER = 'timer-7e55';
  const PILOT = 'pilot-3c4d';
  const CLASS = 'class-88ee';
  const LAYOUT = 'layout-11aa';
  /** Every raw handle the sweep hands the client — none may come back out in a message. */
  const RAW_IDS = [EVT, ROUND, TIMER, PILOT, CLASS, LAYOUT];

  /**
   * A stand-in request body. The failing fetch never reads one, and `never` satisfies every
   * request-param type, so the sweep below needs no fixture per wire shape.
   */
  const REQ = {} as unknown as never;

  /** A fetch that fails every request — with a typed `ProtocolError` body, or with none at all. */
  const failing =
    (status: number, body?: ProtocolError): FetchLike =>
    async () =>
      ({
        ok: false,
        status,
        json: async (): Promise<unknown> => {
          // A bodyless failure is what a bare 500 (or an HTML error page) actually looks like.
          if (!body) throw new SyntaxError('Unexpected end of JSON input');
          return body;
        }
      }) as unknown as Response;

  /** Run a call expected to reject and hand back the error it threw. */
  async function caught(run: () => Promise<unknown>): Promise<Error & { status?: number }> {
    try {
      await run();
    } catch (e) {
      return e as Error & { status?: number };
    }
    throw new Error('expected the request to reject');
  }

  it("throws the Director's refusal verbatim — the sentence #433 was discarding", async () => {
    const REFUSAL =
      'this round has a heat in progress (Practice Heat) — finalize or reset it before removing the round';
    const { deleteRound, isRequestFailure } = await import('./client.js');
    const err = await caught(() =>
      deleteRound(BASE, EVT, ROUND, TOKEN, {
        fetch: failing(400, { code: 'BadRequest', message: REFUSAL })
      })
    );
    // Verbatim: not prefixed, not wrapped, not appended to.
    expect(err.message).toBe(REFUSAL);
    expect(isRequestFailure(err)).toBe(true);
    expect(err.status).toBe(400);
    expect((err as { code?: string }).code).toBe('BadRequest');
  });

  it('falls back only when there is no body, and then says what was attempted', async () => {
    const { deleteRound } = await import('./client.js');
    const err = await caught(() => deleteRound(BASE, EVT, ROUND, TOKEN, { fetch: failing(500) }));
    // Honest, actionable, and status-bearing — a poor message is fine, a silent one is not.
    expect(err.message).toBe('The Director could not remove the round (HTTP 500).');
    expect(err.status).toBe(500);
    expect((err as { code?: string }).code).toBeUndefined();
  });

  it('never puts a raw id — or a route line — in a surfaced message', async () => {
    const c = await import('./client.js');
    const probes: [string, (fetch: FetchLike) => Promise<unknown>][] = [
      ['listEvents', (fetch) => c.listEvents(BASE, { fetch })],
      ['createEvent', (fetch) => c.createEvent(BASE, 'Friday', TOKEN, { fetch })],
      ['deleteEvent', (fetch) => c.deleteEvent(BASE, EVT, TOKEN, { fetch })],
      ['getActiveEvent', (fetch) => c.getActiveEvent(BASE, { fetch })],
      ['setActiveEvent', (fetch) => c.setActiveEvent(BASE, EVT, TOKEN, { fetch })],
      ['listTimers', (fetch) => c.listTimers(BASE, { fetch })],
      ['createTimer', (fetch) => c.createTimer(BASE, REQ, TOKEN, { fetch })],
      ['updateTimer', (fetch) => c.updateTimer(BASE, TIMER, REQ, TOKEN, { fetch })],
      ['connectTimer', (fetch) => c.connectTimer(BASE, TIMER, TOKEN, { fetch })],
      ['disconnectTimer', (fetch) => c.disconnectTimer(BASE, TIMER, TOKEN, { fetch })],
      ['restartTimer', (fetch) => c.restartTimer(BASE, TIMER, TOKEN, { fetch })],
      ['timerSignal', (fetch) => c.timerSignal(BASE, TIMER, { fetch })],
      ['stopTimerSignal', (fetch) => c.stopTimerSignal(BASE, TIMER, TOKEN, { fetch })],
      ['setCalibration', (fetch) => c.setCalibration(BASE, TIMER, REQ, TOKEN, { fetch })],
      ['captureLevel', (fetch) => c.captureLevel(BASE, TIMER, REQ, TOKEN, { fetch })],
      ['timerNodes', (fetch) => c.timerNodes(BASE, TIMER, { fetch })],
      ['setTimerNodes', (fetch) => c.setTimerNodes(BASE, TIMER, REQ, TOKEN, { fetch })],
      ['setNodeChannel', (fetch) => c.setNodeChannel(BASE, TIMER, REQ, TOKEN, { fetch })],
      ['deleteTimer', (fetch) => c.deleteTimer(BASE, TIMER, TOKEN, { fetch })],
      ['setEventTimers', (fetch) => c.setEventTimers(BASE, EVT, [TIMER], TOKEN, { fetch })],
      ['setPrimaryTimer', (fetch) => c.setPrimaryTimer(BASE, EVT, TIMER, TOKEN, { fetch })],
      ['listPilots', (fetch) => c.listPilots(BASE, { fetch })],
      ['createPilot', (fetch) => c.createPilot(BASE, REQ, TOKEN, { fetch })],
      ['updatePilot', (fetch) => c.updatePilot(BASE, PILOT, REQ, TOKEN, { fetch })],
      ['deletePilot', (fetch) => c.deletePilot(BASE, PILOT, TOKEN, { fetch })],
      ['setEventRoster', (fetch) => c.setEventRoster(BASE, EVT, [PILOT], TOKEN, { fetch })],
      ['addToRoster', (fetch) => c.addToRoster(BASE, EVT, PILOT, TOKEN, { fetch })],
      ['removeFromRoster', (fetch) => c.removeFromRoster(BASE, EVT, PILOT, TOKEN, { fetch })],
      ['listClasses', (fetch) => c.listClasses(BASE, { fetch })],
      ['createClass', (fetch) => c.createClass(BASE, REQ, TOKEN, { fetch })],
      ['updateClass', (fetch) => c.updateClass(BASE, CLASS, REQ, TOKEN, { fetch })],
      ['deleteClass', (fetch) => c.deleteClass(BASE, CLASS, TOKEN, { fetch })],
      ['setClassHidden', (fetch) => c.setClassHidden(BASE, CLASS, true, TOKEN, { fetch })],
      ['setEventClasses', (fetch) => c.setEventClasses(BASE, EVT, [CLASS], TOKEN, { fetch })],
      [
        'setClassMembership',
        (fetch) => c.setClassMembership(BASE, EVT, CLASS, [PILOT], TOKEN, { fetch })
      ],
      ['listFormatSchemas', (fetch) => c.listFormatSchemas(BASE, { fetch })],
      ['listFormats', (fetch) => c.listFormats(BASE, { fetch })],
      ['listChannels', (fetch) => c.listChannels(BASE, { fetch })],
      ['rateChannels', (fetch) => c.rateChannels(BASE, [5658], { fetch })],
      ['createRound', (fetch) => c.createRound(BASE, EVT, REQ, TOKEN, { fetch })],
      ['updateRound', (fetch) => c.updateRound(BASE, EVT, ROUND, REQ, TOKEN, { fetch })],
      ['deleteRound', (fetch) => c.deleteRound(BASE, EVT, ROUND, TOKEN, { fetch })],
      ['listChannelLayouts', (fetch) => c.listChannelLayouts(BASE, EVT, { fetch })],
      ['createChannelLayout', (fetch) => c.createChannelLayout(BASE, EVT, REQ, TOKEN, { fetch })],
      [
        'updateChannelLayout',
        (fetch) => c.updateChannelLayout(BASE, EVT, LAYOUT, REQ, TOKEN, { fetch })
      ],
      [
        'deleteChannelLayout',
        (fetch) => c.deleteChannelLayout(BASE, EVT, LAYOUT, TOKEN, { fetch })
      ],
      ['listHeats', (fetch) => c.listHeats(BASE, EVT, { fetch })],
      ['listRoundIssues', (fetch) => c.listRoundIssues(BASE, EVT, { fetch })],
      ['eventAudit', (fetch) => c.eventAudit(BASE, EVT, { fetch })],
      ['roundRanking', (fetch) => c.roundRanking(BASE, EVT, ROUND, { fetch })],
      ['roundStandings', (fetch) => c.roundStandings(BASE, EVT, ROUND, { fetch })],
      ['classStandings', (fetch) => c.classStandings(BASE, EVT, CLASS, { fetch })]
    ];

    for (const [name, run] of probes) {
      const err = await caught(() => run(failing(400)));
      const surfaced = `${name}: ${err.message}`;
      for (const id of RAW_IDS) expect(surfaced).not.toContain(id);
      // No method/URL either — a route line is what carried the ids in the first place.
      expect(surfaced).not.toMatch(/\//);
      expect(surfaced).not.toMatch(/\b(GET|PUT|POST|DELETE)\b/);
      // Still honest about the status, and still branchable on it.
      expect(surfaced).toContain('HTTP 400');
      expect(err.status).toBe(400);
    }
  });

  it("prefers the Director's sentence over the fallback on every call site", async () => {
    const c = await import('./client.js');
    const SAID = 'Track RH is a simulated timer and has no signal to read';
    const fetch = failing(400, { code: 'BadRequest', message: SAID });
    const messages = await Promise.all(
      [
        () => c.timerSignal(BASE, TIMER, { fetch }),
        () => c.restartTimer(BASE, TIMER, TOKEN, { fetch }),
        () => c.deleteRound(BASE, EVT, ROUND, TOKEN, { fetch }),
        () => c.setEventRoster(BASE, EVT, [PILOT], TOKEN, { fetch }),
        () => c.roundRanking(BASE, EVT, ROUND, { fetch })
      ].map(async (run) => (await caught(run)).message)
    );
    expect(messages).toEqual([SAID, SAID, SAID, SAID, SAID]);
  });

  it('carries the status structurally so auth detection never reads the words', async () => {
    const { deleteEvent, isRequestFailure } = await import('./client.js');
    // A 401 whose message says nothing about 401 — the console's `isAuthFailure` must still fire.
    const err = await caught(() =>
      deleteEvent(BASE, EVT, undefined, {
        fetch: failing(401, { code: 'Unauthorized', message: 'Control on this Director is gated.' })
      })
    );
    expect(err.message).toBe('Control on this Director is gated.');
    expect(err.message).not.toContain('401');
    expect(isRequestFailure(err) && err.status === 401).toBe(true);
    expect((err as { code?: string }).code).toBe('Unauthorized');
  });
});
