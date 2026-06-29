/**
 * Multi-main view-model — a small, presentation-only shape for `MultiMainView`.
 *
 * Like {@link RoundRobin} (see `roundRobin.ts`), the protocol wire does not expose a single
 * multi-main projection: the round's standings are derived by a caller from the round's heats (the
 * tiered mains), their {@link HeatResult}s, and the canonical `roundRanking`. Rather than hand-write a
 * wire type (forbidden — `@gridfpv/types` is the only seam to the generated contract), `MultiMainView`
 * takes this explicit view-model. When the server grows a real multi-main projection, this type is
 * replaced by the generated one and the component's prop simply re-points at `@gridfpv/types`.
 *
 * The shape mirrors the wire vocabulary it is built from: a competitor carries a `CompetitorRef`
 * (the source-local handle) plus a resolved display `label` (the friendly callsign — never the raw
 * ref reaches the screen, per the repo's friendly-name rule).
 */

import type { CompetitorRef } from '@gridfpv/types';

/** One pilot's row in the multi-main standings table (best first, by the canonical ranking). */
export interface MultiMainStanding {
  /** Source-local competitor handle (as carried on the wire), if known. */
  competitor?: CompetitorRef;
  /** Resolved display label (callsign); falls back to `competitor` when absent. */
  label: string;
  /** 1-based standings position (from the canonical round ranking; tie-aware). */
  position: number;
  /** The friendly name of the main (tier) this pilot raced in — "A-Main", "B-Main", … when known. */
  tierName?: string;
}

/** A multi-main round: its aggregate standings, best first, each row tagged with the pilot's tier. */
export interface MultiMain {
  /** The aggregate standings, best first (the canonical round ranking), each tagged with its tier. */
  standings: MultiMainStanding[];
}
