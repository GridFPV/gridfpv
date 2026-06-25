/**
 * Shared heat-naming helpers for the RD console.
 *
 * Heats carry no human label on the wire ({@link HeatSummary} is the engine's view), so the
 * console derives a display name from the round + the heat's position within that round. This
 * one place owns that rule so the Rounds & Heats stage ({@link EventRounds}) and the Live-control
 * heat picker render the **same** name for a given heat.
 *
 *  - an **open-practice** round auto-creates a single channel heat → "Open Practice Heat";
 *  - a **multi-main** finals round (#219, D14) names its heats by *tier* → "A-Main", "B-Main", …
 *    since each heat **is** a main (the A/B/C split by qualifying rank), not a numbered heat;
 *  - every other round → "<round label> Heat <N>", where N is the heat's 1-based position in the
 *    round's heat list (a Qualifying round → "Qualifying Heat 1", "Qualifying Heat 2", …).
 *
 * This reads far better than the generated heat id, and matches how an RD thinks of a round's
 * heats ("Qualifying Heat 2", "B-Main") rather than the engine's internal id.
 */
import type { HeatId, HeatSummary, RoundDef } from '@gridfpv/types';

import { isDeterministicFormat, isMultiMainFormat, OPEN_PRACTICE } from './formats.js';

/** The fixed display name for the single auto-created open-practice heat. */
export const OPEN_PRACTICE_HEAT_NAME = 'Open Practice Heat';

/** Whether `round` is an open-practice round (its single heat is named, not numbered). */
export function isOpenPracticeRound(round: RoundDef): boolean {
  return round.format === OPEN_PRACTICE;
}

/** Whether `round` is a multi-main finals round (its heats are tiered mains, A/B/C). */
export function isMultiMainRound(round: RoundDef): boolean {
  return isMultiMainFormat(round.format);
}

/**
 * The tier name for the main at 0-based `index` (the heat's position in the round): 0 → "A-Main",
 * 1 → "B-Main", … matching the engine's `MultiMain::tier_label` (A=0, B=1, …). Past the alphabet
 * (>26 mains, vanishingly unlikely) it falls back to "Main <index+1>" so the name stays unique and
 * readable rather than running off the end of the letters.
 */
export function mainTierName(index: number): string {
  if (index >= 0 && index < 26) return `${String.fromCharCode(65 + index)}-Main`;
  return `Main ${index + 1}`;
}

/**
 * Whether `round`'s heats are deterministic — its whole heat set can be **generated in one action**
 * ("Generate heats", #216) rather than single-stepped. True for every format but Open Practice (see
 * {@link isDeterministicFormat}).
 */
export function isDeterministicRound(round: RoundDef): boolean {
  return isDeterministicFormat(round.format);
}

/**
 * The display name for `heat` within `round`.
 *
 * `heatsInRound` is the round's heats **in list order** (the order the generator emitted them, which
 * for multi-main is A-main, B-main, …); the heat's 1-based position in that list is its number. A
 * heat not (yet) in the list is named as the next one (`heatsInRound.length + 1`), which keeps a
 * just-filled heat sensibly numbered.
 *
 * Multi-main rounds (#219, D14) name each heat by its **tier** ("A-Main", "B-Main", …) off its
 * position in the round, since each heat *is* a main rather than a numbered heat in a series.
 */
export function heatDisplayName(
  round: RoundDef,
  heat: HeatSummary,
  heatsInRound: HeatSummary[]
): string {
  if (isOpenPracticeRound(round)) return OPEN_PRACTICE_HEAT_NAME;
  const index = heatsInRound.findIndex((x) => x.heat === heat.heat);
  if (isMultiMainRound(round)) {
    // The main's tier is its position in the round (A=first, B=second, …); a not-yet-listed heat
    // is the next main. The engine ids these `main-A`, `main-B`, … in this same order, so position
    // and tier agree.
    return mainTierName(index >= 0 ? index : heatsInRound.length);
  }
  const n = index >= 0 ? index + 1 : heatsInRound.length + 1;
  return `${round.label} Heat ${n}`;
}

/**
 * Resolve a bare {@link HeatId} to its friendly display name, given the scheduled `heats` list and
 * the event's `rounds`.
 *
 * This is the by-id convenience the Live-control screen needs for the **current-heat title** and the
 * **on-deck** heat, which it knows only as ids (off the live `LiveRaceState`). It joins the id → its
 * {@link HeatSummary} → that heat's {@link RoundDef} (off `rounds`), then defers to
 * {@link heatDisplayName} for the same "<Round> Heat N" / "Open Practice Heat" name the picker and
 * the Rounds & Heats stage use. Falls back to the bare id when the heat isn't in the list or carries
 * no resolvable round (a sim / free-text heat) — never an empty string.
 */
export function heatNameById(heatId: HeatId, heats: HeatSummary[], rounds: RoundDef[]): string {
  const summary = heats.find((h) => h.heat === heatId);
  if (!summary) return heatId;
  const round = summary.round ? rounds.find((r) => r.id === summary.round) : undefined;
  if (!round) return heatId;
  const inRound = heats.filter((h) => h.round === round.id);
  return heatDisplayName(round, summary, inRound);
}
