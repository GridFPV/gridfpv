import { describe, expect, it, vi } from 'vitest';
import type {
  Class,
  CreateClassRequest,
  CreatePilotRequest,
  DiscoveredTimer,
  EventMeta,
  Pilot,
  Timer,
  TimerConfig,
  TimerView
} from '@gridfpv/types';
import { Session, type SessionApi } from '../src/lib/session.svelte.js';

/**
 * Per-test overrides for the seams a `Session` calls outward through — the session's own
 * {@link SessionApi} record, every key optional. Derived from it rather than restated, so a new
 * endpoint is steerable here the moment it exists.
 */
type SessionOverrides = Partial<SessionApi>;

const EVENT_A: EventMeta = {
  id: 'evt-a',
  name: 'Friday Series',
  created_at: 1,
  persistent: true
};

/**
 * A protocol-client rejection as it now arrives (#433): the Director's own sentence for the RD,
 * with the HTTP status carried **structurally** rather than spelled into the words. Auth detection
 * keys on that number, so these fixtures must too — a test that puts "401" in the message and
 * expects a prompt would be asserting the bug #433 removed.
 */
const refusal = (status: number, message: string): Error =>
  Object.assign(new Error(message), { status });

describe('Session', () => {
  it('starts with no event open; selectEvent opens one locally', () => {
    const session = new Session({ autoRestore: false });
    expect(session.currentEvent).toBeUndefined();

    session.selectEvent(EVENT_A);
    expect(session.currentEvent?.id).toBe('evt-a');
  });

  it('createEventAndEnter creates by name and enters the new event', async () => {
    const createEvent = vi.fn(async () => EVENT_A);

    const session = new Session({
      autoRestore: false,
      api: { createEvent }
    });
    session.setToken('tok');

    const meta = await session.createEventAndEnter('Friday Series');
    expect(createEvent).toHaveBeenCalledWith(session.baseUrl, 'Friday Series', 'tok', {
      fields: undefined
    });
    expect(meta?.id).toBe('evt-a');
    expect(session.currentEvent?.id).toBe('evt-a');
  });

  it('createEventAndEnter creates tokenless against an open Director (no prompt)', async () => {
    const createEvent = vi.fn(async () => EVENT_A);
    const tokenProvider = vi.fn(async () => 'lazy-tok');

    const session = new Session({
      autoRestore: false,
      api: { createEvent }
    });
    session.setTokenProvider(tokenProvider);

    const meta = await session.createEventAndEnter('Friday Series', {
      date: '2026-06-20',
      location: 'Main field'
    });
    // Sent with no token and the optional fields forwarded; no prompt against an open Director.
    expect(createEvent).toHaveBeenCalledWith(session.baseUrl, 'Friday Series', undefined, {
      fields: { date: '2026-06-20', location: 'Main field' }
    });
    expect(tokenProvider).not.toHaveBeenCalled();
    expect(meta?.id).toBe('evt-a');
  });

  it('createEventAndEnter prompts + retries when the Director gates create with a 401', async () => {
    let calls = 0;
    const createEvent = vi.fn(async () => {
      calls += 1;
      if (calls === 1) throw refusal(401, 'Control on this Director needs a token.');
      return EVENT_A;
    });
    const tokenProvider = vi.fn(async () => 'lazy-tok');

    const session = new Session({
      autoRestore: false,
      api: { createEvent }
    });
    session.setTokenProvider(tokenProvider);

    const meta = await session.createEventAndEnter('Friday Series');
    expect(tokenProvider).toHaveBeenCalledOnce();
    expect(createEvent).toHaveBeenCalledTimes(2);
    expect(session.hasToken).toBe(true);
    expect(meta?.id).toBe('evt-a');
  });

  // ── deleteEvent: permanent delete + all data (the papercut fix) ──────────────────────

  it('deleteEvent calls DELETE with the held token and resolves true', async () => {
    const deleteEvent = vi.fn(async () => undefined as unknown as void);
    const session = new Session({ autoRestore: false, api: { deleteEvent } });
    session.setToken('tok');

    const ok = await session.deleteEvent('evt-a');
    expect(deleteEvent).toHaveBeenCalledWith(session.baseUrl, 'evt-a', 'tok');
    expect(ok).toBe(true);
  });

  it('deleteEvent leaves the event locally when deleting the current one', async () => {
    const deleteEvent = vi.fn(async () => undefined as unknown as void);
    const session = new Session({
      autoRestore: false,
      api: { deleteEvent }
    });
    session.selectEvent(EVENT_A);
    expect(session.currentEvent?.id).toBe('evt-a');

    await session.deleteEvent('evt-a');
    // The just-deleted current event is torn down → back to the picker.
    expect(session.currentEvent).toBeUndefined();
  });

  it('deleteEvent prompts + retries when the Director gates delete with a 401', async () => {
    let calls = 0;
    const deleteEvent = vi.fn(async () => {
      calls += 1;
      if (calls === 1) throw refusal(401, 'Control on this Director needs a token.');
      return undefined as unknown as void;
    });
    const tokenProvider = vi.fn(async () => 'lazy-tok');
    const session = new Session({ autoRestore: false, api: { deleteEvent } });
    session.setTokenProvider(tokenProvider);

    const ok = await session.deleteEvent('evt-a');
    expect(tokenProvider).toHaveBeenCalledOnce();
    expect(deleteEvent).toHaveBeenCalledTimes(2);
    expect(ok).toBe(true);
  });

  it('deleteEvent re-throws a non-auth (400/404) failure for the UI to surface', async () => {
    const deleteEvent = vi.fn(async () => {
      throw refusal(400, 'Practice is in progress — finish it before deleting the event.');
    });
    const session = new Session({ autoRestore: false, api: { deleteEvent } });
    session.setToken('tok');
    // Surfaced verbatim: the Director's sentence, not a route line (#433).
    await expect(session.deleteEvent('practice-ab12')).rejects.toThrow(
      /Practice is in progress — finish it before deleting the event\./
    );
  });

  // ── #90: the active event is Director state — resume across reloads ──────────────────

  it('resolveActiveEvent resumes into the Director active event on load', async () => {
    // The Director has an active event set → resume into it (no picker).
    const getActiveEvent = vi.fn(async () => ({ event: EVENT_A }));
    const session = new Session({
      autoRestore: false,
      api: { getActiveEvent }
    });

    expect(session.resolvingActiveEvent).toBe(true);
    await session.resolveActiveEvent();
    expect(getActiveEvent).toHaveBeenCalledWith(session.baseUrl, { token: undefined });
    expect(session.currentEvent?.id).toBe('evt-a');
    expect(session.resolvingActiveEvent).toBe(false);
  });

  it('resolveActiveEvent stays at the picker when no event is active', async () => {
    const getActiveEvent = vi.fn(async () => ({ event: null }));
    const session = new Session({ autoRestore: false, api: { getActiveEvent } });

    await session.resolveActiveEvent();
    expect(session.currentEvent).toBeUndefined();
    expect(session.resolvingActiveEvent).toBe(false);
  });

  it('resolveActiveEvent falls back to the picker when the Director is unreachable', async () => {
    const getActiveEvent = vi.fn(async () => {
      throw new Error('fetch failed');
    });
    const session = new Session({ autoRestore: false, api: { getActiveEvent } });
    await session.resolveActiveEvent();
    expect(session.currentEvent).toBeUndefined();
    expect(session.resolvingActiveEvent).toBe(false);
  });

  // ── #91: the picker reads the active event id to mark the live row ───────────────────

  it('getActiveEventId returns the Director active event id', async () => {
    const getActiveEvent = vi.fn(async () => ({ event: EVENT_A }));
    const session = new Session({ autoRestore: false, api: { getActiveEvent } });
    expect(await session.getActiveEventId()).toBe('evt-a');
    expect(getActiveEvent).toHaveBeenCalledWith(session.baseUrl, { token: undefined });
  });

  it('getActiveEventId resolves undefined when nothing is active', async () => {
    const getActiveEvent = vi.fn(async () => ({ event: null }));
    const session = new Session({ autoRestore: false, api: { getActiveEvent } });
    expect(await session.getActiveEventId()).toBeUndefined();
  });

  it('getActiveEventId swallows a read failure (no pill, never blocks the list)', async () => {
    const getActiveEvent = vi.fn(async () => {
      throw new Error('fetch failed');
    });
    const session = new Session({ autoRestore: false, api: { getActiveEvent } });
    expect(await session.getActiveEventId()).toBeUndefined();
  });

  it('chooseEvent persists the active event server-side, then enters it', async () => {
    const setActiveEvent = vi.fn(async () => EVENT_A);
    const session = new Session({
      autoRestore: false,
      api: { setActiveEvent }
    });

    const chosen = await session.chooseEvent(EVENT_A);
    // The choice is persisted as the Director active event (no token against an open Director)…
    expect(setActiveEvent).toHaveBeenCalledWith(session.baseUrl, 'evt-a', undefined);
    // …and then entered.
    expect(chosen?.id).toBe('evt-a');
    expect(session.currentEvent?.id).toBe('evt-a');
  });

  it('chooseEvent prompts + retries when the Director gates the active-event set with a 401', async () => {
    let calls = 0;
    const setActiveEvent = vi.fn(async () => {
      calls += 1;
      if (calls === 1) throw refusal(401, 'Control on this Director needs a token.');
      return EVENT_A;
    });
    const tokenProvider = vi.fn(async () => 'lazy-tok');
    const session = new Session({
      autoRestore: false,
      api: { setActiveEvent }
    });
    session.setTokenProvider(tokenProvider);

    const chosen = await session.chooseEvent(EVENT_A);
    expect(tokenProvider).toHaveBeenCalledOnce();
    expect(setActiveEvent).toHaveBeenCalledTimes(2);
    expect(chosen?.id).toBe('evt-a');
    expect(session.currentEvent?.id).toBe('evt-a');
  });

  it('leaveEvent (switch event) does NOT clear the Director active event', async () => {
    // Switching back to the picker is a client-side view change; the server active event stays
    // set (so a reload mid-switch resumes, and other clients are not disrupted, #90).
    const setActiveEvent = vi.fn(async () => EVENT_A);
    const session = new Session({
      autoRestore: false,
      api: { setActiveEvent }
    });

    await session.chooseEvent(EVENT_A);
    setActiveEvent.mockClear();
    session.leaveEvent();
    // No call to clear/reset the server active event — switch is purely local.
    expect(setActiveEvent).not.toHaveBeenCalled();
    expect(session.currentEvent).toBeUndefined();
  });

  it('clearToken forgets the held token', () => {
    const session = new Session({ autoRestore: false });
    session.setToken('tok');
    expect(session.hasToken).toBe(true);

    session.clearToken();
    expect(session.hasToken).toBe(false);
  });

  describe('timers', () => {
    const GATE: Timer = { id: 'gate-x1y2', name: 'Gate', kind: 'Oft', address: '10.0.0.5:7270' };
    const timerSession = (overrides?: SessionOverrides) =>
      new Session({ baseUrl: 'http://d.local', autoRestore: false, api: { ...overrides } });

    it('listTimers reads the directory open (no token)', async () => {
      const views: TimerView[] = [{ timer: GATE, status: { connected: false, seats: [] } }];
      const listTimers = vi.fn(async () => views);
      const session = timerSession({ listTimers });
      expect(await session.listTimers()).toEqual(views);
      expect(listTimers).toHaveBeenCalledWith('http://d.local', { token: undefined });
    });

    it('listDiscoveredTimers reads the timers found on the network open (no token)', async () => {
      const found: DiscoveredTimer[] = [
        { name: 'Gate', address: '10.0.0.5:7270', seats: 4, added: false }
      ];
      const listDiscoveredTimers = vi.fn(async () => found);
      const session = timerSession({ listDiscoveredTimers });
      expect(await session.listDiscoveredTimers()).toEqual(found);
      expect(listDiscoveredTimers).toHaveBeenCalledWith('http://d.local', { token: undefined });
    });

    it('createTimer sends without a token first, then prompts once when refused for auth', async () => {
      const createTimer = vi
        .fn()
        .mockRejectedValueOnce(refusal(401, 'Control on this Director needs a token.'))
        .mockResolvedValueOnce(GATE);
      const session = timerSession({ createTimer });
      session.setTokenProvider(async () => 'tok');
      const request = { name: 'Gate', kind: 'Oft' as const, address: '10.0.0.5:7270' };
      expect(await session.createTimer(request)).toEqual(GATE);
      expect(createTimer).toHaveBeenNthCalledWith(1, 'http://d.local', request, undefined);
      expect(createTimer).toHaveBeenNthCalledWith(2, 'http://d.local', request, 'tok');
    });

    it("createTimer throws the Director's sentence for a bad address, with no prompt", async () => {
      const SAID = 'A timer needs an address, like 192.168.1.50:7270.';
      const createTimer = vi.fn().mockRejectedValue(refusal(400, SAID));
      const provider = vi.fn(async () => 'tok');
      const session = timerSession({ createTimer });
      session.setTokenProvider(provider);
      await expect(
        session.createTimer({ name: 'Gate', kind: 'Oft', address: 'nope' })
      ).rejects.toThrow(SAID);
      expect(provider).not.toHaveBeenCalled();
    });

    it('deleteTimer resolves true once removed, and undefined when the prompt is cancelled', async () => {
      const deleteTimer = vi.fn(async () => {});
      const session = timerSession({ deleteTimer });
      session.setToken('tok');
      expect(await session.deleteTimer('gate-x1y2')).toBe(true);
      expect(deleteTimer).toHaveBeenCalledWith('http://d.local', 'gate-x1y2', 'tok');

      const refused = vi.fn().mockRejectedValue(refusal(403, 'Control is gated.'));
      const gated = timerSession({ deleteTimer: refused });
      gated.setTokenProvider(async () => undefined);
      expect(await gated.deleteTimer('gate-x1y2')).toBeUndefined();
    });
  });

  describe("a timer's settings and live feed", () => {
    const CONFIG: TimerConfig = {
      seats: [{ seat: '0', freq_mhz: 5658, enabled: true }],
      seats_per_node: 4,
      threshold: { Auto: { snr_enter: 150, hysteresis: 50 } },
      min_lap_ms: 2000
    };
    const tuneSession = (overrides?: SessionOverrides) =>
      new Session({ baseUrl: 'http://d.local', autoRestore: false, api: { ...overrides } });

    it('getTimerConfig reads the settings open (no token)', async () => {
      const getTimerConfig = vi.fn(async () => CONFIG);
      const session = tuneSession({ getTimerConfig });
      expect(await session.getTimerConfig('gate-x1y2')).toEqual(CONFIG);
      expect(getTimerConfig).toHaveBeenCalledWith('http://d.local', 'gate-x1y2', {
        token: undefined
      });
    });

    it('putTimerConfig sends without a token first, then prompts once when refused for auth', async () => {
      const putTimerConfig = vi
        .fn()
        .mockRejectedValueOnce(refusal(401, 'Control on this Director needs a token.'))
        .mockResolvedValueOnce(CONFIG);
      const session = tuneSession({ putTimerConfig });
      session.setTokenProvider(async () => 'tok');
      expect(await session.putTimerConfig('gate-x1y2', CONFIG)).toEqual(CONFIG);
      expect(putTimerConfig).toHaveBeenNthCalledWith(
        1,
        'http://d.local',
        'gate-x1y2',
        CONFIG,
        undefined
      );
      expect(putTimerConfig).toHaveBeenNthCalledWith(
        2,
        'http://d.local',
        'gate-x1y2',
        CONFIG,
        'tok'
      );
    });

    it("putTimerConfig throws the timer's refusal as the Director wrote it, with no prompt", async () => {
      const SAID = 'The timer did not take the change. It is still set the way it was.';
      const provider = vi.fn(async () => 'tok');
      const session = tuneSession({
        putTimerConfig: vi.fn().mockRejectedValue(refusal(409, SAID))
      });
      session.setTokenProvider(provider);
      await expect(session.putTimerConfig('gate-x1y2', CONFIG)).rejects.toThrow(SAID);
      expect(provider).not.toHaveBeenCalled();
    });

    it('putTimerConfig resolves undefined when the token prompt is cancelled', async () => {
      const session = tuneSession({
        putTimerConfig: vi.fn().mockRejectedValue(refusal(403, 'Control is gated.'))
      });
      session.setTokenProvider(async () => undefined);
      expect(await session.putTimerConfig('gate-x1y2', CONFIG)).toBeUndefined();
    });

    it("getRaceSignal reads a race's logged signal open (no token)", async () => {
      const signal = { race: 'q-1#1', seats: [] };
      const getRaceSignal = vi.fn(async () => signal);
      const session = tuneSession({ getRaceSignal });
      expect(await session.getRaceSignal('evt', 'q-1#1')).toEqual(signal);
      expect(getRaceSignal).toHaveBeenCalledWith('http://d.local', 'evt', 'q-1#1', {
        token: undefined
      });
    });

    it('subscribeTimerLive listens on this Director and hands back the stop function', () => {
      const stop = vi.fn();
      const subscribeTimerLive = vi.fn(() => stop);
      const session = tuneSession({ subscribeTimerLive });
      const handlers = { onSignal: vi.fn(), onLevels: vi.fn() };
      expect(session.subscribeTimerLive('gate-x1y2', handlers)).toBe(stop);
      expect(subscribeTimerLive).toHaveBeenCalledWith('http://d.local', 'gate-x1y2', handlers);
    });
  });

  describe('pilots (#74)', () => {
    const ACE: Pilot = { id: 'p1', callsign: 'Ace', vtx_types: [] };

    function pilotSession(overrides?: SessionOverrides) {
      return new Session({
        baseUrl: 'http://d.local',
        autoRestore: false,
        api: { ...overrides }
      });
    }

    it('listPilots reads the directory open (no token)', async () => {
      const listPilots = vi.fn(async () => [ACE]);
      const session = pilotSession({ listPilots });
      const pilots = await session.listPilots();
      expect(pilots).toEqual([ACE]);
      expect(listPilots).toHaveBeenCalledWith('http://d.local', { token: undefined });
    });

    it('createPilot returns the new pilot (full-trust, no token)', async () => {
      const createPilot = vi.fn(async () => ACE);
      const session = pilotSession({ createPilot });
      const req: CreatePilotRequest = { callsign: 'Ace', vtx_types: [] };
      const result = await session.createPilot(req);
      expect(result).toEqual(ACE);
      expect(createPilot).toHaveBeenCalledWith('http://d.local', req, undefined);
    });

    it('createPilot prompts then retries once on an auth (401) failure', async () => {
      const createPilot = vi
        .fn()
        .mockRejectedValueOnce(refusal(401, 'Control on this Director needs a token.'))
        .mockResolvedValueOnce(ACE);
      const session = pilotSession({ createPilot });
      session.setTokenProvider(async () => 'tok');
      const result = await session.createPilot({ callsign: 'Ace', vtx_types: [] });
      expect(result).toEqual(ACE);
      expect(createPilot).toHaveBeenCalledTimes(2);
      expect(createPilot).toHaveBeenLastCalledWith('http://d.local', expect.anything(), 'tok');
    });

    it('createPilot resolves undefined when the auth prompt is cancelled', async () => {
      const createPilot = vi
        .fn()
        .mockRejectedValue(refusal(401, 'Control on this Director needs a token.'));
      const session = pilotSession({ createPilot });
      session.setTokenProvider(async () => undefined);
      const result = await session.createPilot({ callsign: 'X', vtx_types: [] });
      expect(result).toBeUndefined();
      expect(createPilot).toHaveBeenCalledTimes(1);
    });

    it('updatePilot passes the clear-via-null diff through verbatim', async () => {
      const updated: Pilot = { ...ACE, name: 'Alice' };
      const updatePilot = vi.fn(async () => updated);
      const session = pilotSession({ updatePilot });
      // A representative diff: set name, clear color/country with null, leave the rest absent.
      const req = { name: 'Alice', color: null, country: null } as const;
      const result = await session.updatePilot('p1', req);
      expect(result).toEqual(updated);
      expect(updatePilot).toHaveBeenCalledWith('http://d.local', 'p1', req, undefined);
    });

    it('deletePilot resolves true on success and re-throws a non-auth (404) failure', async () => {
      const deletePilot = vi.fn(async () => undefined as unknown as void);
      const session = pilotSession({ deletePilot });
      await expect(session.deletePilot('p1')).resolves.toBe(true);
      expect(deletePilot).toHaveBeenCalledWith('http://d.local', 'p1', undefined);

      const failing = pilotSession({
        deletePilot: vi.fn().mockRejectedValue(refusal(404, 'That pilot no longer exists.'))
      });
      await expect(failing.deletePilot('p1')).rejects.toThrow(/That pilot no longer exists\./);
    });
  });

  describe('classes (#84)', () => {
    const OPEN: Class = { id: 'c1', name: 'Open', source: 'MultiGP' };

    function classSession(overrides?: SessionOverrides) {
      return new Session({
        baseUrl: 'http://d.local',
        autoRestore: false,
        api: { ...overrides }
      });
    }

    it('listClasses reads the directory open (no token)', async () => {
      const listClasses = vi.fn(async () => [OPEN]);
      const session = classSession({ listClasses });
      const classes = await session.listClasses();
      expect(classes).toEqual([OPEN]);
      expect(listClasses).toHaveBeenCalledWith('http://d.local', { token: undefined });
    });

    it('createClass returns the new class (full-trust, no token)', async () => {
      const createClass = vi.fn(async () => OPEN);
      const session = classSession({ createClass });
      const req: CreateClassRequest = { name: 'Open', source: 'MultiGP' };
      const result = await session.createClass(req);
      expect(result).toEqual(OPEN);
      expect(createClass).toHaveBeenCalledWith('http://d.local', req, undefined);
    });

    it('createClass prompts then retries once on an auth (401) failure', async () => {
      const createClass = vi
        .fn()
        .mockRejectedValueOnce(refusal(401, 'Control on this Director needs a token.'))
        .mockResolvedValueOnce(OPEN);
      const session = classSession({ createClass });
      session.setTokenProvider(async () => 'tok');
      const result = await session.createClass({ name: 'Open', source: 'MultiGP' });
      expect(result).toEqual(OPEN);
      expect(createClass).toHaveBeenCalledTimes(2);
      expect(createClass).toHaveBeenLastCalledWith('http://d.local', expect.anything(), 'tok');
    });

    it('updateClass passes the clear-via-null diff through verbatim', async () => {
      const updated: Class = { ...OPEN, name: 'Pro Open' };
      const updateClass = vi.fn(async () => updated);
      const session = classSession({ updateClass });
      const req = { name: 'Pro Open', reference: null, description: null } as const;
      const result = await session.updateClass('c1', req);
      expect(result).toEqual(updated);
      expect(updateClass).toHaveBeenCalledWith('http://d.local', 'c1', req, undefined);
    });

    it('deleteClass resolves true on success and re-throws a non-auth (404) failure', async () => {
      const deleteClass = vi.fn(async () => undefined as unknown as void);
      const session = classSession({ deleteClass });
      await expect(session.deleteClass('c1')).resolves.toBe(true);
      expect(deleteClass).toHaveBeenCalledWith('http://d.local', 'c1', undefined);

      const failing = classSession({
        deleteClass: vi.fn().mockRejectedValue(refusal(404, 'That class no longer exists.'))
      });
      await expect(failing.deleteClass('c1')).rejects.toThrow(/That class no longer exists\./);
    });
  });

  // ── Auth-failure detection matches the real HTTP status, never digits inside a message ──────
  describe('auth-failure status matching', () => {
    it('does NOT prompt on a 500 whose message merely contains "403"', async () => {
      // The status is 500 — "403" appears only inside the Director's prose. The old whole-message
      // \b(401|403)\b scan matched it and opened the token dialog on a plain server error, and
      // matching a `failed: HTTP <status>` suffix would tie auth detection to wording the Director
      // now writes itself (#433). Only the structural status counts.
      const deleteEvent = vi.fn(async () => {
        throw refusal(500, 'The event could not be deleted — 403 laps were still being written.');
      });
      const tokenProvider = vi.fn(async () => 'lazy-tok');
      const session = new Session({ autoRestore: false, api: { deleteEvent } });
      session.setTokenProvider(tokenProvider);

      await expect(session.deleteEvent('evt-401')).rejects.toThrow(/403 laps/);
      expect(tokenProvider).not.toHaveBeenCalled();
      expect(deleteEvent).toHaveBeenCalledOnce();
    });

    it('prompts + retries on an error carrying the HTTP status STRUCTURALLY (status: 403)', async () => {
      let calls = 0;
      const deleteEvent = vi.fn(async () => {
        calls += 1;
        if (calls === 1) throw refusal(403, 'Control on this Director needs a token.');
        return undefined as unknown as void;
      });
      const tokenProvider = vi.fn(async () => 'lazy-tok');
      const session = new Session({ autoRestore: false, api: { deleteEvent } });
      session.setTokenProvider(tokenProvider);

      const ok = await session.deleteEvent('evt-a');
      expect(tokenProvider).toHaveBeenCalledOnce();
      expect(deleteEvent).toHaveBeenCalledTimes(2);
      expect(ok).toBe(true);
    });
  });
});
