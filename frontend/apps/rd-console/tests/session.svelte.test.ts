import { describe, expect, it, vi } from 'vitest';
import type {
  Class,
  CreateClassRequest,
  CreatePilotRequest,
  DiscoveredTimer,
  EventMeta,
  Format,
  Pilot,
  SavedFormat,
  SeatProfile,
  SeatProfileRequest,
  Timer,
  TimerConfig,
  TimerProfiles,
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

  it('readActiveEvent gives the whole event, null for none, and throws when the read fails', async () => {
    const getActiveEvent = vi.fn(async () => ({ event: EVENT_A }));
    const session = new Session({ autoRestore: false, api: { getActiveEvent } });
    expect(await session.readActiveEvent()).toEqual(EVENT_A);
    expect(getActiveEvent).toHaveBeenCalledWith(session.baseUrl, { token: undefined });
    // Reading it opens nothing in this session.
    expect(session.currentEvent).toBeUndefined();

    const none = new Session({
      autoRestore: false,
      api: { getActiveEvent: vi.fn(async () => ({ event: null })) }
    });
    expect(await none.readActiveEvent()).toBeNull();

    const down = new Session({
      autoRestore: false,
      api: {
        getActiveEvent: vi.fn(async () => {
          throw new Error('fetch failed');
        })
      }
    });
    await expect(down.readActiveEvent()).rejects.toThrow('fetch failed');
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
      threshold: { Auto: { snr_enter: 150, hysteresis: 50 } }
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

  describe('updateEvent', () => {
    const RENAMED: EventMeta = { ...EVENT_A, name: 'Saturday Series', location: 'Back field' };
    const REQUEST = { name: 'Saturday Series', location: 'Back field' };
    const eventSession = (overrides?: SessionOverrides) =>
      new Session({ baseUrl: 'http://d.local', autoRestore: false, api: { ...overrides } });

    it('updateEvent sends the name and details with the held token and returns the event as it now is', async () => {
      const updateEvent = vi.fn(async () => RENAMED);
      const session = eventSession({ updateEvent });
      session.setToken('tok');
      expect(await session.updateEvent('evt-a', REQUEST)).toEqual(RENAMED);
      expect(updateEvent).toHaveBeenCalledOnce();
      expect(updateEvent).toHaveBeenCalledWith('http://d.local', 'evt-a', REQUEST, 'tok');
    });

    it('updateEvent makes the open event follow when it is the one that changed', async () => {
      const session = eventSession({ updateEvent: vi.fn(async () => RENAMED) });
      session.selectEvent(EVENT_A);
      await session.updateEvent('evt-a', REQUEST);
      expect(session.currentEvent).toEqual(RENAMED);
    });

    it('updateEvent leaves the open event alone when a different event changed', async () => {
      const other: EventMeta = { id: 'evt-b', name: 'Sunday', created_at: 2, persistent: true };
      const changed: EventMeta = { ...other, name: 'Sunday Finals' };
      const session = eventSession({ updateEvent: vi.fn(async () => changed) });
      session.selectEvent(EVENT_A);
      expect(await session.updateEvent('evt-b', { name: 'Sunday Finals' })).toEqual(changed);
      expect(session.currentEvent).toEqual(EVENT_A);

      // And with no event open, none is opened.
      const closed = eventSession({ updateEvent: vi.fn(async () => changed) });
      await closed.updateEvent('evt-b', { name: 'Sunday Finals' });
      expect(closed.currentEvent).toBeUndefined();
    });

    it('updateEvent sends without a token first, then prompts once when refused for auth', async () => {
      const updateEvent = vi
        .fn()
        .mockRejectedValueOnce(refusal(401, 'Control on this Director needs a token.'))
        .mockResolvedValueOnce(RENAMED);
      const provider = vi.fn(async () => 'tok');
      const session = eventSession({ updateEvent });
      session.setTokenProvider(provider);
      session.selectEvent(EVENT_A);
      expect(await session.updateEvent('evt-a', REQUEST)).toEqual(RENAMED);
      expect(provider).toHaveBeenCalledOnce();
      expect(updateEvent).toHaveBeenCalledTimes(2);
      expect(updateEvent).toHaveBeenNthCalledWith(1, 'http://d.local', 'evt-a', REQUEST, undefined);
      expect(updateEvent).toHaveBeenNthCalledWith(2, 'http://d.local', 'evt-a', REQUEST, 'tok');
      expect(session.currentEvent).toEqual(RENAMED);
    });

    it('updateEvent resolves undefined and keeps the open event when the token prompt is cancelled', async () => {
      const updateEvent = vi.fn().mockRejectedValue(refusal(403, 'Control is gated.'));
      const session = eventSession({ updateEvent });
      session.setTokenProvider(async () => undefined);
      session.selectEvent(EVENT_A);
      expect(await session.updateEvent('evt-a', REQUEST)).toBeUndefined();
      expect(updateEvent).toHaveBeenCalledTimes(1);
      expect(session.currentEvent).toEqual(EVENT_A);
    });

    it("updateEvent throws the Director's sentence for an empty name, with no prompt, and keeps the open event", async () => {
      const SAID = 'An event needs a name.';
      const provider = vi.fn(async () => 'tok');
      const session = eventSession({
        updateEvent: vi.fn().mockRejectedValue(refusal(400, SAID))
      });
      session.setTokenProvider(provider);
      session.selectEvent(EVENT_A);
      await expect(session.updateEvent('evt-a', { name: '' })).rejects.toThrow(SAID);
      expect(provider).not.toHaveBeenCalled();
      expect(session.currentEvent).toEqual(EVENT_A);
    });
  });

  describe('saved formats', () => {
    const FORMAT: Format = {
      name: '2 minute qualifying',
      race: { end: { FirstToLaps: 3 }, staging_micros: 5_000_000, first_lap: 'HoleShot' },
      completion: { SavedRaces: 1 },
      ranking: { sort: ['LastFinish'] }
    };
    const SAVED: SavedFormat = { id: 'fmt-1a2b', format: FORMAT };
    const formatSession = (overrides?: SessionOverrides) =>
      new Session({ baseUrl: 'http://d.local', autoRestore: false, api: { ...overrides } });

    it('listFormats reads the saved formats open (no token)', async () => {
      const listFormats = vi.fn(async () => [SAVED]);
      const session = formatSession({ listFormats });
      expect(await session.listFormats()).toEqual([SAVED]);
      expect(listFormats).toHaveBeenCalledWith('http://d.local', { token: undefined });
    });

    it('listFormats throws when the read fails', async () => {
      const session = formatSession({
        listFormats: vi.fn(async () => {
          throw new Error('fetch failed');
        })
      });
      await expect(session.listFormats()).rejects.toThrow('fetch failed');
    });

    it('createFormat sends without a token first, then prompts once when refused for auth', async () => {
      const createFormat = vi
        .fn()
        .mockRejectedValueOnce(refusal(401, 'Control on this Director needs a token.'))
        .mockResolvedValueOnce(SAVED);
      const provider = vi.fn(async () => 'tok');
      const session = formatSession({ createFormat });
      session.setTokenProvider(provider);
      expect(await session.createFormat(FORMAT)).toEqual(SAVED);
      expect(provider).toHaveBeenCalledOnce();
      expect(createFormat).toHaveBeenCalledTimes(2);
      expect(createFormat).toHaveBeenNthCalledWith(1, 'http://d.local', FORMAT, undefined);
      expect(createFormat).toHaveBeenNthCalledWith(2, 'http://d.local', FORMAT, 'tok');
    });

    it("createFormat throws the Director's sentence for a name already taken, with no prompt", async () => {
      const SAID = 'There is already a format called 2 minute qualifying.';
      const provider = vi.fn(async () => 'tok');
      const session = formatSession({
        createFormat: vi.fn().mockRejectedValue(refusal(409, SAID))
      });
      session.setTokenProvider(provider);
      await expect(session.createFormat(FORMAT)).rejects.toThrow(SAID);
      expect(provider).not.toHaveBeenCalled();
    });

    it('createFormat resolves undefined when the token prompt is cancelled', async () => {
      const createFormat = vi.fn().mockRejectedValue(refusal(403, 'Control is gated.'));
      const session = formatSession({ createFormat });
      session.setTokenProvider(async () => undefined);
      expect(await session.createFormat(FORMAT)).toBeUndefined();
      expect(createFormat).toHaveBeenCalledTimes(1);
    });

    it('updateFormat sends the id and the format with the held token and returns the saved format', async () => {
      const renamed: SavedFormat = { id: 'fmt-1a2b', format: { ...FORMAT, name: 'Qualifying' } };
      const updateFormat = vi.fn(async () => renamed);
      const session = formatSession({ updateFormat });
      session.setToken('tok');
      expect(await session.updateFormat('fmt-1a2b', renamed.format)).toEqual(renamed);
      expect(updateFormat).toHaveBeenCalledOnce();
      expect(updateFormat).toHaveBeenCalledWith(
        'http://d.local',
        'fmt-1a2b',
        renamed.format,
        'tok'
      );
    });

    it('updateFormat prompts once when refused for auth, and throws any other refusal as written', async () => {
      const updateFormat = vi
        .fn()
        .mockRejectedValueOnce(refusal(401, 'Control on this Director needs a token.'))
        .mockResolvedValueOnce(SAVED);
      const session = formatSession({ updateFormat });
      session.setTokenProvider(async () => 'tok');
      expect(await session.updateFormat('fmt-1a2b', FORMAT)).toEqual(SAVED);
      expect(updateFormat).toHaveBeenNthCalledWith(
        1,
        'http://d.local',
        'fmt-1a2b',
        FORMAT,
        undefined
      );
      expect(updateFormat).toHaveBeenNthCalledWith(2, 'http://d.local', 'fmt-1a2b', FORMAT, 'tok');

      const SAID = 'That format no longer exists.';
      const provider = vi.fn(async () => 'tok');
      const failing = formatSession({
        updateFormat: vi.fn().mockRejectedValue(refusal(404, SAID))
      });
      failing.setTokenProvider(provider);
      await expect(failing.updateFormat('fmt-1a2b', FORMAT)).rejects.toThrow(SAID);
      expect(provider).not.toHaveBeenCalled();
    });

    it('deleteFormat resolves true once removed, and undefined when the prompt is cancelled', async () => {
      const deleteFormat = vi.fn(async () => {});
      const session = formatSession({ deleteFormat });
      session.setToken('tok');
      expect(await session.deleteFormat('fmt-1a2b')).toBe(true);
      expect(deleteFormat).toHaveBeenCalledWith('http://d.local', 'fmt-1a2b', 'tok');

      const refused = vi.fn().mockRejectedValue(refusal(403, 'Control is gated.'));
      const gated = formatSession({ deleteFormat: refused });
      gated.setTokenProvider(async () => undefined);
      expect(await gated.deleteFormat('fmt-1a2b')).toBeUndefined();
    });

    it('deleteFormat prompts once when refused for auth, and re-throws a non-auth (404) failure', async () => {
      const deleteFormat = vi
        .fn()
        .mockRejectedValueOnce(refusal(401, 'Control on this Director needs a token.'))
        .mockResolvedValueOnce(undefined);
      const session = formatSession({ deleteFormat });
      session.setTokenProvider(async () => 'tok');
      expect(await session.deleteFormat('fmt-1a2b')).toBe(true);
      expect(deleteFormat).toHaveBeenNthCalledWith(1, 'http://d.local', 'fmt-1a2b', undefined);
      expect(deleteFormat).toHaveBeenNthCalledWith(2, 'http://d.local', 'fmt-1a2b', 'tok');

      const failing = formatSession({
        deleteFormat: vi.fn().mockRejectedValue(refusal(404, 'That format no longer exists.'))
      });
      await expect(failing.deleteFormat('fmt-1a2b')).rejects.toThrow(
        /That format no longer exists\./
      );
    });
  });

  describe("a timer's seat profiles", () => {
    const CONFIG: TimerConfig = {
      seats: [{ seat: '0', freq_mhz: 5658, enabled: true }],
      threshold: { Auto: { snr_enter: 150, hysteresis: 50 } }
    };
    const REQUEST: SeatProfileRequest = { name: 'Garage', config: CONFIG };
    const PROFILE: SeatProfile = { id: 'prof-3c4d', name: 'Garage', config: CONFIG };
    const AUTH = 'Control on this Director needs a token.';
    const profileSession = (overrides?: SessionOverrides) =>
      new Session({ baseUrl: 'http://d.local', autoRestore: false, api: { ...overrides } });

    it("listProfiles reads a timer's profiles open (no token)", async () => {
      const profiles: TimerProfiles = { active: 'prof-3c4d', profiles: [PROFILE] };
      const listProfiles = vi.fn(async () => profiles);
      const session = profileSession({ listProfiles });
      expect(await session.listProfiles('gate-x1y2')).toEqual(profiles);
      expect(listProfiles).toHaveBeenCalledWith('http://d.local', 'gate-x1y2', {
        token: undefined
      });
    });

    it('listProfiles throws when the read fails', async () => {
      const SAID = 'That timer is not in the list any more.';
      const session = profileSession({
        listProfiles: vi.fn().mockRejectedValue(refusal(404, SAID))
      });
      await expect(session.listProfiles('gate-x1y2')).rejects.toThrow(SAID);
    });

    it('createProfile sends without a token first, then prompts once when refused for auth', async () => {
      const createProfile = vi
        .fn()
        .mockRejectedValueOnce(refusal(401, AUTH))
        .mockResolvedValueOnce(PROFILE);
      const provider = vi.fn(async () => 'tok');
      const session = profileSession({ createProfile });
      session.setTokenProvider(provider);
      expect(await session.createProfile('gate-x1y2', REQUEST)).toEqual(PROFILE);
      expect(provider).toHaveBeenCalledOnce();
      expect(createProfile).toHaveBeenCalledTimes(2);
      expect(createProfile).toHaveBeenNthCalledWith(
        1,
        'http://d.local',
        'gate-x1y2',
        REQUEST,
        undefined
      );
      expect(createProfile).toHaveBeenNthCalledWith(
        2,
        'http://d.local',
        'gate-x1y2',
        REQUEST,
        'tok'
      );
    });

    it("createProfile throws the Director's sentence, with no prompt, and resolves undefined when the prompt is cancelled", async () => {
      const SAID = 'This timer already has a profile called Garage.';
      const provider = vi.fn(async () => 'tok');
      const session = profileSession({
        createProfile: vi.fn().mockRejectedValue(refusal(409, SAID))
      });
      session.setTokenProvider(provider);
      await expect(session.createProfile('gate-x1y2', REQUEST)).rejects.toThrow(SAID);
      expect(provider).not.toHaveBeenCalled();

      const gated = profileSession({
        createProfile: vi.fn().mockRejectedValue(refusal(403, 'Control is gated.'))
      });
      gated.setTokenProvider(async () => undefined);
      expect(await gated.createProfile('gate-x1y2', REQUEST)).toBeUndefined();
    });

    it('updateProfile sends the timer, the profile and the settings with the held token', async () => {
      const renamed: SeatProfile = { ...PROFILE, name: 'Field' };
      const request: SeatProfileRequest = { name: 'Field', config: CONFIG };
      const updateProfile = vi.fn(async () => renamed);
      const session = profileSession({ updateProfile });
      session.setToken('tok');
      expect(await session.updateProfile('gate-x1y2', 'prof-3c4d', request)).toEqual(renamed);
      expect(updateProfile).toHaveBeenCalledOnce();
      expect(updateProfile).toHaveBeenCalledWith(
        'http://d.local',
        'gate-x1y2',
        'prof-3c4d',
        request,
        'tok'
      );
    });

    it('updateProfile prompts once when refused for auth, and throws any other refusal as written', async () => {
      const updateProfile = vi
        .fn()
        .mockRejectedValueOnce(refusal(401, AUTH))
        .mockResolvedValueOnce(PROFILE);
      const session = profileSession({ updateProfile });
      session.setTokenProvider(async () => 'tok');
      expect(await session.updateProfile('gate-x1y2', 'prof-3c4d', REQUEST)).toEqual(PROFILE);
      expect(updateProfile).toHaveBeenNthCalledWith(
        1,
        'http://d.local',
        'gate-x1y2',
        'prof-3c4d',
        REQUEST,
        undefined
      );
      expect(updateProfile).toHaveBeenNthCalledWith(
        2,
        'http://d.local',
        'gate-x1y2',
        'prof-3c4d',
        REQUEST,
        'tok'
      );

      const SAID = 'That profile no longer exists.';
      const provider = vi.fn(async () => 'tok');
      const failing = profileSession({
        updateProfile: vi.fn().mockRejectedValue(refusal(404, SAID))
      });
      failing.setTokenProvider(provider);
      await expect(failing.updateProfile('gate-x1y2', 'prof-3c4d', REQUEST)).rejects.toThrow(SAID);
      expect(provider).not.toHaveBeenCalled();
    });

    it('deleteProfile resolves true once removed, and undefined when the prompt is cancelled', async () => {
      const deleteProfile = vi.fn(async () => {});
      const session = profileSession({ deleteProfile });
      session.setToken('tok');
      expect(await session.deleteProfile('gate-x1y2', 'prof-3c4d')).toBe(true);
      expect(deleteProfile).toHaveBeenCalledWith('http://d.local', 'gate-x1y2', 'prof-3c4d', 'tok');

      const refused = vi.fn().mockRejectedValue(refusal(403, 'Control is gated.'));
      const gated = profileSession({ deleteProfile: refused });
      gated.setTokenProvider(async () => undefined);
      expect(await gated.deleteProfile('gate-x1y2', 'prof-3c4d')).toBeUndefined();
    });

    it('deleteProfile prompts once when refused for auth, and re-throws a non-auth (404) failure', async () => {
      const deleteProfile = vi
        .fn()
        .mockRejectedValueOnce(refusal(401, AUTH))
        .mockResolvedValueOnce(undefined);
      const session = profileSession({ deleteProfile });
      session.setTokenProvider(async () => 'tok');
      expect(await session.deleteProfile('gate-x1y2', 'prof-3c4d')).toBe(true);
      expect(deleteProfile).toHaveBeenNthCalledWith(
        1,
        'http://d.local',
        'gate-x1y2',
        'prof-3c4d',
        undefined
      );
      expect(deleteProfile).toHaveBeenNthCalledWith(
        2,
        'http://d.local',
        'gate-x1y2',
        'prof-3c4d',
        'tok'
      );

      const failing = profileSession({
        deleteProfile: vi.fn().mockRejectedValue(refusal(404, 'That profile no longer exists.'))
      });
      await expect(failing.deleteProfile('gate-x1y2', 'prof-3c4d')).rejects.toThrow(
        /That profile no longer exists\./
      );
    });

    it('applyProfile sends without a token first, then prompts once when refused for auth, and returns what the timer kept', async () => {
      const kept: TimerConfig = { ...CONFIG, lap_cooldown_ms: 2500 };
      const applyProfile = vi
        .fn()
        .mockRejectedValueOnce(refusal(401, AUTH))
        .mockResolvedValueOnce(kept);
      const provider = vi.fn(async () => 'tok');
      const session = profileSession({ applyProfile });
      session.setTokenProvider(provider);
      expect(await session.applyProfile('gate-x1y2', 'prof-3c4d')).toEqual(kept);
      expect(provider).toHaveBeenCalledOnce();
      expect(applyProfile).toHaveBeenCalledTimes(2);
      expect(applyProfile).toHaveBeenNthCalledWith(
        1,
        'http://d.local',
        'gate-x1y2',
        'prof-3c4d',
        undefined
      );
      expect(applyProfile).toHaveBeenNthCalledWith(
        2,
        'http://d.local',
        'gate-x1y2',
        'prof-3c4d',
        'tok'
      );
    });

    it("applyProfile throws the timer's refusal as the Director wrote it, with no prompt", async () => {
      const SAID = 'Could not reach Gate: connection refused';
      const provider = vi.fn(async () => 'tok');
      const session = profileSession({
        applyProfile: vi.fn().mockRejectedValue(refusal(409, SAID))
      });
      session.setTokenProvider(provider);
      await expect(session.applyProfile('gate-x1y2', 'prof-3c4d')).rejects.toThrow(SAID);
      expect(provider).not.toHaveBeenCalled();
    });

    it('applyProfile resolves undefined when the token prompt is cancelled', async () => {
      const applyProfile = vi.fn().mockRejectedValue(refusal(403, 'Control is gated.'));
      const session = profileSession({ applyProfile });
      session.setTokenProvider(async () => undefined);
      expect(await session.applyProfile('gate-x1y2', 'prof-3c4d')).toBeUndefined();
      expect(applyProfile).toHaveBeenCalledTimes(1);
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
