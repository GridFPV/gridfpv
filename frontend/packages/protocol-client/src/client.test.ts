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

/**
 * A `Delta` change envelope, wrapped as the wire `StreamMessage`. The per-projection
 * delta encodings are deferred (#43), so the client cannot fold one into `body` —
 * an in-order delta must fail safe (re-snapshot), never freeze the view silently.
 */
const deltaChange = (sequence: number) => ({
  Change: {
    sequence,
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

    sockets[0].emit(change(1, 'Staged'));
    sockets[0].emit(change(2, 'Armed'));
    sockets[0].emit(change(3, 'Running'));

    // Every envelope applied → body converged. The resume cursor ADVANCES by one per
    // applied envelope (5 → 8) — it tracks the last-applied position (each envelope is
    // ≥ 1 log append), so a reconnect resumes there rather than replaying from the
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
    // is accepted and the body converges. The resume cursor advances past the
    // re-snapshot offset with the applied envelope (5 → 6).
    sockets[1].emit(change(6, 'Unofficial'));
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

    // An applied envelope advances the resume cursor off the snapshot offset (100 → 101)…
    sockets[0].emit(change(1, 'Armed'));
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
    // Resume from the LAST-APPLIED position: the resume cursor advanced by one per
    // applied envelope (snapshot offset 0 + 2 applied = 2), so the re-subscribe does
    // NOT re-present the original snapshot offset and replay the whole backlog
    // through onState (the stale-state flashes), and it cannot age out of the
    // retained window (StaleCursor) while envelopes keep applying.
    expect(req.from).toBe(2);
    expect(client.getState().status).toBe('live');

    // The resumed subscription restarts the sequence; its first envelope converges.
    sockets[1].emit(change(1, 'Running'));
    expect(phaseOf(client.getState().body)).toBe('Running');

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

    sockets[0].emit(change(1, 'Staged'));
    sockets[0].emit(change(2, 'Armed'));
    // At-least-once redelivery of an already-applied sequence as a Delta: it is deduped
    // by sequence BEFORE the unsupported-delta check, so no re-snapshot fires.
    sockets[0].emit(deltaChange(2));
    await flush();

    expect(calls).toHaveLength(1); // no extra snapshot fetch
    expect(sockets).toHaveLength(1); // the socket stayed up
    expect(phaseOf(client.getState().body)).toBe('Armed');
    expect(client.getState().cursor).toBe(2); // 0 + the two applied envelopes

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
