/**
 * RaceGOW — the solo track-run preset, and the pure helpers its screens share.
 *
 * [RaceGOW](https://www.racegow.com/) is an at-home tiny-whoop competition: build the season's
 * track in your living room, fly it, and submit your **fastest 3 consecutive laps** as a video
 * clip plus the time. GridFPV already runs that event shape — a time-trial round scored by best
 * consecutive laps, open-ended so you fly as many runs as you like — but reaching it took six
 * screens (create an event, the wizard's timer / roster / round steps, Race control, Results).
 * A pilot alone in a garage does not want a race director's console; they want a start button.
 *
 * **This module is orchestration and arithmetic, not a second engine.** {@link createRaceGowRun}
 * builds the event through the *same* session commands the setup wizard uses — a RaceGOW run is an
 * ordinary event carrying an ordinary round, marked with the `racegow` preset (`EventMeta.preset`)
 * only so the console opens it on the solo run screen. Everything about it stays editable in the
 * full workspace, and every lap it records goes on the same append-only log, scored by the same
 * engine, with the same marshaling and audit surfaces behind it.
 *
 * The RaceGOW rules that shape the preset (their General Rules and Basic Concept docs):
 *   - the score is the **fastest 3 consecutive laps** → `WinCondition::BestConsecutive { n: 3 }`,
 *     which the engine already ranks across every heat of a round;
 *   - runs are unlimited → the time-trial format's `rounds = 0` (open-ended: every fill draws the
 *     next heat), and **no time limit** on the heat — the pilot stops it;
 *   - the video must show a **timer in view** → the `#/overlay/racegow` OBS browser source;
 *   - no cutting laps together → the three laps are consecutive on the log, with timestamps.
 */

import type {
  EventMeta,
  Lap,
  LapList,
  NewRoundReq,
  PilotId,
  RoundDef,
  RoundStanding,
  TimerId
} from '@gridfpv/types';
import type { Session } from './session.svelte.js';
import { formatHash, type OverlayTheme } from './route.js';

/** The `EventMeta.preset` marker a RaceGOW run carries. */
export const RACEGOW_PRESET = 'racegow';
/** The built-in **Whoop Class** (`mgp-whoop`) every RaceGOW run races in — RaceGOW is whoops. */
export const RACEGOW_CLASS_ID = 'mgp-whoop';
/** The consecutive-lap window RaceGOW scores: your fastest **3** laps in a row. */
export const RACEGOW_LAPS = 3;
/** The one round a RaceGOW run carries, by label. */
export const RACEGOW_ROUND_LABEL = 'RaceGOW';
/**
 * The safety ceiling on one run, in seconds. Best-consecutive scoring never ends a heat by
 * itself, and the Director refuses a round with no way to end; ten minutes is far past any
 * whoop pack, so in practice the pilot's Stop button ends every run.
 */
export const RACEGOW_RUN_CEILING_SECS = 600;
/** The season's first track, as the form's default name. */
export const DEFAULT_TRACK_NAME = 'RaceGOW6 Track 1';

/** Whether `meta` is a RaceGOW run — an event the RaceGOW preset created. */
export function isRaceGowEvent(meta: EventMeta | undefined): meta is EventMeta {
  return meta?.preset === RACEGOW_PRESET;
}

/**
 * The run's scoring round: the first time-trial round on the event. The preset creates exactly
 * one, but the event stays fully editable, so this is a lookup rather than an assumption.
 */
export function raceGowRound(meta: EventMeta | undefined): RoundDef | undefined {
  return meta?.rounds?.find((r) => r.format === 'timed_qual');
}

/**
 * The round request the preset creates — the whole RaceGOW shape in one place.
 *
 * - `timed_qual` with `rounds: '0'` — **open-ended**: each "Fly again" fills the next heat, and
 *   the round ranks the best window across all of them.
 * - `BestConsecutive { n: 3 }` — the RaceGOW score. The condition only *ranks*; it never ends a
 *   heat, so the Director insists on a race time for it (a heat with no way to end is refused
 *   on create). The **10-minute** limit is that ceiling and nothing more: a whoop pack is a few
 *   minutes, and the pilot's Stop button is how a run normally ends. A run that does hit the
 *   limit closes on its own and the run screen offers to save it.
 * - `Static` channel mode — the pilot's own fixed channel (the membership slot), like any
 *   time trial.
 * - a **fixed 1 s start hold** — the randomized 2–5 s race-start delay is for a field of pilots
 *   waiting on a tone; alone, the timer is live one second after Start and laps count from the
 *   first crossing, exactly as RaceGOW times a run (gate to gate, no start signal).
 * - a short staging window (informational only — the run screen stages and starts together).
 */
export function raceGowRoundRequest(): NewRoundReq {
  return {
    label: RACEGOW_ROUND_LABEL,
    classes: [RACEGOW_CLASS_ID],
    format: 'timed_qual',
    params: { rounds: '0' },
    win_condition: { BestConsecutive: { n: RACEGOW_LAPS } },
    time_limit_secs: RACEGOW_RUN_CEILING_SECS,
    seeding: 'FromRoster',
    channel_mode: 'Static',
    staging_timer_secs: 30,
    start_procedure: { mode: 'randomized-delay', min_delay_ms: 1000, max_delay_ms: 1000 }
  };
}

/** What the preset needs to build a run: the track, who flies it, and on what. */
export interface NewRaceGowRun {
  /** The event's display name — the track, e.g. `RaceGOW6 Track 1`. */
  track: string;
  /** The directory pilot flying it (the whole roster). */
  pilot: PilotId;
  /** The timer to fly on — the event's only timer, and so its primary. */
  timer: TimerId;
  /**
   * The pilot's video channel in raw MHz, from the timer's configured channels. Omitted only when
   * the timer offers none (the fill then refuses to seat the heat and says so — a real timer
   * needs its channels configured on the Timers page first).
   */
  channel?: number;
}

/**
 * Build a RaceGOW run: create the event and enter it, then configure it through the ordinary
 * commands — timer + primary, the Whoop class, the one-pilot roster and its channel slot, and the
 * scoring round. Resolves to the configured {@link EventMeta}, or `undefined` if the RD cancelled
 * the control-token prompt partway (the event may then exist half-built; it is an ordinary event
 * and the Events page can delete it). Throws on a transport / HTTP failure, message intact.
 *
 * Deliberately **client-side composition**, the way the setup wizard is: one server endpoint
 * doing all of this would be a second place the event shape lives.
 */
export async function createRaceGowRun(
  session: Session,
  run: NewRaceGowRun
): Promise<EventMeta | undefined> {
  const created = await session.createEventAndEnter(run.track.trim(), {
    preset: RACEGOW_PRESET
  });
  if (!created) return undefined;
  // Timer first: the membership channel below is validated against the primary timer's pool.
  if (!(await session.setEventTimers([run.timer]))) return undefined;
  if (!(await session.setPrimaryTimer(run.timer))) return undefined;
  if (!(await session.setEventClasses([RACEGOW_CLASS_ID]))) return undefined;
  if (!(await session.setEventRoster([run.pilot]))) return undefined;
  const slot =
    run.channel === undefined ? { pilot: run.pilot } : { pilot: run.pilot, channel: run.channel };
  if (!(await session.setClassMembership(RACEGOW_CLASS_ID, [slot]))) return undefined;
  if (!(await session.createRound(raceGowRoundRequest()))) return undefined;
  // Each write re-homed `currentEvent`, so that is the freshest meta; the created one is the
  // fallback for a session whose writes were stubbed.
  return session.currentEvent ?? created;
}

/** A consecutive-lap window: where it starts (0-based lap index) and what it sums to. */
export interface ConsecutiveWindow {
  /** Index of the window's first lap in the lap list it was found in. */
  start: number;
  /** The window's laps, in order (µs each). */
  laps: number[];
  /** The window's total (µs) — the RaceGOW time. */
  totalMicros: number;
}

/**
 * The fastest `n`-consecutive-lap window of a lap sequence (durations in µs), or `undefined` with
 * fewer than `n` laps. Ties go to the **earliest** window, matching the engine's tie-break by the
 * completion time of the window's last lap. Pure; the run screen and the overlay both use it on
 * the *served* lap list, so a voided or edited lap is already reflected.
 */
export function bestConsecutive(
  lapMicros: readonly number[],
  n: number = RACEGOW_LAPS
): ConsecutiveWindow | undefined {
  if (n <= 0 || lapMicros.length < n) return undefined;
  let best: ConsecutiveWindow | undefined;
  for (let start = 0; start + n <= lapMicros.length; start += 1) {
    const laps = lapMicros.slice(start, start + n);
    const totalMicros = laps.reduce((sum, lap) => sum + lap, 0);
    if (!best || totalMicros < best.totalMicros) best = { start, laps, totalMicros };
  }
  return best;
}

/** The lap durations of a solo run's lap list — the one competitor's laps, in order. */
export function runLaps(list: LapList | undefined): Lap[] {
  // A solo run has one competitor; tolerate more (a hand-edited event) by taking them in order.
  return (list?.competitors ?? []).flatMap((c) => c.laps);
}

/** The served round-wide best window (µs) off the standings, or `undefined` before one exists. */
export function servedBestMicros(standings: readonly RoundStanding[]): number | undefined {
  for (const row of standings) {
    const m = row.metric;
    if (typeof m === 'object' && 'BestConsecutive' in m && m.BestConsecutive.micros != null) {
      return m.BestConsecutive.micros;
    }
  }
  return undefined;
}

/**
 * The OBS browser-source URL for the RaceGOW overlay, from the console's own location. `theme`
 * picks the panel's contrast for the footage underneath — light text on a dark panel (default)
 * or dark text on a light one (`#/overlay/racegow/light`, see `route.ts`).
 */
export function overlayUrl(
  loc: { origin: string; pathname: string },
  theme: OverlayTheme = 'dark'
): string {
  return `${loc.origin}${loc.pathname}${formatHash({ kind: 'overlay', overlay: 'racegow', theme })}`;
}

/** What the submission line needs — everything a RaceGOW form asks beside the clip. */
export interface SubmissionInput {
  track: string;
  callsign: string;
  best: ConsecutiveWindow;
  /** 1-based run number the window came from, and how many runs were flown. */
  run: number;
  runs: number;
  /** When the best run was flown. */
  flownAt: Date;
  /** The timer that timed it (its friendly name and kind, e.g. `Track RH (RotorHazard)`). */
  timedBy: string;
}

/** `S.mmm` seconds from µs, without the components' minute roll-over (a lap window is seconds). */
function seconds(micros: number): string {
  return (Math.floor(micros / 1000) / 1000).toFixed(3);
}

/**
 * The copy-and-paste line for the RaceGOW submission form: the time, the three laps, which run
 * and when, and what timed it. Plain text — it goes into a Google Form field.
 */
export function submissionText(input: SubmissionInput): string {
  const laps = input.best.laps.map(seconds).join(' / ');
  const when = input.flownAt.toLocaleString(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short'
  });
  return [
    `${input.track} — ${input.callsign}`,
    `Best ${input.best.laps.length} consecutive laps: ${seconds(input.best.totalMicros)} s (${laps})`,
    `Run ${input.run} of ${input.runs}, laps ${input.best.start + 1}–${input.best.start + input.best.laps.length}, ${when}`,
    `Timed by GridFPV on ${input.timedBy}`
  ].join('\n');
}
