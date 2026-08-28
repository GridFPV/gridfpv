/**
 * Shared heat-naming helpers for the RD console.
 *
 * **The name comes off the wire.** A heat's display name — "Qualifying Heat 2", "A-Main",
 * "Practice Heat 2", or the RD's own typed label — is resolved by the server and carried on
 * {@link HeatSummary.name}. This module is the console's side of that: the id → name lookup, and
 * the raw-handle last resort. It does not re-derive the convention.
 *
 * It used to (#456). The rule lived here AND in the server's `round_engine::heat_display_name`,
 * hand-maintained twins, and they had already drifted: this file numbered a round's extra practice
 * heats while the server returned a bare "Practice Heat" for every one of them, so an RD who added
 * a second practice heat saw numbered rows on screen and an ambiguous name in every sentence the
 * server wrote — the CommandAck detail, the refusals, `ScheduledHeat.name`. One derivation, on the
 * wire, is the only shape in which that cannot happen; the convention itself now lives once, in
 * `crates/server/src/round_engine.rs`'s `heat_name`.
 */
import type { HeatId, HeatSummary, RoundDef } from '@gridfpv/types';

import { isDeterministicFormat, OPEN_PRACTICE } from './formats.js';

/**
 * The name for a heat id the event no longer serves — a heat that was **removed with its round**
 * (#418), or a stale id held from before a refresh.
 *
 * Removing a round takes its still-unstarted heats with it: the log keeps the `HeatScheduled` (it
 * is append-only) but the server stops serving a heat whose round the event no longer defines,
 * because it has no name, no win condition and no scoring left to resolve through. The live
 * `current_heat` can still be pointing at one — an RD who had loaded it in Live control and then
 * deleted the round — and rendering the raw heat id there is exactly the leak the display rule
 * forbids. This says what actually happened instead.
 *
 * This one stays client-side because it is not a *heat's* name: it is what to show when there is no
 * heat to name at all, which only the holder of the stale id can know.
 */
export const REMOVED_HEAT_NAME = 'Removed heat';

/** Whether `round` is an open-practice round (its single heat is named, not numbered). */
export function isOpenPracticeRound(round: RoundDef): boolean {
  return round.format === OPEN_PRACTICE;
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
 * The display name of `heat` — {@link HeatSummary.name}, as the server resolved it.
 *
 * The only judgement left here is the **last resort**: a name that is missing or blank falls back
 * to the raw handle, which for the heat that can reach this state (a sim / free-text heat with no
 * round to derive from) is the RD's own typed identifier rather than a generated id.
 */
export function heatDisplayName(heat: HeatSummary): string {
  return heat.name?.trim() || heat.heat;
}

/**
 * Resolve a bare {@link HeatId} to its friendly display name, given the scheduled `heats` list.
 *
 * This is the by-id convenience the Live-control screen needs for the **current-heat title** and the
 * **on-deck** heat, which it knows only as ids (off the live `LiveRaceState`), and the entry point
 * CLAUDE.md names for heat-id → name everywhere else.
 *
 * A heat the event no longer serves resolves to {@link REMOVED_HEAT_NAME} — never its raw id
 * (#418: removing a round takes its unstarted heats with it, and the live `current_heat` can still
 * name one).
 */
export function heatNameById(heatId: HeatId, heats: HeatSummary[]): string {
  const summary = heats.find((h) => h.heat === heatId);
  if (!summary) return REMOVED_HEAT_NAME;
  return heatDisplayName(summary);
}
