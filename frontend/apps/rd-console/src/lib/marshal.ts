/**
 * Pure helpers for the Marshal page: which races need a look and why, what a race is called,
 * the rows of one seat's lap list, and the times a marshal types or reads. Kept out of the
 * components so they can be tested alone.
 *
 * Nothing here shows an id. A race is named by its heat's label and its number in that heat
 * ("Qualifying Heat 1 · race 2"); pilots, seats and timers go through `lib/names.ts`.
 */
import type {
  EventView,
  LogRef,
  PhaseId,
  PhaseView,
  ProtestOutcome,
  ProtestView,
  RaceId,
  RaceSeatView,
  RaceView,
  RemovalReason,
  SeatSignal,
  SourceTime,
  UncountedWhy
} from '@gridfpv/types';
import type { Names } from './names.js';
import { racesOfHeat } from './phases.js';
import { raceStateLabel } from './run.js';
import { isBench } from './seats.js';

/** A race's number in its heat, counted from 1, discarded races included. 0 if not found. */
export function raceNumber(view: EventView, race: RaceView): number {
  return racesOfHeat(view, race.heat).findIndex((other) => other.race === race.race) + 1;
}

/** What a race is called: "‹heat label› · race N". */
export function raceName(view: EventView, names: Names, race: RaceView): string {
  return `${names.phase(race.heat)} · race ${raceNumber(view, race)}`;
}

/** The open protests against a race. */
export function openProtests(view: EventView, race: RaceId): ProtestView[] {
  return view.protests.filter((p) => p.race === race && p.outcome === undefined);
}

/** One race on the to-do list. */
export interface TodoItem {
  race: RaceId;
  heat: PhaseId;
  /** "‹heat label› · race N". */
  name: string;
  /** Why it is listed, each a short sentence. Never empty. */
  reasons: string[];
  /** The note saved with the race, if any. */
  note?: string;
}

/**
 * The races that need someone to look at them, oldest first: a saved race flagged "needs a
 * look", a race with an open protest, and a stopped race that is not saved or discarded yet.
 * A discarded race does not count toward anything, so a flag on it is not listed.
 */
export function todoList(view: EventView, names: Names): TodoItem[] {
  const items: TodoItem[] = [];
  for (const race of view.races) {
    const reasons: string[] = [];
    if (race.state === 'Stopped') reasons.push('Stopped. Not saved or discarded yet.');
    if (race.state === 'Saved' && race.needs_marshal) reasons.push('Needs a look.');
    const open = openProtests(view, race.race).length;
    if (open === 1) reasons.push('1 open protest.');
    else if (open > 1) reasons.push(`${open} open protests.`);
    if (reasons.length === 0) continue;
    items.push({
      race: race.race,
      heat: race.heat,
      name: raceName(view, names, race),
      reasons,
      note: race.note
    });
  }
  return items;
}

/**
 * The number on the event page's Marshal link: the races that need a look or have an open
 * protest. A stopped race is the Run page's business, so it is not counted here.
 */
export function marshalCount(view: EventView): number {
  return view.races.filter(
    (race) =>
      (race.state === 'Saved' && race.needs_marshal) || openProtests(view, race.race).length > 0
  ).length;
}

/** One heat and its races, for the list every race can be opened from. */
export interface HeatRaces {
  heat: PhaseId;
  label: string;
  races: { race: RaceId; name: string; state: string }[];
}

/**
 * Every race that is not on the to-do list, grouped by heat. Heats are in the event's own
 * order; a heat with no such race is left out.
 */
export function otherRaces(view: EventView, names: Names): HeatRaces[] {
  const listed = new Set(todoList(view, names).map((item) => item.race));
  const groups: HeatRaces[] = [];
  const seen = new Set<PhaseId>();
  const heats = [...view.phases.map((phase) => phase.def.id), ...view.races.map((r) => r.heat)];
  for (const heat of heats) {
    if (seen.has(heat)) continue;
    seen.add(heat);
    const races = racesOfHeat(view, heat)
      .map((race, index) => ({ race, index }))
      .filter(({ race }) => !listed.has(race.race))
      .map(({ race, index }) => ({
        race: race.race,
        name: `Race ${index + 1}`,
        state: raceStateLabel(race)
      }));
    if (races.length > 0) groups.push({ heat, label: names.phase(heat), races });
  }
  return groups;
}

/**
 * The phase whose lock makes a heat locked: the heat itself or the nearest phase above it
 * that carries the lock. `undefined` when nothing above the heat is locked.
 */
export function lockedBy(phases: readonly PhaseView[], heat: PhaseId): PhaseId | undefined {
  const byId = new Map(phases.map((phase) => [phase.def.id, phase]));
  let at: PhaseId | undefined = heat;
  for (let steps = 0; at !== undefined && steps <= phases.length; steps += 1) {
    const phase = byId.get(at);
    if (!phase) return undefined;
    if (phase.locked_here) return phase.def.id;
    at = phase.def.parent;
  }
  return undefined;
}

/** Whether rulings can be made on a race: it has finished flying, counts, and is not locked. */
export function canRule(race: RaceView): boolean {
  return !race.locked && (race.state === 'Stopped' || race.state === 'Saved');
}

/** The removal reasons, in the order the buttons are shown. A label only. */
export const REMOVAL_REASONS: readonly { reason: RemovalReason; label: string }[] = [
  { reason: 'FalseDetection', label: 'False detection' },
  { reason: 'IncorrectLap', label: 'Incorrect lap' },
  { reason: 'Other', label: 'Other' }
];

/** The protest outcomes, in the order the buttons are shown. */
export const PROTEST_OUTCOMES: readonly { outcome: ProtestOutcome; label: string }[] = [
  { outcome: 'Upheld', label: 'Upheld' },
  { outcome: 'Denied', label: 'Denied' },
  { outcome: 'Withdrawn', label: 'Withdrawn' }
];

/** The word for a protest's state. */
export function protestStateLabel(protest: ProtestView): string {
  if (protest.outcome === undefined) return 'Open';
  return PROTEST_OUTCOMES.find((entry) => entry.outcome === protest.outcome)?.label ?? 'Resolved';
}

/** Why a crossing does not count, as a sentence. */
export function uncountedLabel(why: UncountedWhy, reason?: RemovalReason): string {
  switch (why) {
    case 'Removed': {
      const label = REMOVAL_REASONS.find((entry) => entry.reason === reason)?.label;
      return label ? `Removed by a marshal: ${label.toLowerCase()}.` : 'Removed by a marshal.';
    }
    case 'UnderMinLap':
      return 'Too soon after the crossing before it (under the minimum lap time).';
    case 'BeforeStart':
      return 'Before the start.';
  }
}

/** One row of a seat's lap list: the hole shot, a lap, or a marked restart. */
export interface LapRow {
  kind: 'holeshot' | 'lap' | 'restart';
  /** The crossing a ruling about this row names. */
  crossing: LogRef;
  /** When the crossing was made, on the timer's clock. */
  at: SourceTime;
  /** 1 for the first counted lap. Only a lap has one. */
  number?: number;
  /** The lap time. Only a lap has one: a restart has no time. */
  micros?: number;
  /** Whether this is the seat's best lap. */
  best: boolean;
}

/**
 * A seat's rows in the order they were flown. Laps are numbered 1, 2, 3; a marked restart
 * keeps its place in the list but takes no number and has no time.
 */
export function lapRows(seat: RaceSeatView): LapRow[] {
  const rows: LapRow[] = [];
  if (seat.hole_shot) {
    rows.push({
      kind: 'holeshot',
      crossing: seat.hole_shot.crossing,
      at: seat.hole_shot.at,
      best: false
    });
  }
  let number = 0;
  let bestMarked = false;
  for (const lap of seat.laps) {
    if (lap.restart) {
      rows.push({ kind: 'restart', crossing: lap.crossing, at: lap.ended_at, best: false });
      continue;
    }
    number += 1;
    const best = !bestMarked && lap.micros === seat.best_lap_micros;
    if (best) bestMarked = true;
    rows.push({
      kind: 'lap',
      crossing: lap.crossing,
      at: lap.ended_at,
      number,
      micros: lap.micros,
      best
    });
  }
  return rows;
}

/** How many laps of a seat count. */
export function countedLaps(seat: RaceSeatView): number {
  return seat.laps.filter((lap) => !lap.restart).length;
}

/** The moment a seat's times are read from, and what to call it. */
export interface TimeBase {
  /** On the timer's clock, in microseconds. */
  at: SourceTime;
  /** "into the race" when it is the start, "after the first crossing" when it is not known. */
  words: 'into the race' | 'after the first crossing';
}

/**
 * Where a seat's times are counted from. The race's start on the seat's timer, when the
 * Director measured that timer's clock. A bench seat is stamped with the Director's own
 * clock, so the race's start time is used as it is. Failing both, the seat's first crossing.
 * `undefined` when the seat has no crossing at all and no known start.
 */
export function timeBase(
  race: RaceView,
  seat: RaceSeatView,
  signal?: SeatSignal
): TimeBase | undefined {
  if (signal?.start_at !== undefined) return { at: signal.start_at, words: 'into the race' };
  if (isBench(seat.adapter) && race.started_at !== undefined) {
    return { at: race.started_at, words: 'into the race' };
  }
  const times = [
    ...(seat.hole_shot ? [seat.hole_shot.at] : []),
    ...seat.laps.map((lap) => lap.started_at),
    ...seat.uncounted.map((crossing) => crossing.at)
  ];
  if (times.length === 0) return undefined;
  return { at: Math.min(...times), words: 'after the first crossing' };
}

/** A time on the timer's clock as seconds from the base, to the millisecond: "12.345". */
export function formatOffset(at: SourceTime, base: SourceTime): string {
  const micros = at - base;
  const sign = micros < 0 ? '-' : '';
  const totalMs = Math.round(Math.abs(micros) / 1000);
  return `${sign}${Math.floor(totalMs / 1000)}.${String(totalMs % 1000).padStart(3, '0')}`;
}

/**
 * Read what a marshal typed as seconds from the base and give the time on the timer's clock,
 * in whole microseconds. `undefined` for anything that is not a number of seconds at or
 * after zero.
 */
export function parseOffset(text: string, base: SourceTime): SourceTime | undefined {
  const trimmed = text.trim();
  if (!/^\d+([.,]\d+)?$/.test(trimmed)) return undefined;
  const seconds = Number(trimmed.replace(',', '.'));
  if (!Number.isFinite(seconds)) return undefined;
  return base + Math.round(seconds * 1_000_000);
}

/** The signal for one seat of a race, matched by timer and seat. */
export function signalForSeat(
  signals: readonly SeatSignal[] | undefined,
  seat: RaceSeatView
): SeatSignal | undefined {
  return signals?.find((s) => s.adapter === seat.adapter && s.seat === seat.seat);
}

/** A mark on a seat's trace: the start, or one crossing. */
export interface TraceMark {
  kind: 'start' | 'holeshot' | 'lap' | 'restart' | 'uncounted';
  /** On the timer's clock. */
  at: SourceTime;
  /** The crossing behind it. The start has none. */
  crossing?: LogRef;
  /** What is written at the mark: "Start", "HS", a lap number, "R", "×". */
  text: string;
}

/** Everything to mark on a seat's trace, in time order. */
export function traceMarks(seat: RaceSeatView, startAt?: SourceTime): TraceMark[] {
  const marks: TraceMark[] = [];
  if (startAt !== undefined) marks.push({ kind: 'start', at: startAt, text: 'Start' });
  for (const row of lapRows(seat)) {
    marks.push({
      kind: row.kind,
      at: row.at,
      crossing: row.crossing,
      text: row.kind === 'holeshot' ? 'HS' : row.kind === 'restart' ? 'R' : String(row.number)
    });
  }
  for (const crossing of seat.uncounted) {
    marks.push({ kind: 'uncounted', at: crossing.at, crossing: crossing.crossing, text: '×' });
  }
  return marks.sort((a, b) => a.at - b.at);
}
