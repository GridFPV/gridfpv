import { describe, expect, it } from 'vitest';
import type { SeatLevels, SignalChunk, TimerConfig } from '@gridfpv/types';
import type { EventSourceLike, FetchLike } from './client.js';

// A timer's settings and its live feed: what the Tune page reads and writes. The JSON here is
// the JSON the Director's own tests use (`crates/server/src/timers.rs`).

const BASE = 'http://director.local:8080';
const TIMER = 'gate/x1 y2';
const ROOT = `${BASE}/timers/gate%2Fx1%20y2`;

const CONFIG: TimerConfig = {
  seats: [
    { seat: '0', freq_mhz: 5658, enabled: true },
    { seat: '1', freq_mhz: 5917, enabled: false }
  ],
  seats_per_node: 4,
  threshold: { Manual: { enter: -700, exit: -760 } },
  min_lap_ms: 3000
};

function recording(body: unknown): {
  fetch: FetchLike;
  seen: { url: string; init?: RequestInit }[];
} {
  const seen: { url: string; init?: RequestInit }[] = [];
  const fetch: FetchLike = async (input, init) => {
    seen.push({ url: String(input), init });
    return {
      ok: true,
      status: 200,
      json: async (): Promise<unknown> => body
    } as unknown as Response;
  };
  return { fetch, seen };
}
const headersOf = (init?: RequestInit): Record<string, string> =>
  init?.headers as Record<string, string>;
const refusing =
  (status: number, code: string, message: string): FetchLike =>
  async () =>
    ({
      ok: false,
      status,
      json: async (): Promise<unknown> => ({ code, message })
    }) as unknown as Response;

describe("a timer's settings", () => {
  it('getTimerConfig GETs /timers/{id}/config with the id encoded and no token', async () => {
    const { fetch, seen } = recording(CONFIG);
    const { getTimerConfig } = await import('./client.js');
    expect(await getTimerConfig(`${BASE}/`, TIMER, { fetch })).toEqual(CONFIG);
    expect(seen[0].url).toBe(`${ROOT}/config`);
    expect(seen[0].init?.method).toBeUndefined();
    expect(headersOf(seen[0].init).Authorization).toBeUndefined();
  });

  it("getTimerConfig rejects with the Director's sentence when the timer cannot be reached", async () => {
    const SAID = 'Could not reach Gate: connection refused';
    const { getTimerConfig, isRequestFailure } = await import('./client.js');
    const err: unknown = await getTimerConfig(BASE, TIMER, {
      fetch: refusing(409, 'Refused', SAID)
    }).then(
      () => undefined,
      (e: unknown) => e
    );
    expect(isRequestFailure(err)).toBe(true);
    if (!isRequestFailure(err)) return;
    expect(err.message).toBe(SAID);
    expect(err.status).toBe(409);
    expect(err.code).toBe('Refused');
  });

  it('putTimerConfig PUTs the whole settings with the token and returns what the timer kept', async () => {
    const kept: TimerConfig = { ...CONFIG, min_lap_ms: 2500 };
    const { fetch, seen } = recording(kept);
    const { putTimerConfig } = await import('./client.js');
    expect(await putTimerConfig(BASE, TIMER, CONFIG, 'rd-tok', { fetch })).toEqual(kept);
    expect(seen[0].url).toBe(`${ROOT}/config`);
    expect(seen[0].init?.method).toBe('PUT');
    expect(headersOf(seen[0].init).Authorization).toBe('Bearer rd-tok');
    expect(headersOf(seen[0].init)['Content-Type']).toBe('application/json');
    expect(JSON.parse(seen[0].init?.body as string)).toEqual(CONFIG);
  });

  it("putTimerConfig rejects a refusal with the Director's sentence and the 409", async () => {
    const SAID = 'The timer did not take the change. It is still set the way it was.';
    const { putTimerConfig, isRequestFailure } = await import('./client.js');
    const err: unknown = await putTimerConfig(BASE, TIMER, CONFIG, undefined, {
      fetch: refusing(409, 'Refused', SAID)
    }).then(
      () => undefined,
      (e: unknown) => e
    );
    expect(isRequestFailure(err)).toBe(true);
    if (!isRequestFailure(err)) return;
    expect(err.message).toBe(SAID);
    expect(err.status).toBe(409);
  });

  it('a failure with no body says what was tried, without the id or the route', async () => {
    const bare: FetchLike = async () =>
      ({
        ok: false,
        status: 500,
        json: async (): Promise<unknown> => {
          throw new SyntaxError('Unexpected end of JSON input');
        }
      }) as unknown as Response;
    const { getTimerConfig, putTimerConfig } = await import('./client.js');
    await expect(getTimerConfig(BASE, TIMER, { fetch: bare })).rejects.toThrow(
      "The Director could not read the timer's settings (HTTP 500)."
    );
    await expect(putTimerConfig(BASE, TIMER, CONFIG, 't', { fetch: bare })).rejects.toThrow(
      "The Director could not change the timer's settings (HTTP 500)."
    );
  });
});

describe("a timer's live feed", () => {
  /** A stand-in `EventSource` the test drives by hand. */
  class FakeSource implements EventSourceLike {
    static made: FakeSource[] = [];
    listeners = new Map<string, ((event: { data?: unknown }) => void)[]>();
    closed = false;
    constructor(readonly url: string) {
      FakeSource.made.push(this);
    }
    addEventListener(type: string, listener: (event: { data?: unknown }) => void): void {
      this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]);
    }
    close(): void {
      this.closed = true;
    }
    emit(type: string, data?: unknown): void {
      for (const listener of this.listeners.get(type) ?? []) listener({ data });
    }
  }

  const CHUNK: SignalChunk = {
    adapter: 'gate-x1y2',
    seat: '0',
    from: 5_000_000,
    period_micros: 1000,
    levels: [-900, -895, -640],
    unit: 'TenthsDb',
    noise_floor: -920
  };
  const LEVELS: SeatLevels[] = [
    { seat: '0', level: -640, floor: -920, enter: -770, exit: -820, crossing: true },
    { seat: '1', enter: -770, exit: -820, crossing: false }
  ];

  function listen(onError?: () => void) {
    FakeSource.made = [];
    const signals: SignalChunk[] = [];
    const levels: SeatLevels[][] = [];
    return import('./client.js').then(({ subscribeTimerLive }) => {
      const stop = subscribeTimerLive(
        `${BASE}/`,
        TIMER,
        { onSignal: (c) => signals.push(c), onLevels: (l) => levels.push(l), onError },
        { EventSource: FakeSource }
      );
      return { stop, source: FakeSource.made[0], signals, levels };
    });
  }

  it('opens /timers/{id}/live and hands over signal chunks and levels', async () => {
    const { source, signals, levels } = await listen();
    expect(source.url).toBe(`${ROOT}/live`);
    source.emit('signal', JSON.stringify(CHUNK));
    source.emit('levels', JSON.stringify(LEVELS));
    expect(signals).toEqual([CHUNK]);
    expect(levels).toEqual([LEVELS]);
  });

  it('skips an event that is not the JSON it should be, and keeps listening', async () => {
    const { source, signals, levels } = await listen();
    source.emit('signal', 'not json');
    source.emit('signal', JSON.stringify({ seat: '0' }));
    source.emit('signal');
    source.emit('levels', JSON.stringify({ seat: '0' }));
    source.emit('message', JSON.stringify(CHUNK));
    expect(signals).toEqual([]);
    expect(levels).toEqual([]);
    source.emit('signal', JSON.stringify(CHUNK));
    expect(signals).toEqual([CHUNK]);
  });

  it('tells onError when the connection drops', async () => {
    let drops = 0;
    const { source } = await listen(() => (drops += 1));
    source.emit('error');
    expect(drops).toBe(1);
    expect(source.closed).toBe(false);
  });

  it('the returned function closes the feed', async () => {
    const { stop, source } = await listen();
    stop();
    expect(source.closed).toBe(true);
  });

  it('says so when the browser has no EventSource', async () => {
    const { subscribeTimerLive } = await import('./client.js');
    const had = (globalThis as { EventSource?: unknown }).EventSource;
    (globalThis as { EventSource?: unknown }).EventSource = undefined;
    try {
      expect(() =>
        subscribeTimerLive(BASE, TIMER, { onSignal: () => {}, onLevels: () => {} })
      ).toThrow('This browser cannot listen for live changes.');
    } finally {
      (globalThis as { EventSource?: unknown }).EventSource = had;
    }
  });
});
