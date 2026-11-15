/**
 * Pure helpers for the rulings made on the event page: the words of a proposed
 * disqualification, the positions a pilot can be set to in a heat, and the run queue's
 * reordering. Names come from `lib/names.ts`.
 */
import type {
  ClassId,
  EventView,
  PhaseId,
  PhaseView,
  PilotId,
  ProposedDisqualificationView,
  RosterView
} from '@gridfpv/types';
import type { Names } from './names.js';
import { isHeat } from './phases.js';

/**
 * A proposed disqualification, as the notice says it: "Ace has 2 false starts in Finals. The
 * limit is 2." Grid only proposes; the director decides.
 */
export function proposalSentence(names: Names, proposal: ProposedDisqualificationView): string {
  const starts = proposal.false_starts === 1 ? 'false start' : 'false starts';
  return `${names.pilot(proposal.pilot)} has ${proposal.false_starts} ${starts} in ${names.phase(proposal.phase)}. The limit is ${proposal.limit}.`;
}

/**
 * The proposals still to decide: one per pilot and phase, leaving out a pilot who is already
 * disqualified there.
 */
export function openProposals(view: EventView): ProposedDisqualificationView[] {
  return view.proposed_disqualifications.filter((proposal) => {
    const phase = view.phases.find((p) => p.def.id === proposal.phase);
    const placed = phase?.ranking.find((entry) => entry.pilot === proposal.pilot);
    return !placed?.disqualified;
  });
}

/** The positions a pilot can be set to in a heat: 1 to the size of its ranking. */
export function positionChoices(phase: PhaseView): number[] {
  return Array.from({ length: phase.ranking.length }, (_, index) => index + 1);
}

/** Whether a phase's ranking is worth showing: someone flew, or a ruling changed it. */
export function hasRanking(phase: PhaseView): boolean {
  return phase.ranking.some((placed) => placed.flew || placed.disqualified || placed.position_set);
}

/**
 * The queue with the heat at `index` moved one place up (`-1`) or down (`+1`). The same
 * list when it cannot move. The whole new order is what the Director is sent.
 */
export function moveInQueue(queue: readonly PhaseId[], index: number, step: -1 | 1): PhaseId[] {
  const to = index + step;
  const next = [...queue];
  if (index < 0 || index >= next.length || to < 0 || to >= next.length) return next;
  [next[index], next[to]] = [next[to], next[index]];
  return next;
}

/** One row of a class's standings, in words. */
export interface StandingRow {
  position: number;
  callsign: string;
  /** The label of the phase that placed the pilot, or `undefined` when none has yet. */
  placedBy?: string;
}

/** A class's standings with every id turned into a name. */
export function standingRows(roster: RosterView, names: Names): StandingRow[] {
  return roster.standings.map((place) => ({
    position: place.position,
    callsign: names.pilot(place.pilot),
    placedBy: place.placed_by === undefined ? undefined : names.phase(place.placed_by)
  }));
}

/**
 * The phases a class's standings can count: every phase that is ranked. A heat under a
 * format with no ranking (practice) never counts, so it is not offered.
 */
export function countablePhases(phases: readonly PhaseView[]): PhaseView[] {
  return phases.filter((phase) => !isHeat(phase.def) || phase.format?.ranking !== undefined);
}

/** The classes of the directory that are not in the event yet and are not hidden. */
export function classesToAdd<C extends { id: ClassId; hidden?: boolean }>(
  classes: readonly C[],
  rosters: readonly RosterView[]
): C[] {
  const inEvent = new Set(rosters.map((roster) => roster.class));
  return classes.filter((c) => !inEvent.has(c.id) && !c.hidden);
}

/** A roster cut to at most `max` pilots, for prefilling a picker that has a limit. */
export function rosterPrefill(roster: RosterView, max?: number): PilotId[] {
  return max === undefined ? [...roster.pilots] : roster.pilots.slice(0, max);
}
