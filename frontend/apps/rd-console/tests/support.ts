/**
 * Test support: build a `Session` whose seams are mocked so screens render and fire
 * commands without a real server. The returned `sendSpy` records every `Command` a
 * screen emits; `pushLive` injects a `LiveRaceState` onto the read stream.
 */
import { vi } from 'vitest';
import type {
  ProtocolClient,
  ProtocolState,
  StateListener,
  listEvents,
  deleteEvent,
  getActiveEvent,
  listTimers,
  createTimer,
  updateTimer,
  deleteTimer,
  connectTimer,
  disconnectTimer,
  setEventTimers,
  setPrimaryTimer,
  listPilots,
  createPilot,
  updatePilot,
  deletePilot,
  setEventRoster,
  addToRoster,
  removeFromRoster,
  listClasses,
  createClass,
  updateClass,
  deleteClass,
  setClassHidden,
  setEventClasses,
  setClassMembership,
  listFormats,
  listFormatSchemas,
  listChannels,
  setNodeChannel,
  timerNodes,
  setTimerNodes,
  createRound,
  updateRound,
  deleteRound,
  listHeats,
  listRoundIssues,
  eventAudit,
  roundRanking,
  roundStandings,
  classStandings
} from '@gridfpv/protocol-client';
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
import { Session, type SessionRole } from '../src/lib/session.svelte.js';

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

/** The registry seams a screen test can override (all optional; defaults are inert). */
export interface TimerImpls {
  /** The open event-list read — backs the EventPicker page tests. */
  listEventsImpl?: typeof listEvents;
  /** The hard-delete write seam — backs the EventPicker delete-dialog tests. */
  deleteEventImpl?: typeof deleteEvent;
  /** The active-event read (#91) — backs the EventPicker "Active" pill tests. */
  getActiveEventImpl?: typeof getActiveEvent;
  listTimersImpl?: typeof listTimers;
  createTimerImpl?: typeof createTimer;
  updateTimerImpl?: typeof updateTimer;
  deleteTimerImpl?: typeof deleteTimer;
  /** The manual connect/disconnect write seams (#383) — back the TimerManager connect tests. */
  connectTimerImpl?: typeof connectTimer;
  disconnectTimerImpl?: typeof disconnectTimer;
  /** The node-config read/write seams (#412) — back the TimerNodesDialog tests. */
  setTimerNodesImpl?: typeof setTimerNodes;
  setEventTimersImpl?: typeof setEventTimers;
  setPrimaryTimerImpl?: typeof setPrimaryTimer;
  /** The app-level pilot directory read (issue #74) — backs the hub + Pilots page tests. */
  listPilotsImpl?: typeof listPilots;
  /** The pilot-directory write seams (issue #74) — back the PilotManager tests. */
  createPilotImpl?: typeof createPilot;
  updatePilotImpl?: typeof updatePilot;
  deletePilotImpl?: typeof deletePilot;
  /** The per-event roster write seams (issue #74) — back the EventRoster tests. */
  setEventRosterImpl?: typeof setEventRoster;
  addToRosterImpl?: typeof addToRoster;
  removeFromRosterImpl?: typeof removeFromRoster;
  /** The app-level class directory read (issue #84) — backs the hub + Classes page tests. */
  listClassesImpl?: typeof listClasses;
  /** The class-directory write seams (issue #84) — back the ClassManager tests. */
  createClassImpl?: typeof createClass;
  updateClassImpl?: typeof updateClass;
  deleteClassImpl?: typeof deleteClass;
  /** The class hide/un-hide write seam (hide/archive classes) — backs the visibility tests. */
  setClassHiddenImpl?: typeof setClassHidden;
  /** The per-event class-selection write seam (issue #84) — backs the EventClasses tests. */
  setEventClassesImpl?: typeof setEventClasses;
  /** The per-class membership write seam (race redesign Slice 1b) — backs the EventRoster tests. */
  setClassMembershipImpl?: typeof setClassMembership;
  /** The valid format-names read (race redesign Slice 2b) — backs the EventRounds tests. */
  listFormatsImpl?: typeof listFormats;
  /** The format param-schemas read (race redesign Slice 7b) — backs the guided-params tests. */
  listFormatSchemasImpl?: typeof listFormatSchemas;
  /** The standard channel-catalog read (race redesign Slice 4b) — backs the Channels UI tests. */
  listChannelsImpl?: typeof listChannels;
  /** The per-node channel write (#413) — backs the Tune page's channel-dropdown tests. */
  setNodeChannelImpl?: typeof setNodeChannel;
  /** The node-set read (#412) — backs the Tune page's "never offer a disabled node" tests. */
  timerNodesImpl?: typeof timerNodes;
  /** The per-event round write seams (race redesign Slice 2b) — back the EventRounds tests. */
  createRoundImpl?: typeof createRound;
  updateRoundImpl?: typeof updateRound;
  deleteRoundImpl?: typeof deleteRound;
  /** The scheduled-heats read (race redesign Slice 3b) — backs the EventRounds Heats UI tests. */
  listHeatsImpl?: typeof listHeats;
  /** The stored-round seat check (#416) — backs the EventRounds impossible-seat tests. */
  listRoundIssuesImpl?: typeof listRoundIssues;
  /** The event-wide audit read — backs the EventAudit page tests. */
  eventAuditImpl?: typeof eventAudit;
  /** The round-ranking + class-standings reads (race redesign Slice 5/6a) — back the Results +
   * EventRounds standings/advance tests. */
  roundRankingImpl?: typeof roundRanking;
  /** The time-trial round-standings read (Best lap + win-condition metric) — backs the Results
   * timed_qual round-view tests. */
  roundStandingsImpl?: typeof roundStandings;
  classStandingsImpl?: typeof classStandings;
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
  } & TimerImpls
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
    connectImpl: () => client,
    controlFactory: () => ({ baseUrl: 'http://d.local', sendCommand: sendSpy }),
    baseUrl: 'http://d.local',
    autoRestore: false,
    // Event read/delete seams (EventPicker page): inert unless a test overrides them.
    listEventsImpl: opts?.listEventsImpl,
    deleteEventImpl: opts?.deleteEventImpl,
    getActiveEventImpl: opts?.getActiveEventImpl,
    // Timer-registry seams (issue #73): inert unless a test overrides them.
    listTimersImpl: opts?.listTimersImpl,
    createTimerImpl: opts?.createTimerImpl,
    updateTimerImpl: opts?.updateTimerImpl,
    deleteTimerImpl: opts?.deleteTimerImpl,
    connectTimerImpl: opts?.connectTimerImpl,
    disconnectTimerImpl: opts?.disconnectTimerImpl,
    // Node-config seams (#412): inert unless a test overrides them. Left undefined (so the real
    // fetch-backed impls stand) — `fetch` is stubbed to fail below, which is the right default:
    // the node dialog is only mounted by tests that seed it.
    timerNodesImpl: opts?.timerNodesImpl,
    setTimerNodesImpl: opts?.setTimerNodesImpl,
    setEventTimersImpl: opts?.setEventTimersImpl,
    setPrimaryTimerImpl: opts?.setPrimaryTimerImpl,
    // The pilots/heats directory reads default to an INERT SUCCESS (empty list), not the real
    // fetch-backed impls: `fetch` is stubbed to fail below, and a failed directory read now renders
    // a visible error state (#340) — which would leak into every test that doesn't override these
    // seams. Resolving `[]` preserves the old default semantics (empty directory, no error).
    listPilotsImpl: opts?.listPilotsImpl ?? (async () => []),
    // Pilot-directory write seams (issue #74): inert unless a test overrides them.
    createPilotImpl: opts?.createPilotImpl,
    updatePilotImpl: opts?.updatePilotImpl,
    deletePilotImpl: opts?.deletePilotImpl,
    // Per-event roster write seams (issue #74): inert unless a test overrides them.
    setEventRosterImpl: opts?.setEventRosterImpl,
    addToRosterImpl: opts?.addToRosterImpl,
    removeFromRosterImpl: opts?.removeFromRosterImpl,
    // Class-directory + per-event selection seams (issue #84): inert unless a test overrides them.
    listClassesImpl: opts?.listClassesImpl,
    createClassImpl: opts?.createClassImpl,
    updateClassImpl: opts?.updateClassImpl,
    deleteClassImpl: opts?.deleteClassImpl,
    setClassHiddenImpl: opts?.setClassHiddenImpl,
    setEventClassesImpl: opts?.setEventClassesImpl,
    // Per-class membership write seam (race redesign Slice 1b): inert unless a test overrides it.
    setClassMembershipImpl: opts?.setClassMembershipImpl,
    // Round read/write seams (race redesign Slice 2b): inert unless a test overrides them.
    listFormatsImpl: opts?.listFormatsImpl,
    // Format param-schemas read seam (race redesign Slice 7b): inert unless a test overrides it.
    listFormatSchemasImpl: opts?.listFormatSchemasImpl,
    // Channel-catalog read seam (race redesign Slice 4b): inert unless a test overrides it.
    listChannelsImpl: opts?.listChannelsImpl,
    setNodeChannelImpl: opts?.setNodeChannelImpl,
    createRoundImpl: opts?.createRoundImpl,
    updateRoundImpl: opts?.updateRoundImpl,
    deleteRoundImpl: opts?.deleteRoundImpl,
    // Scheduled-heats read seam (race redesign Slice 3b): inert success unless a test overrides it
    // (see the listPilotsImpl note — a failing default would trip the #340 error state everywhere).
    listHeatsImpl: opts?.listHeatsImpl ?? (async () => []),
    // The stored-round seat check (#416): inert success (nothing wrong) unless a test overrides it —
    // a failing default would trip the "couldn't check" banner in every unrelated Rounds test.
    listRoundIssuesImpl: opts?.listRoundIssuesImpl ?? (async () => []),
    // The event-wide audit read: inert success (empty trail) unless a test overrides it — a
    // failing default would trip the Audit page's #340 error state in unrelated tests.
    eventAuditImpl: opts?.eventAuditImpl ?? (async () => []),
    // Ranking + standings read seams (race redesign Slice 5/6a): inert unless overridden.
    roundRankingImpl: opts?.roundRankingImpl,
    roundStandingsImpl: opts?.roundStandingsImpl,
    classStandingsImpl: opts?.classStandingsImpl
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
