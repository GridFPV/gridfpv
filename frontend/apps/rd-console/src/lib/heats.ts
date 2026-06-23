/**
 * Shared heat-naming helpers for the RD console.
 *
 * Heats carry no human label on the wire ({@link HeatSummary} is the engine's view), so the
 * console derives a display name from the round + the heat's position within that round. This
 * one place owns that rule so the Rounds & Heats stage ({@link EventRounds}) and the Live-control
 * heat picker render the **same** name for a given heat.
 *
 *  - an **open-practice** round auto-creates a single channel heat → "Open Practice Heat";
 *  - every other round → "<round label> Heat <N>", where N is the heat's 1-based position in the
 *    round's heat list (a Qualifying round → "Qualifying Heat 1", "Qualifying Heat 2", …).
 *
 * This reads far better than the generated heat id, and matches how an RD thinks of a round's
 * heats ("Qualifying Heat 2") rather than the engine's internal id.
 */
import type { HeatSummary, RoundDef } from '@gridfpv/types';

import { OPEN_PRACTICE } from './formats.js';

/** The fixed display name for the single auto-created open-practice heat. */
export const OPEN_PRACTICE_HEAT_NAME = 'Open Practice Heat';

/** Whether `round` is an open-practice round (its single heat is named, not numbered). */
export function isOpenPracticeRound(round: RoundDef): boolean {
  return round.format === OPEN_PRACTICE;
}

/**
 * The display name for `heat` within `round`.
 *
 * `heatsInRound` is the round's heats **in list order** (the order the generator emitted them);
 * the heat's 1-based position in that list is its number. A heat not (yet) in the list is named
 * as the next one (`heatsInRound.length + 1`), which keeps a just-filled heat sensibly numbered.
 */
export function heatDisplayName(
  round: RoundDef,
  heat: HeatSummary,
  heatsInRound: HeatSummary[]
): string {
  if (isOpenPracticeRound(round)) return OPEN_PRACTICE_HEAT_NAME;
  const index = heatsInRound.findIndex((x) => x.heat === heat.heat);
  const n = index >= 0 ? index + 1 : heatsInRound.length + 1;
  return `${round.label} Heat ${n}`;
}
