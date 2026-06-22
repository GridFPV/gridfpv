/**
 * @gridfpv/protocol-client — the thin, framework-agnostic protocol layer
 * described in docs/clients.html §3 and docs/protocol.html §2–§4.
 *
 * It connects to a single base URL (so it cannot tell LAN from Cloud — the same
 * client backs all three surfaces on both transports), performs the
 * "snapshot first, then subscribe" handshake (protocol.html §2), applies the
 * ordered change-envelope stream idempotently in sequence order (§3), and resumes
 * by cursor — falling back to a re-snapshot — across gaps and reconnects.
 *
 * Everything is typed against the ts-rs–generated wire types re-exported from
 * `@gridfpv/types`; this package hand-writes no wire shape of its own.
 */

export {
  connect,
  listEvents,
  createEvent,
  getActiveEvent,
  setActiveEvent,
  listTimers,
  createTimer,
  updateTimer,
  deleteTimer,
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
  setEventClasses,
  setClassMembership,
  listFormats,
  listChannels,
  createRound,
  updateRound,
  deleteRound,
  listHeats,
  roundRanking,
  classStandings,
  PRACTICE_EVENT_ID
} from './client.js';
export type {
  ConnectOptions,
  ProtocolClient,
  ProtocolState,
  ConnectionStatus,
  StateListener,
  WebSocketLike,
  WebSocketFactory,
  FetchLike
} from './client.js';
