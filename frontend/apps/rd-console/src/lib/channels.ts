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

/**
 * Parse the node index out of an open-practice competitor ref (`node-{i}` → `i`), or `undefined`
 * for any other ref. Open-practice heats lay their channels out as `node-{i}` refs (the timer-seat
 * index), so this is the join key back onto the timer's `available_channels`.
 */
export function nodeIndexOf(ref: string): number | undefined {
  const m = /^node-(\d+)$/.exec(ref);
  if (!m) return undefined;
  return Number(m[1]);
}

/**
 * The display label for one open-practice node seat (`node-{i}`), resolved through the timer's
 * `available_channels` + the catalog:
 *
 * - a node whose seat has a configured channel → `"Raceband R1 · 5658"` (band + channel · MHz), or
 *   `"5800 MHz"` when the raw MHz isn't a catalog channel;
 * - a node with no configured channel (index ≥ the available pool) → `"Node {i}"`.
 *
 * `availableChannels` is the timer's `available_channels` (raw MHz, in seat/node order). Shared by
 * the active-channels picker and the per-channel live board so both label a seat identically.
 */
export function nodeChannelLabel(
  node: number,
  availableChannels: number[],
  catalog: ChannelCatalogEntry[]
): string {
  const mhz = availableChannels[node];
  if (mhz === undefined) return `Node ${node + 1}`;
  const hit = catalogEntryFor(mhz, catalog);
  return hit ? `${hit.band} ${hit.channel} · ${mhz}` : `${mhz} MHz`;
}

/**
 * Auto-assign channels to an ordered list of pilots, deterministic round-robin (first-fit) across an
 * ordered `pool` of available channels: the `i`-th pilot gets `pool[i % pool.length]`. When there are
 * fewer channels than pilots the assignment wraps and repeats — that's fine, repeated pilots simply
 * fly in different heats. The input order is the caller's stable order (roster order), so the result
 * is reproducible. An empty pool yields an empty map (nothing to assign).
 *
 * Returns a `pilotId → channel(MHz)` map covering every input pilot (when the pool is non-empty).
 */
export function assignChannelsRoundRobin<Id>(pilots: Id[], pool: number[]): Map<Id, number> {
  const out = new Map<Id, number>();
  if (pool.length === 0) return out;
  pilots.forEach((id, i) => out.set(id, pool[i % pool.length]));
  return out;
}
