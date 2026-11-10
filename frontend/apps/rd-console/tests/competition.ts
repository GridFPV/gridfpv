/**
 * Test support: small builders for snapshot pieces (`PhaseView`, `RaceView`, `EventView`),
 * so a test states only what it is about.
 */
import type {
  EventView,
  Format,
  PhaseId,
  PhaseView,
  PilotId,
  RaceSeatView,
  RaceView,
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
    proposed_disqualifications: [],
    ...over
  };
}
