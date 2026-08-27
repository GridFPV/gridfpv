/**
 * Event **channel layouts** — the pure readings behind the layout editor (#117 S2).
 *
 * A layout is one complete tuning of the event's timer: `node → channel`, one channel per enabled
 * node, drawn from the timer's **allowed** set. Three scopes answer three different questions and
 * conflating them has been this repo's most repeated bug:
 *
 * | scope            | question                        | state                       |
 * | ---------------- | ------------------------------- | --------------------------- |
 * | Global (a timer) | what may this timer *ever* use? | `Timer.available_channels`  |
 * | **Event**        | **what goes on which node?**    | **layouts — this module**    |
 * | Heat             | which layout does it fly?        | S3, not built               |
 *
 * Everything here is pure: no I/O, no state. The Director owns the rules (it refuses an invalid
 * layout and computes the cross-layout `overlaps`); this module owns **what the RD reads** — the
 * draft the editor binds to, and the sentences that name a layout, a node and a channel rather than
 * an id, an index or a bare MHz.
 *
 * ## Two things this module deliberately does not do
 *
 * It does not decide a strategy. A bracket is one layout for the whole tournament; a GQ qualifier is
 * many layouts so each pilot keeps their own channel. Both fall out of the same mechanism, and
 * choosing between them is the RD's job.
 *
 * And it does not treat cross-layout channel reuse as an error. That only matters for the
 * keep-pilots-on-one-channel strategy, so it is a **warning** an RD can read and ignore.
 */
import type {
  ChannelCatalogEntry,
  ChannelLayout,
  ImdReading,
  LayoutId,
  LayoutNode,
  LayoutRating,
  Timer,
  TimerNodes
} from '@gridfpv/types';
import { channelLabel, nodeSeatLabel } from './channels.js';
import { nodeLabel } from './timerNodes.js';

/**
 * A layout being edited: the same shape as a stored {@link ChannelLayout}, except a node's channel
 * may be **unset** while the RD is still filling it in.
 *
 * The Director refuses an incomplete layout (a layout is a *complete* tuning — a gate left on
 * whatever it happened to be on last is exactly the hole this model closes), so an unset node is a
 * draft state the editor blocks Save on, never something that can be persisted.
 */
export interface LayerDraft {
  /** The layout being edited, or `undefined` for a new one that has no id yet. */
  id?: LayoutId;
  /** The RD-typed name. Blank is a draft state; the Director refuses it. */
  name: string;
  /** `node index → channel (MHz)`, for the nodes the RD has set so far. */
  channels: Map<number, number>;
}

/**
 * The draft as the wire mapping, **ascending by node** — the `nodes` a create/update carries.
 *
 * Unset nodes are simply absent, which is what makes the Director's completeness refusal fire on a
 * half-filled layout rather than the console silently inventing a channel for the gap.
 */
export function draftNodes(draft: LayerDraft): LayoutNode[] {
  return [...draft.channels.entries()]
    .sort((a, b) => a[0] - b[0])
    .map(([node, channel]) => ({ node, channel }));
}

/** The channels a layout's per-node dropdown offers: the timer's **allowed** set, in the RD's order.
 *
 * # Why not the catalog, and why not the capability
 *
 * The Tune page's dropdown (`tuning.ts`'s `channelOptions`) offers everything the hardware can do,
 * because there the RD is picking a bench channel and an empty allowed set means *"unrestricted"*.
 * A layout is the opposite question: it is drawn from what the RD said this timer **may use**, so
 * offering a channel outside that set would offer a choice the Director then refuses. An empty
 * allowed set here is therefore not "offer everything" — it is "there is nothing to choose yet",
 * and the editor says so with {@link unconfiguredTimerMessage} instead of rendering an empty
 * dropdown (the trap that has now appeared in five places).
 */
export function allowedChannels(timer: Timer | undefined): number[] {
  return [...(timer?.available_channels ?? [])];
}

/**
 * What to tell an RD whose event timer has no channels ticked, or `undefined` when it has some.
 *
 * Names the timer, says what is missing and where to fix it — the S1 wording, one level up.
 */
export function unconfiguredTimerMessage(timer: Timer | undefined): string | undefined {
  if (!timer) {
    return 'This event has no timer selected, so there is no node set to tune. Pick a timer for this event first.';
  }
  if (allowedChannels(timer).length > 0) return undefined;
  return `No channels are chosen for ${timer.name} yet. Tick the channels it may use on the Timers page, then define a layout here.`;
}

/**
 * The node indices a layout tunes: the timer's **enabled** set (#412), in ascending order.
 *
 * Not `0..width` — with node index 2 disabled on a four-node timer this is `[0, 1, 3]`, a set with
 * a hole. A disabled node seats nobody, so a layout must not offer to tune it.
 */
export function layerNodes(view: TimerNodes | undefined): number[] {
  return [...(view?.enabled ?? [])];
}

/**
 * The node indices whose channel is duplicated elsewhere in the draft — the **one hard rule inside
 * a layout**: two nodes cannot share a frequency.
 *
 * The editor marks these inline and blocks Save; the Director refuses the same thing with a
 * sentence. Mirrored here only so the RD is told while they are still looking at the dropdown they
 * just changed, rather than after a round-trip.
 */
export function duplicateNodes(draft: LayerDraft): Set<number> {
  const byChannel = new Map<number, number[]>();
  for (const [node, channel] of draft.channels) {
    byChannel.set(channel, [...(byChannel.get(channel) ?? []), node]);
  }
  const out = new Set<number>();
  for (const nodes of byChannel.values()) {
    if (nodes.length > 1) for (const node of nodes) out.add(node);
  }
  return out;
}

/** The nodes a draft has not tuned yet — a layout is a *complete* tuning, so Save waits for these. */
export function untunedNodes(draft: LayerDraft, nodes: readonly number[]): number[] {
  return nodes.filter((node) => draft.channels.get(node) === undefined);
}

/**
 * Why the editor cannot save this draft yet, or `undefined` when it can.
 *
 * Phrased for the RD and resolved through the shared helpers — a node is `"Node 3"`, never an
 * index. The Director enforces the same rules and answers with its own sentence; this is the local
 * half, so a mis-click is answered instantly instead of by a refusal.
 */
export function draftBlocker(
  draft: LayerDraft,
  nodes: readonly number[],
  view: TimerNodes | undefined,
  catalog: readonly ChannelCatalogEntry[]
): string | undefined {
  if (draft.name.trim() === '')
    return 'Name this layout — it is what you pick when a heat flies it.';
  const dupes = [...duplicateNodes(draft)].sort((a, b) => a - b);
  if (dupes.length > 0) {
    const channel = draft.channels.get(dupes[0]);
    const names = dupes.map((node) => nodeName(view, node)).join(' and ');
    const on = channel === undefined ? '' : ` on ${channelLabel(channel, [...catalog])}`;
    return `${names} are both${on} — two nodes cannot share a frequency.`;
  }
  const untuned = untunedNodes(draft, nodes);
  if (untuned.length > 0) {
    const names = untuned.map((node) => nodeName(view, node));
    return `Set a channel for ${names.join(', ')} — a layout tunes every enabled node.`;
  }
  return undefined;
}

/** A node's **display** name, through the shared `#412` resolver — `"Node 3"`, never the index. */
function nodeName(view: TimerNodes | undefined, node: number): string {
  return view ? nodeLabel(view, node) : `Node ${node + 1}`;
}

/**
 * A layout id → its **name** (`"Bracket A"`), or a last-resort `"a deleted layout"`.
 *
 * The resolver the repo display rule asks for when a new entity has both an id and a name: an
 * overlap warning names two layouts, and neither may reach the screen as `bracket-a-k3f9qz`.
 */
export function layoutName(layouts: readonly ChannelLayout[], id: LayoutId): string {
  return layouts.find((l) => l.id === id)?.name ?? 'a deleted layout';
}

/**
 * One node's line in a layout: `"Node 3 · Raceband R7"` — the pair an RD actually needs, through the
 * shared {@link nodeSeatLabel}.
 */
export function layerNodeLabel(entry: LayoutNode, catalog: readonly ChannelCatalogEntry[]): string {
  return nodeSeatLabel(entry.node, entry.channel, catalog);
}

/** A layout's whole tuning as one readable line — every node and the channel it flies. */
export function layerSummary(
  layout: ChannelLayout,
  catalog: readonly ChannelCatalogEntry[]
): string {
  return layout.nodes.map((entry) => layerNodeLabel(entry, catalog)).join(' · ');
}

/**
 * The **warning** sentence for one cross-layout channel overlap — a notice, never a refusal.
 *
 * The RD's own call: reusing a channel between layouts only matters for the
 * keep-pilots-on-one-channel strategy (a GQ-style qualifier), and an RD running a bracket off one
 * layout does not care. So this is written to be *informative and ignorable*: it names both layouts
 * and the shared channels, and says what it costs, without implying anything is broken.
 */
export function overlapMessage(
  overlap: { layout: LayoutId; other: LayoutId; channels: number[] },
  layouts: readonly ChannelLayout[],
  catalog: readonly ChannelCatalogEntry[]
): string {
  const names = overlap.channels.map((mhz) => channelLabel(mhz, [...catalog]));
  const shared =
    names.length === 1
      ? names[0]
      : `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`;
  const verb = names.length === 1 ? 'uses' : 'use';
  return (
    `${layoutName(layouts, overlap.layout)} and ${layoutName(layouts, overlap.other)} both ${verb} ` +
    `${shared}. That is fine for a bracket flying one layout; it means a pilot cannot stay on one ` +
    `channel across both if you are running qualifiers that way.`
  );
}

/* ── IMD (#117 S4) ────────────────────────────────────────────────────────────────────────────
 *
 * How cleanly a layout's channels fly together, read while the RD is still choosing them — the
 * one moment the information can still change anything. A layout is defined once and flown all
 * event, so a check paid here is a check the RD can act on; the same check on a filled heat
 * arrives after the decision is made.
 *
 * **The number is not computed here.** `session.rateChannels()` asks the Director, which owns the
 * only implementation of IMDTabler in the system. That is #430's finding taken seriously: the
 * number's whole value is that it is the same number the RD reads off RotorHazard for the same
 * channels, and a second port of the algorithm in the console is exactly how that stops being
 * true. This module turns the Director's reading into the sentence.
 *
 * **No threshold, no verdict word.** There is deliberately no clean/marginal/poor band, because
 * the achievable ceiling collapses with pilot count (4 nodes → 65, 5 → 33, 6 → 28 from a real
 * pool): a flat band would tell every RD running a six-pilot heat that their spectrum is dirty.
 * And nothing here blocks anything — a poor rating still saves, because the RD may have no better
 * option and a Raceband-only timer genuinely cannot beat 0 at five pilots.
 */

/** How far (MHz) a mixing product has to miss a used channel to stop mattering — IMDTabler's own
 * `RATING_DIFF_LIMIT`, and the line the Director rates against. Quoted in the clean sentence so
 * "clean" means something specific rather than "we found nothing". */
const IMD_LIMIT_MHZ = 35;

/**
 * One layout's IMD reading out of the view's `ratings`, or `undefined` when the Director sent none.
 *
 * Keyed by layout id rather than by position, because that is how the Director sends it — a
 * parallel array mis-labels every layout the day the list is filtered or reordered.
 */
export function layoutRating(
  ratings: readonly LayoutRating[] | undefined,
  id: LayoutId
): ImdReading | undefined {
  return ratings?.find((r) => r.layout === id)?.imd;
}

/**
 * The rating on its own — `"IMD 29"`.
 *
 * Higher is cleaner and 100 is the ceiling; a genuinely bad set goes negative (all eight of
 * Raceband rates −635), so the minus sign is real and is rendered as one (`−`, not a hyphen).
 */
export function imdRatingLabel(reading: ImdReading): string {
  const n = reading.rating;
  return `IMD ${n < 0 ? `−${Math.abs(n)}` : n}`;
}

/**
 * The **worst offender** in plain language — the specific problem behind the rating.
 *
 * `2 × Raceband R2 − Raceband R1 = 5732 MHz — lands on Raceband R3`
 *
 * Three of the four values are channels this layout actually tunes — the two that mix and the one
 * they land near — so all three are named through the shared {@link channelLabel} resolver and
 * none of them reaches the screen as a bare frequency. The **product** is the exception, and
 * deliberately so: it is arithmetic, not an entity. It is the frequency the mix creates, nobody is
 * flying it, and naming it after whichever catalog channel happens to sit at that number would
 * claim a pilot is there.
 *
 * When nothing lands within {@link IMD_LIMIT_MHZ} there is **no offender to name**, and the honest
 * answer is that the set is clean — not a nearest miss dressed up as a problem.
 */
export function imdOffenderMessage(
  reading: ImdReading,
  catalog: readonly ChannelCatalogEntry[]
): string {
  const worst = reading.worst;
  if (!worst) {
    return `nothing mixes within ${IMD_LIMIT_MHZ} MHz of a channel this layout uses`;
  }
  const name = (mhz: number) => channelLabel(mhz, [...catalog]);
  const victim = name(worst.lands_on);
  const where = worst.gap_mhz === 0 ? `lands on ${victim}` : `${worst.gap_mhz} MHz off ${victim}`;
  return `2 × ${name(worst.doubled)} − ${name(worst.subtracted)} = ${worst.product} MHz — ${where}`;
}

/**
 * The whole reading as one line — the rating, then what is wrong with it.
 *
 * `IMD −635 · worst offender: 2 × Raceband R2 − Raceband R1 = 5732 MHz — lands on Raceband R3`
 *
 * `IMD 100 · nothing mixes within 35 MHz of a channel this layout uses`
 */
export function imdMessage(reading: ImdReading, catalog: readonly ChannelCatalogEntry[]): string {
  const tail = reading.worst
    ? `worst offender: ${imdOffenderMessage(reading, catalog)}`
    : imdOffenderMessage(reading, catalog);
  return `${imdRatingLabel(reading)} · ${tail}`;
}

/**
 * The draft's channels as a **set**: ascending, de-duplicated — what the Director is asked to rate.
 *
 * Ascending and de-duplicated so the same choice always produces the same query, which is what
 * makes the editor's per-set cache actually hit as the RD ticks back and forth between two
 * channels.
 */
export function draftChannelSet(draft: LayerDraft): number[] {
  return [...new Set(draft.channels.values())].sort((a, b) => a - b);
}
