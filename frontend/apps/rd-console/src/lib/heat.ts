/**
 * Define-a-heat helpers (#13, v0.4 Director wiring).
 *
 * A basic sim race needs no "seen competitors" handshake — the built-in sim source emits
 * **no** `CompetitorSeen` (architecture.html §9; the sim only emits passes once a heat is
 * `Running`). So the RD must be able to **define the field directly**: type a few pilot
 * names, and those names become both the heat's lineup (the source-local `CompetitorRef`s
 * the sim will fly) *and* the pilots they're registered to.
 *
 * This module is the pure builder the "new heat" form uses. Given a heat id and a list of
 * pilot names it produces the exact wire commands, in order:
 *
 *   1. one `Register { adapter, competitor: <name>, pilot: <name> }` per pilot — binds the
 *      source-local ref to a pilot so the projection can name it (the adapter never does
 *      this itself);
 *   2. a single `ScheduleHeat { heat, lineup: [<name>, …] }` — lays the heat down with the
 *      named field as its lineup.
 *
 * The RD then drives the heat loop (Stage → Arm → Start → …) from the live screen; on
 * `Running` the sim flies passes for exactly this lineup.
 */

import type { AdapterId, Command, CompetitorRef, HeatId } from '@gridfpv/types';
import { registerCommand } from './registration.js';
import { scheduleHeatCommand } from './setup.js';

/** The default adapter a sim heat's competitors are bound under (the built-in source). */
export const SIM_ADAPTER: AdapterId = 'sim';

/** One pilot in a defined heat: the name the RD typed (used as both ref and pilot id). */
export interface HeatPilot {
  /** The pilot/competitor name (becomes the source-local ref *and* the pilot id). */
  name: string;
}

/** A heat the RD has defined directly: an id and an ordered, named field. */
export interface HeatDefinition {
  /** The heat id the RD names (the handle the control path drives). */
  heat: HeatId;
  /** The lineup, in seeding order — each pilot's typed name. */
  pilots: HeatPilot[];
  /** The adapter the competitors are bound under (defaults to {@link SIM_ADAPTER}). */
  adapter?: AdapterId;
}

/** Trim and drop blank pilot names, preserving order. */
function cleanNames(pilots: HeatPilot[]): string[] {
  return pilots.map((p) => p.name.trim()).filter((n) => n.length > 0);
}

/**
 * Validate a heat definition — the form's "you can schedule it" gate. Returns the list of
 * human problems (empty ⇒ ready). A heat needs an id, at least two distinct named pilots
 * (a race is more than one entrant), and no duplicate names (refs must be unique).
 */
export function validateHeat(def: HeatDefinition): string[] {
  const problems: string[] = [];
  if (!def.heat.trim()) problems.push('Heat needs an id.');
  const names = cleanNames(def.pilots);
  if (names.length < 2) problems.push('Add at least two pilots.');
  const seen = new Set<string>();
  for (const n of names) {
    if (seen.has(n)) problems.push(`Duplicate pilot name "${n}".`);
    seen.add(n);
  }
  return problems;
}

/** Is the definition complete enough to schedule? */
export function isHeatValid(def: HeatDefinition): boolean {
  return validateHeat(def).length === 0;
}

/** The lineup (`CompetitorRef[]`) a definition schedules — the cleaned, ordered names. */
export function lineupOf(def: HeatDefinition): CompetitorRef[] {
  return cleanNames(def.pilots);
}

/**
 * Build the ordered command list that defines and schedules a heat from named pilots: one
 * `Register` per pilot (binding `ref → pilot`, both the typed name) then one `ScheduleHeat`
 * carrying the lineup. The caller sends these in order; the sim flies this lineup once the
 * heat is driven to `Running`.
 */
export function defineHeatCommands(def: HeatDefinition): Command[] {
  const adapter = def.adapter ?? SIM_ADAPTER;
  const lineup = lineupOf(def);
  const commands: Command[] = lineup.map((name) => registerCommand(adapter, name, name));
  commands.push(scheduleHeatCommand(def.heat, lineup));
  return commands;
}
