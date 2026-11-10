import { describe, expect, it } from 'vitest';
import type { BracketRequest, Command, EventView, Format, MainsRequest } from '@gridfpv/types';
import type { EventSourceLike, FetchLike } from './client.js';

// The competition calls: the snapshot, one command, the presets, a pass, and the change feed.
// The JSON here is the JSON the Director's own tests send (`crates/server/src/control.rs`).

const BASE = 'http://director.local:8080';
const EVT = 'club night/1';
const ROOT = `${BASE}/events/${encodeURIComponent(EVT)}`;

const FORMAT: Format = {
  name: 'First to 1',
  race: { end: { FirstToLaps: 1 }, staging_micros: 0, first_lap: 'HoleShot' },
  completion: { SavedRaces: 1 },
  ranking: { sort: ['LastFinish'] }
};

/** A fetch that records every request and answers 200 with `body`. */
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

const sentBody = (init?: RequestInit): unknown => JSON.parse(init?.body as string);
const sentHeaders = (init?: RequestInit): Record<string, string> =>
  init?.headers as Record<string, string>;

describe('the event snapshot', () => {
  it('getEventState GETs /events/{id}/state with the id encoded, and needs no token', async () => {
    const view: EventView = {
      cursor: 7,
      now: 1_700_000_000_000_000,
      rosters: [],
      phases: [],
      races: [],
      protests: [],
      proposed_disqualifications: []
    };
    const { fetch, seen } = recording(view);
    const { getEventState } = await import('./client.js');
    expect(await getEventState(BASE, EVT, { fetch })).toEqual(view);
    expect(seen[0].url).toBe(`${ROOT}/state`);
    expect(seen[0].init?.method).toBeUndefined();
    expect(sentHeaders(seen[0].init).Authorization).toBeUndefined();
  });
});

describe('commands', () => {
  it('sendCommand POSTs the command as it is, with the token, and returns the cursor', async () => {
    const { fetch, seen } = recording({ cursor: 12 });
    const { sendCommand } = await import('./client.js');
    const command: Command = { StartRace: { heat: 'finals/final' } };
    expect(await sendCommand(BASE, EVT, command, 'rd-tok', { fetch })).toEqual({ cursor: 12 });
    expect(seen[0].url).toBe(`${ROOT}/commands`);
    expect(seen[0].init?.method).toBe('POST');
    expect(sentHeaders(seen[0].init).Authorization).toBe('Bearer rd-tok');
    expect(sentBody(seen[0].init)).toEqual({ StartRace: { heat: 'finals/final' } });
  });

  it('sendCommand sends a recorded fact inside Record', async () => {
    const { fetch, seen } = recording({ cursor: 3 });
    const { sendCommand } = await import('./client.js');
    await sendCommand(BASE, EVT, { Record: { HeatDone: { heat: 'q/heat-1' } } }, undefined, {
      fetch
    });
    expect(sentBody(seen[0].init)).toEqual({ Record: { HeatDone: { heat: 'q/heat-1' } } });
    expect(sentHeaders(seen[0].init).Authorization).toBeUndefined();
  });

  it("a refused command rejects with the Director's sentence, the 409 and the code", async () => {
    const SAID = 'cannot stop race "finals/final#1" while it is Saved';
    const fetch: FetchLike = async () =>
      ({
        ok: false,
        status: 409,
        json: async (): Promise<unknown> => ({ code: 'Refused', message: SAID })
      }) as unknown as Response;
    const { sendCommand, isRequestFailure } = await import('./client.js');
    const err = await sendCommand(BASE, EVT, { StopRace: { race: 'finals/final#1' } }, undefined, {
      fetch
    }).then(
      () => undefined,
      (e: unknown) => e
    );
    expect(isRequestFailure(err)).toBe(true);
    if (!isRequestFailure(err)) return;
    // As it is: the console shows this sentence without touching it.
    expect(err.message).toBe(SAID);
    expect(err.status).toBe(409);
    expect(err.code).toBe('Refused');
  });
});

describe('presets and passes', () => {
  it('createBracket POSTs the request to /presets/bracket', async () => {
    const { fetch, seen } = recording({ cursor: 9 });
    const { createBracket } = await import('./client.js');
    const request: BracketRequest = {
      id: 'finals',
      label: 'Finals',
      field: [{ Imported: ['ana', 'ben'] }],
      entrants: [{ Pilot: 'ana' }, { Pilot: 'ben' }],
      seats: [
        { adapter: 'bench', seat: 'seat-1' },
        { adapter: 'bench', seat: 'seat-2' }
      ],
      advance: 1,
      elimination: 'Single',
      format: FORMAT
    };
    expect(await createBracket(BASE, EVT, request, 'rd-tok', { fetch })).toEqual({ cursor: 9 });
    expect(seen[0].url).toBe(`${ROOT}/presets/bracket`);
    expect(seen[0].init?.method).toBe('POST');
    expect(sentBody(seen[0].init)).toEqual(request);
    expect(sentHeaders(seen[0].init).Authorization).toBe('Bearer rd-tok');
  });

  it('createMains POSTs the request to /presets/mains', async () => {
    const { fetch, seen } = recording({ cursor: 30 });
    const { createMains } = await import('./client.js');
    const request: MainsRequest = {
      id: 'mains',
      label: 'Mains',
      seeded_from: 'qualifying',
      pilots: 20,
      per_main: 8,
      bump: 2,
      seats: [{ adapter: 'bench', seat: 'seat-1' }],
      advance: 2,
      elimination: 'Double',
      format: FORMAT
    };
    await createMains(BASE, EVT, request, undefined, { fetch });
    expect(seen[0].url).toBe(`${ROOT}/presets/mains`);
    expect(sentBody(seen[0].init)).toEqual(request);
  });

  it('postPass POSTs adapter, seat and time to /passes', async () => {
    const { fetch, seen } = recording({ cursor: 4 });
    const { postPass } = await import('./client.js');
    await postPass(BASE, EVT, { adapter: 'bench', seat: 'seat-2', at: 1_234_000 }, undefined, {
      fetch
    });
    expect(seen[0].url).toBe(`${ROOT}/passes`);
    expect(seen[0].init?.method).toBe('POST');
    expect(sentBody(seen[0].init)).toEqual({ adapter: 'bench', seat: 'seat-2', at: 1_234_000 });
  });
});

describe('the change feed', () => {
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

  it('opens /events/{id}/changes and hands each cursor event to the callback as a number', async () => {
    FakeSource.made = [];
    const { subscribeChanges } = await import('./client.js');
    const cursors: number[] = [];
    subscribeChanges(`${BASE}/`, EVT, (cursor) => cursors.push(cursor), {
      EventSource: FakeSource
    });
    const [source] = FakeSource.made;
    expect(source.url).toBe(`${ROOT}/changes`);
    source.emit('cursor', '5');
    source.emit('cursor', '6');
    expect(cursors).toEqual([5, 6]);
  });

  it('ignores other events and a cursor that is not a number', async () => {
    FakeSource.made = [];
    const { subscribeChanges } = await import('./client.js');
    const cursors: number[] = [];
    subscribeChanges(BASE, EVT, (cursor) => cursors.push(cursor), { EventSource: FakeSource });
    const [source] = FakeSource.made;
    source.emit('message', '9');
    source.emit('cursor', 'soon');
    expect(cursors).toEqual([]);
  });

  it('tells onError when the connection drops, and keeps listening', async () => {
    FakeSource.made = [];
    const { subscribeChanges } = await import('./client.js');
    const cursors: number[] = [];
    let drops = 0;
    subscribeChanges(BASE, EVT, (cursor) => cursors.push(cursor), {
      EventSource: FakeSource,
      onError: () => (drops += 1)
    });
    const [source] = FakeSource.made;
    source.emit('error');
    expect(drops).toBe(1);
    expect(source.closed).toBe(false);
    // The browser reconnects by itself and the Director sends the cursor again.
    source.emit('cursor', '6');
    expect(cursors).toEqual([6]);
  });

  it('the returned function closes the feed', async () => {
    FakeSource.made = [];
    const { subscribeChanges } = await import('./client.js');
    const stop = subscribeChanges(BASE, EVT, () => {}, { EventSource: FakeSource });
    stop();
    expect(FakeSource.made[0].closed).toBe(true);
  });
});
