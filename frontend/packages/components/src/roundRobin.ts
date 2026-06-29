/**
 * Round-robin view-model — a small, presentation-only shape for `RoundRobinView`.
 *
 * Like {@link Bracket} (see `bracket.ts`), the protocol wire does not expose a single round-robin
 * projection: the round's standings + per-rotation grid are derived by a caller from the round's
 * heats, their {@link HeatResult}s, and the canonical `roundRanking`. Rather than hand-write a wire
 * type (forbidden — `@gridfpv/types` is the only seam to the generated contract), `RoundRobinView`
 * takes this explicit view-model. When the server grows a real round-robin projection, this type is
 * replaced by the generated one and the component's prop simply re-points at `@gridfpv/types`.
 *
 * The shape mirrors the wire vocabulary it is built from: a competitor carries a `CompetitorRef`
 * (the source-local handle) plus a resolved display `label` (the friendly callsign — never the raw
 * ref reaches the screen, per the repo's friendly-name rule).
 */

import type { CompetitorRef, HeatId } from '@gridfpv/types';

/** One pilot's row in the round-robin standings table (best first, by the canonical ranking). */
export interface RoundRobinStanding {
  /** Source-local competitor handle (as carried on the wire), if known. */
  competitor?: CompetitorRef;
  /** Resolved display label (callsign); falls back to `competitor` when absent. */
  label: string;
  /** 1-based standings position (from the canonical round ranking; tie-aware). */
  position: number;
  /** Total points across the round's heats (per-position table summed over each finish). */
  points: number;
  /** Best single lap across the pilot's heats in microseconds, when available. */
  bestLapMicros?: number | null;
}

/** One competitor's seat in a round-robin heat. */
export interface RoundRobinEntry {
  /** Source-local competitor handle, if seated. */
  competitor?: CompetitorRef;
  /** Resolved display label (callsign); falls back to `competitor`. */
  label: string;
  /** 1-based finishing position in this heat, when the heat is scored (else undefined). */
  position?: number;
}

/** One heat within a rotation — its name + lineup (in finishing order when scored). */
export interface RoundRobinHeat {
  /** The heat's id (its scheduled handle), if scheduled. */
  heat?: HeatId;
  /** Friendly heat name (e.g. "Round Robin Heat 1"). */
  name: string;
  /** The seats, in finishing order when the heat is scored, else lineup order. */
  entries: RoundRobinEntry[];
}

/** One rotation — a group of heats flown together (every pilot races once per rotation). */
export interface RoundRobinRotation {
  /** Friendly rotation name (e.g. "Rotation 1"). */
  name: string;
  /** The rotation's heats, in heat order. */
  heats: RoundRobinHeat[];
}

/** A round-robin round: its aggregate standings + the per-rotation grid of heats. */
export interface RoundRobin {
  /** The aggregate standings, best first (the canonical round ranking, with computed points/best lap). */
  standings: RoundRobinStanding[];
  /** The heats grouped by rotation, in rotation order. */
  rotations: RoundRobinRotation[];
}
