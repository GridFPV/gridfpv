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

/** The display hints a bracket carries. Wire values, never shown. */
const BRACKET_HINTS = new Set(['single-elimination', 'double-elimination']);
/** The display hint a set of mains carries. The Director keeps them best first (A-Main). */
const MAINS_HINT = 'mains';

/**
 * A bracket's rounds in the order they are flown.
 *
 * A round can be flown once every round that feeds it has been. So each round gets a
 * level: 0 when nothing in the bracket feeds it, else one more than the latest round that
 * does. Rounds are laid out level by level. Two rounds on one level could be flown in
 * either order; the losers round goes first, so the list reads a winners round, then the
 * losers rounds it feeds, then the next winners round (Ryan, 2026-11-18). The Director
 * keeps rounds latest first with the losers side ahead of the winners side, so keeping
 * that order within a level is what puts losers first.
 */
function flyingOrder(
  bracket: PhaseId,
  rounds: readonly PhaseView[],
  phases: readonly PhaseView[]
): PhaseView[] {
  const parents = new Map(phases.map((phase) => [phase.def.id, phase.def.parent]));
  /** The round of this bracket that a phase is, or sits inside. */
  const roundOf = (id: PhaseId): PhaseId | undefined => {
    let at: PhaseId | undefined = id;
    for (let steps = 0; at !== undefined && steps <= phases.length; steps += 1) {
      const parent = parents.get(at);
      if (parent === bracket) return at;
      at = parent;
    }
    return undefined;
  };
  // Which rounds feed each round: the rounds its heats' seats are filled from.
  const feeds = new Map<PhaseId, Set<PhaseId>>();
  for (const phase of phases) {
    if (!('Heat' in phase.def.kind)) continue;
    const into = roundOf(phase.def.id);
    if (into === undefined) continue;
    for (const seat of phase.def.kind.Heat.lineup) {
      if (typeof seat.source !== 'object' || !('FromPhase' in seat.source)) continue;
      const from = roundOf(seat.source.FromPhase.phase);
      if (from === undefined || from === into) continue;
      feeds.set(into, (feeds.get(into) ?? new Set()).add(from));
    }
  }
  const levels = new Map<PhaseId, number>();
  const levelOf = (round: PhaseId, trail: readonly PhaseId[] = []): number => {
    const known = levels.get(round);
    if (known !== undefined) return known;
    // A loop cannot be made by the generator; this only keeps a bad snapshot from hanging.
    if (trail.includes(round)) return 0;
    const level = Math.max(
      0,
      ...[...(feeds.get(round) ?? [])].map((from) => levelOf(from, [...trail, round]) + 1)
    );
    levels.set(round, level);
    return level;
  };
  // A stable sort: rounds on one level keep the order the Director gave them.
  return [...rounds].sort((a, b) => levelOf(a.def.id) - levelOf(b.def.id));
}

/**
 * How many mains a field takes, when each starts `perMain` pilots and `bump` of them come
 * up from the main below. The Director works it out the same way (`mains_needed`).
 */
export function mainsNeeded(pilots: number, perMain: number, bump: number): number {
  if (pilots <= perMain) return 1;
  const direct = Math.max(1, perMain - bump);
  return 1 + Math.ceil((pilots - perMain) / direct);
}

/** The name the Director gives a main, best first: "A-Main", "B-Main", then "Main 27". */
export function mainName(index: number): string {
  return index < 26 ? `${String.fromCharCode(65 + index)}-Main` : `Main ${index + 1}`;
}

/**
 * Lay the phases out as a tree, each parent followed by its children. The snapshot lists
 * phases in the order they were added, and that order is kept among siblings. A phase whose
 * parent is missing is shown at the top, so nothing is ever hidden.
 *
 * A bracket and a set of mains are the exceptions to the order. The Director keeps a
 * bracket's rounds latest first and the mains best first (A-Main, then B-Main), because that
 * is the order the ranking stacks them in. A person reads them in the order they are flown,
 * so a bracket's rounds are laid out as they can be flown (see `flyingOrder`) and mains
 * bottom main first.
 */
export function phaseTree(phases: readonly PhaseView[], root?: PhaseId): PhaseRow[] {
  if (root !== undefined) {
    // One phase and everything beneath it, with that phase at the top.
    const all = phaseTree(phases);
    const at = all.findIndex((row) => row.phase.def.id === root);
    if (at < 0) return [];
    const base = all[at].depth;
    const rows = [{ phase: all[at].phase, depth: 0 }];
    for (const row of all.slice(at + 1)) {
      if (row.depth <= base) break;
      rows.push({ phase: row.phase, depth: row.depth - base });
    }
    return rows;
  }
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
  for (const phase of phases) {
    const inside = children.get(phase.def.id);
    if (inside === undefined || phase.def.display_hint === undefined) continue;
    if (phase.def.display_hint === MAINS_HINT) inside.reverse();
    else if (BRACKET_HINTS.has(phase.def.display_hint)) {
      children.set(phase.def.id, flyingOrder(phase.def.id, inside, phases));
    }
  }
  walk(undefined, 0);
  return rows;
}

/**
 * Whether a generator made this phase and keeps every heat inside its rounds: it carries a
 * display hint, holds phases, and has no heat directly under it. A phase the director made
 * by hand never counts, whatever it holds.
 */
export function holdsOnlyRounds(phase: PhaseView, phases: readonly PhaseView[]): boolean {
  if (phase.def.display_hint === undefined) return false;
  const inside = phases.filter((other) => other.def.parent === phase.def.id);
  return inside.length > 0 && inside.every((other) => !isHeat(other.def));
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
  if (isHeat(phase.def)) return phase.waiting ? 'waiting' : 'ready';
  // For a phase that holds things, the Director's "waiting" means its list of pilots is
  // not settled yet, which is true of any finals until the last semi-final is flown. What
  // the director asks is whether there is something to fly: one heat that can be, anywhere
  // beneath it, makes it ready.
  const heats = view.phases.filter(
    (other) =>
      isHeat(other.def) &&
      other.def.id !== phase.def.id &&
      isWithin(view.phases, other.def.id, phase.def.id)
  );
  if (heats.length === 0) return phase.waiting ? 'waiting' : 'ready';
  return heats.some((heat) => !heat.waiting && !heat.complete && !heat.locked)
    ? 'ready'
    : 'waiting';
}

/** One phase that holds heats or phases, as the Phases page lists it: one line each. */
export interface PhaseSummary {
  id: PhaseId;
  name: string;
  /** 0 for a phase at the top of the event, 1 for a phase inside it, and so on. */
  depth: number;
  /** The phase that holds it, when one does. A wire handle, never shown. */
  parent?: PhaseId;
  /** How many phases sit directly inside it (a bracket's rounds). Heats are not counted. */
  inside: number;
  /**
   * Whether a generator made it and keeps every heat inside its rounds: a bracket, a set
   * of mains, a round robin. Such a phase has no heats of its own, and a loose heat beside
   * its rounds would make no sense, so the pages offer no "+ Heat" on it.
   */
  roundsOnly: boolean;
  status: PhaseStatus;
  /** The format's name, when the phase sets one. */
  format?: string;
  /** How many heats are beneath it, at any depth. */
  heats: number;
  /** How many of them are done: complete, or with a race that was saved. */
  flown: number;
  /** Whether pilots sign themselves up for its heats, and whether that is open to them. */
  signup?: 'open' | 'closed';
  /** The heat on the timer, when it is one of this phase's. */
  onTimer?: PhaseId;
  /** Whether a pilot here has reached a false start limit and waits on a decision. */
  needsDecision: boolean;
}

/**
 * Every phase that holds heats or phases, as a tree laid flat: each one is followed by the
 * phases inside it, one deeper. A heat is never one of them: it has no inside to open.
 */
export function phaseSummaries(view: EventView): PhaseSummary[] {
  const tree = phaseTree(view.phases);
  const saved = new Set(
    view.races.filter((race) => race.state === 'Saved').map((race) => race.heat)
  );
  const summaries: PhaseSummary[] = [];
  tree.forEach((row, index) => {
    if (isHeat(row.phase.def)) return;
    const under: PhaseRow[] = [];
    for (const next of tree.slice(index + 1)) {
      if (next.depth <= row.depth) break;
      under.push(next);
    }
    const heats = under.map((next) => next.phase).filter((phase) => isHeat(phase.def));
    const ids = new Set([row.phase.def.id, ...under.map((next) => next.phase.def.id)]);
    summaries.push({
      id: row.phase.def.id,
      name: row.phase.def.label,
      depth: row.depth,
      // A phase whose parent is missing is laid out at the top, so it has none here.
      ...(row.depth > 0 && row.phase.def.parent !== undefined
        ? { parent: row.phase.def.parent }
        : {}),
      inside: under.filter((next) => next.depth === row.depth + 1 && !isHeat(next.phase.def))
        .length,
      roundsOnly: holdsOnlyRounds(row.phase, view.phases),
      status: phaseStatus(row.phase, view),
      format: row.phase.format?.name,
      heats: heats.length,
      flown: heats.filter((heat) => heat.complete || saved.has(heat.def.id)).length,
      signup: row.phase.signup ? (row.phase.signup.rules.open ? 'open' : 'closed') : undefined,
      onTimer: view.on_timer !== undefined && ids.has(view.on_timer) ? view.on_timer : undefined,
      // A proposal stands until the pilot is disqualified in that phase.
      needsDecision: view.proposed_disqualifications.some(
        (proposal) =>
          ids.has(proposal.phase) &&
          !view.phases
            .find((phase) => phase.def.id === proposal.phase)
            ?.ranking.find((placed) => placed.pilot === proposal.pilot)?.disqualified
      )
    });
  });
  return summaries;
}

/**
 * The lines the Phases page shows: a phase at the top always, and a phase inside another
 * only while every phase above it is unfolded.
 */
export function shownSummaries(
  summaries: readonly PhaseSummary[],
  unfolded: { has(id: PhaseId): boolean }
): PhaseSummary[] {
  const shown = new Set<PhaseId>();
  return summaries.filter((phase) => {
    const visible =
      phase.parent === undefined || (shown.has(phase.parent) && unfolded.has(phase.parent));
    if (visible) shown.add(phase.id);
    return visible;
  });
}

/** The heats that share a parent, as the Run and Marshal pickers offer them. */
export interface HeatGroup {
  /** The parent phase's id, or `''` for heats at the top of the event. Never shown. */
  key: string;
  /** The parent's name with the phases above it: "Mains › A-Main". */
  label: string;
  heats: PhaseView[];
}

/** What the heats at the top of an event, under no phase, are grouped as. */
export const TOP_LEVEL_HEATS = 'Heats';

/**
 * Every heat of the event, grouped by the phase that holds it, in tree order. This is what
 * lets a director pick a phase first and then one of its heats, rather than scroll one
 * long list of every heat in a bracket.
 */
export function heatGroups(phases: readonly PhaseView[]): HeatGroup[] {
  const byId = new Map(phases.map((phase) => [phase.def.id, phase]));
  const path = (id: PhaseId): string => {
    const labels: string[] = [];
    let at: PhaseId | undefined = id;
    // Bounded, so a loop in a bad snapshot cannot hang the page.
    for (let steps = 0; at !== undefined && steps <= phases.length; steps += 1) {
      const phase = byId.get(at);
      if (!phase) break;
      labels.unshift(phase.def.label);
      at = phase.def.parent;
    }
    return labels.join(' › ');
  };
  const groups = new Map<string, HeatGroup>();
  for (const { phase } of phaseTree(phases)) {
    if (!isHeat(phase.def)) continue;
    const parent =
      phase.def.parent !== undefined && byId.has(phase.def.parent) ? phase.def.parent : undefined;
    const key = parent ?? '';
    const group = groups.get(key) ?? {
      key,
      label: parent === undefined ? TOP_LEVEL_HEATS : path(parent),
      heats: []
    };
    group.heats.push(phase);
    groups.set(key, group);
  }
  return [...groups.values()];
}

/** A phase that can hold other phases, with its place in the tree spelled out. */
export interface HolderChoice {
  id: PhaseId;
  /** "Mains › A-Main". */
  label: string;
  /** The format in force there, if any: a new heat under it can simply use it. */
  format?: PhaseView['format'];
  locked: boolean;
}

/** Every phase that holds phases, in tree order: what a new heat or phase can go under. */
export function holderChoices(phases: readonly PhaseView[]): HolderChoice[] {
  const byId = new Map(phases.map((phase) => [phase.def.id, phase]));
  return phaseTree(phases)
    .filter(({ phase }) => !isHeat(phase.def))
    .map(({ phase }) => {
      const labels: string[] = [];
      let at: PhaseId | undefined = phase.def.id;
      for (let steps = 0; at !== undefined && steps <= phases.length; steps += 1) {
        const found = byId.get(at);
        if (!found) break;
        labels.unshift(found.def.label);
        at = found.def.parent;
      }
      return {
        id: phase.def.id,
        label: labels.join(' › '),
        format: phase.format,
        locked: phase.locked
      };
    });
}

/** The group a heat is in, by the heat's id. */
export function groupOfHeat(groups: readonly HeatGroup[], heat: PhaseId): HeatGroup | undefined {
  return groups.find((group) => group.heats.some((phase) => phase.def.id === heat));
}

/**
 * The heat that is on the timer: the one the director is running now. The Director keeps
 * it (`on_timer`): the heat of a race that is not saved or discarded yet, else the heat the
 * director last put on the timer or last started.
 *
 * Before the director has put anything there, it is the heat they would run first: the
 * first in the run queue, else the heat raced last, else the first heat there is.
 */
export function defaultHeat(view: EventView): PhaseId | undefined {
  const racing = currentRace(view);
  if (racing) return racing.heat;
  const onTimer = view.on_timer;
  if (onTimer !== undefined && view.phases.some((p) => p.def.id === onTimer && isHeat(p.def))) {
    return onTimer;
  }
  if (view.queue.length > 0) return view.queue[0];
  const last = view.races.at(-1);
  if (last) return last.heat;
  return view.phases.find((phase) => isHeat(phase.def))?.def.id;
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

// ── Editing a phase ─────────────────────────────────────────────────────────

/** What about a phase can be changed, and why not the rest. */
export interface PhaseEditable {
  /** Whether its format and where it sits can be changed. Its name always can. */
  shape: boolean;
  /** Why not, in words for the director. Left out when `shape` is true. */
  why?: string;
}

/**
 * What can be changed about a phase (docs/primitives.md §12.4). The Director decides in
 * the end; this says the same thing first, so the dialog offers only what will be taken.
 *
 * - **Its name:** always.
 * - **Its format and where it sits:** until a race that counts has been saved beneath it,
 *   in a heat that is ranked. After that a change would alter results already flown.
 * - A phase a generator made (a bracket, mains, a round robin), and everything inside
 *   one, keeps the shape the generator gave it.
 */
export function phaseEditable(
  phase: PhaseView,
  phases: readonly PhaseView[],
  races: readonly Pick<RaceView, 'heat' | 'state'>[]
): PhaseEditable {
  const byId = new Map(phases.map((each) => [each.def.id, each]));
  const under = (id: PhaseId): boolean => {
    let at: PhaseId | undefined = id;
    for (let steps = 0; at !== undefined && steps <= phases.length; steps += 1) {
      if (at === phase.def.id) return true;
      at = byId.get(at)?.def.parent;
    }
    return false;
  };
  let at: PhaseView | undefined = phase;
  for (let steps = 0; at !== undefined && steps <= phases.length; steps += 1) {
    if (at.def.display_hint !== undefined) {
      return {
        shape: false,
        why:
          at === phase
            ? 'Grid made this phase, so only its name can change.'
            : `It is part of ${at.def.label}, which Grid made, so only its name can change.`
      };
    }
    at = at.def.parent === undefined ? undefined : byId.get(at.def.parent);
  }
  const raced = races.some(
    (race) =>
      race.state === 'Saved' &&
      under(race.heat) &&
      byId.get(race.heat)?.format?.ranking !== undefined
  );
  if (raced) {
    return {
      shape: false,
      why: 'A race here has been saved, so only its name can change. Changing its format or where it sits would change results already flown.'
    };
  }
  return { shape: true };
}

/**
 * The phases an edited phase could be moved under: every phase that holds phases, but for
 * itself, anything beneath it, and anything locked.
 */
export function moveChoices(phase: PhaseView, phases: readonly PhaseView[]): HolderChoice[] {
  const byId = new Map(phases.map((each) => [each.def.id, each]));
  const beneath = (id: PhaseId): boolean => {
    let at: PhaseId | undefined = id;
    for (let steps = 0; at !== undefined && steps <= phases.length; steps += 1) {
      if (at === phase.def.id) return true;
      at = byId.get(at)?.def.parent;
    }
    return false;
  };
  return holderChoices(phases).filter((holder) => !beneath(holder.id) && !holder.locked);
}

/** Whether a phase can be removed: nothing beneath it, nothing raced, not locked. */
export function phaseRemovable(view: EventView, phase: PhaseView): boolean {
  return (
    !phase.locked &&
    !view.phases.some((other) => other.def.parent === phase.def.id) &&
    racesOfHeat(view, phase.def.id).length === 0
  );
}
