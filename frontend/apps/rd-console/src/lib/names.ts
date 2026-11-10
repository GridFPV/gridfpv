/**
 * The one place ids become names.
 *
 * Phase ids, race ids, pilot ids, timer ids and seat handles are wire handles. A person never
 * sees one: a phase is shown by the label its director gave it, a pilot by callsign, a timer
 * by its name and a seat as "Seat 2". Every screen in an
 * event resolves through a {@link Names} built here (the live event store builds it once
 * and hands the same one to all of them), so two screens cannot name the same thing
 * differently.
 */
import type {
  AdapterId,
  PhaseId,
  PhaseView,
  Pilot,
  PilotId,
  SeatRef,
  SeatWhoView,
  TimerView
} from '@gridfpv/types';
import { seatLabel, timerName } from './seats.js';

/** Shown for a pilot who is no longer in the pilot directory. */
export const UNKNOWN_PILOT = 'Unknown pilot';
/** Shown for a phase the event no longer has. */
export const UNKNOWN_PHASE = 'Unknown phase';

export interface Names {
  /** A phase's label, e.g. "Winners Round 1 Heat 2". */
  phase(id: PhaseId): string;
  /** A pilot's callsign. */
  pilot(id: PilotId): string;
  /** Who is on a seat: a callsign, or where the pilot will come from. */
  who(who: SeatWhoView): string;
  /**
   * A seat: "Seat 2". `position` is the seat's place in its lineup, counted from 0, used
   * only when the timer directory cannot say (the timer was removed).
   */
  seat(adapter: AdapterId, seat: SeatRef, position?: number): string;
  /** What times a seat: the timer's name, "Bench", or "Unknown timer". */
  timer(adapter: AdapterId): string;
}

/** "1st", "2nd", "3rd", "4th", "11th", "22nd". */
export function ordinal(n: number): string {
  const lastTwo = n % 100;
  if (lastTwo >= 11 && lastTwo <= 13) return `${n}th`;
  switch (n % 10) {
    case 1:
      return `${n}st`;
    case 2:
      return `${n}nd`;
    case 3:
      return `${n}rd`;
    default:
      return `${n}th`;
  }
}

/** A place in another phase: "Winner of Heat 1", "2nd of Heat 1". */
export function placeOf(position: number, phaseLabel: string): string {
  return position === 1 ? `Winner of ${phaseLabel}` : `${ordinal(position)} of ${phaseLabel}`;
}

/**
 * Build the resolver from the event's phases, the pilot directory and the timer directory.
 *
 * The timer directory holds every timer the Director knows, connected or not, so a heat on a
 * timer that is switched off, or a heat that finished long ago, still names its seats.
 *
 * The labels the bracket and mains presets generate already read in full ("Winners Round 1
 * Heat 2"), so a phase's own label is its whole name; nothing is joined on from its parent.
 */
export function buildNames(
  phases: readonly PhaseView[],
  pilots: readonly Pilot[],
  timers: readonly TimerView[] = []
): Names {
  const phaseLabels = new Map(phases.map((phase) => [phase.def.id, phase.def.label]));
  const callsigns = new Map(pilots.map((pilot) => [pilot.id, pilot.callsign]));

  const phase = (id: PhaseId): string => phaseLabels.get(id) ?? UNKNOWN_PHASE;
  const pilot = (id: PilotId): string => callsigns.get(id) ?? UNKNOWN_PILOT;
  const who = (seat: SeatWhoView): string => {
    if (seat === 'Open') return 'Open seat';
    if (seat === 'Nobody') return 'Empty';
    if ('Pilot' in seat) return pilot(seat.Pilot);
    return placeOf(seat.Waiting.position, phase(seat.Waiting.phase));
  };
  const seat = (adapter: AdapterId, ref: SeatRef, position?: number): string =>
    seatLabel(timers, adapter, ref, position);
  const timer = (adapter: AdapterId): string => timerName(timers, adapter);
  return { phase, pilot, who, seat, timer };
}
