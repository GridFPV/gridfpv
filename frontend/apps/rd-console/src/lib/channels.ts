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

/**
 * The label for a channel **inside a picker**, where the frequency itself is wanted:
 * `"Raceband R7 — 5880"`, or `"Custom — 5891"` for a frequency that is not in the catalog.
 *
 * Deliberately different from {@link channelLabel}, which is the label for a channel being
 * *reported* — a heading, a seat, a summary — where a bare number would be the raw handle standing
 * in for the name the display rule exists to prevent.
 *
 * Choosing is the opposite situation. An RD picking a channel is matching it against a VTX, a
 * printed sheet, or RotorHazard's own screen, and those speak in MHz. Here the number is *extra*
 * information sitting beside the friendly name, never a substitute for it — which is why the band
 * and channel still lead.
 */
export function channelOptionLabel(mhz: number, catalog: ChannelCatalogEntry[]): string {
  const hit = catalog.find((e) => e.mhz === mhz);
  return hit ? entryOptionLabel(hit, catalog) : `Custom — ${mhz}`;
}

/**
 * The picker label for a **known catalog entry** — used when the caller already has the entry it
 * offered, which is the only way to keep bands apart at a **coincident frequency**.
 *
 * `HDZero R7` and `Raceband R7` are both 5880. Re-deriving the label from the number alone finds
 * whichever the catalog lists first, silently relabelling the RD's choice as the other band. So an
 * option built from an entry must be labelled from that entry.
 */
export function entryOptionLabel(
  entry: ChannelCatalogEntry,
  catalog: readonly ChannelCatalogEntry[] = []
): string {
  const alt = alternateNames(entry, catalog);
  const also = alt.length > 0 ? ` (${alt.join(', ')})` : '';
  return `${entry.band} ${entry.channel}${also} — ${entry.mhz}`;
}

/**
 * The OTHER common names for this entry's frequency — `5880` is Raceband R7 **and** Fatshark F8.
 *
 * A pilot who knows their VTX as "F8" must still be able to find it, but a picker that lists both
 * as separate rows shows the same frequency twice with no way to tell which is "the" one. So the
 * catalog leads with one name and carries the rest in parentheses: one row per frequency, no name
 * lost.
 *
 * Returns just the channel code, not the band: `(F8)` reads cleanly where `(Fatshark F8)` crowds
 * the row, and the code is what a pilot says out loud.
 */
export function alternateNames(
  entry: ChannelCatalogEntry,
  catalog: readonly ChannelCatalogEntry[]
): string[] {
  const seen = new Set<string>([entry.channel]);
  const out: string[] = [];
  for (const other of catalog) {
    if (other.mhz !== entry.mhz || other === entry) continue;
    if (other.band === entry.band && other.channel === entry.channel) continue;
    if (seen.has(other.channel)) continue;
    seen.add(other.channel);
    out.push(other.channel);
  }
  return out;
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
 * The channel a node seat is configured for **within a timer's channel pool**, or `undefined` when
 * the pool says nothing about it.
 *
 * # An empty pool is "unrestricted", not "none" (#413, #416)
 *
 * `Timer.available_channels` is the set of channels the RD has *made available* on a timer. It is
 * **empty on every Flexible timer** — measured on the bench, the Mock lists eight channels and both
 * RotorHazard timers list none — and empty there means *"no restriction"*, not *"this timer has no
 * channels"*. Indexing straight into it (`pool[node]`) reads that emptiness as data and answers
 * `undefined` for every node of every real timer, which is how the seat label came to degrade to a
 * bare `Node N` on all RotorHazard hardware.
 *
 * So this refuses to index an empty pool at all. The caller asks other sources first (what the node
 * reports it is tuned to, what the heat assigned) and treats `undefined` as **unknown**.
 */
export function poolChannel(node: number, pool: readonly number[] | undefined): number | undefined {
  if (!pool || pool.length === 0) return undefined;
  return pool[node];
}

/**
 * The display label for one node seat: **node + channel**, which is the pair an RD actually needs.
 *
 * - a seat whose channel is known → `"Node 7 · Raceband R7"` — the node number is what the RD reads
 *   off the hardware, the channel is what the pilot needs to dial in, and neither alone is enough;
 * - a seat whose channel is genuinely **unknown** → `"Node 7"`, the node alone. Not "no channel":
 *   the timer may well be tuned to something GridFPV has not been told about.
 *
 * `node` is the 0-based wire index; the label is 1-based, per the repo display rule (index `6` is
 * the node the RD calls "Node 7"). The one place that boundary is crossed for a seat name — the
 * server's `Timer::node_label` is its twin, and `NodeSignal.node`'s doc names this same convention.
 *
 * Resolve `mhz` through `buildCompetitorNames` (`competitorName.ts`) rather than reaching for a
 * single source: it is the one place that knows which sources to try, and in what order.
 */
export function nodeSeatLabel(
  node: number,
  mhz: number | undefined,
  catalog: readonly ChannelCatalogEntry[]
): string {
  const seat = `Node ${node + 1}`;
  if (mhz === undefined) return seat;
  return `${seat} · ${channelLabel(mhz, [...catalog])}`;
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
