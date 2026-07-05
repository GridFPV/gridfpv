/**
 * Shared competitor → display-name resolution for the RD console.
 *
 * A {@link CompetitorRef} is a source-local handle (a node seat, a pilot id, a sim handle) — never a
 * human label. Several screens (Live control, Marshaling) need to render the **callsign** for a ref,
 * and they all face the same three cases. This module owns that one rule so the screens render the
 * *same* name for a given ref and never drift (the regression #214/#212 surfaced when the logic was
 * inlined in Live control alone; Marshaling still showed raw refs).
 *
 * The resolution, in order:
 *  1. an **explicit** `Register` binding (carried on the live `progress[].pilot`, the manual /
 *     open-practice registration path) → the bound pilot's callsign;
 *  2. the **roster-seeded** binding: a `FromRoster` heat seeds each competitor ref *equal to the
 *     pilot id itself* and emits **no** `CompetitorRegistered` event, so `progress.pilot` is `null`
 *     in every phase — the ref interpreted directly as a directory pilot id resolves its callsign.
 *     This is the common competition heat, available pre-race (Scheduled → Running);
 *  3. an **unbound channel seat** — an open-practice `node-{i}` ref with no pilot — → its **channel
 *     label** (so a seat reads "Raceband R1 · 5658", not "node-0");
 *  4. otherwise the bare ref (already a human-entered competitor handle in a normal sim heat).
 *
 * The channel-label fallback is scoped to `node-{i}` refs deliberately: a normal competition heat
 * also assigns a channel per pilot, but there the ref is the pilot's own handle and must show as-is.
 */
import type { ChannelCatalogEntry, CompetitorRef, Pilot, PilotId } from '@gridfpv/types';

import { nodeIndexOf } from './channels.js';

/** The directory + per-heat inputs a {@link CompetitorNameResolver} resolves against. */
export interface CompetitorNameInputs {
  /** The app-level pilots directory (callsigns), keyed by id. */
  pilotById: Map<PilotId, Pilot>;
  /**
   * A competitor ref → its bound pilot id, from an **explicit** `Register` (the live `progress.pilot`
   * binding). Empty for the common roster-seeded heat (which carries no registration event).
   */
  explicitPilotByRef: Map<CompetitorRef, PilotId>;
  /**
   * A competitor ref → its resolved channel label, for the open-practice `node-{i}` seats. Used only
   * as the fallback when a ref doesn't resolve to a directory pilot. Optional (Marshaling has no
   * per-channel context for non-open-practice heats).
   */
  channelByRef?: Map<CompetitorRef, string>;
}

/** Resolves a {@link CompetitorRef} to its friendly display name (callsign / channel / bare ref). */
export type CompetitorNameResolver = (ref: CompetitorRef) => string;

/**
 * Build a {@link CompetitorNameResolver} over the given directory + per-heat inputs.
 *
 * This is the single resolver both Live control and Marshaling go through (DRY). Each screen owns the
 * *reactive* assembly of the inputs (the pilots directory, the explicit-binding map, the channel map)
 * and passes them here; the resolution rule itself lives in one place.
 */
export function createCompetitorNameResolver(inputs: CompetitorNameInputs): CompetitorNameResolver {
  const { pilotById, explicitPilotByRef, channelByRef } = inputs;
  return (ref: CompetitorRef): string => {
    // (1) Explicit registration binding, then (2) the ref interpreted as a directory pilot id (the
    // roster-seeded binding, available pre-race independent of `progress`).
    const pid = explicitPilotByRef.get(ref) ?? ref;
    const callsign = pilotById.get(pid)?.callsign;
    if (callsign) return callsign;
    // (3) An unbound open-practice channel seat → its channel label (not "node-0").
    if (nodeIndexOf(ref) !== undefined) {
      const channel = channelByRef?.get(ref);
      if (channel) return channel;
    }
    // (4) A human-entered handle in a normal sim heat — show as-is.
    return ref;
  };
}

/** Re-export so callers can build the channel map without reaching into `channels.ts` too. */
export type { ChannelCatalogEntry };
