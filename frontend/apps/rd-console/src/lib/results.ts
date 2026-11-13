/**
 * Pure helpers for the Results page: which phases have a result to show, and the numbers in
 * one phase's table.
 *
 * The order of a table is always the Director's ranking. Nothing here ranks anyone. It only
 * picks which columns say something for a phase and reads each pilot's numbers.
 */
import type { EventView, PhaseId, PhaseView, PlacedView } from '@gridfpv/types';
import type { Names } from './names.js';
import { bestRunInHeat, consecutiveLaps } from './overlay.js';
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

/** One row of a phase's results. */
export interface ResultRow {
  position: number;
  callsign: string;
  placed: PlacedView;
  /** The pilot's fastest run of N laps in a row anywhere under the phase, in microseconds. */
  bestRunMicros?: number;
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
  const heats = runLength === undefined ? [] : heatsUnder(view, phase.def.id);
  const sort = phase.format?.ranking?.sort ?? [];
  const rows = phase.ranking.map((placed): ResultRow => {
    let best: number | undefined;
    for (const heat of heats) {
      const run = bestRunInHeat(view, heat.def.id, placed.pilot, runLength ?? 0);
      if (run !== undefined && (best === undefined || run < best)) best = run;
    }
    return {
      position: placed.position,
      callsign: names.pilot(placed.pilot),
      placed,
      bestRunMicros: best
    };
  });
  return {
    runLength,
    showWins: sort.includes('Wins'),
    showPoints: sort.includes('Points'),
    rows
  };
}
