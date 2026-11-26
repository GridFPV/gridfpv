/**
 * Pilot sign-up (docs/primitives.md §11.5), for the screens: which phases have it, what
 * each seat is called, how many runs a pilot has left, and what a pilot can do with a
 * seat right now.
 *
 * The rules themselves are the Director's. What is here only reads the sheet the Director
 * sends, so a button is not offered for something that is plainly not possible. When the
 * Director refuses a change anyway (someone else got there first), its own words are shown.
 */
import type {
  EventView,
  PhaseId,
  PhaseView,
  PilotId,
  SignupHeatView,
  SignupPilotView,
  SignupRules,
  SignupSeat,
  SignupView
} from '@gridfpv/types';
import type { Names } from './names.js';
import type { SeatSource } from './seats.js';

/** A phase pilots sign up for, with its sheet. */
export interface SignupPhase {
  id: PhaseId;
  /** "Qualifying". */
  name: string;
  locked: boolean;
  signup: SignupView;
}

/** Every phase with sign-up switched on, in the order they are in the event. */
export function signupPhases(view: EventView | undefined): SignupPhase[] {
  if (!view) return [];
  return view.phases
    .filter((phase): phase is PhaseView & { signup: SignupView } => phase.signup !== undefined)
    .map((phase) => ({
      id: phase.def.id,
      name: phase.def.label,
      locked: phase.locked,
      signup: phase.signup
    }));
}

/** A phase that could have sign-up: it holds heats (it is not one), and is not locked. */
export function canHaveSignup(phase: PhaseView): boolean {
  return 'Group' in phase.def.kind && !phase.locked;
}

/** One seat of a sign-up phase, as a column heading. */
export interface SeatHeading {
  seat: SignupSeat;
  /** "Seat 1". */
  name: string;
  /** "R1 · 5658 MHz", when the timer has said it. */
  channel?: string;
}

/**
 * The heats a pilot is shown: the ones still open to pilots. A heat that is closed, on or
 * flown cannot be joined or left from a phone, so it only gets in the way. A **full** open
 * heat stays, because a seat in it may come free.
 */
export function openHeats(signup: SignupView): SignupHeatView[] {
  return signup.heats.filter((heat) => heat.state === 'Open');
}

/** The seats of a sign-up phase, named. */
export function seatHeadings(rules: SignupRules, names: Names): SeatHeading[] {
  return rules.seats.map((seat, index) => ({
    seat,
    // The channel is shown in full under it, so the seat is named without one.
    name: names.seatPlain(seat.adapter, seat.seat, index),
    channel: names.channel(seat.adapter, seat.seat)
  }));
}

/** A pilot's runs on the sheet. A pilot who is not on it has used none and may not sign up. */
export function runsOf(signup: SignupView, pilot: PilotId): SignupPilotView | undefined {
  return signup.pilots.find((entry) => entry.pilot === pilot);
}

/**
 * How many more spots a pilot can take before the run limit: the limit, less the runs used,
 * less the spots held. `undefined` when there is no limit.
 */
export function runsLeft(runs: SignupPilotView | undefined): number | undefined {
  if (runs?.allowed === undefined || runs.allowed === null) return undefined;
  return Math.max(0, runs.allowed - runs.used - runs.held);
}

/** A pilot's runs in a sentence: "1 flown · 2 held · 0 left of 3". */
export function runsWords(runs: SignupPilotView | undefined): string {
  if (!runs) return 'You are not in this phase.';
  const parts = [`${runs.used} flown`, `${runs.held} held`];
  const left = runsLeft(runs);
  if (left !== undefined) parts.push(`${left} left of ${runs.allowed}`);
  return parts.join(' · ');
}

/** What a heat's state means to someone signing up. */
export function heatStateWords(heat: SignupHeatView): string | undefined {
  switch (heat.state) {
    case 'Open':
      return heat.full ? 'Full' : undefined;
    case 'Closed':
      return 'Closed';
    case 'Racing':
      return 'On now';
    case 'Flown':
      return 'Flown';
  }
}

/** Which seat of a heat a pilot is on, counted from 0, or -1. */
export function seatOf(heat: SignupHeatView, pilot: PilotId | undefined): number {
  return pilot === undefined ? -1 : heat.seats.findIndex((on) => on === pilot);
}

/** What a pilot can do about one seat of one heat. */
export type SeatAction =
  | { kind: 'take' }
  | { kind: 'leave' }
  /** Theirs, but the heat is closed or has started: nothing to press. */
  | { kind: 'mine' }
  /** Someone else's. */
  | { kind: 'taken' }
  /** Free, but not for this pilot right now. `why` says so. */
  | { kind: 'blocked'; why: string };

/**
 * What the signed-in pilot can do with a seat. This reads the sheet the same way the
 * Director does, so the page offers only what will work. The rest rule is left to the
 * Director: it is off by default, and its refusal says what it needs.
 */
export function seatAction(
  signup: SignupView,
  heat: SignupHeatView,
  seatIndex: number,
  pilot: PilotId
): SeatAction {
  const on = heat.seats[seatIndex];
  const pilotsOpen = heat.state === 'Open';
  if (on === pilot) return pilotsOpen ? { kind: 'leave' } : { kind: 'mine' };
  if (on !== null && on !== undefined) return { kind: 'taken' };
  if (!pilotsOpen) return { kind: 'blocked', why: heatStateWords(heat) ?? 'Closed' };
  if (!signup.rules.open) return { kind: 'blocked', why: 'Sign-up is closed' };
  if (seatOf(heat, pilot) >= 0) return { kind: 'blocked', why: 'You are in this heat' };
  const runs = runsOf(signup, pilot);
  if (!runs) return { kind: 'blocked', why: 'You are not in this phase' };
  const limit = signup.rules.held_limit;
  if (limit !== undefined && limit !== null && runs.held >= limit) {
    return { kind: 'blocked', why: `You hold ${limit}, the most at once` };
  }
  if (runsLeft(runs) === 0) return { kind: 'blocked', why: 'No runs left' };
  return { kind: 'take' };
}

/** Whether "next free" on a seat has somewhere to put the pilot. */
export function hasNextFree(signup: SignupView, seatIndex: number, pilot: PilotId): boolean {
  return signup.heats.some((heat) => seatAction(signup, heat, seatIndex, pilot).kind === 'take');
}

/**
 * The earliest heat, before `heat`, that a pilot could be pulled forward into: the director
 * can still change it, the pilot's seat is free there, and the pilot is not already in it.
 */
export function pullTarget(
  signup: SignupView,
  heat: SignupHeatView,
  seatIndex: number
): SignupHeatView | undefined {
  const pilot = heat.seats[seatIndex];
  if (pilot === null || pilot === undefined) return undefined;
  if (heat.state !== 'Open' && heat.state !== 'Closed') return undefined;
  return signup.heats.find(
    (earlier) =>
      earlier.number < heat.number &&
      (earlier.state === 'Open' || earlier.state === 'Closed') &&
      (earlier.seats[seatIndex] === null || earlier.seats[seatIndex] === undefined) &&
      !earlier.seats.includes(pilot)
  );
}

// ── The rules form ───────────────────────────────────────────────────────────

/** The rules as the form holds them: numbers as typed. */
export interface RulesForm {
  /** The seat source's id. */
  source: string;
  /** Which of the source's seats are in every heat. */
  seats: string[];
  openHeats: string;
  /** Blank is "no limit". */
  heldLimit: string;
  /** Blank is "no limit". */
  runLimit: string;
  rest: string;
  open: boolean;
}

/** How many heats are open at once unless the director says otherwise. */
export const DEFAULT_OPEN_HEATS = 3;

/** The form for a phase: its rules as they are, or the usual start for a new one. */
export function rulesForm(
  rules: SignupRules | undefined,
  sources: readonly SeatSource[]
): RulesForm {
  if (rules) {
    return {
      source: rules.seats[0]?.adapter ?? sources[0]?.id ?? '',
      seats: rules.seats.map((seat) => seat.seat),
      openHeats: String(rules.open_heats),
      heldLimit:
        rules.held_limit === undefined || rules.held_limit === null ? '' : String(rules.held_limit),
      runLimit:
        rules.run_limit === undefined || rules.run_limit === null ? '' : String(rules.run_limit),
      rest: String(rules.rest_heats),
      open: rules.open
    };
  }
  const source = sources.find((entry) => !entry.unavailable) ?? sources[0];
  return {
    source: source?.id ?? '',
    seats: (source?.seats ?? []).map((seat) => seat.seat),
    openHeats: String(DEFAULT_OPEN_HEATS),
    heldLimit: '',
    runLimit: '',
    rest: '0',
    open: false
  };
}

const whole = (text: string): number | undefined =>
  /^\d+$/.test(text.trim()) ? Number(text.trim()) : undefined;

/** What is wrong with the form, in words, or `undefined` when it can be sent. */
export function rulesProblem(form: RulesForm): string | undefined {
  if (form.seats.length === 0) return 'Pick at least one seat.';
  const open = whole(form.openHeats);
  if (open === undefined || open < 1 || open > 20)
    return 'Heats open at once: a number from 1 to 20.';
  for (const [text, what] of [
    [form.heldLimit, 'Spots held at once'],
    [form.runLimit, 'Runs for each pilot']
  ] as const) {
    if (text.trim() === '') continue;
    const value = whole(text);
    if (value === undefined || value < 1) {
      return `${what}: a number from 1 up, or leave it empty for no limit.`;
    }
  }
  const rest = whole(form.rest);
  if (rest === undefined || rest > 20) return 'Heats to sit out: a number from 0 to 20.';
  return undefined;
}

/** The rules the form stands for. Call only when {@link rulesProblem} finds nothing. */
export function rulesFrom(form: RulesForm, sources: readonly SeatSource[]): SignupRules {
  // Keep the source's own seat order, whatever order the boxes were ticked in.
  const order = sources.find((source) => source.id === form.source)?.seats.map((seat) => seat.seat);
  const seats = order ? order.filter((seat) => form.seats.includes(seat)) : form.seats;
  const rules: SignupRules = {
    seats: seats.map((seat) => ({ adapter: form.source, seat })),
    open_heats: whole(form.openHeats) ?? DEFAULT_OPEN_HEATS,
    rest_heats: whole(form.rest) ?? 0,
    open: form.open
  };
  const held = whole(form.heldLimit);
  if (held !== undefined) rules.held_limit = held;
  const runs = whole(form.runLimit);
  if (runs !== undefined) rules.run_limit = runs;
  return rules;
}
