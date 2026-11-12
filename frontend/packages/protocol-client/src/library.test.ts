import { describe, expect, it } from 'vitest';
import type {
  Format,
  SavedFormat,
  SeatProfile,
  SeatProfileRequest,
  TimerConfig,
  TimerProfiles
} from '@gridfpv/types';
import type { FetchLike } from './client.js';

// The saved library: formats kept by name, and each timer's seat profiles. The routes are the
// Director's (`crates/server/src/app.rs`, handlers in `crates/server/src/library.rs`).

const BASE = 'http://director.local:8080';
const TIMER = 'gate/x1 y2';
const TIMER_ROOT = `${BASE}/timers/gate%2Fx1%20y2`;
/** A handle with characters a path cannot carry raw, so a missing encode shows. */
const ODD_ID = 'two min/qual 1';
const ODD_ID_ENCODED = 'two%20min%2Fqual%201';

const FORMAT: Format = {
  name: '2 minute qualifying',
  race: { end: { FirstToLaps: 3 }, staging_micros: 5_000_000, first_lap: 'HoleShot' },
  completion: { SavedRaces: 1 },
  ranking: { sort: ['LastFinish'] }
};
const SAVED: SavedFormat = { id: 'fmt-1a2b', format: FORMAT };

const CONFIG: TimerConfig = {
  seats: [
    { seat: '0', freq_mhz: 5658, enabled: true },
    { seat: '1', freq_mhz: 5917, enabled: false }
  ],
  threshold: { Manual: { enter: -700, exit: -760 } }
};
const REQUEST: SeatProfileRequest = { name: 'Garage', config: CONFIG };
const PROFILE: SeatProfile = { id: 'prof-3c4d', name: 'Garage', config: CONFIG };

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

/** A fetch that records every request and answers 200 with no body, as the DELETEs do. */
function recordingNoBody(): {
  fetch: FetchLike;
  seen: { url: string; init?: RequestInit }[];
} {
  const seen: { url: string; init?: RequestInit }[] = [];
  const fetch: FetchLike = async (input, init) => {
    seen.push({ url: String(input), init });
    return {
      ok: true,
      status: 200,
      json: async (): Promise<unknown> => {
        throw new SyntaxError('Unexpected end of JSON input');
      }
    } as unknown as Response;
  };
  return { fetch, seen };
}

const headersOf = (init?: RequestInit): Record<string, string> =>
  init?.headers as Record<string, string>;
const sentBody = (init?: RequestInit): unknown => JSON.parse(init?.body as string);

const refusing =
  (status: number, code: string, message: string): FetchLike =>
  async () =>
    ({
      ok: false,
      status,
      json: async (): Promise<unknown> => ({ code, message })
    }) as unknown as Response;

/** A failure with no body: a bare 500, or an HTML error page. */
const bare: FetchLike = async () =>
  ({
    ok: false,
    status: 500,
    json: async (): Promise<unknown> => {
      throw new SyntaxError('Unexpected end of JSON input');
    }
  }) as unknown as Response;

/** Run a call expected to reject and hand back what it threw. */
const thrownBy = (run: () => Promise<unknown>): Promise<unknown> =>
  run().then(
    () => undefined,
    (e: unknown) => e
  );

describe('the saved formats', () => {
  it('listFormats GETs /formats with no token and returns the saved formats', async () => {
    const { fetch, seen } = recording([SAVED]);
    const { listFormats } = await import('./client.js');
    expect(await listFormats(`${BASE}/`, { fetch })).toEqual([SAVED]);
    expect(seen).toHaveLength(1);
    expect(seen[0].url).toBe(`${BASE}/formats`);
    expect(seen[0].init?.method).toBeUndefined();
    expect(seen[0].init?.body).toBeUndefined();
    expect(headersOf(seen[0].init).Authorization).toBeUndefined();
    expect(headersOf(seen[0].init).Accept).toBe('application/json');
  });

  it('listFormats sends a token when it is handed one', async () => {
    const { fetch, seen } = recording([]);
    const { listFormats } = await import('./client.js');
    expect(await listFormats(BASE, { fetch, token: 'rd-tok' })).toEqual([]);
    expect(headersOf(seen[0].init).Authorization).toBe('Bearer rd-tok');
  });

  it('createFormat POSTs the format to /formats with the token and returns the saved format', async () => {
    const { fetch, seen } = recording(SAVED);
    const { createFormat } = await import('./client.js');
    expect(await createFormat(BASE, FORMAT, 'rd-tok', { fetch })).toEqual(SAVED);
    expect(seen).toHaveLength(1);
    expect(seen[0].url).toBe(`${BASE}/formats`);
    expect(seen[0].init?.method).toBe('POST');
    expect(headersOf(seen[0].init).Authorization).toBe('Bearer rd-tok');
    expect(headersOf(seen[0].init)['Content-Type']).toBe('application/json');
    expect(sentBody(seen[0].init)).toEqual(FORMAT);
  });

  it('createFormat omits Authorization when no token is given', async () => {
    const { fetch, seen } = recording(SAVED);
    const { createFormat } = await import('./client.js');
    await createFormat(BASE, FORMAT, undefined, { fetch });
    expect(headersOf(seen[0].init).Authorization).toBeUndefined();
  });

  it("createFormat rejects a name already taken with the Director's sentence and the 409", async () => {
    const SAID = 'There is already a format called 2 minute qualifying.';
    const { createFormat, isRequestFailure } = await import('./client.js');
    const err = await thrownBy(() =>
      createFormat(BASE, FORMAT, 'rd-tok', { fetch: refusing(409, 'Refused', SAID) })
    );
    expect(isRequestFailure(err)).toBe(true);
    if (!isRequestFailure(err)) return;
    expect(err.message).toBe(SAID);
    expect(err.status).toBe(409);
    expect(err.code).toBe('Refused');
  });

  it('updateFormat PUTs the format to /formats/{id} with the id encoded and the token', async () => {
    const renamed: SavedFormat = { id: ODD_ID, format: { ...FORMAT, name: 'Qualifying' } };
    const { fetch, seen } = recording(renamed);
    const { updateFormat } = await import('./client.js');
    expect(await updateFormat(BASE, ODD_ID, renamed.format, 'rd-tok', { fetch })).toEqual(renamed);
    expect(seen).toHaveLength(1);
    expect(seen[0].url).toBe(`${BASE}/formats/${ODD_ID_ENCODED}`);
    expect(seen[0].init?.method).toBe('PUT');
    expect(headersOf(seen[0].init).Authorization).toBe('Bearer rd-tok');
    expect(headersOf(seen[0].init)['Content-Type']).toBe('application/json');
    expect(sentBody(seen[0].init)).toEqual(renamed.format);
  });

  it("updateFormat rejects with the Director's sentence and the status", async () => {
    const SAID = 'That format no longer exists.';
    const { updateFormat, isRequestFailure } = await import('./client.js');
    const err = await thrownBy(() =>
      updateFormat(BASE, 'fmt-1a2b', FORMAT, undefined, { fetch: refusing(404, 'NotFound', SAID) })
    );
    expect(isRequestFailure(err)).toBe(true);
    if (!isRequestFailure(err)) return;
    expect(err.message).toBe(SAID);
    expect(err.status).toBe(404);
  });

  it('deleteFormat DELETEs /formats/{id} with the id encoded, and reads no body', async () => {
    const { fetch, seen } = recordingNoBody();
    const { deleteFormat } = await import('./client.js');
    await expect(deleteFormat(BASE, ODD_ID, 'rd-tok', { fetch })).resolves.toBeUndefined();
    expect(seen).toHaveLength(1);
    expect(seen[0].url).toBe(`${BASE}/formats/${ODD_ID_ENCODED}`);
    expect(seen[0].init?.method).toBe('DELETE');
    expect(seen[0].init?.body).toBeUndefined();
    expect(headersOf(seen[0].init).Authorization).toBe('Bearer rd-tok');
    expect(headersOf(seen[0].init)['Content-Type']).toBeUndefined();
  });

  it("deleteFormat rejects with the Director's sentence and the status", async () => {
    const SAID = 'That format no longer exists.';
    const { deleteFormat, isRequestFailure } = await import('./client.js');
    const err = await thrownBy(() =>
      deleteFormat(BASE, 'fmt-1a2b', undefined, { fetch: refusing(404, 'NotFound', SAID) })
    );
    expect(isRequestFailure(err)).toBe(true);
    if (!isRequestFailure(err)) return;
    expect(err.message).toBe(SAID);
    expect(err.status).toBe(404);
  });

  it('a failure with no body says what was tried, without the id or the route', async () => {
    const c = await import('./client.js');
    await expect(c.listFormats(BASE, { fetch: bare })).rejects.toThrow(
      'The Director could not list the formats (HTTP 500).'
    );
    await expect(c.createFormat(BASE, FORMAT, 't', { fetch: bare })).rejects.toThrow(
      'The Director could not save the format (HTTP 500).'
    );
    await expect(c.updateFormat(BASE, ODD_ID, FORMAT, 't', { fetch: bare })).rejects.toThrow(
      'The Director could not change the format (HTTP 500).'
    );
    await expect(c.deleteFormat(BASE, ODD_ID, 't', { fetch: bare })).rejects.toThrow(
      'The Director could not delete the format (HTTP 500).'
    );
  });
});

describe("a timer's seat profiles", () => {
  it('listProfiles GETs /timers/{id}/profiles with the id encoded and no token', async () => {
    const profiles: TimerProfiles = { active: PROFILE.id, profiles: [PROFILE] };
    const { fetch, seen } = recording(profiles);
    const { listProfiles } = await import('./client.js');
    expect(await listProfiles(`${BASE}/`, TIMER, { fetch })).toEqual(profiles);
    expect(seen).toHaveLength(1);
    expect(seen[0].url).toBe(`${TIMER_ROOT}/profiles`);
    expect(seen[0].init?.method).toBeUndefined();
    expect(seen[0].init?.body).toBeUndefined();
    expect(headersOf(seen[0].init).Authorization).toBeUndefined();
  });

  it("listProfiles rejects with the Director's sentence when the timer is not known", async () => {
    const SAID = 'That timer is not in the list any more.';
    const { listProfiles, isRequestFailure } = await import('./client.js');
    const err = await thrownBy(() =>
      listProfiles(BASE, TIMER, { fetch: refusing(404, 'NotFound', SAID) })
    );
    expect(isRequestFailure(err)).toBe(true);
    if (!isRequestFailure(err)) return;
    expect(err.message).toBe(SAID);
    expect(err.status).toBe(404);
  });

  it('createProfile POSTs the name and settings to /timers/{id}/profiles with the token', async () => {
    const { fetch, seen } = recording(PROFILE);
    const { createProfile } = await import('./client.js');
    expect(await createProfile(BASE, TIMER, REQUEST, 'rd-tok', { fetch })).toEqual(PROFILE);
    expect(seen).toHaveLength(1);
    expect(seen[0].url).toBe(`${TIMER_ROOT}/profiles`);
    expect(seen[0].init?.method).toBe('POST');
    expect(headersOf(seen[0].init).Authorization).toBe('Bearer rd-tok');
    expect(headersOf(seen[0].init)['Content-Type']).toBe('application/json');
    expect(sentBody(seen[0].init)).toEqual(REQUEST);
  });

  it('createProfile omits Authorization when no token is given', async () => {
    const { fetch, seen } = recording(PROFILE);
    const { createProfile } = await import('./client.js');
    await createProfile(BASE, TIMER, REQUEST, undefined, { fetch });
    expect(headersOf(seen[0].init).Authorization).toBeUndefined();
  });

  it("createProfile rejects with the Director's sentence and the status", async () => {
    const SAID = 'This timer already has a profile called Garage.';
    const { createProfile, isRequestFailure } = await import('./client.js');
    const err = await thrownBy(() =>
      createProfile(BASE, TIMER, REQUEST, 'rd-tok', { fetch: refusing(409, 'Refused', SAID) })
    );
    expect(isRequestFailure(err)).toBe(true);
    if (!isRequestFailure(err)) return;
    expect(err.message).toBe(SAID);
    expect(err.status).toBe(409);
    expect(err.code).toBe('Refused');
  });

  it('updateProfile PUTs to /timers/{id}/profiles/{pid} with both ids encoded and the token', async () => {
    const renamed: SeatProfile = { id: ODD_ID, name: 'Field', config: CONFIG };
    const request: SeatProfileRequest = { name: 'Field', config: CONFIG };
    const { fetch, seen } = recording(renamed);
    const { updateProfile } = await import('./client.js');
    expect(await updateProfile(BASE, TIMER, ODD_ID, request, 'rd-tok', { fetch })).toEqual(renamed);
    expect(seen).toHaveLength(1);
    expect(seen[0].url).toBe(`${TIMER_ROOT}/profiles/${ODD_ID_ENCODED}`);
    expect(seen[0].init?.method).toBe('PUT');
    expect(headersOf(seen[0].init).Authorization).toBe('Bearer rd-tok');
    expect(headersOf(seen[0].init)['Content-Type']).toBe('application/json');
    expect(sentBody(seen[0].init)).toEqual(request);
  });

  it("updateProfile rejects with the Director's sentence and the status", async () => {
    const SAID = 'That profile no longer exists.';
    const { updateProfile, isRequestFailure } = await import('./client.js');
    const err = await thrownBy(() =>
      updateProfile(BASE, TIMER, PROFILE.id, REQUEST, undefined, {
        fetch: refusing(404, 'NotFound', SAID)
      })
    );
    expect(isRequestFailure(err)).toBe(true);
    if (!isRequestFailure(err)) return;
    expect(err.message).toBe(SAID);
    expect(err.status).toBe(404);
  });

  it('deleteProfile DELETEs /timers/{id}/profiles/{pid} with both ids encoded, and reads no body', async () => {
    const { fetch, seen } = recordingNoBody();
    const { deleteProfile } = await import('./client.js');
    await expect(deleteProfile(BASE, TIMER, ODD_ID, 'rd-tok', { fetch })).resolves.toBeUndefined();
    expect(seen).toHaveLength(1);
    expect(seen[0].url).toBe(`${TIMER_ROOT}/profiles/${ODD_ID_ENCODED}`);
    expect(seen[0].init?.method).toBe('DELETE');
    expect(seen[0].init?.body).toBeUndefined();
    expect(headersOf(seen[0].init).Authorization).toBe('Bearer rd-tok');
    expect(headersOf(seen[0].init)['Content-Type']).toBeUndefined();
  });

  it("deleteProfile rejects with the Director's sentence and the status", async () => {
    const SAID = 'That profile no longer exists.';
    const { deleteProfile, isRequestFailure } = await import('./client.js');
    const err = await thrownBy(() =>
      deleteProfile(BASE, TIMER, PROFILE.id, undefined, { fetch: refusing(404, 'NotFound', SAID) })
    );
    expect(isRequestFailure(err)).toBe(true);
    if (!isRequestFailure(err)) return;
    expect(err.message).toBe(SAID);
    expect(err.status).toBe(404);
  });

  it('applyProfile POSTs to /timers/{id}/profiles/{pid}/apply with no body and returns what the timer kept', async () => {
    const kept: TimerConfig = { ...CONFIG, lap_cooldown_ms: 2500 };
    const { fetch, seen } = recording(kept);
    const { applyProfile } = await import('./client.js');
    expect(await applyProfile(BASE, TIMER, ODD_ID, 'rd-tok', { fetch })).toEqual(kept);
    expect(seen).toHaveLength(1);
    expect(seen[0].url).toBe(`${TIMER_ROOT}/profiles/${ODD_ID_ENCODED}/apply`);
    expect(seen[0].init?.method).toBe('POST');
    expect(headersOf(seen[0].init).Authorization).toBe('Bearer rd-tok');
    // Nothing is sent: the profile is named by the path.
    expect(seen[0].init?.body).toBeUndefined();
    expect(headersOf(seen[0].init)['Content-Type']).toBeUndefined();
  });

  it('applyProfile omits Authorization when no token is given', async () => {
    const { fetch, seen } = recording(CONFIG);
    const { applyProfile } = await import('./client.js');
    await applyProfile(BASE, TIMER, PROFILE.id, undefined, { fetch });
    expect(headersOf(seen[0].init).Authorization).toBeUndefined();
  });

  it("applyProfile rejects a timer's refusal with the Director's sentence and the 409", async () => {
    const SAID = 'Could not reach Gate: connection refused';
    const { applyProfile, isRequestFailure } = await import('./client.js');
    const err = await thrownBy(() =>
      applyProfile(BASE, TIMER, PROFILE.id, 'rd-tok', { fetch: refusing(409, 'Refused', SAID) })
    );
    expect(isRequestFailure(err)).toBe(true);
    if (!isRequestFailure(err)) return;
    expect(err.message).toBe(SAID);
    expect(err.status).toBe(409);
    expect(err.code).toBe('Refused');
  });

  it('a failure with no body says what was tried, without the ids or the route', async () => {
    const c = await import('./client.js');
    await expect(c.listProfiles(BASE, TIMER, { fetch: bare })).rejects.toThrow(
      "The Director could not list the timer's profiles (HTTP 500)."
    );
    await expect(c.createProfile(BASE, TIMER, REQUEST, 't', { fetch: bare })).rejects.toThrow(
      'The Director could not save the profile (HTTP 500).'
    );
    await expect(
      c.updateProfile(BASE, TIMER, ODD_ID, REQUEST, 't', { fetch: bare })
    ).rejects.toThrow('The Director could not change the profile (HTTP 500).');
    await expect(c.deleteProfile(BASE, TIMER, ODD_ID, 't', { fetch: bare })).rejects.toThrow(
      'The Director could not delete the profile (HTTP 500).'
    );
    await expect(c.applyProfile(BASE, TIMER, ODD_ID, 't', { fetch: bare })).rejects.toThrow(
      'The Director could not apply the profile (HTTP 500).'
    );
  });
});
