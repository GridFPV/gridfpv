/**
 * Channel presentation + selection helpers (race redesign Slice 4b).
 *
 * Pure mappers shared by the timer channel-config picker (which offers the standard FPV catalog
 * grouped by band, plus custom raw-MHz entries) and the per-heat channel display (which resolves a
 * heat's assigned raw MHz back to a human band+channel label). No I/O — the session owns the
 * `GET /channels` read; these just shape the catalog and the timer's capability for the UI.
 */
import type { ChannelCapability, ChannelCatalogEntry } from '@gridfpv/types';

/** A band and its catalog entries, in the catalog's stable channel order. */
export interface ChannelBand {
  /** The band name (e.g. `"Raceband"`, `"Fatshark"`, `"DJI"`). */
  band: string;
  /** The band's entries (label + MHz), in catalog order. */
  entries: ChannelCatalogEntry[];
}

/**
 * Group a flat catalog into bands, preserving the catalog's order both within and across bands
 * (the server emits a stable, deterministic order — Raceband, Fatshark, Boscam A/B/E, DJI, HDZero).
 */
export function groupByBand(catalog: ChannelCatalogEntry[]): ChannelBand[] {
  const bands: ChannelBand[] = [];
  const byName = new Map<string, ChannelBand>();
  for (const entry of catalog) {
    let band = byName.get(entry.band);
    if (!band) {
      band = { band: entry.band, entries: [] };
      byName.set(entry.band, band);
      bands.push(band);
    }
    band.entries.push(entry);
  }
  return bands;
}

/**
 * The human label for a raw frequency, resolved through the catalog: `"Raceband R1"` (band +
 * channel) when a catalog entry matches, else a bare `"5800 MHz"` fall-back for a custom/unknown
 * channel. The **first** catalog entry whose MHz matches wins (the catalog is offered in a stable
 * order, so Raceband is preferred over a coincident grid).
 */
export function channelLabel(mhz: number, catalog: ChannelCatalogEntry[]): string {
  const hit = catalog.find((e) => e.mhz === mhz);
  return hit ? `${hit.band} ${hit.channel}` : `${mhz} MHz`;
}

/** The catalog entry an MHz resolves to (the first match), or `undefined` for a custom/unknown one. */
export function catalogEntryFor(
  mhz: number,
  catalog: ChannelCatalogEntry[]
): ChannelCatalogEntry | undefined {
  return catalog.find((e) => e.mhz === mhz);
}

/** Whether a frequency is a catalog channel (vs. a custom raw-MHz entry the RD typed). */
export function isCatalogChannel(mhz: number, catalog: ChannelCatalogEntry[]): boolean {
  return catalog.some((e) => e.mhz === mhz);
}

/** The discriminant tag of a capability (`'Fixed'` | `'Flexible'`). */
export type CapabilityTag = 'Fixed' | 'Flexible';

export function capabilityTag(cap: ChannelCapability | undefined): CapabilityTag {
  return cap && typeof cap === 'object' && 'Fixed' in cap ? 'Fixed' : 'Flexible';
}

/** A Fixed capability's allowed built-in set (its `channels`), or `[]` for a Flexible one. */
export function fixedAllowed(cap: ChannelCapability | undefined): number[] {
  return cap && typeof cap === 'object' && 'Fixed' in cap ? cap.Fixed.channels : [];
}

/**
 * The catalog a picker offers for a given capability: a **Fixed** timer is limited to its built-in
 * allowed set (no custom), so only those catalog entries show; a **Flexible** timer offers the whole
 * catalog (and may add custom raw MHz). Preserves catalog order.
 */
export function offeredCatalog(
  cap: ChannelCapability | undefined,
  catalog: ChannelCatalogEntry[]
): ChannelCatalogEntry[] {
  if (capabilityTag(cap) === 'Flexible') return catalog;
  const allowed = new Set(fixedAllowed(cap));
  return catalog.filter((e) => allowed.has(e.mhz));
}

/** A plausible 5.8 GHz centre frequency (the band the catalog lives in). Guards custom-MHz entry. */
export function isPlausibleMhz(mhz: number): boolean {
  return Number.isInteger(mhz) && mhz >= 5300 && mhz <= 6000;
}
