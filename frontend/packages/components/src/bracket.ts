/**
 * Bracket view-model — a small, presentation-only shape for `BracketTree`.
 *
 * ── Why a view-model lives here ──────────────────────────────────────────────
 * The protocol wire (the ts-rs `bindings/`) does NOT yet expose a single-elim
 * bracket projection: it has `HeatId`, `HeatResult`/`Placement`, `CompletedHeat`,
 * and `CompetitorRef`, but no "rounds → heats → winners" tree. Rather than
 * hand-write a wire type (forbidden — `@gridfpv/types` is the only seam to the
 * generated contract), `BracketTree` takes this explicit **view-model** that a
 * caller derives from heats + their results. When the server grows a real
 * bracket projection, this type is replaced by the generated one and the
 * component's prop simply re-points at `@gridfpv/types`.
 *
 * The shape intentionally mirrors the wire vocabulary it is built from:
 *   - slots carry a `CompetitorRef` (the source-local handle, as `Placement`
 *     and `RankEntry` do) plus an optional human `label`;
 *   - a match's `winner` is a `CompetitorRef`, matching what a `HeatResult`'s
 *     top `Placement` yields.
 */

import type { CompetitorRef, HeatId } from '@gridfpv/types';

/** One competitor's seat in a bracket match. */
export interface BracketSlot {
  /** Source-local competitor handle (as carried on the wire), if seated. */
  competitor?: CompetitorRef;
  /** Optional display label; falls back to `competitor` when absent. */
  label?: string;
  /** Marks the slot that advanced from this match (the heat's winner). */
  winner?: boolean;
  /**
   * Optional **series score** — a finalist's running race-win count in a multi-race series (the
   * Chase-the-Ace final). Rendered as a compact win-count pill on the slot. `undefined` (the normal
   * single-heat match case) shows no pill; `0` is a real score and *does* render a pill.
   */
  score?: number;
}

/** A single heat in the bracket (two or more seats racing for advancement). */
export interface BracketMatch {
  /** The heat this match corresponds to in the log, if scheduled. */
  heat?: HeatId;
  /** The seats in this match, in lineup order. */
  slots: BracketSlot[];
  /**
   * Optional small caption rendered above the match box — e.g. a series summary like
   * `"Best of 3 · 2 races"` for a collapsed Chase-the-Ace final. `undefined` shows no caption.
   */
  note?: string;
}

/** One round of the bracket (e.g. quarterfinals), earliest round first. */
export interface BracketRound {
  /** Human round name (e.g. `'Quarterfinals'`, `'Final'`). */
  name: string;
  /** The matches in this round, top to bottom. */
  matches: BracketMatch[];
}

/** A single-elimination bracket: rounds in order, last round is the final. */
export interface Bracket {
  rounds: BracketRound[];
}
