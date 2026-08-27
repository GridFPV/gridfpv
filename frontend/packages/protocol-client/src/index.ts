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
  deleteEvent,
  getActiveEvent,
  setActiveEvent,
  listTimers,
  createTimer,
  updateTimer,
  deleteTimer,
  connectTimer,
  disconnectTimer,
  restartTimer,
  setCalibration,
  captureLevel,
  setNodeChannel,
  timerNodes,
  setTimerNodes,
  timerSignal,
  stopTimerSignal,
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
  rateChannels,
  createRound,
  updateRound,
  deleteRound,
  listChannelLayouts,
  createChannelLayout,
  updateChannelLayout,
  deleteChannelLayout,
  listHeats,
  listRoundIssues,
  eventAudit,
  roundRanking,
  roundStandings,
  classStandings
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

/**
 * The outbound calibration body for {@link setCalibration}, re-exported so a caller has one import
 * site for the call and its payload. The definition is the ts-rs binding generated from the
 * Director's own route — this package hand-writes no wire shape of its own (see the module note
 * above), so the page and the Director cannot disagree about it.
 */
export type { CalibrationRequest } from '@gridfpv/types';

/**
 * The **capture** wire shapes (#355) — the outbound body for {@link captureLevel} and the dispatch
 * it answers with. Re-exported for the same reason {@link CalibrationRequest} is: one import site
 * for the call and its payload, and a definition that is the ts-rs binding rather than a
 * hand-written guess.
 */
export type { CaptureDispatch, CaptureRequest, CaptureThreshold } from '@gridfpv/types';

/**
 * The outbound channel body for {@link setNodeChannel} and the dispatch it answers with (#413),
 * re-exported for the same reason {@link CalibrationRequest} is: one import site for the call and
 * its payload, and a definition that is the ts-rs binding rather than a hand-written guess.
 */
export type { ChannelDispatch, ChannelRequest } from '@gridfpv/types';

/**
 * The channel-layout wire shapes (#117 S2) — the view every layout read/write answers with, and the
 * two request bodies. Re-exported for the same reason {@link CalibrationRequest} is: one import
 * site for the call and its payload, and a definition that is the ts-rs binding rather than a
 * hand-written guess.
 */
export type {
  ChannelLayout,
  ChannelLayouts,
  LayoutId,
  LayoutNode,
  LayoutOverlap,
  NewChannelLayoutRequest,
  SetChannelLayoutRequest
} from '@gridfpv/types';
