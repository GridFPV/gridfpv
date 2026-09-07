/**
 * Test support: build a `Session` whose seams are mocked so screens render and fire
 * commands without a real server. The returned `sendSpy` records every `Command` a
 * screen emits; `pushLive` injects a `LiveRaceState` onto the read stream.
 */
import { vi } from 'vitest';
import type { ProtocolClient, ProtocolState, StateListener } from '@gridfpv/protocol-client';
import type {
  AuditEntry,
  Command,
  CommandAck,
  EventMeta,
  HeatId,
  HeatResult,
  LapList,
  LiveRaceState,
  SignalTraceView
} from '@gridfpv/types';
import { Session, type SessionApi, type SessionRole } from '../src/lib/session.svelte.js';

/**
 * The event the screen tests render inside — an ordinary **created** event named "Practice",
 * which is what an RD makes now that the built-in one is gone (#414).
 */
const PRACTICE: EventMeta = {
  id: 'practice-ab12',
  name: 'Practice',
  created_at: 0,
  persistent: true,
  timers: ['mock'],
  roster: [],
  classes: []
};

/**
 * The seams a screen test can override — **one key per {@link SessionApi} entry**, suffixed
 * `Impl` (`listHeatsImpl` steers `listHeats`). Derived from the session's own API record rather
 * than restated, so a new endpoint is overridable here the moment it exists and the two lists
 * cannot drift. All optional; anything a test leaves out keeps the default below.
 */
export type SessionSeams = { [K in keyof SessionApi as `${K}Impl`]?: SessionApi[K] };

/**
 * The seams a test actually named, keyed the way {@link Session} wants them.
 *
 * The `Impl` suffix is what marks an option as a **seam** rather than one of the seed values
 * ({@link makeTestSession}'s `live`, `laps`, `role`, …), so the whole options object can be handed
 * over and only the seams are read. A key passed as `undefined` is dropped, so a test may forward
 * an optional override straight through (`listHeatsImpl: overrides?.listHeatsImpl`) without
 * blanking the default below.
 */
function seamOverrides(opts: SessionSeams | undefined): Partial<SessionApi> {
  return Object.fromEntries(
    Object.entries(opts ?? {})
      .filter(([key, impl]) => key.endsWith('Impl') && impl !== undefined)
      .map(([key, impl]) => [key.slice(0, -'Impl'.length), impl])
  ) as Partial<SessionApi>;
}

export interface TestSession {
  session: Session;
  sendSpy: ReturnType<typeof vi.fn<(c: Command) => Promise<CommandAck>>>;
  pushLive: (state: LiveRaceState) => void;
}

export function makeTestSession(
  opts?: {
    ack?: CommandAck;
    live?: LiveRaceState;
    event?: EventMeta;
    /** Skip entering an event — for the app-level hub/page tests that render with no event. */
    noEnter?: boolean;
    /** Seed the marshaling lap list (#55) — the Marshaling screen reads `session.lapList`. */
    laps?: LapList;
    /** Seed the marshaling audit trail (#55) — the screen reads `session.marshalingAudit`. */
    audit?: AuditEntry[];
    /** Seed the captured RSSI signal trace (Slice 4) — the screen reads `session.signalTrace`. */
    signal?: SignalTraceView;
    /**
     * Seed the MARSHALED heat's own live-state fold (`session.heatLiveState`, `?projection=live`
     * over that heat's window). Distinct from `live` (the global live stream): the marshaled heat
     * carries its durable `progress[].pilot` registration bindings here, which the resolver reads to
     * render callsigns for a finished / node-seeded heat. Backs the durable-binding tests.
     */
    heatLive?: LiveRaceState;
    /** The session role (#80). Defaults to `'rd'`; pass `'readonly'` to assert gating. */
    role?: SessionRole;
    /**
     * Serve heat-scope snapshot reads (`/snapshot/heat/{heat}?projection=…`) from the stubbed
     * `fetch`, keyed heat → projection seed. By default every fetch fails (inert — the seeded
     * `laps`/`audit`/… values above stand); a test that exercises the session's real heat-scope
     * fetch path (e.g. `ensureHeatBindings`' durable per-heat bindings, `fetchHeatResult`) seeds
     * the heats it needs here and the stub answers with the wire envelope
     * (`{ body: { LiveRaceState: … } }`). Any un-seeded heat/projection still fails.
     */
    heatFetches?: Record<
      HeatId,
      {
        live?: LiveRaceState;
        result?: HeatResult;
        laps?: LapList;
        audit?: AuditEntry[];
        signal?: SignalTraceView;
      }
    >;
  } & SessionSeams
): TestSession {
  const ack: CommandAck = opts?.ack ?? { ok: true };
  const sendSpy = vi.fn<(c: Command) => Promise<CommandAck>>(async () => ack);

  let listener: StateListener | undefined;
  const initial: ProtocolState = {
    body: opts?.live ? { LiveRaceState: opts.live } : undefined,
    cursor: undefined,
    status: 'live',
    error: undefined
  };
  const client: ProtocolClient = {
    baseUrl: 'http://d.local',
    scope: { Event: { event: 'e' } },
    getState: () => initial,
    onState: (l) => {
      listener = l;
      l(initial);
      return () => (listener = undefined);
    },
    close: () => {}
  };

  const session = new Session({
    baseUrl: 'http://d.local',
    autoRestore: false,
    api: {
      connect: () => client,
      createControlClient: () => ({ baseUrl: 'http://d.local', sendCommand: sendSpy }),
      // The directory reads below default to an INERT SUCCESS (an empty list), not the real
      // fetch-backed ones: `fetch` is stubbed to fail further down, and a failed directory read
      // renders a visible error state (#340) — which would otherwise leak into every test that
      // doesn't name these seams. Resolving empty is the old default semantics: nothing there,
      // no error. Every other seam keeps its real implementation, which the stubbed `fetch`
      // makes inert, and a test that needs one to answer names it below.
      listPilots: async () => [],
      listChannelLayouts: async () => ({ layouts: [], overlaps: [] }),
      listHeats: async () => [],
      // The stored-round seat check (#416): nothing wrong, so the "couldn't check" banner stays
      // down in every test that isn't about it.
      listRoundIssues: async () => [],
      eventAudit: async () => [],
      ...seamOverrides(opts)
    }
  });
  // Seed a token (so privileged sends don't trigger the lazy prompt) and enter the event,
  // unless the test wants the app-level (no-event) context.
  session.setToken('tok');
  if (!opts?.noEnter) session.selectEvent(opts?.event ?? PRACTICE);

  // Marshaling (#55): seed the lap list / audit / role the screen reads, and stub `fetch` so the
  // screen's `refreshMarshaling` (a heat-scope snapshot read) is inert in tests — it re-affirms the
  // seeded values rather than hitting a server. A test that wants to assert a *re-fold* re-seeds the
  // values and dispatches a stream tick.
  if (opts?.role) session.setRole(opts.role);
  if (opts?.laps) session.lapList = opts.laps;
  if (opts?.audit) session.marshalingAudit = opts.audit;
  if (opts?.signal) session.signalTrace = opts.signal;
  if (opts?.heatLive) session.heatLiveState = opts.heatLive;
  // The wire body key per heat-scope projection (mirrors the session's #fetchHeatProjection).
  const bodyKeyOf = (projection: string): string | undefined =>
    (
      ({
        live: 'LiveRaceState',
        result: 'HeatResult',
        laps: 'LapList',
        audit: 'MarshalingAudit',
        signal: 'SignalTrace'
      }) as Record<string, string>
    )[projection];
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      const m = /\/snapshot\/heat\/([^/?]+)\?projection=(\w+)/.exec(url);
      if (m && opts?.heatFetches) {
        const heat = decodeURIComponent(m[1]);
        const projection = m[2] as 'live' | 'result' | 'laps' | 'audit' | 'signal';
        const seeded = opts.heatFetches[heat]?.[projection];
        const bodyKey = bodyKeyOf(projection);
        if (seeded !== undefined && bodyKey !== undefined) {
          return {
            ok: true,
            json: async () => ({ body: { [bodyKey]: seeded } })
          } as unknown as Response;
        }
      }
      return { ok: false, json: async () => ({}) } as unknown as Response;
    })
  );

  const pushLive = (state: LiveRaceState) =>
    listener?.({ body: { LiveRaceState: state }, cursor: 1, status: 'live', error: undefined });

  return { session, sendSpy, pushLive };
}
