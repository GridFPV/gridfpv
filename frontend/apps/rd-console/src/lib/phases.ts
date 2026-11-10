/**
 * Pure helpers over the event snapshot: the phase tree, what state a phase is in, and the
 * few questions the Event page and the Run view both ask about a heat and its races.
 *
 * Nothing here formats a name. Names come from `lib/names.ts`.
 */
import type { EventView, PhaseDef, PhaseId, PhaseView, PilotId, RaceView } from '@gridfpv/types';

/** Whether a phase is a heat (it has a lineup) rather than a holder of phases. */
export function isHeat(def: PhaseDef): boolean {
  return 'Heat' in def.kind;
}

/** One row of the phase tree: a phase and how deep it sits. */
export interface PhaseRow {
  phase: PhaseView;
  /** 0 for a phase at the top of the event. */
  depth: number;
}

/**
 * Lay the phases out as a tree, each parent followed by its children. The snapshot lists
 * phases in the order they were added, and that order is kept among siblings. A phase whose
 * parent is missing is shown at the top, so nothing is ever hidden.
 */
export function phaseTree(phases: readonly PhaseView[]): PhaseRow[] {
  const ids = new Set(phases.map((phase) => phase.def.id));
  const children = new Map<PhaseId | undefined, PhaseView[]>();
  for (const phase of phases) {
    const parent =
      phase.def.parent !== undefined && ids.has(phase.def.parent) ? phase.def.parent : undefined;
    children.set(parent, [...(children.get(parent) ?? []), phase]);
  }
  const rows: PhaseRow[] = [];
  const seen = new Set<PhaseId>();
  const walk = (parent: PhaseId | undefined, depth: number): void => {
    for (const phase of children.get(parent) ?? []) {
      // The Director refuses a parent loop; this guard only keeps a bad snapshot from hanging.
      if (seen.has(phase.def.id)) continue;
      seen.add(phase.def.id);
      rows.push({ phase, depth });
      walk(phase.def.id, depth + 1);
    }
  };
  walk(undefined, 0);
  return rows;
}

/** Where a phase stands, as the Event page shows it. */
export type PhaseStatus = 'waiting' | 'ready' | 'racing' | 'complete' | 'locked';

/** The words for each status. */
export const PHASE_STATUS_LABELS: Record<PhaseStatus, string> = {
  waiting: 'Waiting',
  ready: 'Ready',
  racing: 'Racing',
  complete: 'Complete',
  locked: 'Locked'
};

/** Whether `phase` is `ancestor` or sits somewhere beneath it. */
function isWithin(phases: readonly PhaseView[], phase: PhaseId, ancestor: PhaseId): boolean {
  const parents = new Map(phases.map((p) => [p.def.id, p.def.parent]));
  let at: PhaseId | undefined = phase;
  // Bounded by the number of phases, so a loop in a bad snapshot cannot hang the page.
  for (let steps = 0; at !== undefined && steps <= phases.length; steps += 1) {
    if (at === ancestor) return true;
    at = parents.get(at);
  }
  return false;
}

/** The race that is staging, running, or stopped and not yet saved or discarded. */
export function currentRace(view: EventView): RaceView | undefined {
  if (view.current_race === undefined) return undefined;
  return view.races.find((race) => race.race === view.current_race);
}

/**
 * A phase's status. "Racing" wins while the undecided race is in the phase (or beneath it),
 * because that is what the director needs to see first.
 */
export function phaseStatus(phase: PhaseView, view: EventView): PhaseStatus {
  const racing = currentRace(view);
  if (racing && isWithin(view.phases, racing.heat, phase.def.id)) return 'racing';
  if (phase.locked) return 'locked';
  if (phase.complete) return 'complete';
  if (phase.waiting) return 'waiting';
  return 'ready';
}

/** A heat's races, oldest first. */
export function racesOfHeat(view: EventView, heat: PhaseId): RaceView[] {
  return view.races.filter((race) => race.heat === heat);
}

/** The pilots named on a heat's seats, in seat order. */
export function pilotsInLineup(phase: PhaseView): PilotId[] {
  return phase.lineup.flatMap((seat) =>
    typeof seat.who === 'object' && 'Pilot' in seat.who ? [seat.who.Pilot] : []
  );
}

/**
 * Whether a heat is a bye: one named pilot and nobody else to fly against. The director
 * can move that pilot on without a race.
 */
export function isBye(phase: PhaseView): boolean {
  const flying = phase.lineup.filter((seat) => seat.who !== 'Nobody');
  return flying.length === 1 && pilotsInLineup(phase).length === 1;
}

/** Lower-case words joined by dashes, safe to use inside a phase id. */
export function slugify(name: string): string {
  return (
    name
      .normalize('NFKD')
      // Drop the accents NFKD split off, so "É" becomes "e" rather than a dash.
      .replace(/[\u0300-\u036f]/g, '')
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '-')
      .replace(/^-+|-+$/g, '')
      .slice(0, 40)
  );
}

/**
 * Make an id for a new phase: a slug of its name plus a short random suffix, so two phases
 * with the same name still get different ids. The id is a wire handle and is never shown.
 */
export function newPhaseId(name: string, random: () => number = Math.random): PhaseId {
  const suffix = Math.floor(random() * 36 ** 4)
    .toString(36)
    .padStart(4, '0');
  const slug = slugify(name);
  return slug ? `${slug}-${suffix}` : `phase-${suffix}`;
}
