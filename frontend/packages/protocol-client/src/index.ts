/**
 * @gridfpv/protocol-client — the thin, framework-agnostic protocol layer.
 *
 * It talks to a single base URL (so it cannot tell LAN from Cloud) over plain HTTP: the event
 * registry, the active event, the app-level pilot, class and timer directories, and an event's
 * competition (snapshot, commands, presets, passes and the change feed), and a timer's
 * settings and live signal feed. Every failed
 * request rejects with a {@link RequestFailure} carrying the Director's own words and the
 * HTTP status.
 *
 * Everything is typed against the ts-rs–generated wire types re-exported from
 * `@gridfpv/types`; this package hand-writes no wire shape of its own.
 */

export {
  isRequestFailure,
  listEvents,
  createEvent,
  deleteEvent,
  getActiveEvent,
  setActiveEvent,
  listPilots,
  createPilot,
  updatePilot,
  deletePilot,
  listClasses,
  createClass,
  updateClass,
  deleteClass,
  setClassHidden,
  listTimers,
  createTimer,
  deleteTimer,
  getTimerConfig,
  putTimerConfig,
  getEventState,
  sendCommand,
  createBracket,
  createMains,
  postPass,
  subscribeChanges,
  subscribeTimerLive
} from './client.js';
export type {
  RequestFailure,
  FetchLike,
  EventSourceLike,
  EventSourceConstructor,
  TimerLiveHandlers
} from './client.js';
