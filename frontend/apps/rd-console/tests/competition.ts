/**
 * Test support: small builders for snapshot pieces (`PhaseView`, `RaceView`, `EventView`),
 * so a test states only what it is about.
 */
import type {
  EventView,
  Format,
  LapView,
  PhaseId,
  PhaseView,
  PilotId,
  PlacedView,
  RaceSeatView,
  RaceView,
  SavedFormat,
  SeatWhoView,
  TimerStatus,
  TimerView
} from '@gridfpv/types';

export const FIRST_TO_3: Format = {
  name: 'First to 3 laps',
  race: { end: { FirstToLaps: 3 }, staging_micros: 5_000_000, first_lap: 'HoleShot' },
  completion: { SavedRaces: 1 },
  ranking: { sort: ['LastFinish'] }
};

/** An unranked format the director ends by hand: practice. */
export const OPEN_PRACTICE: Format = {
  name: 'Open practice',
  race: { end: 'Open', staging_micros: 0, first_lap: 'HoleShot' },
  completion: 'DirectorDecides'
};

/** A timed, ranked format: two minutes, best three laps in a row. */
export const TWO_MINUTE_QUALI: Format = {
  name: '2 minute qualifying',
  race: {
    end: { Timed: { micros: 120_000_000, grace_micros: 10_000_000 } },
    staging_micros: 5_000_000,
    first_lap: 'HoleShot'
  },
  completion: { SavedRaces: 3 },
  ranking: { sort: [{ BestConsecutive: 3 }] }
};

/**
 * The saved formats the screen tests read. The ids are made to look like the Director's, so a
 * leak of one is easy to spot. Practice is first on purpose: a picker that just takes the
 * first format must not pass a test that expects a ranked one.
 */
export const SAVED_FORMATS: SavedFormat[] = [
  { id: 'fmt-practice-9k2d', format: OPEN_PRACTICE },
  { id: 'fmt-first3-7q1x', format: FIRST_TO_3 },
  { id: 'fmt-quali-4m8z', format: TWO_MINUTE_QUALI }
];

const base = {
  locked: false,
  locked_here: false,
  waiting: false,
  complete: false,
  director_done: false,
  field: [],
  lineup: [],
  ranking: []
};

/** A phase that holds phases. */
export function group(
  id: PhaseId,
  label: string,
  parent?: PhaseId,
  over: Partial<PhaseView> = {}
): PhaseView {
  return {
    ...base,
    def: { id, label, parent, kind: { Group: { combine: 'Stack' } } },
    ...over
  };
}

/** A heat. Each entry of `seats` is a pilot id, or a `SeatWhoView` for anything else. */
export function heat(
  id: PhaseId,
  label: string,
  seats: (PilotId | SeatWhoView)[],
  over: Partial<PhaseView> & { parent?: PhaseId } = {}
): PhaseView {
  const { parent, ...rest } = over;
  const who = (seat: PilotId | SeatWhoView): SeatWhoView =>
    seat === 'Open' || seat === 'Nobody' || typeof seat === 'object' ? seat : { Pilot: seat };
  return {
    ...base,
    def: { id, label, parent, kind: { Heat: { lineup: [] } } },
    format: FIRST_TO_3,
    field: seats.filter(
      (seat): seat is PilotId => typeof seat === 'string' && seat !== 'Open' && seat !== 'Nobody'
    ),
    lineup: seats.map((seat, index) => ({
      adapter: 'bench',
      seat: `seat-${index + 1}`,
      who: who(seat)
    })),
    ...rest
  };
}

/** One seat of a race, with no laps. */
export function raceSeat(
  index: number,
  pilot?: PilotId,
  over: Partial<RaceSeatView> = {}
): RaceSeatView {
  return {
    adapter: 'bench',
    seat: `seat-${index + 1}`,
    pilot,
    false_start: false,
    laps: [],
    uncounted: [],
    ...over
  };
}

/** One counted lap of `seconds` seconds. The crossing number is its handle in the log. */
export function lap(crossing: number, seconds: number, over: Partial<LapView> = {}): LapView {
  return {
    crossing,
    started_at: 0,
    ended_at: seconds * 1_000_000,
    micros: seconds * 1_000_000,
    restart: false,
    timer_restarted: false,
    ...over
  };
}

/** One pilot's place in a ranking. */
export function placed(
  pilot: PilotId,
  position: number,
  over: Partial<PlacedView> = {}
): PlacedView {
  return {
    pilot,
    position,
    disqualified: false,
    position_set: false,
    flew: true,
    wins: 0,
    points: 0,
    laps: 0,
    ...over
  };
}

export function race(
  heatId: PhaseId,
  number: number,
  state: RaceView['state'],
  over: Partial<RaceView> = {}
): RaceView {
  return {
    race: `${heatId}#${number}`,
    heat: heatId,
    state,
    owner: 'Grid',
    needs_marshal: false,
    locked: false,
    seats: [],
    ...over
  };
}

/**
 * A timer from the directory. Connected with `seats` seats unless `status` says otherwise.
 * The id is made to look like the Director's (`slug-tail`), so a leak of it is easy to spot.
 */
export function timerView(
  id: string,
  name: string,
  seats = 4,
  status: Partial<TimerStatus> = {}
): TimerView {
  return {
    timer: { id, name, kind: 'Oft', address: '192.168.1.50:7270' },
    status: {
      connected: true,
      seats: Array.from({ length: seats }, (_, index) => String(index)),
      ...status
    }
  };
}

export function eventView(over: Partial<EventView> = {}): EventView {
  return {
    cursor: 1,
    now: 0,
    rosters: [],
    phases: [],
    races: [],
    protests: [],
    queue: [],
    proposed_disqualifications: [],
    lap_rules: {},
    ...over
  };
}
