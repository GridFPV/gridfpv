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
 * **No verdict WORD, but a colour, scaled by pilot count (#474).** The reason there was no band at
 * all is still true — the achievable ceiling collapses with pilot count, so a *flat* clean/poor
 * line tells every RD running a six-pilot heat that their spectrum is dirty. The answer is not to
 * say nothing; it is to compare a layout against what is achievable **at its own pilot count**.
 * {@link IMD_BANDS} is that comparison, and it is the only place the numbers live.
 *
 * Still no verdict word, and still nothing blocking: the colour is guidance beside a number the RD
 * can act on, the worst-offender sentence is unchanged, and a red layout saves exactly as it
 * always did — the RD may have no better option, and a Raceband-only timer genuinely cannot beat
 * 2 at five pilots.
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
 * Everything after the rating in the reading's one line — the worst-offender half on its own.
 *
 * `worst offender: 2 × Raceband R2 − Raceband R1 = 5732 MHz — lands on Raceband R3`
 *
 * `nothing mixes within 35 MHz of a channel this layout uses`
 *
 * This used to be spliced inside a single `imdMessage(reading, catalog)`. #474 splits it, because
 * the two halves are now rendered differently: the **rating** carries the green/amber/red colour,
 * and this does not. The offender sentence names three channels somebody is flying, and tinting it
 * red would read as a verdict on those channels rather than on the set they are part of.
 *
 * The whole line is still `${imdRatingLabel(reading)} · ${imdTail(reading, catalog)}` and nothing
 * about its text has changed.
 */
export function imdTail(reading: ImdReading, catalog: readonly ChannelCatalogEntry[]): string {
  const offender = imdOffenderMessage(reading, catalog);
  return reading.worst ? `worst offender: ${offender}` : offender;
}

/**
 * The green / amber / red bands for the IMD rating, per pilot count (#474).
 *
 * **This table is the only place these numbers exist.** It is calibrated, not arbitrary, and it is
 * expected to be field-tuned — so the derivation is written down here rather than left as folklore.
 *
 * ## What the colour means
 *
 * - **green** — as clean as the layout the hobby would recommend for this many pilots. There is
 *   nothing meaningful left to win by re-picking.
 * - **amber** — meaningfully worse than that: about what you get by moving **one** pilot onto a
 *   poorly-chosen channel. Flyable, and worth another look at the worst-offender line.
 * - **red** — clearly worse than any single bad channel could explain. The *set* is wrong, not one
 *   channel in it.
 *
 * ## How the numbers were derived
 *
 * The engine (`crates/engine/src/imd.rs`) is a faithful IMDTabler port, so the reference points are
 * the hobby's own published sets and the ratings it produces for them:
 *
 * | pilots | best achievable, full catalog | the reference layout                  |
 * |-------:|------------------------------:|---------------------------------------|
 * |      2 |                           100 | (any well-spaced pair)                |
 * |      3 |                           100 | (any well-spaced trio)                |
 * |      4 |                           100 | **Racebnd4** = 100                    |
 * |      5 |                           100 | **ET6minus1** = 98                    |
 * |      6 |                            67 | **ETBest6** (MultiGP official) = 67   |
 * |      7 |                           −14 | (no canonical set exists)             |
 * |      8 |                          −203 | all of Raceband = −635 (the worst)    |
 *
 * "Best achievable" is the highest-rating subset of the **whole 40-frequency catalog** at that size
 * that also clears the engine's 35 MHz separation floor — the ceiling an RD could reach with a
 * fully-configured timer.
 *
 * - **The green floor is the ceiling less 10.** The recommended layout *is* the ceiling at 4, 5 and
 *   6 pilots, so this makes it green by construction, together with anything indistinguishable
 *   from it.
 * - **The amber floor is what moving one pilot to a typical wrong channel costs.** Taking the best
 *   set at each size and substituting one channel for every other legal one, the *median* result is
 *   100 / 100 / 38 / 32 / −27 / −98 / −263 for 2…8 pilots. Rounded, those are the amber floors —
 *   which puts the boundary at "worse than one mistake", i.e. the set itself.
 *
 * Sanity checks that fall out, and the reason to believe the shape: **Racebnd4** (100 at 4) green;
 * **ET6minus1** (98 at 5) green; **ETBest6** (67 at 6) green; **RotorHazard's own IMD6C** (29 at 6)
 * **amber** — workable, and MultiGP's set really is 38 points cleaner; alternating Raceband at 4
 * (−145) red; all eight of Raceband (−635) deep red; and the best 8-set (−203) green, because at
 * eight pilots that is genuinely as good as it gets.
 *
 * ## What this table is NOT calibrated against
 *
 * Not a percentile of all possible sets. Swept exhaustively, most separation-legal sets at six or
 * more pilots land red — which is a true statement about 5.8 GHz, not a mis-set threshold. An RD
 * picks from a timer's configured pool and usually starts from a recommended set, so this is not
 * the population they draw from. If red turns out to be the *normal* reading in the field rather
 * than the exceptional one, widen the amber floors here — one edit, one place.
 */
export const IMD_BANDS: readonly { pilots: number; green: number; amber: number }[] = [
  { pilots: 2, green: 90, amber: 40 },
  { pilots: 3, green: 90, amber: 40 },
  { pilots: 4, green: 90, amber: 35 },
  { pilots: 5, green: 90, amber: 30 },
  { pilots: 6, green: 55, amber: -30 },
  { pilots: 7, green: -25, amber: -100 },
  { pilots: 8, green: -215, amber: -265 }
];

/**
 * The band row for a pilot count, clamped at both ends of {@link IMD_BANDS}.
 *
 * Fewer than two channels cannot interfere with anything, so they take the first row and rate at
 * the ceiling regardless. Beyond eight the last row holds: the engine's ratings keep falling, but
 * nine simultaneous pilots on 5.8 GHz is off the edge of the map and a fabricated ninth row would
 * be a guess wearing a constant's clothes.
 */
function imdBandFor(pilots: number): { green: number; amber: number } {
  const first = IMD_BANDS[0];
  const last = IMD_BANDS[IMD_BANDS.length - 1];
  return IMD_BANDS.find((b) => b.pilots === pilots) ?? (pilots < first.pilots ? first : last);
}

/**
 * The tone for one layout's IMD rating at `pilots` pilots — the console's `success` / `warn` /
 * `danger` tokens, so it themes with every other verdict on the console rather than picking its
 * own greens (#474).
 */
export function imdTone(reading: ImdReading, pilots: number): 'success' | 'warn' | 'danger' {
  const band = imdBandFor(pilots);
  if (reading.rating >= band.green) return 'success';
  return reading.rating >= band.amber ? 'warn' : 'danger';
}

/**
 * What the colour means, for the tooltip on the rating (#474).
 *
 * Frames it as **guidance**, in the RD's own terms, and names the pilot count it was judged
 * against — because "is 29 good?" has no answer that is not "for how many pilots?". It never says
 * a layout is broken and never implies a refusal: the worst-offender line beside it is the
 * actionable half, and everything saves either way.
 */
export function imdToneHint(reading: ImdReading, pilots: number): string {
  const band = imdBandFor(pilots);
  const seats = `${pilots} channel${pilots === 1 ? '' : 's'} flying at once`;
  if (reading.rating >= band.green) {
    return `Guidance only: for ${seats}, this is as clean as the layouts the hobby recommends. Nothing here blocks saving.`;
  }
  if (reading.rating >= band.amber) {
    return `Guidance only: for ${seats}, cleaner sets exist — about what one badly-placed channel costs. The worst offender beside it says which. Nothing here blocks saving.`;
  }
  return `Guidance only: for ${seats}, this rates well below what is achievable, more than one wrong channel would explain. The worst offender beside it is where to start. Nothing here blocks saving.`;
}

/**
 * How many channels a saved layout flies **at once** — the pilot count its rating is judged
 * against (#474).
 *
 * Distinct channels, not nodes: a layout is validated as conflict-free so the two agree today, and
 * counting the set rather than the seats keeps this answering the question IMD actually asks —
 * *how many transmitters are mixing* — if that ever stops being true.
 */
export function layoutPilotCount(layout: ChannelLayout): number {
  return new Set(layout.nodes.map((entry) => entry.channel)).size;
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
