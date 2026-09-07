/**
 * Shared competitor → display-name resolution for the RD console.
 *
 * A {@link CompetitorRef} is a source-local handle (a node seat, a pilot id, a sim handle) — never a
 * human label. Several screens (Live control, Marshaling, Rounds & Heats, Tune, Results, Audit) need
 * to render the **callsign** for a ref, and they all face the same cases. This module owns that one
 * rule so the screens render the *same* name for a given ref and never drift (the regression
 * #214/#212 surfaced when the logic was inlined in Live control alone; Marshaling still showed raw
 * refs).
 *
 * The resolution, in order:
 *  1. an **explicit** `Register` binding (carried on the live `progress[].pilot`, the manual /
 *     open-practice registration path) → the bound pilot's callsign;
 *  2. the **roster-seeded** binding: a `FromRoster` heat seeds each competitor ref *equal to the
 *     pilot id itself* and emits **no** `CompetitorRegistered` event, so `progress.pilot` is `null`
 *     in every phase — the ref interpreted directly as a directory pilot id resolves its callsign.
 *     This is the common competition heat, available pre-race (Scheduled → Running);
 *  3. an **unbound node seat** — an open-practice `node-{i}` ref with no pilot — → its **seat
 *     label**: `"Node 7 · Raceband R7"` where the channel is known, `"Node 7"` where it is not.
 *     A `node-{i}` ref NEVER reaches the screen raw (#416);
 *  4. otherwise the bare ref (already a human-entered competitor handle in a normal sim heat).
 *
 * The seat-label fallback is scoped to `node-{i}` refs deliberately: a normal competition heat
 * also assigns a channel per pilot, but there the ref is the pilot's own handle and must show as-is.
 *
 * ## One place builds the inputs (#416)
 *
 * The resolver was already shared — what was not, was the **assembly of its inputs**. Three screens
 * each built their own: one with no channel data at all (so a `node-{i}` seat fell through to the
 * raw ref), one with heat channels, one with live progress. Same ref, three different answers, and
 * `node-6` on one screen against `Node 7` on another. That is the drift the shared-resolver rule
 * exists to prevent, one level above where the rule was aimed.
 *
 * So {@link buildCompetitorNames} is now the one place: every screen hands it whatever sources it
 * has (pilots, live progress, the heat, the timer, the live signal, class membership, the catalog)
 * and gets back a resolver — plus the channel lookups that used to be re-derived per screen.
 */
import type {
  ChannelCatalogEntry,
  ChannelLayout,
  ClassMembership,
  CompetitorRef,
  HeatSummary,
  LayoutId,
  Pilot,
  PilotId,
  PilotProgress,
  Timer,
  TimerSignal
} from '@gridfpv/types';

import { channelLabel, nodeIndexOf, nodeSeatLabel } from './channels.js';
import { timerWidth } from './timerNodes.js';

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
   * The **seat label** for an unbound `node-{i}` ref — `"Node 7 · Raceband R7"`, or `"Node 7"` where
   * the channel is unknown. Used only when a ref doesn't resolve to a directory pilot.
   *
   * Optional, and its absence is safe: a `node-{i}` ref with no entry still resolves to `"Node N"`
   * rather than leaking the raw ref. Build it with {@link buildCompetitorNames} rather than by hand.
   */
  seatLabelByRef?: Map<CompetitorRef, string>;
}

/** Resolves a {@link CompetitorRef} to its friendly display name (callsign / seat / bare ref). */
export type CompetitorNameResolver = (ref: CompetitorRef) => string;

/**
 * Build a {@link CompetitorNameResolver} over the given directory + per-heat inputs.
 *
 * The resolution *rule*; {@link buildCompetitorNames} is what assembles the inputs. Prefer that —
 * calling this directly is how the per-screen input drift of #416 happened.
 */
export function createCompetitorNameResolver(inputs: CompetitorNameInputs): CompetitorNameResolver {
  const { pilotById, explicitPilotByRef, seatLabelByRef } = inputs;
  return (ref: CompetitorRef): string => {
    // (1) Explicit registration binding, then (2) the ref interpreted as a directory pilot id (the
    // roster-seeded binding, available pre-race independent of `progress`).
    const pid = explicitPilotByRef.get(ref) ?? ref;
    const callsign = pilotById.get(pid)?.callsign;
    if (callsign) return callsign;
    // (3) An unbound node seat → its seat label. The `?? nodeSeatLabel(node, undefined, [])` is not
    // belt-and-braces: it is the guarantee. A caller that assembled no channel data at all still
    // gets "Node 7" and never `node-6` — the raw-ref leak of #416 cannot come back by omission.
    const node = nodeIndexOf(ref);
    if (node !== undefined) return seatLabelByRef?.get(ref) ?? nodeSeatLabel(node, undefined, []);
    // (4) A human-entered handle in a normal sim heat — show as-is.
    return ref;
  };
}

/**
 * Everything a screen needs to render a competitor ref, built from one set of sources.
 *
 * `name` is the resolver; the channel lookups are exposed because a screen that shows a **channel
 * column** next to the name needs the channel on its own, and re-deriving it per screen is what
 * produced two different answers for one seat.
 */
export interface CompetitorNames {
  /** ref → friendly display name (callsign / `"Node 7 · Raceband R7"` / bare handle). */
  name: CompetitorNameResolver;
  /** ref → the channel it is on, in raw MHz, or `undefined` when genuinely unknown. */
  mhzFor: (ref: CompetitorRef) => number | undefined;
  /**
   * ref → its channel label alone (`"Raceband R7"`, or `"5111 MHz"` off-catalog), or `undefined`
   * when genuinely unknown. **Unknown is not "none"** — a caller rendering this must say so.
   */
  channelFor: (ref: CompetitorRef) => string | undefined;
  /** node index → its seat label (`"Node 7 · Raceband R7"`), for a picker laid out by node. */
  seatLabel: (node: number) => string;
  /** The assembled resolver inputs, for a consumer that takes them directly. */
  inputs: CompetitorNameInputs;
}

/** The sources a screen can offer. Every one is optional — a screen passes what it has. */
export interface CompetitorNameSources {
  /** The app-level pilots directory (callsigns). */
  pilots?: readonly Pilot[];
  /** The live `progress` rows, which carry an **explicit** `Register` binding where one exists. */
  progress?: readonly PilotProgress[] | null;
  /**
   * Explicit `ref → pilot` bindings a screen already holds (the per-heat registration folds the
   * Results screen pulls). Applied **before** {@link progress}, so a live binding still wins.
   */
  bindings?: Iterable<readonly [CompetitorRef, PilotId]>;
  /**
   * Every heat in scope, for a screen that spans a whole event (Results, Audit). Their
   * `frequencies` all contribute; {@link heat} is applied last and wins for a shared ref.
   */
  heats?: readonly HeatSummary[];
  /** The heat being rendered — its `frequencies` are the channels actually assigned to its seats. */
  heat?: HeatSummary | null;
  /** The standard channel catalog (`GET /channels`), for MHz → band+channel labels. */
  catalog?: readonly ChannelCatalogEntry[];
  /**
   * The timer's **live signal** (`GET /timers/{id}/signal`), whose `NodeSignal.frequency_mhz` is
   * what each node is *actually* tuned to. The most authoritative channel source there is — and the
   * only one that works on a Flexible RotorHazard timer. Only the Tune page holds a subscription.
   */
  signal?: TimerSignal | null;
  /** The event's primary timer — for its node width, which seeds the seat labels. */
  timer?: Timer | null;
  /**
   * The event's **channel layouts** (`EventMeta.channel_layouts`) — #117 S3.
   *
   * A layout is a complete `node → channel` tuning of the event's timer, and the heat being
   * rendered names the one it flies (`HeatSummary.layout`). Together they are the **per-node
   * mapping** a seat's channel resolves through — the thing an allowed set never had, and the
   * reason source (3) below used to be a fabrication.
   */
  layouts?: readonly ChannelLayout[] | null;
  /**
   * The layout to resolve node channels through when there is **no heat to ask** — a screen that is
   * laying out node seats before any heat exists (#402).
   *
   * The open-practice round form is the case this is for: it asks the RD *which nodes practice runs
   * on*, and #402's sharpest complaint was that the picker is channel-blind at exactly the moment
   * the RD is choosing which channels practice flies. The round's own selected layout answers it.
   *
   * A heat's own {@link HeatSummary.layout} wins when there is one — that is the heat's recorded
   * fact, and this is only a stand-in for its absence.
   */
  layout?: LayoutId | null;
  /** The event's `classes_membership` — a pilot's fixed per-class channel in a Static round. */
  membership?: readonly ClassMembership[] | null;
}

/**
 * Assemble every competitor-name input from a screen's sources — **the one place that does this**.
 *
 * # Resolving a channel without `available_channels`
 *
 * A node seat's channel is looked for in this order, and the order is the point:
 *
 * 1. **the heat's own assignment** (`HeatSummary.frequencies`) — what the round allocated for this
 *    heat, and the only source that is per-heat rather than per-timer;
 * 2. **what the node is tuned to** (`NodeSignal.frequency_mhz`) — the hardware's own answer;
 * 3. **the heat's channel layout** (`HeatSummary.layout` → that layout's `node → channel` entry);
 * 4. **the pilot's class membership channel**, for a seat bound to a pilot.
 *
 * ## Source (3) was a fabrication until #117 S3
 *
 * It used to read `Timer.available_channels[node]` — indexing the timer's **allowed set** by node
 * index. An allowed set is a *set*: it says which channels this timer may ever use and carries no
 * per-node mapping whatsoever, so position `n` in it had no relationship to node `n`. It produced a
 * plausible-looking answer that was simply invented, and only looked harmless because
 * `available_channels` is **empty on every Flexible timer** (measured on the bench: the Mock lists
 * eight, both RotorHazard timers list none) — so on real hardware it always returned `undefined`
 * and the seat degraded to a bare `Node N`. That was #416's remaining half.
 *
 * A **channel layout** is the mapping the allowed set never had: one channel per node, chosen by
 * the RD, and named by the heat that flies it. So source (3) is now the heat's layout, which is a
 * real per-node answer rather than a coincidence of list order.
 *
 * When every source comes up empty the channel is **genuinely unknown**, and the seat reads as the
 * node alone (`"Node 7"`). A caller showing a channel *column* must render that as unknown — never
 * as an em dash meaning "no channel", which is a different and false statement.
 */
export function buildCompetitorNames(sources: CompetitorNameSources): CompetitorNames {
  const catalog = [...(sources.catalog ?? [])];

  const pilotById = new Map<PilotId, Pilot>((sources.pilots ?? []).map((p) => [p.id, p]));

  const explicitPilotByRef = new Map<CompetitorRef, PilotId>();
  for (const [ref, pilot] of sources.bindings ?? []) explicitPilotByRef.set(ref, pilot);
  for (const row of sources.progress ?? []) {
    if (row.pilot != null) explicitPilotByRef.set(row.competitor, row.pilot);
  }

  // (1) The heats' own per-seat assignments; the focused heat is applied last and wins.
  const heatMhzByRef = new Map<CompetitorRef, number>();
  for (const h of sources.heats ?? []) {
    for (const [ref, mhz] of h.frequencies ?? []) heatMhzByRef.set(ref, mhz);
  }
  for (const [ref, mhz] of sources.heat?.frequencies ?? []) heatMhzByRef.set(ref, mhz);
  // (2) What each node reports it is tuned to.
  const signalMhzByNode = new Map<number, number>();
  for (const node of sources.signal?.nodes ?? []) {
    if (node.frequency_mhz !== undefined) signalMhzByNode.set(node.node, node.frequency_mhz);
  }
  // (3) The heat's channel layout: the RD's own `node → channel` mapping for this heat (#117 S3).
  // `undefined` when the heat flies no layout — never fall back to indexing the allowed set, which
  // is what made this source a fabrication before layouts existed.
  const layoutId = sources.heat?.layout ?? sources.layout ?? undefined;
  const layout = layoutId ? (sources.layouts ?? []).find((l) => l.id === layoutId) : undefined;
  const layoutMhzByNode = new Map<number, number>(
    (layout?.nodes ?? []).map((n) => [n.node, n.channel])
  );
  // (4) A pilot's fixed per-class channel.
  const membershipMhzByPilot = new Map<PilotId, number>();
  for (const entry of sources.membership ?? []) {
    for (const slot of entry.pilots ?? []) {
      if (slot.channel !== undefined) membershipMhzByPilot.set(slot.pilot, slot.channel);
    }
  }

  const mhzFor = (ref: CompetitorRef): number | undefined => {
    const assigned = heatMhzByRef.get(ref);
    if (assigned !== undefined) return assigned;
    const node = nodeIndexOf(ref);
    if (node !== undefined) {
      const tuned = signalMhzByNode.get(node);
      if (tuned !== undefined) return tuned;
      const tuned_to = layoutMhzByNode.get(node);
      if (tuned_to !== undefined) return tuned_to;
    }
    const pilot = explicitPilotByRef.get(ref) ?? ref;
    return membershipMhzByPilot.get(pilot);
  };

  const channelFor = (ref: CompetitorRef): string | undefined => {
    const mhz = mhzFor(ref);
    return mhz === undefined ? undefined : channelLabel(mhz, catalog);
  };

  const seatLabel = (node: number): string => nodeSeatLabel(node, mhzFor(`node-${node}`), catalog);

  // Every node seat the screen could render, labelled once. The union of what the heat, the live
  // progress, the signal and the timer's width know about — a superset of what any panel draws.
  const seatRefs = new Set<CompetitorRef>([
    ...(sources.heats ?? []).flatMap((h) => h.lineup ?? []),
    ...(sources.heat?.lineup ?? []),
    ...(sources.progress ?? []).map((p) => p.competitor),
    ...(sources.signal?.nodes ?? []).map((n) => n.seat)
  ]);
  // #412: `node_count` is the RD's OVERRIDE and is normally null — `?? 0` would seed no seat
  // labels at all on a timer that was never pinned. `timerWidth` resolves override → reported
  // → fallback, the same rule the Director's `Timer::node_width` applies.
  const width = sources.timer ? Math.max(0, Math.round(timerWidth(sources.timer))) : 0;
  for (let i = 0; i < width; i++) seatRefs.add(`node-${i}`);

  const seatLabelByRef = new Map<CompetitorRef, string>();
  for (const ref of seatRefs) {
    const node = nodeIndexOf(ref);
    if (node !== undefined) seatLabelByRef.set(ref, nodeSeatLabel(node, mhzFor(ref), catalog));
  }

  const inputs: CompetitorNameInputs = { pilotById, explicitPilotByRef, seatLabelByRef };
  return { name: createCompetitorNameResolver(inputs), mhzFor, channelFor, seatLabel, inputs };
}

/** Re-export so callers can build the channel map without reaching into `channels.ts` too. */
export type { ChannelCatalogEntry };
