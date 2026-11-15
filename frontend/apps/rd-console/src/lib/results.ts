/**
 * Pure helpers for the Results page: which phases have a result to show, and the numbers in
 * one phase's table.
 *
 * The order of a table is always the Director's ranking. Nothing here ranks anyone. It only
 * picks which columns say something for a phase and reads each pilot's numbers.
 */
import type {
  EventView,
  PhaseId,
  PhaseView,
  PilotId,
  PlacedView,
  RaceId,
  RaceView
} from '@gridfpv/types';
import type { Names } from './names.js';
import { bestLapRun, consecutiveLaps, holeShot, type LapRun } from './overlay.js';
import { isHeat, phaseTree } from './phases.js';

/** A phase the Results page can show, with its place in the tree spelled out. */
export interface ResultChoice {
  id: PhaseId;
  /** "Mains › A-Main". */
  label: string;
  heat: boolean;
}

/** Every phase with a ranking, in tree order. */
export function resultChoices(view: EventView): ResultChoice[] {
  const byId = new Map(view.phases.map((phase) => [phase.def.id, phase]));
  return phaseTree(view.phases)
    .filter(({ phase }) => phase.ranking.length > 0 && phase.format?.ranking !== undefined)
    .map(({ phase }) => {
      const labels: string[] = [];
      let at: PhaseId | undefined = phase.def.id;
      for (let steps = 0; at !== undefined && steps <= view.phases.length; steps += 1) {
        const found = byId.get(at);
        if (!found) break;
        labels.unshift(found.def.label);
        at = found.def.parent;
      }
      return { id: phase.def.id, label: labels.join(' › '), heat: isHeat(phase.def) };
    });
}

/** The heats at or beneath a phase. */
export function heatsUnder(view: EventView, phase: PhaseId): PhaseView[] {
  const under = new Set<PhaseId>([phase]);
  // The snapshot lists a parent before its children, so one pass finds every level.
  for (const candidate of view.phases) {
    if (candidate.def.parent !== undefined && under.has(candidate.def.parent)) {
      under.add(candidate.def.id);
    }
  }
  return view.phases.filter((candidate) => under.has(candidate.def.id) && isHeat(candidate.def));
}

/** The phase to open on: the one raced last that has a ranking, else the first that has one. */
export function defaultResult(
  view: EventView,
  choices: readonly ResultChoice[]
): PhaseId | undefined {
  const lastHeat = view.races.filter((race) => race.state === 'Saved').at(-1)?.heat;
  if (lastHeat !== undefined) {
    // Prefer the phase that holds the heat: it is the fuller table.
    const parent = view.phases.find((phase) => phase.def.id === lastHeat)?.def.parent;
    const found =
      choices.find((choice) => choice.id === parent) ??
      choices.find((choice) => choice.id === lastHeat);
    if (found) return found.id;
  }
  return choices[0]?.id;
}

/** One race, as the Results page names it. */
export interface RaceRef {
  race: RaceId;
  heat: PhaseId;
  /** Which race of its heat it is, 1 first. */
  number: number;
  /** "Track 2 race 3": the heat and the number. */
  name: string;
}

/** One race under a phase, with what the page calls it. */
export interface PhaseRace extends RaceRef {
  view: RaceView;
}

/**
 * Every race of the heats at or beneath a phase, oldest first, each numbered within its own
 * heat. Discarded races are in the list, so the numbers are the ones every other page uses.
 */
export function phaseRaces(view: EventView, phase: PhaseId, names: Names): PhaseRace[] {
  const heats = new Set(heatsUnder(view, phase).map((heat) => heat.def.id));
  const seen = new Map<PhaseId, number>();
  const found: PhaseRace[] = [];
  for (const race of view.races) {
    if (!heats.has(race.heat)) continue;
    const number = (seen.get(race.heat) ?? 0) + 1;
    seen.set(race.heat, number);
    found.push({
      race: race.race,
      heat: race.heat,
      number,
      name: `${names.phase(race.heat)} race ${number}`,
      view: race
    });
  }
  return found;
}

/** A number in a results table, and the race it was flown in. */
export interface Sourced {
  micros: number;
  from: RaceRef;
}

const refOf = ({ race, heat, number, name }: PhaseRace): RaceRef => ({ race, heat, number, name });

/**
 * A pilot's fastest run of `n` laps in a row among saved races, and the race it is in. Two
 * equal runs give the earlier race.
 */
export function bestRunSource(
  races: readonly PhaseRace[],
  pilot: PilotId,
  n: number
): Sourced | undefined {
  let best: Sourced | undefined;
  for (const entry of races) {
    if (entry.view.state !== 'Saved') continue;
    for (const seat of entry.view.seats) {
      if (seat.pilot !== pilot) continue;
      const run = bestLapRun(seat.laps, n);
      if (run && (best === undefined || run.totalMicros < best.micros)) {
        best = { micros: run.totalMicros, from: refOf(entry) };
      }
    }
  }
  return best;
}

/** A pilot's fastest lap among saved races, and the race it is in. */
export function bestLapSource(races: readonly PhaseRace[], pilot: PilotId): Sourced | undefined {
  let best: Sourced | undefined;
  for (const entry of races) {
    if (entry.view.state !== 'Saved') continue;
    for (const seat of entry.view.seats) {
      if (seat.pilot !== pilot) continue;
      for (const lap of seat.laps) {
        if (lap.restart || lap.timer_restarted) continue;
        if (best === undefined || lap.micros < best.micros) {
          best = { micros: lap.micros, from: refOf(entry) };
        }
      }
    }
  }
  return best;
}

/** One row of a phase's results. */
export interface ResultRow {
  position: number;
  callsign: string;
  placed: PlacedView;
  /** The pilot's fastest run of N laps in a row anywhere under the phase, in microseconds. */
  bestRunMicros?: number;
  /** The race that run was flown in. */
  bestRunFrom?: RaceRef;
  /** The race the pilot's best lap was flown in. Absent when no saved race holds that lap. */
  bestLapFrom?: RaceRef;
}

/** A phase's results table: which extra columns it has, and its rows in ranked order. */
export interface ResultTable {
  /** The N of "best N laps in a row", when the phase is ranked on it. */
  runLength?: number;
  showWins: boolean;
  showPoints: boolean;
  rows: ResultRow[];
}

/** The table for one phase. */
export function resultTable(view: EventView, phase: PhaseView, names: Names): ResultTable {
  const runLength = consecutiveLaps(phase.format);
  const races = phaseRaces(view, phase.def.id, names);
  const sort = phase.format?.ranking?.sort ?? [];
  const rows = phase.ranking.map((placed): ResultRow => {
    const run = runLength === undefined ? undefined : bestRunSource(races, placed.pilot, runLength);
    const lap = bestLapSource(races, placed.pilot);
    return {
      position: placed.position,
      callsign: names.pilot(placed.pilot),
      placed,
      bestRunMicros: run?.micros,
      bestRunFrom: run?.from,
      // The Director's best lap is the number shown. The race is named only when it is the
      // one that holds that lap.
      bestLapFrom: lap && lap.micros === placed.best_lap_micros ? lap.from : undefined
    };
  });
  return {
    runLength,
    showWins: sort.includes('Wins'),
    showPoints: sort.includes('Points'),
    rows
  };
}

// ── One race in full ─────────────────────────────────────────────────────────

/** One entry of a pilot's lap list. */
export interface RaceLap {
  /** "HS" for the hole shot, else "L1", "L2", … */
  label: string;
  micros: number;
  /** Part of the pilot's best run of N laps in a row. */
  inBestRun: boolean;
  /** The pilot's fastest lap of the race. */
  best: boolean;
}

/** One pilot's part in a race, in full. */
export interface RaceDetailRow {
  /** Stable key for the list. A wire handle: never shown. */
  key: string;
  /** The callsign, or the seat's name for an open seat. */
  name: string;
  place?: number;
  laps: number;
  bestLapMicros?: number;
  bestRun?: LapRun;
  /** From the start of lap 1 to the last counted crossing. */
  totalMicros?: number;
  falseStart: boolean;
  /** The hole shot, then every counted lap in order. */
  lapList: RaceLap[];
}

/**
 * A race's pilots, in finishing order (seats with no place after, in lineup order), each
 * with every lap. `runLength` is the N of a heat ranked on best N in a row.
 */
export function raceDetail(race: RaceView, names: Names, runLength?: number): RaceDetailRow[] {
  const rows = race.seats.map((seat, index): RaceDetailRow => {
    const counted = seat.laps.filter((lap) => !lap.restart && !lap.timer_restarted);
    const bestRun = runLength === undefined ? undefined : bestLapRun(seat.laps, runLength);
    const fastest = counted.length ? Math.min(...counted.map((lap) => lap.micros)) : undefined;
    const shot = holeShot(seat);
    return {
      key: `${seat.adapter}\u0000${seat.seat}`,
      name:
        seat.pilot === undefined
          ? names.seat(seat.adapter, seat.seat, index)
          : names.pilot(seat.pilot),
      place: seat.finish,
      laps: counted.length,
      bestLapMicros: seat.best_lap_micros ?? fastest,
      bestRun,
      totalMicros: seat.total_micros,
      falseStart: seat.false_start,
      lapList: [
        ...(shot === undefined
          ? []
          : [{ label: 'HS', micros: shot, inBestRun: false, best: false }]),
        ...counted.map((lap, i) => ({
          label: `L${i + 1}`,
          micros: lap.micros,
          inBestRun:
            bestRun !== undefined && i >= bestRun.start && i < bestRun.start + bestRun.length,
          best: lap.micros === fastest
        }))
      ]
    };
  });
  return rows
    .map((row, index) => ({ row, index }))
    .sort(
      (a, b) =>
        (a.row.place ?? Number.POSITIVE_INFINITY) - (b.row.place ?? Number.POSITIVE_INFINITY) ||
        a.index - b.index
    )
    .map(({ row }) => row);
}
