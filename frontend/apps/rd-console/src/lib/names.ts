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
  Class,
  ClassId,
  PhaseId,
  PhaseView,
  Pilot,
  PilotId,
  SeatChannel,
  SeatRef,
  SeatWhoView,
  TimerView
} from '@gridfpv/types';
import { channelLabel, formatMhz, frequencyLabel } from './channels.js';
import { seatLabel, timerName, isBench } from './seats.js';

/** Shown for a pilot who is no longer in the pilot directory. */
export const UNKNOWN_PILOT = 'Unknown pilot';
/** Shown for a class that is no longer in the class directory. */
export const UNKNOWN_CLASS = 'Unknown class';
/** Shown for a phase the event no longer has. */
export const UNKNOWN_PHASE = 'Unknown phase';

export interface Names {
  /** A phase's label, e.g. "Winners Round 1 Heat 2". */
  phase(id: PhaseId): string;
  /** A pilot's callsign. */
  pilot(id: PilotId): string;
  /** A class's name, e.g. "Open". */
  klass(id: ClassId): string;
  /** Who is on a seat: a callsign, or where the pilot will come from. */
  who(who: SeatWhoView): string;
  /**
   * A seat, with the video channel it is on when the timer has said it: "Seat 2 · R2".
   * The channel is what a pilot needs, so it goes wherever the seat is named. `position`
   * is the seat's place in its lineup, counted from 0, used only when the timer directory
   * cannot say (the timer was removed).
   */
  seat(adapter: AdapterId, seat: SeatRef, position?: number): string;
  /** A seat with no channel: "Seat 2". For a place that shows the channel in full beside it. */
  seatPlain(adapter: AdapterId, seat: SeatRef, position?: number): string;
  /** A seat's channel in full: "R2 · 5695 MHz". `undefined` until the timer has said it. */
  channel(adapter: AdapterId, seat: SeatRef): string | undefined;
  /** What times a seat: the timer's name, "Bench", or "Unknown timer". */
  timer(adapter: AdapterId): string;
  /** The pilot's own colour (`#RRGGBB`), when they have one. For a dot beside the name. */
  pilotColor(id: PilotId): string | undefined;
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
 * Build the resolver from the event's phases and the pilot, timer and class directories.
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
  timers: readonly TimerView[] = [],
  classes: readonly Class[] = [],
  channels: readonly SeatChannel[] = []
): Names {
  const phaseLabels = new Map(phases.map((phase) => [phase.def.id, phase.def.label]));
  const callsigns = new Map(pilots.map((pilot) => [pilot.id, pilot.callsign]));

  const phase = (id: PhaseId): string => phaseLabels.get(id) ?? UNKNOWN_PHASE;
  const classNames = new Map(classes.map((c) => [c.id, c.name]));
  const pilot = (id: PilotId): string => callsigns.get(id) ?? UNKNOWN_PILOT;
  const klass = (id: ClassId): string => classNames.get(id) ?? UNKNOWN_CLASS;
  const who = (seat: SeatWhoView): string => {
    if (seat === 'Open') return 'Open seat';
    if (seat === 'Nobody') return 'Empty';
    if ('Pilot' in seat) return pilot(seat.Pilot);
    return placeOf(seat.Waiting.position, phase(seat.Waiting.phase));
  };
  const frequency = new Map(
    channels.map((entry) => [`${entry.adapter}\u0000${entry.seat}`, entry.freq_mhz])
  );
  // A heat can have seats on more than one timer: four one-seat gates, say. "Seat 1" would
  // then name every one of them. A timer that shares a lineup with another timer has its
  // seats named by the timer instead, which is also what the director sees on the gate.
  const shared = new Set<AdapterId>();
  for (const entry of phases) {
    // A lineup lists only the seats that are filled. A sign-up phase's rules list every
    // seat its heats have, so a gate with nobody on it yet is still known to be shared.
    for (const seats of [entry.lineup, entry.signup?.rules.seats ?? []]) {
      const used = new Set(seats.map((seat) => seat.adapter));
      if (used.size > 1) for (const adapter of used) shared.add(adapter);
    }
  }
  const seatPlain = (adapter: AdapterId, ref: SeatRef, position?: number): string => {
    const plain = seatLabel(timers, adapter, ref, position);
    const known = timers.find((candidate) => candidate.timer.id === adapter);
    if (!shared.has(adapter) || isBench(adapter) || !known) return plain;
    // A timer with one seat is that seat: its name says it all.
    return known.status.seats.length > 1 ? `${known.timer.name} ${plain}` : known.timer.name;
  };
  const seat = (adapter: AdapterId, ref: SeatRef, position?: number): string => {
    const mhz = frequency.get(`${adapter}\u0000${ref}`);
    const plain = seatPlain(adapter, ref, position);
    // "R2" when the frequency is a named channel, else the frequency itself.
    return mhz === undefined ? plain : `${plain} · ${channelLabel(mhz) ?? formatMhz(mhz)}`;
  };
  const channel = (adapter: AdapterId, ref: SeatRef): string | undefined => {
    const mhz = frequency.get(`${adapter}\u0000${ref}`);
    return mhz === undefined ? undefined : frequencyLabel(mhz);
  };
  const timer = (adapter: AdapterId): string => timerName(timers, adapter);
  const colors = new Map(pilots.map((p) => [p.id, p.color]));
  const pilotColor = (id: PilotId): string | undefined => colors.get(id);
  return { phase, pilot, klass, who, seat, seatPlain, channel, timer, pilotColor };
}
